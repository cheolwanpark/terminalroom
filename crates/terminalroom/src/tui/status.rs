use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};
use crate::db::CullingState;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let text = if let Some(entry) = app.current() {
        let state_label = match entry.state {
            CullingState::Pick => "PICK",
            CullingState::Reject => "REJECT",
            CullingState::Unset => "UNSET",
        };
        let total = app.visible.len();
        let filter_indicator = filter_indicator(app);
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
            format!("{prefix}   {}", shortcuts(app.focus))
        }
    } else {
        let filter_indicator = filter_indicator(app);
        format!(
            "(no files visible){filter_indicator}   {}",
            shortcuts(app.focus)
        )
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn shortcuts(focus: Focus) -> &'static str {
    match focus {
        Focus::Navigation => {
            "j/k navigate · p/x/u cull · f filter · enter develop · q quit"
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
