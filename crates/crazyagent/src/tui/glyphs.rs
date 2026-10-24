use super::theme::Role;
use crate::project_setup::ResearchKind;

/// The meaning a status glyph carries, shared by every status word.
/// Input: a status word through `Status::of`. Output: the glyph kind and role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Needs,
    Active,
    Waiting,
    Off,
    Limited,
    Unknown,
    Suggested,
}

const STATUS_WORDS: [(Status, &[&str]); 9] = [
    (
        Status::Ok,
        &[
            "Ready",
            "Signed in",
            "Key saved",
            "Connected",
            "Done",
            "Approved",
            "Saved",
            "Replied",
        ],
    ),
    (
        Status::Fail,
        &["Failed", "Can't reach this URL", "Can't start"],
    ),
    (
        Status::Needs,
        &[
            "Not signed in",
            "Needs approval",
            "Needs a pool",
            "Needs you",
        ],
    ),
    (
        Status::Active,
        &["Running", "Thinking...", "Signing in...", "Starting"],
    ),
    (
        Status::Waiting,
        &["Queued", "Waiting for a free account", "Starts after"],
    ),
    (Status::Off, &["Not installed", "Turned off", "Stopped"]),
    (Status::Limited, &["Limited control"]),
    (Status::Unknown, &["Unknown"]),
    (Status::Suggested, &["Suggested"]),
];

impl Status {
    /// Finds the status kind of one status word from the design vocabulary.
    /// Input: the word, possibly followed by a space or period and more text.
    /// Output: its kind, or None for words that carry no status glyph.
    pub fn of(word: &str) -> Option<Self> {
        STATUS_WORDS.iter().find_map(|(status, words)| {
            words
                .iter()
                .any(|known| {
                    word == *known
                        || word
                            .strip_prefix(known)
                            .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('.'))
                })
                .then_some(*status)
        })
    }

    /// The color role that reinforces this status.
    /// Input: none. Output: success, error, warning, running or muted.
    pub fn role(self) -> Role {
        match self {
            Self::Ok => Role::Success,
            Self::Fail => Role::Error,
            Self::Needs | Self::Limited => Role::Warning,
            Self::Active => Role::Running,
            Self::Waiting | Self::Off | Self::Unknown => Role::Muted,
            Self::Suggested => Role::Accent,
        }
    }
}

/// Settings sections shown in the Settings tab strip.
/// Input: none. Output: one section; `Glyphs::section` draws its glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Accounts,
    Pools,
    Harnesses,
    Plugins,
    Memory,
    Profile,
}

/// One glyph set: Unicode, or ASCII of the same width so layouts never shift.
/// Input: chosen once from the locale by `Glyphs::detect`. Output: every glyph
/// a widget draws; widgets never use glyph string literals.
#[derive(Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub ascii: bool,
    status: [&'static str; 9],
    sections: [&'static str; 6],
    providers: [&'static str; 8],
    research: [&'static str; 5],
    pub hline: &'static str,
    pub vline: &'static str,
    pub tee: &'static str,
    pub heavy: &'static str,
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub dot: &'static str,
    pub bar: &'static str,
    pub thumb: &'static str,
    pub ellipsis: &'static str,
    pub left: &'static str,
    pub right: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    pub spinner: &'static [&'static str],
}

/// The Unicode glyph set, every glyph one cell wide.
pub static UNICODE: Glyphs = Glyphs {
    ascii: false,
    status: ["✓", "✗", "!", "▸", "◌", "⊘", "◧", "?", "✧"],
    sections: ["@", "⊞", "❯", "✦", "⁂", "◉"],
    providers: ["⬡", "⬡", "✻", "✻", "⇄", "⌁", "⌂", "•"],
    research: ["◔", "≷", "◬", "▭", "≣"],
    hline: "─",
    vline: "│",
    tee: "├",
    heavy: "━",
    top_left: "┌",
    top_right: "┐",
    bottom_left: "└",
    bottom_right: "┘",
    dot: "•",
    bar: "▌",
    thumb: "█",
    ellipsis: "…",
    left: "←",
    right: "→",
    up: "↑",
    down: "↓",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
};

/// The ASCII fallback set for terminals without a UTF-8 locale.
pub static ASCII: Glyphs = Glyphs {
    ascii: true,
    status: ["+", "x", "!", ">", "o", "-", "~", "?", "*"],
    sections: ["@", "#", "$", "&", "*", "o"],
    providers: ["O", "O", "C", "A", "R", "U", "L", "*"],
    research: ["$", "v", "^", "[", "="],
    hline: "-",
    vline: "|",
    tee: "+",
    heavy: "=",
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    dot: "*",
    bar: "|",
    thumb: "#",
    ellipsis: ".",
    left: "<",
    right: ">",
    up: "^",
    down: "v",
    spinner: &["|", "/", "-", "\\"],
};

/// Says whether the locale is UTF-8, from LC_ALL, then LC_CTYPE, then LANG.
/// Input: an environment lookup. Output: true when the first set value names
/// UTF-8; false when none is set.
pub fn unicode_locale(var: impl Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()))
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

impl Glyphs {
    /// Picks the glyph set for this terminal's locale.
    /// Input: an environment lookup. Output: Unicode for UTF-8, else ASCII.
    pub fn detect(var: impl Fn(&str) -> Option<String>) -> &'static Glyphs {
        if unicode_locale(var) {
            &UNICODE
        } else {
            &ASCII
        }
    }

    /// The glyph for one status kind.
    /// Input: the kind. Output: its one-cell glyph.
    pub fn status(&self, status: Status) -> &'static str {
        self.status[status as usize]
    }

    /// The glyph for one section.
    /// Input: the section. Output: its one-cell glyph.
    pub fn section(&self, section: Section) -> &'static str {
        self.sections[section as usize]
    }

    /// The provider mark shown before a provider name.
    /// Input: the plain provider name from the engine. Output: its glyph or
    /// monogram; an unknown provider gets a neutral dot.
    pub fn provider(&self, name: &str) -> &'static str {
        let index = match name {
            "ChatGPT / Codex" => 0,
            "OpenAI API" => 1,
            "Claude" => 2,
            "Anthropic API" => 3,
            "OpenRouter" => 4,
            "OpenAI-compatible URL" => 5,
            "Local model" => 6,
            _ => 7,
        };
        self.providers[index]
    }

    /// The glyph for one research kind.
    #[cfg_attr(not(test), expect(dead_code, reason = "drawn by the project screens"))]
    /// Input: the kind. Output: its one-cell glyph.
    pub fn research(&self, kind: ResearchKind) -> &'static str {
        let index = match kind {
            ResearchKind::MarketResearch => 0,
            ResearchKind::CompetitorResearch => 1,
            ResearchKind::DesignResearch => 2,
            ResearchKind::FrontEndDesign => 3,
            ResearchKind::TechStackResearch => 4,
        };
        self.research[index]
    }

    /// The spinner frame for one tick.
    /// Input: the tick count. Output: the frame shown at that tick.
    pub fn spin(&self, tick: u64) -> &'static str {
        self.spinner[(tick % self.spinner.len() as u64) as usize]
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
    fn locale_picks_unicode_only_for_utf8() {
        assert!(unicode_locale(env(&[("LANG", "en_US.UTF-8")])));
        assert!(unicode_locale(env(&[
            ("LC_CTYPE", "en_GB.utf8"),
            ("LANG", "C")
        ])));
        assert!(!unicode_locale(env(&[
            ("LC_ALL", "C"),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(unicode_locale(env(&[
            ("LC_ALL", ""),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(!unicode_locale(env(&[])));
        assert!(Glyphs::detect(env(&[("LANG", "POSIX")])).ascii);
        assert!(!Glyphs::detect(env(&[("LC_ALL", "de_DE.UTF-8")])).ascii);
    }

    #[test]
    fn status_words_map_to_one_glyph_kind() {
        assert_eq!(Status::of("Signed in"), Some(Status::Ok));
        assert_eq!(Status::of("Setup done."), None);
        assert_eq!(Status::of("Done 10:38"), Some(Status::Ok));
        assert_eq!(Status::of("Can't reach this URL"), Some(Status::Fail));
        assert_eq!(Status::of("Not signed in"), Some(Status::Needs));
        assert_eq!(
            Status::of("Starts after Readings API"),
            Some(Status::Waiting)
        );
        assert_eq!(Status::of("Signing in..."), Some(Status::Active));
        assert_eq!(Status::of("Not installed"), Some(Status::Off));
        assert_eq!(Status::of("Signed"), None);
        assert_eq!(Status::of("Readyish"), None);
        assert_eq!(UNICODE.status(Status::Ok), "✓");
        assert_eq!(ASCII.status(Status::Ok), "+");
        assert_eq!(Status::Fail.role(), Role::Error);
    }

    #[test]
    fn ascii_fallback_keeps_every_glyph_one_cell() {
        for set in [&UNICODE, &ASCII] {
            let shapes = [
                set.hline,
                set.vline,
                set.heavy,
                set.top_left,
                set.top_right,
                set.bottom_left,
                set.bottom_right,
                set.dot,
                set.bar,
                set.thumb,
                set.ellipsis,
                set.left,
                set.right,
                set.up,
                set.down,
            ];
            let all = set
                .status
                .iter()
                .chain(set.sections.iter())
                .chain(set.providers.iter())
                .chain(set.research.iter())
                .chain(set.spinner.iter())
                .chain(shapes.iter());
            for glyph in all {
                assert_eq!(ratatui::text::Span::raw(*glyph).width(), 1, "{glyph:?}");
                assert!(!set.ascii || glyph.is_ascii(), "{glyph:?}");
            }
        }
        assert_eq!(UNICODE.provider("Claude"), "✻");
        assert_eq!(ASCII.provider("Anthropic API"), "A");
        assert_eq!(ASCII.provider("Somewhere new"), "*");
        assert_eq!(ASCII.section(Section::Pools), "#");
        assert_eq!(UNICODE.research(ResearchKind::CompetitorResearch), "≷");
        assert_eq!(UNICODE.spin(4), "⠼");
        assert_eq!(ASCII.spin(5), "/");
    }
}
