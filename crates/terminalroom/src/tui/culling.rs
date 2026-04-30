use std::path::PathBuf;

use lru::LruCache;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_image::StatefulImage;

use super::PreviewEntry;
use crate::app::{App, FileEntry};
use crate::db::CullingState;

const FILMSTRIP_WIDTH: u16 = 28;

pub fn render(
    frame: &mut Frame,
    app: &App,
    cache: &mut LruCache<PathBuf, PreviewEntry>,
    font_size: (u16, u16),
) {
    let area = frame.area();
    let [main, status] = Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(area);
    let [preview_area, strip_area] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(FILMSTRIP_WIDTH)]).areas(main);

    render_preview(frame, app, cache, preview_area, font_size);
    render_filmstrip(frame, app, strip_area);
    render_status(frame, app, status);
}

fn render_preview(
    frame: &mut Frame,
    app: &App,
    cache: &mut LruCache<PathBuf, PreviewEntry>,
    area: Rect,
    font_size: (u16, u16),
) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(entry) = app.current() else {
        let p = Paragraph::new("No images match the current filter.").wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
        return;
    };

    let preview = cache.get_mut(&entry.file.canonical_path);

    if let Some(preview) = preview {
        let centered = aspect_fit_rect(inner, preview.src_w, preview.src_h, font_size);
        let widget = StatefulImage::default().resize(super::resize_strategy());
        frame.render_stateful_widget(widget, centered, &mut preview.proto);
    } else {
        let msg = app
            .status
            .clone()
            .unwrap_or_else(|| "loading preview…".to_string());
        let p = Paragraph::new(msg).wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
    }
}

/// Aspect-fit `(src_w, src_h)` (in pixels) within `area` (in cells), centered both axes.
/// Cell aspect comes from `font_size` so non-square cells still look right.
fn aspect_fit_rect(area: Rect, src_w: u32, src_h: u32, font_size: (u16, u16)) -> Rect {
    if src_w == 0 || src_h == 0 || area.width == 0 || area.height == 0 {
        return area;
    }
    let (cell_w, cell_h) = (font_size.0.max(1) as f64, font_size.1.max(1) as f64);
    let area_w_px = area.width as f64 * cell_w;
    let area_h_px = area.height as f64 * cell_h;
    let scale = (area_w_px / src_w as f64).min(area_h_px / src_h as f64);
    let dst_w_px = src_w as f64 * scale;
    let dst_h_px = src_h as f64 * scale;
    // Round up to whole cells so we never crop the image short by a row/col.
    let cells_w = ((dst_w_px / cell_w).ceil() as u16).max(1).min(area.width);
    let cells_h = ((dst_h_px / cell_h).ceil() as u16).max(1).min(area.height);
    let dx = (area.width - cells_w) / 2;
    let dy = (area.height - cells_h) / 2;
    Rect {
        x: area.x + dx,
        y: area.y + dy,
        width: cells_w,
        height: cells_h,
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
        let shortcuts = "p pick · x reject · u unset · f filter · d develop · q quit";
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

#[cfg(test)]
mod aspect_tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn landscape_image_uses_full_width_and_centers_vertically() {
        // 80x40 cells, square cells (font 1x1). Source 200x100 (2:1 landscape).
        let r = aspect_fit_rect(area(80, 40), 200, 100, (1, 1));
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 40);
        assert_eq!((r.x, r.y), (0, 0));

        // Source 200x50 (4:1) into 80x40 → fit width, height = 20, centered vertically.
        let r = aspect_fit_rect(area(80, 40), 200, 50, (1, 1));
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 20);
        assert_eq!(r.y, 10);
    }

    #[test]
    fn portrait_image_uses_full_height_and_centers_horizontally() {
        let r = aspect_fit_rect(area(80, 40), 100, 200, (1, 1));
        assert_eq!(r.height, 40);
        assert_eq!(r.width, 20);
        assert_eq!(r.x, 30);
    }

    #[test]
    fn non_square_cells_are_respected() {
        // Cells 1 wide, 2 tall (halfblocks). 80x40 cells = 80x80 px → square image fits both axes.
        let r = aspect_fit_rect(area(80, 40), 100, 100, (1, 2));
        assert_eq!((r.width, r.height), (80, 40));
    }

    #[test]
    fn zero_dims_are_safe() {
        let r = aspect_fit_rect(area(80, 40), 0, 100, (1, 1));
        assert_eq!((r.x, r.y, r.width, r.height), (0, 0, 80, 40));
    }
}
