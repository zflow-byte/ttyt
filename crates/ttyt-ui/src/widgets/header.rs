use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ttyt_core::{PromptMode, VendorDetectionStatus};

use crate::app::App;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let line = Line::from(format!(
        " {conn:?}  |  Port: {port}  |  Vendor: {vendor}  |  Hostname: {hostname}  |  Mode: {mode}  |  Rec: {rec}",
        conn = app.connection_state,
        port = app.port_name.as_deref().unwrap_or("-"),
        vendor = vendor_label(&app.vendor_status),
        hostname = app.prompt.as_ref().map_or("-", |p| p.hostname.as_str()),
        mode = app
            .prompt
            .as_ref()
            .map_or("-".to_string(), |p| mode_label(&p.mode)),
        rec = if app.recording_path.is_some() {
            "●"
        } else {
            "-"
        },
    ));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" ttyt ".bold());

    let paragraph = Paragraph::new(line)
        .block(block)
        .style(Style::default().fg(theme.foreground));
    frame.render_widget(paragraph, area);
}

/// `None` = detection still in progress (within the banner window);
/// `Some(Unknown)` = the window was exhausted with no match -- shown
/// distinctly so "still waiting" and "gave up" never look the same.
fn vendor_label(status: &Option<VendorDetectionStatus>) -> String {
    match status {
        None => "-".to_string(),
        Some(VendorDetectionStatus::Unknown) => "Unknown".to_string(),
        Some(VendorDetectionStatus::Detected(result)) => match &result.version {
            Some(version) => format!("{} {} ({version})", result.vendor, result.platform),
            None => format!("{} {}", result.vendor, result.platform),
        },
    }
}

fn mode_label(mode: &PromptMode) -> String {
    match mode {
        PromptMode::User => "user".to_string(),
        PromptMode::Privileged => "privileged".to_string(),
        PromptMode::Config => "config".to_string(),
        PromptMode::ConfigIf(name) if name.is_empty() => "config-if".to_string(),
        PromptMode::ConfigIf(name) => format!("config-if({name})"),
        PromptMode::ConfigRouter(name) if name.is_empty() => "config-router".to_string(),
        PromptMode::ConfigRouter(name) => format!("config-router({name})"),
        PromptMode::Other(raw) => raw.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ttyt_core::DetectionResult;

    #[test]
    fn vendor_label_pending_vs_unknown_vs_detected_are_all_distinct() {
        assert_eq!(vendor_label(&None), "-");
        assert_eq!(
            vendor_label(&Some(VendorDetectionStatus::Unknown)),
            "Unknown"
        );
        assert_eq!(
            vendor_label(&Some(VendorDetectionStatus::Detected(DetectionResult {
                vendor: "Cisco".to_string(),
                platform: "IOS".to_string(),
                version: Some("15.2(2)E7".to_string()),
            }))),
            "Cisco IOS (15.2(2)E7)"
        );
    }

    #[test]
    fn mode_label_shows_submode_payload_when_present() {
        assert_eq!(
            mode_label(&PromptMode::ConfigIf("GigabitEthernet1/0/1".to_string())),
            "config-if(GigabitEthernet1/0/1)"
        );
        assert_eq!(
            mode_label(&PromptMode::ConfigIf(String::new())),
            "config-if"
        );
    }
}
