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

/// One unit of output assembled from the device's byte stream: a complete,
/// newline-terminated line; a pagination prompt recognized from an
/// unterminated buffer tail (these never end in a newline -- the device is
/// blocked waiting for a single keystroke, not more text); or a live
/// preview of a line still in progress, so the console can render output
/// as it streams in like a real terminal instead of only after each
/// newline (see `feed`'s doc comment for why this is safe to show on
/// screen but must never be recorded to the session log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembledOutput {
    Line(String),
    PaginationPrompt(String),
    Partial(String),
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
    /// earlier calls; a pagination prompt if the remaining unterminated
    /// buffer matches a known marker; otherwise, if anything is still
    /// buffered with no newline yet, a `Partial` carrying the buffer's
    /// current content so far.
    ///
    /// `Partial` is meant for on-screen display only -- callers must never
    /// write it to the session log/history the way a `Line` is. A
    /// redaction pattern like `\bpassword\b` matches whole keywords, so a
    /// fragment like `username admin passw` (word not fully arrived yet)
    /// would pass through unredacted even though the eventual complete
    /// line matches and gets `[REDACTED]` -- recording the fragment
    /// separately would let a reader reconstruct the secret from
    /// unredacted pieces despite the finished line being safe.
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
            } else {
                output.push(AssembledOutput::Partial(partial.into_owned()));
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
                other => panic!("expected only lines, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn line_split_across_multiple_reads_assembles_correctly() {
        let mut assembler = LineAssembler::new();
        assert_eq!(
            assembler.feed(b"Switch"),
            vec![AssembledOutput::Partial("Switch".to_string())]
        );
        assert_eq!(
            assembler.feed(b"> "),
            vec![AssembledOutput::Partial("Switch> ".to_string())]
        );
        assert_eq!(
            lines_only(assembler.feed(b"\r\n")),
            vec!["Switch> ".to_string()]
        );
    }

    #[test]
    fn partial_is_emitted_live_and_replaced_by_the_line_once_terminated() {
        // The console renders `Partial` as a growing preview, then
        // discards it in favor of the authoritative `Line` once the
        // newline arrives -- this is what makes that safe: the feed()
        // call that completes the line does NOT also emit a stale
        // Partial for the same content.
        let mut assembler = LineAssembler::new();
        assert_eq!(
            assembler.feed(b"show run"),
            vec![AssembledOutput::Partial("show run".to_string())]
        );
        assert_eq!(
            assembler.feed(b"ning-config\n"),
            vec![AssembledOutput::Line("show running-config".to_string())]
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
            vec![AssembledOutput::Partial("no newline yet".to_string())]
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
        assert_eq!(
            assembler.feed(b"--Mo"),
            vec![AssembledOutput::Partial("--Mo".to_string())]
        );
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
            vec![AssembledOutput::Partial("Router#show run".to_string())]
        );
        assert_eq!(assembler.flush(), Some("Router#show run".to_string()));
    }
}
