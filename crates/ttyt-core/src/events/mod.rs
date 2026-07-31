mod line_assembler;

pub use line_assembler::{AssembledOutput, LineAssembler};

use crate::model::{ParsedEvent, PromptInfo, VendorDetectionStatus};

/// Identifies one connection/tab among possibly several concurrent
/// sessions (Phase 3 tabs). Assigned once when a session is created from
/// its position in the CLI's `--port` list and never reused -- stable
/// enough to key the UI's `Vec<Session>` and the CLI's parallel
/// `Vec<ConnectionHandle>`/`Vec<CommandHistory>` by the same value without
/// the two ever silently desynchronizing (sessions are never removed at
/// runtime in Phase 3 -- see the design doc's tabs note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(usize);

impl SessionId {
    pub fn new(index: usize) -> SessionId {
        SessionId(index)
    }

    pub fn index(self) -> usize {
        self.0
    }
}

/// Lifecycle state of a device connection, published via
/// `SessionEvent::ConnectionStateChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
    Reconnecting,
}

/// Events published on a session's event bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// One full line of device output, already assembled from the raw byte
    /// stream by a [`LineAssembler`].
    RawLine(String),
    /// The connection's lifecycle state changed (connected, dropped,
    /// reconnecting, ...).
    ConnectionStateChanged(ConnectionState),
    /// The detector (Phase 2) finished its banner-window scan: either a
    /// vendor was identified, or the window was exhausted with no match.
    VendorDetection(VendorDetectionStatus),
    /// A line matched the detected vendor's prompt shape.
    PromptChanged(PromptInfo),
    /// A line was classified by the detected vendor's `parse_output`.
    Parsed(ParsedEvent),
    /// Published alongside `PromptChanged`, from the same
    /// `VendorPlugin::suggestions` call: TAB-autocomplete candidates for
    /// the prompt context that just became current (Task 3.3). Computed
    /// eagerly by the detector rather than lazily by the UI so
    /// `ttyt-ui` never needs a `PluginRegistry` -- suggestions are vendor
    /// logic, and the design doc keeps all vendor logic in
    /// `ttyt-core`, reached by the UI only through bus events.
    Suggestions(Vec<String>),
    /// The device is blocked on a "press a key to continue" pagination
    /// prompt (`--More--` and vendor equivalents), recognized from an
    /// unterminated buffer tail by a [`LineAssembler`]. Carries the raw
    /// prompt text so it can still be shown in the console like any other
    /// output line.
    PaginationPrompt(String),
    /// A live preview of a line still being received (no trailing newline
    /// yet), so the console can render output as it streams in rather
    /// than only after each line completes. Display-only: never recorded
    /// to the session log/history the way `RawLine` is -- see
    /// [`LineAssembler::feed`]'s doc comment for the redaction reasoning
    /// -- and never fed to the detector, which only classifies complete
    /// lines.
    PartialLine(String),
}

/// A single-producer, multi-consumer fan-out for `SessionEvent`s. Every
/// subscriber (recorder, parser, UI) gets its own receiver and sees every
/// event published after it subscribed.
pub struct EventBus {
    sender: tokio::sync::broadcast::Sender<SessionEvent>,
}

impl EventBus {
    /// `capacity` bounds how many not-yet-received events the channel
    /// buffers per subscriber before the slowest subscriber starts missing
    /// events (`RecvError::Lagged`, not a panic).
    pub fn new(capacity: usize) -> Self {
        let (sender, _receiver) = tokio::sync::broadcast::channel(capacity);
        EventBus { sender }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.sender.subscribe()
    }

    /// Publish an event to all current subscribers. Returns the number of
    /// receivers it was delivered to; 0 is expected (not an error) before
    /// anything has subscribed yet.
    pub fn publish(&self, event: SessionEvent) -> usize {
        self.sender.send(event).unwrap_or(0)
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn session_id_round_trips_its_index_and_compares_by_value() {
        let a = SessionId::new(0);
        let b = SessionId::new(1);
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);
        assert_ne!(a, b);
        assert_eq!(a, SessionId::new(0));
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_all_published_events() {
        let bus = EventBus::new(16);
        let mut sub_a = bus.subscribe();
        let mut sub_b = bus.subscribe();

        for i in 0..5 {
            bus.publish(SessionEvent::RawLine(format!("line {i}")));
        }

        for i in 0..5 {
            assert_eq!(
                sub_a.recv().await.unwrap(),
                SessionEvent::RawLine(format!("line {i}"))
            );
        }
        for i in 0..5 {
            assert_eq!(
                sub_b.recv().await.unwrap(),
                SessionEvent::RawLine(format!("line {i}"))
            );
        }
    }

    #[test]
    fn publish_with_no_subscribers_returns_zero_not_an_error() {
        let bus = EventBus::new(16);
        assert_eq!(bus.publish(SessionEvent::RawLine("hello".to_string())), 0);
    }
}
