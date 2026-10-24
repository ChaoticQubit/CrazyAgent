use super::new_project::{
    Control, HarnessState, PLATFORM_ORDER, Phase, PickFor, Step, Wizard, harness_name, size_label,
    team_summary,
};
use super::new_project_input::{TextInput, cells, char_cells};
use super::project_markdown::sanitize;
use crate::project_setup::ResearchKind;
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
/// Returns it without escape sequences, control characters or direction overrides; line breaks become spaces.
pub(in crate::tui) fn printable(text: String) -> String {
    let risky = |c: char| {
        c.is_control()
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
    };
    if !text.chars().any(risky) {
        return text;
    }
    let clean = sanitize(&text).replace('\n', " ");
    assert!(!clean.chars().any(risky));
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
    }
}

fn research_description(kind: ResearchKind) -> &'static str {
    match kind {
        ResearchKind::MarketResearch => "Who needs it, market size and pricing",
        ResearchKind::CompetitorResearch => "Similar products, their features and gaps",
        ResearchKind::DesignResearch => "Patterns and references for the look",
        ResearchKind::FrontEndDesign => "A first UI for the platforms you picked",
        ResearchKind::TechStackResearch => "Libraries, hosting and architecture options",
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

/// Draws the New project wizard body for the current step.
/// Takes the wizard, the frame width and height, and the look.
/// Returns the body rows, cursor, breadcrumb and help keys.
pub(in crate::tui) fn wizard_screen(
    wizard: &Wizard,
    width: u16,
    height: u16,
    look: &Look,
) -> Screen {
    let width = usize::from(width);
    let wide = width >= 100;
    let content = if wide { width - 30 } else { width - 4 };
    let mut draw = Draw {
        wizard,
        look,
        width,
        content,
        field: content.min(72),
        rows: Vec::new(),
        cursor: None,
    };
    match wizard.step {
        Step::About => about(&mut draw),
        Step::Stack => stack(&mut draw),
        Step::Documents => documents(&mut draw),
        Step::Work => work(&mut draw),
        Step::Pool => pool(&mut draw),
        Step::Team => team(&mut draw),
        Step::Review => review(&mut draw),
    }
    let body_height = usize::from(height).saturating_sub(4);
    let (rows, cursor) = if wide {
        rail(&draw, body_height)
    } else {
        strip(&draw)
    };
    let mut rows = rows;
    rows.truncate(body_height);
    let mut help = help_keys(wizard);
    if let Some(picker) = wizard.picker {
        rows = picker_box(wizard, picker.target, picker.cursor, rows, width, look);
        help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Enter", "Pick"),
            ("r", "Check again"),
            ("Esc", "Close"),
        ];
    }
    Screen {
        rows,
        cursor: cursor.filter(|(_, y)| usize::from(*y) < body_height),
        crumbs: vec![
            "Home".into(),
            "New project".into(),
            wizard.step.title().into(),
        ],
        help,
        status: None,
    }
}

fn rail(draw: &Draw, height: usize) -> (Vec<Row>, Option<(u16, u16)>) {
    let look = draw.look;
    let wizard = draw.wizard;
    let mut rail = vec![Row::of("New project", look.heading), Row::default()];
    for step in Step::ALL {
        rail.push(if step == wizard.step {
            Row::of(format!("> {}", step.title()), look.accent)
        } else if wizard.done[step.index()] {
            Row::of(look.glyph("\u{2713}"), look.success)
                .push(format!(" {}", step.title()), look.fg)
        } else {
            Row::of(format!("  {}", step.title()), look.muted)
        });
    }
    rail.push(Row::default());
    rail.push(Row::of("Esc keeps this draft", look.muted));
    rail.push(Row::of("and goes home.", look.muted));
    let mut rows = Vec::new();
    for index in 0..height.max(draw.rows.len()) {
        let left = rail.get(index).cloned().unwrap_or_default();
        let right = draw.rows.get(index).cloned().unwrap_or_default();
        rows.push(
            Row::of("  ", Style::new())
                .cat(left.pad(24))
                .push(look.glyph("\u{2502}"), look.muted)
                .push(" ", Style::new())
                .cat(right.pad(draw.content))
                .pad(draw.width),
        );
    }
    let cursor = draw
        .cursor
        .map(|(row, column)| ((28 + column) as u16, row as u16));
    (rows, cursor)
}

fn strip(draw: &Draw) -> (Vec<Row>, Option<(u16, u16)>) {
    let wizard = draw.wizard;
    let items: Vec<(String, String)> = Step::ALL
        .iter()
        .map(|step| {
            let label = if wizard.done[step.index()] && *step != wizard.step {
                format!("{} {}", draw.look.glyph("\u{2713}"), step.short())
            } else {
                step.short().to_owned()
            };
            (label, String::new())
        })
        .collect();
    let [top, under] = tab_strip(draw.width, &items, wizard.step.index(), draw.look);
    let mut rows = vec![top, under];
    for row in &draw.rows {
        rows.push(Row::of("  ", Style::new()).cat(row.clone()).pad(draw.width));
    }
    let cursor = draw
        .cursor
        .map(|(row, column)| ((2 + column) as u16, (row + 2) as u16));
    (rows, cursor)
}

fn about(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("About", look.accent));
    draw.add(Row::of("Tell agents what you are building.", look.muted));
    draw.add(Row::default());
    draw.field("Name", Control::Name, &wizard.name, None);
    draw.field("What is it about?", Control::About, &wizard.about, None);
    draw.add(Row::default());
    draw.add(Row::of("Project folder", look.heading));
    draw.add(
        Row::of(radio(true, "Create a new folder"), look.fg)
            .pad(28)
            .push("Made when you create the project", look.muted),
    );
    draw.add(Row::of(
        radio(false, "Use an existing folder  Not ready yet"),
        look.muted,
    ));
    draw.add(Row::of(
        "Agents can change files only inside this folder.",
        look.muted,
    ));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn stack(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Stack and platforms", look.accent));
    draw.add(Row::default());
    let filter = if wizard.filtering || !wizard.filter.value().is_empty() {
        Row::of("/ ", look.muted).push(wizard.filter.value(), look.accent)
    } else {
        Row::of("/ Filter", look.muted)
    };
    if wizard.filtering {
        draw.cursor = Some((
            draw.rows.len(),
            14 + wizard.filter.value().chars().map(char_cells).sum::<usize>(),
        ));
    }
    let content = draw.content;
    draw.add(spread(
        content,
        Row::of("Tech stack", look.heading)
            .push("  ", Style::new())
            .cat(filter),
        Row::of(format!("{} selected", wizard.tech.len()), look.muted),
    ));
    let options = wizard.tech_options();
    let (columns, rows) = wizard.grid_shape();
    let cell = if draw.width >= 100 { 22 } else { 24 };
    match &wizard.catalog {
        None => {
            let text = if wizard.catalog_problem.is_some() {
                "The tech list is not ready yet. Add your own below."
            } else {
                "Loading the tech list..."
            };
            draw.add(Row::of(text, look.muted));
        }
        Some(catalog) => {
            let visible = options.len().min(columns * rows);
            let focus = draw.focus(Control::Grid) && !wizard.filtering;
            for row in 0..rows.min(visible) {
                let mut line = Row::default();
                for column in 0..columns {
                    let index = column * rows + row;
                    let Some(label) = options.get(index).filter(|_| index < visible) else {
                        continue;
                    };
                    let text = checkbox(wizard.tech.contains(label), label);
                    line = line.cat(if focus && index == wizard.grid {
                        Row::of(format!(">{text}"), look.selection)
                            .pad(cell)
                            .restyle(look.selection)
                    } else {
                        Row::of(format!(" {text}"), look.fg).pad(cell)
                    });
                }
                draw.add(line);
            }
            let total = catalog.tech_stack.len();
            let more = if visible < options.len() || visible < total {
                " Press / to find more."
            } else {
                ""
            };
            draw.add(Row::of(
                format!("{visible} of {total} shown.{more}"),
                look.muted,
            ));
        }
    }
    let focus = draw.focus(Control::Custom);
    let bracket = if focus { look.accent } else { look.muted };
    if focus && wizard.typing {
        draw.cursor = Some((draw.rows.len(), 16 + cells(wizard.custom.value())));
    }
    let mut custom = Row::of("Add your own", if focus { look.accent } else { look.fg })
        .pad(14)
        .push("[", bracket)
        .push(" ", Style::new())
        .cat(Row::of(wizard.custom.value(), look.fg).pad(28))
        .push(" ", Style::new())
        .push("]", bracket);
    if let Some((text, error)) = draw.note_for(Control::Custom) {
        custom = custom
            .push("  ", Style::new())
            .push(text, if error { look.error } else { look.success });
    }
    draw.add(custom);
    if let Some((text, _)) = draw.note_for(Control::Grid) {
        draw.note(&text, look.error);
    }
    draw.add(Row::default());
    draw.add(Row::of("Platforms", look.heading));
    let focus = draw.focus(Control::Platforms);
    for line in [&PLATFORM_ORDER[..3], &PLATFORM_ORDER[3..]] {
        let mut row = Row::default();
        for (index, platform) in line.iter().enumerate() {
            if index > 0 {
                row = row.push("   ", Style::new());
            }
            let text = checkbox(wizard.platforms.contains(platform), platform.label());
            let at = PLATFORM_ORDER
                .iter()
                .position(|p| p == platform)
                .unwrap_or(0);
            row = row.push(
                text,
                if focus && at == wizard.platform_cursor {
                    look.selection
                } else {
                    look.fg
                },
            );
        }
        draw.add(row);
    }
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn documents(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Documents", look.accent));
    draw.muted("Add PDFs, notes or research files. They are copied into context/documents in the project folder. This step is optional.");
    draw.add(Row::default());
    draw.field(
        "Add a file or folder",
        Control::Path,
        &wizard.path,
        Some("Tab completes the path. Enter adds it."),
    );
    draw.add(Row::default());
    if wizard.documents.is_empty() {
        draw.add(Row::of("No documents yet.", look.muted));
    } else {
        let widths = [30, 12, 8];
        draw.add(
            Row::of("  ", Style::new()).cat(
                columns(
                    &widths,
                    vec![
                        Row::of("File", look.muted),
                        Row::of("Type", look.muted),
                        Row::of("Size", look.muted),
                    ],
                )
                .restyle(look.muted),
            ),
        );
        let focus = draw.focus(Control::Documents);
        let shown = draw.content.saturating_sub(0);
        for (index, document) in wizard.documents.iter().enumerate() {
            let row = columns(
                &widths,
                vec![
                    Row::of(document.name.clone(), look.fg),
                    Row::of(document.kind(), look.fg),
                    Row::of(size_label(document.bytes), look.fg),
                ],
            );
            draw.add(list_row(
                row,
                56.min(shown),
                index == wizard.document_cursor,
                focus,
                &look,
            ));
        }
        let total: u64 = wizard.documents.iter().map(|d| d.bytes).sum();
        let count = wizard.documents.len();
        draw.add(Row::of(
            format!(
                "{count} {}, {} in total",
                if count == 1 { "file" } else { "files" },
                size_label(total)
            ),
            look.muted,
        ));
    }
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn work(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Work to start", look.accent));
    draw.muted("Agents start these as soon as the project exists. Results land in context/research as Markdown, with links to every source.");
    draw.add(Row::default());
    let focus = draw.focus(Control::Work);
    let content = draw.content;
    for (index, kind) in ResearchKind::ALL.into_iter().enumerate() {
        let label = format!("{} {}", look.glyph(research_glyph(kind)), kind.label());
        let row = columns(
            &[26, content.saturating_sub(32)],
            vec![
                Row::of(checkbox(wizard.research.contains(&kind), &label), look.fg),
                Row::of(research_description(kind), look.muted),
            ],
        );
        draw.add(list_row(
            row,
            content,
            focus && index == wizard.work_cursor,
            focus,
            &look,
        ));
    }
    draw.add(Row::default());
    let count = wizard.research.len();
    draw.add(Row::of(
        match count {
            0 => "No research starts. You can start it later from the project.".to_owned(),
            1 => "1 agent starts. Change counts in Team.".to_owned(),
            n => format!("{n} agents start, one per item. Change counts in Team."),
        },
        look.fg,
    ));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn pool(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Pool", look.accent));
    draw.add(Row::of(
        "Agents in this project use accounts from one pool.",
        look.muted,
    ));
    draw.add(Row::default());
    let focus = draw.focus(Control::Pools);
    let content = draw.content;
    if wizard.pools.is_empty() {
        draw.add(list_row(
            Row::of(radio(true, "No pool yet"), look.fg),
            content,
            true,
            focus,
            &look,
        ));
        draw.add(Row::default());
        for line in wrap_words(
            "You have no pools yet. Press a to create one now, or continue and pick one later. Work waits and shows \"Needs a pool\" until then.",
            content,
        ) {
            draw.add(Row::of(line, look.fg));
        }
    } else {
        let widths = [16, 12, content.saturating_sub(34)];
        for pool in &wizard.pools {
            let picked = wizard.pool.as_ref() == Some(&pool.id);
            let mix: Vec<String> = pool
                .harnesses
                .iter()
                .map(|(id, _)| harness_name(id, wizard.harnesses.as_deref()))
                .collect();
            let row = columns(
                &widths,
                vec![
                    Row::of(radio(picked, &pool.name), look.fg),
                    Row::of(
                        format!(
                            "{} {}",
                            pool.accounts,
                            if pool.accounts == 1 {
                                "account"
                            } else {
                                "accounts"
                            }
                        ),
                        look.fg,
                    ),
                    Row::of(mix.join(", "), look.muted),
                ],
            );
            draw.add(list_row(row, content, picked, focus, &look));
        }
        let none = wizard.pool.is_none();
        let row = columns(
            &widths,
            vec![
                Row::of(radio(none, "No pool yet"), look.fg),
                Row::default(),
                Row::of("Work waits and shows \"Needs a pool\"", look.muted),
            ],
        );
        draw.add(list_row(row, content, none, focus, &look));
    }
    draw.add(Row::default());
    draw.add(Row::of("a  Create a pool", look.muted));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn team(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    let wide = draw.width >= 100;
    draw.add(Row::of("Team", look.accent));
    let source = match wizard.pool_name() {
        Some(name) => format!("Defaults come from pool {name}."),
        None => "Pick a pool for smarter defaults.".to_owned(),
    };
    draw.add(Row::of(
        if wide {
            format!("Choose which harness leads and which ones do the work. {source}")
        } else {
            format!("Pick who leads and who does the work. {source}")
        },
        look.muted,
    ));
    draw.add(Row::default());
    let Some(team) = &wizard.team else {
        draw.add(Row::of(
            if wizard.catalog_problem.is_some() {
                "The team defaults are not ready yet. CrazyAgent picks them when the project is created."
            } else {
                "Loading the team defaults..."
            },
            look.muted,
        ));
        draw.add(Row::default());
        draw.buttons("Continue");
        return;
    };
    let widths: [usize; 5] = if wide {
        [12, 14, 8, 6, 20]
    } else {
        [11, 13, 7, 6, 20]
    };
    draw.add(
        Row::of("  ", Style::new()).cat(
            columns(
                &widths,
                ["Role", "Harness", "Agents", "Tools", "Accounts in pool"]
                    .iter()
                    .map(|h| Row::of(*h, look.muted))
                    .collect(),
            )
            .restyle(look.muted),
        ),
    );
    let focus = draw.focus(Control::Team);
    let catalog = wizard.harnesses.as_deref();
    let accounts = |id: &str| match wizard.pool_accounts(id) {
        Some(count) => Row::of(
            format!(
                "{count} {}",
                if count == 1 { "account" } else { "accounts" }
            ),
            look.fg,
        ),
        None => Row::of("-", look.muted),
    };
    let on = |tools: bool| if tools { "On" } else { "Off" };
    let content = draw.content;
    let master = columns(
        &widths,
        vec![
            Row::of("Master", look.fg),
            Row::of(harness_name(&team.master, catalog), look.fg),
            Row::of("1", look.fg),
            Row::of(on(team.master_tools), look.fg),
            accounts(&team.master),
        ],
    );
    draw.add(list_row(
        master,
        content,
        wizard.team_row == 0,
        focus,
        &look,
    ));
    for (index, slot) in team.sub_harnesses.iter().enumerate() {
        let row = index + 1;
        let selected = wizard.team_row == row;
        let count = if selected && focus {
            format!("< {} >", slot.agents)
        } else {
            slot.agents.to_string()
        };
        let line = columns(
            &widths,
            vec![
                Row::of(if index == 0 { "Sub-agents" } else { "" }, look.fg),
                Row::of(harness_name(&slot.harness, catalog), look.fg),
                Row::of(count, look.fg),
                Row::of(on(slot.tools), look.fg),
                accounts(&slot.harness),
            ],
        );
        draw.add(list_row(line, content, selected, focus, &look));
    }
    let add_row = team.sub_harnesses.len() + 1;
    let add = columns(
        &widths,
        vec![
            Row::of(
                if team.sub_harnesses.is_empty() {
                    "Sub-agents"
                } else {
                    ""
                },
                look.fg,
            ),
            Row::of("+ Add harness", look.muted),
            Row::default(),
            Row::default(),
            Row::default(),
        ],
    );
    draw.add(list_row(
        add,
        content,
        wizard.team_row == add_row,
        focus,
        &look,
    ));
    if let Some((text, _)) = draw.note_for(Control::Team) {
        draw.note(&text, look.error);
    } else if let Some(text) = wizard.limited_note().filter(|_| focus) {
        draw.note(&text, look.warning);
    }
    draw.add(Row::default());
    let mut size = Row::of("Team size", look.heading).pad(12);
    for (label, control, input) in [
        ("Min", Control::Min, &wizard.min),
        ("Max", Control::Max, &wizard.max),
    ] {
        let focused = draw.focus(control);
        let bracket = if focused { look.accent } else { look.muted };
        if control == Control::Max {
            size = size.push("   ", Style::new());
        }
        size = size
            .push(
                format!("{label} "),
                if focused { look.accent } else { look.fg },
            )
            .push("[", bracket)
            .push(" ", Style::new());
        if focused && wizard.typing {
            draw.cursor = Some((draw.rows.len(), size.width() + cells(input.value())));
        }
        size = size
            .push(input.value(), look.fg)
            .push(if focused { "  " } else { " " }, Style::new())
            .push("]", bracket);
    }
    draw.add(size);
    match draw
        .note_for(Control::Min)
        .or_else(|| draw.note_for(Control::Max))
    {
        Some((text, _)) => draw.note(&text, look.error),
        None => draw.add(Row::of(
            format!(
                "At most {} agents run at once. Extra work waits until one finishes.",
                team.max_agents
            ),
            look.muted,
        )),
    }
    draw.add(Row::default());
    let research = wizard.research.len();
    let on_whom = if team.sub_harnesses.is_empty() {
        "on the master's harness"
    } else {
        "on the sub-agent harnesses"
    };
    draw.add(Row::of(
        match research {
            0 => "Research: none picked in Work to start.".to_owned(),
            1 => format!("Research: 1 agent, for the one Work item, {on_whom}."),
            n => format!("Research: {n} agents, one per Work item, {on_whom}."),
        },
        look.fg,
    ));
    draw.add(Row::default());
    let tools =
        std::iter::once(team.master_tools).chain(team.sub_harnesses.iter().map(|s| s.tools));
    let any = tools.clone().any(|t| t);
    draw.add(Row::of(
        if any {
            "Tools on: agents edit files and run commands, only in the project folder."
        } else {
            "Tools off: agents read the project and reply, without running commands."
        },
        look.muted,
    ));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn picker_box(
    wizard: &Wizard,
    target: PickFor,
    cursor: usize,
    base: Vec<Row>,
    width: usize,
    look: &Look,
) -> Vec<Row> {
    let title = match target {
        PickFor::Master => "Harness for the master",
        PickFor::Slot(_) | PickFor::NewSlot => "Harness for sub-agents",
    };
    let mut body = Vec::new();
    let choices = wizard.harness_choices();
    let widths = [12, 15, 17];
    for (index, choice) in choices.iter().enumerate() {
        let (status, style) = match choice.state {
            HarnessState::Ready => (format!("{} Ready", look.glyph("\u{2713}")), look.success),
            HarnessState::NotInstalled => (
                format!("{} Not installed", look.glyph("\u{2298}")),
                look.muted,
            ),
            HarnessState::CantStart => (
                format!("{} Can't start", look.glyph("\u{2298}")),
                look.warning,
            ),
            HarnessState::Unknown => (String::new(), look.muted),
        };
        let extra = if choice.limited.is_some() {
            Row::of(
                format!("{} Limited control", look.glyph("\u{25e7}")),
                look.warning,
            )
        } else {
            match wizard.pool_accounts(&choice.id) {
                Some(count) if count > 0 => Row::of(format!("{count} in pool"), look.fg),
                _ => Row::default(),
            }
        };
        let row = columns(
            &widths,
            vec![
                Row::of(choice.name.clone(), look.fg),
                Row::of(status, style),
                extra,
            ],
        );
        let row = list_row(row, 50, index == cursor, true, look);
        body.push(
            if choice.state == HarnessState::NotInstalled && index != cursor {
                row.restyle(look.muted)
            } else {
                row
            },
        );
    }
    body.push(Row::default());
    if wizard.harnesses.is_some() {
        body.push(Row::of(
            "Harnesses that are not installed cannot be picked.",
            look.fg,
        ));
        body.push(Row::of(
            "Install one, then press r to check again.",
            look.fg,
        ));
    } else {
        body.push(Row::of(
            "Not ready yet: the full harness list with install",
            look.fg,
        ));
        body.push(Row::of(
            "checks. These come from your pool's accounts.",
            look.fg,
        ));
    }
    let top = 2;
    modal(&base, width, 56, title, body, top, look)
}

fn review_summary(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    let wide = draw.width >= 100;
    let content = draw.content;
    draw.add(Row::of("Review", look.accent));
    if wide {
        draw.add(Row::default());
    }
    draw.add(Row::of(wizard.name.value().trim(), look.heading));
    for line in wrap_words(wizard.about.value().trim(), content) {
        if !line.is_empty() {
            draw.add(Row::of(line, look.fg));
        }
    }
    draw.add(Row::default());
    let entry = |label: &str, value: Row| Row::of(label, look.muted).pad(16).cat(value);
    let list = |items: Vec<String>, empty: &str| {
        if items.is_empty() {
            Row::of(empty.to_owned(), look.muted)
        } else {
            Row::of(items.join(", "), look.fg)
        }
    };
    draw.add(entry(
        "Project folder",
        Row::of("Made by CrazyAgent", look.fg).push("  new", look.muted),
    ));
    draw.add(entry("Stack", list(wizard.tech.clone(), "None picked")).pad(content));
    let platforms: Vec<String> = PLATFORM_ORDER
        .iter()
        .filter(|p| wizard.platforms.contains(p))
        .map(|p| p.label().to_owned())
        .collect();
    draw.add(entry("Platforms", list(platforms, "None picked")));
    let count = wizard.documents.len();
    draw.add(entry(
        "Documents",
        match count {
            0 => Row::of("None", look.muted),
            1 => Row::of("1 file", look.fg),
            n => Row::of(format!("{n} files"), look.fg),
        },
    ));
    let research: Vec<String> = wizard
        .research_sorted()
        .into_iter()
        .map(|kind| {
            let label = kind.label();
            if wide {
                label.to_owned()
            } else {
                label.strip_suffix(" research").unwrap_or(label).to_owned()
            }
        })
        .collect();
    draw.add(entry("Work to start", list(research, "Nothing yet")).pad(content));
    draw.add(entry(
        "Pool",
        match wizard.pool_name() {
            Some(name) => Row::of(name.to_owned(), look.fg),
            None => Row::of("No pool yet", look.warning),
        },
    ));
    draw.add(
        entry(
            "Team",
            match &wizard.team {
                Some(team) => Row::of(team_summary(team, wizard.harnesses.as_deref()), look.fg),
                None => Row::of("Picked by CrazyAgent", look.fg),
            },
        )
        .pad(content),
    );
    draw.add(Row::default());
}

fn review(draw: &mut Draw) {
    let look = *draw.look;
    review_summary(draw);
    let wizard = draw.wizard;
    match &wizard.phase {
        Phase::Editing => {
            draw.muted(
                "Research starts right away. You can close CrazyAgent and agents keep working.",
            );
            if let Some((text, _)) = draw.note_for(Control::Continue) {
                draw.note(&text, look.error);
            }
            draw.add(Row::default());
            draw.buttons("Create project");
        }
        Phase::Creating { pool, created } => {
            let spinner = "\u{283c}";
            let documents = wizard.documents.len();
            let research = wizard.research.len();
            if *created {
                draw.add(
                    Row::of(look.glyph("\u{2713}"), look.success).push(" Folder created", look.fg),
                );
                if documents > 0 {
                    draw.add(Row::of(look.glyph("\u{2713}"), look.success).push(
                        format!(
                            " {documents} {} copied",
                            if documents == 1 {
                                "document"
                            } else {
                                "documents"
                            }
                        ),
                        look.fg,
                    ));
                }
                if research > 0 {
                    draw.add(Row::of(look.glyph("\u{2713}"), look.success).push(
                        format!(
                            " {research} research {} queued",
                            if research == 1 { "job" } else { "jobs" }
                        ),
                        look.fg,
                    ));
                }
                if *pool {
                    draw.add(Row::of(spinner, look.running).push(
                        format!(" Assigning pool {}", wizard.pool_name().unwrap_or("")),
                        look.fg,
                    ));
                }
            } else {
                draw.add(
                    Row::of(spinner, look.running).push(" Creating the project folder", look.fg),
                );
            }
        }
        Phase::Failed {
            title,
            detail,
            step,
        } => {
            draw.add(Row::of(title.clone(), look.error));
            for line in wrap_words(detail, draw.content) {
                draw.add(Row::of(line, look.fg));
            }
            draw.add(Row::default());
            let label = if *step == Step::Review {
                "Back to Review".to_owned()
            } else {
                format!("Back to {}", step.title())
            };
            draw.add(Row::of(format!("[> {label} ]"), look.selection));
        }
    }
}

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
        Control::Back => vec![("Enter", "Back"), ("Tab", "Next"), STEPS, LEAVE],
    }
}
