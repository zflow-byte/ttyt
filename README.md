# smart-console

A terminal-only (TUI) serial/network console for network engineers working
Cisco, Dell OS10, Aruba CX, Comware, and JunOS devices — live vendor
detection, a real-time event feed, automatic redacted session recording,
and multi-tab sessions, in the spirit of LazyGit/k9s/btop.

**Status: Phase 1 of 3 complete.** Single-session connect/console/record
works end to end against real serial hardware. Multi-vendor detection,
persistent history, tabs, and the command palette are Phase 2/3 (see
`outputs/2026-07-29-smart-console-plan.md` in the parent workspace).

## Build

Requires the Rust stable toolchain (`rustup` — see <https://rustup.rs> if
`cargo`/`rustc` aren't already installed).

```bash
cargo build --workspace
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
disconnected — there's no other quit key yet), `Ctrl+L` clear. `Ctrl+N`,
`Ctrl+P`, `Ctrl+R`, `TAB`, `ESC` are defined by the spec but not
implemented until Phase 2/3 — pressing them shows a "not yet implemented"
hint instead of doing nothing silently.

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

## Known limitations (Phase 1)

- Only Cisco IOS/IOS XE/NX-OS is detected. Dell OS10, Aruba CX, Comware,
  and JunOS plugins are Phase 2.
- The header's Vendor/Hostname/Mode fields are placeholders (`-`) until
  Phase 2 wires live plugin detection into the UI.
- No persistent command history, tabs, command palette, autocomplete, or
  replay yet (Phase 2/3).
- Verified on this development machine against this machine's own serial
  ports and via a Unix PTY pair (`serialport::TTYPort::pair()`) for the
  connection manager's read/reconnect logic, plus `ratatui::TestBackend`
  for the TUI's rendering. **Not yet verified against a physical
  USB-serial adapter or a real Cisco/Dell/Aruba/Comware/JunOS device** —
  the development environment this was built in has neither attached.
  Please test against real hardware before relying on this for
  production troubleshooting.
- Linux and Windows are not supported yet (macOS only); this is explicit
  Phase 3 groundwork, not a bug.
