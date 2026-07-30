use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use ttyt_core::{
    ConnectionState, ParsedEvent, PromptInfo, SessionEvent, SessionId, VendorDetectionStatus,
};

use crate::theme::Theme;
use crate::widgets;

/// Scrollback is bounded so a long-running session can't grow the
/// in-memory buffer without limit.
const MAX_SCROLLBACK_LINES: usize = 2000;

/// Bottom-left Events pane keeps only the most recent entries.
const MAX_PARSED_EVENTS: usize = 200;

/// `Session::history` is bounded the same way as every other unbounded-
/// input buffer here (`scrollback`, `events`) so a long session can't grow
/// it without limit. `ttyt_core::CommandHistory` enforces its own
/// (configurable) `max_entries` on the persisted copy; this is a separate,
/// fixed cap on the in-memory copy this crate owns, since `ttyt-ui`
/// has no dependency on `Config` to read the configured value from.
const MAX_HISTORY_ENTRIES: usize = 1000;

/// Ctrl-R reverse-history-search state, active while searching (bash
/// `reverse-i-search`-style). `query` filters `Session::history`;
/// `match_index` (mod the match count) selects which match is currently
/// previewed, so repeated Ctrl-R cycles to progressively older matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySearchState {
    pub query: String,
    pub match_index: usize,
}

/// A session's modal input state. At most one is active at a time --
/// modeling this as one enum rather than several `Option<T>` fields makes
/// the mutual exclusion structural instead of a convention every new
/// overlay (palette, autocomplete, confirm-send in later Phase 3 tasks)
/// has to remember to preserve. `on_key`'s global keys (Ctrl+C, Ctrl+N,
/// Ctrl+R) are checked before this dispatch, same principle either way:
/// a key that must always be reachable is handled before any mode gets a
/// chance to swallow it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    #[default]
    Normal,
    HistorySearch(HistorySearchState),
}

/// One connection/tab's worth of UI state. `App` owns a `Vec<Session>`;
/// `widgets::render` and `on_key` always act on whichever one is
/// currently focused (`App::active_session`).
pub struct Session {
    pub id: SessionId,
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
    /// (from `ttyt_core::CommandHistory::entries()`) and appended to live
    /// as commands are submitted this session. Already redacted where it
    /// came from persisted history; commands submitted *this* session are
    /// kept as-typed here (the persisted copy on disk is redacted
    /// separately by `CommandHistory::append`, which this crate has no
    /// dependency on -- see design doc's crate-boundary notes) -- Ctrl-R
    /// searching this session's own just-typed commands is no more
    /// exposed than the scrollback already visible on screen.
    pub history: Vec<String>,
    pub(crate) mode: Mode,
    /// Set by keys the spec defines but this phase doesn't implement yet
    /// (Ctrl+P/TAB/ESC) — shown in the bottom-right hints pane rather than
    /// the keypress being silently swallowed.
    pub hint: Option<String>,
    pub disconnect_requested: bool,
    pending_submit: Option<String>,
}

impl Session {
    fn new(id: SessionId) -> Self {
        Session {
            id,
            scrollback: VecDeque::new(),
            input: String::new(),
            connection_state: ConnectionState::Disconnected,
            port_name: None,
            recording_path: None,
            vendor_status: None,
            prompt: None,
            events: VecDeque::new(),
            history: Vec::new(),
            mode: Mode::Normal,
            hint: None,
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
    /// `MAX_HISTORY_ENTRIES` the same way `push_line`/`apply` bound
    /// `scrollback`/`events`.
    pub fn push_history(&mut self, line: String) {
        self.history.push(line);
        while self.history.len() > MAX_HISTORY_ENTRIES {
            self.history.remove(0);
        }
    }

    fn apply(&mut self, event: SessionEvent) {
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

    /// Handles one key event already known to be scoped to this session
    /// (global keys -- Ctrl+C, Ctrl+N -- are intercepted by `App::on_key`
    /// before reaching here). Ctrl+R enters/cycles history search
    /// regardless of mode; while searching, every other key is handled by
    /// `on_key_in_history_search` instead of the normal bindings below.
    fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('r') {
            self.cycle_history_search();
            return;
        }
        if matches!(self.mode, Mode::HistorySearch(_)) {
            self.on_key_in_history_search(key);
            return;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => self.clear_console(),
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
        match &mut self.mode {
            Mode::HistorySearch(search) => search.match_index += 1,
            Mode::Normal => self.mode = Mode::HistorySearch(HistorySearchState::default()),
        }
    }

    /// Key handling while `mode` is `HistorySearch`: typed characters edit
    /// the query, Enter accepts the current match into the input line
    /// (never auto-submits it -- same principle as
    /// `VendorPlugin::suggestions`), Esc cancels leaving `input`
    /// untouched.
    fn on_key_in_history_search(&mut self, key: KeyEvent) {
        let Mode::HistorySearch(mut search) = std::mem::take(&mut self.mode) else {
            return;
        };
        match key.code {
            KeyCode::Esc => {} // cancelled: mode reset to Normal, input untouched
            KeyCode::Enter => {
                if let Some(matched) = self.current_history_match(&search) {
                    self.input = matched.to_string();
                }
            }
            KeyCode::Backspace => {
                search.query.pop();
                search.match_index = 0;
                self.mode = Mode::HistorySearch(search);
            }
            KeyCode::Char(c) => {
                search.query.push(c);
                search.match_index = 0;
                self.mode = Mode::HistorySearch(search);
            }
            _ => self.mode = Mode::HistorySearch(search),
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

    pub(crate) fn is_history_search_active(&self) -> bool {
        matches!(self.mode, Mode::HistorySearch(_))
    }

    /// What the console pane's input line should display: the normal
    /// `> {input}` prompt, or a bash-style `(reverse-i-search)` line while
    /// history search is active.
    pub fn input_line_display(&self) -> String {
        match &self.mode {
            Mode::HistorySearch(search) => {
                let matched = self.current_history_match(search).unwrap_or("");
                format!("(reverse-i-search)`{}': {matched}", search.query)
            }
            Mode::Normal => format!("> {}", self.input),
        }
    }
}

/// Central UI state: one or more concurrent `Session`s (Phase 3 tabs) plus
/// which one is currently focused. `widgets::render` is a pure function of
/// this struct; `on_key`/`apply_session_event` are the only ways it
/// mutates.
pub struct App {
    pub sessions: Vec<Session>,
    /// Index into `sessions` of the tab currently shown/typed into. Not
    /// the same thing as a `SessionId`: this changes on every Ctrl+N,
    /// `SessionId` never does.
    pub active: usize,
    pub should_quit: bool,
}

impl App {
    /// A single-session app (the common case: `connect --port X`).
    pub fn new() -> Self {
        App::with_session_count(1)
    }

    /// `n` concurrent sessions (Phase 3: `connect --port A --port B ...`),
    /// each with its own `SessionId` equal to its fixed position in
    /// `sessions` -- stable for the process lifetime since Phase 3 never
    /// adds or removes a tab at runtime (see design doc's tabs note).
    pub fn with_session_count(n: usize) -> Self {
        let n = n.max(1);
        App {
            sessions: (0..n).map(|i| Session::new(SessionId::new(i))).collect(),
            active: 0,
            should_quit: false,
        }
    }

    pub fn active_session(&self) -> &Session {
        &self.sessions[self.active]
    }

    pub fn active_session_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active]
    }

    /// Routes an event tagged with the `SessionId` it came from to the
    /// matching session. Silently ignored if no session with that id
    /// exists (shouldn't happen in practice -- every id in play was
    /// created from `sessions`' own positions), rather than panicking on
    /// what would otherwise be an internal-wiring bug.
    pub fn apply_session_event(&mut self, id: SessionId, event: SessionEvent) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
            session.apply(event);
        }
    }

    /// Cycles the focused tab forward, wrapping around. A single-session
    /// app has nothing to cycle to; shows a hint rather than doing
    /// nothing silently, same as any other currently-inapplicable key.
    fn cycle_active_session(&mut self) {
        if self.sessions.len() <= 1 {
            self.active_session_mut().hint = Some("Ctrl+N: only one session".to_string());
            return;
        }
        self.active = (self.active + 1) % self.sessions.len();
    }

    /// Ctrl+C always disconnects the *focused* tab if it's not already
    /// disconnected. If it is, the app quits only once every other
    /// session has also reached `Disconnected` -- otherwise a lone
    /// already-disconnected tab would let Ctrl+C tear down the whole app
    /// out from under sessions that are still live. Cancels the focused
    /// tab's mode too, on the same "get me out of here" principle Ctrl+C
    /// already had in the single-session phases.
    fn handle_ctrl_c(&mut self) {
        let others_still_up =
            self.sessions.iter().enumerate().any(|(i, s)| {
                i != self.active && s.connection_state != ConnectionState::Disconnected
            });

        let session = self.active_session_mut();
        session.mode = Mode::Normal;
        if session.connection_state != ConnectionState::Disconnected {
            session.disconnect_requested = true;
        } else if !others_still_up {
            self.should_quit = true;
        }
        // else: this tab is already disconnected but another tab isn't --
        // no-op, matching the doc comment above.
    }

    /// Handles one key event. Global keys (Ctrl+C, Ctrl+N) are checked
    /// before anything session-scoped: they must stay reachable
    /// regardless of the focused session's mode, for the same reason
    /// Ctrl+C had to be checked before history search's own key dispatch
    /// in Phase 2 -- routing it through a mode's handler risks the mode
    /// swallowing it as ordinary input instead.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('n') {
            self.cycle_active_session();
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.handle_ctrl_c();
            return;
        }
        self.active_session_mut().on_key(key);
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
/// `ttyt_core::device::connection`.
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

/// Drives the TUI until the user quits. `session_events` delivers events
/// tagged with the `SessionId` they came from -- the caller (the CLI) is
/// responsible for fanning each session's own `broadcast::Receiver` into
/// this single tagged channel (see `commands.rs`'s per-session forwarder
/// tasks); `tokio::select!` can't fan over a `Vec` of receivers directly,
/// and pulling in `StreamMap`/`SelectAll` would mean an undeclared crate
/// (see design doc's mandated-crate-list constraint). `submit_tx`/
/// `disconnect_tx` are likewise tagged with the `SessionId` whose input
/// line was submitted / whose Ctrl+C requested a disconnect. Neither
/// inbound nor outbound channel is acted on inside this crate -- that
/// keeps `ttyt-ui` decoupled from `ttyt-core`'s connection/serial types;
/// the caller owns every session's `ConnectionHandle` and reacts to both.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    session_events: &mut mpsc::UnboundedReceiver<(SessionId, SessionEvent)>,
    submit_tx: mpsc::UnboundedSender<(SessionId, String)>,
    disconnect_tx: mpsc::UnboundedSender<SessionId>,
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
                    Some((id, event)) => app.apply_session_event(id, event),
                    None => return Ok(()), // every session's forwarder task ended
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
    submit_tx: &mpsc::UnboundedSender<(SessionId, String)>,
    disconnect_tx: &mpsc::UnboundedSender<SessionId>,
) -> bool {
    app.on_key(key);
    let session = app.active_session_mut();
    let id = session.id;
    if let Some(line) = session.take_pending_submit() {
        session.push_history(line.clone());
        let _ = submit_tx.send((id, line));
    }
    if session.disconnect_requested {
        session.disconnect_requested = false;
        let _ = disconnect_tx.send(id);
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
        // Regression test: an earlier version set Session::disconnect_requested
        // but nothing downstream ever consumed it, so Ctrl+C while
        // connected did nothing observable outside the App struct.
        let mut app = App::new();
        app.active_session_mut().connection_state = ConnectionState::Connected;
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
            !app.active_session().disconnect_requested,
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
        app.active_session_mut().input = "show version".to_string();
        let (submit_tx, mut submit_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
        );

        assert_eq!(
            submit_rx.try_recv().ok(),
            Some((SessionId::new(0), "show version".to_string()))
        );
    }

    #[test]
    fn typing_and_backspace_edit_the_input_line() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('h')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('i')));
        assert_eq!(app.active_session().input, "hi");
        app.on_key(key(KeyModifiers::NONE, KeyCode::Backspace));
        assert_eq!(app.active_session().input, "h");
    }

    #[test]
    fn enter_submits_and_clears_the_input_line() {
        let mut app = App::new();
        for c in "show version".chars() {
            app.on_key(key(KeyModifiers::NONE, KeyCode::Char(c)));
        }
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
        assert_eq!(app.active_session().input, "");
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            Some("show version".to_string())
        );
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
    }

    #[test]
    fn enter_on_empty_input_does_not_submit() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
    }

    #[test]
    fn ctrl_l_clears_console_scrollback() {
        let mut app = App::new();
        app.active_session_mut()
            .push_line("some output".to_string());
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('l')));
        assert!(app.active_session().scrollback.is_empty());
    }

    #[test]
    fn ctrl_c_requests_disconnect_when_connected() {
        let mut app = App::new();
        app.active_session_mut().connection_state = ConnectionState::Connected;
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(app.active_session().disconnect_requested);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_when_already_disconnected() {
        let mut app = App::new();
        assert_eq!(
            app.active_session().connection_state,
            ConnectionState::Disconnected
        );
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_on_disconnected_tab_does_not_quit_while_another_tab_is_still_up() {
        let mut app = App::with_session_count(2);
        app.sessions[1].connection_state = ConnectionState::Connected;
        assert_eq!(app.active, 0);
        assert_eq!(
            app.active_session().connection_state,
            ConnectionState::Disconnected
        );

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(
            !app.should_quit,
            "another session is still up -- Ctrl+C on this already-disconnected \
             tab must not tear down the whole app"
        );
        assert!(!app.active_session().disconnect_requested);
    }

    #[test]
    fn ctrl_n_cycles_the_active_session_and_wraps_around() {
        let mut app = App::with_session_count(3);
        assert_eq!(app.active, 0);
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert_eq!(app.active, 1);
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert_eq!(app.active, 2);
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert_eq!(app.active, 0, "should wrap back to the first tab");
    }

    #[test]
    fn ctrl_n_with_a_single_session_shows_a_hint_instead_of_a_no_op() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert_eq!(app.active, 0);
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("Ctrl+N: only one session")
        );
    }

    #[test]
    fn each_session_gets_a_distinct_stable_session_id() {
        let app = App::with_session_count(3);
        assert_eq!(app.sessions[0].id, SessionId::new(0));
        assert_eq!(app.sessions[1].id, SessionId::new(1));
        assert_eq!(app.sessions[2].id, SessionId::new(2));
    }

    #[test]
    fn apply_session_event_routes_to_the_matching_session_only() {
        let mut app = App::with_session_count(2);
        app.apply_session_event(
            SessionId::new(1),
            SessionEvent::RawLine("only for session 1".to_string()),
        );
        assert!(app.sessions[0].scrollback.is_empty());
        assert_eq!(
            app.sessions[1].scrollback.back().map(String::as_str),
            Some("only for session 1")
        );
    }

    #[test]
    fn unimplemented_keys_set_a_visible_hint_instead_of_being_silently_dropped() {
        let mut app = App::new();
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("Ctrl+P: not yet implemented")
        );

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("TAB: not yet implemented")
        );

        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("ESC: not yet implemented")
        );
    }

    #[test]
    fn raw_line_session_events_append_to_scrollback() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::RawLine("Switch> ".to_string()),
        );
        assert_eq!(
            app.active_session().scrollback.back().map(String::as_str),
            Some("Switch> ")
        );
    }

    #[test]
    fn connection_state_changed_event_updates_header_state() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::ConnectionStateChanged(ConnectionState::Connected),
        );
        assert_eq!(
            app.active_session().connection_state,
            ConnectionState::Connected
        );
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_SCROLLBACK_LINES + 10) {
            app.active_session_mut().push_line(format!("line {i}"));
        }
        assert_eq!(app.active_session().scrollback.len(), MAX_SCROLLBACK_LINES);
        assert_eq!(
            app.active_session().scrollback.front().map(String::as_str),
            Some("line 10")
        );
    }

    #[test]
    fn vendor_status_starts_pending_and_updates_on_detection_event() {
        let mut app = App::new();
        assert_eq!(
            app.active_session().vendor_status,
            None,
            "no event yet -> pending"
        );

        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::VendorDetection(VendorDetectionStatus::Unknown),
        );
        assert_eq!(
            app.active_session().vendor_status,
            Some(VendorDetectionStatus::Unknown),
            "Unknown must be a distinct, visible state from pending"
        );
    }

    #[test]
    fn prompt_changed_event_updates_prompt_state() {
        use ttyt_core::PromptMode;

        let mut app = App::new();
        assert_eq!(app.active_session().prompt, None);

        let prompt = PromptInfo {
            hostname: "Switch".to_string(),
            mode: PromptMode::Privileged,
            privilege: Some(15),
        };
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PromptChanged(prompt.clone()),
        );
        assert_eq!(app.active_session().prompt, Some(prompt));
    }

    #[test]
    fn parsed_events_accumulate_and_are_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_PARSED_EVENTS + 5) {
            app.apply_session_event(
                SessionId::new(0),
                SessionEvent::Parsed(ParsedEvent::Warning(format!("warn {i}"))),
            );
        }
        assert_eq!(app.active_session().events.len(), MAX_PARSED_EVENTS);
        assert_eq!(
            app.active_session().events.front(),
            Some(&ParsedEvent::Warning("warn 5".to_string()))
        );
    }

    #[test]
    fn ctrl_r_enters_search_and_shows_most_recent_match_with_empty_query() {
        let mut app = App::new();
        app.active_session_mut().history =
            vec!["show version".to_string(), "show ip int brief".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));

        assert!(app.active_session().is_history_search_active());
        assert_eq!(
            app.active_session().input_line_display(),
            "(reverse-i-search)`': show ip int brief"
        );
    }

    #[test]
    fn typing_in_search_mode_filters_query_not_the_input_line() {
        let mut app = App::new();
        app.active_session_mut().history =
            vec!["show version".to_string(), "show ip int brief".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('v')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('e')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('r')));

        assert_eq!(
            app.active_session().input,
            "",
            "typed chars must not leak into input while searching"
        );
        assert_eq!(
            app.active_session().input_line_display(),
            "(reverse-i-search)`ver': show version"
        );
    }

    #[test]
    fn repeated_ctrl_r_cycles_to_next_older_match() {
        let mut app = App::new();
        app.active_session_mut().history = vec![
            "show version".to_string(),
            "show version detail".to_string(),
        ];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert_eq!(
            app.active_session().input_line_display(),
            "(reverse-i-search)`': show version detail"
        );

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert_eq!(
            app.active_session().input_line_display(),
            "(reverse-i-search)`': show version"
        );
    }

    #[test]
    fn enter_accepts_match_into_input_without_auto_submitting() {
        let mut app = App::new();
        app.active_session_mut().history = vec!["show version".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));

        assert!(
            !app.active_session().is_history_search_active(),
            "search mode should end"
        );
        assert_eq!(app.active_session().input, "show version");
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            None,
            "accepting a match must never auto-submit it"
        );
    }

    #[test]
    fn esc_cancels_search_without_touching_input() {
        let mut app = App::new();
        app.active_session_mut().history = vec!["show version".to_string()];
        app.active_session_mut().input = "unrelated".to_string();

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));

        assert!(!app.active_session().is_history_search_active());
        assert_eq!(app.active_session().input, "unrelated");
    }

    #[test]
    fn submitted_command_is_appended_to_history_via_handle_key_event() {
        let mut app = App::new();
        app.active_session_mut().input = "show version".to_string();
        let (submit_tx, _submit_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
        );

        assert_eq!(
            app.active_session().history,
            vec!["show version".to_string()]
        );
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
        assert!(app.active_session().history.is_empty());

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));

        assert!(!app.active_session().is_history_search_active());
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("Ctrl+R: no command history yet")
        );
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
        app.active_session_mut().history = vec!["show version".to_string()];
        app.active_session_mut().connection_state = ConnectionState::Connected;

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        assert!(app.active_session().is_history_search_active());

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(
            !app.active_session().is_history_search_active(),
            "Ctrl+C must cancel search, not type 'c' into the query"
        );
        assert!(app.active_session().disconnect_requested);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_while_disconnected_quits_even_if_history_search_is_active() {
        let mut app = App::new();
        app.active_session_mut().history = vec!["show version".to_string()];
        assert_eq!(
            app.active_session().connection_state,
            ConnectionState::Disconnected
        );

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(!app.active_session().is_history_search_active());
        assert!(app.should_quit);
    }

    #[test]
    fn history_is_bounded_like_scrollback_and_events() {
        // Regression test: app.history grew without limit, unlike every
        // other session-lifetime buffer (scrollback, events), which are
        // all explicitly capped.
        let mut app = App::new();
        for i in 0..(MAX_HISTORY_ENTRIES + 10) {
            app.active_session_mut().push_history(format!("cmd {i}"));
        }
        assert_eq!(app.active_session().history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(
            app.active_session().history.first().map(String::as_str),
            Some("cmd 10")
        );
    }
}
