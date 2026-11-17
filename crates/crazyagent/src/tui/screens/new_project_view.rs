use super::new_project::{
    Control, HarnessState, PLATFORM_ORDER, Phase, PickFor, Step, Wizard, any_harness_note,
    harness_name, size_label, team_summary,
};
use super::new_project_input::{TextInput, cells, char_cells};
use super::new_project_publish::{
    GitHub, PUBLISH_DOCS, PUBLISH_LINE, PUBLISH_TOGGLE, PanelRow, Publish, SIGN_IN_WAIT,
};
use super::project_access::FULL_ACCESS_WARNING;
use super::project_markdown::sanitize;
use crate::project_setup::ResearchKind;
use crate::sandbox::policy::AgentAccess;
#[cfg(all(test, unix))]
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The style of every design color role, built once from the color depth and theme.
/// Holds one style per role from DESIGN.md section 6.2 and whether glyphs fall back to ASCII.
/// `plain` is the NO_COLOR look, which the frame tests use.
#[derive(Clone, Copy, Debug)]
pub(in crate::tui) struct Look {
    pub fg: Style,
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
    pub ascii: bool,
}

impl Look {
    /// Builds the NO_COLOR look: modifiers only, Unicode glyphs.
    /// Takes nothing.
    /// Returns the look.
    #[cfg(all(test, unix))]
    pub fn plain() -> Self {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        Self {
            fg: Style::new(),
            heading: bold,
            muted: Style::new().add_modifier(Modifier::DIM),
            accent: bold,
            mark: Style::new(),
            success: Style::new(),
            warning: Style::new(),
            error: bold,
            running: Style::new(),
            selection: Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            selection_dim: Style::new().add_modifier(Modifier::UNDERLINED),
            backdrop: Style::new().add_modifier(Modifier::DIM),
            ascii: false,
        }
    }

    /// Gives a glyph, or its one-cell ASCII fallback when the terminal is not UTF-8.
    /// Takes the Unicode glyph from DESIGN.md section 6.3.
    /// Returns the glyph to draw.
    pub fn glyph(&self, unicode: &'static str) -> &'static str {
        if !self.ascii {
            return unicode;
        }
        match unicode {
            "\u{2713}" => "+",
            "\u{2717}" => "x",
            "\u{25b8}" => ">",
            "\u{25cc}" => "o",
            "\u{2298}" => "-",
            "\u{25e7}" => "~",
            "\u{2727}" => "*",
            "\u{2500}" => "-",
            "\u{2502}" => "|",
            "\u{2501}" => "=",
            "\u{250c}" | "\u{2510}" | "\u{2514}" | "\u{2518}" => "+",
            "\u{2022}" => "*",
            "\u{258c}" => "|",
            "\u{2026}" => ".",
            "\u{25d4}" => "$",
            "\u{2277}" => "v",
            "\u{25ec}" => "^",
            "\u{25ad}" => "[",
            "\u{2263}" => "=",
            "\u{25eb}" => "=",
            "\u{00bb}" => ">",
            "\u{229b}" => "*",
            "\u{2213}" => "~",
            "\u{225f}" => "?=",
            "\u{29c9}" => "[",
            "\u{2630}" => "=",
            "\u{2190}" => "<",
            "\u{2192}" => ">",
            "\u{2191}" => "^",
            "\u{2193}" => "v",
            other => other,
        }
    }
}

/// A line under construction: styled pieces measured in terminal cells.
/// Holds owned spans so a screen can return it after the state is borrowed.
/// Converted to a ratatui `Line` for drawing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::tui) struct Row(pub Vec<Span<'static>>);

/// Makes text safe to draw on one terminal row.
/// Takes any text, including names and reports from outside CrazyAgent.
/// Returns it without escape sequences or control characters, with hidden and direction-changing characters shown as escapes; line breaks become spaces.
pub(in crate::tui) fn printable(text: String) -> String {
    if !text.chars().any(char::is_control) && !crate::memory::hidden_text(&text) {
        return text;
    }
    let clean = sanitize(&text).replace('\n', " ");
    assert!(!clean.chars().any(char::is_control) && !crate::memory::hidden_text(&clean));
    clean
}

impl Row {
    /// Starts a line with one piece.
    /// Takes the text and its style.
    /// Returns the row.
    pub fn of(text: impl Into<String>, style: Style) -> Self {
        Self(vec![Span::styled(printable(text.into()), style)])
    }

    /// Appends one piece.
    /// Takes the text and its style.
    /// Returns the longer row.
    pub fn push(mut self, text: impl Into<String>, style: Style) -> Self {
        let text = printable(text.into());
        if !text.is_empty() {
            self.0.push(Span::styled(text, style));
        }
        self
    }

    /// Appends another row.
    /// Takes the row to add.
    /// Returns the joined row.
    pub fn cat(mut self, other: Row) -> Self {
        self.0.extend(other.0);
        self
    }

    /// Measures the row.
    /// Takes the row.
    /// Returns its width in cells.
    pub fn width(&self) -> usize {
        self.0.iter().map(|span| cells(&span.content)).sum()
    }

    /// Pads the row with spaces, or cuts it with an ellipsis, to an exact width.
    /// Takes the width in cells; a zero width gives an empty row.
    /// Returns a row exactly that wide.
    pub fn pad(self, width: usize) -> Self {
        let used = self.width();
        if used <= width {
            return self.push(" ".repeat(width - used), Style::new());
        }
        if width == 0 {
            return Self::default();
        }
        let cut = self.slice(0, width - 1);
        let style = cut.0.last().map_or(Style::new(), |span| span.style);
        let cut = cut.push("\u{2026}", style);
        let short = width.saturating_sub(cut.width());
        cut.push(" ".repeat(short), Style::new())
    }

    /// Restyles every piece, keeping the text.
    /// Takes the new style.
    /// Returns the row.
    pub fn restyle(self, style: Style) -> Self {
        Self(
            self.0
                .into_iter()
                .map(|span| Span::styled(span.content, style))
                .collect(),
        )
    }

    /// Takes the cells between two columns.
    /// Takes the first column and the column after the last.
    /// Returns the cut row; a wide character crossing an edge becomes a space.
    pub fn slice(&self, from: usize, to: usize) -> Self {
        let mut out = Row::default();
        let mut column = 0;
        for span in &self.0 {
            let mut text = String::new();
            for character in span.content.chars() {
                let size = char_cells(character);
                if column >= from && column + size <= to {
                    text.push(character);
                } else if column < to && column + size > from {
                    text.push(' ');
                }
                column += size;
            }
            out = out.push(text, span.style);
        }
        out
    }

    /// Converts the row for drawing.
    /// Takes the row.
    /// Returns the ratatui line.
    pub fn line(self) -> Line<'static> {
        Line::from(self.0)
    }

    /// Gives the row's text without styles.
    /// Takes the row.
    /// Returns the text.
    #[cfg(all(test, unix))]
    pub fn text(&self) -> String {
        self.0.iter().map(|span| span.content.as_ref()).collect()
    }
}

/// Lays out a left and a right part on one row of an exact width.
/// Takes the width and both parts.
/// Returns the row; the left part is cut when both do not fit.
pub(in crate::tui) fn spread(width: usize, left: Row, right: Row) -> Row {
    let room = width.saturating_sub(right.width());
    let left = if left.width() > room {
        left.pad(room)
    } else {
        left
    };
    let gap = width.saturating_sub(left.width() + right.width());
    left.push(" ".repeat(gap), Style::new())
        .cat(right)
        .pad(width)
}

/// Lays out table cells with two spaces between columns.
/// Takes the column widths and one row per column.
/// Returns the joined row.
pub(in crate::tui) fn columns(widths: &[usize], values: Vec<Row>) -> Row {
    let mut out = Row::default();
    let count = widths.len().min(values.len());
    for (index, (width, value)) in widths.iter().zip(values).enumerate() {
        out = out.cat(value.pad(*width));
        if index + 1 < count {
            out = out.push("  ", Style::new());
        }
    }
    out
}

/// Draws a list row: focused rows get `> ` and the selection style across the width.
/// Takes the content, the width, whether it is the remembered row and whether the list has focus.
/// Returns the row.
pub(in crate::tui) fn list_row(
    content: Row,
    width: usize,
    selected: bool,
    focus: bool,
    look: &Look,
) -> Row {
    match (selected, focus) {
        (true, true) => Row::of("> ", Style::new())
            .cat(content)
            .pad(width)
            .restyle(look.selection),
        (true, false) => Row::of("  ", Style::new())
            .cat(content)
            .pad(width)
            .restyle(look.selection_dim),
        _ => Row::of("  ", Style::new()).cat(content),
    }
}

/// Wraps plain text at spaces, filling each line up to the width.
/// Takes the text and the width in cells; line breaks in the text start new lines and words wider than the width are split.
/// Returns the lines, none wider than the width.
pub(in crate::tui) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split(' ').filter(|word| !word.is_empty()) {
            let size = cells(word);
            if used > 0 && used + 1 + size <= width {
                line.push(' ');
                line.push_str(word);
                used += 1 + size;
                continue;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for character in word.chars() {
                let size = char_cells(character);
                if used + size > width && used > 0 {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(character);
                used += size;
            }
        }
        lines.push(line);
    }
    lines
}

/// Draws the 80-column tab strip and its underline.
/// Takes the width, the labels with optional counts, the active label and whether section glyphs fit.
/// Returns two rows: labels, then the rule with a heavy mark under the active label.
pub(in crate::tui) fn tab_strip(
    width: usize,
    items: &[(String, String)],
    active: usize,
    look: &Look,
) -> [Row; 2] {
    let full: usize = items
        .iter()
        .map(|(label, count)| {
            cells(label)
                + if count.is_empty() {
                    0
                } else {
                    cells(count) + 1
                }
        })
        .sum::<usize>()
        + 3 * items.len().saturating_sub(1)
        + 2;
    let bare: usize = items.iter().map(|(label, _)| cells(label)).sum::<usize>() + 2;
    let (counts, gap) = if full <= width {
        (true, 3)
    } else if bare + 3 * items.len().saturating_sub(1) <= width {
        (false, 3)
    } else {
        (false, 2)
    };
    let mut top = Row::of("  ", Style::new());
    let mut under = Row::of("\u{2500}\u{2500}", look.muted);
    for (index, (label, count)) in items.iter().enumerate() {
        let mut segment = Row::of(
            label.clone(),
            if index == active {
                look.accent
            } else {
                look.fg
            },
        );
        if counts && !count.is_empty() {
            segment = segment
                .push(" ", Style::new())
                .push(count.clone(), look.muted);
        }
        let size = segment.width();
        top = top.cat(segment);
        under = if index == active {
            under.push(look.glyph("\u{2501}").repeat(size), look.mark)
        } else {
            under.push(look.glyph("\u{2500}").repeat(size), look.muted)
        };
        if index + 1 < items.len() {
            top = top.push(" ".repeat(gap), Style::new());
            under = under.push(look.glyph("\u{2500}").repeat(gap), look.muted);
        }
    }
    let rest = width.saturating_sub(under.width());
    under = under.push(look.glyph("\u{2500}").repeat(rest), look.muted);
    [top.pad(width), under.pad(width)]
}

/// Draws a box over the body, dimming what is behind it.
/// Takes the body rows, the box width, its title, its content rows, the top row and the look.
/// Returns the body with the box drawn centered across the width.
pub(in crate::tui) fn modal(
    base: &[Row],
    width: usize,
    box_width: usize,
    title: &str,
    body: Vec<Row>,
    top: usize,
    look: &Look,
) -> Vec<Row> {
    let inner = box_width - 4;
    let horizontal = look.glyph("\u{2500}");
    let mut lines = vec![
        Row::of(
            format!("{}{horizontal} ", look.glyph("\u{250c}")),
            look.accent,
        )
        .push(title, look.heading)
        .push(
            format!(
                " {}{}",
                horizontal.repeat(box_width.saturating_sub(5 + cells(title))),
                look.glyph("\u{2510}")
            ),
            look.accent,
        ),
    ];
    for row in body {
        lines.push(
            Row::of(look.glyph("\u{2502}"), look.accent)
                .push(" ", Style::new())
                .cat(row.pad(inner))
                .push(" ", Style::new())
                .push(look.glyph("\u{2502}"), look.accent),
        );
    }
    lines.push(Row::of(
        format!(
            "{}{}{}",
            look.glyph("\u{2514}"),
            horizontal.repeat(box_width - 2),
            look.glyph("\u{2518}")
        ),
        look.accent,
    ));
    let left = width.saturating_sub(box_width) / 2;
    let mut out: Vec<Row> = base
        .iter()
        .map(|row| row.clone().pad(width).restyle(look.backdrop))
        .collect();
    while out.len() < top + lines.len() {
        out.push(Row::default().pad(width));
    }
    for (index, line) in lines.into_iter().enumerate() {
        let row = &out[top + index];
        out[top + index] = row
            .slice(0, left)
            .cat(line)
            .cat(row.slice(left + box_width, width));
    }
    out
}

/// One drawn screen body with what the frame needs around it.
/// Holds the body rows, the cursor in body coordinates, the breadcrumb, the help keys and the header status.
/// The frame draws the header, rule, message line and help line.
#[derive(Clone, Debug, Default)]
pub(in crate::tui) struct Screen {
    pub rows: Vec<Row>,
    pub cursor: Option<(u16, u16)>,
    pub crumbs: Vec<String>,
    pub help: Vec<(&'static str, &'static str)>,
    pub status: Option<Row>,
}

fn checkbox(on: bool, label: &str) -> String {
    format!("{} {label}", if on { "[x]" } else { "[ ]" })
}

fn radio(on: bool, label: &str) -> String {
    format!("{} {label}", if on { "(*)" } else { "( )" })
}

/// The words of the roadmap planning toggle on New project and on the import and clone
/// setup review.
pub(in crate::tui) const ROADMAP_LABEL: &str = "Plan a roadmap after research";

/// Gives the glyph of a research kind.
/// Takes the kind.
/// Returns its Unicode glyph from DESIGN.md section 6.3.
pub(in crate::tui) fn research_glyph(kind: ResearchKind) -> &'static str {
    match kind {
        ResearchKind::MarketResearch => "\u{25d4}",
        ResearchKind::CompetitorResearch => "\u{2277}",
        ResearchKind::DesignResearch => "\u{25ec}",
        ResearchKind::FrontEndDesign => "\u{25ad}",
        ResearchKind::TechStackResearch => "\u{2263}",
        ResearchKind::RoadmapPlanning => "\u{25a4}",
    }
}

fn research_description(kind: ResearchKind) -> &'static str {
    match kind {
        ResearchKind::MarketResearch => "Who needs it, market size and pricing",
        ResearchKind::CompetitorResearch => "Similar products, their features and gaps",
        ResearchKind::DesignResearch => "Patterns and references for the look",
        ResearchKind::FrontEndDesign => "A first UI for the platforms you picked",
        ResearchKind::TechStackResearch => "Libraries, hosting and architecture options",
        ResearchKind::RoadmapPlanning => "Versions, time per version and how to release",
    }
}

struct Draw<'a> {
    wizard: &'a Wizard,
    look: &'a Look,
    width: usize,
    content: usize,
    field: usize,
    rows: Vec<Row>,
    cursor: Option<(usize, usize)>,
}

impl Draw<'_> {
    fn add(&mut self, row: Row) {
        self.rows.push(row);
    }

    fn muted(&mut self, text: &str) {
        self.note(text, self.look.muted);
    }

    fn note(&mut self, text: &str, style: Style) {
        for line in wrap_words(text, self.content) {
            self.add(Row::of(line, style));
        }
    }

    fn note_for(&self, control: Control) -> Option<(String, bool)> {
        self.wizard
            .note
            .as_ref()
            .filter(|note| note.control == control)
            .map(|note| (note.text.clone(), note.error))
    }

    fn focus(&self, control: Control) -> bool {
        self.wizard.picker.is_none()
            && self.wizard.phase == Phase::Editing
            && self.wizard.focused() == control
    }

    fn field(&mut self, label: &str, control: Control, input: &TextInput, hint: Option<&str>) {
        let focus = self.focus(control);
        let look = *self.look;
        let inner = self.field - 4;
        self.add(Row::of(label, if focus { look.accent } else { look.heading }).pad(self.field));
        let (lines, (line, column)) = input.wrap(inner - 1);
        let first = line.saturating_sub(3);
        let shown: Vec<String> = lines.iter().skip(first).take(4).cloned().collect();
        let bracket = if focus { look.accent } else { look.muted };
        for (index, text) in shown.iter().enumerate() {
            if focus && self.wizard.typing && index + first == line {
                self.cursor = Some((self.rows.len(), 2 + column));
            }
            self.add(
                Row::of("[", bracket)
                    .push(" ", Style::new())
                    .cat(Row::of(text.clone(), look.fg).pad(inner))
                    .push(" ", Style::new())
                    .push("]", bracket),
            );
        }
        match (self.note_for(control), hint) {
            (Some((text, true)), _) => self.note(&text, look.error),
            (Some((text, false)), _) => self.note(&text, look.success),
            (None, Some(hint)) => self.add(Row::of(hint, look.muted)),
            (None, None) => {}
        }
    }

    fn buttons(&mut self, primary: &str) {
        let look = *self.look;
        let button = |label: &str, focus: bool| {
            if focus {
                Row::of(format!("[> {label} ]"), look.selection)
            } else {
                Row::of(format!("[ {label} ]"), look.fg)
            }
        };
        let row = button("Back", self.focus(Control::Back))
            .push("   ", Style::new())
            .cat(button(primary, self.focus(Control::Continue)));
        self.add(row);
    }
}

mod wizard;
pub(in crate::tui) use wizard::{panel_hints, publish_panel, shown_tail, wizard_screen};

/// Lists the keys for the wizard's current focus, in priority order.
/// Takes the wizard.
/// Returns key and label pairs for the help line.
pub(in crate::tui) fn help_keys(wizard: &Wizard) -> Vec<(&'static str, &'static str)> {
    const STEPS: (&str, &str) = ("Shift-\u{2190}\u{2192}", "Steps");
    const LEAVE: (&str, &str) = ("Esc", "Leave, keep draft");
    match &wizard.phase {
        Phase::Creating { .. } => return vec![("Esc", "Run in background")],
        Phase::Failed { step, .. } => {
            return vec![
                (
                    "Enter",
                    match step {
                        Step::About => "Back to About",
                        Step::Documents => "Back to Documents",
                        Step::Pool => "Back to Pool",
                        Step::Team => "Back to Team",
                        _ => "Back to Review",
                    },
                ),
                LEAVE,
            ];
        }
        Phase::Editing => {}
    }
    if wizard.filtering {
        return vec![("Enter", "Keep filter"), ("Esc", "Clear filter")];
    }
    match wizard.focused() {
        Control::About => vec![
            ("Tab", "Next field"),
            ("Ctrl-J", "New line"),
            ("Shift-Tab", "Previous"),
            STEPS,
            LEAVE,
        ],
        Control::Name | Control::Custom | Control::Min | Control::Max => {
            vec![
                ("Tab", "Next field"),
                ("Shift-Tab", "Previous"),
                ("Enter", "Next"),
                STEPS,
                LEAVE,
            ]
        }
        Control::Grid | Control::Platforms => vec![
            ("\u{2191}\u{2193}\u{2190}\u{2192}", "Move"),
            ("Space", "Select"),
            ("/", "Filter"),
            ("Tab", "Next"),
            STEPS,
            LEAVE,
        ],
        Control::Path | Control::Documents => vec![
            ("Enter", "Add"),
            ("Tab", "Complete path"),
            ("\u{2193}", "To the list"),
            ("d", "Remove"),
            STEPS,
            LEAVE,
        ],
        Control::Work => vec![
            ("Space", "On or off"),
            ("\u{2191}\u{2193}", "Move"),
            ("Tab", "Buttons"),
            STEPS,
            LEAVE,
        ],
        Control::Pools => vec![
            ("\u{2191}\u{2193}", "Choose"),
            ("a", "Create a pool"),
            ("Tab", "Buttons"),
            STEPS,
            LEAVE,
        ],
        Control::Team => vec![
            ("\u{2190}\u{2192}", "Change count"),
            ("Enter", "Pick harness"),
            ("Space", "Tools on or off"),
            ("d", "Remove"),
            STEPS,
            LEAVE,
        ],
        Control::Access => vec![
            ("\u{2190}\u{2192}", "Change"),
            ("Enter", "Create project"),
            ("Tab", "Buttons"),
            STEPS,
            LEAVE,
        ],
        Control::Continue if wizard.step == Step::Review => {
            vec![
                ("Enter", "Create project"),
                ("Shift-Tab", "Back"),
                STEPS,
                LEAVE,
            ]
        }
        Control::Continue => vec![
            ("Enter", "Continue"),
            ("Shift-Tab", "Previous"),
            STEPS,
            LEAVE,
        ],
        Control::Publish => vec![
            ("Space", "On or off"),
            ("Enter", "GitHub details"),
            ("Tab", "Next"),
            STEPS,
            LEAVE,
        ],
        Control::Back => vec![("Enter", "Back"), ("Tab", "Next"), STEPS, LEAVE],
    }
}
