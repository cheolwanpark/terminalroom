use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let area = centered_rect(frame.area(), &app);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Filter formats");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [list_area, footer_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let max_label = app
        .available_formats
        .iter()
        .map(|(k, _)| k.label().len())
        .max()
        .unwrap_or(0);

    let items: Vec<ListItem> = app
        .available_formats
        .iter()
        .map(|(kind, count)| {
            let mark = if app.enabled_formats.contains(kind) {
                "[x]"
            } else {
                "[ ]"
            };
            let label = kind.label();
            let pad = " ".repeat(max_label.saturating_sub(label.len()));
            ListItem::new(Line::from(format!("{mark} {label}{pad}  ({count})")))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.filter_cursor));
    frame.render_stateful_widget(list, list_area, &mut state);

    let footer = Paragraph::new("space toggle · enter close");
    frame.render_widget(footer, footer_area);
}

fn centered_rect(parent: Rect, app: &App) -> Rect {
    let rows = app.available_formats.len() as u16 + 4; // borders + footer + padding
    let height = rows.clamp(6, parent.height.saturating_sub(2).max(6));
    let width: u16 = 36.min(parent.width.saturating_sub(2));
    let x = parent.x + (parent.width.saturating_sub(width)) / 2;
    let y = parent.y + (parent.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
