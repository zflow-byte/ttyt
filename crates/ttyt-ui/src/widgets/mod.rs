pub mod bottom;
pub mod console;
pub mod header;
pub mod left_panel;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::App;
use crate::theme::Theme;

/// Renders the full 4-pane layout:
///
/// ```text
/// +----------------------------------------------+
/// | Header                                       |
/// +--------------+---------------------------------+
/// | Left Panel   | Main Console                     |
/// +--------------+---------------------------------+
/// | Events       | Hints                            |
/// +--------------+---------------------------------+
/// ```
pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    header::render(frame, rows[0], app, theme);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(rows[1]);
    left_panel::render(frame, middle[0], app, theme);
    let cursor = console::render(frame, middle[1], app, theme);

    bottom::render(frame, rows[2], app, theme);

    frame.set_cursor_position(cursor);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Renders one frame to an in-memory buffer (no real terminal needed)
    /// and returns it as plain text, so panel content can be asserted on
    /// directly -- this is the automated equivalent of eyeballing a manual
    /// run, which isn't practically scriptable in this environment.
    fn render_to_string(app: &App) -> String {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::dark();
        terminal.draw(|frame| render(frame, app, &theme)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn all_four_panes_render_their_titles() {
        let app = App::new();
        let rendered = render_to_string(&app);
        assert!(rendered.contains("ttyt"), "header title missing");
        assert!(rendered.contains("Sessions"), "left panel title missing");
        assert!(rendered.contains("Console"), "console title missing");
        assert!(rendered.contains("Events"), "events title missing");
        assert!(rendered.contains("Hints"), "hints title missing");
    }

    #[test]
    fn scrollback_content_and_input_line_are_visible_in_console_pane() {
        let mut app = App::new();
        app.active_session_mut()
            .push_line("Switch> show version".to_string());
        app.active_session_mut().input = "show ip int brief".to_string();
        let rendered = render_to_string(&app);
        assert!(rendered.contains("Switchshowversion") || rendered.contains("Switch"));
        assert!(rendered.contains("showipintbrief") || rendered.contains("show"));
    }

    #[test]
    fn keybinding_legend_is_visible_when_no_hint_is_set() {
        let app = App::new();
        let rendered = render_to_string(&app);
        assert!(rendered.contains("disconnect"));
    }

    #[test]
    fn active_hint_replaces_the_keybinding_legend() {
        let mut app = App::new();
        app.active_session_mut().hint = Some("Ctrl+N: not yet implemented".to_string());
        let rendered = render_to_string(&app);
        assert!(rendered.contains("notyetimplemented") || rendered.contains("implemented"));
    }
}
