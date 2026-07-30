use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::CoreError;
use crate::events::{AssembledOutput, ConnectionState, EventBus, LineAssembler, SessionEvent};

/// Minimal blocking transport abstraction satisfied by both a real
/// `serialport::SerialPort` and, in tests, an in-memory mock. This is what
/// keeps the reconnect/backoff/line-assembly logic below unit-testable
/// without physical serial hardware attached.
pub trait SerialTransport: Send {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
}

impl SerialTransport for Box<dyn serialport::SerialPort> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self.as_mut(), buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(self.as_mut(), buf)
    }
}

/// Opens a real serial port, ready to hand to [`ConnectionHandle::spawn`].
/// A short read timeout is set so the reader thread can periodically check
/// for a stop request instead of blocking forever with no data.
pub fn open_serial_transport(
    port_name: &str,
    baud: u32,
) -> Result<Box<dyn SerialTransport>, CoreError> {
    let port: Box<dyn serialport::SerialPort> = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(200))
        .open()
        .map_err(CoreError::Serial)?;
    Ok(Box::new(port))
}

/// Consecutive non-timeout read errors before a connection is considered
/// dropped and reconnect kicks in.
const CONSECUTIVE_ERROR_THRESHOLD: u32 = 3;

const RECONNECT_BASE_DELAY: Duration = Duration::from_millis(500);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

/// A live connection to a serial device: a dedicated OS thread blocks on
/// reads (isolating the blocking `serialport` API from the async runtime)
/// and forwards assembled lines and connection-state changes to the event
/// bus; writes are dispatched through `spawn_blocking` against the same
/// shared transport.
pub struct ConnectionHandle {
    transport: Arc<Mutex<Box<dyn SerialTransport>>>,
    stop: Arc<AtomicBool>,
    write_pending: Arc<AtomicUsize>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ConnectionHandle {
    /// Spawn the reader thread for an already-open transport. `reopen` is
    /// called to re-establish the connection after it is judged dropped;
    /// it is retried with bounded exponential backoff until it succeeds or
    /// `disconnect()`/drop stops the thread.
    pub fn spawn<F>(transport: Box<dyn SerialTransport>, bus: Arc<EventBus>, reopen: F) -> Self
    where
        F: Fn() -> Result<Box<dyn SerialTransport>, CoreError> + Send + 'static,
    {
        let transport = Arc::new(Mutex::new(transport));
        let stop = Arc::new(AtomicBool::new(false));
        let write_pending = Arc::new(AtomicUsize::new(0));

        let thread_transport = Arc::clone(&transport);
        let thread_stop = Arc::clone(&stop);
        let thread_write_pending = Arc::clone(&write_pending);
        let reader_thread = std::thread::spawn(move || {
            reader_loop(
                &thread_transport,
                &thread_stop,
                &thread_write_pending,
                &bus,
                &reopen,
            );
        });

        ConnectionHandle {
            transport,
            stop,
            write_pending,
            reader_thread: Some(reader_thread),
        }
    }

    /// Write a line to the device (a trailing `\n` is appended). Runs on
    /// `spawn_blocking` so the async runtime is never blocked on serial
    /// I/O; must be called from within a Tokio runtime context.
    ///
    /// `write_pending` is incremented before the lock is requested and
    /// decremented once the write completes, so `reader_loop` knows to
    /// yield the transport lock rather than immediately re-acquiring it
    /// after every read -- without this, a reader thread holding the lock
    /// for a full read-timeout cycle on every iteration (the common case on
    /// a quiet device) can starve a waiting writer indefinitely under an
    /// unfair mutex (observed: multi-second to 78s waits in testing).
    pub async fn write_line(&self, line: &str) -> Result<(), CoreError> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.write_bytes(bytes).await
    }

    /// Write a single raw byte to the device with no newline appended --
    /// e.g. a bare space, `\r`, or `q` in response to a `--More--`-style
    /// pagination prompt, where the device is waiting for exactly one
    /// keystroke rather than a submitted line. Shares `write_line`'s
    /// `write_pending` wrapping so this path doesn't reintroduce the
    /// reader/writer starvation `write_line` was fixed for.
    pub async fn write_raw(&self, byte: u8) -> Result<(), CoreError> {
        self.write_bytes(vec![byte]).await
    }

    async fn write_bytes(&self, bytes: Vec<u8>) -> Result<(), CoreError> {
        let transport = Arc::clone(&self.transport);
        let write_pending = Arc::clone(&self.write_pending);

        tokio::task::spawn_blocking(move || {
            write_pending.fetch_add(1, Ordering::SeqCst);
            let result = (|| {
                let mut guard = transport
                    .lock()
                    .map_err(|_| CoreError::Config("serial transport lock poisoned".to_string()))?;
                guard.write_all(&bytes).map_err(CoreError::Io)
            })();
            write_pending.fetch_sub(1, Ordering::SeqCst);
            result
        })
        .await
        .map_err(|e| CoreError::Config(format!("write task panicked: {e}")))?
    }

    /// Stop the reader thread and wait for it to exit.
    pub fn disconnect(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// How long to yield the transport lock for when a writer is waiting.
/// Small relative to the read timeout so it doesn't meaningfully delay
/// processing device output, but enough to reliably lose the next lock
/// race to a writer that is already blocked on `Mutex::lock`.
const WRITER_YIELD: Duration = Duration::from_millis(2);

fn reader_loop<F>(
    transport: &Arc<Mutex<Box<dyn SerialTransport>>>,
    stop: &Arc<AtomicBool>,
    write_pending: &Arc<AtomicUsize>,
    bus: &Arc<EventBus>,
    reopen: &F,
) where
    F: Fn() -> Result<Box<dyn SerialTransport>, CoreError>,
{
    bus.publish(SessionEvent::ConnectionStateChanged(
        ConnectionState::Connected,
    ));

    let mut assembler = LineAssembler::new();
    let mut consecutive_errors = 0u32;
    let mut backoff = RECONNECT_BASE_DELAY;
    let mut buf = [0u8; 1024];

    while !stop.load(Ordering::SeqCst) {
        let read_result = {
            let Ok(mut guard) = transport.lock() else {
                break; // poisoned -- nothing more we can safely do
            };
            guard.read(&mut buf)
        };
        // Lock released above (guard out of scope) before this check --
        // sleeping here, not inside the guard's scope, is what actually
        // gives a waiting writer a window to win the next lock race. See
        // `ConnectionHandle::write_line`'s doc comment for why this exists.
        if write_pending.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(WRITER_YIELD);
        }

        match read_result {
            Ok(0) => consecutive_errors += 1,
            Ok(n) => {
                consecutive_errors = 0;
                for item in assembler.feed(&buf[..n]) {
                    match item {
                        AssembledOutput::Line(line) => {
                            bus.publish(SessionEvent::RawLine(line));
                        }
                        AssembledOutput::PaginationPrompt(prompt) => {
                            bus.publish(SessionEvent::PaginationPrompt(prompt));
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // No data within the poll timeout -- expected, keep polling.
            }
            Err(_) => consecutive_errors += 1,
        }

        if consecutive_errors >= CONSECUTIVE_ERROR_THRESHOLD {
            bus.publish(SessionEvent::ConnectionStateChanged(
                ConnectionState::Disconnected,
            ));
            if !reconnect_with_backoff(transport, stop, bus, reopen, &mut backoff) {
                // stop was requested during backoff/retry -- fall through
                // to the shared exit path below rather than returning
                // directly, so this path also flushes and reports
                // Disconnected exactly like the normal stop path.
                break;
            }
            consecutive_errors = 0;
            assembler = LineAssembler::new();
        }
    }

    if let Some(line) = assembler.flush() {
        bus.publish(SessionEvent::RawLine(line));
    }
    // Always reported on every exit path (explicit disconnect() call,
    // stop requested mid-backoff, or a poisoned lock) so the UI can rely
    // on seeing Disconnected rather than the connection silently going
    // quiet -- otherwise a user-requested disconnect never updates
    // App::connection_state and the "press Ctrl+C again to quit" fallback
    // never becomes reachable.
    bus.publish(SessionEvent::ConnectionStateChanged(
        ConnectionState::Disconnected,
    ));
}

/// Retries `reopen` with bounded exponential backoff. Returns `false` if
/// `stop` was set while waiting (caller should exit its loop rather than
/// continue with a connection that was never re-established).
fn reconnect_with_backoff<F>(
    transport: &Arc<Mutex<Box<dyn SerialTransport>>>,
    stop: &Arc<AtomicBool>,
    bus: &Arc<EventBus>,
    reopen: &F,
    backoff: &mut Duration,
) -> bool
where
    F: Fn() -> Result<Box<dyn SerialTransport>, CoreError>,
{
    bus.publish(SessionEvent::ConnectionStateChanged(
        ConnectionState::Reconnecting,
    ));

    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        match reopen() {
            Ok(new_transport) => {
                if let Ok(mut guard) = transport.lock() {
                    *guard = new_transport;
                }
                bus.publish(SessionEvent::ConnectionStateChanged(
                    ConnectionState::Connected,
                ));
                *backoff = RECONNECT_BASE_DELAY;
                return true;
            }
            Err(_) => {
                std::thread::sleep(*backoff);
                *backoff = (*backoff * 2).min(RECONNECT_MAX_DELAY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    /// A scripted transport: reads are served from a fixed queue of
    /// canned results (bytes, timeouts, or errors); writes are captured
    /// for assertions.
    struct MockTransport {
        reads: std::collections::VecDeque<std::io::Result<Vec<u8>>>,
        writes_tx: mpsc::Sender<Vec<u8>>,
        /// How long a read blocks once the queue is exhausted, simulating
        /// an idle port's read-timeout cycle. Existing tests use a short
        /// value to stay fast; the writer-starvation regression test below
        /// uses `open_serial_transport`'s real 200ms to reproduce the
        /// duty-cycle conditions that actually triggered the bug.
        idle_sleep: Duration,
    }

    impl SerialTransport for MockTransport {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.reads.pop_front() {
                Some(Ok(data)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
                Some(Err(e)) => Err(e),
                None => {
                    // Queue exhausted: behave like an idle port timing out
                    // forever so the reader thread just polls quietly.
                    std::thread::sleep(self.idle_sleep);
                    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "idle"))
                }
            }
        }

        fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
            let _ = self.writes_tx.send(buf.to_vec());
            Ok(())
        }
    }

    fn broken_pipe() -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
    }

    #[tokio::test]
    async fn incoming_bytes_become_raw_line_events() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::from([Ok(b"Switch> \r\n".to_vec())]),
            writes_tx,
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        let mut handle = ConnectionHandle::spawn(Box::new(transport), Arc::clone(&bus), || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        // First event: connected.
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("Switch> ".to_string())
        );

        handle.disconnect();
    }

    #[tokio::test]
    async fn write_line_forwards_bytes_with_trailing_newline() {
        let (writes_tx, writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::new(),
            writes_tx,
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));

        let mut handle = ConnectionHandle::spawn(Box::new(transport), bus, || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        handle.write_line("show version").await.unwrap();

        let written = writes_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("write should have been forwarded to the transport");
        assert_eq!(written, b"show version\n");

        handle.disconnect();
    }

    #[tokio::test]
    async fn write_raw_forwards_exactly_one_byte_with_no_newline_appended() {
        let (writes_tx, writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::new(),
            writes_tx,
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));

        let mut handle = ConnectionHandle::spawn(Box::new(transport), bus, || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        handle.write_raw(b' ').await.unwrap();

        let written = writes_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("write should have been forwarded to the transport");
        assert_eq!(
            written,
            vec![b' '],
            "write_raw must send exactly the one byte, no trailing newline"
        );

        handle.disconnect();
    }

    #[tokio::test]
    async fn unterminated_more_prompt_becomes_a_pagination_prompt_event() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::from([Ok(b"Router#show run\r\n--More--".to_vec())]),
            writes_tx,
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        let mut handle = ConnectionHandle::spawn(Box::new(transport), Arc::clone(&bus), || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::RawLine("Router#show run".to_string())
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::PaginationPrompt("--More--".to_string())
        );

        handle.disconnect();
    }

    /// Regression test for a real latency bug: the reader thread holds the
    /// transport mutex for the entire duration of each read (bounded by the
    /// port's read timeout -- 200ms on a real device, matched here), then
    /// releases and immediately re-locks for the next iteration. On an idle
    /// port that's a near-100% duty cycle, and under macOS's unfair
    /// `os_unfair_lock` a `write_line` call competing for the same mutex
    /// was observed to starve for anywhere from several seconds to 78s
    /// waiting for a lock window that the reader kept winning first. Uses a
    /// real 200ms idle cycle (not the 5ms used by other tests here) to
    /// reproduce the duty cycle that actually triggered it.
    #[tokio::test]
    async fn write_line_is_not_starved_by_a_busy_reader_on_an_idle_port() {
        let (writes_tx, writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::new(),
            writes_tx,
            idle_sleep: Duration::from_millis(200),
        };
        let bus = Arc::new(EventBus::new(16));

        let mut handle = ConnectionHandle::spawn(Box::new(transport), bus, || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        // Let the reader thread settle into its idle read-timeout cycle
        // before racing a write against it.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let start = Instant::now();
        handle.write_line("show version").await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "write_line took {elapsed:?} against an idle 200ms-cycle reader -- \
             should bound to roughly one read cycle, not be starved indefinitely"
        );

        writes_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("write should have been forwarded to the transport");

        handle.disconnect();
    }

    #[tokio::test]
    async fn repeated_broken_pipe_triggers_disconnect_then_reconnect() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::from([broken_pipe(), broken_pipe(), broken_pipe()]),
            writes_tx: writes_tx.clone(),
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        let reopened = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reopened_clone = Arc::clone(&reopened);
        let mut handle =
            ConnectionHandle::spawn(Box::new(transport), Arc::clone(&bus), move || {
                reopened_clone.store(true, Ordering::SeqCst);
                Ok(Box::new(MockTransport {
                    reads: std::collections::VecDeque::new(),
                    writes_tx: writes_tx.clone(),
                    idle_sleep: Duration::from_millis(5),
                }) as Box<dyn SerialTransport>)
            });

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Disconnected)
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Reconnecting)
        );
        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        while !reopened.load(Ordering::SeqCst) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            reopened.load(Ordering::SeqCst),
            "reopen() should have been called"
        );

        handle.disconnect();
    }

    #[tokio::test]
    async fn disconnect_stops_reader_thread_promptly() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::new(),
            writes_tx,
            idle_sleep: Duration::from_millis(5),
        };
        let bus = Arc::new(EventBus::new(16));

        let mut handle = ConnectionHandle::spawn(Box::new(transport), bus, || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        let start = Instant::now();
        handle.disconnect();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "disconnect() should join the reader thread promptly"
        );
    }

    /// Exercises the real `serialport` code path (open_serial_transport's
    /// `SerialTransport` impl, actual OS reads/writes) against a Unix PTY
    /// pair instead of a mock -- the closest this machine can get to real
    /// hardware without a physical USB-serial adapter attached. This does
    /// NOT substitute for a manual test against real Cisco/etc. hardware
    /// (no USB enumeration, no device-specific timing/quirks), but it does
    /// confirm the reader thread, line assembler, and event bus work
    /// end-to-end over genuine OS file descriptors.
    #[cfg(unix)]
    #[tokio::test]
    async fn real_pty_bytes_flow_through_to_raw_line_events() {
        let (mut controller, device_side) =
            serialport::TTYPort::pair().expect("failed to allocate a PTY pair for this test");
        let device_side: Box<dyn serialport::SerialPort> = Box::new(device_side);
        let device_side: Box<dyn SerialTransport> = Box::new(device_side);

        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        let mut handle = ConnectionHandle::spawn(device_side, Arc::clone(&bus), || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );

        std::io::Write::write_all(&mut controller, b"Switch> \r\n")
            .expect("write into the PTY controller side should succeed");

        let event = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .expect("timed out waiting for the real-PTY RawLine event")
            .unwrap();
        assert_eq!(event, SessionEvent::RawLine("Switch> ".to_string()));

        handle.disconnect();
    }

    /// End-to-end proof of the full `--More--` passthrough round trip over
    /// a real PTY pair, standing in for a manual test against a physical
    /// device (no keyboard/TTY to drive the interactive `ttyt` binary
    /// itself is available in this environment, so this exercises the
    /// same real-OS-file-descriptor path `real_pty_bytes_flow_through_to_
    /// raw_line_events` does, extended through `write_raw`): an
    /// unterminated `--More--` write must surface as a `PaginationPrompt`
    /// event, and responding with `write_raw` must deliver to the other
    /// side of the PTY exactly the one byte pressed -- no newline, no
    /// extra bytes, nothing left mangled in the assembler's buffer.
    #[cfg(unix)]
    #[tokio::test]
    async fn real_pty_more_prompt_and_raw_key_response_round_trip() {
        let (mut controller, device_side) =
            serialport::TTYPort::pair().expect("failed to allocate a PTY pair for this test");
        let device_side: Box<dyn serialport::SerialPort> = Box::new(device_side);
        let device_side: Box<dyn SerialTransport> = Box::new(device_side);

        let bus = Arc::new(EventBus::new(16));
        let mut sub = bus.subscribe();

        let mut handle = ConnectionHandle::spawn(device_side, Arc::clone(&bus), || {
            Err(CoreError::Config("no reopen in this test".to_string()))
        });

        assert_eq!(
            sub.recv().await.unwrap(),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected)
        );

        // The device pages a `show run` and blocks -- no trailing newline,
        // a real terminal is waiting on a single keystroke here.
        std::io::Write::write_all(&mut controller, b"interface Gi0/1\r\n--More--")
            .expect("write into the PTY controller side should succeed");

        let line_event = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .expect("timed out waiting for the real-PTY RawLine event")
            .unwrap();
        assert_eq!(
            line_event,
            SessionEvent::RawLine("interface Gi0/1".to_string())
        );

        let prompt_event = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .expect("timed out waiting for the real-PTY PaginationPrompt event")
            .unwrap();
        assert_eq!(
            prompt_event,
            SessionEvent::PaginationPrompt("--More--".to_string())
        );

        // Respond as the UI would on a space keypress in Mode::Pagination.
        handle
            .write_raw(b' ')
            .await
            .expect("write_raw should succeed");

        let received = tokio::task::spawn_blocking(move || {
            use serialport::SerialPort;
            controller
                .set_timeout(Duration::from_secs(2))
                .expect("set_timeout should succeed on a PTY controller");
            let mut buf = [0u8; 64];
            let n = std::io::Read::read(&mut controller, &mut buf).unwrap_or(0);
            buf[..n].to_vec()
        })
        .await
        .expect("blocking read of the PTY controller should not panic");
        assert_eq!(
            received,
            vec![b' '],
            "the device side must receive exactly the one raw byte, nothing else"
        );

        handle.disconnect();
    }
}
