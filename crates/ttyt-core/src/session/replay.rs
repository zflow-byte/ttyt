use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::error::CoreError;
use crate::events::{EventBus, SessionEvent};

/// Reads a saved session log (Task 1.6's `SessionRecorder` format, one
/// assembled/redacted line per file line -- including the
/// `--- connection state: ... ---` marker lines the recorder writes
/// verbatim) and republishes each line as `SessionEvent::RawLine` on `bus`,
/// exactly as if it were arriving live from a device. This is deliberately
/// the *only* thing this module does: everything downstream (vendor
/// detection, prompt/output parsing, scrollback rendering) is the same
/// pipeline a live `ConnectionHandle` feeds, reused as-is rather than
/// duplicated for playback.
///
/// The connection-state marker lines are replayed as plain text, not
/// re-parsed back into `SessionEvent::ConnectionStateChanged`: the log
/// format has no structured representation of them (they're a
/// human-readable `Debug` dump, see `SessionRecorder::run`'s doc comment),
/// so treating them as just more scrollback content is simpler and avoids
/// guessing a parse that was never designed to round-trip.
///
/// Any line the recorder redacted was already replaced with
/// `[REDACTED]` before it reached disk -- replay has no way to recover the
/// original text, and isn't expected to.
///
/// `record_line` writes bare text with no per-line timestamp (see
/// `SessionRecorder::record_line`), so there is no original pacing to
/// reproduce. "Adjustable speed" here means a fixed lines-per-second rate,
/// not fidelity to how the original session was actually timed.
pub async fn run(path: &Path, lines_per_second: f64, bus: Arc<EventBus>) -> Result<(), CoreError> {
    if lines_per_second.is_nan() || lines_per_second <= 0.0 {
        return Err(CoreError::Config(format!(
            "replay speed must be a positive number of lines per second, got {lines_per_second}"
        )));
    }
    let delay = Duration::from_secs_f64(1.0 / lines_per_second);

    let file = tokio::fs::File::open(path).await?;
    let mut lines = tokio::io::BufReader::new(file).lines();
    while let Some(line) = lines.next_line().await? {
        bus.publish(SessionEvent::RawLine(line));
        tokio::time::sleep(delay).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    async fn write_temp_log(contents: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ttyt-replay-test-{}-{n}.log", std::process::id()));
        tokio::fs::write(&path, contents).await.unwrap();
        path
    }

    #[tokio::test]
    async fn replays_each_line_as_a_raw_line_event_in_order() {
        let path = write_temp_log("Switch>\nSwitch#\nshow version\n").await;
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        // Fast enough to keep the test quick without asserting on timing.
        run(&path, 1000.0, Arc::clone(&bus)).await.unwrap();

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("Switch>".to_string())
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("Switch#".to_string())
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("show version".to_string())
        );
        assert!(sub.try_recv().is_err());

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn connection_state_marker_lines_replay_as_plain_text() {
        let path = write_temp_log("--- connection state: Connected ---\nSwitch>\n").await;
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        run(&path, 1000.0, Arc::clone(&bus)).await.unwrap();

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("--- connection state: Connected ---".to_string())
        );

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn empty_log_publishes_nothing_and_returns_ok() {
        let path = write_temp_log("").await;
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        run(&path, 1000.0, Arc::clone(&bus)).await.unwrap();

        assert!(sub.try_recv().is_err());

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn missing_file_returns_io_error() {
        let bus = Arc::new(EventBus::new(16));
        let missing = std::env::temp_dir().join("ttyt-replay-test-does-not-exist.log");

        let result = run(&missing, 10.0, bus).await;

        assert!(matches!(result, Err(CoreError::Io(_))));
    }

    #[tokio::test]
    async fn non_positive_speed_is_a_config_error_not_a_panic() {
        let path = write_temp_log("Switch>\n").await;
        let bus = Arc::new(EventBus::new(16));

        assert!(matches!(
            run(&path, 0.0, Arc::clone(&bus)).await,
            Err(CoreError::Config(_))
        ));
        assert!(matches!(
            run(&path, -5.0, Arc::clone(&bus)).await,
            Err(CoreError::Config(_))
        ));
        assert!(matches!(
            run(&path, f64::NAN, bus).await,
            Err(CoreError::Config(_))
        ));

        let _ = tokio::fs::remove_file(&path).await;
    }
}
