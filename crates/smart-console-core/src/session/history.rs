use std::io::Write;
use std::path::Path;

use crate::error::CoreError;
use crate::session::recorder::Redactor;
use crate::session::secure_fs::{create_secure_dir_all, open_secure_file};

/// Persistent, redacted command history (Ctrl-R search).
///
/// History is not exempt from the redaction control that applies to
/// session recordings: a submitted `enable secret ...` line is exactly as
/// sensitive in history as it is in a session transcript. Entries are
/// redacted **before** being kept in memory or written to disk -- the
/// unredacted text is never retained past the moment it's typed, which
/// means a Ctrl-R search can only ever surface `[REDACTED]` for a past
/// sensitive command, never the original value. That's intentional: the
/// alternative (keeping the real value searchable) is the exact problem
/// this control exists to prevent.
pub struct CommandHistory {
    file: std::fs::File,
    redactor: Redactor,
    entries: Vec<String>,
    max_entries: usize,
}

impl CommandHistory {
    /// Opens (creating if needed) the history file at `path`, loading
    /// existing entries (most recent `max_entries` kept) for search.
    pub fn open(
        path: &Path,
        redaction_patterns: &[String],
        max_entries: usize,
    ) -> Result<CommandHistory, CoreError> {
        if let Some(parent) = path.parent() {
            create_secure_dir_all(parent)?;
        }

        let mut entries: Vec<String> = match std::fs::read_to_string(path) {
            Ok(contents) => contents.lines().map(str::to_string).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(CoreError::Io(e)),
        };
        if entries.len() > max_entries {
            entries.drain(..entries.len() - max_entries);
        }

        let file = open_secure_file(path)?;
        let redactor = Redactor::new(redaction_patterns)?;

        Ok(CommandHistory {
            file,
            redactor,
            entries,
            max_entries,
        })
    }

    /// Entries loaded/appended so far, oldest first -- used to seed
    /// `ui::App::history` at startup.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Redacts and appends one submitted command to both the persistent
    /// file and the in-memory `entries()` list.
    pub fn append(&mut self, command: &str) -> Result<(), CoreError> {
        let redacted = self.redactor.redact(command);
        writeln!(self.file, "{redacted}")?;

        self.entries.push(redacted);
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::config::Config;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A history file path nested inside its own fresh per-test directory
    /// -- matching real usage, where `history_path`'s parent is
    /// smart-console's own data directory, never the shared system temp
    /// dir directly. `CommandHistory::open` secures that parent directory
    /// (see `secure_fs::create_secure_dir_all`'s doc comment on why it
    /// only tightens the leaf, never ancestors it doesn't own) -- placing
    /// the file straight in `temp_dir()` would make that parent the
    /// shared system temp dir itself, which this process can't chmod.
    fn scratch_path(label: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "smart-console-history-test-{label}-{}-{n}",
                std::process::id()
            ))
            .join("history.txt")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn opening_missing_file_starts_with_no_entries() {
        let path = scratch_path("missing");
        let history =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();
        assert_eq!(history.entries(), &[] as &[String]);
        cleanup(&path);
    }

    #[test]
    fn append_persists_and_is_visible_via_entries() {
        let path = scratch_path("append");
        let mut history =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();

        history.append("show version").unwrap();
        history.append("show ip interface brief").unwrap();

        assert_eq!(
            history.entries(),
            &[
                "show version".to_string(),
                "show ip interface brief".to_string()
            ]
        );

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("show version"));
        assert!(on_disk.contains("show ip interface brief"));

        cleanup(&path);
    }

    #[test]
    fn sensitive_command_is_redacted_before_being_kept_or_written() {
        let path = scratch_path("redact");
        let mut history =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();

        history
            .append("username admin password MyRealSecret1")
            .unwrap();

        assert_eq!(history.entries(), &["[REDACTED]".to_string()]);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("MyRealSecret1"));

        cleanup(&path);
    }

    #[test]
    fn reopening_loads_previously_persisted_entries() {
        let path = scratch_path("reopen");
        {
            let mut history =
                CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();
            history.append("show version").unwrap();
        }

        let reopened =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();
        assert_eq!(reopened.entries(), &["show version".to_string()]);

        cleanup(&path);
    }

    #[test]
    fn entries_are_bounded_to_max_entries_on_load_and_append() {
        let path = scratch_path("bounded");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "cmd0\ncmd1\ncmd2\ncmd3\ncmd4\n").unwrap();

        let mut history =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 3).unwrap();
        assert_eq!(
            history.entries(),
            &["cmd2".to_string(), "cmd3".to_string(), "cmd4".to_string()],
            "load should keep only the most recent max_entries"
        );

        history.append("cmd5").unwrap();
        assert_eq!(
            history.entries(),
            &["cmd3".to_string(), "cmd4".to_string(), "cmd5".to_string()]
        );

        cleanup(&path);
    }

    #[cfg(unix)]
    #[test]
    fn history_file_has_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = scratch_path("perms");
        let _history =
            CommandHistory::open(&path, &Config::default().redaction_patterns, 1000).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "history file must be 0600, same as session logs"
        );

        cleanup(&path);
    }
}
