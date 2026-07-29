#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod config;
pub mod device;
pub mod error;
pub mod events;
pub mod model;
pub mod plugin;
pub mod session;

pub use config::Config;
pub use error::CoreError;
pub use events::{ConnectionState, EventBus, SessionEvent};
pub use model::{DetectionResult, ParsedEvent, PromptInfo, PromptMode, VendorDetectionStatus};
pub use plugin::{CiscoPlugin, ComwarePlugin, JunosPlugin, PluginRegistry, VendorPlugin};
pub use session::{Redactor, SessionRecorder};
