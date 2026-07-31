use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

/// Replays a raw device-output string through a minimal single-line
/// cursor model before it's handed to ratatui for display. `Line`/`Span`
/// render every character as literal, non-advancing cell content -- they
/// are not a terminal emulator and don't interpret control bytes or
/// escape sequences. Two real hardware reports made that gap visible:
/// FortiGate's `--More--` paging erases its own prompt with a
/// `\r`-erase-`\r` sequence (v0.1.6, corrupted the real terminal once
/// crossterm wrote the literal `\r` to it -- see `LineAssembler::feed`'s
/// `embedded_carriage_returns_survive_into_the_emitted_line` test for why
/// this survives verbatim into the assembled line, by design, and the fix
/// belongs here, not there); a device's Backspace response left literal
/// `[K` visible on screen (v0.1.7/v0.1.8's `\x1b[K`, since only the bare
/// `ESC` byte was a control character). Both were fixed with ad hoc
/// pattern-specific rules (`\r`-collapse, then CSI-stripping) that
/// removed the garbage but never made editing *correct* -- a bare
/// `\x08\x1b[K` still left the character it was erasing in place, just
/// with the escape bytes gone instead of visible. This replaces both
/// rules with one general model: replay the bytes against a virtual
/// cursor over a single line of cells, the same way a real terminal
/// would for one line of input (not a full 2D screen -- no cursor
/// row/column addressing, no scroll regions, no SGR/color; only what a
/// device's own line-editing echo plausibly sends: `\r` returns to
/// column 0, `\x08` moves the cursor left without erasing, `\x7f` (DEL)
/// moves left *and* deletes that cell, `\x1b[K`/`\x1b[1K`/`\x1b[2K` erase
/// in line, `\x1b[nD`/`\x1b[nC` move the cursor). Printable characters
/// overwrite the cell at the cursor if one exists there, otherwise
/// append -- this is what makes a `\r`-prefixed redraw (or a device
/// overwriting a shorter reply over a longer one) look right instead of
/// mashed together. Any other escape sequence's final byte (SGR color,
/// cursor positioning, screen clear, ...) is recognized and consumed but
/// has no visible effect here -- out of scope for a single-line model.
fn visible_text(text: &str) -> String {
    let mut cells: Vec<char> = Vec::new();
    let mut cursor: usize = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => cursor = 0,
            '\x08' => cursor = cursor.saturating_sub(1),
            '\x7f' => {
                if cursor > 0 {
                    cursor -= 1;
                    cells.remove(cursor);
                }
            }
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next(); // consume '['
                let mut params = String::new();
                while matches!(chars.peek(), Some(&p) if (' '..='?').contains(&p)) {
                    if let Some(p) = chars.next() {
                        params.push(p);
                    }
                }
                match chars.next() {
                    Some('D') => {
                        let n: usize = params.parse().unwrap_or(1).max(1);
                        cursor = cursor.saturating_sub(n);
                    }
                    Some('C') => {
                        let n: usize = params.parse().unwrap_or(1).max(1);
                        cursor = (cursor + n).min(cells.len());
                    }
                    Some('K') => match params.as_str() {
                        // Erase to start of line: blanks in place rather
                        // than removing cells, so later cells keep their
                        // column position (matches a real terminal --
                        // erasing never shifts unrelated text).
                        "1" => {
                            let end = cursor.min(cells.len());
                            for cell in cells.iter_mut().take(end) {
                                *cell = ' ';
                            }
                        }
                        // Erase entire line.
                        "2" => {
                            cells.clear();
                            cursor = 0;
                        }
                        // Erase to end of line (default with no param).
                        _ => cells.truncate(cursor),
                    },
                    // Any other CSI final byte (SGR color, cursor
                    // positioning, screen clear, ...): consumed, no
                    // visible effect -- out of scope for one line.
                    _ => {}
                }
            }
            '\u{1b}' => {
                if chars.peek().is_some() {
                    chars.next(); // consume the one byte after ESC
                }
            }
            other if !other.is_control() => {
                if cursor < cells.len() {
                    cells[cursor] = other;
                } else {
                    cells.push(other);
                }
                cursor += 1;
            }
            _ => {} // any other raw control byte (bell, ...): no cell effect
        }
    }
    cells.into_iter().collect()
}

/// Prepares a live partial-line preview for display: unlike a finished
/// scrollback line (read once, doesn't change), a partial line keeps
/// growing, and what's new is always at the end -- so this shows the
/// *tail*, not the head, once it's too long for the pane's width, after
/// `visible_text` above has already replayed `\r`/backspace/escape bytes
/// against a virtual cursor. `Paragraph` without `.wrap()` renders from
/// the start of the string and clips at the area edge, so an unbounded-
/// length line (Cisco `copy` progress marks, any command with no newline
/// until it finishes) would otherwise render its first ~`width`
/// characters once and then appear frozen while the device keeps
/// working -- indistinguishable from an actual hang, the exact symptom
/// this feature exists to rule out.
fn partial_line_preview(partial: &str, width: u16) -> String {
    let sanitized = visible_text(partial);
    let width = width as usize;
    let char_count = sanitized.chars().count();
    if char_count <= width {
        return sanitized;
    }
    let skip = char_count - width;
    match sanitized.char_indices().nth(skip) {
        Some((byte_idx, _)) => sanitized[byte_idx..].to_string(),
        None => sanitized,
    }
}

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
        .map(|line| Line::from(visible_text(line)))
        .collect();

    if let Some(partial) = &session.partial_output {
        lines.push(Line::from(partial_line_preview(partial, inner.width)));
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

    #[test]
    fn partial_line_preview_returns_short_text_unchanged() {
        assert_eq!(partial_line_preview("Router#show ru", 40), "Router#show ru");
    }

    #[test]
    fn partial_line_preview_shows_the_tail_not_the_head_when_too_long() {
        // A `copy`/firmware-transfer progress line grows unbounded with no
        // newline until it finishes -- showing the head would render once
        // and then look frozen while the device keeps working, even
        // though new characters keep arriving off the visible edge.
        let long = "!".repeat(50) + "END";
        let preview = partial_line_preview(&long, 10);
        assert_eq!(preview, "!!!!!!!END");
        assert_eq!(preview.chars().count(), 10);
    }

    #[test]
    fn partial_line_preview_collapses_to_the_segment_after_the_last_carriage_return() {
        // A `\r`-based progress counter overwrites the same terminal
        // column rather than appending -- `Line` doesn't interpret `\r`
        // as cursor movement, so without collapsing this the stages would
        // render mashed together ("10%20%30%") instead of the current
        // value ("30%").
        assert_eq!(partial_line_preview("10%\r20%\r30%", 40), "30%");
    }

    #[test]
    fn partial_line_preview_applies_width_truncation_after_cr_collapse() {
        // "abcdefghij" is the segment after the last `\r`; width-5
        // truncation should then take its tail, "fghij" -- proving the
        // two rules compose (collapse first, then truncate what's left),
        // not just that each works in isolation.
        assert_eq!(partial_line_preview("old\rabcdefghij", 5), "fghij");
    }

    /// End-to-end proof (not just the pure-function unit tests above) that
    /// a long partial line renders its tail on screen, using the same
    /// `TestBackend` methodology as the other console regression tests.
    #[test]
    fn long_partial_line_renders_its_tail_in_the_pane() {
        let width = 40u16;
        let height = 10u16;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut app = App::new();
        let long = "!".repeat(100) + "DONE";
        app.active_session_mut().partial_output = Some(long);
        let theme = Theme::dark();

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, area, &app, &theme);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        // Inner width is 38 (bordered pane); partial renders one row
        // above the input line, i.e. y=7.
        let partial_row = row_text(buffer, 7, width);
        assert!(
            partial_row.contains("DONE"),
            "the partial line's tail (most recent content) should be visible, got {partial_row:?}"
        );
        assert!(
            !partial_row.contains("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"),
            "the partial line should not still be showing its stale head, got {partial_row:?}"
        );
    }

    #[test]
    fn visible_text_leaves_plain_text_unchanged() {
        assert_eq!(visible_text("no controls here"), "no controls here");
    }

    #[test]
    fn visible_text_genuinely_erases_on_backspace_not_just_strips_the_byte() {
        // v0.1.6-v0.1.8's naive "strip the control byte" approach left
        // "backspace" + "here" concatenated as "backspacehere" -- the
        // \x08 byte was gone but nothing was actually erased. A real
        // terminal moves the cursor left on \x08 without erasing; typing
        // "here" starting from that position overwrites the trailing "e"
        // of "backspace" instead of appending after it.
        assert_eq!(visible_text("backspace\x08here"), "backspachere");
    }

    #[test]
    fn visible_text_collapses_a_fortigate_style_erase_sequence() {
        // Real hardware capture: FortiGate's `--More--` paging erases its
        // own prompt with `\r` + spaces + `\r` before printing the next
        // line -- confirmed to survive verbatim into the assembled
        // `RawLine` by `LineAssembler`'s own test. Left unsanitized here,
        // that embedded `\r` gets written to the real terminal (which
        // *does* interpret it, unlike `Line`) and corrupts everything
        // rendered afterward in the same paint.
        assert_eq!(
            visible_text("\r        \rvirtual domain: root"),
            "virtual domain: root"
        );
    }

    #[test]
    fn visible_text_strips_a_color_escape_sequence() {
        // A CSI sequence can carry any number of parameter bytes before
        // its final byte, not just none -- e.g. an SGR color code like
        // `\x1b[1;32m` (bold green). Proves the parameter-byte loop
        // consumes the whole sequence, not just a single-parameter case.
        // Color has no visible effect in this single-line model (no
        // styling pass-through), but the escape bytes themselves must
        // not leak into the rendered text as literal garbage.
        assert_eq!(visible_text("\x1b[1;32mok\x1b[0m"), "ok");
    }

    // Three real-world backspace idioms a device's line-editing echo
    // might use -- whichever one a given device turns out to send,
    // there's a test here already proving ttyt renders it correctly.
    // (v0.1.8 shipped a fix for one specific case, `\x1b[K` alone, that
    // turned out not to be enough on its own -- these cover the ones
    // most likely to be the rest of the real sequence.)

    #[test]
    fn visible_text_erases_via_the_backspace_space_backspace_idiom() {
        // BS moves left, a space overwrites the cell, BS moves left
        // again -- the classic dumb-terminal erase pattern that needs no
        // escape sequences at all.
        assert_eq!(visible_text("user\x08 \x08"), "use ");
    }

    #[test]
    fn visible_text_erases_via_cursor_left_then_erase_in_line() {
        // BS (or `\x1b[D`) moves the cursor left without erasing, then
        // `\x1b[K` erases from there to the end of the line -- a common
        // pairing since it also correctly erases more than one trailing
        // character if the cursor moved back further than one cell.
        assert_eq!(visible_text("user\x08\x1b[K"), "use");
        assert_eq!(visible_text("user\x1b[D\x1b[K"), "use");
    }

    #[test]
    fn visible_text_erases_several_characters_via_repeated_cursor_left_then_one_erase() {
        // A device optimizing multi-character Backspace echoes several
        // bare `\x08` (cursor-left only, no per-character erase) followed
        // by a single trailing `\x1b[K` rather than repeating a full
        // erase pattern per keystroke. The naive v0.1.6-v0.1.8 model
        // (strip control/escape bytes, concatenate what's left) had no
        // concept of the cursor moving backward at all here and would
        // have rendered the untouched "user1234".
        assert_eq!(visible_text("user1234\x08\x08\x08\x08\x1b[K"), "user");
    }

    /// End-to-end proof, not just the pure-function test above: a
    /// scrollback line carrying the exact FortiGate erase sequence must
    /// render as clean text with no `\r` (or any other control character)
    /// present in any rendered cell -- if one were, ratatui would have
    /// stored the literal `\r` byte, crossterm would write it to the real
    /// terminal, and the terminal itself (not ttyt) would act on it,
    /// which is invisible to a `TestBackend` assertion unless it checks
    /// cell *content* directly, as this does.
    #[test]
    fn scrollback_line_with_embedded_carriage_return_renders_with_no_control_bytes() {
        let width = 40u16;
        let height = 10u16;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut app = App::new();
        app.active_session_mut()
            .push_line("\r        \rvirtual domain: root".to_string());
        let theme = Theme::dark();

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, area, &app, &theme);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let symbol = cell.symbol();
            assert!(
                symbol.chars().all(|c| !c.is_control()),
                "no rendered cell should contain a raw control character, found {symbol:?}"
            );
        }

        // Inner height is 8; with only one scrollback line, `Paragraph`'s
        // leading-blank-line padding (see the top-alignment fix above)
        // pushes it down to sit immediately above the input line (y=8),
        // i.e. y=7 -- not the pane's first row.
        let line_row = row_text(buffer, 7, width);
        assert!(
            line_row.contains("virtual domain: root"),
            "the sanitized line content should still be visible, got {line_row:?}"
        );
    }
}
