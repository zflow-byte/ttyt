use regex::Regex;

use crate::error::CoreError;

/// Compiled form of `Config::dangerous_command_patterns`.
///
/// Cheap to clone: `regex::Regex` shares its compiled state internally, so
/// giving every concurrent session (Phase 3 tabs) its own copy of this
/// guard costs no extra compilation.
#[derive(Clone)]
pub struct DangerousCommandGuard {
    patterns: Vec<Regex>,
}

impl DangerousCommandGuard {
    pub fn new(patterns: &[String]) -> Result<DangerousCommandGuard, CoreError> {
        let compiled = patterns
            .iter()
            .map(|p| {
                Regex::new(p).map_err(|e| {
                    CoreError::Config(format!("invalid dangerous-command pattern {p:?}: {e}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DangerousCommandGuard { patterns: compiled })
    }

    /// No patterns configured -- `is_dangerous` never matches. The default
    /// for contexts that don't have a `Config` to build a real guard from
    /// (e.g. `ttyt-ui`'s tests), never used for a real `connect` session.
    pub fn empty() -> DangerousCommandGuard {
        DangerousCommandGuard {
            patterns: Vec::new(),
        }
    }

    /// Whether `cmd` matches any configured dangerous-command pattern and
    /// must be confirmed before being sent to the device.
    pub fn is_dangerous(&self, cmd: &str) -> bool {
        self.patterns.iter().any(|p| p.is_match(cmd))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn empty_guard_never_flags_anything_as_dangerous() {
        let guard = DangerousCommandGuard::empty();
        assert!(!guard.is_dangerous("reload"));
        assert!(!guard.is_dangerous("show version"));
    }

    #[test]
    fn configured_pattern_flags_matching_commands_only() {
        let guard =
            DangerousCommandGuard::new(&["reload".to_string(), "shutdown".to_string()]).unwrap();
        assert!(guard.is_dangerous("reload"));
        assert!(guard.is_dangerous("interface Gi0/1\nshutdown"));
        assert!(!guard.is_dangerous("show version"));
    }

    #[test]
    fn invalid_pattern_is_a_config_error_not_a_panic() {
        let result = DangerousCommandGuard::new(&["(unclosed".to_string()]);
        assert!(result.is_err());
    }
}
