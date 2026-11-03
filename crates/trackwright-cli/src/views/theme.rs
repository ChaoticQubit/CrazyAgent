use ratatui::style::{Color, Modifier, Style};

/// The styles a view draws with, one per role. A host builds its own from its palette;
/// `plain` draws without color and `color` is the standalone palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub text: Style,
    pub muted: Style,
    pub heading: Style,
    pub accent: Style,
    pub selected: Style,
    pub success: Style,
    pub warning: Style,
    pub error: Style,
}

impl Theme {
    /// The theme for terminals without color and for NO_COLOR. Output: bold headings,
    /// reversed selection, every other role unstyled.
    pub fn plain() -> Self {
        Self {
            text: Style::default(),
            muted: Style::default(),
            heading: Style::default().add_modifier(Modifier::BOLD),
            accent: Style::default(),
            selected: Style::default().add_modifier(Modifier::REVERSED),
            success: Style::default(),
            warning: Style::default(),
            error: Style::default(),
        }
    }

    /// The standalone palette in the 16 ANSI colors, so it follows the terminal's own
    /// light or dark scheme. Output: the theme.
    pub fn color() -> Self {
        Self {
            text: Style::default(),
            muted: Style::default().fg(Color::DarkGray),
            heading: Style::default().add_modifier(Modifier::BOLD),
            accent: Style::default().fg(Color::Cyan),
            selected: Style::default().add_modifier(Modifier::REVERSED),
            success: Style::default().fg(Color::Green),
            warning: Style::default().fg(Color::Yellow),
            error: Style::default().fg(Color::Red),
        }
    }
}
