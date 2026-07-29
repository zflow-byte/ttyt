use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Vendor/hostname/mode are shown as "-" until Phase 2 wires live
    // plugin detection results into the header (plan.md Task 2.5).
    let line = Line::from(format!(
        " {conn:?}  |  Port: {port}  |  Vendor: -  |  Hostname: -  |  Mode: -  |  Rec: {rec}",
        conn = app.connection_state,
        port = app.port_name.as_deref().unwrap_or("-"),
        rec = if app.recording_path.is_some() {
            "●"
        } else {
            "-"
        },
    ));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" smart-console ".bold());

    let paragraph = Paragraph::new(line)
        .block(block)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(paragraph, area);
}
