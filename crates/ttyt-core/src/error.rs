use thiserror::Error;

/// Errors surfaced across `ttyt-core` (config, device I/O, session
/// handling). `ttyt-cli` wraps these in `anyhow` at the top level.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serial port error: {0}")]
    Serial(#[from] serialport::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("port not found: {0}")]
    PortNotFound(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn io_error_converts_via_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: CoreError = io_err.into();
        assert!(matches!(err, CoreError::Io(_)));
    }

    #[test]
    fn config_error_displays_message() {
        let err = CoreError::Config("bad value".to_string());
        assert_eq!(err.to_string(), "configuration error: bad value");
    }

    #[test]
    fn port_not_found_displays_port_name() {
        let err = CoreError::PortNotFound("/dev/cu.usbserial-1410".to_string());
        assert_eq!(err.to_string(), "port not found: /dev/cu.usbserial-1410");
    }
}
