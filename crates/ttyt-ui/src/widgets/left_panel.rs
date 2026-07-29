use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

/// Sessions / devices / recent commands, per the design doc. Phase 1 has
/// exactly one session and no device scan or command history wired in yet
/// (those are Task 1.9's CLI-level scan and Phase 2's persistent history),
/// so this shows only the current session's port and state.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let session_line = Line::from(format!(
        "1. {port} ({state:?})",
        port = app.port_name.as_deref().unwrap_or("(no port)"),
        state = app.connection_state,
    ));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Sessions ");

    let paragraph = Paragraph::new(vec![session_line])
        .block(block)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(paragraph, area);
}
