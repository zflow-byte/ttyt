use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::CoreError;
use crate::events::{ConnectionState, EventBus, LineAssembler, SessionEvent};

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

        let thread_transport = Arc::clone(&transport);
        let thread_stop = Arc::clone(&stop);
        let reader_thread = std::thread::spawn(move || {
            reader_loop(&thread_transport, &thread_stop, &bus, &reopen);
        });

        ConnectionHandle {
            transport,
            stop,
            reader_thread: Some(reader_thread),
        }
    }

    /// Write a line to the device (a trailing `\n` is appended). Runs on
    /// `spawn_blocking` so the async runtime is never blocked on serial
    /// I/O; must be called from within a Tokio runtime context.
    pub async fn write_line(&self, line: &str) -> Result<(), CoreError> {
        let transport = Arc::clone(&self.transport);
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');

        tokio::task::spawn_blocking(move || {
            let mut guard = transport
                .lock()
                .map_err(|_| CoreError::Config("serial transport lock poisoned".to_string()))?;
            guard.write_all(&bytes).map_err(CoreError::Io)
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

fn reader_loop<F>(
    transport: &Arc<Mutex<Box<dyn SerialTransport>>>,
    stop: &Arc<AtomicBool>,
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

        match read_result {
            Ok(0) => consecutive_errors += 1,
            Ok(n) => {
                consecutive_errors = 0;
                for line in assembler.feed(&buf[..n]) {
                    bus.publish(SessionEvent::RawLine(line));
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
                return; // stop was requested during backoff/retry
            }
            consecutive_errors = 0;
            assembler = LineAssembler::new();
        }
    }

    if let Some(line) = assembler.flush() {
        bus.publish(SessionEvent::RawLine(line));
    }
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
                    std::thread::sleep(Duration::from_millis(5));
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
    async fn repeated_broken_pipe_triggers_disconnect_then_reconnect() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let transport = MockTransport {
            reads: std::collections::VecDeque::from([broken_pipe(), broken_pipe(), broken_pipe()]),
            writes_tx: writes_tx.clone(),
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
}
