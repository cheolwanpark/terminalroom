use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;

pub fn render(frame: &mut Frame, _app: &App) {
    let area = frame.area();
    let block = Block::default().borders(Borders::ALL).title("Develop");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let body = "Develop view\n\nEditing controls are not implemented yet.\n\nPress c to return to culling.\nPress q to quit.";
    let p = Paragraph::new(body)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(p, inner);
}
