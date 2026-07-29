pub mod connection;
pub mod scanner;

pub use connection::{ConnectionHandle, SerialTransport, open_serial_transport};
pub use scanner::{DeviceCandidate, scan};
