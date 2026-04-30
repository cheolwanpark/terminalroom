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
use ratatui::layout::{Constraint, Layout, Size};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use darkroom::{
    DevelopError, DevelopParams, ImageKind, Loaded, Srgb8, TargetSize, decode, develop_preview,
};

use crate::app::{App, FileMeta, Focus, View};
use crate::db::CullingState;

mod banner;
mod develop;
mod filmstrip;
mod filter;
mod info;
mod preview;
mod status;

const TICK: Duration = Duration::from_millis(100);
const PREVIEW_CACHE_CAP: usize = 9;
/// Width of each side tab (Develop, Image Info, Navigation/filmstrip).
const TAB_WIDTH: u16 = 28;
/// Total side-tab columns + 2-cell preview borders.
const SIDE_RESERVED_COLS: u16 = TAB_WIDTH * 3 + 2;
/// Status row + 2-cell preview borders.
const STATUS_RESERVED_ROWS: u16 = 1 + 2;
/// Re-enqueue jobs only when target dimensions move at least this fraction.
const TARGET_DEBOUNCE: f64 = 0.25;

pub(crate) struct PreviewEntry {
    pub(crate) proto: StatefulProtocol,
    pub(crate) src_w: u32,
    pub(crate) src_h: u32,
    pub(crate) rendered_target: TargetSize,
    pub(crate) params_fingerprint: u64,
}

struct Job {
    path: PathBuf,
    target: TargetSize,
    cancel: Arc<AtomicBool>,
    generation: u64,
    params: DevelopParams,
    params_fingerprint: u64,
    size_bytes: u64,
}

struct JobDone {
    path: PathBuf,
    generation: u64,
    target: TargetSize,
    params_fingerprint: u64,
    result: std::result::Result<Srgb8, DevelopError>,
    meta: Option<FileMeta>,
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
    execute!(stdout, EnterAlternateScreen).context("failed to enter terminal alternate screen")?;
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

    let mut cache: LruCache<PathBuf, PreviewEntry> =
        LruCache::new(NonZeroUsize::new(PREVIEW_CACHE_CAP).unwrap());
    let mut last_selection: Option<PathBuf> = None;
    let mut last_target: Option<TargetSize> = None;
    let mut last_params_fp: Option<u64> = None;
    let mut current_generation: u64 = 0;
    let mut current_cancel: Option<Arc<AtomicBool>> = None;
    let mut latest_generation: HashMap<PathBuf, u64> = HashMap::new();

    loop {
        let size = terminal.size().unwrap_or(Size {
            width: 80,
            height: 24,
        });
        let banner_h = banner::height_for(size.width);
        let target = preview_target(size, banner_h, picker.font_size());

        let path_now = app.current().map(|e| e.file.canonical_path.clone());
        let size_now = app.current().map(|e| e.file.size_bytes).unwrap_or(0);

        let target_changed = last_target
            .map(|prev| target_changed_meaningfully(prev, target))
            .unwrap_or(true);
        let selection_changed = path_now != last_selection;
        let params_fp = app.develop_params.fingerprint();
        let params_changed = last_params_fp != Some(params_fp);

        if selection_changed || (path_now.is_some() && target_changed) || params_changed {
            // Cancel any in-flight job for the prior generation.
            if let Some(c) = current_cancel.take() {
                c.store(true, Ordering::Relaxed);
            }
            current_generation = current_generation.saturating_add(1);

            if let Some(path) = path_now.as_ref() {
                let needs_render = cache
                    .peek(path)
                    .map(|e| {
                        target_changed_meaningfully(e.rendered_target, target)
                            || e.params_fingerprint != params_fp
                    })
                    .unwrap_or(true);
                if needs_render {
                    let cancel = Arc::new(AtomicBool::new(false));
                    current_cancel = Some(cancel.clone());
                    latest_generation.insert(path.clone(), current_generation);
                    let _ = job_tx.send(Job {
                        path: path.clone(),
                        target,
                        cancel,
                        generation: current_generation,
                        params: app.develop_params.clone(),
                        params_fingerprint: params_fp,
                        size_bytes: size_now,
                    });
                }
            }

            last_selection = path_now;
            last_target = Some(target);
            last_params_fp = Some(params_fp);
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
                        // Next loop iteration recomputes target and may re-enqueue jobs.
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
                target,
                cancel,
                generation,
                params,
                params_fingerprint,
                size_bytes,
            } = job;
            let (result, meta) = if cancel.load(Ordering::Relaxed) {
                (Err(DevelopError::Cancelled), None)
            } else {
                match decode(&path) {
                    Ok(loaded) => {
                        let meta = file_meta_from(&loaded, size_bytes);
                        let r = develop_preview(&loaded, &params, target, Some(&cancel));
                        (r, Some(meta))
                    }
                    Err(e) => (Err(DevelopError::Decode(e)), None),
                }
            };
            if tx
                .send(JobDone {
                    path,
                    generation,
                    target,
                    params_fingerprint,
                    result,
                    meta,
                })
                .is_err()
            {
                break;
            }
        }
    });
}

fn file_meta_from(loaded: &Loaded, size_bytes: u64) -> FileMeta {
    match loaded {
        Loaded::Image(i) => FileMeta {
            shot_info: i.shot_info.clone(),
            width: i.width,
            height: i.height,
            orientation: Some(i.orientation),
            size_bytes,
            kind: i.kind,
        },
        Loaded::Raw(r) => FileMeta {
            shot_info: r.shot_info.clone(),
            width: r.width,
            height: r.height,
            orientation: None,
            size_bytes,
            kind: ImageKind::Raw,
        },
    }
}

fn handle_job_done(
    done: JobDone,
    cache: &mut LruCache<PathBuf, PreviewEntry>,
    picker: &Picker,
    app: &mut App,
    latest_generation: &HashMap<PathBuf, u64>,
) {
    if latest_generation.get(&done.path).copied() != Some(done.generation) {
        return;
    }

    if let Some(meta) = done.meta {
        app.file_meta.insert(done.path.clone(), meta);
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
                        rendered_target: done.target,
                        params_fingerprint: done.params_fingerprint,
                    };
                    cache.put(done.path.clone(), entry);
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
    cache: &mut LruCache<PathBuf, PreviewEntry>,
    font_size: (u16, u16),
) {
    let area = frame.area();
    let banner_h = banner::height_for(area.width);
    let [banner_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(banner_h),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(area);

    banner::render(frame, banner_area);

    let [preview_area, develop_area, info_area, filmstrip_area] = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(TAB_WIDTH),
        Constraint::Length(TAB_WIDTH),
        Constraint::Length(TAB_WIDTH),
    ])
    .areas(main_area);

    preview::render(frame, app, cache, preview_area, font_size);
    develop::render(frame, app, develop_area);
    info::render(frame, app, info_area);
    filmstrip::render(frame, app, filmstrip_area);
    status::render(frame, app, status_area);

    if app.view == View::Filter {
        filter::render(frame, app);
    }
}

fn preview_target(terminal_size: Size, banner_h: u16, font_size: (u16, u16)) -> TargetSize {
    let cols = terminal_size
        .width
        .saturating_sub(SIDE_RESERVED_COLS)
        .max(1);
    let rows = terminal_size
        .height
        .saturating_sub(STATUS_RESERVED_ROWS)
        .saturating_sub(banner_h)
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
        View::Filter => handle_filter_key(app, key.code),
        View::Main => match app.focus {
            Focus::Navigation => handle_navigation_key(app, key.code),
            Focus::Develop => handle_develop_key(app, key.code),
        },
    }
}

fn handle_navigation_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => app.next(),
        KeyCode::Char('k') | KeyCode::Up => app.prev(),
        KeyCode::Char('p') => app.set_state(CullingState::Pick, now_unix()),
        KeyCode::Char('x') => app.set_state(CullingState::Reject, now_unix()),
        KeyCode::Char('u') => app.set_state(CullingState::Unset, now_unix()),
        KeyCode::Char('f') => app.open_filter(),
        KeyCode::Enter => app.enter_develop(),
        _ => {}
    }
    Action::Continue
}

fn handle_develop_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Esc => app.exit_develop(),
        KeyCode::Char('j') | KeyCode::Down => app.develop_next(),
        KeyCode::Char('k') | KeyCode::Up => app.develop_prev(),
        KeyCode::Char('h') | KeyCode::Left => app.develop_adjust(-1.0),
        KeyCode::Char('l') | KeyCode::Right => app.develop_adjust(1.0),
        KeyCode::Char('r') => app.develop_reset(),
        _ => {}
    }
    Action::Continue
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
    Resize::Scale(None)
}

/// Block with the focus-aware border styling shared by the three side tabs.
pub(crate) fn tab_block(title: &'static str, focused: bool) -> Block<'static> {
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block = block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Yellow));
    }
    block
}
