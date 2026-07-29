use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ttyt_core::{ConnectionState, ParsedEvent};

use crate::app::App;
use crate::theme::Theme;

const KEYBINDING_LEGEND: &str = "Ctrl+C disconnect  Ctrl+N session  Ctrl+P palette  Ctrl+L clear  Ctrl+R history  TAB complete  ESC menu";
const HISTORY_SEARCH_HINT: &str =
    "history search: type to filter, Ctrl+R again for older match, Enter to accept, Esc to cancel";

/// Bottom-left: parsed events (errors/warnings/link/hostname changes),
/// most recent visible. Bottom-right: hints -- history-search usage while
/// active, else the most recent "not yet implemented" notice, else the
/// static keybinding legend.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_events(frame, columns[0], app, theme);

    let hints_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Hints ");
    let hints_body = if app.history_search.is_some() {
        HISTORY_SEARCH_HINT
    } else {
        app.hint.as_deref().unwrap_or(KEYBINDING_LEGEND)
    };
    let hints = Paragraph::new(Line::from(hints_body))
        .block(hints_block)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(hints, columns[1]);
}

fn render_events(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Events ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.events.is_empty() {
        let placeholder = if app.connection_state == ConnectionState::Connected {
            "(no events yet)"
        } else {
            "(not connected)"
        };
        let paragraph =
            Paragraph::new(Line::from(placeholder)).style(Style::default().fg(theme.muted));
        frame.render_widget(paragraph, inner);
        return;
    }

    let visible_rows = inner.height as usize;
    let start = app.events.len().saturating_sub(visible_rows);
    let lines: Vec<Line> = app
        .events
        .iter()
        .skip(start)
        .map(|event| format_event_line(event, theme))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_event_line<'a>(event: &ParsedEvent, theme: &Theme) -> Line<'a> {
    let (text, color) = match event {
        ParsedEvent::Error(msg) => (format!("[ERROR] {msg}"), theme.error),
        ParsedEvent::Warning(msg) => (format!("[WARN]  {msg}"), theme.warning),
        ParsedEvent::HostnameChanged(name) => {
            (format!("[HOST]  hostname changed to {name}"), theme.accent)
        }
        ParsedEvent::LinkStatus { interface, up } => (
            format!("[LINK]  {interface} is {}", if *up { "up" } else { "down" }),
            if *up { theme.accent } else { theme.warning },
        ),
    };
    Line::from(text).style(Style::default().fg(color))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn error_event_is_prefixed_and_colored_red() {
        let theme = Theme::dark();
        let line = format_event_line(&ParsedEvent::Error("bad thing".to_string()), &theme);
        assert!(line.to_string().contains("[ERROR] bad thing"));
    }

    #[test]
    fn link_status_shows_up_or_down() {
        let theme = Theme::dark();
        let up = format_event_line(
            &ParsedEvent::LinkStatus {
                interface: "Gi0/1".to_string(),
                up: true,
            },
            &theme,
        );
        assert!(up.to_string().contains("Gi0/1 is up"));

        let down = format_event_line(
            &ParsedEvent::LinkStatus {
                interface: "Gi0/1".to_string(),
                up: false,
            },
            &theme,
        );
        assert!(down.to_string().contains("Gi0/1 is down"));
    }
}
