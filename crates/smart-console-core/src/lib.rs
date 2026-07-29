#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod config;
pub mod error;

pub use config::Config;
pub use error::CoreError;
