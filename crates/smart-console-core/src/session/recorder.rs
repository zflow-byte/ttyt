use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::error::CoreError;
use crate::events::SessionEvent;
use crate::session::time_util::UtcParts;

const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Compiled form of `Config::redaction_patterns`.
///
/// A match anywhere in a line redacts the **entire line**, never a partial
/// substring: a partial replacement risks leaving another fragment of a
/// secret visible (e.g. a value that wraps or is echoed oddly), so a match
/// is treated as "this line is sensitive" rather than "this token is
/// sensitive".
pub struct Redactor {
    patterns: Vec<Regex>,
}

impl Redactor {
    pub fn new(patterns: &[String]) -> Result<Redactor, CoreError> {
        let compiled = patterns
            .iter()
            .map(|p| {
                Regex::new(p)
                    .map_err(|e| CoreError::Config(format!("invalid redaction pattern {p:?}: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Redactor { patterns: compiled })
    }

    /// Redact a single assembled line. Must only ever be called with a
    /// complete line (post line-assembler) — redacting partial buffers
    /// would let an in-progress secret slip through before its keyword has
    /// fully arrived.
    pub fn redact(&self, line: &str) -> String {
        if self.patterns.iter().any(|p| p.is_match(line)) {
            REDACTED_PLACEHOLDER.to_string()
        } else {
            line.to_string()
        }
    }
}

/// Writes assembled, redacted session lines to
/// `log_dir/YYYY-MM-DD/HHMMSS.log`.
pub struct SessionRecorder {
    file: File,
    redactor: Redactor,
    path: PathBuf,
}

impl SessionRecorder {
    /// Create a new recording under `log_dir`, named from the current UTC
    /// time. Creates `log_dir/YYYY-MM-DD/` with `0700` permissions and the
    /// log file with `0600` permissions, both set at creation time (not
    /// via a follow-up `chmod`, which would leave a window where the file
    /// exists at the process umask's default permissions).
    pub fn create(
        log_dir: &Path,
        redaction_patterns: &[String],
    ) -> Result<SessionRecorder, CoreError> {
        let now = UtcParts::now();
        let day_dir = log_dir.join(now.date_string());
        create_secure_dir_all(log_dir)?;
        create_secure_dir(&day_dir)?;

        let path = day_dir.join(format!("{}.log", now.time_string()));
        let file = open_secure_file(&path)?;
        let redactor = Redactor::new(redaction_patterns)?;

        Ok(SessionRecorder {
            file,
            redactor,
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Redact and append one assembled line, followed by a newline.
    pub fn record_line(&mut self, line: &str) -> Result<(), CoreError> {
        let redacted = self.redactor.redact(line);
        writeln!(self.file, "{redacted}")?;
        Ok(())
    }
}

/// Drives a `SessionRecorder` from a session's event bus until the sender
/// side is dropped (session ended) or the recorder falls too far behind
/// (`Lagged`, logged and skipped rather than treated as fatal).
pub async fn run(
    mut recorder: SessionRecorder,
    mut events: tokio::sync::broadcast::Receiver<SessionEvent>,
) {
    loop {
        match events.recv().await {
            Ok(SessionEvent::RawLine(line)) => {
                if let Err(e) = recorder.record_line(&line) {
                    tracing::error!(error = %e, "failed to write session log line");
                }
            }
            Ok(SessionEvent::ConnectionStateChanged(state)) => {
                // Not sensitive -- recorded as-is, not passed through the
                // redactor, so the transcript shows exactly when the link
                // dropped/reconnected during the session.
                if let Err(e) =
                    recorder.record_line(&format!("--- connection state: {state:?} ---"))
                {
                    tracing::error!(error = %e, "failed to write session log line");
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped,
                    "session recorder lagged, some lines were not recorded"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(unix)]
fn create_secure_dir_all(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::DirBuilderExt;
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_secure_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(CoreError::Io(e)),
    }
}

#[cfg(not(unix))]
fn create_secure_dir_all(path: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(path).map_err(CoreError::Io)
}

#[cfg(unix)]
fn create_secure_dir(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(CoreError::Io(e)),
    }
}

#[cfg(not(unix))]
fn create_secure_dir(path: &Path) -> Result<(), CoreError> {
    std::fs::create_dir(path).map_err(CoreError::Io)
}

#[cfg(unix)]
fn open_secure_file(path: &Path) -> Result<File, CoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(CoreError::Io)
}

#[cfg(not(unix))]
fn open_secure_file(path: &Path) -> Result<File, CoreError> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(CoreError::Io)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::config::Config;
    use crate::events::LineAssembler;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn scratch_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "smart-console-recorder-test-{}-{n}",
            std::process::id()
        ))
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn enable_secret_line_is_redacted() {
        let redactor = Redactor::new(&Config::default().redaction_patterns).unwrap();
        let redacted = redactor.redact("enable secret 5 $1$abc$somehashvalue");
        assert_eq!(redacted, "[REDACTED]");
        assert!(!redacted.contains("somehashvalue"));
    }

    #[test]
    fn username_password_line_is_redacted() {
        let redactor = Redactor::new(&Config::default().redaction_patterns).unwrap();
        let redacted = redactor.redact("username admin password MyRealPass123");
        assert_eq!(redacted, "[REDACTED]");
        assert!(!redacted.contains("MyRealPass123"));
    }

    #[test]
    fn snmp_community_line_is_redacted() {
        let redactor = Redactor::new(&Config::default().redaction_patterns).unwrap();
        let redacted = redactor.redact("snmp-server community SuperSecretCommunity RO");
        assert_eq!(redacted, "[REDACTED]");
    }

    #[test]
    fn ordinary_output_line_passes_through_unchanged() {
        let redactor = Redactor::new(&Config::default().redaction_patterns).unwrap();
        let line = "GigabitEthernet0/1 is up, line protocol is up";
        assert_eq!(redactor.redact(line), line);
    }

    #[test]
    fn password_split_across_three_reads_is_still_redacted_at_line_boundary() {
        // The recorder must only ever see complete lines from the line
        // assembler -- never raw byte chunks -- otherwise a password
        // split mid-keyword across reads would never match the pattern.
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.feed(b"username admin "), Vec::<String>::new());
        assert_eq!(assembler.feed(b"password Sup"), Vec::<String>::new());
        let lines = assembler.feed(b"erSecret1\r\n");
        assert_eq!(
            lines,
            vec!["username admin password SuperSecret1".to_string()]
        );

        let redactor = Redactor::new(&Config::default().redaction_patterns).unwrap();
        let redacted = redactor.redact(&lines[0]);
        assert_eq!(redacted, "[REDACTED]");
    }

    #[test]
    fn log_file_path_matches_yyyy_mm_dd_hhmmss_layout() {
        let dir = scratch_dir();
        let recorder =
            SessionRecorder::create(&dir, &Config::default().redaction_patterns).unwrap();

        let path = recorder.path();
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(file_name.ends_with(".log"));
        assert_eq!(file_name.len(), "HHMMSS.log".len());

        let day_dir = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(day_dir.len(), "YYYY-MM-DD".len());
        assert!(day_dir.chars().nth(4) == Some('-') && day_dir.chars().nth(7) == Some('-'));

        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn log_dir_and_file_have_secure_permissions_set_at_creation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir();
        let recorder =
            SessionRecorder::create(&dir, &Config::default().redaction_patterns).unwrap();

        let day_dir = recorder.path().parent().unwrap();
        let dir_mode = std::fs::metadata(day_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "log day-directory must be 0700");

        let file_mode = std::fs::metadata(recorder.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "log file must be 0600");

        cleanup(&dir);
    }

    #[test]
    fn record_line_writes_redacted_content_to_disk() {
        let dir = scratch_dir();
        let mut recorder =
            SessionRecorder::create(&dir, &Config::default().redaction_patterns).unwrap();
        recorder.record_line("interface up").unwrap();
        recorder
            .record_line("enable secret 5 shouldnotappear")
            .unwrap();

        let contents = std::fs::read_to_string(recorder.path()).unwrap();
        assert!(contents.contains("interface up"));
        assert!(contents.contains("[REDACTED]"));
        assert!(!contents.contains("shouldnotappear"));

        cleanup(&dir);
    }
}
