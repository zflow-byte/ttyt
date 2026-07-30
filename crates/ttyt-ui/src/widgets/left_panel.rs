use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;
use crate::widgets::header::vendor_label;

/// Sessions / tabs, per the design doc. One line per concurrent session
/// (`connect --port A --port B ...`), the focused tab highlighted and
/// marked `>` -- `Ctrl+N` cycles which one that is. A single-session run
/// (the common case) still renders as a one-line list; there's just
/// nothing to cycle to.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let lines: Vec<Line> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let marker = if i == app.active { ">" } else { " " };
            let text = format!(
                "{marker} {n}. {port} ({state:?}) {vendor}",
                n = i + 1,
                port = session.port_name.as_deref().unwrap_or("(no port)"),
                state = session.connection_state,
                vendor = vendor_label(&session.vendor_status),
            );
            let style = if i == app.active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            Line::from(text).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Sessions ");

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ttyt_core::ConnectionState;

    fn render_to_string(app: &App) -> String {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::dark();
        terminal
            .draw(|frame| render(frame, frame.area(), app, &theme))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn every_session_gets_its_own_line() {
        let mut app = App::with_session_count(2);
        app.sessions[0].port_name = Some("/dev/cu.usbserial-A".to_string());
        app.sessions[1].port_name = Some("/dev/cu.usbserial-B".to_string());
        app.sessions[1].connection_state = ConnectionState::Connected;

        let rendered = render_to_string(&app);
        assert!(rendered.contains("usbserial-A"));
        assert!(rendered.contains("usbserial-B"));
    }

    #[test]
    fn active_session_is_marked() {
        let mut app = App::with_session_count(2);
        app.active = 1;
        let rendered = render_to_string(&app);
        // Row 1 (index 0, the inactive tab) starts with a blank marker;
        // row 2 (index 1, active) starts with '>'.
        assert!(rendered.contains('>'));
    }
}
