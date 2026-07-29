use crate::model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode};
use crate::plugin::VendorPlugin;

/// Cisco IOS / IOS XE / NX-OS.
pub struct CiscoPlugin;

impl VendorPlugin for CiscoPlugin {
    fn id(&self) -> &'static str {
        "cisco-ios"
    }

    fn detect(&self, banner: &str) -> Option<DetectionResult> {
        // IOS XE and NX-OS banners are checked before the plain "Cisco IOS
        // Software" substring so they aren't misclassified as classic IOS.
        let platform = if banner.contains("Cisco IOS XE Software") {
            "IOS XE"
        } else if banner.contains("NX-OS") || banner.contains("Nexus Operating System") {
            "NX-OS"
        } else if banner.contains("Cisco IOS Software") {
            "IOS"
        } else {
            return None;
        };

        Some(DetectionResult {
            vendor: "Cisco".to_string(),
            platform: platform.to_string(),
            version: extract_version(banner),
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
            // Inferred from prompt convention -- see PromptInfo::privilege doc.
            privilege: Some(if privileged { 15 } else { 1 }),
        })
    }

    fn parse_output(&self, line: &str) -> Vec<ParsedEvent> {
        let mut events = Vec::new();

        if let Some(severity) = cisco_syslog_severity(line) {
            if severity <= 3 {
                events.push(ParsedEvent::Error(line.to_string()));
            } else if severity == 4 {
                events.push(ParsedEvent::Warning(line.to_string()));
            }
        }

        if let Some((interface, up)) = cisco_link_status(line) {
            events.push(ParsedEvent::LinkStatus { interface, up });
        }

        events
    }
}

/// `Switch(config)#` -> Config, `Switch(config-if)#` -> ConfigIf(""),
/// `Switch(config-router)#` -> ConfigRouter(""), anything else -> Other.
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

/// Extracts the version token from a banner containing `Version X.Y.Z`.
/// Plain string search rather than regex -- the format is fixed and this
/// keeps the built-in pattern infallible (no `Regex::new` to `unwrap`).
fn extract_version(banner: &str) -> Option<String> {
    let idx = banner.find("Version ")?;
    let after = &banner[idx + "Version ".len()..];
    let end = after.find([',', ' ', '\r', '\n']).unwrap_or(after.len());
    let version = &after[..end];
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Cisco syslog lines look like `%FACILITY-SEVERITY-MNEMONIC: message`
/// (e.g. `%LINK-3-UPDOWN: ...`). Returns the severity digit (0-7, lower is
/// more severe) if the line matches that shape.
fn cisco_syslog_severity(line: &str) -> Option<u8> {
    let start = line.find('%')?;
    let rest = &line[start + 1..];
    let mut parts = rest.splitn(3, '-');
    let _facility = parts.next()?;
    let severity_str = parts.next()?;
    severity_str.parse::<u8>().ok()
}

/// Matches Cisco's `%LINK-*-UPDOWN`/`%LINEPROTO-*-UPDOWN` message shape:
/// `...Interface <name>, changed state to up|down`.
fn cisco_link_status(line: &str) -> Option<(String, bool)> {
    let idx = line.find("Interface ")?;
    let after = &line[idx + "Interface ".len()..];
    let comma = after.find(',')?;
    let interface = after[..comma].trim().to_string();

    let trimmed = line.trim_end();
    if trimmed.ends_with("changed state to up") {
        Some((interface, true))
    } else if trimmed.ends_with("changed state to down") {
        Some((interface, false))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const IOS_BANNER: &str = "Cisco IOS Software, C3560 Software (C3560-IPSERVICESK9-M), Version 15.2(2)E7, RELEASE SOFTWARE (fc1)";
    const IOS_XE_BANNER: &str = "Cisco IOS XE Software, Version 17.03.04a";
    const NX_OS_BANNER: &str = "Cisco Nexus Operating System (NX-OS) Software, Version 9.3(8)";

    #[test]
    fn detects_classic_ios() {
        let result = CiscoPlugin.detect(IOS_BANNER).unwrap();
        assert_eq!(result.vendor, "Cisco");
        assert_eq!(result.platform, "IOS");
        assert_eq!(result.version.as_deref(), Some("15.2(2)E7"));
    }

    #[test]
    fn detects_ios_xe_not_classic_ios() {
        let result = CiscoPlugin.detect(IOS_XE_BANNER).unwrap();
        assert_eq!(result.platform, "IOS XE");
        assert_eq!(result.version.as_deref(), Some("17.03.04a"));
    }

    #[test]
    fn detects_nx_os() {
        let result = CiscoPlugin.detect(NX_OS_BANNER).unwrap();
        assert_eq!(result.platform, "NX-OS");
        assert_eq!(result.version.as_deref(), Some("9.3(8)"));
    }

    #[test]
    fn unrelated_banner_is_not_detected() {
        assert!(CiscoPlugin.detect("ArubaOS-CX, Version 10.09").is_none());
    }

    #[test]
    fn prompt_user_mode() {
        let info = CiscoPlugin.parse_prompt("Switch>").unwrap();
        assert_eq!(info.hostname, "Switch");
        assert_eq!(info.mode, PromptMode::User);
        assert_eq!(info.privilege, Some(1));
    }

    #[test]
    fn prompt_privileged_mode() {
        let info = CiscoPlugin.parse_prompt("Switch#").unwrap();
        assert_eq!(info.hostname, "Switch");
        assert_eq!(info.mode, PromptMode::Privileged);
        assert_eq!(info.privilege, Some(15));
    }

    #[test]
    fn prompt_global_config_mode() {
        let info = CiscoPlugin.parse_prompt("Switch(config)#").unwrap();
        assert_eq!(info.hostname, "Switch");
        assert_eq!(info.mode, PromptMode::Config);
    }

    #[test]
    fn prompt_config_if_mode() {
        let info = CiscoPlugin.parse_prompt("Switch(config-if)#").unwrap();
        assert_eq!(info.hostname, "Switch");
        assert_eq!(info.mode, PromptMode::ConfigIf(String::new()));
    }

    #[test]
    fn prompt_config_router_mode() {
        let info = CiscoPlugin.parse_prompt("Switch(config-router)#").unwrap();
        assert_eq!(info.hostname, "Switch");
        assert_eq!(info.mode, PromptMode::ConfigRouter(String::new()));
    }

    #[test]
    fn prompt_with_hyphenated_hostname_is_parsed_correctly() {
        let info = CiscoPlugin.parse_prompt("core-sw-01#").unwrap();
        assert_eq!(info.hostname, "core-sw-01");
    }

    #[test]
    fn non_prompt_line_is_not_parsed_as_a_prompt() {
        assert!(
            CiscoPlugin
                .parse_prompt("GigabitEthernet0/1 is up, line protocol is up")
                .is_none()
        );
    }

    #[test]
    fn syslog_error_severity_is_classified_as_error() {
        let events = CiscoPlugin.parse_output("%SYS-3-CPUHOG: Task ran for too long");
        assert_eq!(
            events,
            vec![ParsedEvent::Error(
                "%SYS-3-CPUHOG: Task ran for too long".to_string()
            )]
        );
    }

    #[test]
    fn syslog_notice_severity_is_classified_as_warning() {
        let events = CiscoPlugin.parse_output("%SYS-4-CONFIG_I: input queue nearly full");
        assert_eq!(
            events,
            vec![ParsedEvent::Warning(
                "%SYS-4-CONFIG_I: input queue nearly full".to_string()
            )]
        );
    }

    #[test]
    fn syslog_informational_severity_produces_no_event() {
        let events = CiscoPlugin.parse_output("%SYS-5-CONFIG_I: Configured from console");
        assert_eq!(events, Vec::new());
    }

    #[test]
    fn link_down_event_is_classified() {
        let events = CiscoPlugin
            .parse_output("%LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down");
        assert!(events.contains(&ParsedEvent::LinkStatus {
            interface: "GigabitEthernet0/1".to_string(),
            up: false,
        }));
    }

    #[test]
    fn link_up_event_is_classified() {
        let events = CiscoPlugin.parse_output(
            "%LINEPROTO-5-UPDOWN: Line protocol on Interface GigabitEthernet0/2, changed state to up",
        );
        assert!(events.contains(&ParsedEvent::LinkStatus {
            interface: "GigabitEthernet0/2".to_string(),
            up: true,
        }));
    }

    #[test]
    fn ordinary_show_command_output_produces_no_events() {
        let events = CiscoPlugin.parse_output("GigabitEthernet0/1   up   up   auto   auto");
        assert_eq!(events, Vec::new());
    }

    #[test]
    fn suggestions_is_empty_in_phase_1() {
        let ctx = PromptInfo {
            hostname: "Switch".to_string(),
            mode: PromptMode::Privileged,
            privilege: Some(15),
        };
        assert_eq!(CiscoPlugin.suggestions(&ctx), Vec::<String>::new());
    }
}
