use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use ttyt_core::{
    ConnectionState, DangerousCommandGuard, ParsedEvent, PromptInfo, SessionEvent, SessionId,
    VendorDetectionStatus,
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

/// Command-palette state (Ctrl+P), active while open. `query` fuzzy-
/// filters `Session::palette_candidates()` (mode suggestions then recent
/// history, most-recent-first, deduplicated); `index` (mod the match
/// count) selects which match is current, so repeated Ctrl+P cycles the
/// same way repeated Ctrl-R does. Not stored as a candidate list itself,
/// same reasoning as `HistorySearchState`: recomputing live means a
/// changing `suggestions`/`history` can never leave this holding a stale
/// list, at the cost of only ever needing `query`+`index` here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub index: usize,
}

/// TAB-cycle state: `prefix` is the input text as it stood before the
/// first TAB press, `candidates` are `Session::suggestions` filtered to
/// those starting with it, `index` (mod the candidate count) is which one
/// is currently inserted into `input`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabCycleState {
    prefix: String,
    candidates: Vec<String>,
    index: usize,
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
    /// Enter was pressed on a line matching a `DangerousCommandGuard`
    /// pattern; the held `String` is that line, not yet sent. Only an
    /// explicit `y`/`Y` in `on_key_in_confirm_send` turns it into a
    /// `pending_submit` -- every other key (including switching to a
    /// different mode entirely, e.g. Ctrl+R) declines it. Declining never
    /// re-enters it into `history`: history records what was actually
    /// sent, not what was typed and abandoned.
    ConfirmSend(String),
    /// Command palette open (Ctrl+P): fuzzy search over mode suggestions
    /// plus recent history, per plan 3.2. Enter inserts the current
    /// match into `input` without submitting it, same principle as
    /// history search and TAB-autocomplete.
    Palette(PaletteState),
    /// A `--More--`-style pagination prompt arrived (`SessionEvent::
    /// PaginationPrompt`): the device is blocked waiting for exactly one
    /// keystroke, not a submitted line. Every key except Esc converts
    /// straight to a raw byte via `on_key_in_pagination` instead of going
    /// through `input`/`pending_submit` -- Esc cancels back to `Normal`
    /// without sending anything, a deliberate escape hatch for the case
    /// where the marker match was a false positive (e.g. the literal
    /// text `--More--` inside a banner) and the device isn't actually
    /// waiting for a keystroke at all.
    Pagination,
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
    /// TAB-autocomplete candidates for the current prompt context,
    /// published by the detector alongside `PromptChanged` (Task 3.3).
    /// Empty until a `Suggestions` event arrives or nothing is offered
    /// for this mode.
    pub suggestions: Vec<String>,
    /// State of an in-progress TAB-cycle (repeated TAB presses stepping
    /// through `suggestions` that matched the prefix typed before the
    /// first press). Not part of `Mode`: unlike history search or
    /// confirm-send, it never intercepts a key other than TAB itself --
    /// any other keystroke naturally invalidates it (see `on_tab`'s
    /// "still cycling?" check) without needing to swallow that key here.
    tab_cycle: Option<TabCycleState>,
    pub(crate) mode: Mode,
    /// Compiled `Config::dangerous_command_patterns`. Defaults to
    /// `DangerousCommandGuard::empty()` (never triggers) since this crate
    /// has no dependency on `Config` to read the real patterns from --
    /// the caller sets the real one via `App::set_dangerous_command_guard`
    /// once it has a `Config` in hand (see `commands.rs::connect`).
    guard: DangerousCommandGuard,
    /// Set by keys the spec defines but this phase doesn't implement yet
    /// (Ctrl+P/TAB/ESC) — shown in the bottom-right hints pane rather than
    /// the keypress being silently swallowed.
    pub hint: Option<String>,
    pub disconnect_requested: bool,
    pending_submit: Option<String>,
    /// Set by `on_key_in_pagination` when a keystroke should go straight
    /// to the device as a single raw byte instead of through the normal
    /// `input`/`pending_submit` line-submission path.
    pending_raw_key: Option<u8>,
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
            suggestions: Vec::new(),
            tab_cycle: None,
            mode: Mode::Normal,
            guard: DangerousCommandGuard::empty(),
            hint: None,
            disconnect_requested: false,
            pending_submit: None,
            pending_raw_key: None,
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
            SessionEvent::Suggestions(suggestions) => {
                self.suggestions = suggestions;
                // The prompt just changed (this is always published
                // alongside `PromptChanged`), so any TAB-cycle in
                // progress was built from the *previous* mode's
                // candidates -- stale now, cleared rather than left to
                // cycle through a list that no longer matches reality.
                self.tab_cycle = None;
            }
            SessionEvent::PaginationPrompt(prompt) => {
                // Shown in scrollback like any other output line, then
                // the very next keystroke goes straight to the device
                // instead of through the normal input line -- entering
                // this mode discards whatever else was open, same
                // principle Ctrl+R/Ctrl+P already use.
                self.push_line(prompt);
                self.mode = Mode::Pagination;
            }
        }
    }

    /// Takes the line submitted by the most recent Enter keypress, if any.
    pub fn take_pending_submit(&mut self) -> Option<String> {
        self.pending_submit.take()
    }

    /// Takes the single raw byte queued by `on_key_in_pagination`, if any.
    pub fn take_pending_raw_key(&mut self) -> Option<u8> {
        self.pending_raw_key.take()
    }

    /// ESC's behavior in `Mode::Normal` (the other modes handle their own
    /// ESC directly: `on_key_in_history_search` cancels leaving `input`
    /// untouched, `on_key_in_confirm_send` declines). With no overlay
    /// open there's nothing to close, so ESC clears the in-progress input
    /// line instead of being a permanent no-op -- "closes whatever's open,
    /// else clears what you're typing" is one consistent rule rather than
    /// a stubbed-out key waiting for a menu system no Phase 3 task builds.
    fn on_esc_in_normal_mode(&mut self) {
        if self.input.is_empty() {
            self.hint = Some("ESC: nothing to clear".to_string());
        } else {
            self.input.clear();
        }
    }

    /// Handles one key event already known to be scoped to this session
    /// (global keys -- Ctrl+C, Ctrl+N -- are intercepted by `App::on_key`
    /// before reaching here). Ctrl+R and Ctrl+P enter/cycle their overlay
    /// regardless of mode -- including declining whatever else is open,
    /// on the same principle Ctrl+C already uses: switching to a
    /// different mode always discards the old one safely rather than
    /// carrying it forward. While in a mode, every other key is handled
    /// by that mode's own handler instead of the normal bindings below.
    fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('r') {
            self.cycle_history_search();
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p') {
            self.cycle_palette();
            return;
        }
        match self.mode {
            Mode::HistorySearch(_) => {
                self.on_key_in_history_search(key);
                return;
            }
            Mode::ConfirmSend(_) => {
                self.on_key_in_confirm_send(key);
                return;
            }
            Mode::Palette(_) => {
                self.on_key_in_palette(key);
                return;
            }
            Mode::Pagination => {
                self.on_key_in_pagination(key);
                return;
            }
            Mode::Normal => {}
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => self.clear_console(),
            (_, KeyCode::Tab) => self.on_tab(),
            (_, KeyCode::Esc) => self.on_esc_in_normal_mode(),
            (_, KeyCode::Enter) => {
                if !self.input.is_empty() {
                    let cmd = std::mem::take(&mut self.input);
                    if self.guard.is_dangerous(&cmd) {
                        self.mode = Mode::ConfirmSend(cmd);
                    } else {
                        self.pending_submit = Some(cmd);
                    }
                }
            }
            (_, KeyCode::Backspace) => {
                self.input.pop();
            }
            (_, KeyCode::Char(c)) => self.input.push(c),
            _ => {}
        }
    }

    /// Key handling while `mode` is `ConfirmSend`: an explicit `y`/`Y`
    /// press, with **no** Ctrl/Alt modifier, is the only key that turns
    /// the held command into a `pending_submit`; every other key (`n`,
    /// Esc, Enter, anything) declines it and drops it -- there is no
    /// bypass path, and a declined command is not kept anywhere (not
    /// re-entered into `input`, not pushed to `history`).
    ///
    /// The modifier check matters: without it, Ctrl+Y (readline's
    /// "yank", not Ctrl+C/Ctrl+R which are already intercepted upstream)
    /// would match `KeyCode::Char('y')` and confirm-send a dangerous
    /// command from a keystroke a user meant as ordinary editing muscle
    /// memory, not confirmation.
    fn on_key_in_confirm_send(&mut self, key: KeyEvent) {
        let Mode::ConfirmSend(cmd) = std::mem::take(&mut self.mode) else {
            return;
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if plain && matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.pending_submit = Some(cmd);
        }
        // else: declined -- mode is already Normal, cmd is dropped.
    }

    /// Key handling while `mode` is `Pagination`: converts the keystroke
    /// straight to a single raw byte for `ConnectionHandle::write_raw`
    /// rather than the normal input-line-then-Enter path, since the device
    /// is blocked on a `--More--`-style prompt waiting for exactly one
    /// keystroke. `Enter` sends `\r` (0x0D), matching what a real terminal
    /// sends for that key -- `write_line`'s `\n` would not advance paging
    /// on every vendor. Esc is the one key that does *not* pass through:
    /// it cancels back to `Normal` with nothing sent, the escape hatch for
    /// a false-positive marker match (e.g. `--More--` appearing literally
    /// inside a banner) where the device was never actually waiting on a
    /// keystroke. Any other non-ASCII key (arrows, function keys, ...) is
    /// dropped the same way -- there's no sensible single byte to send.
    fn on_key_in_pagination(&mut self, key: KeyEvent) {
        self.mode = Mode::Normal;
        let byte = match key.code {
            KeyCode::Esc => None,
            KeyCode::Enter => Some(b'\r'),
            KeyCode::Char(c) if c.is_ascii() => Some(c as u8),
            _ => None,
        };
        self.pending_raw_key = byte;
    }

    /// TAB: completes `input` from `suggestions`, inserting into the
    /// input line only -- it is never auto-submitted (same security
    /// principle as `VendorPlugin::suggestions`'s own doc comment).
    /// First press filters `suggestions` to those starting with the
    /// current `input` and inserts the first match; a second consecutive
    /// press (input still exactly equal to the candidate just inserted)
    /// cycles to the next one instead of restarting the filter from
    /// scratch. Any edit in between naturally starts a fresh cycle next
    /// time, since `input` no longer equals `tab_cycle`'s stored
    /// candidate -- no explicit "cancel the cycle" handling needed for
    /// any key other than TAB itself.
    fn on_tab(&mut self) {
        let continuing = self
            .tab_cycle
            .as_ref()
            .is_some_and(|c| c.candidates.get(c.index).map(String::as_str) == Some(&self.input));

        if continuing {
            if let Some(cycle) = self.tab_cycle.as_mut() {
                cycle.index = (cycle.index + 1) % cycle.candidates.len();
                self.input = cycle.candidates[cycle.index].clone();
            }
            return;
        }

        let prefix = self.input.clone();
        let candidates: Vec<String> = self
            .suggestions
            .iter()
            .filter(|s| s.starts_with(&prefix))
            .cloned()
            .collect();

        if candidates.is_empty() {
            self.hint = Some("TAB: no matching suggestions".to_string());
            self.tab_cycle = None;
            return;
        }

        self.input = candidates[0].clone();
        self.tab_cycle = Some(TabCycleState {
            prefix,
            candidates,
            index: 0,
        });
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
            Mode::Normal | Mode::ConfirmSend(_) | Mode::Palette(_) | Mode::Pagination => {
                self.mode = Mode::HistorySearch(HistorySearchState::default())
            }
        }
    }

    /// Enters the command palette on the first Ctrl+P; cycles to the
    /// next match on subsequent presses, same convention as Ctrl-R.
    /// Candidates are `palette_candidates()` (mode suggestions + recent
    /// history); with none available, shows a hint rather than opening
    /// an overlay that could never show a match.
    fn cycle_palette(&mut self) {
        if self.palette_candidates().is_empty() {
            self.hint = Some("Ctrl+P: no commands to show yet".to_string());
            return;
        }
        match &mut self.mode {
            Mode::Palette(state) => state.index += 1,
            Mode::Normal | Mode::ConfirmSend(_) | Mode::HistorySearch(_) | Mode::Pagination => {
                self.mode = Mode::Palette(PaletteState::default())
            }
        }
    }

    /// Key handling while `mode` is `Palette`: typed characters edit the
    /// fuzzy-search query, Enter accepts the current match into the input
    /// line (never auto-submits it), Esc cancels leaving `input`
    /// untouched -- same shape as `on_key_in_history_search`.
    fn on_key_in_palette(&mut self, key: KeyEvent) {
        let Mode::Palette(mut state) = std::mem::take(&mut self.mode) else {
            return;
        };
        match key.code {
            KeyCode::Esc => {} // cancelled: mode reset to Normal, input untouched
            KeyCode::Enter => {
                if let Some(matched) = self.current_palette_match(&state) {
                    self.input = matched.to_string();
                }
            }
            KeyCode::Backspace => {
                state.query.pop();
                state.index = 0;
                self.mode = Mode::Palette(state);
            }
            KeyCode::Char(c) => {
                state.query.push(c);
                state.index = 0;
                self.mode = Mode::Palette(state);
            }
            _ => self.mode = Mode::Palette(state),
        }
    }

    /// Palette source list: this mode's TAB-suggestions first (still
    /// relevant to what you'd type next), then recent history most-
    /// recent-first, deduplicated so a command appearing in both isn't
    /// offered twice.
    fn palette_candidates(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for candidate in self
            .suggestions
            .iter()
            .map(String::as_str)
            .chain(self.history.iter().rev().map(String::as_str))
        {
            if seen.insert(candidate) {
                out.push(candidate);
            }
        }
        out
    }

    /// `palette_candidates()` fuzzy-filtered by `query` (case-insensitive
    /// subsequence match -- every character of `query`, in order, appears
    /// somewhere in the candidate; not necessarily contiguous).
    fn palette_matches(&self, query: &str) -> Vec<&str> {
        self.palette_candidates()
            .into_iter()
            .filter(|c| fuzzy_matches(c, query))
            .collect()
    }

    fn current_palette_match(&self, state: &PaletteState) -> Option<&str> {
        let matches = self.palette_matches(&state.query);
        if matches.is_empty() {
            return None;
        }
        Some(matches[state.index % matches.len()])
    }

    pub(crate) fn is_palette_active(&self) -> bool {
        matches!(self.mode, Mode::Palette(_))
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

    pub(crate) fn is_confirm_send_active(&self) -> bool {
        matches!(self.mode, Mode::ConfirmSend(_))
    }

    pub(crate) fn is_pagination_active(&self) -> bool {
        matches!(self.mode, Mode::Pagination)
    }

    /// What the console pane's input line should display: the normal
    /// `> {input}` prompt, a bash-style `(reverse-i-search)` line while
    /// history search is active, or a y/N confirmation prompt while a
    /// dangerous command awaits confirmation.
    pub fn input_line_display(&self) -> String {
        match &self.mode {
            Mode::HistorySearch(search) => {
                let matched = self.current_history_match(search).unwrap_or("");
                format!("(reverse-i-search)`{}': {matched}", search.query)
            }
            Mode::ConfirmSend(cmd) => format!("Send \"{cmd}\" to the device? [y/N] "),
            Mode::Palette(state) => {
                let matched = self.current_palette_match(state).unwrap_or("");
                format!("(palette)`{}': {matched}", state.query)
            }
            Mode::Pagination => {
                "-- More -- press space/Enter to page, q to stop, Esc to cancel".to_string()
            }
            Mode::Normal => format!("> {}", self.input),
        }
    }
}

/// Case-insensitive subsequence fuzzy match for the command palette:
/// every character of `query`, in order, appears somewhere in
/// `candidate` (not necessarily contiguous) -- the same relaxed
/// "fzf-style" matching a fuzzy finder uses, implemented directly since
/// no fuzzy-matching crate is in the mandated dependency list. An empty
/// query matches everything (nothing typed yet -> show all candidates).
fn fuzzy_matches(candidate: &str, query: &str) -> bool {
    let query_lower = query.to_lowercase();
    let mut query_chars = query_lower.chars().peekable();
    for c in candidate.to_lowercase().chars() {
        if query_chars.peek() == Some(&c) {
            query_chars.next();
        }
    }
    query_chars.peek().is_none()
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

    /// Applies the real `Config::dangerous_command_patterns` guard to
    /// every session (each gets its own clone -- cheap, see
    /// `DangerousCommandGuard`'s doc comment). Called once by the CLI
    /// after `Config::load()`, since this crate has no dependency on
    /// `Config` itself. Sessions default to `DangerousCommandGuard::empty()`
    /// until this is called.
    pub fn set_dangerous_command_guard(&mut self, guard: DangerousCommandGuard) {
        for session in &mut self.sessions {
            session.guard = guard.clone();
        }
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
/// `raw_key_tx` is the same idea for a single passthrough byte queued by
/// `Mode::Pagination` (see `Session::take_pending_raw_key`) -- kept as its
/// own channel rather than folded into `submit_tx`'s `String` payload so
/// neither call site has to distinguish "a submitted line" from "one raw
/// byte" inside a shared payload type. `theme` is likewise the caller's
/// choice (`Theme::from_name(&config.theme)`) rather than hardcoded here,
/// for the same "no `ttyt-core` dependency" reason -- this crate resolves
/// a theme *name* to a palette, but never reads `Config` itself.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    session_events: &mut mpsc::UnboundedReceiver<(SessionId, SessionEvent)>,
    submit_tx: mpsc::UnboundedSender<(SessionId, String)>,
    disconnect_tx: mpsc::UnboundedSender<SessionId>,
    raw_key_tx: mpsc::UnboundedSender<(SessionId, u8)>,
    theme: Theme,
) -> std::io::Result<()> {
    let mut key_events = spawn_input_thread(Duration::from_millis(100));

    loop {
        terminal.draw(|frame| widgets::render(frame, app, &theme))?;

        tokio::select! {
            key = key_events.recv() => {
                match key {
                    Some(key) => {
                        if handle_key_event(app, key, &submit_tx, &disconnect_tx, &raw_key_tx) {
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
    raw_key_tx: &mpsc::UnboundedSender<(SessionId, u8)>,
) -> bool {
    app.on_key(key);
    let session = app.active_session_mut();
    let id = session.id;
    if let Some(line) = session.take_pending_submit() {
        session.push_history(line.clone());
        let _ = submit_tx.send((id, line));
    }
    if let Some(byte) = session.take_pending_raw_key() {
        let _ = raw_key_tx.send((id, byte));
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
        let (raw_key_tx, _raw_key_rx) = mpsc::unbounded_channel();

        let should_stop = handle_key_event(
            &mut app,
            key(KeyModifiers::CONTROL, KeyCode::Char('c')),
            &submit_tx,
            &disconnect_tx,
            &raw_key_tx,
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
        let (raw_key_tx, _raw_key_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
            &raw_key_tx,
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
    fn pagination_prompt_event_pushes_to_scrollback_and_enters_pagination_mode() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        assert_eq!(
            app.active_session().scrollback.back().map(String::as_str),
            Some("--More--"),
            "the prompt text should be visible in scrollback like any other output"
        );
        assert!(app.active_session().is_pagination_active());
    }

    #[test]
    fn space_key_in_pagination_mode_queues_a_raw_space_byte_and_exits_the_mode() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char(' ')));
        assert_eq!(app.active_session_mut().take_pending_raw_key(), Some(b' '));
        assert!(!app.active_session().is_pagination_active());
    }

    #[test]
    fn enter_key_in_pagination_mode_queues_carriage_return_not_linefeed() {
        // A real terminal responding to `--More--` sends '\r' for Enter,
        // not the '\n' write_line appends for a submitted command -- some
        // vendors don't treat '\n' as advancing the page.
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
        assert_eq!(app.active_session_mut().take_pending_raw_key(), Some(b'\r'));
    }

    #[test]
    fn q_key_in_pagination_mode_queues_the_literal_byte() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('q')));
        assert_eq!(app.active_session_mut().take_pending_raw_key(), Some(b'q'));
    }

    #[test]
    fn esc_in_pagination_mode_cancels_without_queuing_any_byte() {
        // The escape hatch for a false-positive marker match (e.g. the
        // literal text "--More--" inside a banner): the device was never
        // actually waiting on a keystroke, so nothing should be sent.
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));
        assert_eq!(app.active_session_mut().take_pending_raw_key(), None);
        assert!(!app.active_session().is_pagination_active());
    }

    #[test]
    fn ctrl_c_still_disconnects_while_pagination_mode_is_active() {
        // Ctrl+C must stay reachable even during pagination passthrough --
        // otherwise a false-positive marker match would leave the user
        // with no way out except typing a byte straight at a production
        // device. Mirrors the same escape-hatch guarantee already proven
        // for history search/confirm-send/palette.
        let mut app = App::new();
        app.active_session_mut().connection_state = ConnectionState::Connected;
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(app.active_session().disconnect_requested);
        assert!(!app.active_session().is_pagination_active());
    }

    #[test]
    fn input_line_display_shows_the_pagination_hint_while_active() {
        let mut app = App::new();
        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::PaginationPrompt("--More--".to_string()),
        );
        assert!(
            app.active_session()
                .input_line_display()
                .starts_with("-- More --")
        );
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
    fn ctrl_p_with_no_candidates_shows_a_hint_instead_of_a_no_op() {
        let mut app = App::new();
        assert!(app.active_session().suggestions.is_empty());
        assert!(app.active_session().history.is_empty());
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("Ctrl+P: no commands to show yet")
        );
        assert!(!app.active_session().is_palette_active());
    }

    #[test]
    fn esc_in_normal_mode_clears_the_input_line() {
        let mut app = App::new();
        app.active_session_mut().input = "show ver".to_string();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));
        assert_eq!(app.active_session().input, "");
    }

    #[test]
    fn esc_with_empty_input_and_no_overlay_shows_a_hint_instead_of_a_no_op() {
        let mut app = App::new();
        assert_eq!(app.active_session().input, "");
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("ESC: nothing to clear")
        );
    }

    #[test]
    fn tab_with_no_suggestions_shows_a_hint_instead_of_a_no_op() {
        let mut app = App::new();
        assert!(app.active_session().suggestions.is_empty());
        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("TAB: no matching suggestions")
        );
        assert_eq!(app.active_session().input, "");
    }

    #[test]
    fn tab_completes_from_the_typed_prefix() {
        let mut app = App::new();
        app.active_session_mut().suggestions =
            vec!["show version".to_string(), "show ip interface".to_string()];
        app.active_session_mut().input = "show v".to_string();

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));

        assert_eq!(app.active_session().input, "show version");
    }

    #[test]
    fn repeated_tab_cycles_through_matching_candidates() {
        let mut app = App::new();
        app.active_session_mut().suggestions =
            vec!["show version".to_string(), "show vlan".to_string()];
        app.active_session_mut().input = "show v".to_string();

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "show version");

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "show vlan");

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(
            app.active_session().input,
            "show version",
            "should wrap back to the first candidate"
        );
    }

    #[test]
    fn typing_after_a_tab_completion_starts_a_fresh_cycle_next_tab() {
        let mut app = App::new();
        app.active_session_mut().suggestions =
            vec!["show version".to_string(), "show vlan".to_string()];
        app.active_session_mut().input = "show v".to_string();

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "show version");

        // Editing after the completion must not leave the old cycle
        // active -- the next TAB should re-filter from the new input,
        // not silently continue cycling the previous candidate list.
        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('!')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(
            app.active_session().hint.as_deref(),
            Some("TAB: no matching suggestions")
        );
    }

    #[test]
    fn tab_never_auto_submits() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["show version".to_string()];
        app.active_session_mut().input = "show".to_string();

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));

        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            None,
            "TAB must only insert into the input line, never submit it"
        );
    }

    #[test]
    fn suggestions_event_replaces_the_list_and_clears_a_stale_tab_cycle() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["show version".to_string()];
        app.active_session_mut().input = "show".to_string();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "show version");

        app.apply_session_event(
            SessionId::new(0),
            SessionEvent::Suggestions(vec!["interface ".to_string()]),
        );

        assert_eq!(
            app.active_session().suggestions,
            vec!["interface ".to_string()]
        );
        // Next TAB must re-filter against the new list, not continue
        // cycling candidates computed for the mode that just ended.
        app.active_session_mut().input = String::new();
        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "interface ");
    }

    #[test]
    fn tab_completing_to_a_dangerous_command_then_enter_asks_for_confirmation() {
        // Closes the loop between Task 3.4 (confirm-before-send) and
        // Task 3.3 (autocomplete): the guard checks whatever text is in
        // `input` when Enter is pressed, regardless of whether it was
        // typed by hand or inserted via TAB -- plan 3.4 requires covering
        // both, so this is asserted rather than assumed.
        let mut app = app_with_dangerous_pattern("reload");
        app.active_session_mut().suggestions = vec!["reload".to_string()];
        app.active_session_mut().input = "rel".to_string();

        app.on_key(key(KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(app.active_session().input, "reload");

        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));

        assert!(app.active_session().is_confirm_send_active());
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
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
        let (raw_key_tx, _raw_key_rx) = mpsc::unbounded_channel();

        handle_key_event(
            &mut app,
            key(KeyModifiers::NONE, KeyCode::Enter),
            &submit_tx,
            &disconnect_tx,
            &raw_key_tx,
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

    fn app_with_dangerous_pattern(pattern: &str) -> App {
        let mut app = App::new();
        app.set_dangerous_command_guard(
            DangerousCommandGuard::new(&[pattern.to_string()]).unwrap(),
        );
        app
    }

    fn type_and_enter(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyModifiers::NONE, KeyCode::Char(c)));
        }
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));
    }

    #[test]
    fn enter_on_a_dangerous_command_asks_for_confirmation_instead_of_submitting() {
        let mut app = app_with_dangerous_pattern("reload");
        type_and_enter(&mut app, "reload");

        assert!(app.active_session().is_confirm_send_active());
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            None,
            "must not submit before confirmation"
        );
        assert_eq!(
            app.active_session().input_line_display(),
            "Send \"reload\" to the device? [y/N] "
        );
    }

    #[test]
    fn y_confirms_and_submits_the_dangerous_command_exactly_once() {
        let mut app = app_with_dangerous_pattern("reload");
        type_and_enter(&mut app, "reload");

        app.on_key(key(KeyModifiers::NONE, KeyCode::Char('y')));

        assert!(!app.active_session().is_confirm_send_active());
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            Some("reload".to_string())
        );
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            None,
            "must only submit once"
        );
    }

    #[test]
    fn any_key_other_than_y_declines_and_never_submits_or_enters_history() {
        for declining_key in [
            key(KeyModifiers::NONE, KeyCode::Char('n')),
            key(KeyModifiers::NONE, KeyCode::Char('N')),
            key(KeyModifiers::NONE, KeyCode::Esc),
            key(KeyModifiers::NONE, KeyCode::Enter),
            // Regression test: Ctrl+Y (readline "yank") matches
            // `KeyCode::Char('y')` just like a plain 'y' does, and an
            // earlier version checked only `key.code`, so Ctrl+Y
            // confirmed and sent the dangerous command. Only a plain
            // (no Ctrl/Alt) 'y'/'Y' may confirm.
            key(KeyModifiers::CONTROL, KeyCode::Char('y')),
        ] {
            let mut app = app_with_dangerous_pattern("reload");
            type_and_enter(&mut app, "reload");

            app.on_key(declining_key);

            assert!(!app.active_session().is_confirm_send_active());
            assert_eq!(app.active_session_mut().take_pending_submit(), None);
            assert!(
                app.active_session().history.is_empty(),
                "a declined command must not be recorded in history -- \
                 history reflects what was actually sent, not what was typed"
            );
        }
    }

    #[test]
    fn ctrl_c_declines_a_pending_confirm_send_rather_than_bypassing_it() {
        // "No bypass path" (Phase 3 exit criterion) means every route out
        // of ConfirmSend must either send explicitly via 'y' or decline --
        // never silently send. Ctrl+C already cancels every other mode on
        // the same principle; this confirms it does so here too instead
        // of being some kind of accidental confirm shortcut.
        let mut app = app_with_dangerous_pattern("reload");
        type_and_enter(&mut app, "reload");

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(!app.active_session().is_confirm_send_active());
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
    }

    #[test]
    fn ctrl_r_declines_a_pending_confirm_send_and_opens_history_search() {
        let mut app = app_with_dangerous_pattern("reload");
        app.active_session_mut().history = vec!["show version".to_string()];
        type_and_enter(&mut app, "reload");

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('r')));

        assert!(!app.active_session().is_confirm_send_active());
        assert!(app.active_session().is_history_search_active());
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
    }

    #[test]
    fn non_matching_command_still_submits_immediately() {
        let mut app = app_with_dangerous_pattern("reload");
        type_and_enter(&mut app, "show version");

        assert!(!app.active_session().is_confirm_send_active());
        assert_eq!(
            app.active_session_mut().take_pending_submit(),
            Some("show version".to_string())
        );
    }

    #[test]
    fn fuzzy_matches_is_a_case_insensitive_ordered_subsequence() {
        assert!(fuzzy_matches("show version", "sv"));
        assert!(fuzzy_matches("show version", "SHOWVER"));
        assert!(fuzzy_matches("configure terminal", "cfgterm"));
        assert!(fuzzy_matches("anything", ""));
        assert!(
            !fuzzy_matches("version", "rev"),
            "order matters: 'r' only appears after 'e' in \"version\""
        );
        assert!(!fuzzy_matches("show version", "xyz"));
    }

    #[test]
    fn ctrl_p_opens_palette_over_suggestions_and_history_combined() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["show version".to_string()];
        app.active_session_mut().history = vec!["configure terminal".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));

        assert!(app.active_session().is_palette_active());
        // Empty query -> first candidate is suggestions[0].
        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`': show version"
        );
    }

    #[test]
    fn typing_in_palette_fuzzy_filters_query_not_the_input_line() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["show version".to_string()];
        app.active_session_mut().history = vec!["configure terminal".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        // "cfgterm" as a subsequence fuzzy-matches "configure terminal"
        // but not "show version".
        for c in "cfgterm".chars() {
            app.on_key(key(KeyModifiers::NONE, KeyCode::Char(c)));
        }

        assert_eq!(app.active_session().input, "", "must not leak into input");
        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`cfgterm': configure terminal"
        );
    }

    #[test]
    fn repeated_ctrl_p_cycles_to_the_next_match() {
        let mut app = App::new();
        app.active_session_mut().suggestions =
            vec!["show version".to_string(), "show vlan".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`': show version"
        );
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`': show vlan"
        );
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`': show version",
            "should wrap back to the first candidate"
        );
    }

    #[test]
    fn enter_accepts_palette_match_without_auto_submitting() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["configure terminal".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Enter));

        assert!(!app.active_session().is_palette_active());
        assert_eq!(app.active_session().input, "configure terminal");
        assert_eq!(app.active_session_mut().take_pending_submit(), None);
    }

    #[test]
    fn esc_cancels_palette_without_touching_input() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["configure terminal".to_string()];
        app.active_session_mut().input = "unrelated".to_string();

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        app.on_key(key(KeyModifiers::NONE, KeyCode::Esc));

        assert!(!app.active_session().is_palette_active());
        assert_eq!(app.active_session().input, "unrelated");
    }

    #[test]
    fn ctrl_c_declines_a_pending_palette_rather_than_bypassing_confirm_guard() {
        let mut app = app_with_dangerous_pattern("reload");
        app.active_session_mut().suggestions = vec!["reload".to_string()];
        app.active_session_mut().connection_state = ConnectionState::Connected;

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        assert!(app.active_session().is_palette_active());

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('c')));

        assert!(!app.active_session().is_palette_active());
        assert!(app.active_session().disconnect_requested);
    }

    #[test]
    fn duplicate_entries_between_suggestions_and_history_are_offered_once() {
        let mut app = App::new();
        app.active_session_mut().suggestions = vec!["show version".to_string()];
        app.active_session_mut().history = vec!["show version".to_string()];

        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));
        app.on_key(key(KeyModifiers::CONTROL, KeyCode::Char('p')));

        assert_eq!(
            app.active_session().input_line_display(),
            "(palette)`': show version",
            "cycling past the single deduplicated candidate should land \
             back on it, not on a hidden duplicate"
        );
    }
}
