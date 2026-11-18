use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

/// The default text colour.
pub const FOREGROUND: Rgb = Rgb {
    r: 0xd8,
    g: 0xd8,
    b: 0xd8,
};

/// The default background colour.
pub const BACKGROUND: Rgb = Rgb {
    r: 0x1e,
    g: 0x1e,
    b: 0x1e,
};

/// The first 16 colours of the xterm palette.
const BASE16: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

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

/// The cells marked text (an input method's composition) covers: its row, first column and
/// width in cells, underlined by the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub row: usize,
    pub column: usize,
    pub cells: usize,
}

/// What one frame draws: the spans of every row, a hash per row to skip unchanged rows, the
/// background fills, the cursor cell and the marked text's cells.
pub struct Frame {
    pub rows: Vec<Vec<Span>>,
    pub keys: Vec<u64>,
    pub fills: Vec<Fill>,
    pub cursor: Option<(usize, usize)>,
    pub preedit: Option<Preedit>,
}

/// The xterm 256-colour palette entry for an index. Input: the index. Output: the colour.
pub fn indexed(index: u8) -> Rgb {
    let value = match index {
        0..=15 => BASE16[usize::from(index)],
        16..=231 => {
            let level = |n: u8| if n == 0 { 0 } else { 55 + u32::from(n) * 40 };
            let cube = index - 16;
            (level(cube / 36) << 16) | (level(cube / 6 % 6) << 8) | level(cube % 6)
        }
        _ => {
            let gray = 8 + u32::from(index - 232) * 10;
            (gray << 16) | (gray << 8) | gray
        }
    };
    assert!(value <= 0xffffff);
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

fn dim(color: Rgb) -> Rgb {
    Rgb {
        r: color.r / 3 * 2,
        g: color.g / 3 * 2,
        b: color.b / 3 * 2,
    }
}

/// The colour a cell colour shows as, with the terminal's own overrides first.
fn resolve(color: Color, colors: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(index) => colors[usize::from(index)].unwrap_or_else(|| indexed(index)),
        Color::Named(name) => {
            let slot = name as usize;
            if let Some(rgb) = colors[slot] {
                return rgb;
            }
            match name {
                NamedColor::Background => BACKGROUND,
                NamedColor::DimForeground => dim(FOREGROUND),
                _ if slot < 16 => indexed(slot as u8),
                _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                    .contains(&slot) =>
                {
                    dim(indexed((slot - NamedColor::DimBlack as usize) as u8))
                }
                _ => FOREGROUND,
            }
        }
    }
}

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}

/// The colours one cell draws in: its resolved foreground and background, swapped once for
/// inverse video and once more when selected, the foreground dimmed for faint text and
/// equal to the background for hidden text. Inputs: the cell, the palette and whether the
/// cell is selected. Output: (foreground, background).
fn cell_colors(cell: &Cell, colors: &Colors, selected: bool) -> (Rgb, Rgb) {
    let pair = (resolve(cell.fg, colors), resolve(cell.bg, colors));
    let (mut fg, mut bg) = pair;
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if selected {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = dim(fg);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    assert!(!cell.flags.contains(Flags::HIDDEN) || fg == bg);
    let faint = cell.flags.intersects(Flags::DIM | Flags::HIDDEN);
    assert!(faint || (fg, bg) == pair || (bg, fg) == pair);
    (fg, bg)
}

/// Builds the frame for the visible screen. Inputs: the terminal and the marked text, if
/// any. Output: rows, fills, the cursor and the marked text's cells; wide-character spacer
/// cells add no text, selected cells show swapped colours, and marked text replaces the
/// cells from the terminal cursor on (clipped at the row end, cursor hidden while shown).
pub fn frame(term: &Term<VoidListener>, marked: Option<&str>) -> Frame {
    let _ = marked;
    let content = term.renderable_content();
    let lines = term.screen_lines();
    let offset = content.display_offset as i32;
    let mut rows: Vec<Vec<Span>> = (0..lines).map(|_| Vec::new()).collect();
    let mut keys = vec![0xcbf2_9ce4_8422_2325u64; lines];
    let mut fills: Vec<Fill> = Vec::new();
    let spacer = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    let selection = content.selection;
    for indexed in content.display_iter {
        let row = usize::try_from(indexed.point.line.0 + offset).unwrap_or(usize::MAX);
        assert!(row < lines, "display row inside the screen");
        let cell = indexed.cell;
        let selected = selection.is_some_and(|range| range.contains(indexed.point));
        let (fg, bg) = cell_colors(cell, content.colors, selected);
        let column = indexed.point.column.0;
        if bg != BACKGROUND {
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
    let cursor = (content.cursor.shape != CursorShape::Hidden)
        .then(|| {
            let row = usize::try_from(content.cursor.point.line.0 + offset).ok()?;
            (row < lines).then_some((row, content.cursor.point.column.0))
        })
        .flatten();
    assert_eq!(rows.len(), lines);
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

    fn term(columns: usize, lines: usize, bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize { columns, lines };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn marked_text_replaces_cells_from_the_cursor_and_changes_only_its_row_key() {
        let term = term(10, 3, b"ab\r\ncd");
        let plain = frame(&term, None);
        assert_eq!(plain.preedit, None);
        assert_eq!(plain.cursor, Some((1, 2)));
        let marked = frame(&term, Some("\u{306b}x"));
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
        let long = frame(&term, Some(&"\u{754c}".repeat(5)));
        assert_eq!(text_rows(&long)[1], format!("cd{}", "\u{754c}".repeat(4)));
        assert_eq!(long.preedit.map(|p| p.cells), Some(8));
        assert!(frame(&term, Some("")).preedit.is_none());
    }

    #[test]
    fn marked_text_covers_the_text_under_it_and_follows_a_hidden_cursor() {
        let over = frame(&term(10, 2, b"abcd\x1b[1;2H"), Some("x"));
        assert_eq!(text_rows(&over)[0], "axcd");
        assert_eq!(
            over.preedit.map(|p| (p.row, p.column, p.cells)),
            Some((0, 1, 1))
        );
        let hidden = frame(&term(10, 2, b"\x1b[?25lab"), Some("y"));
        assert_eq!(text_rows(&hidden)[0], "aby");
        assert_eq!(hidden.preedit.map(|p| p.column), Some(2));
    }

    #[test]
    fn palette_corners() {
        assert_eq!(indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
        assert_eq!(
            indexed(1),
            Rgb {
                r: 0xcd,
                g: 0,
                b: 0
            }
        );
    }
}
