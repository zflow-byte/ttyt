//! Ties [`crate::plugin::PluginRegistry`] to a live session's event
//! stream: scans the first lines after connect for a vendor match
//! (Task 2.5), then dispatches each subsequent line through the detected
//! plugin's `parse_prompt`/`parse_output` (Task 2.6).
//!
//! Not a `Session` struct (see the design doc / changes.log for why):
//! this just publishes derived events back onto the same bus, and
//! `ui::App` accumulates the ones it cares about.

use std::sync::Arc;

use crate::events::{EventBus, SessionEvent};
use crate::model::{ParsedEvent, VendorDetectionStatus};
use crate::plugin::PluginRegistry;

/// How many lines after connect to scan for a vendor match before giving
/// up and reporting `Unknown`. Generous enough to cover a full Cisco-style
/// banner (copyright text, ASCII art, etc.) without scanning forever.
const BANNER_WINDOW: usize = 40;

enum DetectionState {
    Pending(Vec<String>),
    /// Holds the detected plugin's `id()` rather than a borrowed
    /// `&dyn VendorPlugin`, so this state doesn't need a lifetime tied to
    /// the registry -- looked back up via `PluginRegistry::get` each time.
    Detected(String),
    Unknown,
}

struct Detector {
    registry: PluginRegistry,
    state: DetectionState,
    /// Most recently seen prompt hostname, used to detect hostname
    /// changes across prompt observations. This is inherently stateful
    /// (comparing two observations over time), which is why
    /// `HostnameChanged` is computed here rather than by any single
    /// `VendorPlugin::parse_output` call operating on one line at a time.
    last_hostname: Option<String>,
}

impl Detector {
    fn new(registry: PluginRegistry) -> Self {
        Detector {
            registry,
            state: DetectionState::Pending(Vec::new()),
            last_hostname: None,
        }
    }

    fn handle_raw_line(&mut self, line: &str, bus: &EventBus) {
        let state = std::mem::replace(&mut self.state, DetectionState::Unknown);
        self.state = match state {
            DetectionState::Pending(mut banner_lines) => {
                banner_lines.push(line.to_string());
                let joined = banner_lines.join("\n");
                if let Some((plugin, result)) = self.registry.detect(&joined) {
                    let id = plugin.id().to_string();
                    bus.publish(SessionEvent::VendorDetection(
                        VendorDetectionStatus::Detected(result),
                    ));
                    DetectionState::Detected(id)
                } else if banner_lines.len() >= BANNER_WINDOW {
                    bus.publish(SessionEvent::VendorDetection(
                        VendorDetectionStatus::Unknown,
                    ));
                    DetectionState::Unknown
                } else {
                    DetectionState::Pending(banner_lines)
                }
            }
            DetectionState::Detected(plugin_id) => {
                if let Some(plugin) = self.registry.get(&plugin_id) {
                    if let Some(prompt) = plugin.parse_prompt(line) {
                        let hostname_changed = self
                            .last_hostname
                            .as_deref()
                            .is_some_and(|prev| prev != prompt.hostname);
                        if hostname_changed {
                            bus.publish(SessionEvent::Parsed(ParsedEvent::HostnameChanged(
                                prompt.hostname.clone(),
                            )));
                        }
                        self.last_hostname = Some(prompt.hostname.clone());
                        let suggestions = plugin.suggestions(&prompt);
                        bus.publish(SessionEvent::PromptChanged(prompt));
                        bus.publish(SessionEvent::Suggestions(suggestions));
                    }
                    for event in plugin.parse_output(line) {
                        bus.publish(SessionEvent::Parsed(event));
                    }
                }
                DetectionState::Detected(plugin_id)
            }
            DetectionState::Unknown => DetectionState::Unknown,
        };
    }
}

/// Drives detection + per-line dispatch from a session's event bus until
/// the sender side is dropped or the receiver falls too far behind.
pub async fn run(
    registry: PluginRegistry,
    bus: Arc<EventBus>,
    mut events: tokio::sync::broadcast::Receiver<SessionEvent>,
) {
    let mut detector = Detector::new(registry);
    loop {
        match events.recv().await {
            Ok(SessionEvent::RawLine(line)) => detector.handle_raw_line(&line, &bus),
            Ok(SessionEvent::ConnectionStateChanged(_)) => {
                // Not relevant to vendor/prompt/output detection.
            }
            Ok(SessionEvent::VendorDetection(_))
            | Ok(SessionEvent::PromptChanged(_))
            | Ok(SessionEvent::Parsed(_))
            | Ok(SessionEvent::Suggestions(_)) => {
                // The detector's own output, echoed back on the same
                // bus it publishes to -- not re-processed as input.
            }
            Ok(SessionEvent::PaginationPrompt(_)) => {
                // A single-keystroke prompt, not a line to classify --
                // the UI handles it directly (scrollback + passthrough
                // mode), and there's nothing here for prompt/vendor
                // detection to do with it.
            }
            Ok(SessionEvent::PartialLine(_)) => {
                // Deliberately not classified: only the eventual complete
                // `RawLine` is. Running `parse_prompt`/`parse_output`
                // against a half-arrived line risks a premature match
                // that then has to be corrected once the rest of the
                // line arrives -- flicker for no benefit, since the
                // complete classification is always seconds away at
                // most.
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "detector lagged, some lines were not classified");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::model::PromptMode;

    fn detected_result(bus: &EventBus) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        bus.subscribe()
    }

    #[tokio::test]
    async fn cisco_banner_produces_detected_vendor_detection_event() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);

        let event = sub.recv().await.unwrap();
        match event {
            SessionEvent::VendorDetection(VendorDetectionStatus::Detected(result)) => {
                assert_eq!(result.vendor, "Cisco");
            }
            other => panic!("expected VendorDetection::Detected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unmatched_lines_report_unknown_once_window_is_exhausted() {
        let bus = Arc::new(EventBus::new(BANNER_WINDOW + 8));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        for i in 0..BANNER_WINDOW {
            detector.handle_raw_line(&format!("unrecognized banner line {i}"), &bus);
        }

        let event = sub.recv().await.unwrap();
        assert_eq!(
            event,
            SessionEvent::VendorDetection(VendorDetectionStatus::Unknown)
        );
    }

    #[tokio::test]
    async fn one_more_unmatched_line_past_the_window_does_not_republish_unknown() {
        let bus = Arc::new(EventBus::new(BANNER_WINDOW + 8));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        for i in 0..(BANNER_WINDOW + 1) {
            detector.handle_raw_line(&format!("unrecognized banner line {i}"), &bus);
        }

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::VendorDetection(VendorDetectionStatus::Unknown)
        );
        // No second event queued -- Unknown is terminal, not re-published
        // on every subsequent line.
        assert!(sub.try_recv().is_err());
    }

    #[tokio::test]
    async fn after_detection_prompt_lines_produce_prompt_changed_events() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::VendorDetection(VendorDetectionStatus::Detected(_))
        ));

        detector.handle_raw_line("Switch>", &bus);
        let event = sub.recv().await.unwrap();
        match event {
            SessionEvent::PromptChanged(prompt) => {
                assert_eq!(prompt.hostname, "Switch");
                assert_eq!(prompt.mode, PromptMode::User);
            }
            other => panic!("expected PromptChanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prompt_changed_is_followed_by_suggestions_for_the_same_mode() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        sub.recv().await.unwrap(); // VendorDetection

        detector.handle_raw_line("Switch>", &bus);
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::PromptChanged(_)
        ));
        match sub.recv().await.unwrap() {
            SessionEvent::Suggestions(suggestions) => {
                assert!(!suggestions.is_empty());
            }
            other => panic!("expected Suggestions right after PromptChanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hostname_change_across_prompts_publishes_hostname_changed() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        sub.recv().await.unwrap(); // VendorDetection

        detector.handle_raw_line("Switch>", &bus);
        sub.recv().await.unwrap(); // PromptChanged (Switch)
        sub.recv().await.unwrap(); // Suggestions

        detector.handle_raw_line("core-sw-01>", &bus);
        let event = sub.recv().await.unwrap();
        assert_eq!(
            event,
            SessionEvent::Parsed(ParsedEvent::HostnameChanged("core-sw-01".to_string()))
        );
        // Followed by the PromptChanged for the new prompt itself.
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::PromptChanged(_)
        ));
    }

    #[tokio::test]
    async fn same_hostname_again_does_not_publish_hostname_changed() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        sub.recv().await.unwrap(); // VendorDetection

        detector.handle_raw_line("Switch>", &bus);
        sub.recv().await.unwrap(); // PromptChanged
        sub.recv().await.unwrap(); // Suggestions

        detector.handle_raw_line("Switch#", &bus);
        // Same hostname, different mode -- PromptChanged again, but no
        // HostnameChanged in between.
        let event = sub.recv().await.unwrap();
        assert!(matches!(event, SessionEvent::PromptChanged(_)));
    }

    #[tokio::test]
    async fn syslog_error_line_produces_parsed_error_event() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        sub.recv().await.unwrap(); // VendorDetection

        detector.handle_raw_line("%SYS-3-CPUHOG: Task ran for too long", &bus);
        let event = sub.recv().await.unwrap();
        assert_eq!(
            event,
            SessionEvent::Parsed(ParsedEvent::Error(
                "%SYS-3-CPUHOG: Task ran for too long".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn non_prompt_non_syslog_line_after_detection_publishes_nothing() {
        let bus = Arc::new(EventBus::new(64));
        let mut sub = detected_result(&bus);
        let mut detector = Detector::new(PluginRegistry::with_default_plugins());

        detector.handle_raw_line("Cisco IOS Software, Version 15.2(2)E7", &bus);
        sub.recv().await.unwrap(); // VendorDetection

        detector.handle_raw_line("GigabitEthernet0/1 is up, line protocol is up", &bus);
        assert!(sub.try_recv().is_err());
    }
}
