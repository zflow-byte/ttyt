/// Known "press a key to continue" pagination markers, checked against the
/// still-unterminated tail of the buffer (no trailing newline). Kept
/// vendor-agnostic and generic rather than routed through `VendorPlugin`:
/// pagination can appear before the vendor is even detected (a long banner
/// can itself page), and the device is blocked waiting for a keypress it
/// will never get -- if detection had to run first, that's a permanent
/// deadlock on connect. JunOS's marker carries a variable progress suffix
/// (`---(more 27%)---`), hence the prefix-only match for it.
const PAGINATION_MARKERS: &[&str] = &[
    "--More--",       // Cisco, Dell OS10, Aruba CX
    "---- More ----", // Comware/H3C
    "---(more",       // JunOS: "---(more)---" or "---(more N%)---"
];

fn matches_pagination_marker(partial: &str) -> bool {
    PAGINATION_MARKERS
        .iter()
        .any(|marker| partial.contains(marker))
}

/// One unit of output assembled from the device's byte stream: either a
/// complete, newline-terminated line, or a pagination prompt recognized
/// from an unterminated buffer tail (these never end in a newline -- the
/// device is blocked waiting for a single keystroke, not more text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembledOutput {
    Line(String),
    PaginationPrompt(String),
}

/// Turns a raw byte stream (arriving in arbitrarily-sized, arbitrarily-split
/// reads from a serial port) into discrete lines.
///
/// Splits on `\n`; a trailing `\r` (CRLF line endings, which most network
/// device CLIs use) is stripped. Bytes after the last newline are held back
/// as a partial line until more data arrives, a recognized pagination
/// marker appears in them, or [`LineAssembler::flush`] is called.
#[derive(Debug, Default)]
pub struct LineAssembler {
    buffer: Vec<u8>,
}

impl LineAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed newly-read bytes in. Returns zero or more assembled outputs:
    /// complete lines from this call's bytes plus anything buffered from
    /// earlier calls, and a pagination prompt if the remaining unterminated
    /// buffer matches a known marker.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<AssembledOutput> {
        self.buffer.extend_from_slice(bytes);

        let mut output = Vec::new();
        while let Some(newline_pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line_bytes: Vec<u8> = self.buffer.drain(..=newline_pos).collect();
            line_bytes.pop(); // drop the '\n'
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            output.push(AssembledOutput::Line(
                String::from_utf8_lossy(&line_bytes).into_owned(),
            ));
        }

        // Check what's left (no trailing newline yet) for a pagination
        // marker. Drain it on match -- otherwise the next page's bytes
        // would append onto "--More--" and eventually surface as one
        // mangled line once a newline finally arrives.
        if !self.buffer.is_empty() {
            let partial = String::from_utf8_lossy(&self.buffer);
            if matches_pagination_marker(&partial) {
                let prompt = partial.into_owned();
                self.buffer.clear();
                output.push(AssembledOutput::PaginationPrompt(prompt));
            }
        }

        output
    }

    /// Flush any remaining buffered partial line — e.g. on disconnect,
    /// where the device's last output has no trailing newline. Returns
    /// `None` if nothing is buffered.
    pub fn flush(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            None
        } else {
            let remaining = std::mem::take(&mut self.buffer);
            Some(String::from_utf8_lossy(&remaining).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn lines_only(output: Vec<AssembledOutput>) -> Vec<String> {
        output
            .into_iter()
            .map(|item| match item {
                AssembledOutput::Line(s) => s,
                AssembledOutput::PaginationPrompt(s) => {
                    panic!("expected only lines, got a pagination prompt: {s:?}")
                }
            })
            .collect()
    }

    #[test]
    fn line_split_across_multiple_reads_assembles_correctly() {
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"Switch"), Vec::<AssembledOutput>::new());
        assert_eq!(assembler.feed(b"> "), Vec::<AssembledOutput>::new());
        assert_eq!(
            lines_only(assembler.feed(b"\r\n")),
            vec!["Switch> ".to_string()]
        );
    }

    #[test]
    fn multiple_complete_lines_in_one_read() {
        let mut assembler = LineAssembler::new();
        let lines = lines_only(assembler.feed(b"line one\r\nline two\r\nline three\r\n"));
        assert_eq!(
            lines,
            vec![
                "line one".to_string(),
                "line two".to_string(),
                "line three".to_string(),
            ]
        );
    }

    #[test]
    fn bare_lf_without_cr_is_also_accepted() {
        let mut assembler = LineAssembler::new();
        assert_eq!(
            lines_only(assembler.feed(b"unix style\n")),
            vec!["unix style".to_string()]
        );
    }

    #[test]
    fn flush_returns_buffered_partial_line_with_no_trailing_newline() {
        let mut assembler = LineAssembler::new();
        assert_eq!(
            assembler.feed(b"no newline yet"),
            Vec::<AssembledOutput>::new()
        );
        assert_eq!(assembler.flush(), Some("no newline yet".to_string()));
    }

    #[test]
    fn flush_returns_none_when_nothing_buffered() {
        let mut assembler = LineAssembler::new();
        assert_eq!(
            lines_only(assembler.feed(b"complete\n")),
            vec!["complete".to_string()]
        );
        assert_eq!(assembler.flush(), None);
    }

    #[test]
    fn cisco_style_more_prompt_with_no_newline_is_recognized() {
        let mut assembler = LineAssembler::new();
        let output = assembler.feed(b"line one\r\n--More--");
        assert_eq!(
            output,
            vec![
                AssembledOutput::Line("line one".to_string()),
                AssembledOutput::PaginationPrompt("--More--".to_string()),
            ]
        );
        // Buffer was drained on match -- nothing left to mangle later.
        assert_eq!(assembler.flush(), None);
    }

    #[test]
    fn more_prompt_split_across_reads_is_still_recognized() {
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"--Mo"), Vec::<AssembledOutput>::new());
        let output = assembler.feed(b"re--");
        assert_eq!(
            output,
            vec![AssembledOutput::PaginationPrompt("--More--".to_string())]
        );
    }

    #[test]
    fn comware_more_prompt_is_recognized() {
        let mut assembler = LineAssembler::new();
        let output = assembler.feed(b"  ---- More ----");
        assert_eq!(
            output,
            vec![AssembledOutput::PaginationPrompt(
                "  ---- More ----".to_string()
            )]
        );
    }

    #[test]
    fn junos_more_prompt_with_progress_percentage_is_recognized() {
        let mut assembler = LineAssembler::new();
        let output = assembler.feed(b"---(more 27%)---");
        assert_eq!(
            output,
            vec![AssembledOutput::PaginationPrompt(
                "---(more 27%)---".to_string()
            )]
        );
    }

    #[test]
    fn ordinary_partial_line_without_a_marker_stays_buffered() {
        let mut assembler = LineAssembler::new();
        assert_eq!(
            assembler.feed(b"Router#show run"),
            Vec::<AssembledOutput>::new()
        );
        assert_eq!(assembler.flush(), Some("Router#show run".to_string()));
    }
}
