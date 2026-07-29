use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// smart-console's persisted configuration (`config.toml`).
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
    /// Regex patterns that trigger a confirm-before-send prompt (Phase 3)
    /// and are treated as sensitive in redaction tooling.
    pub dangerous_command_patterns: Vec<String>,
}

impl Config {
    /// Load config from the OS-standard path, creating it with defaults on
    /// first run if it does not exist yet.
    pub fn load() -> Result<Config, CoreError> {
        Self::load_from_path(&Self::default_path()?)
    }

    /// The OS-standard config file path
    /// (macOS: `~/Library/Application Support/smart-console/config.toml`).
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
            ],
        }
    }
}

fn project_dirs() -> Result<ProjectDirs, CoreError> {
    ProjectDirs::from("dev", "smart-console", "smart-console")
        .ok_or_else(|| CoreError::Config("could not determine OS config directory".to_string()))
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
        std::env::temp_dir().join(format!(
            "smart-console-test-{label}-{}-{n}.toml",
            std::process::id()
        ))
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
    fn load_from_malformed_file_errors_instead_of_panicking() {
        let path = scratch_path("malformed");
        std::fs::write(&path, "not = [valid toml").unwrap();

        let result = Config::load_from_path(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }
}
