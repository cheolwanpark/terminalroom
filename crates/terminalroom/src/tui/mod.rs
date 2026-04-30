use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, select};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use image::{DynamicImage, ImageBuffer, Rgb};
use lru::LruCache;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use darkroom::{DevelopError, ImageKind, RgbImage, TargetSize};

use crate::app::{App, View};
use crate::db::CullingState;

mod culling;
mod develop;
mod filter;

const TICK: Duration = Duration::from_millis(100);
const PREVIEW_CACHE_CAP: usize = 9;
// Filmstrip width + 2-cell preview borders. Mirrors layout in culling.rs.
const FILMSTRIP_RESERVED_COLS: u16 = 28 + 2;
// Status row + 2-cell preview borders.
const STATUS_RESERVED_ROWS: u16 = 1 + 2;
// Re-enqueue jobs only when target dimensions move at least this fraction.
const TARGET_DEBOUNCE: f64 = 0.25;

pub(crate) struct PreviewEntry {
    pub(crate) proto: StatefulProtocol,
    pub(crate) src_w: u32,
    pub(crate) src_h: u32,
}

pub(crate) struct PreviewSlot {
    pub(crate) fast: Option<PreviewEntry>,
    pub(crate) full: Option<PreviewEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Fast,
    Full,
}

struct Job {
    path: PathBuf,
    kind: ImageKind,
    target: TargetSize,
    tier: Tier,
    cancel: Arc<AtomicBool>,
    generation: u64,
}

struct JobDone {
    path: PathBuf,
    tier: Tier,
    generation: u64,
    result: std::result::Result<RgbImage, DevelopError>,
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run(app: &mut App) -> Result<()> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .context("failed to enter terminal alternate screen")?;
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to construct ratatui terminal")?;

    let picker = match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(e) => {
            app.status = Some(format!(
                "image protocol detection failed: {e}; rendering text only"
            ));
            // Fall back to a 1x2 fontsize picker; ratatui-image will choose halfblocks.
            Picker::from_fontsize((1, 2))
        }
    };

    let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<JobDone>();
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<Event>();

    spawn_event_thread(event_tx);
    spawn_worker(job_rx, done_tx);

    let mut cache: LruCache<PathBuf, PreviewSlot> =
        LruCache::new(NonZeroUsize::new(PREVIEW_CACHE_CAP).unwrap());
    let mut last_selection: Option<PathBuf> = None;
    let mut last_target: Option<TargetSize> = None;
    let mut current_generation: u64 = 0;
    let mut current_cancel: Option<Arc<AtomicBool>> = None;
    let mut latest_generation: HashMap<PathBuf, u64> = HashMap::new();

    loop {
        let size = terminal.size().unwrap_or(Size {
            width: 80,
            height: 24,
        });
        let target = preview_target(size, picker.font_size());

        let path_now = app.current().map(|e| e.file.canonical_path.clone());
        let kind_now = app.current().map(|e| e.file.kind);

        let target_changed = last_target
            .map(|prev| target_changed_meaningfully(prev, target))
            .unwrap_or(true);
        let selection_changed = path_now != last_selection;

        if selection_changed || (path_now.is_some() && target_changed) {
            // Cancel any previous generation's outstanding work.
            if let Some(c) = current_cancel.take() {
                c.store(true, Ordering::Relaxed);
            }
            current_generation = current_generation.saturating_add(1);

            if let (Some(path), Some(kind)) = (path_now.as_ref(), kind_now) {
                let cancel = Arc::new(AtomicBool::new(false));
                current_cancel = Some(cancel.clone());
                latest_generation.insert(path.clone(), current_generation);

                let fast_target = TargetSize::new(
                    (target.max_w / 4).max(1),
                    (target.max_h / 4).max(1),
                );

                let (has_fast, has_full) = match cache.peek(path) {
                    Some(slot) => (slot.fast.is_some(), slot.full.is_some()),
                    None => (false, false),
                };
                // Skip the fast tier when something is already on screen for this file.
                if !has_fast && !has_full {
                    let _ = job_tx.send(Job {
                        path: path.clone(),
                        kind,
                        target: fast_target,
                        tier: Tier::Fast,
                        cancel: cancel.clone(),
                        generation: current_generation,
                    });
                }
                if !has_full {
                    let _ = job_tx.send(Job {
                        path: path.clone(),
                        kind,
                        target,
                        tier: Tier::Full,
                        cancel,
                        generation: current_generation,
                    });
                }
            }

            last_selection = path_now;
            last_target = Some(target);
        }

        let font_size = picker.font_size();
        terminal
            .draw(|frame| draw(frame, app, &mut cache, font_size))
            .context("failed to draw frame")?;

        select! {
            recv(event_rx) -> ev => {
                let Ok(ev) = ev else { break; };
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(app, key) == Action::Quit { break; }
                    }
                    Event::Resize(_, _) => {
                        // Next loop iteration recomputes target and may re-enqueue jobs
                        // (subject to TARGET_DEBOUNCE).
                    }
                    _ => {}
                }
            }
            recv(done_rx) -> done => {
                let Ok(done) = done else { break; };
                handle_job_done(done, &mut cache, &picker, app, &latest_generation);
            }
            default(TICK) => {}
        }
    }

    // Tell the worker to exit; its thread will wake on the next recv() error.
    drop(job_tx);
    Ok(())
}

fn spawn_event_thread(tx: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
}

fn spawn_worker(rx: Receiver<Job>, tx: Sender<JobDone>) {
    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            let Job {
                path,
                kind,
                target,
                tier,
                cancel,
                generation,
            } = job;
            let result = if cancel.load(Ordering::Relaxed) {
                Err(DevelopError::Cancelled)
            } else {
                darkroom::develop_to_rgb(&path, kind, target, Some(&cancel))
            };
            if tx
                .send(JobDone {
                    path,
                    tier,
                    generation,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
}

fn handle_job_done(
    done: JobDone,
    cache: &mut LruCache<PathBuf, PreviewSlot>,
    picker: &Picker,
    app: &mut App,
    latest_generation: &HashMap<PathBuf, u64>,
) {
    // Drop results from prior generations — the user has moved on.
    if latest_generation.get(&done.path).copied() != Some(done.generation) {
        return;
    }

    match done.result {
        Ok(rgb) => {
            let (w, h) = (rgb.width, rgb.height);
            match ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, rgb.pixels) {
                Some(buf) => {
                    let entry = PreviewEntry {
                        proto: picker.new_resize_protocol(DynamicImage::ImageRgb8(buf)),
                        src_w: w,
                        src_h: h,
                    };
                    let slot = cache.get_or_insert_mut(done.path.clone(), || PreviewSlot {
                        fast: None,
                        full: None,
                    });
                    match done.tier {
                        Tier::Fast => {
                            if slot.full.is_none() {
                                slot.fast = Some(entry);
                            }
                        }
                        Tier::Full => {
                            slot.full = Some(entry);
                            slot.fast = None;
                        }
                    }
                    if app
                        .status
                        .as_deref()
                        .is_some_and(|s| s.starts_with("preview error"))
                    {
                        app.status = None;
                    }
                }
                None => {
                    app.status = Some("preview error: rgb buffer shape mismatch".into());
                }
            }
        }
        Err(DevelopError::Cancelled) => {} // benign
        Err(e) => {
            app.status = Some(format!("preview error: {e}"));
        }
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    app: &mut App,
    cache: &mut LruCache<PathBuf, PreviewSlot>,
    font_size: (u16, u16),
) {
    match app.view {
        View::Culling => culling::render(frame, app, cache, font_size),
        View::Develop => develop::render(frame, app),
        View::Filter => {
            culling::render(frame, app, cache, font_size);
            filter::render(frame, app);
        }
    }
}

fn preview_target(terminal_size: Size, font_size: (u16, u16)) -> TargetSize {
    let cols = terminal_size
        .width
        .saturating_sub(FILMSTRIP_RESERVED_COLS)
        .max(1);
    let rows = terminal_size
        .height
        .saturating_sub(STATUS_RESERVED_ROWS)
        .max(1);
    TargetSize::new(
        cols as u32 * font_size.0 as u32,
        rows as u32 * font_size.1 as u32,
    )
}

fn target_changed_meaningfully(prev: TargetSize, next: TargetSize) -> bool {
    fn diff(a: u32, b: u32) -> bool {
        let (lo, hi) = (a.min(b), a.max(b));
        if hi == 0 {
            return false;
        }
        ((hi - lo) as f64 / hi as f64) >= TARGET_DEBOUNCE
    }
    diff(prev.max_w, next.max_w) || diff(prev.max_h, next.max_h)
}

#[derive(PartialEq, Eq)]
enum Action {
    Continue,
    Quit,
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    match app.view {
        View::Culling => handle_culling_key(app, key.code),
        View::Develop => handle_develop_key(app, key.code),
        View::Filter => handle_filter_key(app, key.code),
    }
}

fn handle_culling_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('j') | KeyCode::Right => app.next(),
        KeyCode::Char('k') | KeyCode::Left => app.prev(),
        KeyCode::Char('p') => app.set_state(CullingState::Pick, now_unix()),
        KeyCode::Char('x') => app.set_state(CullingState::Reject, now_unix()),
        KeyCode::Char('u') => app.set_state(CullingState::Unset, now_unix()),
        KeyCode::Char('d') => app.view = View::Develop,
        KeyCode::Char('f') => app.open_filter(),
        _ => {}
    }
    Action::Continue
}

fn handle_develop_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') => {
            app.view = View::Culling;
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn handle_filter_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => app.filter_next(),
        KeyCode::Char('k') | KeyCode::Up => app.filter_prev(),
        KeyCode::Char(' ') => app.toggle_current_filter(),
        KeyCode::Enter | KeyCode::Esc | KeyCode::Char('f') => app.close_filter(),
        _ => {}
    }
    Action::Continue
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn resize_strategy() -> Resize {
    // Scale (not Fit) so the fast-tier preview, whose source is much smaller than the
    // preview area, is upscaled to fill the same display rect as the full-tier preview.
    // Both tiers therefore have identical display size; they differ only in sharpness.
    Resize::Scale(None)
}
