# smart-console

A terminal-only (TUI) serial/network console for network engineers working
Cisco, Dell OS10, Aruba CX, Comware, and JunOS devices — live vendor
detection, a real-time event feed, automatic redacted session recording,
and multi-tab sessions, in the spirit of LazyGit/k9s/btop.

**Status: Phase 2 of 3 complete.** Single-session connect/console/record
works end to end against real serial hardware, with live multi-vendor
detection (Cisco, Dell OS10, Aruba CX, Comware, JunOS) and persistent
Ctrl-R command history search. Tabs, the command palette, and a config
summary view are Phase 3 (see
`outputs/2026-07-29-smart-console-plan.md` in the parent workspace).

## Build

Requires the Rust stable toolchain (`rustup` — see <https://rustup.rs> if
`cargo`/`rustc` aren't already installed).

```bash
cargo build --workspace
```

To install the `smart-console` binary onto your `PATH` (so it runs like any
other CLI tool, without `cargo run --`):

```bash
cargo install --path crates/smart-console-cli
```

Re-run the same command with `--force` after pulling changes to update the
installed binary.

### Homebrew (not yet published)

`Formula/smart-console.rb` is a Homebrew formula for this project, ready
for once the repo has a GitHub remote and a tagged release — see the
comment at the top of that file for the two placeholder fields
(`url`/`sha256`) that need filling in first. Until then it can only be
exercised locally:

```bash
brew install --build-from-source ./Formula/smart-console.rb
```

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

List serial devices on this machine:

```bash
cargo run --bin smart-console -- list-devices
```

Connect to one (macOS callout devices look like `/dev/cu.usbserial-1410`):

```bash
cargo run --bin smart-console -- connect --port /dev/cu.usbserial-1410 --baud 9600
```

`--baud` is optional; it defaults to the first entry in `config.toml`'s
`baud_candidates` (9600 unless you've changed it). Config lives at the OS
standard location (macOS:
`~/Library/Application Support/smart-console/config.toml`) and is created
with defaults on first run.

Keybindings inside the console: `Ctrl+C` disconnect (quits if already
disconnected — there's no other quit key yet), `Ctrl+L` clear, `Ctrl+R`
reverse history search (bash `reverse-i-search` style: repeated presses
cycle to older matches, typing filters, Enter accepts the match into the
input line without submitting it, Esc cancels). Both `Ctrl+C` and `Ctrl+R`
work even while a history search is active. `Ctrl+N`, `Ctrl+P`, `TAB`,
`ESC` are defined by the spec but not implemented until Phase 3 — pressing
them shows a "not yet implemented" hint instead of doing nothing silently.

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
`crates/smart-console-core/src/session/time_util.rs` for the reasoning.

## Command history

Submitted commands are persisted to `history.txt` under the same config
directory as `config.toml`, capped at `history_max_entries` (default 1000).
**The same redaction control as session recordings applies**: a command is
redacted before it's kept in memory or written to disk, so a past sensitive
command can only ever show as `[REDACTED]` in Ctrl-R search, never the
original value. The file is created `0600`.

## Known limitations (Phase 2)

- Vendor detection scans up to 40 lines after connect looking for a known
  banner; if none of the 5 plugins match in that window, vendor status
  becomes `Unknown` rather than continuing to scan indefinitely.
- Non-Cisco/Comware console message classification (errors/warnings/link
  status) is not implemented — Dell OS10, Aruba CX, and JunOS `parse_output`
  return no classified events. This is a deliberate scope decision (see
  `changes.log`'s Phase 2 entry), not an oversight: their console message
  formats weren't confirmed precisely enough to classify without risking
  misclassifying an unrelated line.
- No tabs, split-view, command palette, autocomplete, session replay, or
  config summary view yet (Phase 3).
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
- Linux and Windows are not supported yet (macOS only); this is explicit
  Phase 3 groundwork, not a bug.
