use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

/// Renders the scrollback (most recent lines that fit) plus the live
/// input line, and returns where the terminal cursor should sit so the
/// caller can call `frame.set_cursor_position`.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) -> Position {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .title(" Console ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve the last inner row for the input line.
    let scrollback_rows = inner.height.saturating_sub(1) as usize;
    let visible_start = app.scrollback.len().saturating_sub(scrollback_rows);
    let mut lines: Vec<Line> = app
        .scrollback
        .iter()
        .skip(visible_start)
        .map(|line| Line::from(line.as_str()))
        .collect();

    let prompt = format!("> {}", app.input);
    lines.push(Line::from(prompt.clone()).style(Style::default().fg(theme.accent)));

    let paragraph = Paragraph::new(lines).style(Style::default().fg(theme.foreground));
    frame.render_widget(paragraph, inner);

    Position {
        x: inner.x + prompt.len() as u16,
        y: inner.y + inner.height.saturating_sub(1),
    }
}
