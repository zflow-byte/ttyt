use crate::model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode};
use crate::plugin::VendorPlugin;

/// Dell EMC Networking OS10 -- a Cisco-shaped CLI (`>`/`#`, parenthesized
/// submodes), so this plugin mirrors `CiscoPlugin`'s structure closely.
///
/// Banner/prompt fixtures below are reconstructed from general OS10
/// documentation/knowledge, **not verified against real hardware**.
/// Notably uncertain: whether OS10 uses `conf-if-`/`conf-router-`
/// (assumed here) or `config-if-`/`config-router-` like Cisco -- flag and
/// correct against a real device before trusting this in the field.
pub struct DellOs10Plugin;

impl VendorPlugin for DellOs10Plugin {
    fn id(&self) -> &'static str {
        "dell-os10"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        if !banner.contains("OS10") {
            return None;
        }
        Some(DetectionResult {
            vendor: "Dell EMC".to_string(),
            platform: "OS10".to_string(),
            version: extract_os_version(banner),
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
            // Unlike Cisco, OS10's privilege model isn't confidently known
            // here -- not guessed as 1/15 by visual similarity alone.
            privilege: None,
        })
    }

    fn parse_output(&self, _line: &str) -> Vec<ParsedEvent> {
        // Not implemented in Phase 2: OS10 runs on a Linux base and its
        // console message format hasn't been verified against real
        // hardware -- guessing a Cisco-like %FACILITY-SEVERITY-MNEMONIC
        // shape risks silently misclassifying (or never matching) real
        // output.
        Vec::new()
    }

    fn suggestions(&self, ctx: &PromptInfo) -> Vec<String> {
        crate::plugin::common::cisco_style_suggestions(&ctx.mode)
    }
}

/// `OS10(conf)#` -> Config, `OS10(conf-if-eth1/1/1)#` -> ConfigIf,
/// `OS10(conf-router-bgp)#` -> ConfigRouter, anything else -> Other.
fn submode_from(inner: &str) -> PromptMode {
    if inner == "conf" {
        PromptMode::Config
    } else if let Some(rest) = inner.strip_prefix("conf-if-") {
        PromptMode::ConfigIf(rest.to_string())
    } else if let Some(rest) = inner.strip_prefix("conf-router-") {
        PromptMode::ConfigRouter(rest.to_string())
    } else {
        PromptMode::Other(inner.to_string())
    }
}

/// OS10's `show version` banner uses `OS Version: X`, not the `Version X`
/// convention `plugin::common::extract_version_token` looks for.
fn extract_os_version(banner: &str) -> Option<String> {
    let idx = banner.find("OS Version: ")?;
    let after = &banner[idx + "OS Version: ".len()..];
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

    const BANNER: &str =
        "Dell EMC Networking OS10-Enterprise\nOS Version: 10.5.2.4\nBuild Version: 10.5.2.4.317";

    #[test]
    fn detects_os10_banner() {
        let result = DellOs10Plugin.detect(BANNER).unwrap();
        assert_eq!(result.vendor, "Dell EMC");
        assert_eq!(result.platform, "OS10");
        assert_eq!(result.version.as_deref(), Some("10.5.2.4"));
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(
            DellOs10Plugin
                .detect("Cisco IOS Software, Version 15.2(2)E7")
                .is_none()
        );
    }

    #[test]
    fn prompt_user_mode() {
        let info = DellOs10Plugin.parse_prompt("OS10>").unwrap();
        assert_eq!(info.hostname, "OS10");
        assert_eq!(info.mode, PromptMode::User);
        assert_eq!(info.privilege, None);
    }

    #[test]
    fn prompt_privileged_mode() {
        let info = DellOs10Plugin.parse_prompt("OS10#").unwrap();
        assert_eq!(info.hostname, "OS10");
        assert_eq!(info.mode, PromptMode::Privileged);
    }

    #[test]
    fn prompt_global_config_mode() {
        let info = DellOs10Plugin.parse_prompt("OS10(conf)#").unwrap();
        assert_eq!(info.mode, PromptMode::Config);
    }

    #[test]
    fn prompt_interface_config_carries_interface_name() {
        let info = DellOs10Plugin
            .parse_prompt("OS10(conf-if-eth1/1/1)#")
            .unwrap();
        assert_eq!(info.hostname, "OS10");
        assert_eq!(info.mode, PromptMode::ConfigIf("eth1/1/1".to_string()));
    }

    #[test]
    fn prompt_router_config_carries_protocol_name() {
        let info = DellOs10Plugin
            .parse_prompt("OS10(conf-router-bgp)#")
            .unwrap();
        assert_eq!(info.mode, PromptMode::ConfigRouter("bgp".to_string()));
    }

    #[test]
    fn prompt_with_custom_hostname_is_parsed_correctly() {
        let info = DellOs10Plugin.parse_prompt("leaf-switch-01#").unwrap();
        assert_eq!(info.hostname, "leaf-switch-01");
    }

    #[test]
    fn non_prompt_line_is_not_parsed_as_a_prompt() {
        assert!(
            DellOs10Plugin
                .parse_prompt("eth1/1/1 is up, line protocol is up")
                .is_none()
        );
    }

    #[test]
    fn suggestions_are_non_empty_and_mode_dependent() {
        let privileged = PromptInfo {
            hostname: "OS10".to_string(),
            mode: PromptMode::Privileged,
            privilege: None,
        };
        let config_if = PromptInfo {
            mode: PromptMode::ConfigIf("eth1/1/1".to_string()),
            ..privileged.clone()
        };
        assert!(!DellOs10Plugin.suggestions(&privileged).is_empty());
        assert_ne!(
            DellOs10Plugin.suggestions(&privileged),
            DellOs10Plugin.suggestions(&config_if)
        );
    }
}
