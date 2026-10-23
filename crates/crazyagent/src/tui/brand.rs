use super::glyphs::Glyphs;
use super::theme::Theme;
use ratatui::text::{Line, Span};

/// Cells in every row of the splash mark, in Unicode and in ASCII.
pub const MARK_WIDTH: usize = 16;
/// Rows in the splash mark.
pub const MARK_HEIGHT: usize = 7;
/// Cells in the header badge, "[◆▪◆▪] CrazyAgent" or "[#.#.] CrazyAgent".
pub const BADGE_WIDTH: usize = 17;

const MARK: [&str; MARK_HEIGHT] = [
    "┏━━━━━━━━━━━━━╸ ",
    "┃ ┏━━━━━━━━━━━━┓",
    "┃ ┃      ▪     ┃",
    "┃ ┃  ◆     ◆ ╻ ┃",
    "┃ ┃    ▪     ┃ ┃",
    "┃ ┗━━━━━━━━━━┛ ┃",
    "┗━━━━━━━━━━━━━━┛",
];
const BADGE: &str = "[◆▪◆▪]";

fn ascii(cell: char) -> char {
    match cell {
        '┏' | '┓' | '┗' | '┛' => '+',
        '━' | '╸' => '=',
        '┃' | '╻' => '|',
        '◆' => '#',
        '▪' => '.',
        other => other,
    }
}

fn styled(row: &str, theme: &Theme, glyphs: &Glyphs) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut squares = false;
    for cell in row.chars() {
        let square = matches!(cell, '◆' | '▪');
        if square != squares && !run.is_empty() {
            let style = if squares { theme.mark } else { theme.text };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        squares = square;
        run.push(if glyphs.ascii { ascii(cell) } else { cell });
    }
    if !run.is_empty() {
        let style = if squares { theme.mark } else { theme.text };
        spans.push(Span::styled(run, style));
    }
    spans
}

/// The Ember 4 splash mark: an ink coil holding four tilted squares, drawn with heavy
/// box-drawing and geometric glyphs. Inputs: theme and glyphs. Output: `MARK_HEIGHT` lines of
/// `MARK_WIDTH` cells, the coil in `theme.text` and the squares in `theme.mark`; an ASCII glyph
/// set gets the same drawing in ASCII at the same size.
pub fn mark(theme: &Theme, glyphs: &Glyphs) -> Vec<Line<'static>> {
    let lines: Vec<Line<'static>> = MARK
        .iter()
        .map(|row| Line::from(styled(row, theme, glyphs)))
        .collect();
    assert!(
        lines.iter().all(|line| line.width() == MARK_WIDTH),
        "every brand mark row keeps one width"
    );
    lines
}

/// The one-line header badge: the four squares in brackets, then the name. Inputs: theme and
/// glyphs. Output: spans `BADGE_WIDTH` cells wide with the brackets in `theme.text`, the
/// squares in `theme.mark` and "CrazyAgent" in `theme.heading`, to stand where the header
/// shows the name.
pub fn badge(theme: &Theme, glyphs: &Glyphs) -> Vec<Span<'static>> {
    let mut spans = styled(BADGE, theme, glyphs);
    spans.push(Span::raw(" "));
    spans.push(Span::styled("CrazyAgent", theme.heading));
    assert_eq!(
        spans.iter().map(Span::width).sum::<usize>(),
        BADGE_WIDTH,
        "the badge keeps one width"
    );
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::{ASCII, UNICODE};
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn mark_and_badge_keep_their_width_in_unicode_and_ascii() {
        let theme = Theme::new(Depth::TrueColor, Colors::Dark, Appearance::Dark);
        assert_ne!(theme.mark, theme.text);
        for glyphs in [&UNICODE, &ASCII] {
            let lines = mark(&theme, glyphs);
            assert_eq!(lines.len(), MARK_HEIGHT);
            let mut squares = 0;
            for line in &lines {
                assert_eq!(line.width(), MARK_WIDTH);
                assert_eq!(text(&line.spans).chars().count(), MARK_WIDTH);
                for span in &line.spans {
                    assert!(
                        !glyphs.ascii || span.content.is_ascii(),
                        "{:?}",
                        span.content
                    );
                    if span.style == theme.mark {
                        squares += span.content.chars().count();
                    } else {
                        assert_eq!(span.style, theme.text);
                    }
                }
            }
            assert_eq!(squares, 4, "four squares in the mark color");
            let badge = badge(&theme, glyphs);
            assert_eq!(badge.iter().map(Span::width).sum::<usize>(), BADGE_WIDTH);
            assert_eq!(text(&badge).chars().count(), BADGE_WIDTH);
            assert_eq!(badge.last().map(|span| span.style), Some(theme.heading));
            assert!(!glyphs.ascii || text(&badge).is_ascii());
        }
        assert_eq!(text(&mark(&theme, &UNICODE)[3].spans), "┃ ┃  ◆     ◆ ╻ ┃");
        assert_eq!(text(&mark(&theme, &ASCII)[3].spans), "| |  #     # | |");
        assert_eq!(text(&badge(&theme, &UNICODE)), "[◆▪◆▪] CrazyAgent");
        assert_eq!(text(&badge(&theme, &ASCII)), "[#.#.] CrazyAgent");
    }
}
