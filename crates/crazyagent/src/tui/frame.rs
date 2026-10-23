use super::glyphs::{Glyphs, Section};
use super::text::{join, pad, row, width};
use super::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};

/// Smallest window the TUI draws into.
pub const MIN_WIDTH: u16 = 80;
/// Smallest window height the TUI draws into.
pub const MIN_HEIGHT: u16 = 24;
const SUCCESS_LIFETIME: Duration = Duration::from_secs(6);

/// One key and what it does, for the help line and the Keys overlay.
/// Input: key name and label. Output: shown as "Enter Open".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: String,
    pub label: String,
}

/// Builds one hint.
/// Inputs: key name and label. Output: the hint.
pub fn hint(key: &str, label: &str) -> Hint {
    Hint {
        key: key.to_owned(),
        label: label.to_owned(),
    }
}

/// The shared "Shift-←→ Sections" hint.
/// Input: glyphs for the arrows. Output: the hint.
pub fn sections_hint(glyphs: &Glyphs) -> Hint {
    hint(
        &format!("Shift-{}{}", glyphs.left, glyphs.right),
        "Sections",
    )
}

/// What kind of result the message line shows.
/// Input: none. Output: success, warning or error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Success,
    Warning,
    Error,
}

/// The latest result on the message line: a lead in its tone, then plain text.
/// Input: built with `Message::success` and friends. Output: drawn with a
/// glyph for success, warning and error; successes clear after 6 s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub tone: Tone,
    pub lead: String,
    pub rest: String,
    pub at: Instant,
}

impl Message {
    /// A success such as "Account added."; it clears after 6 s.
    /// Inputs: lead and following text. Output: the message.
    pub fn success(lead: &str, rest: &str) -> Self {
        Self::new(Tone::Success, lead, rest)
    }

    /// A warning such as the pool limit.
    /// Inputs: lead and following text. Output: the message.
    pub fn warning(lead: &str, rest: &str) -> Self {
        Self::new(Tone::Warning, lead, rest)
    }

    /// An error such as a lost connection.
    /// Inputs: lead and following text. Output: the message.
    pub fn error(lead: &str, rest: &str) -> Self {
        Self::new(Tone::Error, lead, rest)
    }

    fn new(tone: Tone, lead: &str, rest: &str) -> Self {
        Self {
            tone,
            lead: lead.to_owned(),
            rest: rest.to_owned(),
            at: Instant::now(),
        }
    }

    /// Says whether the message should still show.
    /// Input: the current time. Output: false for a success older than 6 s.
    pub fn visible(&self, now: Instant) -> bool {
        self.tone != Tone::Success || now.duration_since(self.at) < SUCCESS_LIFETIME
    }

    /// The message line content.
    /// Inputs: theme and glyphs. Output: " ✓ Lead rest" styled by tone.
    pub fn line(&self, theme: &Theme, glyphs: &Glyphs) -> Line<'static> {
        let (mark, style) = match self.tone {
            Tone::Success => (glyphs.status(super::glyphs::Status::Ok), theme.success),
            Tone::Warning => (glyphs.status(super::glyphs::Status::Needs), theme.warning),
            Tone::Error => (glyphs.status(super::glyphs::Status::Fail), theme.error),
        };
        let mut spans = vec![Span::raw(" "), Span::styled(format!("{mark} "), style)];
        spans.push(Span::styled(self.lead.clone(), style));
        if !self.rest.is_empty() {
            spans.push(Span::raw(format!(" {}", self.rest)));
        }
        Line::from(spans)
    }
}

/// The header line: breadcrumbs on the left, one status on the right.
/// Inputs: width, crumbs after "CrazyAgent", the status line, theme and glyphs.
/// Output: one line; crumbs shorten from the left to "… / last two" and then
/// "… / last one" when they do not fit.
pub fn header(
    limit: usize,
    crumbs: &[String],
    status: Line<'static>,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Line<'static> {
    let right = if status.width() > 0 {
        join([status, Line::raw(" ")])
    } else {
        Line::default()
    };
    let mut options = vec![crumbs.to_vec()];
    if crumbs.len() > 2 {
        options.push(
            std::iter::once(glyphs.ellipsis.to_owned())
                .chain(crumbs[crumbs.len() - 2..].iter().cloned())
                .collect(),
        );
    }
    if crumbs.len() > 1 {
        options.push(vec![
            glyphs.ellipsis.to_owned(),
            crumbs[crumbs.len() - 1].clone(),
        ]);
    }
    let build = |crumbs: &[String]| {
        let mut spans = vec![Span::raw(" "), Span::styled("CrazyAgent", theme.heading)];
        for crumb in crumbs {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("/", theme.muted));
            spans.push(Span::raw(format!("  {crumb}")));
        }
        Line::from(spans)
    };
    let chosen = options
        .iter()
        .map(|option| build(option))
        .find(|left| left.width() + right.width() < limit)
        .unwrap_or_else(|| build(options.last().expect("one option exists")));
    row(limit, chosen, right, glyphs)
}

/// The help line: keys in priority order, then `? Help` at the right edge.
/// Inputs: width, hints, theme and glyphs. Output: one line that drops trailing
/// hints that do not fit; `? Help` always stays.
pub fn help_line(limit: usize, hints: &[Hint], theme: &Theme, glyphs: &Glyphs) -> Line<'static> {
    let separator = if limit >= 100 { "   " } else { "  " };
    let right = Line::from(vec![Span::styled("?", theme.heading), Span::raw(" Help ")]);
    let mut used = 1;
    let mut spans = vec![Span::raw(" ")];
    for (index, item) in hints.iter().enumerate() {
        let cost =
            usize::from(index > 0) * separator.len() + width(&item.key) + 1 + width(&item.label);
        if used + cost + right.width() + 2 > limit {
            break;
        }
        if index > 0 {
            spans.push(Span::raw(separator));
        }
        spans.push(Span::styled(item.key.clone(), theme.heading));
        spans.push(Span::raw(format!(" {}", item.label)));
        used += cost;
    }
    row(limit, Line::from(spans), right, glyphs)
}

/// One tab or section in a tab strip.
/// Input: label, optional section glyph and optional count. Output: drawn as
/// "@ Accounts 240".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub label: String,
    pub section: Option<Section>,
    pub count: String,
}

/// The two-row tab strip: labels, then a rule with `━` under the active tab.
/// Inputs: width, tabs, active index, theme and glyphs. Output: two lines; at
/// narrow widths counts go first, then gaps narrow, then glyphs go.
pub fn tab_strip(
    limit: usize,
    tabs: &[Tab],
    active: usize,
    theme: &Theme,
    glyphs: &Glyphs,
) -> [Line<'static>; 2] {
    assert!(active < tabs.len());
    let cost = |counts: bool, marks: bool, gap: usize| {
        tabs.iter()
            .map(|tab| {
                width(&tab.label)
                    + if marks && tab.section.is_some() { 2 } else { 0 }
                    + if counts && !tab.count.is_empty() {
                        width(&tab.count) + 1
                    } else {
                        0
                    }
            })
            .sum::<usize>()
            + gap * (tabs.len() - 1)
            + 2
    };
    let (counts, marks, gap) = [
        (true, true, 3),
        (false, true, 3),
        (false, true, 2),
        (false, false, 3),
    ]
    .into_iter()
    .find(|(counts, marks, gap)| cost(*counts, *marks, *gap) <= limit)
    .unwrap_or((false, false, 2));
    let mut top = vec![Span::raw("  ")];
    let mut under = vec![Span::styled(glyphs.hline.repeat(2), theme.muted)];
    for (index, tab) in tabs.iter().enumerate() {
        let shown = match tab.section {
            Some(section) if marks => format!("{} {}", glyphs.section(section), tab.label),
            _ => tab.label.clone(),
        };
        let mut segment = vec![Span::styled(
            shown,
            if index == active {
                theme.accent
            } else {
                theme.text
            },
        )];
        if counts && !tab.count.is_empty() {
            segment.push(Span::raw(" "));
            segment.push(Span::styled(tab.count.clone(), theme.muted));
        }
        let segment_width: usize = segment.iter().map(Span::width).sum();
        top.extend(segment);
        under.push(if index == active {
            Span::styled(glyphs.heavy.repeat(segment_width), theme.mark)
        } else {
            Span::styled(glyphs.hline.repeat(segment_width), theme.muted)
        });
        if index + 1 < tabs.len() {
            top.push(Span::raw(" ".repeat(gap)));
            under.push(Span::styled(glyphs.hline.repeat(gap), theme.muted));
        }
    }
    let under_width: usize = under.iter().map(Span::width).sum();
    under.push(Span::styled(
        glyphs.hline.repeat(limit.saturating_sub(under_width)),
        theme.muted,
    ));
    [
        pad(Line::from(top), limit, glyphs),
        pad(Line::from(under), limit, glyphs),
    ]
}

/// Draws lines into an area, one per row, cut at the area's width.
/// Inputs: buffer, area and lines. Output: the rows drawn; extra lines are
/// dropped.
pub fn draw_lines(buffer: &mut Buffer, area: Rect, lines: &[Line<'static>]) {
    for (index, line) in lines.iter().take(area.height as usize).enumerate() {
        buffer.set_line(area.x, area.y + index as u16, line, area.width);
    }
}

/// The five bands of every screen.
/// Input: the full frame area. Output: header, rule, body, message and help
/// areas.
pub fn bands(area: Rect) -> [Rect; 5] {
    assert!(area.height >= 4);
    let line = |y: u16| Rect::new(area.x, y, area.width, 1);
    [
        line(area.y),
        line(area.y + 1),
        Rect::new(area.x, area.y + 2, area.width, area.height - 4),
        line(area.y + area.height - 2),
        line(area.y + area.height - 1),
    ]
}

/// The "Window too small" screen.
/// Inputs: buffer, current size, theme. Output: the notice drawn at the top
/// left with the current and needed size.
pub fn too_small(buffer: &mut Buffer, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::default(),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("CrazyAgent needs a bigger window", theme.heading),
        ]),
        Line::default(),
        Line::raw(format!(" Make it at least {MIN_WIDTH} x {MIN_HEIGHT}.")),
        Line::from(Span::styled(
            format!(" Now {} x {}.", area.width, area.height),
            theme.muted,
        )),
        Line::default(),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Ctrl-Q", theme.heading),
            Span::raw(" Quit"),
        ]),
    ];
    draw_lines(buffer, area, &lines);
}

/// A modal box drawn over a dimmed screen.
/// Input: width, title, body lines and an optional top row. Output: drawn by
/// `Modal::draw`.
#[derive(Clone, Debug)]
pub struct Modal {
    pub width: u16,
    pub title: String,
    pub body: Vec<Line<'static>>,
    pub top: Option<u16>,
}

impl Modal {
    /// Dims the body rows of the frame, then draws the bordered box centered.
    /// Inputs: buffer, full frame area, theme and glyphs. Output: the box area
    /// inside the border, for placing a cursor.
    pub fn draw(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &Glyphs) -> Rect {
        let dim = Style::reset().patch(theme.backdrop);
        for y in area.y + 2..area.y + area.height.saturating_sub(2) {
            for x in area.x..area.x + area.width {
                buffer[(x, y)].set_style(dim);
            }
        }
        let box_width = self.width.min(area.width);
        let inner = box_width as usize - 4;
        let title_room = box_width as usize - 5 - width(&self.title).min(box_width as usize - 5);
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!("{}{} ", glyphs.top_left, glyphs.hline),
                theme.accent,
            ),
            Span::styled(self.title.clone(), theme.heading),
            Span::styled(
                format!(" {}{}", glyphs.hline.repeat(title_room), glyphs.top_right),
                theme.accent,
            ),
        ])];
        for line in &self.body {
            lines.push(join([
                Line::from(Span::styled(format!("{} ", glyphs.vline), theme.accent)),
                pad(line.clone(), inner, glyphs),
                Line::from(Span::styled(format!(" {}", glyphs.vline), theme.accent)),
            ]));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "{}{}{}",
                glyphs.bottom_left,
                glyphs.hline.repeat(box_width as usize - 2),
                glyphs.bottom_right
            ),
            theme.accent,
        )));
        let height = (lines.len() as u16).min(area.height);
        let x = area.x + (area.width - box_width) / 2;
        let y = area.y
            + self
                .top
                .unwrap_or((area.height.saturating_sub(height)) / 2)
                .min(area.height - height);
        let boxed = Rect::new(x, y, box_width, height);
        let base = theme.paint.unwrap_or_default();
        for row_y in boxed.y..boxed.y + boxed.height {
            for column in boxed.x..boxed.x + boxed.width {
                buffer[(column, row_y)].reset();
                buffer[(column, row_y)].set_style(base);
            }
        }
        draw_lines(buffer, boxed, &lines);
        Rect::new(
            x + 2,
            y + 1,
            box_width.saturating_sub(4),
            height.saturating_sub(2),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::{ASCII, UNICODE};
    use crate::tui::text::plain;
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn theme() -> Theme {
        Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark)
    }

    fn crumbs(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn breadcrumbs_shorten_from_the_left() {
        let deep = crumbs(&[
            "Settings",
            "Accounts",
            "Add account",
            "ChatGPT / Codex",
            "Sign in with ChatGPT",
        ]);
        let wide = header(120, &deep, Line::default(), &theme(), &UNICODE);
        assert_eq!(
            plain(&wide).trim_end(),
            " CrazyAgent  /  Settings  /  Accounts  /  Add account  /  ChatGPT / Codex  /  Sign in with ChatGPT"
        );
        let narrow = header(80, &deep, Line::default(), &theme(), &UNICODE);
        assert_eq!(
            plain(&narrow).trim_end(),
            " CrazyAgent  /  …  /  ChatGPT / Codex  /  Sign in with ChatGPT"
        );
        assert_eq!(narrow.width(), 80);
        let status = header(
            80,
            &crumbs(&["Home"]),
            Line::raw("2 agents running"),
            &theme(),
            &ASCII,
        );
        assert_eq!(
            plain(&status),
            format!(" CrazyAgent  /  Home{}2 agents running ", " ".repeat(43))
        );
        let tight = header(
            30,
            &crumbs(&["Settings", "Accounts", "Claude Pro - personal"]),
            Line::default(),
            &theme(),
            &UNICODE,
        );
        assert_eq!(tight.width(), 30);
    }

    #[test]
    fn help_line_drops_trailing_hints_and_keeps_help() {
        let hints = [
            hint("n", "New project"),
            hint("Enter", "Open"),
            hint("s", "Settings"),
            hint("/", "Filter"),
            hint("Ctrl-K", "Go to"),
            hint("Ctrl-Q", "Quit"),
        ];
        assert_eq!(
            plain(&help_line(80, &hints, &theme(), &UNICODE)),
            " n New project  Enter Open  s Settings  / Filter  Ctrl-K Go to           ? Help "
        );
        assert_eq!(
            plain(&help_line(120, &hints, &theme(), &UNICODE)),
            format!(
                " n New project   Enter Open   s Settings   / Filter   Ctrl-K Go to   Ctrl-Q Quit{}? Help ",
                " ".repeat(33)
            )
        );
        assert!(plain(&help_line(20, &hints, &theme(), &UNICODE)).ends_with("? Help "));
    }

    #[test]
    fn tab_strip_drops_counts_then_gaps_then_glyphs() {
        let tabs = vec![
            Tab {
                label: "Accounts".into(),
                section: Some(Section::Accounts),
                count: "240".into(),
            },
            Tab {
                label: "Pools".into(),
                section: Some(Section::Pools),
                count: "3".into(),
            },
            Tab {
                label: "Harnesses".into(),
                section: Some(Section::Harnesses),
                count: "2 of 9".into(),
            },
            Tab {
                label: "Plugins and skills".into(),
                section: Some(Section::Plugins),
                count: String::new(),
            },
            Tab {
                label: "Memory".into(),
                section: Some(Section::Memory),
                count: "41".into(),
            },
            Tab {
                label: "Profile".into(),
                section: Some(Section::Profile),
                count: String::new(),
            },
        ];
        let [top, under] = tab_strip(120, &tabs, 0, &theme(), &UNICODE);
        assert_eq!(
            plain(&top).trim_end(),
            "  @ Accounts 240   ⊞ Pools 3   ❯ Harnesses 2 of 9   ✦ Plugins and skills   ⁂ Memory 41   ◉ Profile"
        );
        assert!(plain(&under).starts_with("──━━━━━━━━━━━━━━──"));
        let [top, under] = tab_strip(80, &tabs, 1, &theme(), &UNICODE);
        assert_eq!(
            plain(&top).trim_end(),
            "  @ Accounts  ⊞ Pools  ❯ Harnesses  ✦ Plugins and skills  ⁂ Memory  ◉ Profile"
        );
        assert!(plain(&under).starts_with("──────────────━━━━━━━──"));
        assert_eq!(under.width(), 80);
        let [top, _] = tab_strip(72, &tabs, 1, &theme(), &ASCII);
        assert_eq!(
            plain(&top).trim_end(),
            "  Accounts   Pools   Harnesses   Plugins and skills   Memory   Profile"
        );
        let [top, under] = tab_strip(50, &tabs, 1, &theme(), &ASCII);
        assert_eq!((top.width(), under.width()), (50, 50));
    }

    #[test]
    fn messages_carry_a_glyph_and_successes_expire() {
        let added = Message::success("Account added.", "");
        assert_eq!(plain(&added.line(&theme(), &UNICODE)), " ✓ Account added.");
        let lost = Message::error(
            "Lost the connection to the engine.",
            "Retrying in 3 s. What you typed is kept.",
        );
        assert_eq!(
            plain(&lost.line(&theme(), &ASCII)),
            " x Lost the connection to the engine. Retrying in 3 s. What you typed is kept."
        );
        assert!(added.visible(added.at + Duration::from_secs(5)));
        assert!(!added.visible(added.at + Duration::from_secs(6)));
        assert!(lost.visible(lost.at + Duration::from_secs(600)));
    }
}
