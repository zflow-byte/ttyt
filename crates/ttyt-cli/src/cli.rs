use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ttyt",
    version,
    about = "TUI serial console for network engineers (Cisco, Dell OS10, Aruba CX, Comware, JunOS)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// List serial devices found on this machine (macOS: /dev/cu.*)
    ListDevices,
    /// Connect to one or more serial devices and open the console
    Connect {
        /// Serial port path, e.g. /dev/cu.usbserial-1410 (see
        /// `list-devices`). Repeat `--port` for multiple concurrent
        /// sessions opened as tabs; Ctrl+N cycles between them.
        #[arg(long = "port", required = true)]
        ports: Vec<String>,
        /// Baud rate, applied to every `--port` given. Defaults to the
        /// first configured candidate (9600 unless changed in
        /// config.toml).
        #[arg(long)]
        baud: Option<u32>,
    },
    /// Replay a saved session log through the same TUI a live connection
    /// uses (vendor detection, prompt parsing, scrollback), at a fixed
    /// lines-per-second rate. The log format has no per-line timestamps,
    /// so this reproduces the original session's content, not its timing.
    Replay {
        /// Path to a log file previously written under `log_dir`
        /// (`log_dir/YYYY-MM-DD/HHMMSS.log`).
        path: PathBuf,
        /// Playback rate in lines per second.
        #[arg(long, default_value_t = 5.0)]
        speed: f64,
    },
}
