use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use lru::LruCache;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::app::{App, View};
use crate::db::CullingState;
use crate::preview;

mod culling;
mod develop;
mod filter;

const TICK: Duration = Duration::from_millis(100);
const PREVIEW_CACHE_CAP: usize = 9;

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

    let mut cache: LruCache<PathBuf, StatefulProtocol> =
        LruCache::new(NonZeroUsize::new(PREVIEW_CACHE_CAP).unwrap());

    loop {
        ensure_preview_loaded(app, &picker, &mut cache);

        terminal
            .draw(|frame| draw(frame, app, &mut cache))
            .context("failed to draw frame")?;

        if event::poll(TICK).context("failed to poll terminal events")? {
            let ev = event::read().context("failed to read terminal event")?;
            if let Event::Key(key) = ev {
                if key.kind == KeyEventKind::Press {
                    if handle_key(app, key) == Action::Quit {
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn draw(frame: &mut ratatui::Frame, app: &mut App, cache: &mut LruCache<PathBuf, StatefulProtocol>) {
    match app.view {
        View::Culling => culling::render(frame, app, cache),
        View::Develop => develop::render(frame, app),
        View::Filter => {
            culling::render(frame, app, cache);
            filter::render(frame, app);
        }
    }
}

fn ensure_preview_loaded(
    app: &mut App,
    picker: &Picker,
    cache: &mut LruCache<PathBuf, StatefulProtocol>,
) {
    let Some(entry) = app.current() else {
        return;
    };
    let path = entry.file.canonical_path.clone();
    let kind = entry.file.kind;
    if cache.contains(&path) {
        return;
    }
    match preview::load_preview(&path, kind) {
        Ok(image) => {
            let proto = picker.new_resize_protocol(image);
            cache.put(path, proto);
            app.status = None;
        }
        Err(e) => {
            app.status = Some(format!("preview error: {e}"));
        }
    }
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
    Resize::Fit(None)
}
