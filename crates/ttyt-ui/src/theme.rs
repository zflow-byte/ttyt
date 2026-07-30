use ratatui::style::Color;

/// A single dark palette (LazyGit/k9s/Warp-inspired: subtle borders, no
/// rainbow colors -- Task 3.7's finalization confirms this against the
/// design doc's constraint: `foreground`/`muted`/`border` are all
/// grayscale, and `accent`/`error`/`warning` are the only three non-gray
/// colors used anywhere, each carrying a distinct, consistent meaning
/// rather than decorating different widgets in arbitrary colors).
/// `Theme::dark()` is the explicit entry point a second theme would sit
/// alongside; see [`Theme::from_name`] for how `Config::theme`'s string
/// selects one.
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

    /// Resolves `Config::theme`'s name to a palette. Unrecognized names
    /// fall back to `dark()` rather than erroring -- a typo'd or
    /// stale-from-a-future-version theme name in `config.toml` should
    /// degrade to a working console, not refuse to start it. `ttyt-ui`
    /// takes a plain `&str` here rather than depending on
    /// `ttyt_core::Config` directly, keeping the UI crate's zero-`ttyt-core`
    /// dependency the design doc calls for.
    pub fn from_name(name: &str) -> Self {
        match name {
            "dark" => Self::dark(),
            _ => Self::dark(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn from_name_dark_matches_dark() {
        let named = Theme::from_name("dark");
        let dark = Theme::dark();
        assert_eq!(named.foreground, dark.foreground);
        assert_eq!(named.accent, dark.accent);
    }

    #[test]
    fn from_name_falls_back_to_dark_for_an_unrecognized_name() {
        let named = Theme::from_name("solarized-neon");
        let dark = Theme::dark();
        assert_eq!(named.foreground, dark.foreground);
        assert_eq!(named.border, dark.border);
    }

    /// Confirms the design doc's "no rainbow colors" constraint directly,
    /// not just by inspection: every field is either one of the three
    /// grayscale tones or one of the three meaningful accent colors --
    /// nothing else has snuck in.
    #[test]
    fn dark_palette_uses_only_grayscale_and_the_three_meaningful_accents() {
        let theme = Theme::dark();
        let grayscale = [Color::Gray, Color::DarkGray];
        let accents = [Color::Cyan, Color::Red, Color::Yellow];

        assert!(grayscale.contains(&theme.foreground));
        assert!(grayscale.contains(&theme.muted));
        assert!(grayscale.contains(&theme.border));
        assert!(accents.contains(&theme.border_focused));
        assert!(accents.contains(&theme.accent));
        assert!(accents.contains(&theme.error));
        assert!(accents.contains(&theme.warning));
    }
}
