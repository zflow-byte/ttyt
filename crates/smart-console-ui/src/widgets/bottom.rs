use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

const KEYBINDING_LEGEND: &str = "Ctrl+C disconnect  Ctrl+N session  Ctrl+P palette  Ctrl+L clear  Ctrl+R history  TAB complete  ESC menu";

/// Bottom-left: parsed events (errors/warnings/link/hostname changes --
/// real-time classification is wired in Phase 2, so this pane is
/// intentionally empty until then). Bottom-right: hints -- either the most
/// recent "not yet implemented" notice or the static keybinding legend.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let events_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Events ");
    let events_body = if app.connection_state == smart_console_core::ConnectionState::Connected {
        "(no events yet)"
    } else {
        "(not connected)"
    };
    let events = Paragraph::new(Line::from(events_body))
        .block(events_block)
        .style(Style::default().fg(theme.muted));
    frame.render_widget(events, columns[0]);

    let hints_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Hints ");
    let hints_body = app.hint.as_deref().unwrap_or(KEYBINDING_LEGEND);
    let hints = Paragraph::new(Line::from(hints_body))
        .block(hints_block)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(hints, columns[1]);
}
