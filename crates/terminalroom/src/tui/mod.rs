use std::collections::{HashMap, HashSet};
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use ratatui_image::picker::cap_parser::Parser as CapParser;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

use darkroom::{
    DevelopError, DevelopParams, ImageKind, Loaded, LookRegistry, PreparedSource, Srgb8,
    TargetSize, apply_pipeline, decode, prepare_source,
};

use crate::app::{App, DevelopKnob, FileMeta, Focus, View, DEVELOP_KNOBS};
use crate::cache::Cache as DiskCache;
use crate::paths;

mod banner;
mod develop;
mod filmstrip;
mod filter;
mod info;
mod looks;
mod preview;
mod status;

const TICK: Duration = Duration::from_millis(100);
/// Debounce when the worker has not yet decoded the active source. Prevents
/// firing a heavy decode on every knob tick.
const DEBOUNCE_COLD: Duration = Duration::from_millis(250);
/// Debounce when the worker has the active source cached. Drops to a value
/// just above typical key-repeat (~30 ms) so knob ticks feel real-time.
const DEBOUNCE_HOT: Duration = Duration::from_millis(50);
/// Capacity of the rendered-preview LRU. Bumped from 9 → 15 to comfortably
/// hold the active preview, prefetched cursor±1, and recent history under
/// fast scrolling without churn. Each entry is a `StatefulProtocol`, not a
/// raw f32 buffer, so the memory cost is modest.
const PREVIEW_CACHE_CAP: usize = 15;
/// Capacity of the worker-side prepared-source cache. Each entry holds a
/// target-sized `Buffer<CameraLinear>` (~18-36 MB at typical preview sizes).
const SOURCE_CACHE_CAP: usize = 3;
/// While the cursor is changing faster than this, skip foreground develop
/// dispatch and prefetch — both would only be cancelled as the user keeps
/// scrolling. Once the cursor sits still for `NAV_SETTLE`, the next
/// iteration kicks off the real preview for whatever the cursor is on.
const NAV_SETTLE: Duration = Duration::from_millis(150);
/// Two same-direction nav keypresses within this gap are treated as part of
/// the same key-repeat burst. Set above the typical OS auto-repeat *delay*
/// (~250 ms on macOS) so the very first auto-repeat is classified as a
/// continuation of the initial press; otherwise the 1-second slow phase
/// would start at the first auto-repeat instead of the keypress, pushing
/// the slow→fast transition out to ~1.3 s.
const NAV_BURST_GAP: Duration = Duration::from_millis(350);
/// Minimum spacing between cursor advances during the *slow phase* of a held
/// burst. Holding j/k initially advances at 5 Hz so the user can read the
/// names flying by; after `NAV_RAMP_DURATION` the rate-limit is dropped and
/// the cursor moves at OS-repeat speed.
const NAV_SLOW_INTERVAL: Duration = Duration::from_millis(200);
/// How long a held burst stays in the slow phase before ramping up to full
/// auto-repeat speed.
const NAV_RAMP_DURATION: Duration = Duration::from_millis(1000);
const TAB_WIDTH: u16 = 28;
const SIDE_RESERVED_COLS: u16 = TAB_WIDTH * 3 + 2;
const STATUS_RESERVED_ROWS: u16 = 1 + 2;
const TARGET_DEBOUNCE: f64 = 0.25;

pub(crate) struct PreviewEntry {
    pub(crate) proto: StatefulProtocol,
    pub(crate) src_w: u32,
    pub(crate) src_h: u32,
    pub(crate) rendered_target: TargetSize,
    pub(crate) params_fingerprint: u64,
}

/// Foreground (active selection) vs. prefetch (cursor±1 warm-up). The kind
/// flows from `Job` into `JobDone` so the main thread knows whether to
/// validate against `latest_generation` (fg) or just install if Ok (prefetch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Foreground,
    Prefetch,
}

struct Job {
    path: PathBuf,
    target: TargetSize,
    cancel: Arc<AtomicBool>,
    generation: u64,
    params: DevelopParams,
    params_fingerprint: u64,
    source_fp: u64,
    size_bytes: u64,
    kind: JobKind,
    /// Snapshot of the look registry at dispatch time. Cloning the `Arc` is
    /// O(1); the worker uses it inside `apply_pipeline` to resolve the look
    /// id stored in `params.look`.
    look_registry: Arc<LookRegistry>,
}

struct JobDone {
    path: PathBuf,
    generation: u64,
    target: TargetSize,
    params_fingerprint: u64,
    source_fp: u64,
    result: std::result::Result<Srgb8, DevelopError>,
    meta: Option<FileMeta>,
    kind: JobKind,
}

/// Persistence work routed to the save worker. The worker owns its own `Db`
/// connection and a clone of the on-disk cache, so the UI thread never
/// blocks on `fs::sync_all` or SQLite during a knob tick.
enum SaveMsg {
    /// Persist updated develop knobs for `file_id`. The matching in-memory
    /// commit happens on the UI thread immediately before this is sent.
    Params {
        file_id: i64,
        params: DevelopParams,
        fp: u64,
        now: i64,
    },
    /// Persist a freshly-rendered preview into the on-disk cache.
    CacheBlob {
        path: PathBuf,
        source_fp: u64,
        params_fp: u64,
        srgb: Srgb8,
        now: i64,
    },
}

/// Worker → main message about prepared-source cache state. Main keeps a
/// `HashSet<PathBuf>` of paths the worker currently has decoded so the
/// debounce can switch to `DEBOUNCE_HOT` on cache hits.
#[derive(Debug)]
enum CacheEvent {
    Cached(PathBuf),
    Evicted(PathBuf),
}

/// Key for the worker's prepared-source LRU. `target_bucket` quantizes the
/// requested target so terminal-resize jitter doesn't churn the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceKey {
    path: PathBuf,
    source_fp: u64,
    target_bucket: (i32, i32),
}

/// Prepared source + the file metadata derived during decode (kept here so
/// the worker doesn't need to redecode `Loaded` for `FileMeta` on cache hit).
struct SourceEntry {
    prepared: Arc<PreparedSource>,
    meta: FileMeta,
}

/// Quantize a TargetSize into a coarse bucket. Two targets sit in the same
/// bucket when both dimensions are within ~25% — matches the existing
/// `target_changed_meaningfully` decision so cache reuse aligns with re-render
/// decisions. Log-base-1.25 puts adjacent integer buckets ~25% apart.
fn target_bucket(t: TargetSize) -> (i32, i32) {
    fn b(v: u32) -> i32 {
        if v == 0 {
            return 0;
        }
        ((v as f64).ln() / 1.25_f64.ln()).round() as i32
    }
    (b(t.max_w), b(t.max_h))
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run(app: &mut App, disk_cache: DiskCache) -> Result<()> {
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
            Picker::from_fontsize((1, 2))
        }
    };
    let is_tmux = detect_is_tmux();

    // Develop job channel is bounded(1) so the queue can hold at most "the
    // next pending job" while one is in flight. The cancel-flag pattern
    // (current_cancel) supersedes stale work, so a try_send that fails
    // because the channel is full just means the most recent dispatch
    // already covers the latest selection.
    let (job_tx, job_rx) = crossbeam_channel::bounded::<Job>(1);
    let (prefetch_tx, prefetch_rx) = crossbeam_channel::bounded::<Job>(2);
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<JobDone>();
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<Event>();
    let (cache_tx, cache_rx) = crossbeam_channel::unbounded::<CacheEvent>();
    let (save_tx, save_rx) = crossbeam_channel::unbounded::<SaveMsg>();

    // Save worker owns its own Db connection and a clone of the disk cache.
    // WAL + busy_timeout (set in `Db::with_connection`) lets it write
    // concurrently with the main thread's reads/touch_access calls.
    let save_db = crate::db::Db::open_global().context("failed to open save-worker DB")?;
    let save_handle = spawn_save_worker(save_rx, save_db, disk_cache.clone());

    spawn_event_thread(event_tx);
    spawn_worker(job_rx, prefetch_rx, done_tx, cache_tx);

    let mut mem_cache: LruCache<PathBuf, PreviewEntry> =
        LruCache::new(NonZeroUsize::new(PREVIEW_CACHE_CAP).unwrap());
    let mut last_id: Option<i64> = None;
    let mut last_target: Option<TargetSize> = None;
    let mut last_rendered_fp: Option<u64> = None;
    let mut params_dirty_at: Option<Instant> = None;
    let mut last_dirty_fp: Option<u64> = None;
    let mut current_generation: u64 = 0;
    let mut current_cancel: Option<Arc<AtomicBool>> = None;
    let mut latest_generation: HashMap<PathBuf, u64> = HashMap::new();
    // Paths the worker currently has prepared in its source cache. Drives
    // the tiered debounce: hot paths get DEBOUNCE_HOT, cold get DEBOUNCE_COLD.
    let mut hot_paths: HashSet<PathBuf> = HashSet::new();
    // Active prefetch jobs keyed by path. Each Arc<AtomicBool> is the cancel
    // flag for that job; we flip it when the path falls outside the cursor±1
    // window. Removed when the matching JobDone arrives or on cancellation.
    let mut prefetch_cancels: HashMap<PathBuf, Arc<AtomicBool>> = HashMap::new();
    // Held-key nav state: rate-limits cursor advances during a burst (slow
    // for the first second, then full speed), and serves as the "user is
    // still pressing keys" signal that gates image dispatch via NAV_SETTLE.
    let mut nav = NavCoalesce::default();
    // Path the preview pane is currently showing. Updated only on settled
    // iterations once the target path is in mem_cache, so a held-key burst
    // doesn't flash the preview through every previously-cached file the
    // cursor scrolls over.
    let mut displayed_path: Option<PathBuf> = None;
    // Tracks the previous frame's view. When it transitions modal→Main the
    // loop calls `terminal.clear()` to force a full redraw — ratatui-image's
    // kitty backend leaves stale image fragments on screen after a Clear
    // overlay if we only rely on buffer-diff updates. The clear forces every
    // cell (including kitty placeholders) to re-emit on the next draw.
    let mut prev_view = app.view;

    loop {
        // 0. Drain all queued input events first. With OS key-repeat at
        //    ~30 Hz, holding j/k produces a burst that would otherwise force
        //    one render-decision + terminal.draw per event; coalescing folds
        //    the burst into a single iteration. Any further events that
        //    arrive while we render get caught by the next iteration's drain
        //    or the recv(event_rx) arm in select!.
        match drain_input_events(app, &event_rx, &save_tx, &mut nav) {
            EventDrain::Quit => {
                if pending_develop_change(app) {
                    flush_pending_develop(app, &save_tx, now_unix());
                }
                break;
            }
            EventDrain::Continue => {}
        }

        // 1. Pending-edit detection. The timer resets on every fingerprint
        //    change; once the user stops adjusting and the active debounce
        //    elapses, we flush. Debounce is tiered: hot paths (worker has
        //    decoded source cached) use DEBOUNCE_HOT, cold paths DEBOUNCE_COLD.
        let pending = pending_develop_change(app);
        if pending {
            let live_fp = app.develop_params.fingerprint();
            if last_dirty_fp != Some(live_fp) {
                params_dirty_at = Some(Instant::now());
                last_dirty_fp = Some(live_fp);
            }
        } else {
            params_dirty_at = None;
            last_dirty_fp = None;
        }

        let active_debounce = match app.current().map(|e| &e.file.canonical_path) {
            Some(p) if hot_paths.contains(p) => DEBOUNCE_HOT,
            _ => DEBOUNCE_COLD,
        };

        // 2. Debounced flush: persist pending edits + commit once the timer
        //    has lain idle for the active debounce window.
        if pending && params_dirty_at.is_some_and(|t| t.elapsed() >= active_debounce) {
            flush_pending_develop(app, &save_tx, now_unix());
            params_dirty_at = None;
            last_dirty_fp = None;
        }

        // 3. Compute the current selection state and decide whether to render.
        let current_id = app.current().map(|e| e.id);
        let current_path = app.current().map(|e| e.file.canonical_path.clone());
        let current_source_fp = app.current().map(|e| e.source_fp);
        let render_fp = app.current().map(|e| e.develop_params_fp);
        let size_now = app.current().map(|e| e.file.size_bytes).unwrap_or(0);

        let size = terminal.size().unwrap_or(Size {
            width: 80,
            height: 24,
        });
        let banner_h = banner::height_for(size.width);
        let target = preview_target(size, banner_h, picker.font_size());

        let target_changed = last_target
            .map(|prev| target_changed_meaningfully(prev, target))
            .unwrap_or(true);
        let selection_changed = current_id != last_id;
        let render_fp_changed = render_fp != last_rendered_fp;

        // "User has stopped pressing nav keys for NAV_SETTLE." Anchored on
        // the most recent nav *input*, not on the most recent cursor advance,
        // so rate-limited advances during a burst don't accidentally let
        // nav_settled flip true between two advances.
        let nav_settled = nav
            .last_input_at
            .map(|t| t.elapsed() >= NAV_SETTLE)
            .unwrap_or(true);

        if (selection_changed || target_changed || render_fp_changed) && current_id.is_some() {
            if let (Some(path), Some(src_fp), Some(rfp)) =
                (current_path.clone(), current_source_fp, render_fp)
            {
                // Cancel any in-flight job from the prior generation. Always
                // do this so a stale develop doesn't land after the user has
                // moved on.
                if let Some(c) = current_cancel.take() {
                    c.store(true, Ordering::Relaxed);
                }

                let in_memory_hit = mem_cache.peek(&path).is_some_and(|e| {
                    !target_changed_meaningfully(e.rendered_target, target)
                        && e.params_fingerprint == rfp
                });

                // A "resolved" iteration is one where we either found the
                // preview (mem/disk hit) or actually dispatched a develop
                // job. Only on resolved iterations do we update the
                // last_target/last_id/last_rendered_fp markers — otherwise
                // the next iteration sees selection_changed again and
                // retries once nav_settled flips true.
                let mut resolved = in_memory_hit;

                if !in_memory_hit && nav_settled {
                    // Both disk-cache install and worker dispatch are gated on
                    // nav_settled — during a held burst we don't want to
                    // install previously-decoded entries for every cursor
                    // position the user scrolls through (each install mutates
                    // mem_cache and re-transmits a kitty image, which would
                    // be visibly flashed in the preview pane below).
                    if let Some(srgb8) =
                        disk_cache.get(&path, src_fp, rfp, &mut app.db, now_unix())
                    {
                        install_in_memory(
                            &mut mem_cache,
                            &picker,
                            is_tmux,
                            &path,
                            target,
                            rfp,
                            srgb8,
                            app,
                        );
                        resolved = true;
                    } else {
                        // Settled (no nav input for NAV_SETTLE → user has
                        // released the key): dispatch a real develop job for
                        // the cursor's final position.
                        current_generation = current_generation.saturating_add(1);
                        let cancel = Arc::new(AtomicBool::new(false));
                        current_cancel = Some(cancel.clone());
                        latest_generation.insert(path.clone(), current_generation);
                        let _ = job_tx.try_send(Job {
                            path: path.clone(),
                            target,
                            cancel,
                            generation: current_generation,
                            params: app.develop_params.clone(),
                            params_fingerprint: rfp,
                            source_fp: src_fp,
                            size_bytes: size_now,
                            kind: JobKind::Foreground,
                            look_registry: Arc::clone(&app.look_registry),
                        });
                        resolved = true;
                    }
                }
                // else (still scrolling): defer to the next iteration after
                // NAV_SETTLE elapses. The preview pane is frozen on the last
                // settled path via `displayed_path` below, so the user
                // doesn't see flicker.

                if resolved {
                    last_target = Some(target);
                    last_id = current_id;
                    last_rendered_fp = render_fp;
                }
            }

            // Prefetch ±1 only when settled — during a burst the window
            // moves every iteration, so any prefetch we queue would be
            // cancelled on the next tick anyway.
            if nav_settled {
                update_prefetch(
                    app,
                    target,
                    &mem_cache,
                    &mut prefetch_cancels,
                    &prefetch_tx,
                );
            }
        } else if selection_changed {
            // No current selection (empty list) — clear tracking.
            last_id = current_id;
            last_rendered_fp = None;
        }

        // Decide what the preview pane should show this frame. While a
        // burst is active we keep showing the last settled image so the
        // user has a stable reference; once the burst settles and the new
        // target is in mem_cache, switch.
        if nav_settled
            && let Some(curr_path) = current_path.as_ref()
            && mem_cache.contains(curr_path)
        {
            displayed_path = Some(curr_path.clone());
        }

        // 4. Draw. On a modal→Main transition (Filter/Looks modal closing),
        //    force ratatui to repaint every cell from scratch. ratatui's
        //    diff would otherwise treat the modal-overlapped cells as
        //    already-correct (its previous-buffer reflects what we wrote,
        //    not what the terminal actually rendered), leaving stale
        //    modal pixels visible. We don't touch `mem_cache` or kitty's
        //    image storage — the StatefulProtocol re-emits its placeholders
        //    on every render call, and the kitty image stays in terminal
        //    storage across `\x1b[2J`, so placeholders re-bind to it once
        //    the next draw runs.
        if prev_view != View::Main && app.view == View::Main {
            terminal
                .clear()
                .context("failed to clear terminal on modal close")?;
        }
        prev_view = app.view;
        let font_size = picker.font_size();
        terminal
            .draw(|frame| draw(frame, app, &mut mem_cache, displayed_path.as_deref(), font_size))
            .context("failed to draw frame")?;

        // 5. Wait for events, bounding the timeout by the remaining debounce
        //    and (if a navigation burst is in progress) by the remaining
        //    settle window so we can pick up dispatch as soon as the cursor
        //    stops moving.
        let mut timeout = TICK;
        if let Some(t) = params_dirty_at {
            timeout = timeout.min(active_debounce.saturating_sub(t.elapsed()));
        }
        // Wake exactly when the nav-settle window elapses so dispatch
        // kicks in on key-up without waiting for the next TICK.
        if let Some(t) = nav.last_input_at {
            let remaining = NAV_SETTLE.saturating_sub(t.elapsed());
            if !remaining.is_zero() {
                timeout = timeout.min(remaining);
            }
        }
        let timeout = timeout.max(Duration::from_millis(5));

        select! {
            recv(event_rx) -> ev => {
                // Process this single event inline; siblings that arrive
                // while we're working are caught by the next iteration's
                // drain at the top of the loop.
                let Ok(ev) = ev else { break; };
                if let Event::Key(key) = ev
                    && key.kind == KeyEventKind::Press
                    && process_key(app, key, &save_tx, &mut nav) == Action::Quit
                {
                    if pending_develop_change(app) {
                        flush_pending_develop(app, &save_tx, now_unix());
                    }
                    break;
                }
            }
            recv(done_rx) -> done => {
                let Ok(done) = done else { break; };
                handle_job_done(
                    done,
                    &mut mem_cache,
                    &save_tx,
                    &picker,
                    is_tmux,
                    app,
                    &latest_generation,
                    &mut prefetch_cancels,
                );
            }
            recv(cache_rx) -> ev => {
                let Ok(ev) = ev else { break; };
                match ev {
                    CacheEvent::Cached(p) => { hot_paths.insert(p); }
                    CacheEvent::Evicted(p) => { hot_paths.remove(&p); }
                }
            }
            default(timeout) => {}
        }
    }

    // Free terminal-side image storage (kitty graphics) for everything we
    // had cached so we don't leave a session's worth of orphaned images on
    // the terminal after we exit.
    for (_, entry) in mem_cache.iter() {
        release_terminal_image(entry, &picker, is_tmux);
    }

    // Shutdown discipline: drop the worker channels so the develop thread
    // exits, then drop the save channel and join the save worker so any
    // pending DB writes / disk-cache blobs land before the alternate-screen
    // guard tears down.
    drop(job_tx);
    drop(prefetch_tx);
    drop(save_tx);
    let _ = save_handle.join();
    Ok(())
}

fn pending_develop_change(app: &App) -> bool {
    let Some(entry) = app.current() else {
        return false;
    };
    let current_fp = app.develop_params.fingerprint();
    current_fp != entry.develop_params_fp
}

/// Commit pending knob edits in-memory immediately and queue the persistent
/// write for the save worker. Returns without blocking on disk; if the user
/// quits, the save worker is joined so this write still lands.
fn flush_pending_develop(app: &mut App, save_tx: &Sender<SaveMsg>, now: i64) {
    let Some(entry) = app.current() else { return };
    let id = entry.id;
    let current_fp = app.develop_params.fingerprint();
    if current_fp == entry.develop_params_fp {
        return;
    }
    let params = app.develop_params.clone();
    // In-memory first so subsequent renders see the new fp without waiting
    // on the DB. The save worker handles the actual write asynchronously.
    app.commit_develop_params(current_fp);
    let _ = save_tx.send(SaveMsg::Params {
        file_id: id,
        params,
        fp: current_fp,
        now,
    });
}

fn install_in_memory(
    mem_cache: &mut LruCache<PathBuf, PreviewEntry>,
    picker: &Picker,
    is_tmux: bool,
    path: &Path,
    target: TargetSize,
    params_fp: u64,
    srgb8: Srgb8,
    app: &mut App,
) {
    let (w, h) = (srgb8.width, srgb8.height);
    if let Some(buf) = ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, srgb8.pixels) {
        let entry = PreviewEntry {
            proto: picker.new_resize_protocol(DynamicImage::ImageRgb8(buf)),
            src_w: w,
            src_h: h,
            rendered_target: target,
            params_fingerprint: params_fp,
        };
        // Replace any prior protocol for the same path (otherwise its kitty
        // image data leaks on the terminal side until LRU eviction).
        if let Some(prev) = mem_cache.pop(path) {
            release_terminal_image(&prev, picker, is_tmux);
        }
        // `push` (vs `put`) returns the LRU-evicted entry so we can free its
        // terminal-side resources. Without this, kitty's graphics quota fills
        // and the terminal evicts images whose unicode placeholders we still
        // emit, leaving the preview pane blank.
        if let Some((_, evicted)) = mem_cache.push(path.to_path_buf(), entry) {
            release_terminal_image(&evicted, picker, is_tmux);
        }
        if app
            .status
            .as_deref()
            .is_some_and(|s| s.starts_with("preview error"))
        {
            app.status = None;
        }
    } else {
        app.status = Some("preview error: rgb buffer shape mismatch".into());
    }
}

/// Mirror of `ratatui_image::picker::detect_tmux_and_outer_protocol_from_env`
/// since `Picker` doesn't expose its `is_tmux` flag. Used so the kitty delete
/// escape we emit on cache eviction is wrapped the same way the transmit was.
fn detect_is_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "tmux")
}

/// Free the terminal-side resources backing a `PreviewEntry` we are about to
/// drop. Only the kitty unicode-placeholder backend has terminal-side state
/// (an image transmitted under a 32-bit id, referenced later by placeholder
/// cells); sixel / iterm2 / halfblocks carry pixels inline per render and
/// have nothing to release. Without this, kitty/Ghostty/WezTerm hit their
/// graphics-storage quota after a few hundred previews and start evicting
/// images that are still referenced by our cache, which surfaces as a black
/// preview pane.
fn release_terminal_image(entry: &PreviewEntry, picker: &Picker, is_tmux: bool) {
    use std::fmt::Write as _;
    use std::io::Write as _;

    if picker.protocol_type() != ProtocolType::Kitty {
        return;
    }
    let StatefulProtocol::Kitty(ref k) = entry.proto else {
        return;
    };
    let id = k.unique_id;

    let (start, escape, end) = CapParser::escape_tmux(is_tmux);
    let mut seq = String::with_capacity(48);
    seq.push_str(start);
    seq.push_str(escape);
    let _ = write!(seq, "_Ga=d,d=I,i={id};");
    seq.push_str(escape);
    seq.push('\\');
    seq.push_str(end);

    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

/// Result of [`drain_input_events`]: whether the loop should keep going or
/// exit because the user pressed quit.
#[derive(PartialEq, Eq)]
enum EventDrain {
    Continue,
    Quit,
}

/// State for rate-limiting held-key navigation. While the user holds j/k the
/// OS emits a key-repeat burst at ~30 Hz; without this we'd scrub the cursor
/// through 30 files per second and image dispatch would chase a moving
/// target. The policy:
///  1. The first press of a tap or fresh burst advances immediately.
///  2. While the burst continues (same key, gaps < `NAV_BURST_GAP`), advances
///     are rate-limited to `NAV_SLOW_INTERVAL` (5 Hz) for the first
///     `NAV_RAMP_DURATION` so the user can read what's scrolling past.
///  3. After the ramp, advances run at the full incoming event rate.
///  4. Image dispatch is gated on `last_input_at`, not `last_advance_at`, so
///     no preview decode kicks off until the user releases the key
///     (detected as `NAV_SETTLE` of input silence).
#[derive(Default)]
struct NavCoalesce {
    /// Timestamp of the most recent nav key event we received, advance or
    /// not. Drives "user has released the key" detection.
    last_input_at: Option<Instant>,
    /// Direction of the most recent nav key, for is-this-a-continuation.
    last_key: Option<KeyCode>,
    /// When the current burst started — needed to switch from slow to fast
    /// once `NAV_RAMP_DURATION` has passed.
    burst_started_at: Option<Instant>,
    /// When we last actually advanced the cursor — drives the slow-phase
    /// rate limit.
    last_advance_at: Option<Instant>,
}

/// Apply every queued input event without blocking. Returns [`EventDrain::Quit`]
/// if any handler signaled quit (caller is responsible for the flush + break).
fn drain_input_events(
    app: &mut App,
    event_rx: &Receiver<Event>,
    save_tx: &Sender<SaveMsg>,
    nav: &mut NavCoalesce,
) -> EventDrain {
    while let Ok(ev) = event_rx.try_recv() {
        if let Event::Key(key) = ev
            && key.kind == KeyEventKind::Press
            && process_key(app, key, save_tx, nav) == Action::Quit
        {
            return EventDrain::Quit;
        }
    }
    EventDrain::Continue
}

/// Route a Press key event: navigation keys go through the coalescer first;
/// everything else hits `handle_key` as before.
fn process_key(
    app: &mut App,
    key: KeyEvent,
    save_tx: &Sender<SaveMsg>,
    nav: &mut NavCoalesce,
) -> Action {
    if try_coalesce_nav(app, key, save_tx, nav) {
        return Action::Continue;
    }
    handle_key(app, key, save_tx)
}

/// Returns true if `key` was a navigation key in Navigation focus and was
/// handled by the coalescer. Otherwise the caller falls through to `handle_key`.
fn try_coalesce_nav(
    app: &mut App,
    key: KeyEvent,
    save_tx: &Sender<SaveMsg>,
    nav: &mut NavCoalesce,
) -> bool {
    if app.view != View::Main || app.focus != Focus::Navigation {
        return false;
    }
    let delta: i32 = match key.code {
        KeyCode::Char('j') | KeyCode::Down => 1,
        KeyCode::Char('k') | KeyCode::Up => -1,
        _ => return false,
    };
    let now = Instant::now();
    let in_burst = nav
        .last_input_at
        .map(|t| now.duration_since(t) < NAV_BURST_GAP)
        .unwrap_or(false)
        && nav.last_key == Some(key.code);

    let should_advance = if in_burst {
        // Mid-burst: rate-limit. Slow phase for the first NAV_RAMP_DURATION,
        // then full speed.
        let burst_elapsed = nav
            .burst_started_at
            .map(|t| now.duration_since(t))
            .unwrap_or(Duration::ZERO);
        let interval = if burst_elapsed < NAV_RAMP_DURATION {
            NAV_SLOW_INTERVAL
        } else {
            Duration::ZERO
        };
        nav.last_advance_at
            .map(|t| now.duration_since(t) >= interval)
            .unwrap_or(true)
    } else {
        // Fresh tap / new direction / gap longer than auto-repeat: this is
        // the start of a new burst. Always advance the first event.
        nav.burst_started_at = Some(now);
        true
    };

    if should_advance {
        flush_pending_develop(app, save_tx, now_unix());
        if delta > 0 {
            app.next();
        } else {
            app.prev();
        }
        nav.last_advance_at = Some(now);
    }

    nav.last_input_at = Some(now);
    nav.last_key = Some(key.code);
    true
}

/// Spawn the save worker. Owns its own `Db` connection and a clone of the
/// disk cache; closes when its receiver returns Err (i.e. the sender is
/// dropped). The returned handle is joined on shutdown so pending writes
/// land before the alternate-screen guard tears down.
fn spawn_save_worker(
    rx: Receiver<SaveMsg>,
    mut db: crate::db::Db,
    disk_cache: DiskCache,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                SaveMsg::Params {
                    file_id,
                    params,
                    fp,
                    now,
                } => {
                    // Errors are not surfaced to the UI thread in v1; WAL +
                    // busy_timeout makes failures rare. The next successful
                    // write recovers the row.
                    let _ = db.update_params(file_id, &params, fp, now);
                }
                SaveMsg::CacheBlob {
                    path,
                    source_fp,
                    params_fp,
                    srgb,
                    now,
                } => {
                    let _ = disk_cache.insert(&path, source_fp, params_fp, &srgb, &mut db, now);
                }
            }
        }
    })
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

fn spawn_worker(
    fg_rx: Receiver<Job>,
    prefetch_rx: Receiver<Job>,
    tx: Sender<JobDone>,
    cache_tx: Sender<CacheEvent>,
) {
    thread::spawn(move || {
        let mut source_cache: LruCache<SourceKey, SourceEntry> =
            LruCache::new(NonZeroUsize::new(SOURCE_CACHE_CAP).unwrap());
        loop {
            // Drain foreground queue first. Prefetch is best-effort and only
            // runs when the user is idle (no pending fg jobs).
            while let Ok(job) = fg_rx.try_recv() {
                if process_job(job, &mut source_cache, &tx, &cache_tx).is_err() {
                    return;
                }
            }
            // Block on either channel. If either disconnects, shut down.
            select! {
                recv(fg_rx) -> j => match j {
                    Ok(job) => {
                        if process_job(job, &mut source_cache, &tx, &cache_tx).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                recv(prefetch_rx) -> j => match j {
                    Ok(job) => {
                        if process_job(job, &mut source_cache, &tx, &cache_tx).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
            }
        }
    });
}

/// Run a single develop job through the (cached) prepare → apply pipeline.
/// Returns `Err(())` only when the JobDone channel is closed (shutdown).
fn process_job(
    job: Job,
    source_cache: &mut LruCache<SourceKey, SourceEntry>,
    tx: &Sender<JobDone>,
    cache_tx: &Sender<CacheEvent>,
) -> std::result::Result<(), ()> {
    let Job {
        path,
        target,
        cancel,
        generation,
        params,
        params_fingerprint,
        source_fp,
        size_bytes,
        kind,
        look_registry,
    } = job;

    if cancel.load(Ordering::Relaxed) {
        return tx
            .send(JobDone {
                path,
                generation,
                target,
                params_fingerprint,
                source_fp,
                result: Err(DevelopError::Cancelled),
                meta: None,
                kind,
            })
            .map_err(|_| ());
    }

    let key = SourceKey {
        path: path.clone(),
        source_fp,
        target_bucket: target_bucket(target),
    };

    let (prepared, meta) = if let Some(entry) = source_cache.get(&key) {
        (entry.prepared.clone(), Some(entry.meta.clone()))
    } else {
        match decode(&path) {
            Ok(loaded) => {
                let meta = file_meta_from(&loaded, size_bytes);
                match prepare_source(&loaded, target, Some(&cancel)) {
                    Ok(prep) => {
                        let prepared = Arc::new(prep);
                        let entry = SourceEntry {
                            prepared: prepared.clone(),
                            meta: meta.clone(),
                        };
                        if let Some((evicted_key, _)) = source_cache.push(key, entry) {
                            let _ = cache_tx.send(CacheEvent::Evicted(evicted_key.path));
                        }
                        let _ = cache_tx.send(CacheEvent::Cached(path.clone()));
                        (prepared, Some(meta))
                    }
                    Err(e) => {
                        return tx
                            .send(JobDone {
                                path,
                                generation,
                                target,
                                params_fingerprint,
                                source_fp,
                                result: Err(e),
                                meta: Some(meta),
                                kind,
                            })
                            .map_err(|_| ());
                    }
                }
            }
            Err(e) => {
                return tx
                    .send(JobDone {
                        path,
                        generation,
                        target,
                        params_fingerprint,
                        source_fp,
                        result: Err(DevelopError::Decode(e)),
                        meta: None,
                        kind,
                    })
                    .map_err(|_| ());
            }
        }
    };

    let result = apply_pipeline(&prepared, &params, &look_registry, Some(&cancel));
    tx.send(JobDone {
        path,
        generation,
        target,
        params_fingerprint,
        source_fp,
        result,
        meta,
        kind,
    })
    .map_err(|_| ())
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
    mem_cache: &mut LruCache<PathBuf, PreviewEntry>,
    save_tx: &Sender<SaveMsg>,
    picker: &Picker,
    is_tmux: bool,
    app: &mut App,
    latest_generation: &HashMap<PathBuf, u64>,
    prefetch_cancels: &mut HashMap<PathBuf, Arc<AtomicBool>>,
) {
    let is_prefetch = done.kind == JobKind::Prefetch;

    if is_prefetch {
        // The cancel flag for this prefetch is still in the map; remove it
        // now that the job has reported back (Ok or Err).
        prefetch_cancels.remove(&done.path);
    } else if latest_generation.get(&done.path).copied() != Some(done.generation) {
        return;
    }

    if let Some(meta) = done.meta {
        app.file_meta.insert(done.path.clone(), meta);
    }

    match done.result {
        Ok(rgb) => {
            // Update the in-memory cache first so the screen refreshes
            // immediately (or so a future selection finds the warm entry).
            // The on-disk write is shipped to the save worker.
            let blob = SaveMsg::CacheBlob {
                path: done.path.clone(),
                source_fp: done.source_fp,
                params_fp: done.params_fingerprint,
                srgb: rgb.clone(),
                now: now_unix(),
            };
            install_in_memory(
                mem_cache,
                picker,
                is_tmux,
                &done.path,
                done.target,
                done.params_fingerprint,
                rgb,
                app,
            );
            let _ = save_tx.send(blob);
        }
        Err(DevelopError::Cancelled) => {}
        Err(e) => {
            // Don't surface prefetch errors to the user — they're best-effort.
            if !is_prefetch {
                app.status = Some(format!("preview error: {e}"));
            }
        }
    }
}

/// Maintain the prefetch window around the cursor: cancel + drop any
/// in-flight prefetch whose path is no longer cursor±1, and queue prefetch
/// jobs for cursor-1 / cursor+1 if they're not already in mem_cache or
/// already pending. Best-effort: bounded(2) with try_send means the worker
/// can refuse a third candidate, which is fine — the next call retries.
fn update_prefetch(
    app: &App,
    target: TargetSize,
    mem_cache: &LruCache<PathBuf, PreviewEntry>,
    prefetch_cancels: &mut HashMap<PathBuf, Arc<AtomicBool>>,
    prefetch_tx: &Sender<Job>,
) {
    let cursor = app.cursor;
    let visible = &app.visible;

    // Compute the window: cursor-1 and cursor+1 (if they exist).
    let mut window: Vec<usize> = Vec::with_capacity(2);
    if cursor > 0 {
        if let Some(&i) = visible.get(cursor - 1) {
            window.push(i);
        }
    }
    if let Some(&i) = visible.get(cursor + 1) {
        window.push(i);
    }
    let window_paths: HashSet<&PathBuf> = window
        .iter()
        .map(|&i| &app.files[i].file.canonical_path)
        .collect();

    // Cancel any prefetches that have fallen outside the window.
    prefetch_cancels.retain(|path, cancel| {
        if window_paths.contains(path) {
            true
        } else {
            cancel.store(true, Ordering::Relaxed);
            false
        }
    });

    // Queue new prefetches for window slots that aren't already cached or
    // already in flight.
    for idx in window {
        let entry = &app.files[idx];
        let path = &entry.file.canonical_path;
        if mem_cache.contains(path) {
            continue;
        }
        if prefetch_cancels.contains_key(path) {
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        prefetch_cancels.insert(path.clone(), cancel.clone());
        let _ = prefetch_tx.try_send(Job {
            path: path.clone(),
            target,
            cancel,
            generation: 0, // unused for prefetch
            params: entry.develop_params.clone(),
            params_fingerprint: entry.develop_params_fp,
            source_fp: entry.source_fp,
            size_bytes: entry.file.size_bytes,
            kind: JobKind::Prefetch,
            look_registry: Arc::clone(&app.look_registry),
        });
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    app: &mut App,
    mem_cache: &mut LruCache<PathBuf, PreviewEntry>,
    displayed_path: Option<&Path>,
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

    preview::render(frame, app, mem_cache, displayed_path, preview_area, font_size);
    develop::render(frame, app, develop_area);
    info::render(frame, app, info_area);
    filmstrip::render(frame, app, filmstrip_area);
    status::render(frame, app, status_area);

    if app.view == View::Filter {
        filter::render(frame, app);
    } else if app.view == View::Looks {
        looks::render(frame, app);
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

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Continue,
    Quit,
}

fn handle_key(app: &mut App, key: KeyEvent, save_tx: &Sender<SaveMsg>) -> Action {
    match app.view {
        View::Filter => handle_filter_key(app, key.code),
        View::Looks => handle_looks_key(app, key.code, save_tx),
        View::Main => match app.focus {
            Focus::Navigation => handle_navigation_key(app, key, save_tx),
            Focus::Develop => handle_develop_key(app, key.code, save_tx),
        },
    }
}

fn handle_navigation_key(app: &mut App, key: KeyEvent, save_tx: &Sender<SaveMsg>) -> Action {
    let now = now_unix();
    match key.code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => {
            flush_pending_develop(app, save_tx, now);
            app.next();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            flush_pending_develop(app, save_tx, now);
            app.prev();
        }
        KeyCode::Char('x') => {
            flush_pending_develop(app, save_tx, now);
            app.remove_current(now);
        }
        // Lowercase 'r' restores the current entry (no-op if not removed).
        // Uppercase 'R' toggles "show removed" view. crossterm sends 'R' as
        // Char('R') with KeyModifiers::SHIFT.
        KeyCode::Char('r') => {
            flush_pending_develop(app, save_tx, now);
            app.restore_current(now);
        }
        KeyCode::Char('R') => {
            flush_pending_develop(app, save_tx, now);
            app.toggle_show_removed();
        }
        KeyCode::Char('f') => {
            flush_pending_develop(app, save_tx, now);
            app.open_filter();
        }
        KeyCode::Char('L') => {
            flush_pending_develop(app, save_tx, now);
            open_looks_modal(app, now);
        }
        KeyCode::Enter => {
            flush_pending_develop(app, save_tx, now);
            app.enter_develop();
        }
        _ => {}
    }
    Action::Continue
}

fn handle_develop_key(app: &mut App, code: KeyCode, save_tx: &Sender<SaveMsg>) -> Action {
    match code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Esc => {
            flush_pending_develop(app, save_tx, now_unix());
            app.exit_develop();
        }
        KeyCode::Char('j') | KeyCode::Down => app.develop_next(),
        KeyCode::Char('k') | KeyCode::Up => app.develop_prev(),
        KeyCode::Char('h') | KeyCode::Left => app.develop_adjust(-1.0),
        KeyCode::Char('l') | KeyCode::Right => app.develop_adjust(1.0),
        KeyCode::Char('r') => app.develop_reset(),
        KeyCode::Char('L') => {
            // Capital L opens the Looks modal from either focus, so users
            // don't have to remember which focus they're in.
            let now = now_unix();
            flush_pending_develop(app, save_tx, now);
            open_looks_modal(app, now);
        }
        KeyCode::Enter => {
            // Enter on the LookSelector knob opens the Looks modal.
            // Anywhere else, Enter is inert.
            if let Some(&(_, knob)) = DEVELOP_KNOBS.get(app.develop_cursor) {
                if knob == DevelopKnob::LookSelector {
                    let now = now_unix();
                    flush_pending_develop(app, save_tx, now);
                    open_looks_modal(app, now);
                }
            }
        }
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

fn handle_looks_key(app: &mut App, code: KeyCode, _save_tx: &Sender<SaveMsg>) -> Action {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.looks_next(),
        KeyCode::Char('k') | KeyCode::Up => app.looks_prev(),
        KeyCode::Enter => {
            app.looks_apply_to_current();
            app.close_looks();
        }
        KeyCode::Esc | KeyCode::Char('L') => app.close_looks(),
        _ => {}
    }
    Action::Continue
}

/// Resolve the watch directory and ask `App` to reconcile + open the modal.
/// On directory-resolution failure, surface an error in the status line and
/// don't open the modal (no point in showing an empty list with no way to
/// discover XMPs).
fn open_looks_modal(app: &mut App, now: i64) {
    match paths::looks_dir() {
        Ok(dir) => app.open_looks(&dir, now),
        Err(e) => app.status = Some(format!("looks dir unavailable: {e}")),
    }
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

pub(crate) fn tab_block(title: &'static str, focused: bool) -> Block<'static> {
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block = block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Yellow));
    }
    block
}

#[cfg(test)]
mod dispatch_tests {
    //! Integration-style tests that route a `KeyEvent` through `handle_key`
    //! to verify the modal dispatch chain end-to-end. Caught by these
    //! tests: any future regression where opening the Looks modal silently
    //! routes keys to navigation/develop instead.
    use super::*;
    use crate::app::App;
    use crate::db::Db;
    use crate::session::{DiscoveredFile, Session};
    use crossterm::event::KeyModifiers;
    use darkroom::ImageKind;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn build_app() -> (App, Arc<crossbeam_channel::Sender<SaveMsg>>) {
        let mut db = Db::open_in_memory().unwrap();
        let f = DiscoveredFile {
            canonical_path: PathBuf::from("/t/a.cr3"),
            display_name: "a.cr3".into(),
            size_bytes: 100,
            modified_unix_seconds: 1000,
            kind: ImageKind::Raw,
        };
        let row = db.upsert_file(&f, 1000).unwrap();
        let session = Session {
            root: PathBuf::from("/t"),
            files: vec![f],
        };
        let app = App::init(session, db, vec![row]).unwrap();
        let (tx, _rx) = crossbeam_channel::unbounded::<SaveMsg>();
        (app, Arc::new(tx))
    }

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn looks_modal_consumes_navigation_keys_not_main_view() {
        let (mut app, save_tx) = build_app();
        // Manually open the modal. (open_looks_modal calls paths::looks_dir
        // which would create real ~/.terminalroom/looks — bypass that here.)
        app.view = crate::app::View::Looks;
        // Inject two fake registered looks so j/k actually has somewhere to go.
        app.looks = vec![
            crate::db::LookRow {
                id: 1, slug: "xmp:1".into(), name: "Alpha".into(),
                source_path: PathBuf::from("/dev/null"), xmp_xml: "".into(),
                source_fp: 1, created_at_unix_seconds: 0,
            },
            crate::db::LookRow {
                id: 2, slug: "xmp:2".into(), name: "Beta".into(),
                source_path: PathBuf::from("/dev/null"), xmp_xml: "".into(),
                source_fp: 2, created_at_unix_seconds: 0,
            },
        ];
        let cursor_before_file = app.cursor;
        app.looks_cursor = 0;

        // j routes to handle_looks_key (because view is Looks), not to
        // handle_navigation_key — verify by checking app.cursor (file cursor)
        // is unchanged while looks_cursor advances.
        let action = handle_key(&mut app, k(KeyCode::Char('j')), &save_tx);
        assert_eq!(action, Action::Continue);
        assert_eq!(
            app.cursor, cursor_before_file,
            "file cursor must not move while modal is open"
        );
        assert_eq!(app.looks_cursor, 1, "j should advance looks cursor");

        // k moves back.
        handle_key(&mut app, k(KeyCode::Char('k')), &save_tx);
        assert_eq!(app.looks_cursor, 0);

        // Down/Up alternate forms.
        handle_key(&mut app, k(KeyCode::Down), &save_tx);
        assert_eq!(app.looks_cursor, 1);
        handle_key(&mut app, k(KeyCode::Up), &save_tx);
        assert_eq!(app.looks_cursor, 0);
    }

    #[test]
    fn looks_modal_enter_applies_and_closes() {
        let (mut app, save_tx) = build_app();
        app.view = crate::app::View::Looks;
        app.looks = vec![crate::db::LookRow {
            id: 1, slug: "xmp:1".into(), name: "Alpha".into(),
            source_path: PathBuf::from("/dev/null"), xmp_xml: "".into(),
            source_fp: 1, created_at_unix_seconds: 0,
        }];
        app.looks_cursor = 1; // pointing at the first registered look

        handle_key(&mut app, k(KeyCode::Enter), &save_tx);
        assert_eq!(app.view, crate::app::View::Main, "Enter should close modal");
        assert_eq!(app.develop_params.look, "xmp:1", "Enter should apply slug");
    }

    #[test]
    fn looks_modal_esc_closes_without_applying() {
        let (mut app, save_tx) = build_app();
        app.view = crate::app::View::Looks;
        let look_before = app.develop_params.look.clone();
        handle_key(&mut app, k(KeyCode::Esc), &save_tx);
        assert_eq!(app.view, crate::app::View::Main);
        assert_eq!(app.develop_params.look, look_before);
    }

    #[test]
    fn looks_modal_l_key_closes() {
        let (mut app, save_tx) = build_app();
        app.view = crate::app::View::Looks;
        handle_key(&mut app, k(KeyCode::Char('L')), &save_tx);
        assert_eq!(app.view, crate::app::View::Main);
    }


    #[test]
    fn keys_route_to_navigation_when_modal_closed() {
        let (mut app, save_tx) = build_app();
        // Add a second file so j has somewhere to go.
        let f2 = DiscoveredFile {
            canonical_path: PathBuf::from("/t/b.cr3"),
            display_name: "b.cr3".into(),
            size_bytes: 100,
            modified_unix_seconds: 1000,
            kind: ImageKind::Raw,
        };
        let row2 = app.db.upsert_file(&f2, 1000).unwrap();
        app.files.push(crate::app::FileEntry {
            id: row2.id, file: f2, removed: false,
            develop_params: row2.develop_params.clone(),
            develop_params_fp: row2.develop_params_fp,
            source_fp: row2.source_fp,
        });
        app.rebuild_visible();
        app.cursor = 0;

        // view=Main, focus=Navigation, j should advance file cursor.
        let action = handle_key(&mut app, k(KeyCode::Char('j')), &save_tx);
        assert_eq!(action, Action::Continue);
        assert_eq!(app.cursor, 1, "j in nav focus should advance file cursor");
    }
}

