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
    /// Connect to a serial device and open the console
    Connect {
        /// Serial port path, e.g. /dev/cu.usbserial-1410 (see `list-devices`)
        #[arg(long)]
        port: String,
        /// Baud rate. Defaults to the first configured candidate (9600
        /// unless changed in config.toml).
        #[arg(long)]
        baud: Option<u32>,
    },
}
