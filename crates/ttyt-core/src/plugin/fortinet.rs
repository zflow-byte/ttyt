use crate::model::{DetectionResult, ParsedEvent, PromptInfo};
use crate::plugin::VendorPlugin;

/// Fortinet FortiOS -- a **recognition-only stub** (Task 3.10), not a
/// supported vendor: `detect` proves this plugin can tell a FortiGate
/// apart from the other five so it stops being silently misreported as
/// `Unknown`, but `parse_prompt`/`parse_output` are intentionally
/// unimplemented. A device detected here shows its vendor in the header
/// and then sits at hostname/mode `-` for the rest of the session --
/// that's the correct behavior for "recognized but not yet supported", not
/// a bug (see `PluginRegistry::detect`: once `Detected`, the detector only
/// calls this plugin's own parsing, which yields nothing).
///
/// Registered **last** in `PluginRegistry::with_default_plugins` on
/// purpose: `detect` matches on a loose banner token
/// (`"FortiGate"`/`"FortiOS"`/`"Fortinet"`) rather than a structural
/// format the way the other five do, and first-match-wins registration
/// order means a loosely-matched stub must never have a chance to shadow
/// a real vendor's plugin.
///
/// FortiOS's version string (`v7.0.5,build0304,220401 (GA)`) doesn't
/// follow the `Version X` convention `plugin::common::extract_version_token`
/// looks for, and a bespoke parser for it isn't worth writing for a plugin
/// that doesn't parse prompts or output yet either -- left as `None` until
/// this plugin gets real support.
pub struct FortinetPlugin;

const DETECTION_TOKENS: [&str; 3] = ["FortiGate", "FortiOS", "Fortinet"];

impl VendorPlugin for FortinetPlugin {
    fn id(&self) -> &'static str {
        "fortinet"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        if !DETECTION_TOKENS.iter().any(|token| banner.contains(token)) {
            return None;
        }
        Some(DetectionResult {
            vendor: "Fortinet".to_string(),
            platform: "not yet supported".to_string(),
            version: None,
        })
    }

    fn parse_prompt(&self, _line: &str) -> Option<PromptInfo> {
        None
    }

    fn parse_output(&self, _line: &str) -> Vec<ParsedEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn detects_fortigate_banner() {
        let result = FortinetPlugin
            .detect("FortiGate-60E v7.0.5,build0304,220401 (GA)")
            .unwrap();
        assert_eq!(result.vendor, "Fortinet");
        assert_eq!(result.platform, "not yet supported");
        assert_eq!(result.version, None);
    }

    #[test]
    fn detects_fortios_banner_without_the_fortigate_token() {
        assert!(FortinetPlugin.detect("Welcome to FortiOS").is_some());
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(
            FortinetPlugin
                .detect("Cisco IOS Software, Version 15.2(2)E7")
                .is_none()
        );
    }

    #[test]
    fn parse_prompt_is_unimplemented() {
        assert!(FortinetPlugin.parse_prompt("FGT60E #").is_none());
    }

    #[test]
    fn parse_output_is_unimplemented() {
        assert_eq!(
            FortinetPlugin.parse_output("some device output line"),
            Vec::new()
        );
    }
}
