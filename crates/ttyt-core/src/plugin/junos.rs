use crate::model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode};
use crate::plugin::VendorPlugin;

/// Juniper JunOS.
///
/// Banner/prompt fixtures below are reconstructed from general JunOS
/// documentation/knowledge, **not verified against real hardware**.
///
/// Detection caveat, documented rather than papered over: unlike Cisco
/// IOS (which prints a large banner before login), a JunOS device's
/// pre-login console output is often minimal (just a hostname and a
/// login prompt) and may not contain the literal "JUNOS" token at all
/// until after login (e.g. in `show version` output, or the
/// `--- JUNOS 21.4R3.15 Software Release ... ---` line some releases
/// print right after authentication). Detection may only succeed once
/// output past the login prompt is visible.
pub struct JunosPlugin;

impl VendorPlugin for JunosPlugin {
    fn id(&self) -> &'static str {
        "junos"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        if !banner.contains("JUNOS") {
            return None;
        }
        Some(DetectionResult {
            vendor: "Juniper".to_string(),
            platform: "JunOS".to_string(),
            version: extract_junos_version(banner),
        })
    }

    fn parse_prompt(&self, line: &str) -> Option<PromptInfo> {
        let line = line.trim_end();

        // JunOS prints the current configuration hierarchy as its own
        // line (e.g. "[edit interfaces ge-0/0/0]") immediately before the
        // actual prompt line. That's a separate line, not a prompt itself,
        // and correlating it with the prompt that follows would need
        // cross-line state this trait's single-line `parse_prompt` isn't
        // shaped for -- explicitly not attempted here (known limitation),
        // rather than half-implemented.
        if line.starts_with('[') {
            return None;
        }

        let (mode, without_mode_char) = if let Some(rest) = line.strip_suffix('>') {
            (PromptMode::User, rest)
        } else {
            // JunOS has no privileged-exec tier the way Cisco does --
            // `#` means configuration mode, not "more privileged".
            (PromptMode::Config, line.strip_suffix('#')?)
        };

        // "user@hostname" -- the hostname is the part after '@', not the
        // whole token.
        let (_user, hostname) = without_mode_char.split_once('@')?;
        if hostname.is_empty() {
            return None;
        }

        Some(PromptInfo {
            hostname: hostname.to_string(),
            mode,
            privilege: None,
        })
    }

    fn parse_output(&self, _line: &str) -> Vec<ParsedEvent> {
        // Not implemented in Phase 2: JunOS routes system messages to
        // syslog/files rather than printing them unsolicited to an
        // interactive CLI session by default (unlike Cisco IOS), so there
        // isn't a reliable "this line is a syslog message" shape to match
        // against console output without a real device to verify against.
        Vec::new()
    }
}

/// JunOS's release banner puts the version right after the literal
/// "JUNOS " token (e.g. `"--- JUNOS 21.4R3.15 Software Release ..."`),
/// unlike the "Version X" convention most other vendors use.
fn extract_junos_version(banner: &str) -> Option<String> {
    let idx = banner.find("JUNOS ")?;
    let after = &banner[idx + "JUNOS ".len()..];
    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let version = &after[..end];
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const BANNER: &str = "--- JUNOS 21.4R3.15 Software Release [export] (Junos) ---";

    #[test]
    fn detects_junos_banner() {
        let result = JunosPlugin.detect(BANNER).unwrap();
        assert_eq!(result.vendor, "Juniper");
        assert_eq!(result.platform, "JunOS");
        assert_eq!(result.version.as_deref(), Some("21.4R3.15"));
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(
            JunosPlugin
                .detect("Cisco IOS Software, Version 15.2(2)E7")
                .is_none()
        );
    }

    #[test]
    fn prompt_operational_mode() {
        let info = JunosPlugin.parse_prompt("admin@router1> ").unwrap();
        assert_eq!(info.hostname, "router1");
        assert_eq!(info.mode, PromptMode::User);
        assert_eq!(info.privilege, None);
    }

    #[test]
    fn prompt_configuration_mode_is_config_not_privileged() {
        let info = JunosPlugin.parse_prompt("admin@router1# ").unwrap();
        assert_eq!(info.hostname, "router1");
        assert_eq!(info.mode, PromptMode::Config);
    }

    #[test]
    fn hostname_is_the_part_after_at_sign_not_the_whole_token() {
        let info = JunosPlugin
            .parse_prompt("lab-admin@core-router-02>")
            .unwrap();
        assert_eq!(info.hostname, "core-router-02");
    }

    #[test]
    fn edit_hierarchy_line_is_not_treated_as_a_prompt() {
        assert!(
            JunosPlugin
                .parse_prompt("[edit interfaces ge-0/0/0]")
                .is_none()
        );
    }

    #[test]
    fn line_without_at_sign_is_not_a_junos_prompt() {
        assert!(JunosPlugin.parse_prompt("Switch>").is_none());
    }

    #[test]
    fn non_prompt_line_is_not_parsed_as_a_prompt() {
        assert!(
            JunosPlugin
                .parse_prompt("ge-0/0/0 up, physical link is up")
                .is_none()
        );
    }

    #[test]
    fn parse_output_is_empty_pending_real_hardware_verification() {
        assert_eq!(
            JunosPlugin.parse_output("Jan  1 00:00:00 router1 mgd[1234]: UI_COMMIT: something"),
            Vec::new()
        );
    }
}
