use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::tab_block;
use crate::app::{App, DEVELOP_KNOBS, Focus};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Develop;
    let block = tab_block("Develop", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let label_w = label_width(inner.width);

    let value_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let label_style = if focused {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };

    let items: Vec<ListItem> = DEVELOP_KNOBS
        .iter()
        .map(|(label, knob)| {
            let value = knob.format(&app.develop_params);
            let line = Line::from(vec![
                Span::styled(format!("{:<width$}", label, width = label_w), label_style),
                Span::styled(value, value_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(
        app.develop_cursor
            .min(DEVELOP_KNOBS.len().saturating_sub(1)),
    ));

    let highlight = Style::default().add_modifier(Modifier::REVERSED);
    let list = List::new(items)
        .highlight_style(highlight)
        .highlight_symbol(if focused { "▶ " } else { "  " });

    frame.render_stateful_widget(list, inner, &mut state);
}

fn label_width(area_width: u16) -> usize {
    // Tab width is 28 with 2-cell border = 26 inside. Leave room for the
    // highlight symbol (2) + value (~8). 16 fits "Soft Highlights".
    let inner = area_width as usize;
    inner.saturating_sub(10).clamp(8, 18)
}
