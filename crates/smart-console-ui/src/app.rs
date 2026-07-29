use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;
use smart_console_core::{
    ConnectionState, ParsedEvent, PromptInfo, SessionEvent, VendorDetectionStatus,
};
use tokio::sync::{broadcast, mpsc};

use crate::theme::Theme;
use crate::widgets;

/// Scrollback is bounded so a long-running session can't grow the
/// in-memory buffer without limit.
const MAX_SCROLLBACK_LINES: usize = 2000;

/// Bottom-left Events pane keeps only the most recent entries.
const MAX_PARSED_EVENTS: usize = 200;

/// `App::history` is bounded the same way as every other unbounded-input
/// buffer here (`scrollback`, `events`) so a long session can't grow it
/// without limit. `smart_console_core::CommandHistory` enforces its own
/// (configurable) `max_entries` on the persisted copy; this is a separate,
/// fixed cap on the in-memory copy this crate owns, since `smart-console-ui`
/// has no dependency on `Config` to read the configured value from.
const MAX_HISTORY_ENTRIES: usize = 1000;

/// Ctrl-R reverse-history-search state, active while searching (bash
/// `reverse-i-search`-style). `query` filters `App::history`; `match_index`
/// (mod the match count) selects which match is currently previewed, so
/// repeated Ctrl-R cycles to progressively older matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySearchState {
    pub query: String,
    pub match_index: usize,
}

/// Central UI state. `widgets::render` is a pure function of this struct;
/// `on_key`/`apply_session_event` are the only ways it mutates.
pub struct App {
    pub scrollback: VecDeque<String>,
    pub input: String,
    pub connection_state: ConnectionState,
    pub port_name: Option<String>,
    pub recording_path: Option<String>,
    /// `None` until a `VendorDetection` event arrives -- i.e. detection is
    /// still in progress (within the banner window). Distinct from
    /// `Some(VendorDetectionStatus::Unknown)`, which means detection
    /// finished and found nothing.
    pub vendor_status: Option<VendorDetectionStatus>,
    /// Most recent prompt state (hostname/mode), once the detected
    /// vendor's `parse_prompt` has matched a line.
    pub prompt: Option<PromptInfo>,
    /// Most recent classified events, bounded to `MAX_PARSED_EVENTS`.
    pub events: VecDeque<ParsedEvent>,
    /// Submitted commands, oldest first. Seeded at startup by the caller
    /// (from `smart_console_core::CommandHistory::entries()`) and
    /// appended to live as commands are submitted this session. Already
    /// redacted where it came from persisted history; commands submitted
    /// *this* session are kept as-typed here (the persisted copy on disk
    /// is redacted separately by `CommandHistory::append`, which this
    /// crate has no dependency on -- see design doc's crate-boundary
    /// notes) -- Ctrl-R searching this session's own just-typed commands
    /// is no more exposed than the scrollback already visible on screen.
    pub history: Vec<String>,
    pub history_search: Option<HistorySearchState>,
    /// Set by keys the spec defines but this phase doesn't implement yet
    /// (Ctrl+N/Ctrl+P/TAB/ESC) — shown in the bottom-right hints pane
    /// rather than the keypress being silently swallowed.
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
            vendor_status: None,
            prompt: None,
            events: VecDeque::new(),
            history: Vec::new(),
            history_search: None,
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

    /// Records one submitted command in the in-memory history, bounded to
    /// `MAX_HISTORY_ENTRIES` the same way `push_line`/`apply_session_event`
    /// bound `scrollback`/`events`.
    pub fn push_history(&mut self, line: String) {
        self.history.push(line);
        while self.history.len() > MAX_HISTORY_ENTRIES {
            self.history.remove(0);
        }
    }

    pub fn apply_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::RawLine(line) => self.push_line(line),
            SessionEvent::ConnectionStateChanged(state) => self.connection_state = state,
            SessionEvent::VendorDetection(status) => self.vendor_status = Some(status),
            SessionEvent::PromptChanged(prompt) => self.prompt = Some(prompt),
            SessionEvent::Parsed(event) => {
                self.events.push_back(event);
                while self.events.len() > MAX_PARSED_EVENTS {
                    self.events.pop_front();
                }
            }
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
    ///
    /// Ctrl+R enters/cycles history search regardless of mode. Ctrl+C is
    /// also handled before the search-mode dispatch below: it must stay
    /// reachable even while searching, since it's the app's only exit
    /// path (disconnect, then quit on a second press) -- routing it
    /// through `on_key_in_history_search` instead would swallow it as a
    /// literal 'c' appended to the query, trapping the user in search
    /// mode with no way to disconnect or quit. Entering search also
    /// cancels it, on the same "get me out of here" principle. Every
    /// other key while searching is handled by `on_key_in_history_search`
    /// instead of the normal bindings below.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('r') {
            self.cycle_history_search();
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.history_search = None;
            if self.connection_state == ConnectionState::Disconnected {
                self.should_quit = true;
            } else {
                self.disconnect_requested = true;
            }
            return;
        }
        if self.history_search.is_some() {
            self.on_key_in_history_search(key);
            return;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => self.clear_console(),
            (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
                self.set_not_yet_implemented_hint("Ctrl+N")
            }
            (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                self.set_not_yet_implemented_hint("Ctrl+P")
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

    /// Enters history search on the first Ctrl+R; cycles to the next
    /// (older) match on subsequent presses while already searching. With
    /// no history to search, stays out of search mode entirely and shows
    /// a hint instead -- otherwise the user lands in a
    /// `(reverse-i-search)` line that can never show a match, with
    /// typing and Enter both inert and only Esc/Ctrl+C escaping it.
    fn cycle_history_search(&mut self) {
        if self.history.is_empty() {
            self.hint = Some("Ctrl+R: no command history yet".to_string());
            return;
        }
        match &mut self.history_search {
            Some(search) => search.match_index += 1,
            None => self.history_search = Some(HistorySearchState::default()),
        }
    }

    /// Key handling while `history_search` is active: typed characters
    /// edit the query, Enter accepts the current match into the input
    /// line (never auto-submits it -- same principle as
    /// `VendorPlugin::suggestions`), Esc cancels leaving `input`
    /// untouched.
    fn on_key_in_history_search(&mut self, key: KeyEvent) {
        let Some(mut search) = self.history_search.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {} // cancelled: search state dropped, input untouched
            KeyCode::Enter => {
                if let Some(matched) = self.current_history_match(&search) {
                    self.input = matched.to_string();
                }
            }
            KeyCode::Backspace => {
                search.query.pop();
                search.match_index = 0;
                self.history_search = Some(search);
            }
            KeyCode::Char(c) => {
                search.query.push(c);
                search.match_index = 0;
                self.history_search = Some(search);
            }
            _ => self.history_search = Some(search),
        }
    }

    /// History entries containing `query`, most recently submitted first.
    pub fn history_matches(&self, query: &str) -> Vec<&str> {
        self.history
            .iter()
            .rev()
            .filter(|cmd| cmd.contains(query))
            .map(String::as_str)
            .collect()
    }

    fn current_history_match(&self, search: &HistorySearchState) -> Option<&str> {
        let matches = self.history_matches(&search.query);
        if matches.is_empty() {
            return None;
        }
        Some(matches[search.match_index % matches.len()])
    }

    /// What the console pane's input line should display: the normal
    /// `> {input}` prompt, or a bash-style `(reverse-i-search)` line while
    /// history search is active.
    pub fn input_line_display(&self) -> String {
        match &self.history_search {
            Some(search) => {
                let matched = self.current_history_match(search).unwrap_or("");
                format!("(reverse-i-search)`{}': {matched}", search.query)
            }
            None => format!("> {}", self.input),
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
/// user submits with Enter; `disconnect_tx` receives a `()` each time
/// Ctrl+C requests a disconnect while connected. Neither channel is acted
/// on inside this crate -- that keeps `smart-console-ui` decoupled from
/// `smart-console-core`'s connection/serial types; the caller (the CLI)
/// owns the `ConnectionHandle` and reacts to both.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    session_events: &mut broadcast::Receiver<SessionEvent>,
    submit_tx: mpsc::UnboundedSender<String>,
    disconnect_tx: mpsc::UnboundedSender<()>,
) -> std::io::Result<()> {
    let theme = Theme::dark();
    let mut key_events = spawn_input_thread(Duration::from_millis(100));

    loop {
        terminal.draw(|frame| widgets::render(frame, app, &theme))?;

        tokio::select! {
            key = key_events.recv() => {
                match key {
                    Some(key) => {
                        if handle_key_event(app, key, &submit_tx, &disconnect_tx) {
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

/// One key event's worth of `App` mutation plus outbound-channel wiring.
/// Pulled out of `run`'s select loop so the disconnect_tx/submit_tx wiring
/// is unit-testable without a real terminal or input thread -- an
/// `App`-only test can confirm `disconnect_requested` gets set, but not
/// that anything downstream ever reads it. Returns `true` if the caller
/// should stop the loop.
fn handle_key_event(
    app: &mut App,
    key: KeyEvent,
    submit_tx: &mpsc::UnboundedSender<String>,
    disconnect_tx: &mpsc::UnboundedSender<()>,
) -> bool {
    app.on_key(key);
    if let Some(line) = app.take_pending_submit() {
        app.push_history(line.clone());
        let _ = submit_tx.send(line);
    }
    if app.disconnect_requested {
        app.disconnect_requested = false;
        let _ = disconnect_tx.send(());
    }
    app.should_quit
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn key(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_while_connected_sends_on_disconnect_tx_not_just_the_app_flag() {
        // Regression test: an earlier version set App::disconnect_requested
        // but nothing downstream ever consumed it, so Ctrl+C while
        // connected did nothing observable outside the App struct.
        let mut app = App::new();
        app.connection_state = ConnectionState::Connected;
        let (submit_tx, _submit_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel();

        let should_stop = handle_key_event(
            &mut app,
            key(KeyModifiers::CONTROL, KeyCode::Char('c')),
            &submit_tx,
            &disconnect_tx,
        );

        assert!(!should_stop);
        assert!(
            !app.disconnect_requested,
            "flag should be cleared once forwarded"
        );
        assert!(
            disconnect_rx.try_recv().is_ok(),
            "disconnect_tx should have received a signal"
        );
    }

    #[test]
    fn enter_forwards_submitted_line_through_handle_key_event() {
        let mut app = App::new();
        app.input = "show version".to_string();
        let (submit_tx, mut submit_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
        );

        assert_eq!(submit_rx.try_recv().ok(), Some("show version".to_string()));
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

    #[test]
    fn vendor_status_starts_pending_and_updates_on_detection_event() {
        let mut app = App::new();
        assert_eq!(app.vendor_status, None, "no event yet -> pending");

        app.apply_session_event(SessionEvent::VendorDetection(
            VendorDetectionStatus::Unknown,
        ));
        assert_eq!(
            app.vendor_status,
            Some(VendorDetectionStatus::Unknown),
            "Unknown must be a distinct, visible state from pending"
        );
    }

    #[test]
    fn prompt_changed_event_updates_prompt_state() {
        use smart_console_core::PromptMode;

        let mut app = App::new();
        assert_eq!(app.prompt, None);

        let prompt = PromptInfo {
            hostname: "Switch".to_string(),
            mode: PromptMode::Privileged,
            privilege: Some(15),
        };
        app.apply_session_event(SessionEvent::PromptChanged(prompt.clone()));
        assert_eq!(app.prompt, Some(prompt));
    }

    #[test]
    fn parsed_events_accumulate_and_are_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_PARSED_EVENTS + 5) {
            app.apply_session_event(SessionEvent::Parsed(ParsedEvent::Warning(format!(
                "warn {i}"
            ))));
        }
        assert_eq!(app.events.len(), MAX_PARSED_EVENTS);
        assert_eq!(
            app.events.front(),
            Some(&ParsedEvent::Warning("warn 5".to_string()))
        );
    }

    #[test]
    fn ctrl_r_enters_search_and_shows_most_recent_match_with_empty_query() {
        let mut app = App::new();
        app.history = vec!["show version".to_string(), "show ip int brief".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));

        assert!(app.history_search.is_some());
        assert_eq!(
            app.input_line_display(),
            "(reverse-i-search)`': show ip int brief"
        );
    }

    #[test]
    fn typing_in_search_mode_filters_query_not_the_input_line() {
        let mut app = App::new();
        app.history = vec!["show version".to_string(), "show ip int brief".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('v')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('e')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('r')));

        assert_eq!(
            app.input, "",
            "typed chars must not leak into input while searching"
        );
        assert_eq!(
            app.input_line_display(),
            "(reverse-i-search)`ver': show version"
        );
    }

    #[test]
    fn repeated_ctrl_r_cycles_to_next_older_match() {
        let mut app = App::new();
        app.history = vec![
            "show version".to_string(),
            "show version detail".to_string(),
        ];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert_eq!(
            app.input_line_display(),
            "(reverse-i-search)`': show version detail"
        );

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert_eq!(
            app.input_line_display(),
            "(reverse-i-search)`': show version"
        );
    }

    #[test]
    fn enter_accepts_match_into_input_without_auto_submitting() {
        let mut app = App::new();
        app.history = vec!["show version".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));

        assert!(app.history_search.is_none(), "search mode should end");
        assert_eq!(app.input, "show version");
        assert_eq!(
            app.take_pending_submit(),
            None,
            "accepting a match must never auto-submit it"
        );
    }

    #[test]
    fn esc_cancels_search_without_touching_input() {
        let mut app = App::new();
        app.history = vec!["show version".to_string()];
        app.input = "unrelated".to_string();

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));

        assert!(app.history_search.is_none());
        assert_eq!(app.input, "unrelated");
    }

    #[test]
    fn submitted_command_is_appended_to_history_via_handle_key_event() {
        let mut app = App::new();
        app.input = "show version".to_string();
        let (submit_tx, _submit_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
        );

        assert_eq!(app.history, vec!["show version".to_string()]);
    }

    #[test]
    fn ctrl_r_with_empty_history_does_not_enter_search_mode() {
        // Regression test: entering search with no history to search
        // produced a `(reverse-i-search)` line with no possible match,
        // typing and Enter both inert -- a dead end escapable only by
        // Esc/Ctrl+C. Staying out of search mode entirely is simpler and
        // matches every other "not applicable right now" key, which shows
        // a hint instead of a stuck UI state.
        let mut app = App::new();
        assert!(app.history.is_empty());

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));

        assert!(app.history_search.is_none());
        assert_eq!(app.hint.as_deref(), Some("Ctrl+R: no command history yet"));
    }

    #[test]
    fn ctrl_c_while_history_search_is_active_still_disconnects_and_cancels_search() {
        // Regression test: on_key routed every non-Ctrl+R key while
        // searching into on_key_in_history_search, which had no case for
        // Ctrl+C -- KeyCode::Char('c') fell into the generic Char arm and
        // was appended to the search query as a literal 'c'. That made
        // Ctrl+C -- the app's only disconnect/quit path -- unreachable
        // for as long as history search was open.
        let mut app = App::new();
        app.history = vec!["show version".to_string()];
        app.connection_state = ConnectionState::Connected;

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert!(app.history_search.is_some());

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(
            app.history_search.is_none(),
            "Ctrl+C must cancel search, not type 'c' into the query"
        );
        assert!(app.disconnect_requested);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_while_disconnected_quits_even_if_history_search_is_active() {
        let mut app = App::new();
        app.history = vec!["show version".to_string()];
        assert_eq!(app.connection_state, ConnectionState::Disconnected);

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(app.history_search.is_none());
        assert!(app.should_quit);
    }

    #[test]
    fn history_is_bounded_like_scrollback_and_events() {
        // Regression test: app.history grew without limit, unlike every
        // other session-lifetime buffer (scrollback, events), which are
        // all explicitly capped.
        let mut app = App::new();
        for i in 0..(MAX_HISTORY_ENTRIES + 10) {
            app.push_history(format!("cmd {i}"));
        }
        assert_eq!(app.history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(app.history.first().map(String::as_str), Some("cmd 10"));
    }
}
