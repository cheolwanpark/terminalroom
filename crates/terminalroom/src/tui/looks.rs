use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::App;

const FOOTER_LINES: [&str; 2] = [
    "j/k navigate · enter apply · esc close",
    "drop XMPs in ~/.terminalroom/looks/",
];
const NONE_LABEL: &str = "(none)";
/// Fixed modal sizing — generous, NOT content-fitted, so the focus border is
/// always visible and the footer hint never wraps. Long look names are
/// truncated with `…` rather than driving the modal width up.
const TARGET_WIDTH_RATIO: f32 = 0.55;
const TARGET_HEIGHT_RATIO: f32 = 0.55;
const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 12;
const MAX_WIDTH: u16 = 90;
const MAX_HEIGHT: u16 = 28;

pub fn render(frame: &mut Frame, app: &App) {
    let parent = frame.area();
    let area = compute_rect(parent);
    frame.render_widget(Clear, area);

    // Yellow Thick border — same vocabulary the focused side tabs use, so the
    // user has a clear signal that the modal is what's receiving keys.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Looks ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let footer_h = FOOTER_LINES.len() as u16;
    let [list_area, footer_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(footer_h)]).areas(inner);

    // Truncate names to whatever the inner list area can hold (minus 2 cells
    // for the `▶ ` highlight symbol + 1 trailing cell of breathing room).
    let max_name_w = list_area.width.saturating_sub(3) as usize;

    let mut items: Vec<ListItem> = Vec::with_capacity(1 + app.looks.len());
    items.push(ListItem::new(Line::from(truncate(NONE_LABEL, max_name_w))));
    for row in &app.looks {
        items.push(ListItem::new(Line::from(truncate(&row.name, max_name_w))));
    }
    // Empty state: when there are no registered XMPs, the only row is
    // "(none)". Append a dim hint underneath so the user knows the modal is
    // alive and *what to do* — j/k on a 1-row list looks like a dead UI.
    if app.looks.is_empty() {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(
            Line::from("No XMP looks registered.").style(Style::default().add_modifier(Modifier::DIM)),
        ));
        items.push(ListItem::new(
            Line::from("Drop .xmp files in ~/.terminalroom/looks/ and reopen.")
                .style(Style::default().add_modifier(Modifier::DIM)),
        ));
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(app.looks_cursor.min(app.looks.len())));
    frame.render_stateful_widget(list, list_area, &mut state);

    let footer_text: Vec<Line> = FOOTER_LINES
        .iter()
        .map(|&s| Line::from(s).style(Style::default().add_modifier(Modifier::DIM)))
        .collect();
    frame.render_widget(Paragraph::new(footer_text), footer_area);
}

fn compute_rect(parent: Rect) -> Rect {
    // Width: target ratio of the terminal, clamped to [MIN, MAX], and never
    // wider than the terminal minus a 2-cell margin.
    let target_w = (parent.width as f32 * TARGET_WIDTH_RATIO).round() as u16;
    let cap_w = parent.width.saturating_sub(2).max(8);
    let width = target_w.clamp(MIN_WIDTH, MAX_WIDTH).min(cap_w);

    let target_h = (parent.height as f32 * TARGET_HEIGHT_RATIO).round() as u16;
    let cap_h = parent.height.saturating_sub(2).max(8);
    let height = target_h.clamp(MIN_HEIGHT, MAX_HEIGHT).min(cap_h);

    let x = parent.x + (parent.width.saturating_sub(width)) / 2;
    let y = parent.y + (parent.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Char-count-based truncation with an ellipsis. CJK and other wide chars are
/// counted as one cell each, which is approximate but good enough for a
/// preset name display — the modal is generously sized so this rarely fires.
fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let take = max_chars - 1;
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn truncate_zero_max_returns_empty() {
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn compute_rect_clamps_to_min_on_tiny_terminal() {
        let parent = Rect::new(0, 0, 30, 10);
        let r = compute_rect(parent);
        // cap_w = parent.width - 2 = 28; min(MIN_WIDTH=50, cap_w=28) = 28.
        assert_eq!(r.width, 28);
        // cap_h = parent.height - 2 = 8.
        assert_eq!(r.height, 8);
    }

    #[test]
    fn compute_rect_uses_min_when_terminal_is_average() {
        let parent = Rect::new(0, 0, 80, 20);
        let r = compute_rect(parent);
        // 80 * 0.55 = 44, clamped to MIN=50, capped at parent-2=78 → 50.
        assert_eq!(r.width, 50);
        // 20 * 0.55 = 11, clamped to MIN=12, capped at parent-2=18 → 12.
        assert_eq!(r.height, 12);
    }

    #[test]
    fn compute_rect_uses_ratio_on_large_terminal() {
        let parent = Rect::new(0, 0, 200, 60);
        let r = compute_rect(parent);
        // 200 * 0.55 = 110, clamped to MAX=90 → 90.
        assert_eq!(r.width, 90);
        // 60 * 0.55 = 33, clamped to MAX=28 → 28.
        assert_eq!(r.height, 28);
    }
}
