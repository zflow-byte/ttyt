//! Shared vendor-agnostic types produced by `VendorPlugin` implementations.

/// Result of a successful `VendorPlugin::detect` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionResult {
    pub vendor: String,
    pub platform: String,
    pub version: Option<String>,
}

/// The CLI mode a device's prompt indicates it is currently in.
///
/// `ConfigIf`/`ConfigRouter` carry whatever submode qualifier the prompt
/// itself exposes (e.g. a routing-protocol name). Cisco's own prompts
/// (`Switch(config-if)#`, `Switch(config-router)#`) don't embed the
/// interface or protocol name, so `CiscoPlugin` always produces an empty
/// string here; vendors with more descriptive prompts can populate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMode {
    User,
    Privileged,
    Config,
    ConfigIf(String),
    ConfigRouter(String),
    Other(String),
}

/// Structured state extracted from a single prompt line by
/// `VendorPlugin::parse_prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInfo {
    pub hostname: String,
    pub mode: PromptMode,
    /// Inferred from the prompt character convention (`>` = 1, `#` = 15),
    /// not queried via `show privilege` — the prompt alone cannot reveal a
    /// custom (0-14) privilege level.
    pub privilege: Option<u8>,
}

/// A classified event surfaced from one line of device output by
/// `VendorPlugin::parse_output`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedEvent {
    Error(String),
    Warning(String),
    HostnameChanged(String),
    LinkStatus { interface: String, up: bool },
}
