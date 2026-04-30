use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, DEVELOP_KNOBS};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let outer = Block::default().borders(Borders::ALL).title("Develop");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);

    let items: Vec<ListItem> = DEVELOP_KNOBS
        .iter()
        .map(|(label, knob)| {
            let value = knob.format(&app.develop_params);
            let line = Line::from(vec![
                Span::raw(format!("{:<18}", label)),
                Span::styled(value, Style::default().add_modifier(Modifier::BOLD)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(
        app.develop_cursor
            .min(DEVELOP_KNOBS.len().saturating_sub(1)),
    ));

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, layout[0], &mut state);

    let hint = "j/k  navigate    h/l  adjust    r  reset    c  back to culling    q  quit";
    let hint_p = Paragraph::new(hint);
    frame.render_widget(hint_p, layout[1]);
}
