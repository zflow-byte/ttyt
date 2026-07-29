use ratatui::style::Color;

/// A single dark palette (LazyGit/k9s/Warp-inspired: subtle borders, no
/// rainbow colors). Only one theme exists today; `Theme::dark()` is the
/// explicit entry point future themes (Phase 3 `theme.toml`) will sit
/// alongside.
pub struct Theme {
    pub foreground: Color,
    pub muted: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub error: Color,
    pub warning: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
            foreground: Color::Gray,
            muted: Color::DarkGray,
            border: Color::DarkGray,
            border_focused: Color::Cyan,
            accent: Color::Cyan,
            error: Color::Red,
            warning: Color::Yellow,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
