use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// ttyt's persisted configuration (`config.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Baud rates offered when connecting to a device.
    pub baud_candidates: Vec<u32>,
    /// Root directory session recordings are written under
    /// (`logs/YYYY-MM-DD/HHMMSS.log` relative to this).
    pub log_dir: PathBuf,
    /// Session logs older than this are swept on startup.
    pub log_retention_days: u32,
    /// Theme name (currently only "dark" is implemented).
    pub theme: String,
    /// Regex patterns that trigger a confirm-before-send prompt (Phase 3).
    pub dangerous_command_patterns: Vec<String>,
    /// Case-insensitive regex patterns checked against every recorded/
    /// history line; a match redacts the entire line before it is written
    /// to disk (see `session::recorder::Redactor`).
    pub redaction_patterns: Vec<String>,
    /// Persistent command history file (Phase 2, Ctrl-R search).
    ///
    /// `#[serde(default = ...)]`: added after Phase 1 shipped, so a
    /// Phase-1-shaped `config.toml` on disk (missing these keys) must
    /// still parse -- without a default, `toml::from_str` would fail and
    /// the app wouldn't start for anyone who already has a config file.
    #[serde(default = "default_history_path")]
    pub history_path: PathBuf,
    /// Older entries beyond this count are dropped from the in-memory/
    /// search view (the on-disk file is not truncated retroactively).
    #[serde(default = "default_history_max_entries")]
    pub history_max_entries: usize,
}

impl Config {
    /// Load config from the OS-standard path, creating it with defaults on
    /// first run if it does not exist yet.
    pub fn load() -> Result<Config, CoreError> {
        Self::load_from_path(&Self::default_path()?)
    }

    /// The OS-standard config file path
    /// (macOS: `~/Library/Application Support/ttyt/config.toml`).
    pub fn default_path() -> Result<PathBuf, CoreError> {
        project_dirs().map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Load from an explicit path. Missing file -> defaults are returned and
    /// persisted to `path` for next time; any other I/O or parse error is
    /// propagated.
    pub fn load_from_path(path: &Path) -> Result<Config, CoreError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).map_err(|e| {
                CoreError::Config(format!("invalid config at {}: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Config::default();
                config.save_to_path(path)?;
                Ok(config)
            }
            Err(e) => Err(CoreError::Io(e)),
        }
    }

    /// Serialize and write this config to `path`, creating parent
    /// directories as needed.
    pub fn save_to_path(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rendered = toml::to_string_pretty(self)
            .map_err(|e| CoreError::Config(format!("failed to serialize config: {e}")))?;
        std::fs::write(path, rendered)?;
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        let log_dir = project_dirs()
            .map(|dirs| dirs.data_dir().join("logs"))
            .unwrap_or_else(|_| PathBuf::from("logs"));

        Config {
            baud_candidates: vec![9600, 38400, 57600, 115200],
            log_dir,
            log_retention_days: 90,
            theme: "dark".to_string(),
            dangerous_command_patterns: vec![
                "reload".to_string(),
                "write erase".to_string(),
                "erase startup-config".to_string(),
                "no shutdown".to_string(),
                "shutdown".to_string(),
                // Comware/H3C's reboot verb -- "reload" (above) is
                // Cisco-shaped and does not match it, which would
                // otherwise let a Comware `reboot` sail past this guard
                // with no confirmation despite Comware being a fully
                // supported vendor.
                "reboot".to_string(),
                // JunOS equivalents; JunOS has no "reload"/"shutdown"
                // verbs at all in this shape.
                "request system reboot".to_string(),
                "request system zeroize".to_string(),
            ],
            redaction_patterns: vec![
                r"(?i)\bpassword\b".to_string(),
                r"(?i)\bsecret\b".to_string(),
                r"(?i)\bcommunity\b".to_string(),
                r"(?i)\bpre-shared-key\b".to_string(),
                r"(?i)\bpsk\b".to_string(),
                // Broader than the above on purpose: `key-string`,
                // `authentication key 7 <hash>`, `radius-server key`,
                // `tacacs-server key`, and `ntp authentication-key` all
                // carry a credential but don't match "password"/"secret".
                // Over-redacting a benign line containing "key" is the
                // stated safe default here, not an oversight.
                r"(?i)\bkey\b".to_string(),
                // SNMPv3 `snmp-server user <name> <group> v3 auth <algo>
                // <authpass> priv <algo> <privpass>` carries two real
                // passphrases and matches none of the patterns above.
                r"(?i)\bauth\b".to_string(),
                r"(?i)\bpriv\b".to_string(),
                // VRRP/HSRP plaintext authentication (`vrrp 1
                // authentication text CLEARTEXT`, `standby 1
                // authentication CLEARTEXT`) carries a real value in the
                // clear and matches none of the patterns above either.
                r"(?i)\bauthentication\b".to_string(),
                r"(?i)\bcleartext\b".to_string(),
            ],
            history_path: default_history_path(),
            history_max_entries: default_history_max_entries(),
        }
    }
}

fn project_dirs() -> Result<ProjectDirs, CoreError> {
    ProjectDirs::from("dev", "ttyt", "ttyt")
        .ok_or_else(|| CoreError::Config("could not determine OS config directory".to_string()))
}

fn default_history_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_dir().join("history.txt"))
        .unwrap_or_else(|_| PathBuf::from("history.txt"))
}

fn default_history_max_entries() -> usize {
    1000
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique scratch path per test so parallel test runs never collide.
    fn scratch_path(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ttyt-test-{label}-{}-{n}.toml", std::process::id()))
    }

    #[test]
    fn default_includes_all_spec_bauds() {
        let config = Config::default();
        for baud in [9600, 38400, 57600, 115200] {
            assert!(
                config.baud_candidates.contains(&baud),
                "missing baud {baud} in default candidates"
            );
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let config = Config::default();
        let rendered = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&rendered).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn load_from_missing_path_falls_back_to_default_and_persists() {
        let path = scratch_path("missing");
        assert!(!path.exists());

        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded, Config::default());
        assert!(
            path.exists(),
            "default config should be persisted on first load"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_existing_path_reads_saved_values() {
        let path = scratch_path("existing");
        let config = Config {
            log_retention_days: 42,
            ..Config::default()
        };
        config.save_to_path(&path).unwrap();

        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded.log_retention_days, 42);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn phase_1_shaped_toml_missing_history_fields_still_loads() {
        // A config.toml written by Phase 1 (before history_path/
        // history_max_entries existed) must still parse -- otherwise
        // every existing user's config file breaks the app on upgrade.
        let old_toml = r#"
            baud_candidates = [9600, 38400, 57600, 115200]
            log_dir = "/tmp/ttyt-old/logs"
            log_retention_days = 90
            theme = "dark"
            dangerous_command_patterns = ["reload"]
            redaction_patterns = ["(?i)\\bpassword\\b"]
        "#;

        let config: Config = toml::from_str(old_toml).unwrap();
        assert_eq!(config.history_max_entries, 1000);
        assert!(
            config
                .history_path
                .to_string_lossy()
                .contains("history.txt")
        );
    }

    #[test]
    fn load_from_malformed_file_errors_instead_of_panicking() {
        let path = scratch_path("malformed");
        std::fs::write(&path, "not = [valid toml").unwrap();

        let result = Config::load_from_path(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    /// Pre-publish security review (2026-07-30) found that the original
    /// default redaction patterns had no path to catching SNMPv3
    /// auth/priv passphrases or VRRP/HSRP plaintext authentication --
    /// neither contains "password"/"secret"/"community"/"key". Regression
    /// test against `Redactor` directly, not just the pattern strings, so
    /// a future edit that accidentally breaks one of these regexes fails
    /// loudly here.
    #[test]
    fn default_redaction_patterns_cover_snmpv3_and_fhrp_cleartext_auth() {
        let redactor = crate::Redactor::new(&Config::default().redaction_patterns).unwrap();
        assert_eq!(
            redactor.redact(
                "snmp-server user bob GROUP1 v3 auth sha AuthPass128 priv aes 128 PrivPass256"
            ),
            "[REDACTED]"
        );
        assert_eq!(
            redactor.redact("vrrp 1 authentication text CLEARTEXT"),
            "[REDACTED]"
        );
        assert_eq!(
            redactor.redact("standby 1 authentication CLEARTEXT"),
            "[REDACTED]"
        );
    }

    /// Same review found the default dangerous-command patterns were
    /// Cisco-IOS-shaped only: Comware's actual reboot verb is `reboot`,
    /// not `reload`, and JunOS has neither -- so a Comware/JunOS reboot
    /// previously sailed past the confirm-before-send guard unconfirmed
    /// on two vendors this project fully supports.
    #[test]
    fn default_dangerous_command_patterns_cover_comware_and_junos_reboot_verbs() {
        let guard =
            crate::DangerousCommandGuard::new(&Config::default().dangerous_command_patterns)
                .unwrap();
        assert!(guard.is_dangerous("reboot"));
        assert!(guard.is_dangerous("request system reboot"));
        assert!(guard.is_dangerous("request system zeroize"));
    }
}
