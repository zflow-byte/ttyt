use crate::model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode};
use crate::plugin::VendorPlugin;

/// HPE Aruba Networking ArubaOS-CX -- interface-centric and Cisco-shaped
/// (`>`/`#`, `(config)#`, `(config-if)#`), unlike Aruba's older
/// ProVision/ArubaOS-Switch line (VLAN-centric, not handled by this
/// plugin). This mirrors `CiscoPlugin`'s structure closely, using the
/// full `config-` keyword (not OS10's abbreviated `conf-`).
///
/// Banner/prompt fixtures below are reconstructed from general ArubaOS-CX
/// documentation/knowledge, **not verified against real hardware**.
pub struct ArubaCxPlugin;

impl VendorPlugin for ArubaCxPlugin {
    fn id(&self) -> &'static str {
        "aruba-cx"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        if !banner.contains("ArubaOS-CX") {
            return None;
        }
        Some(DetectionResult {
            vendor: "Aruba".to_string(),
            platform: "ArubaOS-CX".to_string(),
            version: extract_aruba_version(banner),
        })
    }

    fn parse_prompt(&self, line: &str) -> Option<PromptInfo> {
        let line = line.trim_end();

        let (privileged, without_priv) = if let Some(rest) = line.strip_suffix('#') {
            (true, rest)
        } else {
            (false, line.strip_suffix('>')?)
        };

        let (hostname, mode) = match without_priv.find('(') {
            Some(paren_start) if without_priv.ends_with(')') => {
                let hostname = &without_priv[..paren_start];
                let inner = &without_priv[paren_start + 1..without_priv.len() - 1];
                (hostname.to_string(), submode_from(inner))
            }
            Some(_) => return None, // unbalanced parens -- not a prompt we recognize
            None => (
                without_priv.to_string(),
                if privileged {
                    PromptMode::Privileged
                } else {
                    PromptMode::User
                },
            ),
        };

        if hostname.is_empty() {
            return None;
        }

        Some(PromptInfo {
            hostname,
            mode,
            // ArubaOS-CX's access model is role-based (administrators/
            // operators), not Cisco's numbered 0-15 privilege levels --
            // there is no equivalent number to report here.
            privilege: None,
        })
    }

    fn parse_output(&self, _line: &str) -> Vec<ParsedEvent> {
        // Not implemented in Phase 2: ArubaOS-CX's console message format
        // hasn't been verified against real hardware -- guessing a
        // Cisco-like shape risks silently misclassifying real output.
        Vec::new()
    }

    fn suggestions(&self, ctx: &PromptInfo) -> Vec<String> {
        crate::plugin::common::cisco_style_suggestions(&ctx.mode)
    }
}

/// `switch(config)#` -> Config, `switch(config-if)#` -> ConfigIf,
/// `switch(config-router)#` -> ConfigRouter, anything else -> Other.
fn submode_from(inner: &str) -> PromptMode {
    if inner == "config" {
        PromptMode::Config
    } else if let Some(rest) = inner.strip_prefix("config-if") {
        PromptMode::ConfigIf(rest.trim_start_matches('-').to_string())
    } else if let Some(rest) = inner.strip_prefix("config-router") {
        PromptMode::ConfigRouter(rest.trim_start_matches('-').to_string())
    } else {
        PromptMode::Other(inner.to_string())
    }
}

/// Handles both `"Version 10.09.0010"` and `"Version : 10.09.0010"` --
/// genuinely uncertain which separator ArubaOS-CX banners use without a
/// real device to check against, so both are accepted rather than
/// picking one and silently failing to match the other.
fn extract_aruba_version(banner: &str) -> Option<String> {
    let idx = banner.find("Version")?;
    let after = banner[idx + "Version".len()..].trim_start_matches([':', ' ']);
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

    const BANNER_SPACE_COLON: &str = "Aruba Operating System (ArubaOS-CX)\nVersion : 10.09.0010";
    const BANNER_PLAIN: &str = "ArubaOS-CX Version 10.09.0010";

    #[test]
    fn detects_with_space_colon_version_separator() {
        let result = ArubaCxPlugin.detect(BANNER_SPACE_COLON).unwrap();
        assert_eq!(result.vendor, "Aruba");
        assert_eq!(result.platform, "ArubaOS-CX");
        assert_eq!(result.version.as_deref(), Some("10.09.0010"));
    }

    #[test]
    fn detects_with_plain_version_separator() {
        let result = ArubaCxPlugin.detect(BANNER_PLAIN).unwrap();
        assert_eq!(result.version.as_deref(), Some("10.09.0010"));
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(
            ArubaCxPlugin
                .detect("Cisco IOS Software, Version 15.2(2)E7")
                .is_none()
        );
    }

    #[test]
    fn provision_aruba_switch_banner_is_not_detected_as_aruba_cx() {
        // ArubaOS-Switch (ProVision) is a different, VLAN-centric CLI
        // family this plugin does not handle -- must not be conflated
        // with ArubaOS-CX.
        assert!(
            ArubaCxPlugin
                .detect("Aruba Operating System (ArubaOS-Switch)\nVersion 16.10.0")
                .is_none()
        );
    }

    #[test]
    fn prompt_user_mode() {
        let info = ArubaCxPlugin.parse_prompt("switch>").unwrap();
        assert_eq!(info.hostname, "switch");
        assert_eq!(info.mode, PromptMode::User);
        assert_eq!(info.privilege, None);
    }

    #[test]
    fn prompt_privileged_mode() {
        let info = ArubaCxPlugin.parse_prompt("switch#").unwrap();
        assert_eq!(info.mode, PromptMode::Privileged);
    }

    #[test]
    fn prompt_global_config_mode() {
        let info = ArubaCxPlugin.parse_prompt("switch(config)#").unwrap();
        assert_eq!(info.mode, PromptMode::Config);
    }

    #[test]
    fn prompt_config_if_mode() {
        let info = ArubaCxPlugin.parse_prompt("switch(config-if)#").unwrap();
        assert_eq!(info.mode, PromptMode::ConfigIf(String::new()));
    }

    #[test]
    fn prompt_config_router_mode() {
        let info = ArubaCxPlugin
            .parse_prompt("switch(config-router)#")
            .unwrap();
        assert_eq!(info.mode, PromptMode::ConfigRouter(String::new()));
    }

    #[test]
    fn non_prompt_line_is_not_parsed_as_a_prompt() {
        assert!(
            ArubaCxPlugin
                .parse_prompt("1/1/1 up, line protocol is up")
                .is_none()
        );
    }

    #[test]
    fn suggestions_are_non_empty_and_mode_dependent() {
        let privileged = PromptInfo {
            hostname: "switch".to_string(),
            mode: PromptMode::Privileged,
            privilege: None,
        };
        let config = PromptInfo {
            mode: PromptMode::Config,
            ..privileged.clone()
        };
        assert!(!ArubaCxPlugin.suggestions(&privileged).is_empty());
        assert_ne!(
            ArubaCxPlugin.suggestions(&privileged),
            ArubaCxPlugin.suggestions(&config)
        );
    }
}
