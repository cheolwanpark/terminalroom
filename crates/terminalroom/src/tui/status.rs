use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(entry) = app.current() {
        let total = app.visible.len();
        let mut spans = vec![
            Span::raw(entry.file.display_name.clone()),
            Span::raw("   "),
            Span::raw(format!("{}/{}", app.cursor + 1, total)),
        ];
        if entry.removed {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(
                "REMOVED",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let filter_indicator = filter_indicator(app);
        if !filter_indicator.is_empty() {
            spans.push(Span::raw(filter_indicator));
        }
        spans.push(Span::raw("   "));
        if let Some(msg) = &app.status {
            spans.push(Span::raw(msg.clone()));
        } else {
            spans.push(Span::raw(shortcuts(app.focus, app.show_removed).to_string()));
        }
        Line::from(spans)
    } else {
        let mut spans = vec![Span::raw("(no files visible)")];
        let filter_indicator = filter_indicator(app);
        if !filter_indicator.is_empty() {
            spans.push(Span::raw(filter_indicator));
        }
        spans.push(Span::raw("   "));
        spans.push(Span::raw(shortcuts(app.focus, app.show_removed).to_string()));
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn shortcuts(focus: Focus, show_removed: bool) -> &'static str {
    match focus {
        Focus::Navigation => {
            if show_removed {
                "j/k navigate · x remove · r restore · R show-removed:on · f filter · enter develop · q quit"
            } else {
                "j/k navigate · x remove · r restore · R show-removed:off · f filter · enter develop · q quit"
            }
        }
        Focus::Develop => "j/k knob · h/l adjust · r reset · esc back · q quit",
    }
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
