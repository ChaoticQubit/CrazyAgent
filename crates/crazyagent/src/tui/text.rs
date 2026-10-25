use super::glyphs::{Glyphs, Status};
use super::theme::{Role, Theme};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Display width of one character in terminal cells.
/// Input: the character. Output: 0, 1 or 2 cells as ratatui measures it.
pub fn char_width(character: char) -> usize {
    let mut buffer = [0u8; 4];
    Span::raw(&*character.encode_utf8(&mut buffer)).width()
}

/// Display width of a string in terminal cells.
/// Input: the text. Output: its width as ratatui draws it.
pub fn width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Cuts text to a width, ending in the ellipsis glyph when anything was cut.
/// Inputs: text, width in cells and the ellipsis. Output: text no wider than
/// the width.
pub fn cut(text: &str, limit: usize, ellipsis: &str) -> String {
    if width(text) <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = char_width(character);
        if used + cells > limit - 1 {
            break;
        }
        used += cells;
        out.push(character);
    }
    out.push_str(ellipsis);
    out
}

/// Wraps text on spaces to a width; words longer than the width are split.
/// Inputs: text and width. Output: lines no wider than the width, except that a
/// character wider than the width gets a line of its own; explicit newlines start
/// new lines; empty text gives no lines.
pub fn wrap(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split(' ') {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if width(&candidate) <= limit {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut rest = word;
            while width(rest) > limit {
                let mut used = 0;
                let split = rest
                    .char_indices()
                    .find(|(_, character)| {
                        used += char_width(*character);
                        used > limit
                    })
                    .map_or(rest.len(), |(index, character)| match index {
                        0 => character.len_utf8(),
                        _ => index,
                    });
                assert!(split > 0);
                lines.push(rest[..split].to_owned());
                rest = &rest[split..];
            }
            current = rest.to_owned();
        }
        lines.push(current);
    }
    while lines.last().is_some_and(String::is_empty) && lines.len() > 1 {
        lines.pop();
    }
    if lines.len() == 1 && lines[0].is_empty() {
        lines.clear();
    }
    lines
}

/// Pads or cuts a line to exactly one width.
/// Inputs: the line, the width and the glyph set for the ellipsis. Output: the
/// line with trailing spaces, or cut with an ellipsis when too wide.
pub fn pad(line: Line<'static>, limit: usize, glyphs: &Glyphs) -> Line<'static> {
    let current = line.width();
    if current == limit {
        return line;
    }
    if current < limit {
        let mut spans = line.spans;
        spans.push(Span::raw(" ".repeat(limit - current)));
        return Line::from(spans);
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let span_width = span.width();
        if used + span_width < limit {
            used += span_width;
            spans.push(span);
            continue;
        }
        let room = limit - used;
        let text = cut(&span.content, room, glyphs.ellipsis);
        used += width(&text);
        spans.push(Span::styled(text, span.style));
        break;
    }
    if used < limit {
        spans.push(Span::raw(" ".repeat(limit - used)));
    }
    Line::from(spans)
}

/// Builds one row with text on the left and text aligned to the right edge.
/// Inputs: total width, left line and right line. Output: a line of exactly
/// that width; the left side is cut first when both do not fit.
pub fn row(
    limit: usize,
    left: Line<'static>,
    right: Line<'static>,
    glyphs: &Glyphs,
) -> Line<'static> {
    let right_width = right.width();
    let left = if left.width() + right_width > limit {
        pad(left, limit.saturating_sub(right_width + 1), glyphs)
    } else {
        left
    };
    let gap = limit.saturating_sub(left.width() + right_width);
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right.spans);
    pad(Line::from(spans), limit, glyphs)
}

/// Lays out table cells at fixed widths with a gap between them.
/// Inputs: column widths, one line per column and the gap. Output: one line;
/// each cell is padded or cut to its width.
pub fn cells(
    widths: &[usize],
    values: Vec<Line<'static>>,
    gap: usize,
    glyphs: &Glyphs,
) -> Line<'static> {
    assert_eq!(widths.len(), values.len());
    let mut spans = Vec::new();
    let count = widths.len();
    for (index, (limit, value)) in widths.iter().zip(values).enumerate() {
        spans.extend(pad(value, *limit, glyphs).spans);
        if index + 1 < count {
            spans.push(Span::raw(" ".repeat(gap)));
        }
    }
    Line::from(spans)
}

/// Gives every span of a line one style, as selection and backdrop do.
/// Inputs: the line and the style. Output: the same text in that style.
pub fn restyle(line: Line<'static>, style: Style) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content, style))
            .collect::<Vec<_>>(),
    )
}

/// Joins lines of spans into one line.
/// Input: the parts. Output: their spans in order.
pub fn join(parts: impl IntoIterator<Item = Line<'static>>) -> Line<'static> {
    Line::from(
        parts
            .into_iter()
            .flat_map(|line| line.spans)
            .collect::<Vec<_>>(),
    )
}

/// Indents every line by a number of spaces.
/// Inputs: lines and the indent. Output: the indented lines.
pub fn indent(lines: Vec<Line<'static>>, by: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| join([Line::raw(" ".repeat(by)), line]))
        .collect()
}

/// A status word with its glyph, in its role's style.
/// Inputs: the word, theme and glyphs. Output: "✓ Signed in" styled; a word
/// with no status kind is shown plain.
pub fn status(word: &str, theme: &Theme, glyphs: &Glyphs) -> Line<'static> {
    match Status::of(word) {
        Some(kind) => Line::from(Span::styled(
            format!("{} {word}", glyphs.status(kind)),
            theme.style(kind.role()),
        )),
        None => Line::raw(word.to_owned()),
    }
}

/// One plain span in a role's style.
/// Inputs: text, role and theme. Output: the styled line.
pub fn styled(text: impl Into<String>, role: Role, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.into(), theme.style(role)))
}

/// A list row: focused rows get `> ` and the selection style across the full
/// width, remembered rows in an unfocused list get the dim selection style.
/// Inputs: content, width, whether selected and whether the list has focus.
/// Output: one line of exactly that width.
pub fn list_row(
    content: Line<'static>,
    limit: usize,
    selected: bool,
    focused: bool,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Line<'static> {
    let marker = if selected && focused { "> " } else { "  " };
    let line = pad(join([Line::raw(marker), content]), limit, glyphs);
    match (selected, focused) {
        (true, true) => restyle(line, theme.selection),
        (true, false) => restyle(line, theme.selection_dim),
        _ => line,
    }
}

/// Appends a scrollbar column to list lines.
/// Inputs: lines, total width including the bar, the first thumb row and the
/// thumb size. Output: lines with `█` for the thumb and `│` for the track.
pub fn with_bar(
    lines: Vec<Line<'static>>,
    limit: usize,
    top: usize,
    size: usize,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let thumb = index >= top && index < top + size;
            join([
                pad(line, limit - 1, glyphs),
                Line::from(if thumb {
                    Span::styled(glyphs.thumb, theme.accent)
                } else {
                    Span::styled(glyphs.vline, theme.muted)
                }),
            ])
        })
        .collect()
}

/// Thumb position and size for a list window.
/// Inputs: visible rows, first visible row and total rows. Output: thumb top
/// and size in rows, at least one row.
pub fn thumb(visible: usize, offset: usize, total: usize) -> (usize, usize) {
    if total == 0 || visible == 0 {
        return (0, visible);
    }
    let size = ((visible * visible + total / 2) / total).clamp(1, visible);
    let top = if total <= visible {
        0
    } else {
        (offset * (visible - size) + (total - visible) / 2) / (total - visible)
    };
    (top.min(visible - size), size)
}

/// Plain text of a line, for tests and copy.
/// Input: the line. Output: its characters without styles.
#[cfg(test)]
pub fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::{ASCII, UNICODE};
    use crate::tui::theme::{Appearance, Colors, Depth};

    #[test]
    fn wrap_matches_the_design_wrapping() {
        assert_eq!(
            wrap(
                "Start one with n. You can add accounts first, or start a project now and pick a pool later.",
                74
            ),
            [
                "Start one with n. You can add accounts first, or start a project now and",
                "pick a pool later."
            ]
        );
        assert_eq!(wrap("", 10), Vec::<String>::new());
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one\ntwo three", 5), ["one", "two", "three"]);
    }

    #[test]
    fn wrap_gives_a_character_wider_than_the_width_its_own_line() {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send([
                wrap("漢", 1),
                wrap("a漢b", 1),
                wrap("漢字 x", 1),
                wrap("漢字", 3),
            ]);
        });
        let wrapped = receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("wrap must return for every width");
        assert_eq!(
            wrapped,
            [
                vec!["漢"],
                vec!["a", "漢", "b"],
                vec!["漢", "字", "x"],
                vec!["漢", "字"]
            ]
        );
    }

    #[test]
    fn rows_pad_cut_and_align() {
        let line = row(20, Line::raw("Accounts"), Line::raw("240"), &UNICODE);
        assert_eq!(plain(&line), "Accounts         240");
        let long = row(
            10,
            Line::raw("Claude Pro - personal"),
            Line::raw("1"),
            &UNICODE,
        );
        assert_eq!(plain(&long), "Claude … 1");
        assert_eq!(long.width(), 10);
        assert_eq!(cut("Fieldnotes Mobile", 14, "…"), "Fieldnotes Mo…");
        assert_eq!(cut("Fieldnotes Mobile", 14, "."), "Fieldnotes Mo.");
        assert_eq!(cut("short", 14, "…"), "short");
        let cut_line = pad(
            Line::raw("Buoy firmware sends readings every 90 s in CBOR."),
            44,
            &UNICODE,
        );
        assert_eq!(
            plain(&cut_line),
            "Buoy firmware sends readings every 90 s in …"
        );
    }

    #[test]
    fn list_rows_mark_focus_without_color() {
        let theme = Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark);
        let focused = list_row(Line::raw("Main"), 10, true, true, &theme, &ASCII);
        assert_eq!(plain(&focused), "> Main    ");
        assert!(
            focused
                .spans
                .iter()
                .all(|span| span.style == theme.selection)
        );
        let remembered = list_row(Line::raw("Main"), 10, true, false, &theme, &ASCII);
        assert_eq!(plain(&remembered), "  Main    ");
        assert!(
            remembered
                .spans
                .iter()
                .all(|span| span.style == theme.selection_dim)
        );
        assert_eq!(
            plain(&list_row(
                Line::raw("Main"),
                10,
                false,
                true,
                &theme,
                &ASCII
            )),
            "  Main    "
        );
    }

    #[test]
    fn status_words_carry_their_glyph() {
        let theme = Theme::new(Depth::Ansi16, Colors::Dark, Appearance::Dark);
        assert_eq!(plain(&status("Signed in", &theme, &UNICODE)), "✓ Signed in");
        assert_eq!(
            plain(&status("Can't reach this URL", &theme, &ASCII)),
            "x Can't reach this URL"
        );
        assert_eq!(plain(&status("Idle", &theme, &UNICODE)), "Idle");
        assert_eq!(
            status("Not signed in", &theme, &UNICODE).spans[0].style,
            theme.warning
        );
    }

    #[test]
    fn thumb_tracks_the_window() {
        assert_eq!(thumb(24, 0, 240), (0, 2));
        assert_eq!(thumb(12, 0, 240), (0, 1));
        assert_eq!(thumb(9, 7, 240), (0, 1));
        assert_eq!(thumb(10, 230, 240), (9, 1));
        assert_eq!(thumb(10, 0, 5), (0, 10));
    }

    #[test]
    fn a_scroll_bar_marks_the_thumb_rows_in_the_last_column() {
        let theme = Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark);
        let lines: Vec<Line<'static>> = ["one", "two", "three", "four"].map(Line::raw).to_vec();
        let barred = with_bar(lines.clone(), 8, 1, 2, &theme, &UNICODE);
        let text: Vec<String> = barred.iter().map(plain).collect();
        assert_eq!(text, ["one    │", "two    █", "three  █", "four   │"]);
        let ascii: Vec<String> = with_bar(lines, 8, 0, 1, &theme, &ASCII)
            .iter()
            .map(plain)
            .collect();
        assert_eq!(ascii, ["one    #", "two    |", "three  |", "four   |"]);
    }
}
