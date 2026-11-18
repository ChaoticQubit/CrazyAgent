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

/// A run of cells in one row that share colour and style.
pub struct Span {
    pub text: String,
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
/// background fills, the cursor and the marked text's cells.
pub struct Frame {
    pub rows: Vec<Vec<Span>>,
    pub keys: Vec<u64>,
    pub fills: Vec<Fill>,
    pub cursor: Option<Cursor>,
    pub preedit: Option<Preedit>,
}

/// The cells marked text (an input method's composition) covers: its row, first column and
/// width in cells, underlined by the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub row: usize,
    pub column: usize,
    pub cells: usize,
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

/// The colours one cell draws in, as today's xterm panel draws them: bold text in one of
/// the 8 base colours takes its bright twin, inverse video swaps, the block cursor shows
/// the theme's cursor behind the cursor accent, faint text is the foreground at half over
/// the background, hidden text equals the background and a selected cell has the
/// selection laid over its background. Inputs: the cell, the palette, the theme, whether
/// the block cursor is on it and whether it is selected. Output: (foreground, background).
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

/// Builds the frame for the visible screen. Inputs: the terminal, the theme, whether the
/// cursor shows now (false in a blink's off phase) and the marked text, if any. Output:
/// rows, fills, the cursor and the marked text's cells; wide-character spacer cells add no
/// text.
pub fn frame(
    term: &Term<VoidListener>,
    theme: &Theme,
    cursor_shown: bool,
    marked: Option<&str>,
) -> Frame {
    let _ = marked;
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
        let bold = cell.flags.contains(Flags::BOLD);
        let italic = cell.flags.contains(Flags::ITALIC);
        let packed = (u64::from(fg.r) << 16) | (u64::from(fg.g) << 8) | u64::from(fg.b);
        let style = (u64::from(bold) << 24) | (u64::from(italic) << 25);
        keys[row] = mix(mix(keys[row], cell.c as u64), packed | style);
        let spans = &mut rows[row];
        match spans.last_mut() {
            Some(s) if s.fg == fg && s.bold == bold && s.italic == italic => s.text.push(cell.c),
            _ => spans.push(Span {
                text: cell.c.to_string(),
                fg,
                bold,
                italic,
            }),
        }
    }
    assert_eq!(rows.len(), lines);
    assert!(cursor.is_none_or(|c| c.row < lines));
    Frame {
        rows,
        keys,
        fills,
        cursor,
        preedit: None,
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
        let frame = frame(&term, &theme, true, None);
        let fg: Vec<Rgb> = frame.rows[0].iter().map(|s| s.fg).collect();
        let (back, fore) = (theme.background, theme.foreground);
        assert_eq!(theme.cursor_accent, back);
        assert_eq!(
            fg,
            vec![theme.ansi[9], theme.ansi[1], back, fore, back, fore]
        );
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
    fn a_blink_off_phase_hides_the_cursor_and_its_block() {
        let theme = Theme::default();
        let term = screen(b"AB");
        let frame = frame(&term, &theme, false, None);
        assert_eq!(frame.cursor, None);
        assert!(frame.fills.is_empty());
        let shown = super::frame(&term, &theme, true, None);
        assert_ne!(shown.keys[0], frame.keys[0]);
    }

    fn sized(columns: usize, lines: usize, bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize { columns, lines };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn marked_text_replaces_cells_from_the_cursor_and_changes_only_its_row_key() {
        let theme = Theme::default();
        let term = sized(10, 3, b"ab\r\ncd");
        let plain = frame(&term, &theme, true, None);
        assert_eq!(plain.preedit, None);
        assert_eq!(plain.cursor.map(|c| (c.row, c.column)), Some((1, 2)));
        let marked = frame(&term, &theme, true, Some("\u{306b}x"));
        assert_eq!(text_rows(&marked), vec!["ab", "cd\u{306b}x", ""]);
        assert_eq!(marked.keys[0], plain.keys[0]);
        assert_ne!(marked.keys[1], plain.keys[1]);
        assert_eq!(marked.keys[2], plain.keys[2]);
        let cells = Preedit {
            row: 1,
            column: 2,
            cells: 3,
        };
        assert_eq!(marked.preedit, Some(cells));
        assert_eq!(marked.cursor, None);
        assert!(marked.fills.is_empty());
        let long = frame(&term, &theme, true, Some(&"\u{754c}".repeat(5)));
        assert_eq!(text_rows(&long)[1], format!("cd{}", "\u{754c}".repeat(4)));
        assert_eq!(long.preedit.map(|p| p.cells), Some(8));
        assert!(frame(&term, &theme, true, Some("")).preedit.is_none());
    }

    #[test]
    fn marked_text_covers_the_text_under_it_and_follows_a_hidden_cursor() {
        let theme = Theme::default();
        let over = frame(&sized(10, 2, b"abcd\x1b[1;2H"), &theme, true, Some("x"));
        assert_eq!(text_rows(&over)[0], "axcd");
        assert_eq!(
            over.preedit.map(|p| (p.row, p.column, p.cells)),
            Some((0, 1, 1))
        );
        let hidden = frame(&sized(10, 2, b"\x1b[?25lab"), &theme, true, Some("y"));
        assert_eq!(text_rows(&hidden)[0], "aby");
        assert_eq!(hidden.preedit.map(|p| p.column), Some(2));
        let blink = frame(&sized(10, 2, b"ab"), &theme, false, Some("z"));
        assert_eq!(text_rows(&blink)[0], "abz");
        assert_eq!(blink.cursor, None);
    }
}
