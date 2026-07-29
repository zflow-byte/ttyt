use std::sync::Arc;

use smart_console_core::device::{ConnectionHandle, open_serial_transport, scan};
use smart_console_core::{CommandHistory, Config, EventBus, PluginRegistry, SessionRecorder};

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
        let _ = smart_console_ui::terminal::restore();
        original_hook(panic_info);
    }));
}

pub async fn connect(port: String, baud: Option<u32>) -> anyhow::Result<()> {
    let config = Config::load()?;
    let baud = baud.unwrap_or_else(|| config.baud_candidates.first().copied().unwrap_or(9600));

    let transport = open_serial_transport(&port, baud)?;
    let bus = Arc::new(EventBus::new(1024));

    let recorder = SessionRecorder::create(&config.log_dir, &config.redaction_patterns)?;
    let recording_path = recorder.path().display().to_string();
    tokio::spawn(smart_console_core::session::recorder::run(
        recorder,
        bus.subscribe(),
    ));
    tokio::spawn(smart_console_core::detector::run(
        PluginRegistry::with_default_plugins(),
        Arc::clone(&bus),
        bus.subscribe(),
    ));

    let reopen_port = port.clone();
    let mut handle = ConnectionHandle::spawn(transport, Arc::clone(&bus), move || {
        open_serial_transport(&reopen_port, baud)
    });

    let mut history = CommandHistory::open(
        &config.history_path,
        &config.redaction_patterns,
        config.history_max_entries,
    )?;

    let mut app = smart_console_ui::App::new();
    app.port_name = Some(port);
    app.recording_path = Some(recording_path);
    app.history = history.entries().to_vec();

    install_panic_hook();
    let mut terminal = smart_console_ui::terminal::init()?;
    let mut session_events = bus.subscribe();
    let (submit_tx, mut submit_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (disconnect_tx, mut disconnect_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let ui_result = {
        let mut ui_future = Box::pin(smart_console_ui::run(
            &mut terminal,
            &mut app,
            &mut session_events,
            submit_tx,
            disconnect_tx,
        ));
        loop {
            tokio::select! {
                res = &mut ui_future => break res,
                Some(line) = submit_rx.recv() => {
                    if let Err(e) = history.append(&line) {
                        tracing::error!(error = %e, "failed to persist command history");
                    }
                    if let Err(e) = handle.write_line(&line).await {
                        tracing::error!(error = %e, "failed to write to device");
                    }
                }
                Some(()) = disconnect_rx.recv() => {
                    // Ctrl+C while connected: stop the reader thread now
                    // rather than waiting for the TUI to quit. The reader
                    // thread publishes ConnectionStateChanged(Disconnected)
                    // on its way out, which flows back to `app` and makes
                    // a second Ctrl+C quit the app per App::on_key.
                    handle.disconnect();
                }
            }
        }
    };

    handle.disconnect();
    smart_console_ui::terminal::restore()?;
    ui_result?;
    Ok(())
}
