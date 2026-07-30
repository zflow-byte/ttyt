//! Best-effort, vendor-agnostic parser over a device's running-config
//! text (Task 3.6, plan item "Config summary (best-effort skeleton)").
//!
//! Explicitly **not** a completeness requirement for Phase 3, per the plan
//! doc's own framing ("per spec item #10, explicitly scoped as a
//! future/experimental parser... implement a minimal version and flag
//! remaining work rather than stub it out silently"). This module is a
//! standalone parser, not wired into the live session event pipeline or
//! any UI widget: the design doc's 4-pane TUI layout has no free pane for
//! it (the left panel is already committed to sessions/tabs), and adding
//! a fifth pane wasn't part of this task. It exists so a real
//! config-summary feature has a tested starting point in a future phase
//! instead of nothing.
//!
//! Recognizes only a generic Cisco-shaped config grammar (`hostname X`,
//! `interface X`, `vlan N`) that Cisco, Dell OS10, and Aruba CX all share
//! closely enough for a first pass. Comware (`[hostname]` view syntax) and
//! JunOS (`set`/braces syntax) are structurally different and are
//! known-unsummarized here -- a real per-vendor implementation is exactly
//! the future work this stub flags, not silently omitted.

/// Extracted from arbitrary config text by [`summarize`]. Every field is
/// best-effort: a config this parser doesn't recognize any lines from
/// simply produces an empty summary, not an error -- there is no such
/// thing as an invalid input to this parser, only an unrecognized one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSummary {
    pub hostname: Option<String>,
    pub interfaces: Vec<String>,
    pub vlans: Vec<u32>,
    /// Total lines in the input, recognized or not -- a rough size
    /// indicator for "how much of this config did we actually look at."
    pub line_count: usize,
}

/// Scans `config_text` line by line for a small set of generic Cisco-shaped
/// directives. Later occurrences of `hostname` overwrite earlier ones
/// (matching how a real config would only take its last `hostname` line
/// effect); `interface`/`vlan` lines accumulate in the order they appear,
/// duplicates included -- deduplication isn't attempted since a repeated
/// `interface X` block (e.g. the same interface reconfigured twice in one
/// paste) is itself something a future, less minimal version would want
/// to be able to show.
pub fn summarize(config_text: &str) -> ConfigSummary {
    let mut summary = ConfigSummary {
        line_count: config_text.lines().count(),
        ..Default::default()
    };

    for line in config_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("hostname ") {
            let name = rest.trim();
            if !name.is_empty() {
                summary.hostname = Some(name.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("interface ") {
            let name = rest.trim();
            if !name.is_empty() {
                summary.interfaces.push(name.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("vlan ")
            && let Ok(id) = rest.trim().parse::<u32>()
        {
            summary.vlans.push(id);
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn extracts_hostname() {
        let summary = summarize("hostname core-sw-01\ninterface GigabitEthernet0/1\n");
        assert_eq!(summary.hostname.as_deref(), Some("core-sw-01"));
    }

    #[test]
    fn last_hostname_line_wins() {
        let summary = summarize("hostname first\nhostname second\n");
        assert_eq!(summary.hostname.as_deref(), Some("second"));
    }

    #[test]
    fn collects_interfaces_in_order() {
        let summary = summarize(
            "interface GigabitEthernet0/1\n description uplink\ninterface GigabitEthernet0/2\n",
        );
        assert_eq!(
            summary.interfaces,
            vec![
                "GigabitEthernet0/1".to_string(),
                "GigabitEthernet0/2".to_string()
            ]
        );
    }

    #[test]
    fn collects_valid_vlan_ids_and_skips_malformed_ones() {
        let summary = summarize("vlan 10\nvlan 20\nvlan not-a-number\nvlan 30\n");
        assert_eq!(summary.vlans, vec![10, 20, 30]);
    }

    #[test]
    fn line_count_reflects_the_whole_input_not_just_recognized_lines() {
        let summary = summarize("hostname sw1\n! a comment\ninterface Gi0/1\n no shutdown\n");
        assert_eq!(summary.line_count, 4);
    }

    #[test]
    fn empty_input_produces_an_empty_summary() {
        let summary = summarize("");
        assert_eq!(summary, ConfigSummary::default());
    }

    #[test]
    fn unrecognized_config_syntax_produces_an_empty_summary_not_an_error() {
        // Comware/JunOS-shaped config -- this parser only knows the
        // generic Cisco-shaped grammar, so this is the documented
        // known-unsummarized case, not a bug.
        let summary = summarize("[core-sw-01] interface GigabitEthernet1/0/1\n quit\n");
        assert_eq!(summary.hostname, None);
        assert!(summary.interfaces.is_empty());
        assert_eq!(summary.line_count, 2);
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        let summary = summarize("  hostname   padded-name  \n");
        assert_eq!(summary.hostname.as_deref(), Some("padded-name"));
    }
}
