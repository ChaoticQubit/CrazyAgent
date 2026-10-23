use ratatui::style::{Color, Modifier, Style};

/// How many colors the terminal shows, read once at start.
/// Input: environment variables through `depth`. Output: one of four depths;
/// a dumb terminal has no depth and the TUI does not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Ansi256,
    Ansi16,
    NoColor,
}

/// The owner's "Colors" choice in Profile.
/// Input: the saved choice. Output: which palette the theme uses; Follow macOS
/// takes the system appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colors {
    #[default]
    FollowMacos,
    Dark,
    Light,
    Terminal,
}

impl Colors {
    /// Every choice in display order with its label, for the Profile radio row.
    /// Input: none. Output: the four choices and their plain labels.
    pub const ALL: [(Colors, &'static str); 4] = [
        (Colors::FollowMacos, "Follow macOS"),
        (Colors::Dark, "Dark"),
        (Colors::Light, "Light"),
        (Colors::Terminal, "Terminal colors"),
    ];
}

/// Light or dark appearance, from the choice or from macOS.
/// Input: none. Output: which half of the Signal workshop palette applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// Every color role a widget can ask the theme for.
/// Input: none. Output: a role name; `Theme::style` maps it to a style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Heading,
    Muted,
    Accent,
    Success,
    Warning,
    Error,
    Running,
}

/// Reads the color depth from TERM, NO_COLOR and COLORTERM.
/// Input: an environment lookup. Output: the depth, or None when TERM is dumb
/// and the TUI must not start.
pub fn depth(var: impl Fn(&str) -> Option<String>) -> Option<Depth> {
    let term = var("TERM").unwrap_or_default();
    if term == "dumb" {
        return None;
    }
    if var("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return Some(Depth::NoColor);
    }
    let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return Some(Depth::TrueColor);
    }
    if term.contains("256color") {
        return Some(Depth::Ansi256);
    }
    Some(Depth::Ansi16)
}

/// Reads the macOS appearance with `defaults read -g AppleInterfaceStyle`.
/// Input: none. Output: Dark when macOS prints "Dark", otherwise Light,
/// including when the command is missing or fails.
pub fn system_appearance() -> Appearance {
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() && output.stdout.trim_ascii() == b"Dark" => {
            Appearance::Dark
        }
        _ => Appearance::Light,
    }
}

/// The resolved palette: one style per role, plus the background to paint.
/// Input: built by `Theme::new` from depth, choice and appearance. Output:
/// styles every widget uses; no widget names a color directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub paint: Option<Style>,
    pub text: Style,
    pub heading: Style,
    pub muted: Style,
    pub accent: Style,
    pub mark: Style,
    pub success: Style,
    pub warning: Style,
    pub error: Style,
    pub running: Style,
    pub selection: Style,
    pub selection_dim: Style,
    pub backdrop: Style,
}

struct Palette<C> {
    bg: C,
    fg: C,
    muted: C,
    accent: C,
    mark: C,
    success: C,
    warning: C,
    error: C,
    running: C,
    selection: (C, C),
    selection_dim: (C, C),
    backdrop: C,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const DARK_RGB: Palette<u32> = Palette {
    bg: 0x161513,
    fg: 0xECE7DD,
    muted: 0x9A958A,
    accent: 0xFF6A45,
    mark: 0xFF6A45,
    success: 0x4DB27A,
    warning: 0xD9A033,
    error: 0xF0607E,
    running: 0x4FB0D1,
    selection: (0x161513, 0xFF6A45),
    selection_dim: (0xECE7DD, 0x2E2B27),
    backdrop: 0x55514A,
};

const LIGHT_RGB: Palette<u32> = Palette {
    bg: 0xF2EEE6,
    fg: 0x1B1A17,
    muted: 0x6E6A60,
    accent: 0xB9381C,
    mark: 0xE5482D,
    success: 0x2D784C,
    warning: 0x8F620D,
    error: 0xB0244E,
    running: 0x1B7390,
    selection: (0xFFFFFF, 0xC23A1E),
    selection_dim: (0x1B1A17, 0xE1DACD),
    backdrop: 0xB9B3A7,
};

const DARK_256: Palette<u8> = Palette {
    bg: 233,
    fg: 254,
    muted: 246,
    accent: 209,
    mark: 209,
    success: 72,
    warning: 178,
    error: 204,
    running: 74,
    selection: (233, 209),
    selection_dim: (254, 236),
    backdrop: 239,
};

const LIGHT_256: Palette<u8> = Palette {
    bg: 255,
    fg: 234,
    muted: 242,
    accent: 124,
    mark: 160,
    success: 22,
    warning: 94,
    error: 125,
    running: 24,
    selection: (231, 124),
    selection_dim: (234, 253),
    backdrop: 249,
};

impl Theme {
    /// Builds the theme for one terminal and one Colors choice.
    /// Inputs: color depth, the owner's choice and the system appearance used by
    /// Follow macOS. Output: painted Signal workshop roles in truecolor and 256,
    /// the terminal's own 16 colors, or modifiers only for NO_COLOR.
    pub fn new(depth: Depth, colors: Colors, system: Appearance) -> Self {
        let appearance = match colors {
            Colors::Dark => Appearance::Dark,
            Colors::Light => Appearance::Light,
            Colors::FollowMacos | Colors::Terminal => system,
        };
        match (depth, colors) {
            (Depth::NoColor, _) => Self::plain(),
            (Depth::Ansi16, _) | (_, Colors::Terminal) => Self::sixteen(),
            (Depth::TrueColor, _) => Self::painted(
                &match appearance {
                    Appearance::Dark => DARK_RGB,
                    Appearance::Light => LIGHT_RGB,
                },
                rgb,
            ),
            (Depth::Ansi256, _) => Self::painted(
                &match appearance {
                    Appearance::Dark => DARK_256,
                    Appearance::Light => LIGHT_256,
                },
                Color::Indexed,
            ),
        }
    }

    /// Returns the style for one role.
    /// Input: the role. Output: its style in this theme.
    pub fn style(&self, role: Role) -> Style {
        match role {
            Role::Heading => self.heading,
            Role::Muted => self.muted,
            Role::Accent => self.accent,
            Role::Success => self.success,
            Role::Warning => self.warning,
            Role::Error => self.error,
            Role::Running => self.running,
        }
    }

    fn painted<C: Copy>(palette: &Palette<C>, color: impl Fn(C) -> Color) -> Self {
        let fg = |value: C| Style::new().fg(color(value));
        let bg = color(palette.bg);
        Self {
            paint: Some(Style::new().bg(bg).fg(color(palette.fg))),
            text: fg(palette.fg),
            heading: fg(palette.fg).add_modifier(Modifier::BOLD),
            muted: fg(palette.muted),
            accent: fg(palette.accent),
            mark: fg(palette.mark),
            success: fg(palette.success),
            warning: fg(palette.warning),
            error: fg(palette.error).add_modifier(Modifier::BOLD),
            running: fg(palette.running),
            selection: Style::new()
                .fg(color(palette.selection.0))
                .bg(color(palette.selection.1))
                .add_modifier(Modifier::BOLD),
            selection_dim: Style::new()
                .fg(color(palette.selection_dim.0))
                .bg(color(palette.selection_dim.1)),
            backdrop: Style::new().fg(color(palette.backdrop)).bg(bg),
        }
    }

    fn sixteen() -> Self {
        Self {
            paint: None,
            text: Style::new().fg(Color::Reset),
            heading: Style::new().add_modifier(Modifier::BOLD),
            muted: Style::new().fg(Color::DarkGray),
            accent: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            mark: Style::new().fg(Color::Red),
            success: Style::new().fg(Color::Green),
            warning: Style::new().fg(Color::Yellow),
            error: Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            running: Style::new().fg(Color::Cyan),
            selection: Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            selection_dim: Style::new().add_modifier(Modifier::UNDERLINED),
            backdrop: Style::new().fg(Color::DarkGray),
        }
    }

    fn plain() -> Self {
        let none = Style::new();
        Self {
            paint: None,
            text: none,
            heading: none.add_modifier(Modifier::BOLD),
            muted: none.add_modifier(Modifier::DIM),
            accent: none.add_modifier(Modifier::BOLD),
            mark: none,
            success: none,
            warning: none,
            error: none.add_modifier(Modifier::BOLD),
            running: none,
            selection: none.add_modifier(Modifier::REVERSED | Modifier::BOLD),
            selection_dim: none.add_modifier(Modifier::UNDERLINED),
            backdrop: none.add_modifier(Modifier::DIM),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn depth_follows_term_no_color_and_colorterm() {
        assert_eq!(
            depth(env(&[("TERM", "dumb"), ("COLORTERM", "truecolor")])),
            None
        );
        assert_eq!(
            depth(env(&[
                ("TERM", "xterm-256color"),
                ("NO_COLOR", "1"),
                ("COLORTERM", "truecolor")
            ])),
            Some(Depth::NoColor)
        );
        assert_eq!(
            depth(env(&[("TERM", "xterm-256color"), ("NO_COLOR", "")])),
            Some(Depth::Ansi256),
            "an empty NO_COLOR does not turn color off"
        );
        assert_eq!(
            depth(env(&[("TERM", "xterm"), ("COLORTERM", "24bit")])),
            Some(Depth::TrueColor)
        );
        assert_eq!(
            depth(env(&[
                ("TERM", "xterm-256color"),
                ("COLORTERM", "TrueColor")
            ])),
            Some(Depth::TrueColor)
        );
        assert_eq!(
            depth(env(&[("TERM", "screen-256color")])),
            Some(Depth::Ansi256)
        );
        assert_eq!(depth(env(&[("TERM", "xterm")])), Some(Depth::Ansi16));
        assert_eq!(depth(env(&[])), Some(Depth::Ansi16));
    }

    #[test]
    fn truecolor_paints_signal_workshop_for_each_appearance() {
        let dark = Theme::new(Depth::TrueColor, Colors::FollowMacos, Appearance::Dark);
        assert_eq!(dark.paint.unwrap().bg, Some(Color::Rgb(0x16, 0x15, 0x13)));
        assert_eq!(dark.accent.fg, Some(Color::Rgb(0xFF, 0x6A, 0x45)));
        assert_eq!(dark.selection.bg, Some(Color::Rgb(0xFF, 0x6A, 0x45)));
        let light = Theme::new(Depth::TrueColor, Colors::FollowMacos, Appearance::Light);
        assert_eq!(light.paint.unwrap().bg, Some(Color::Rgb(0xF2, 0xEE, 0xE6)));
        assert_eq!(light.accent.fg, Some(Color::Rgb(0xB9, 0x38, 0x1C)));
        assert_eq!(light.mark.fg, Some(Color::Rgb(0xE5, 0x48, 0x2D)));
        assert_eq!(
            Theme::new(Depth::TrueColor, Colors::Light, Appearance::Dark),
            light,
            "an explicit choice overrides the system appearance"
        );
        assert_eq!(
            Theme::new(Depth::TrueColor, Colors::Dark, Appearance::Light),
            dark
        );
    }

    #[test]
    fn indexed_palette_uses_the_256_mapping() {
        let dark = Theme::new(Depth::Ansi256, Colors::Dark, Appearance::Light);
        assert_eq!(dark.paint.unwrap().bg, Some(Color::Indexed(233)));
        assert_eq!(dark.error.fg, Some(Color::Indexed(204)));
        let light = Theme::new(Depth::Ansi256, Colors::FollowMacos, Appearance::Light);
        assert_eq!(light.paint.unwrap().bg, Some(Color::Indexed(255)));
        assert_eq!(light.mark.fg, Some(Color::Indexed(160)));
        assert_eq!(light.selection.fg, Some(Color::Indexed(231)));
    }

    #[test]
    fn terminal_colors_and_sixteen_colors_never_paint() {
        let sixteen = Theme::new(Depth::Ansi16, Colors::Dark, Appearance::Dark);
        assert_eq!(sixteen.paint, None);
        assert_eq!(sixteen.accent.fg, Some(Color::Red));
        assert_eq!(sixteen.error.fg, Some(Color::Magenta));
        assert_ne!(sixteen.accent.fg, sixteen.error.fg);
        assert_eq!(
            Theme::new(Depth::TrueColor, Colors::Terminal, Appearance::Light),
            sixteen
        );
        assert_eq!(
            Theme::new(Depth::Ansi256, Colors::Terminal, Appearance::Dark),
            sixteen
        );
    }

    #[test]
    fn no_color_uses_modifiers_only() {
        let plain = Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark);
        for role in [
            Role::Heading,
            Role::Muted,
            Role::Accent,
            Role::Success,
            Role::Warning,
            Role::Error,
            Role::Running,
        ] {
            let style = plain.style(role);
            assert_eq!((style.fg, style.bg), (None, None), "{role:?} has no color");
        }
        for style in [
            plain.text,
            plain.mark,
            plain.selection,
            plain.selection_dim,
            plain.backdrop,
        ] {
            assert_eq!((style.fg, style.bg), (None, None));
        }
        assert_eq!(plain.paint, None);
        assert!(plain.selection.add_modifier.contains(Modifier::REVERSED));
        assert!(plain.muted.add_modifier.contains(Modifier::DIM));
        assert!(plain.error.add_modifier.contains(Modifier::BOLD));
    }
}
