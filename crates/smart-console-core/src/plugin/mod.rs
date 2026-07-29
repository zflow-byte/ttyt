mod aruba_cx;
mod cisco;
pub(crate) mod common;
mod comware;
mod dell_os10;
mod junos;

pub use aruba_cx::ArubaCxPlugin;
pub use cisco::CiscoPlugin;
pub use comware::ComwarePlugin;
pub use dell_os10::DellOs10Plugin;
pub use junos::JunosPlugin;

use crate::model::{DetectionResult, ParsedEvent, PromptInfo};

/// Vendor-specific behavior, compiled in as a trait implementation rather
/// than dynamically loaded (see the design doc's "why no separate plugin
/// crates" section: dynamic loading of native code is an RCE surface this
/// project's scale doesn't need). Adding a vendor means implementing this
/// trait and registering it in [`PluginRegistry::with_default_plugins`],
/// not touching a big if-else chain.
pub trait VendorPlugin: Send + Sync {
    /// Stable identifier, e.g. `"cisco-ios"`.
    fn id(&self) -> &'static str;

    /// Try to identify this vendor from the device's startup banner /
    /// early output. Returns `None` if this plugin doesn't recognize it.
    fn detect(&self, banner: &str) -> Option<DetectionResult>;

    /// Parse a line believed to be a shell prompt into structured state.
    /// Returns `None` if `line` isn't a prompt this plugin recognizes.
    fn parse_prompt(&self, line: &str) -> Option<PromptInfo>;

    /// Classify a line of device output into zero or more events. Most
    /// lines produce none — only lines a plugin can specifically identify
    /// (syslog-style errors/warnings, link-state changes, ...) do.
    fn parse_output(&self, line: &str) -> Vec<ParsedEvent>;

    /// Normalize a user-typed command before sending (vendor-specific
    /// aliases, etc). Default: identity.
    fn normalize_command(&self, cmd: &str) -> String {
        cmd.to_string()
    }

    /// Autocomplete/suggestion candidates for the current prompt context.
    /// The UI only ever inserts these into the input line for the human to
    /// review — never auto-submits them (see design doc's security layer).
    /// Returns an empty list until Phase 3 builds real suggestion tables.
    fn suggestions(&self, ctx: &PromptInfo) -> Vec<String> {
        let _ = ctx;
        Vec::new()
    }
}

/// Tries each registered plugin's `detect` in registration order; first
/// match wins.
pub struct PluginRegistry {
    plugins: Vec<Box<dyn VendorPlugin>>,
}

impl PluginRegistry {
    pub fn new(plugins: Vec<Box<dyn VendorPlugin>>) -> Self {
        PluginRegistry { plugins }
    }

    /// All vendor plugins implemented so far, compiled in. Phase 1: Cisco
    /// only. Phase 2 adds Dell OS10 / Aruba CX / Comware / JunOS here.
    pub fn with_default_plugins() -> Self {
        PluginRegistry::new(vec![
            Box::new(CiscoPlugin),
            Box::new(ComwarePlugin),
            Box::new(JunosPlugin),
            Box::new(DellOs10Plugin),
            Box::new(ArubaCxPlugin),
        ])
    }

    pub fn detect(&self, banner: &str) -> Option<(&dyn VendorPlugin, DetectionResult)> {
        self.plugins
            .iter()
            .find_map(|p| p.detect(banner).map(|result| (p.as_ref(), result)))
    }

    /// Looks up an already-registered plugin by [`VendorPlugin::id`] --
    /// used by the detector (Task 2.5/2.6) to re-find the plugin it
    /// detected with, across the async boundary, without holding a
    /// borrowed `&dyn VendorPlugin` alive.
    pub fn get(&self, id: &str) -> Option<&dyn VendorPlugin> {
        self.plugins
            .iter()
            .find(|p| p.id() == id)
            .map(|p| p.as_ref())
    }

    pub fn plugins(&self) -> &[Box<dyn VendorPlugin>] {
        &self.plugins
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_default_plugins()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn default_registry_detects_cisco_and_reports_nothing_for_unknown_banner() {
        let registry = PluginRegistry::with_default_plugins();

        let (plugin, result) = registry
            .detect("Cisco IOS Software, C3560 Software, Version 15.2(2)E7")
            .expect("should detect Cisco");
        assert_eq!(plugin.id(), "cisco-ios");
        assert_eq!(result.vendor, "Cisco");

        assert!(registry.detect("some unrelated banner text").is_none());
    }
}
