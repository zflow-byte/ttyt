mod line_assembler;

pub use line_assembler::LineAssembler;

/// Events published on a session's event bus.
///
/// Phase 1 only needs `RawLine` — the recorder and the TUI console pane
/// both consume assembled device output lines. `ConnectionStateChanged`,
/// `VendorDetected`, and `Parsed`-style variants are added by the
/// connection manager (Task 1.4) and the plugin/parser work (Task 1.7,
/// Phase 2) as those concrete types land, rather than being stubbed out
/// ahead of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// One full line of device output, already assembled from the raw byte
    /// stream by a [`LineAssembler`].
    RawLine(String),
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
