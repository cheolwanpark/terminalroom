use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

/// ANSI Shadow figlet of `TERMINALROOM`. Each row is the same display width;
/// glyphs are box-drawing characters and `█` (each one display cell wide).
pub const BANNER: &[&str] = &[
    "████████╗███████╗██████╗ ███╗   ███╗██╗███╗   ██╗ █████╗ ██╗     ██████╗  ██████╗  ██████╗ ███╗   ███╗",
    "╚══██╔══╝██╔════╝██╔══██╗████╗ ████║██║████╗  ██║██╔══██╗██║     ██╔══██╗██╔═══██╗██╔═══██╗████╗ ████║",
    "   ██║   █████╗  ██████╔╝██╔████╔██║██║██╔██╗ ██║███████║██║     ██████╔╝██║   ██║██║   ██║██╔████╔██║",
    "   ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║██║██║╚██╗██║██╔══██║██║     ██╔══██╗██║   ██║██║   ██║██║╚██╔╝██║",
    "   ██║   ███████╗██║  ██║██║ ╚═╝ ██║██║██║ ╚████║██║  ██║███████╗██║  ██║╚██████╔╝╚██████╔╝██║ ╚═╝ ██║",
    "   ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚═╝     ╚═╝",
];

const FALLBACK: &str = "──── ▌ T E R M I N A L R O O M ▐ ────";

fn full_width() -> u16 {
    BANNER
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0) as u16
}

fn full_height() -> u16 {
    BANNER.len() as u16
}

/// Number of rows to allocate at the top of the screen for the banner. Returns
/// `full_height()` if the terminal is wide enough to fit the full banner; a
/// single row otherwise.
pub fn height_for(width: u16) -> u16 {
    if width >= full_width() {
        full_height()
    } else {
        1
    }
}

pub fn render(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = Style::default().add_modifier(Modifier::BOLD);
    let bw = full_width();
    let bh = full_height();
    if area.width < bw || area.height < bh {
        let p = Paragraph::new(FALLBACK)
            .alignment(Alignment::Center)
            .style(style);
        frame.render_widget(p, area);
        return;
    }
    let dx = (area.width - bw) / 2;
    let total_h = bh.min(area.height);
    let dy = (area.height - total_h) / 2;
    for (i, line) in BANNER.iter().take(total_h as usize).enumerate() {
        let row = Rect {
            x: area.x + dx,
            y: area.y + dy + i as u16,
            width: bw,
            height: 1,
        };
        frame.render_widget(Paragraph::new(*line).style(style), row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_rows_have_equal_display_width() {
        let widths: Vec<usize> = BANNER.iter().map(|s| s.chars().count()).collect();
        let first = widths[0];
        for (i, w) in widths.iter().enumerate() {
            assert_eq!(*w, first, "row {i} has width {w}, expected {first}");
        }
    }

    #[test]
    fn height_for_small_width_falls_back_to_single_row() {
        assert_eq!(height_for(0), 1);
        assert_eq!(height_for(20), 1);
        assert_eq!(height_for(full_width()), full_height());
    }
}
