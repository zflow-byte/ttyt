use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

/// Renders the scrollback (most recent lines that fit), an in-progress
/// partial line if one is being previewed, and the live input line, and
/// returns where the terminal cursor should sit so the caller can call
/// `frame.set_cursor_position`.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) -> Position {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .title(" Console ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let session = app.active_session();

    // Reserve the last inner row for the input line, plus one more when a
    // partial (in-progress, no newline yet) line is being previewed below
    // scrollback -- without this, a full scrollback plus a partial line
    // hands `Paragraph` one more line than the pane has rows, clipping the
    // input line off the bottom while the cursor (still calculated as the
    // pane's last row, below) points at a row `Paragraph` never drew
    // anything on. Same failure shape as the v0.1.1 top-alignment bug,
    // different cause.
    let reserved_rows: u16 = if session.partial_output.is_some() {
        2
    } else {
        1
    };
    let scrollback_rows = inner.height.saturating_sub(reserved_rows) as usize;
    let visible_start = session.scrollback.len().saturating_sub(scrollback_rows);
    let mut lines: Vec<Line> = session
        .scrollback
        .iter()
        .skip(visible_start)
        .map(|line| Line::from(line.as_str()))
        .collect();

    if let Some(partial) = &session.partial_output {
        lines.push(Line::from(partial.as_str()));
    }

    let prompt = session.input_line_display();
    lines.push(Line::from(prompt.clone()).style(Style::default().fg(theme.accent)));

    // `Paragraph` top-aligns by default: with little scrollback (a freshly
    // connected, quiet, or not-yet-detected device), `lines` is far
    // shorter than `inner.height`, so without padding the input line would
    // render a few rows down from the pane's top border while the cursor
    // below is placed at the pane's last row regardless -- an empty gap
    // between the visible "> " prompt and the blinking cursor, making it
    // look like typed text is appearing in the wrong place. Padding with
    // leading blank lines up to the full pane height keeps the input line
    // pinned to the actual bottom row, matching how a real terminal fills
    // upward from the bottom rather than downward from the top.
    let pad = (inner.height as usize).saturating_sub(lines.len());
    if pad > 0 {
        let mut padded = vec![Line::from(""); pad];
        padded.extend(lines);
        lines = padded;
    }

    let paragraph = Paragraph::new(lines).style(Style::default().fg(theme.foreground));
    frame.render_widget(paragraph, inner);

    Position {
        x: inner.x + prompt.len() as u16,
        y: inner.y + inner.height.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let content = buffer.content();
        let start = y as usize * width as usize;
        content[start..start + width as usize]
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Regression test for a real bug found testing against actual
    /// hardware: `Paragraph` top-aligns by default, so with little/no
    /// scrollback (a freshly connected or quiet device) the input line
    /// rendered a few rows below the pane's top border while the reported
    /// cursor position stayed pinned to the pane's last row -- a visible
    /// gap between the "> " prompt text and the blinking cursor, making it
    /// look like typed input was appearing in the wrong place with no way
    /// to tell how much had been typed or where Enter would actually send
    /// from.
    #[test]
    fn input_line_renders_on_the_pane_last_row_even_with_no_scrollback() {
        let width = 40u16;
        let height = 10u16;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut app = App::new();
        app.active_session_mut().input = "show version".to_string();
        let theme = Theme::dark();

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cursor = Position { x: 0, y: 0 };
        terminal
            .draw(|frame| {
                cursor = render(frame, area, &app, &theme);
            })
            .unwrap();

        // Bordered pane: inner starts at y=1, spans height-2 rows, so the
        // last inner row is y=8 -- the cursor calculation is unchanged by
        // this fix and must still land there.
        assert_eq!(cursor.y, 8);

        let buffer = terminal.backend().buffer();
        let last_row = row_text(buffer, cursor.y, width);
        assert!(
            last_row.contains("show version"),
            "input line should render on the same row as the reported cursor, got {last_row:?}"
        );

        // The bug this regresses against rendered the input line here
        // (the pane's first inner row) instead, with the real cursor
        // several blank rows below it.
        let top_inner_row = row_text(buffer, 1, width);
        assert!(
            !top_inner_row.contains("show version"),
            "input line should not still be top-aligned with a mostly-empty pane below it, got {top_inner_row:?}"
        );
    }

    /// Regression test for a bug introduced by live partial-line preview:
    /// with scrollback already filling the pane, adding a partial line
    /// without also reserving an extra row for it hands `Paragraph` one
    /// more line than the pane has rows -- the input line clips off the
    /// bottom while the cursor (still calculated as the pane's last row)
    /// points at a row nothing was actually drawn on. Same failure shape
    /// as the v0.1.1 top-alignment bug, different cause: this time from
    /// under-reserving space rather than not padding it.
    #[test]
    fn partial_line_does_not_push_the_input_line_off_a_full_pane() {
        let width = 40u16;
        let height = 10u16;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut app = App::new();
        // Inner height is 8 (bordered pane) -- fill scrollback well past
        // that so the pane is already full before the partial line is
        // added.
        for i in 0..20 {
            app.active_session_mut()
                .push_line(format!("scrollback line {i}"));
        }
        app.active_session_mut().partial_output = Some("Router#show ru".to_string());
        app.active_session_mut().input = "show version".to_string();
        let theme = Theme::dark();

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cursor = Position { x: 0, y: 0 };
        terminal
            .draw(|frame| {
                cursor = render(frame, area, &app, &theme);
            })
            .unwrap();

        assert_eq!(cursor.y, 8);

        let buffer = terminal.backend().buffer();
        let last_row = row_text(buffer, cursor.y, width);
        assert!(
            last_row.contains("show version"),
            "input line should still render on the cursor's row with a full scrollback \
             plus a partial line, got {last_row:?}"
        );

        let partial_row = row_text(buffer, cursor.y - 1, width);
        assert!(
            partial_row.contains("Router#show ru"),
            "the partial line should render immediately above the input line, got {partial_row:?}"
        );
    }
}
