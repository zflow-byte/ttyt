# ttyt

A terminal-only (TUI) serial/network console for network engineers working
Cisco, Dell OS10, Aruba CX, Comware, and JunOS devices — live vendor
detection, a real-time event feed, automatic redacted session recording,
multi-tab sessions, a fuzzy command palette, TAB autocomplete, a
confirm-before-send guard for dangerous commands, and session replay, in
the spirit of LazyGit/k9s/btop.

**Status: Phase 3 of 3 complete.** All three planned phases are
implemented: connect/console/record against real serial hardware, live
multi-vendor detection (Cisco, Dell OS10, Aruba CX, Comware, JunOS, plus a
Fortinet recognition-only stub), persistent Ctrl-R history search,
multiple concurrent session tabs, the command palette, TAB autocomplete,
the dangerous-command confirmation guard, and session replay. See
`changes.log` for the detailed history and this file's "Known
limitations" section below for what's still rough around the edges.

## Install

### Homebrew (recommended)

```bash
brew tap zflow-byte/ttyt
brew install ttyt
```

That's the whole thing — `ttyt --help` works immediately afterward, no
`PATH` changes needed (Homebrew's own bin directory is already on `PATH`
from Homebrew's own setup). Verified end to end against the real `v0.1.0`
release: `brew install --build-from-source` and `brew test` both pass.

If Homebrew refuses the first `brew install` with "Refusing to load
formula ... from untrusted tap," that's Homebrew asking for one-time
confirmation on a third-party tap — run this once, then retry:

```bash
brew trust zflow-byte/ttyt
```

**Don't also `cargo install` this project on the same machine** (see
"Build from source" below) unless you mean to: `~/.cargo/bin` comes
before Homebrew's bin directory in most default `PATH`s, so a
cargo-installed `ttyt` silently shadows the Homebrew one — `brew install`
will say so explicitly ("shadowed by /Users/you/.cargo/bin/ttyt") if this
happens, and running `ttyt` afterward will keep running the old
cargo-installed copy instead. Fix it with `cargo uninstall ttyt-cli`.

### Build from source

Requires the Rust stable toolchain (`rustup` — see <https://rustup.rs> if
`cargo`/`rustc` aren't already installed).

```bash
cargo build --workspace
```

To install the `ttyt` binary onto your `PATH` directly with cargo instead
of Homebrew:

```bash
cargo install --path crates/ttyt-cli
```

Re-run the same command with `--force` after pulling changes to update the
installed binary.

## Verify

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass clean before any change is considered done — this is
enforced by `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`
at the root of every crate, not just a convention.

## Run

The examples below use `cargo run --bin ttyt -- <args>` since that works
from a source checkout either way. If you installed via Homebrew or
`cargo install` (see "Install" above), drop that prefix and run `ttyt
<args>` directly instead.

List serial devices on this machine:

```bash
cargo run --bin ttyt -- list-devices
```

Connect to one (macOS callout devices look like `/dev/cu.usbserial-1410`):

```bash
cargo run --bin ttyt -- connect --port /dev/cu.usbserial-1410 --baud 9600
```

Connect to several at once, opened as tabs (repeat `--port`, one baud rate
applies to all of them):

```bash
cargo run --bin ttyt -- connect --port /dev/cu.usbserial-1410 --port /dev/cu.usbserial-1420 --baud 9600
```

`--baud` is optional; it defaults to the first entry in `config.toml`'s
`baud_candidates` (9600 unless you've changed it). Config lives at the OS
standard location (macOS:
`~/Library/Application Support/ttyt/config.toml`) and is created
with defaults on first run.

Keybindings inside the console:

| Key      | Action |
|----------|--------|
| `Ctrl+C` | Disconnect the focused tab; a second `Ctrl+C` on an already-disconnected tab quits the app once every other tab is also disconnected. |
| `Ctrl+N` | Cycle to the next session tab (wraps around; a no-op hint if only one tab is open). |
| `Ctrl+P` | Open the command palette: fuzzy-filter over this mode's TAB-suggestions plus recent history, `Ctrl+P` again cycles to the next match, Enter accepts the match into the input line (does **not** submit it), Esc cancels. |
| `Ctrl+L` | Clear the console scrollback. |
| `Ctrl+R` | Reverse history search (bash `reverse-i-search` style): repeated presses cycle to older matches, typing filters, Enter accepts the match into the input line (does **not** submit it), Esc cancels. |
| `TAB`    | Autocomplete the input line from the current vendor mode's suggestion table; a second consecutive press cycles to the next candidate. Never auto-submits. |
| `ESC`    | Clears the input line if it has anything typed; otherwise shows a hint. Also cancels an active history search or command palette. |
| `Enter`  | Submit the input line. If it matches a configured dangerous-command pattern (`reload`, `write erase`, `shutdown`, ...), asks for a plain `y`/`Y` confirmation first instead of sending it immediately — any other key declines and drops it. |

`Ctrl+C`, `Ctrl+N`, and `Ctrl+P` all stay reachable even while a history
search or the palette is active, so there's always a way out of a mode
without getting stuck in it.

### Pagination (`--More--`) support

Devices that page long output (`--More--` on Cisco/Dell OS10/Aruba CX,
`---- More ----` on Comware, `---(more)---` on JunOS) block waiting for a
single keystroke rather than a submitted line — ttyt recognizes this and
switches the input line to a pass-through mode instead of the normal
type-then-Enter flow:

| Key | Action while a pagination prompt is active |
|-----|--------|
| `Space` / `Enter` / `q` / any other character | Sent to the device immediately as that one raw byte (`Enter` sends `\r`, matching what a real terminal sends — not the `\n` a submitted command gets). |
| `Esc` | Cancels back to normal input **without sending anything** — use this if the prompt turned out to be a false match (see "Known limitations" below), not a real device waiting on a keystroke. |
| `Ctrl+C` / `Ctrl+N` / `Ctrl+P` | Still work exactly as usual — the escape hatches above never get swallowed by pagination mode. |

### Session replay

Play a saved recording back through the same console UI (vendor
detection, prompt/mode parsing, scrollback) instead of connecting to a
device:

```bash
cargo run --bin ttyt -- replay ~/Library/Application\ Support/ttyt/logs/2026-07-30/143022.log --speed 10
```

`--speed` is lines per second (default 5) — the log format has no
per-line timestamps (see "Session recordings" below), so this reproduces
what happened in the session, not how quickly it originally happened.
Redacted lines replay as `[REDACTED]`, same as they were written; that's
expected, not a replay bug. There's no live device in a replay session, so
typed commands go nowhere — press `Ctrl+C` once to quit.

## Session recordings

Every connection is recorded to `logs/YYYY-MM-DD/HHMMSS.log` under the
configured log directory (default: the OS data dir's `logs/` subfolder).
**Passwords, secrets, and SNMP community strings are redacted** before
being written — the whole matching line is replaced with `[REDACTED]`,
never a partial edit. Log files are created `0600`, the day directory
`0700`.

Timestamps in log paths are UTC, not local time — there's no date/time
crate in this project's dependency list, and correct local-timezone
conversion needs one (or unsafe FFI); see
`crates/ttyt-core/src/session/time_util.rs` for the reasoning.

## Command history

Submitted commands are persisted under the same config directory as
`config.toml`, capped at `history_max_entries` (default 1000) per session.
Each `--port` gets its own history file (`history-<sanitized-port-name>.txt`)
rather than one shared file, so two concurrent tabs' writers can't
interleave into the same file. **The same redaction control as session
recordings applies**: a command is redacted before it's kept in memory or
written to disk, so a past sensitive command can only ever show as
`[REDACTED]` in Ctrl-R search or the command palette, never the original
value. Every history file is created `0600`.

## Known limitations

- Pagination-prompt detection (see "Pagination (`--More--`) support" above)
  is a plain substring match against unterminated device output, checked
  generically rather than per-vendor — deliberately so, since it must work
  even before a vendor is detected (a long banner can itself page, and the
  device would otherwise be permanently stuck waiting for a keystroke
  detection can never arrive to unblock). The tradeoff: if `--More--` (or
  one of the other markers) ever appears as literal text inside ordinary,
  non-paginated output with no trailing newline yet, ttyt will switch to
  pagination mode on a false match. `Esc` cancels back to normal input
  without sending anything if this happens.
- Vendor detection scans up to 40 lines after connect looking for a known
  banner; if none of the plugins match in that window, vendor status
  becomes `Unknown` rather than continuing to scan indefinitely. Fortinet
  is a recognition-only stub: a detected FortiGate/FortiOS device shows its
  vendor in the header but stays at hostname/mode `-` for the rest of the
  session, since `parse_prompt`/`parse_output` aren't implemented for it —
  that's the intended "recognized but unsupported" behavior, not a bug.
- Non-Cisco/Comware console message classification (errors/warnings/link
  status) is not implemented — Dell OS10, Aruba CX, and JunOS `parse_output`
  return no classified events. This is a deliberate scope decision (see
  `changes.log`'s Phase 2 entry), not an oversight: their console message
  formats weren't confirmed precisely enough to classify without risking
  misclassifying an unrelated line.
- A config-summary parser (`ttyt_core::config_summary::summarize`) exists
  and is tested, but is **not wired into the TUI** — the 4-pane layout has
  no free pane for it (the left panel is already the session-tab list), so
  it's a standalone core module for now, not a visible feature. It also
  only recognizes a generic Cisco-shaped config grammar; Comware/JunOS
  config text produces an empty-ish summary.
- Verified on this development machine against this machine's own serial
  ports and via a Unix PTY pair (`serialport::TTYPort::pair()`) for the
  connection manager's read/reconnect logic, plus `ratatui::TestBackend`
  for the TUI's rendering. **Not yet verified against a physical
  USB-serial adapter or a real Cisco/Dell/Aruba/Comware/JunOS device** —
  the development environment this was built in has neither attached. The
  4 vendor plugins added in Phase 2 (Comware/JunOS/Dell OS10/Aruba CX) are
  built from banner/prompt fixtures reconstructed from vendor
  documentation, not captured from real devices — the cross-vendor
  detection matrix test only proves the 5 fixtures don't collide with
  each other, not that they match what a real device sends. Please test
  against real hardware before relying on this for production
  troubleshooting.
- **Linux**: not supported yet, but the device scanner has groundwork for
  it (`/dev/ttyUSB*`/`/dev/ttyACM*` naming, unit-tested against fixture
  port names) — untested against a real Linux machine or VM, so still
  groundwork, not a completed port. The connection layer itself
  (`ConnectionHandle`/`open_serial_transport`) has no macOS-specific
  assumptions; it accepts whatever port name the scanner (or `--port`)
  hands it.
- **Windows**: not supported, no implementation attempted — design note
  only (Task 3.9), since actually building it is out of this phase's
  scope:
  - Serial enumeration itself is nearly free: the `serialport` crate
    already returns Windows COM ports from `available_ports()`; the
    scanner's platform filter (`is_callout_device_for`) would just need a
    `"windows"` arm, likely `true` (no filtering) since Windows doesn't
    have macOS's `cu.*`/`tty.*` duplicate-listing problem to begin with.
  - Config/log/history paths are already Windows-safe: the `directories`
    crate resolves XDG-style paths to their Windows equivalents
    (`%APPDATA%`) without any code change here.
  - **The real gap is file/directory permissions.**
    `session::secure_fs`'s `0600`/`0700` guarantee — the mechanism this
    project's security posture leans on to keep session recordings and
    command history (redacted, but still real operational data) private
    to the current user — is POSIX mode bits, `#[cfg(unix)]`-gated. The
    existing `#[cfg(not(unix))]` fallback compiles on Windows but creates
    files/directories with default, unrestricted permissions: on a shared
    multi-user Windows machine, that's a real confidentiality gap, not a
    cosmetic one. A real Windows port needs a Windows ACL equivalent
    restricting access to the current user before it can honestly claim
    the same guarantee this README makes for macOS/Linux.
  - The Unix-PTY-based integration tests (`serialport::TTYPort::pair()`)
    are already `#[cfg(unix)]`-gated, so Windows CI would build and run a
    smaller test suite, not fail to build.
