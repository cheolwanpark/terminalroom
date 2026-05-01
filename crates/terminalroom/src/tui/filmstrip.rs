use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::tab_block;
use crate::app::{App, FileEntry, Focus};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Navigation;
    let block = tab_block("Navigation", focused);
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
    // Reserve 2 cols for a trailing badge ("R" or blank), clip the name to the rest.
    let usable = width.saturating_sub(3).max(1);
    let mut name = entry.file.display_name.clone();
    if name.chars().count() > usable {
        let truncated: String = name.chars().take(usable.saturating_sub(1)).collect();
        name = format!("{truncated}…");
    }

    if entry.removed {
        let line = Line::from(vec![
            Span::styled(name, Style::default().add_modifier(Modifier::DIM)),
            Span::raw(" "),
            Span::styled(
                "R",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        ListItem::new(line)
    } else {
        ListItem::new(Line::from(name))
    }
}
