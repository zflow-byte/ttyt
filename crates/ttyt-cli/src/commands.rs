use std::path::{Path, PathBuf};
use std::sync::Arc;

use ttyt_core::device::{ConnectionHandle, open_serial_transport, scan};
use ttyt_core::{CommandHistory, Config, EventBus, PluginRegistry, SessionId, SessionRecorder};

pub fn list_devices() -> anyhow::Result<()> {
    let config = Config::load()?;
    let candidates = scan(&config.baud_candidates)?;

    if candidates.is_empty() {
        println!("No serial devices found.");
        return Ok(());
    }

    println!(
        "{:<32} {:<24} {:<24} SUGGESTED BAUDS",
        "PORT", "VENDOR", "PRODUCT"
    );
    for candidate in candidates {
        let bauds = candidate
            .suggested_bauds
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{:<32} {:<24} {:<24} {}",
            candidate.port_name,
            candidate.usb_vendor.as_deref().unwrap_or("-"),
            candidate.usb_product.as_deref().unwrap_or("-"),
            bauds,
        );
    }
    Ok(())
}

/// Restores the terminal on panic -- without this, a panic while raw mode/
/// the alternate screen is active leaves the user's shell visibly broken
/// until they blindly type `reset`.
fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = ttyt_ui::terminal::restore();
        original_hook(panic_info);
    }));
}

/// Multiple concurrent sessions share one `history.txt` path in `Config`;
/// giving each session its own file (suffixed by its sanitized port name)
/// avoids two sessions' writers interleaving into the same file. A
/// single-session run gets `history-<port>.txt` too, not the bare
/// configured path -- one consistent rule rather than a single-vs-multi
/// special case.
fn per_session_history_path(base: &Path, port: &str) -> PathBuf {
    let sanitized: String = port
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("history");
    let ext = base.extension().and_then(|s| s.to_str());
    match ext {
        Some(ext) => base.with_file_name(format!("{stem}-{sanitized}.{ext}")),
        None => base.with_file_name(format!("{stem}-{sanitized}")),
    }
}

/// Everything the CLI keeps per session outside of `ttyt_ui::App` (which
/// owns the display-facing state): the live connection and the persisted
/// history writer. Indexed in parallel with `App::sessions` by
/// `SessionId::index()` -- safe because Phase 3 never adds or removes a
/// session at runtime (see `ttyt_core::events::SessionId`'s doc comment).
struct SessionHandles {
    connection: ConnectionHandle,
    history: CommandHistory,
}

pub async fn connect(ports: Vec<String>, baud: Option<u32>) -> anyhow::Result<()> {
    let config = Config::load()?;
    let baud = baud.unwrap_or_else(|| config.baud_candidates.first().copied().unwrap_or(9600));

    let (session_tx, mut session_events) =
        tokio::sync::mpsc::unbounded_channel::<(SessionId, ttyt_core::SessionEvent)>();

    let mut app = ttyt_ui::App::with_session_count(ports.len());
    app.set_dangerous_command_guard(ttyt_core::DangerousCommandGuard::new(
        &config.dangerous_command_patterns,
    )?);
    let mut handles: Vec<SessionHandles> = Vec::with_capacity(ports.len());

    for (index, port) in ports.iter().enumerate() {
        let id = SessionId::new(index);

        let transport = open_serial_transport(port, baud)?;
        let bus = Arc::new(EventBus::new(1024));

        let recorder = SessionRecorder::create(&config.log_dir, &config.redaction_patterns)?;
        let recording_path = recorder.path().display().to_string();
        tokio::spawn(ttyt_core::session::recorder::run(recorder, bus.subscribe()));
        tokio::spawn(ttyt_core::detector::run(
            PluginRegistry::with_default_plugins(),
            Arc::clone(&bus),
            bus.subscribe(),
        ));

        // Tags this session's own events with its SessionId onto the one
        // shared channel `ttyt_ui::run` selects on -- `tokio::select!`
        // can't fan over a `Vec` of receivers directly (see app.rs's
        // `run` doc comment), so each session gets a small forwarder task
        // instead of pulling in an undeclared stream-multiplexing crate.
        let mut forward_rx = bus.subscribe();
        let forward_tx = session_tx.clone();
        tokio::spawn(async move {
            loop {
                match forward_rx.recv().await {
                    Ok(event) => {
                        if forward_tx.send((id, event)).is_err() {
                            return; // UI side gone
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        let reopen_port = port.clone();
        let connection = ConnectionHandle::spawn(transport, Arc::clone(&bus), move || {
            open_serial_transport(&reopen_port, baud)
        });

        let history_path = per_session_history_path(&config.history_path, port);
        let history = CommandHistory::open(
            &history_path,
            &config.redaction_patterns,
            config.history_max_entries,
        )?;

        let session = &mut app.sessions[index];
        session.port_name = Some(port.clone());
        session.recording_path = Some(recording_path);
        session.history = history.entries().to_vec();

        handles.push(SessionHandles {
            connection,
            history,
        });
    }
    // Every forwarder task holds its own clone; dropping this one lets
    // `session_events` close once all of them have exited.
    drop(session_tx);

    install_panic_hook();
    let mut terminal = ttyt_ui::terminal::init()?;
    let (submit_tx, mut submit_rx) = tokio::sync::mpsc::unbounded_channel::<(SessionId, String)>();
    let (disconnect_tx, mut disconnect_rx) = tokio::sync::mpsc::unbounded_channel::<SessionId>();
    let (raw_key_tx, mut raw_key_rx) =
        tokio::sync::mpsc::unbounded_channel::<(SessionId, Vec<u8>)>();

    let ui_result = {
        let mut ui_future = Box::pin(ttyt_ui::run(
            &mut terminal,
            &mut app,
            &mut session_events,
            submit_tx,
            disconnect_tx,
            raw_key_tx,
            ttyt_ui::Theme::from_name(&config.theme),
        ));
        loop {
            tokio::select! {
                res = &mut ui_future => break res,
                Some((id, line)) = submit_rx.recv() => {
                    let session = &mut handles[id.index()];
                    if let Err(e) = session.history.append(&line) {
                        tracing::error!(error = %e, "failed to persist command history");
                    }
                    if let Err(e) = session.connection.write_line(&line).await {
                        tracing::error!(error = %e, "failed to write to device");
                    }
                }
                Some((id, bytes)) = raw_key_rx.recv() => {
                    // Single-keystroke passthrough for a --More---style
                    // pagination prompt, or every keystroke while raw
                    // passthrough mode is active: not a submitted command,
                    // so no history entry, just the raw bytes straight to
                    // the device (see Mode::Pagination / write_raw's doc
                    // comments for why this needs its own path).
                    if let Err(e) = handles[id.index()].connection.write_raw(&bytes).await {
                        tracing::error!(error = %e, "failed to write raw key to device");
                    }
                }
                Some(id) = disconnect_rx.recv() => {
                    // Ctrl+C on a connected tab: stop that session's reader
                    // thread now rather than waiting for the TUI to quit.
                    // The reader thread publishes ConnectionStateChanged
                    // (Disconnected) on its way out, which flows back to
                    // that tab's Session and makes a second Ctrl+C on it
                    // eligible to quit the app per App::handle_ctrl_c.
                    handles[id.index()].connection.disconnect();
                }
            }
        }
    };

    for session in &mut handles {
        session.connection.disconnect();
    }
    ttyt_ui::terminal::restore()?;
    ui_result?;
    Ok(())
}

/// Replays a saved session log through the same TUI/detector pipeline a
/// live `connect()` session uses -- a single session, no `ConnectionHandle`
/// and no `CommandHistory`, since there is no device to write to or
/// history to persist. Deliberately its own subcommand rather than a
/// session type folded into `connect()`: that loop indexes
/// `handles[id.index()]` on every submit/disconnect, one entry per
/// `--port`, so a replay tab added to `app.sessions` without a matching
/// `SessionHandles` entry would panic on the first Enter press in that
/// tab.
pub async fn replay(path: PathBuf, speed: f64) -> anyhow::Result<()> {
    // Fail fast on a bad path/permissions before entering raw-mode/the
    // alternate screen, matching `connect()`'s upfront
    // `open_serial_transport(..)?` -- otherwise the error would only
    // surface as a silently-empty replay session once inside the TUI
    // (the actual read happens later, inside a spawned task whose errors
    // are only logged, not propagated).
    tokio::fs::File::open(&path)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open replay log {}: {e}", path.display()))?;

    // Replay has no device to connect to and so needs nothing else out of
    // Config (no log_dir/redaction/dangerous-command-guard) -- loaded only
    // for the theme, so replay renders with the same palette `connect`
    // would use rather than always falling back to the default.
    let config = Config::load()?;

    let (session_tx, mut session_events) =
        tokio::sync::mpsc::unbounded_channel::<(SessionId, ttyt_core::SessionEvent)>();

    let mut app = ttyt_ui::App::with_session_count(1);
    let id = SessionId::new(0);
    app.sessions[0].port_name = Some(format!("replay:{}", path.display()));

    let bus = Arc::new(EventBus::new(1024));
    tokio::spawn(ttyt_core::detector::run(
        PluginRegistry::with_default_plugins(),
        Arc::clone(&bus),
        bus.subscribe(),
    ));

    let mut forward_rx = bus.subscribe();
    let forward_tx = session_tx.clone();
    tokio::spawn(async move {
        loop {
            match forward_rx.recv().await {
                Ok(event) => {
                    if forward_tx.send((id, event)).is_err() {
                        return; // UI side gone
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let replay_bus = Arc::clone(&bus);
    tokio::spawn(async move {
        if let Err(e) = ttyt_core::session::replay::run(&path, speed, replay_bus).await {
            tracing::error!(error = %e, "session replay failed");
        }
    });
    // The forwarder above holds its own clone; dropping this one lets
    // `session_events` close once the forwarder exits (which happens when
    // the replay task finishes and the bus's last publisher goes away).
    drop(session_tx);

    install_panic_hook();
    let mut terminal = ttyt_ui::terminal::init()?;
    let (submit_tx, mut submit_rx) = tokio::sync::mpsc::unbounded_channel::<(SessionId, String)>();
    let (disconnect_tx, mut disconnect_rx) = tokio::sync::mpsc::unbounded_channel::<SessionId>();
    let (raw_key_tx, mut raw_key_rx) =
        tokio::sync::mpsc::unbounded_channel::<(SessionId, Vec<u8>)>();

    let ui_result = {
        let mut ui_future = Box::pin(ttyt_ui::run(
            &mut terminal,
            &mut app,
            &mut session_events,
            submit_tx,
            disconnect_tx,
            raw_key_tx,
            ttyt_ui::Theme::from_name(&config.theme),
        ));
        loop {
            tokio::select! {
                res = &mut ui_future => break res,
                // Replay has no live device: there's nothing to write a
                // submitted command to and nothing to disconnect, so all
                // three channels are drained and discarded rather than
                // acted on. Ctrl+C still quits -- a replay session's
                // `connection_state` never leaves `Disconnected` (no
                // `ConnectionStateChanged` events are published during
                // replay), so `App::handle_ctrl_c` treats the first Ctrl+C
                // as "already disconnected, no other tab up" and quits.
                Some(_) = submit_rx.recv() => {}
                Some(_) = disconnect_rx.recv() => {}
                Some(_) = raw_key_rx.recv() => {}
            }
        }
    };

    ttyt_ui::terminal::restore()?;
    ui_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn per_session_history_path_suffixes_the_sanitized_port_name() {
        let base = PathBuf::from("/data/ttyt/history.txt");
        let path = per_session_history_path(&base, "/dev/cu.usbserial-1410");
        assert_eq!(
            path,
            PathBuf::from("/data/ttyt/history-_dev_cu.usbserial-1410.txt")
        );
    }

    #[test]
    fn per_session_history_path_gives_two_ports_distinct_files() {
        let base = PathBuf::from("/data/ttyt/history.txt");
        let a = per_session_history_path(&base, "/dev/cu.usbserial-AAAA");
        let b = per_session_history_path(&base, "/dev/cu.usbserial-BBBB");
        assert_ne!(a, b);
    }

    /// Exercises the real multi-session wiring `connect()` sets up --
    /// two real PTY pairs, two `ConnectionHandle`s, two per-session
    /// forwarder tasks tagging events onto one shared channel -- without
    /// a terminal/TUI attached. Unit tests already prove
    /// `App::apply_session_event` routes by `SessionId` correctly given a
    /// tagged event; this test proves the tagging itself is correct
    /// end-to-end over genuine OS file descriptors, in both directions:
    /// device output must reach only its own tab's scrollback, and a
    /// line written through one session's `ConnectionHandle` must reach
    /// only that session's device, never the other one. A mis-keyed
    /// `SessionId` anywhere in this pipeline -- exactly the bug class two
    /// concurrent tabs make newly possible -- would fail this test.
    #[cfg(unix)]
    #[tokio::test]
    async fn two_concurrent_sessions_route_events_and_writes_to_the_correct_device() {
        use serialport::SerialPort;
        use std::time::Duration;
        use ttyt_core::device::SerialTransport;
        use ttyt_core::{CoreError, EventBus, SessionEvent};

        let (mut controller_a, mut device_a) =
            serialport::TTYPort::pair().expect("failed to allocate PTY pair A");
        let (controller_b, mut device_b) =
            serialport::TTYPort::pair().expect("failed to allocate PTY pair B");
        // Without a read timeout, the reader thread's blocking `read()` can
        // sit forever with no data pending, so `disconnect()`'s thread-join
        // waits indefinitely -- this is what made this test's wall time
        // vary wildly (single-digit to 45+ seconds) across runs. Mirrors
        // `open_serial_transport`'s real-device timeout.
        device_a
            .set_timeout(Duration::from_millis(200))
            .expect("set_timeout should succeed on PTY device side A");
        device_b
            .set_timeout(Duration::from_millis(200))
            .expect("set_timeout should succeed on PTY device side B");
        let device_a: Box<dyn serialport::SerialPort> = Box::new(device_a);
        let device_a: Box<dyn SerialTransport> = Box::new(device_a);
        let device_b: Box<dyn serialport::SerialPort> = Box::new(device_b);
        let device_b: Box<dyn SerialTransport> = Box::new(device_b);

        let bus_a = Arc::new(EventBus::new(16));
        let bus_b = Arc::new(EventBus::new(16));

        let no_reopen = || Err(CoreError::Config("no reopen in test".to_string()));
        let mut handle_a = ConnectionHandle::spawn(device_a, Arc::clone(&bus_a), no_reopen);
        let mut handle_b = ConnectionHandle::spawn(device_b, Arc::clone(&bus_b), no_reopen);

        let (session_tx, mut session_events) =
            tokio::sync::mpsc::unbounded_channel::<(SessionId, SessionEvent)>();
        for (id, bus) in [(SessionId::new(0), &bus_a), (SessionId::new(1), &bus_b)] {
            let mut rx = bus.subscribe();
            let tx = session_tx.clone();
            tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    if tx.send((id, event)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(session_tx);

        let mut app = ttyt_ui::App::with_session_count(2);

        std::io::Write::write_all(&mut controller_a, b"from-device-A\r\n")
            .expect("write into PTY controller A should succeed");

        // Drain tagged events until session 0's scrollback has the line --
        // session 1 stays untouched since nothing was written to device B.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while app.sessions[0].scrollback.is_empty() && tokio::time::Instant::now() < deadline {
            if let Ok(Some((id, event))) =
                tokio::time::timeout(Duration::from_millis(200), session_events.recv()).await
            {
                app.apply_session_event(id, event);
            }
        }
        assert_eq!(
            app.sessions[0].scrollback.back().map(String::as_str),
            Some("from-device-A"),
            "session 0's scrollback must contain device A's output"
        );
        assert!(
            app.sessions[1].scrollback.is_empty(),
            "session 1 must not see device A's output"
        );

        // Now the other direction: a write through session 0's handle
        // must land on controller A, not controller B.
        handle_a
            .write_line("cmd-for-a")
            .await
            .expect("write_line through handle_a should succeed");

        let controller_a = tokio::task::spawn_blocking(move || {
            controller_a
                .set_timeout(Duration::from_secs(2))
                .expect("set_timeout should succeed on a PTY controller");
            let mut buf = [0u8; 64];
            let n = std::io::Read::read(&mut controller_a, &mut buf).unwrap_or(0);
            assert!(
                String::from_utf8_lossy(&buf[..n]).contains("cmd-for-a"),
                "controller A should receive the line written to handle_a"
            );
            controller_a
        })
        .await
        .expect("blocking read of controller A should not panic");
        drop(controller_a);
        drop(controller_b);

        handle_a.disconnect();
        handle_b.disconnect();
    }
}
