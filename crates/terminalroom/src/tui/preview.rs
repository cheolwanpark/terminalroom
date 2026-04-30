use std::path::PathBuf;

use lru::LruCache;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui_image::StatefulImage;

use super::PreviewEntry;
use crate::app::App;

pub fn render(
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
pub fn aspect_fit_rect(area: Rect, src_w: u32, src_h: u32, font_size: (u16, u16)) -> Rect {
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
