use std::path::PathBuf;

use lru::LruCache;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_image::StatefulImage;
use ratatui_image::protocol::StatefulProtocol;

use crate::app::{App, FileEntry};
use crate::db::CullingState;

const FILMSTRIP_WIDTH: u16 = 28;

pub fn render(
    frame: &mut Frame,
    app: &App,
    cache: &mut LruCache<PathBuf, StatefulProtocol>,
) {
    let area = frame.area();
    let [main, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
    let [preview_area, strip_area] = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(FILMSTRIP_WIDTH),
    ])
    .areas(main);

    render_preview(frame, app, cache, preview_area);
    render_filmstrip(frame, app, strip_area);
    render_status(frame, app, status);
}

fn render_preview(
    frame: &mut Frame,
    app: &App,
    cache: &mut LruCache<PathBuf, StatefulProtocol>,
    area: Rect,
) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(entry) = app.current() else {
        let p = Paragraph::new("No images match the current filter.")
            .wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
        return;
    };

    if let Some(proto) = cache.get_mut(&entry.file.canonical_path) {
        let widget = StatefulImage::default().resize(super::resize_strategy());
        frame.render_stateful_widget(widget, inner, proto);
    } else {
        let msg = app
            .status
            .clone()
            .unwrap_or_else(|| "loading preview…".to_string());
        let p = Paragraph::new(msg).wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
    }
}

fn render_filmstrip(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Files");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.visible.is_empty() {
        let p = Paragraph::new("(empty)").wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&i| filmstrip_item(&app.files[i], inner.width as usize))
        .collect();

    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.cursor));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn filmstrip_item(entry: &FileEntry, width: usize) -> ListItem<'static> {
    let badge = match entry.state {
        CullingState::Pick => "✓",
        CullingState::Reject => "✗",
        CullingState::Unset => "·",
    };
    // Reserve 2 cols for the trailing " {badge}", clip the name to the rest.
    let usable = width.saturating_sub(3).max(1);
    let mut name = entry.file.display_name.clone();
    if name.chars().count() > usable {
        let truncated: String = name.chars().take(usable.saturating_sub(1)).collect();
        name = format!("{truncated}…");
    }
    ListItem::new(Line::from(format!("{name} {badge}")))
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let text = if let Some(entry) = app.current() {
        let state_label = match entry.state {
            CullingState::Pick => "PICK",
            CullingState::Reject => "REJECT",
            CullingState::Unset => "UNSET",
        };
        let total = app.visible.len();
        let filter_indicator = filter_indicator(app);
        let shortcuts =
            "p pick · x reject · u unset · f filter · d develop · q quit";
        let prefix = format!(
            "{}   {}/{}   {}{}",
            entry.file.display_name,
            app.cursor + 1,
            total,
            state_label,
            filter_indicator
        );
        if let Some(msg) = &app.status {
            format!("{prefix}   {msg}")
        } else {
            format!("{prefix}   {shortcuts}")
        }
    } else {
        let filter_indicator = filter_indicator(app);
        format!("(no files visible){filter_indicator}   f filter · q quit")
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn filter_indicator(app: &App) -> String {
    let total = app.available_formats.len();
    let enabled = app.enabled_count();
    if enabled == total {
        String::new()
    } else {
        format!("   filter: {enabled}/{total}")
    }
}
