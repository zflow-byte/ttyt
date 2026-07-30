use crate::model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode};
use crate::plugin::VendorPlugin;
use crate::plugin::common::extract_version_token;

/// HPE / H3C Comware (structurally the most different of the five target
/// vendors from Cisco's shape: `<hostname>` for user view, `[hostname]`
/// for system view, `[hostname-suffix]` for any sub-view -- not `>`/`#`).
///
/// Banner and prompt fixtures below are reconstructed from general
/// Comware documentation/knowledge, **not verified against real
/// hardware** -- flag accordingly if a real device becomes available
/// (see plan.md Task 2.3's own convention for unverified vendor formats).
pub struct ComwarePlugin;

impl VendorPlugin for ComwarePlugin {
    fn id(&self) -> &'static str {
        "comware"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        if !banner.contains("Comware") {
            return None;
        }
        // OEM badge varies (H3C-branded vs HPE-branded hardware running
        // the same Comware base); report whichever the banner names,
        // rather than guessing one.
        let vendor = if banner.contains("HPE") {
            "HPE"
        } else if banner.contains("H3C") {
            "H3C"
        } else {
            "H3C/HPE"
        };
        Some(DetectionResult {
            vendor: vendor.to_string(),
            platform: "Comware".to_string(),
            version: extract_version_token(banner),
        })
    }

    fn parse_prompt(&self, line: &str) -> Option<PromptInfo> {
        let line = line.trim_end();

        if let Some(inner) = line.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
            if inner.is_empty() {
                return None;
            }
            return Some(PromptInfo {
                hostname: inner.to_string(),
                mode: PromptMode::User,
                // Comware has privilege levels 0-3, but none of them are
                // derivable from the prompt shape the way Cisco's `>`/`#`
                // convention allows -- not queried, not guessed.
                privilege: None,
            });
        }

        let inner = line.strip_prefix('[').and_then(|s| s.strip_suffix(']'))?;
        if inner.is_empty() {
            return None;
        }

        // Comware hostnames can themselves contain a hyphen, so splitting
        // on the first '-' is an inherent ambiguity in this format, not
        // just this implementation -- documented as a known limitation
        // rather than solved, since there is no unambiguous rule without
        // knowing the configured hostname out of band.
        let (hostname, mode) = match inner.split_once('-') {
            None => (inner, PromptMode::Config),
            Some((hostname, suffix)) if !hostname.is_empty() => {
                (hostname, classify_submode(suffix))
            }
            Some(_) => return None,
        };

        Some(PromptInfo {
            hostname: hostname.to_string(),
            mode,
            privilege: None,
        })
    }

    fn parse_output(&self, line: &str) -> Vec<ParsedEvent> {
        // Comware's `%%NNFACILITY/SEVERITY/MNEMONIC: message` structural
        // format is well-documented and stable, so severity classification
        // is implemented. Link-status message wording is NOT implemented
        // here (unlike Cisco's LINK-3-UPDOWN, which is widely documented):
        // encoding a guessed exact phrase risks silently misclassifying
        // real output, which is worse than not classifying it. Revisit
        // with real hardware or vendor documentation in hand.
        let Some(severity) = comware_syslog_severity(line) else {
            return Vec::new();
        };
        match severity {
            0..=3 => vec![ParsedEvent::Error(line.to_string())],
            4 => vec![ParsedEvent::Warning(line.to_string())],
            _ => Vec::new(),
        }
    }

    fn suggestions(&self, ctx: &PromptInfo) -> Vec<String> {
        // Common top-level verbs per view, not a full command tree -- see
        // `plugin::common::cisco_style_suggestions`'s doc comment for why
        // this doesn't need hardware-verified rigor the way parsing does.
        match &ctx.mode {
            PromptMode::User => vec![
                "display current-configuration".to_string(),
                "display version".to_string(),
                "system-view".to_string(),
                "quit".to_string(),
            ],
            PromptMode::Config => vec![
                "interface ".to_string(),
                "sysname ".to_string(),
                "quit".to_string(),
                "save".to_string(),
            ],
            PromptMode::ConfigIf(_) => vec![
                "undo shutdown".to_string(),
                "shutdown".to_string(),
                "description ".to_string(),
                "ip address ".to_string(),
                "quit".to_string(),
            ],
            PromptMode::ConfigRouter(_) => {
                vec!["network ".to_string(), "quit".to_string()]
            }
            PromptMode::Privileged | PromptMode::Other(_) => {
                vec!["quit".to_string()]
            }
        }
    }
}

/// `%%10SHELL/5/SHELL_LOGIN: ...` -> severity `5`. Splitting on `/` finds
/// the severity field regardless of exactly how the leading numeric/
/// facility prefix is formatted.
fn comware_syslog_severity(line: &str) -> Option<u8> {
    let start = line.find("%%")?;
    let rest = &line[start + 2..];
    let mut parts = rest.splitn(3, '/');
    let _facility = parts.next()?;
    let severity_str = parts.next()?;
    severity_str.parse::<u8>().ok()
}

/// Interface-view vs routing-protocol-view vs anything else, by prefix.
/// Not exhaustive -- unrecognized suffixes fall back to `Other`, which is
/// still useful (hostname/some-mode beats no match at all).
fn classify_submode(suffix: &str) -> PromptMode {
    let lower = suffix.to_lowercase();
    const INTERFACE_PREFIXES: &[&str] = &[
        "gigabitethernet",
        "ten-gigabitethernet",
        "hundredgige",
        "fortygige",
        "vlan-interface",
        "loopback",
    ];
    const ROUTER_PREFIXES: &[&str] = &["bgp", "ospf", "rip", "isis"];

    if INTERFACE_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        PromptMode::ConfigIf(suffix.to_string())
    } else if ROUTER_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        PromptMode::ConfigRouter(suffix.to_string())
    } else {
        PromptMode::Other(suffix.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const HPE_BANNER: &str = "HPE Comware Platform Software, Version 7.1.070, Release 6555P29";
    const H3C_BANNER: &str = "H3C Comware Software, Version 7.1.070, Release 6555P29";

    #[test]
    fn detects_hpe_badged_comware() {
        let result = ComwarePlugin.detect(HPE_BANNER).unwrap();
        assert_eq!(result.vendor, "HPE");
        assert_eq!(result.platform, "Comware");
        assert_eq!(result.version.as_deref(), Some("7.1.070"));
    }

    #[test]
    fn detects_h3c_badged_comware() {
        let result = ComwarePlugin.detect(H3C_BANNER).unwrap();
        assert_eq!(result.vendor, "H3C");
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(
            ComwarePlugin
                .detect("Cisco IOS Software, Version 15.2(2)E7")
                .is_none()
        );
    }

    #[test]
    fn prompt_user_view() {
        let info = ComwarePlugin.parse_prompt("<HPE>").unwrap();
        assert_eq!(info.hostname, "HPE");
        assert_eq!(info.mode, PromptMode::User);
        assert_eq!(info.privilege, None);
    }

    #[test]
    fn prompt_system_view() {
        let info = ComwarePlugin.parse_prompt("[HPE]").unwrap();
        assert_eq!(info.hostname, "HPE");
        assert_eq!(info.mode, PromptMode::Config);
    }

    #[test]
    fn prompt_interface_view_carries_interface_name() {
        let info = ComwarePlugin
            .parse_prompt("[HPE-GigabitEthernet1/0/1]")
            .unwrap();
        assert_eq!(info.hostname, "HPE");
        assert_eq!(
            info.mode,
            PromptMode::ConfigIf("GigabitEthernet1/0/1".to_string())
        );
    }

    #[test]
    fn prompt_routing_protocol_view_carries_protocol_name() {
        let info = ComwarePlugin.parse_prompt("[HPE-bgp]").unwrap();
        assert_eq!(info.hostname, "HPE");
        assert_eq!(info.mode, PromptMode::ConfigRouter("bgp".to_string()));
    }

    #[test]
    fn prompt_unrecognized_submode_falls_back_to_other() {
        let info = ComwarePlugin.parse_prompt("[HPE-acl-basic-2000]").unwrap();
        assert_eq!(info.hostname, "HPE");
        assert_eq!(info.mode, PromptMode::Other("acl-basic-2000".to_string()));
    }

    #[test]
    fn non_prompt_line_is_not_parsed_as_a_prompt() {
        assert!(
            ComwarePlugin
                .parse_prompt("GigabitEthernet1/0/1 current state: UP")
                .is_none()
        );
    }

    #[test]
    fn syslog_severity_3_is_classified_as_error() {
        let events = ComwarePlugin.parse_output("%%10DEV/3/CRITICAL: Fan failure detected");
        assert_eq!(
            events,
            vec![ParsedEvent::Error(
                "%%10DEV/3/CRITICAL: Fan failure detected".to_string()
            )]
        );
    }

    #[test]
    fn syslog_severity_4_is_classified_as_warning() {
        let events = ComwarePlugin.parse_output("%%10DEV/4/WARN: Temperature high");
        assert_eq!(
            events,
            vec![ParsedEvent::Warning(
                "%%10DEV/4/WARN: Temperature high".to_string()
            )]
        );
    }

    #[test]
    fn syslog_severity_6_produces_no_event() {
        let events = ComwarePlugin.parse_output("%%10SHELL/6/SHELL_LOGIN: login succeeded");
        assert_eq!(events, Vec::new());
    }

    #[test]
    fn ordinary_output_produces_no_events() {
        assert_eq!(
            ComwarePlugin.parse_output("GigabitEthernet1/0/1 current state: UP"),
            Vec::new()
        );
    }

    #[test]
    fn suggestions_use_comware_verbs_and_depend_on_mode() {
        let user = PromptInfo {
            hostname: "HPE".to_string(),
            mode: PromptMode::User,
            privilege: None,
        };
        let config = PromptInfo {
            mode: PromptMode::Config,
            ..user.clone()
        };
        assert!(
            ComwarePlugin
                .suggestions(&user)
                .contains(&"quit".to_string())
        );
        assert!(
            !ComwarePlugin
                .suggestions(&user)
                .contains(&"exit".to_string())
        );
        assert_ne!(
            ComwarePlugin.suggestions(&user),
            ComwarePlugin.suggestions(&config)
        );
    }
}
