/// Turns a raw byte stream (arriving in arbitrarily-sized, arbitrarily-split
/// reads from a serial port) into discrete lines.
///
/// Splits on `\n`; a trailing `\r` (CRLF line endings, which most network
/// device CLIs use) is stripped. Bytes after the last newline are held back
/// as a partial line until more data arrives or [`LineAssembler::flush`] is
/// called.
#[derive(Debug, Default)]
pub struct LineAssembler {
    buffer: Vec<u8>,
}

impl LineAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed newly-read bytes in. Returns zero or more complete lines
    /// assembled from this call's bytes plus anything buffered from
    /// earlier calls.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);

        let mut lines = Vec::new();
        while let Some(newline_pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line_bytes: Vec<u8> = self.buffer.drain(..=newline_pos).collect();
            line_bytes.pop(); // drop the '\n'
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            lines.push(String::from_utf8_lossy(&line_bytes).into_owned());
        }
        lines
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

    #[test]
    fn line_split_across_multiple_reads_assembles_correctly() {
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"Switch"), Vec::<String>::new());
        assert_eq!(assembler.feed(b"> "), Vec::<String>::new());
        assert_eq!(assembler.feed(b"\r\n"), vec!["Switch> ".to_string()]);
    }

    #[test]
    fn multiple_complete_lines_in_one_read() {
        let mut assembler = LineAssembler::new();
        let lines = assembler.feed(b"line one\r\nline two\r\nline three\r\n");
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
            assembler.feed(b"unix style\n"),
            vec!["unix style".to_string()]
        );
    }

    #[test]
    fn flush_returns_buffered_partial_line_with_no_trailing_newline() {
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"no newline yet"), Vec::<String>::new());
        assert_eq!(assembler.flush(), Some("no newline yet".to_string()));
    }

    #[test]
    fn flush_returns_none_when_nothing_buffered() {
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"complete\n"), vec!["complete".to_string()]);
        assert_eq!(assembler.flush(), None);
    }
}
