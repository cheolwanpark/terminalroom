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
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use darkroom::{
    DevelopError, DevelopParams, ImageKind, Loaded, PreparedSource, Srgb8, TargetSize,
    apply_pipeline, decode, prepare_source,
};

use crate::app::{App, FileMeta, Focus, View};
use crate::cache::Cache as DiskCache;

mod banner;
mod develop;
mod filmstrip;
mod filter;
mod info;
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

    loop {
        // 0. Drain all queued input events first. With OS key-repeat at
        //    ~30 Hz, holding j/k produces a burst that would otherwise force
        //    one render-decision + terminal.draw per event; coalescing folds
        //    the burst into a single iteration. Any further events that
        //    arrive while we render get caught by the next iteration's drain
        //    or the recv(event_rx) arm in select!.
        match drain_input_events(app, &event_rx, &save_tx) {
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

        if (selection_changed || target_changed || render_fp_changed) && current_id.is_some() {
            if let (Some(path), Some(src_fp), Some(rfp)) =
                (current_path.clone(), current_source_fp, render_fp)
            {
                // Cancel any in-flight job from the prior generation.
                if let Some(c) = current_cancel.take() {
                    c.store(true, Ordering::Relaxed);
                }
                current_generation = current_generation.saturating_add(1);

                let in_memory_hit = mem_cache.peek(&path).is_some_and(|e| {
                    !target_changed_meaningfully(e.rendered_target, target)
                        && e.params_fingerprint == rfp
                });

                if !in_memory_hit {
                    if let Some(srgb8) =
                        disk_cache.get(&path, src_fp, rfp, &mut app.db, now_unix())
                    {
                        install_in_memory(&mut mem_cache, &picker, &path, target, rfp, srgb8, app);
                    } else {
                        let cancel = Arc::new(AtomicBool::new(false));
                        current_cancel = Some(cancel.clone());
                        latest_generation.insert(path.clone(), current_generation);
                        // Bounded(1) channel: if full, the queued job is
                        // already stale (its cancel will be flipped on the
                        // next iteration anyway). The next iteration's
                        // render-decision will retry with the latest state.
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
                        });
                    }
                }

                last_target = Some(target);
                last_id = current_id;
                last_rendered_fp = render_fp;
            }

            // After the foreground dispatch, prune stale prefetch jobs and
            // queue prefetches for cursor±1 (best-effort; the worker drains
            // foreground first).
            update_prefetch(
                app,
                target,
                &mem_cache,
                &mut prefetch_cancels,
                &prefetch_tx,
            );
        } else if selection_changed {
            // No current selection (empty list) — clear tracking.
            last_id = current_id;
            last_rendered_fp = None;
        }

        // 4. Draw.
        let font_size = picker.font_size();
        terminal
            .draw(|frame| draw(frame, app, &mut mem_cache, font_size))
            .context("failed to draw frame")?;

        // 5. Wait for events, bounding the timeout by the remaining debounce.
        let timeout = match params_dirty_at {
            Some(t) => active_debounce
                .saturating_sub(t.elapsed())
                .min(TICK)
                .max(Duration::from_millis(5)),
            None => TICK,
        };

        select! {
            recv(event_rx) -> ev => {
                // Process this single event inline; siblings that arrive
                // while we're working are caught by the next iteration's
                // drain at the top of the loop.
                let Ok(ev) = ev else { break; };
                if let Event::Key(key) = ev
                    && key.kind == KeyEventKind::Press
                    && handle_key(app, key, &save_tx) == Action::Quit
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
        mem_cache.put(path.to_path_buf(), entry);
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

/// Result of [`drain_input_events`]: whether the loop should keep going or
/// exit because the user pressed quit.
#[derive(PartialEq, Eq)]
enum EventDrain {
    Continue,
    Quit,
}

/// Apply every queued input event without blocking. Returns [`EventDrain::Quit`]
/// if any handler signaled quit (caller is responsible for the flush + break).
fn drain_input_events(
    app: &mut App,
    event_rx: &Receiver<Event>,
    save_tx: &Sender<SaveMsg>,
) -> EventDrain {
    while let Ok(ev) = event_rx.try_recv() {
        if let Event::Key(key) = ev
            && key.kind == KeyEventKind::Press
            && handle_key(app, key, save_tx) == Action::Quit
        {
            return EventDrain::Quit;
        }
    }
    EventDrain::Continue
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

    let result = apply_pipeline(&prepared, &params, Some(&cancel));
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
        });
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    app: &mut App,
    mem_cache: &mut LruCache<PathBuf, PreviewEntry>,
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

    preview::render(frame, app, mem_cache, preview_area, font_size);
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

fn handle_key(app: &mut App, key: KeyEvent, save_tx: &Sender<SaveMsg>) -> Action {
    match app.view {
        View::Filter => handle_filter_key(app, key.code),
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

pub(crate) fn tab_block(title: &'static str, focused: bool) -> Block<'static> {
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block = block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Yellow));
    }
    block
}

