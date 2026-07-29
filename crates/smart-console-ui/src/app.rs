use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;
use smart_console_core::{ConnectionState, SessionEvent};
use tokio::sync::{broadcast, mpsc};

use crate::theme::Theme;
use crate::widgets;

/// Scrollback is bounded so a long-running session can't grow the
/// in-memory buffer without limit.
const MAX_SCROLLBACK_LINES: usize = 2000;

/// Central UI state. `widgets::render` is a pure function of this struct;
/// `on_key`/`apply_session_event` are the only ways it mutates.
pub struct App {
    pub scrollback: VecDeque<String>,
    pub input: String,
    pub connection_state: ConnectionState,
    pub port_name: Option<String>,
    pub recording_path: Option<String>,
    /// Set by keys the spec defines but this phase doesn't implement yet
    /// (Ctrl+N/Ctrl+P/Ctrl+R/TAB/ESC) — shown in the bottom-right hints
    /// pane rather than the keypress being silently swallowed.
    pub hint: Option<String>,
    pub should_quit: bool,
    pub disconnect_requested: bool,
    pending_submit: Option<String>,
}

impl App {
    pub fn new() -> Self {
        App {
            scrollback: VecDeque::new(),
            input: String::new(),
            connection_state: ConnectionState::Disconnected,
            port_name: None,
            recording_path: None,
            hint: None,
            should_quit: false,
            disconnect_requested: false,
            pending_submit: None,
        }
    }

    pub fn push_line(&mut self, line: String) {
        self.scrollback.push_back(line);
        while self.scrollback.len() > MAX_SCROLLBACK_LINES {
            self.scrollback.pop_front();
        }
    }

    pub fn clear_console(&mut self) {
        self.scrollback.clear();
    }

    pub fn apply_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::RawLine(line) => self.push_line(line),
            SessionEvent::ConnectionStateChanged(state) => self.connection_state = state,
        }
    }

    /// Takes the line submitted by the most recent Enter keypress, if any.
    pub fn take_pending_submit(&mut self) -> Option<String> {
        self.pending_submit.take()
    }

    fn set_not_yet_implemented_hint(&mut self, key: &str) {
        self.hint = Some(format!("{key}: not yet implemented"));
    }

    /// Handles one key event. Ctrl+C requests a disconnect; pressed again
    /// once already disconnected, it quits the app instead -- the spec
    /// defines Ctrl+C as "disconnect" but no dedicated quit key, so this
    /// is the fallback that makes the app exitable without stepping on
    /// the spec's other bindings (ESC is reserved for the Phase 3 menu).
    pub fn on_key(&mut self, key: KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                if self.connection_state == ConnectionState::Disconnected {
                    self.should_quit = true;
                } else {
                    self.disconnect_requested = true;
                }
            }
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => self.clear_console(),
            (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
                self.set_not_yet_implemented_hint("Ctrl+N")
            }
            (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                self.set_not_yet_implemented_hint("Ctrl+P")
            }
            (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
                self.set_not_yet_implemented_hint("Ctrl+R")
            }
            (_, KeyCode::Tab) => self.set_not_yet_implemented_hint("TAB"),
            (_, KeyCode::Esc) => self.set_not_yet_implemented_hint("ESC"),
            (_, KeyCode::Enter) => {
                if !self.input.is_empty() {
                    self.pending_submit = Some(std::mem::take(&mut self.input));
                }
            }
            (_, KeyCode::Backspace) => {
                self.input.pop();
            }
            (_, KeyCode::Char(c)) => self.input.push(c),
            _ => {}
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Polls the terminal for input on a dedicated OS thread (crossterm's
/// blocking API doesn't compose with async directly), forwarding key-press
/// events to the returned channel. Mirrors the same blocking-I/O-on-a-
/// dedicated-thread pattern used for serial reads in
/// `smart_console_core::device::connection`.
pub fn spawn_input_thread(poll_interval: Duration) -> mpsc::UnboundedReceiver<KeyEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match event::poll(poll_interval) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if tx.send(key).is_err() {
                            return; // receiver dropped -- app is shutting down
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
    rx
}

/// Drives the TUI until the user quits. `submit_tx` receives each line the
/// user submits with Enter; this crate has no dependency on how (or
/// whether) a caller sends it anywhere, keeping the UI decoupled from
/// `smart-console-core`'s connection/serial types.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    session_events: &mut broadcast::Receiver<SessionEvent>,
    submit_tx: mpsc::UnboundedSender<String>,
) -> std::io::Result<()> {
    let theme = Theme::dark();
    let mut key_events = spawn_input_thread(Duration::from_millis(100));

    loop {
        terminal.draw(|frame| widgets::render(frame, app, &theme))?;

        tokio::select! {
            key = key_events.recv() => {
                match key {
                    Some(key) => {
                        app.on_key(key);
                        if let Some(line) = app.take_pending_submit() {
                            let _ = submit_tx.send(line);
                        }
                        if app.should_quit {
                            return Ok(());
                        }
                    }
                    None => return Ok(()), // input thread ended
                }
            }
            event = session_events.recv() => {
                match event {
                    Ok(event) => app.apply_session_event(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn key(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn typing_and_backspace_edit_the_input_line() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('h')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('i')));
        assert_eq!(app.input, "hi");
        app.on_key(key(KeyModifiers::NONE, KeyCode::Backspace));
        assert_eq!(app.input, "h");
    }

    #[test]
    fn enter_submits_and_clears_the_input_line() {
        let mut app = App::new();
        for c in "show version".chars() {
            app.on_key(key(KeyModifiers::NONE, KeyCode::Char(c)));
        }
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
        assert_eq!(app.input, "");
        assert_eq!(app.take_pending_submit(), Some("show version".to_string()));
        assert_eq!(app.take_pending_submit(), None);
    }

    #[test]
    fn enter_on_empty_input_does_not_submit() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
        assert_eq!(app.take_pending_submit(), None);
    }

    #[test]
    fn ctrl_l_clears_console_scrollback() {
        let mut app = App::new();
        app.push_line("some output".to_string());
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('l')));
        assert!(app.scrollback.is_empty());
    }

    #[test]
    fn ctrl_c_requests_disconnect_when_connected() {
        let mut app = App::new();
        app.connection_state = ConnectionState::Connected;
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(app.disconnect_requested);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_when_already_disconnected() {
        let mut app = App::new();
        assert_eq!(app.connection_state, ConnectionState::Disconnected);
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn unimplemented_keys_set_a_visible_hint_instead_of_being_silently_dropped() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert_eq!(app.hint.as_deref(), Some("Ctrl+N: not yet implemented"));

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.hint.as_deref(), Some("TAB: not yet implemented"));

        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));
        assert_eq!(app.hint.as_deref(), Some("ESC: not yet implemented"));
    }

    #[test]
    fn raw_line_session_events_append_to_scrollback() {
        let mut app = App::new();
        app.apply_session_event(SessionEvent::RawLine("Switch> ".to_string()));
        assert_eq!(app.scrollback.back().map(String::as_str), Some("Switch> "));
    }

    #[test]
    fn connection_state_changed_event_updates_header_state() {
        let mut app = App::new();
        app.apply_session_event(SessionEvent::ConnectionStateChanged(
            ConnectionState::Connected,
        ));
        assert_eq!(app.connection_state, ConnectionState::Connected);
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_SCROLLBACK_LINES + 10) {
            app.push_line(format!("line {i}"));
        }
        assert_eq!(app.scrollback.len(), MAX_SCROLLBACK_LINES);
        assert_eq!(app.scrollback.front().map(String::as_str), Some("line 10"));
    }
}
