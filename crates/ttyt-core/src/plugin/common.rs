//! Helpers shared across `VendorPlugin` implementations. Plain string
//! parsing rather than regex throughout -- these patterns are fixed and
//! this keeps them infallible (no `Regex::new` to handle/`unwrap`).

use crate::model::PromptMode;

/// TAB-autocomplete candidates shared by the three Cisco-shaped CLIs
/// (Cisco, Dell OS10, Aruba CX -- same `>`/`#`/`(config)#` grammar and
/// `PromptMode` mapping). Deliberately a short list of common top-level
/// verbs per mode, not a full command tree: a wrong suggestion here is
/// reviewed by the user before Enter (and still gated by
/// `DangerousCommandGuard` if it matches a dangerous pattern), unlike a
/// wrong `parse_output` classification, which would silently mislabel
/// device state -- so this doesn't need the same hardware-verified rigor
/// the parsing code does. Minor per-vendor syntax differences (e.g. exact
/// save-config wording) are knowingly approximated.
pub(crate) fn cisco_style_suggestions(mode: &PromptMode) -> Vec<String> {
    match mode {
        PromptMode::User => vec![
            "enable".to_string(),
            "show version".to_string(),
            "show running-config".to_string(),
            "exit".to_string(),
        ],
        PromptMode::Privileged => vec![
            "configure terminal".to_string(),
            "show running-config".to_string(),
            "show version".to_string(),
            "show interfaces".to_string(),
            "write memory".to_string(),
            "exit".to_string(),
        ],
        PromptMode::Config => vec![
            "interface ".to_string(),
            "hostname ".to_string(),
            "no ".to_string(),
            "exit".to_string(),
            "end".to_string(),
        ],
        PromptMode::ConfigIf(_) => vec![
            "no shutdown".to_string(),
            "shutdown".to_string(),
            "description ".to_string(),
            "ip address ".to_string(),
            "exit".to_string(),
        ],
        PromptMode::ConfigRouter(_) => {
            vec![
                "network ".to_string(),
                "no ".to_string(),
                "exit".to_string(),
            ]
        }
        PromptMode::Other(_) => vec!["exit".to_string(), "end".to_string()],
    }
}

/// Extracts the token following the first `Version ` in `text` (e.g.
/// `"...Version 15.2(2)E7, ..."` -> `"15.2(2)E7"`). Used by every vendor
/// plugin whose banner follows this common `Version X` convention.
pub(crate) fn extract_version_token(text: &str) -> Option<String> {
    let idx = text.find("Version ")?;
    let after = &text[idx + "Version ".len()..];
    let end = after.find([',', ' ', '\r', '\n']).unwrap_or(after.len());
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

    #[test]
    fn extracts_version_terminated_by_comma() {
        assert_eq!(
            extract_version_token("Cisco IOS Software, Version 15.2(2)E7, RELEASE"),
            Some("15.2(2)E7".to_string())
        );
    }

    #[test]
    fn extracts_version_terminated_by_end_of_string() {
        assert_eq!(
            extract_version_token("Some Software, Version 10.09"),
            Some("10.09".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_version_keyword_present() {
        assert_eq!(extract_version_token("no version keyword here"), None);
    }
}
