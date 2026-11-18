use super::style::{Theme, blend};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{RenderableContent, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

/// A fixed grid size for a terminal: no scrollback beyond the screen.
pub struct GridSize {
    pub columns: usize,
    pub lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// A run of cells in one row that share colour and style: `cells` cells from `column` on,
/// one character per cell; or one cell group drawn as a whole, a wide character over two
/// cells (`wide`) or a character with its zero-width characters, such as combining marks,
/// variation selectors and joiners (`cluster`).
pub struct Span {
    pub text: String,
    pub column: usize,
    pub cells: usize,
    pub wide: bool,
    pub cluster: bool,
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
}

/// A background rectangle: a run of cells in one row with a non-default background.
pub struct Fill {
    pub row: usize,
    pub column: usize,
    pub width: usize,
    pub color: Rgb,
}

/// The cursor a frame draws: its cell and shape. A block is drawn into the cell's colours;
/// the other shapes are drawn over the cell in the theme's cursor colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
    pub shape: CursorShape,
}

/// What one frame draws: the spans of every row, a hash per row to skip unchanged rows, the
/// background fills and the cursor.
pub struct Frame {
    pub rows: Vec<Vec<Span>>,
    pub keys: Vec<u64>,
    pub fills: Vec<Fill>,
    pub cursor: Option<Cursor>,
}

/// The xterm 256-colour palette entry for an index, with the theme's 16 colours first.
/// Inputs: the index and the theme. Output: the colour.
pub fn indexed(index: u8, theme: &Theme) -> Rgb {
    if index < 16 {
        return theme.ansi[usize::from(index)];
    }
    let value = if index < 232 {
        let level = |n: u8| if n == 0 { 0 } else { 55 + u32::from(n) * 40 };
        let cube = index - 16;
        (level(cube / 36) << 16) | (level(cube / 6 % 6) << 8) | level(cube % 6)
    } else {
        let gray = 8 + u32::from(index - 232) * 10;
        (gray << 16) | (gray << 8) | gray
    };
    assert!(value <= 0xffffff);
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

/// The colour a cell colour shows as, with the terminal's own overrides first.
fn resolve(color: Color, colors: &Colors, theme: &Theme) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(index) => {
            colors[usize::from(index)].unwrap_or_else(|| indexed(index, theme))
        }
        Color::Named(name) => {
            let slot = name as usize;
            if let Some(rgb) = colors[slot] {
                return rgb;
            }
            let dims = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            match name {
                NamedColor::Background => theme.background,
                NamedColor::Cursor => theme.cursor,
                NamedColor::DimForeground => blend(theme.foreground, 0.5, theme.background),
                _ if slot < 16 => theme.ansi[slot],
                _ if dims.contains(&slot) => {
                    let base = theme.ansi[slot - NamedColor::DimBlack as usize];
                    blend(base, 0.5, theme.background)
                }
                _ => theme.foreground,
            }
        }
    }
}

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}

/// The colours one cell draws in, as the xterm panel draws them: bold base colours take
/// their bright twin, inverse swaps, the block cursor is the cursor over its accent, faint
/// is half foreground, hidden is the background, selection lies over the background.
/// Inputs: the cell, the palette, the theme, whether the block cursor is on it and whether
/// it is selected. Output: (foreground, background).
fn cell_colors(
    cell: &Cell,
    colors: &Colors,
    theme: &Theme,
    block: bool,
    selected: bool,
) -> (Rgb, Rgb) {
    let bold = cell.flags.contains(Flags::BOLD);
    let fg_color = match cell.fg {
        Color::Named(name) if bold && (name as usize) < 8 => Color::Indexed(name as u8 + 8),
        Color::Indexed(index) if bold && index < 8 => Color::Indexed(index + 8),
        color => color,
    };
    let (mut fg, mut bg) = (
        resolve(fg_color, colors, theme),
        resolve(cell.bg, colors, theme),
    );
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if block {
        (fg, bg) = (theme.cursor_accent, theme.cursor);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = blend(fg, 0.5, bg);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    if selected {
        bg = blend(theme.selection, theme.selection_alpha, bg);
    }
    assert!(!cell.flags.contains(Flags::HIDDEN) || selected || fg == bg);
    assert!(!block || selected || bg == theme.cursor);
    (fg, bg)
}

/// Where the cursor shows on screen, if it does: its row, column and shape. Inputs: the
/// terminal's renderable content, the screen lines and whether the blink shows it now.
fn cursor_at(content: &RenderableContent<'_>, lines: usize, shown: bool) -> Option<Cursor> {
    let shape = content.cursor.shape;
    if !shown || shape == CursorShape::Hidden {
        return None;
    }
    let row = usize::try_from(content.cursor.point.line.0 + content.display_offset as i32).ok()?;
    let column = content.cursor.point.column.0;
    (row < lines).then_some(Cursor { row, column, shape })
}

/// Appends one cell's character to a row's spans and folds it into the row's key: a plain
/// narrow character joins the span it continues; a wide one, or one with zero-width
/// characters after it, is its own span. Inputs: the row's spans and key, the character and
/// its zero-width characters, its colour and (bold, italic), its column and whether wide.
fn push_char(
    spans: &mut Vec<Span>,
    key: &mut u64,
    (c, zero): (char, &[char]),
    (fg, (bold, italic)): (Rgb, (bool, bool)),
    (column, wide): (usize, bool),
) {
    let count = spans.len();
    let packed = (u64::from(fg.r) << 16) | (u64::from(fg.g) << 8) | u64::from(fg.b);
    let style = (u64::from(bold) << 24) | (u64::from(italic) << 25);
    *key = mix(mix(*key, c as u64), packed | style);
    *key = zero.iter().fold(*key, |key, mark| mix(key, *mark as u64));
    let cluster = !zero.is_empty();
    match spans.last_mut() {
        Some(s)
            if !wide
                && !cluster
                && !s.wide
                && !s.cluster
                && s.column + s.cells == column
                && (s.fg, s.bold, s.italic) == (fg, bold, italic) =>
        {
            s.text.push(c);
            s.cells += 1;
        }
        _ => spans.push(Span {
            text: std::iter::once(c).chain(zero.iter().copied()).collect(),
            column,
            cells: if wide { 2 } else { 1 },
            wide,
            cluster,
            fg,
            bold,
            italic,
        }),
    }
    assert!(spans.len() == count || spans.len() == count + 1);
    assert!(
        spans
            .last()
            .is_some_and(|s| s.column + s.cells == column + 1 + usize::from(wide))
    );
}

/// Builds the frame for the visible screen. Inputs: the terminal, the theme and whether the
/// cursor shows now (false in a blink's off phase). Output: rows, fills and the cursor;
/// wide-character spacer cells add no text.
pub fn frame(term: &Term<VoidListener>, theme: &Theme, cursor_shown: bool) -> Frame {
    let content = term.renderable_content();
    let lines = term.screen_lines();
    let offset = content.display_offset as i32;
    let cursor = cursor_at(&content, lines, cursor_shown);
    let mut rows: Vec<Vec<Span>> = (0..lines).map(|_| Vec::new()).collect();
    let mut keys = vec![0xcbf2_9ce4_8422_2325u64; lines];
    let mut fills: Vec<Fill> = Vec::new();
    let spacer = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    let selection = content.selection;
    for indexed in content.display_iter {
        let row = usize::try_from(indexed.point.line.0 + offset).unwrap_or(usize::MAX);
        assert!(row < lines, "display row inside the screen");
        let cell = indexed.cell;
        let column = indexed.point.column.0;
        let block = cursor
            .is_some_and(|c| c.shape == CursorShape::Block && (c.row, c.column) == (row, column));
        let selected = selection.is_some_and(|range| range.contains(indexed.point));
        let (fg, bg) = cell_colors(cell, content.colors, theme, block, selected);
        if bg != theme.background {
            match fills.last_mut() {
                Some(f) if f.row == row && f.column + f.width == column && f.color == bg => {
                    f.width += 1
                }
                _ => fills.push(Fill {
                    row,
                    column,
                    width: 1,
                    color: bg,
                }),
            }
        }
        if cell.flags.intersects(spacer) {
            continue;
        }
        let style = (
            cell.flags.contains(Flags::BOLD),
            cell.flags.contains(Flags::ITALIC),
        );
        let text = (cell.c, cell.zerowidth().unwrap_or(&[]));
        let at = (column, cell.flags.contains(Flags::WIDE_CHAR));
        push_char(&mut rows[row], &mut keys[row], text, (fg, style), at);
    }
    assert_eq!(rows.len(), lines);
    assert!(cursor.is_none_or(|c| c.row < lines));
    assert!(rows.iter().flatten().all(|s| !s.wide || s.cells == 2));
    Frame {
        rows,
        keys,
        fills,
        cursor,
    }
}

/// The plain text of each row, for the headless dump. Input: the frame. Output: one string
/// per row, trailing spaces trimmed.
pub fn text_rows(frame: &Frame) -> Vec<String> {
    let rows: Vec<String> = frame
        .rows
        .iter()
        .map(|spans| {
            spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert_eq!(rows.len(), frame.rows.len());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    fn screen(bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize {
            columns: 10,
            lines: 2,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn palette_corners() {
        let theme = Theme::default();
        assert_eq!(indexed(16, &theme), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            indexed(231, &theme),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(indexed(232, &theme), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            indexed(255, &theme),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
        assert_eq!(indexed(1, &theme), theme.ansi[1]);
    }

    #[test]
    fn cells_draw_in_the_theme_colours() {
        let theme = Theme::default();
        let term = screen(b"\x1b[1;31mA\x1b[0;31mB\x1b[0;7mC\x1b[0mD");
        let frame = frame(&term, &theme, true);
        let fg: Vec<Rgb> = frame.rows[0].iter().map(|s| s.fg).collect();
        let (back, fore) = (theme.background, theme.foreground);
        assert_eq!(theme.cursor_accent, back);
        assert_eq!(
            fg,
            vec![theme.ansi[9], theme.ansi[1], back, fore, back, fore]
        );
        let cells: Vec<(usize, usize)> =
            frame.rows[0].iter().map(|s| (s.column, s.cells)).collect();
        assert_eq!(cells, vec![(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 5)]);
        assert_eq!(
            frame.rows[0]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "ABCD      "
        );
        let fills: Vec<(usize, usize, Rgb)> = frame
            .fills
            .iter()
            .map(|f| (f.column, f.width, f.color))
            .collect();
        assert_eq!(fills, vec![(2, 1, theme.foreground), (4, 1, theme.cursor)]);
        let cursor = Cursor {
            row: 0,
            column: 4,
            shape: CursorShape::Block,
        };
        assert_eq!(frame.cursor, Some(cursor));
    }

    #[test]
    fn a_wide_character_is_its_own_two_cell_span() {
        let theme = Theme::default();
        let term = screen("a\u{4e2d}b".as_bytes());
        let frame = frame(&term, &theme, false);
        let spans: Vec<(&str, usize, usize, bool)> = frame.rows[0]
            .iter()
            .map(|s| (s.text.as_str(), s.column, s.cells, s.wide))
            .collect();
        let wide = ("\u{4e2d}", 1, 2, true);
        assert_eq!(
            spans,
            vec![("a", 0, 1, false), wide, ("b      ", 3, 7, false)]
        );
    }

    #[test]
    fn zero_width_characters_stay_in_their_cell_and_change_the_row_key() {
        let theme = Theme::default();
        let marked = frame(
            &screen("e\u{301}x\u{2764}\u{fe0f}y".as_bytes()),
            &theme,
            false,
        );
        let plain = frame(&screen("ex\u{2764}y".as_bytes()), &theme, false);
        let spans: Vec<(&str, usize, usize, bool)> = marked.rows[0]
            .iter()
            .map(|s| (s.text.as_str(), s.column, s.cells, s.cluster))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("e\u{301}", 0, 1, true),
                ("x", 1, 1, false),
                ("\u{2764}\u{fe0f}", 2, 1, true),
                ("y      ", 3, 7, false),
            ]
        );
        assert_ne!(marked.keys[0], plain.keys[0]);
        assert_eq!(text_rows(&marked)[0], "e\u{301}x\u{2764}\u{fe0f}y");
        assert_eq!(text_rows(&plain)[0], "ex\u{2764}y");
    }

    #[test]
    fn a_blink_off_phase_hides_the_cursor_and_its_block() {
        let theme = Theme::default();
        let term = screen(b"AB");
        let frame = frame(&term, &theme, false);
        assert_eq!(frame.cursor, None);
        assert!(frame.fills.is_empty());
        let shown = super::frame(&term, &theme, true);
        assert_ne!(shown.keys[0], frame.keys[0]);
    }
}
