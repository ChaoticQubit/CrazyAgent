use super::ime::{slice_units, units};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};

/// The most zero-width characters one cell gives the accessibility text; more are dropped.
pub const ZERO_WIDTH_MAX: usize = 4;

/// The start of the FNV-1a fold the text key uses.
const HASH_START: u64 = 0xcbf2_9ce4_8422_2325;

/// The value the text key folds in after each cell's characters; no character has it.
const CELL_END: u64 = u64::MAX;

/// One visible cell as the accessibility text reads it: its text (a blank cell is a space)
/// and whether it is the right half of a wide character, which adds no text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub text: String,
    pub spacer: bool,
}

/// The visible screen as the accessibility text reads it: every row's cells in column
/// order, the cursor cell (row, column) and the selection's first and last cells, both
/// clamped to the screen.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub rows: Vec<Vec<Glyph>>,
    pub columns: usize,
    pub cursor: Option<(usize, usize)>,
    pub selection: Option<((usize, usize), (usize, usize))>,
}

/// The cursor cell and the selection's first and last cells of one frame, both on the
/// screen; a change between frames is what VoiceOver hears as a selection change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub cursor: Option<(usize, usize)>,
    pub selection: Option<((usize, usize), (usize, usize))>,
}

/// The size of one cell and the padding in physical pixels, and the display scale, which
/// turn cells into view points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub cell: (f64, f64),
    pub pad: f64,
    pub scale: f64,
}

/// The text VoiceOver reads for one view: the visible rows joined by newlines, each row's
/// trailing blanks trimmed except up to the cursor; the UTF-16 start of every row and the
/// UTF-16 offset of every column inside its row; the cursor index; the selected range.
#[derive(Clone, Debug, Default)]
pub struct Text {
    pub string: String,
    units: usize,
    starts: Vec<usize>,
    offsets: Vec<Vec<usize>>,
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
}

/// Reads the visible screen of a terminal: cells with their zero-width characters, the
/// cursor cell even while the cursor is hidden (input methods and screen readers follow
/// it), and the selection clamped to the screen. Input: the terminal. Output: the grid.
pub fn visible(term: &Term<VoidListener>) -> Grid {
    let content = term.renderable_content();
    let (lines, columns) = (term.screen_lines(), term.columns());
    assert!(lines > 0 && columns > 0);
    let offset = content.display_offset as i32;
    let blank = Glyph {
        text: " ".to_owned(),
        spacer: false,
    };
    let mut rows = vec![vec![blank; columns]; lines];
    for indexed in content.display_iter {
        let row = usize::try_from(indexed.point.line.0 + offset).unwrap_or(usize::MAX);
        let column = indexed.point.column.0;
        assert!(
            row < lines && column < columns,
            "display cell inside the screen"
        );
        let cell = indexed.cell;
        let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
        let text: String = cell_chars(cell).collect();
        assert!(!spacer || text.is_empty());
        rows[row][column] = Glyph { text, spacer };
    }
    let Marks { cursor, selection } = marks(term);
    assert!(cursor.is_none_or(|(row, column)| row < lines && column < columns));
    Grid {
        rows,
        columns,
        cursor,
        selection,
    }
}

/// Reads the cursor cell (even while the cursor is hidden) and the selection of a terminal,
/// both clamped to the screen. Input: the terminal. Output: the marks.
pub fn marks(term: &Term<VoidListener>) -> Marks {
    let content = term.renderable_content();
    let (lines, columns) = (term.screen_lines(), term.columns());
    assert!(lines > 0 && columns > 0);
    let offset = content.display_offset as i32;
    let last = lines as i32 - 1;
    let point = content.cursor.point;
    let cursor_row = point.line.0 + offset;
    let cursor = (0..=last)
        .contains(&cursor_row)
        .then(|| (cursor_row as usize, point.column.0.min(columns - 1)));
    let selection = content.selection.and_then(|range| {
        let (start, end) = (range.start.line.0 + offset, range.end.line.0 + offset);
        if end < 0 || start > last {
            return None;
        }
        let first = if start < 0 {
            (0, 0)
        } else {
            (start as usize, range.start.column.0)
        };
        let final_cell = if end > last {
            (lines - 1, columns - 1)
        } else {
            (end as usize, range.end.column.0)
        };
        Some((first, final_cell))
    });
    assert!(cursor.is_none_or(|(row, column)| row < lines && column < columns));
    Marks { cursor, selection }
}

/// The characters one cell gives the accessibility text: none for the right half of a wide
/// character, a space for the blank a wrapped wide character left, else the character and
/// up to ZERO_WIDTH_MAX zero-width characters. Input: the cell. Output: the characters.
fn cell_chars(cell: &Cell) -> impl Iterator<Item = char> + '_ {
    let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
    let leading = cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
    let first = (!spacer).then_some(if leading { ' ' } else { cell.c });
    let take = if spacer { 0 } else { ZERO_WIDTH_MAX };
    let zero_width = cell.zerowidth().unwrap_or(&[]).iter().take(take).copied();
    first.into_iter().chain(zero_width)
}

/// One step of the FNV-1a fold. Inputs: the hash so far and a value. Output: the hash.
fn fold(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}

/// One hash of the accessibility text a terminal shows, made without building it: each
/// cell's characters as `visible` reads them and each row's kept width as `build` trims it,
/// so a view posts a value change when what VoiceOver reads changed and not for a colour,
/// blink phase, selection or marked text. Input: the terminal. Output: the hash.
pub fn text_key(term: &Term<VoidListener>) -> u64 {
    let content = term.renderable_content();
    let (lines, columns) = (term.screen_lines(), term.columns());
    assert!(lines > 0 && columns > 0);
    assert!(lines <= super::pane::GRID_MAX && columns <= super::pane::GRID_MAX);
    let offset = content.display_offset as i32;
    let mut rows = vec![HASH_START; lines];
    let mut ends = vec![0usize; lines];
    for indexed in content.display_iter {
        let row = usize::try_from(indexed.point.line.0 + offset).unwrap_or(usize::MAX);
        let column = indexed.point.column.0;
        assert!(
            row < lines && column < columns,
            "display cell inside the screen"
        );
        let (mut count, mut spaces) = (0usize, 0usize);
        for c in cell_chars(indexed.cell) {
            rows[row] = fold(rows[row], u64::from(c));
            count += 1;
            spaces += usize::from(c == ' ');
        }
        rows[row] = fold(rows[row], CELL_END);
        if count != 1 || spaces != 1 {
            ends[row] = column + 1;
        }
    }
    let cursor = marks(term).cursor;
    let mut key = HASH_START;
    for (row, hash) in rows.iter().enumerate() {
        let cursor_end = cursor.filter(|cell| cell.0 == row).map_or(0, |cell| cell.1);
        let keep = ends[row].max(cursor_end).min(columns);
        assert!(keep <= columns);
        key = fold(fold(key, *hash), keep as u64);
    }
    key
}

/// Builds the accessibility text of a grid. Input: the grid. Output: the text.
pub fn build(grid: &Grid) -> Text {
    let lines = grid.rows.len();
    assert!(lines <= super::pane::GRID_MAX && grid.columns <= super::pane::GRID_MAX);
    let mut text = Text {
        starts: Vec::with_capacity(lines),
        offsets: Vec::with_capacity(lines),
        ..Text::default()
    };
    for (row, glyphs) in grid.rows.iter().enumerate() {
        assert_eq!(glyphs.len(), grid.columns);
        let cursor_end = grid
            .cursor
            .filter(|cell| cell.0 == row)
            .map_or(0, |cell| cell.1);
        let content_end = glyphs
            .iter()
            .rposition(|glyph| glyph.spacer || glyph.text != " ")
            .map_or(0, |column| column + 1);
        let keep = content_end.max(cursor_end).min(grid.columns);
        let mut offsets = Vec::with_capacity(grid.columns + 1);
        let mut local = 0usize;
        for (column, glyph) in glyphs.iter().enumerate() {
            if column >= keep {
                offsets.push(local);
            } else if glyph.spacer {
                offsets.push(offsets.last().copied().unwrap_or(local));
            } else {
                offsets.push(local);
                text.string.push_str(&glyph.text);
                local += units(&glyph.text);
            }
        }
        offsets.push(local);
        text.starts.push(text.units);
        text.units += local;
        if row + 1 < lines {
            text.string.push('\n');
            text.units += 1;
        }
        text.offsets.push(offsets);
    }
    assert_eq!(units(&text.string), text.units);
    assert!(text.units <= lines * (grid.columns * (2 + 2 * ZERO_WIDTH_MAX) + 1));
    text.cursor = grid
        .cursor
        .map_or(text.units, |(row, column)| text.index_at(row, column));
    text.selection = grid.selection.map(|(first, last)| {
        let start = text.index_at(first.0, first.1);
        let past = (last.1 + 1).min(grid.columns);
        let spacer = grid.rows[last.0]
            .get(past)
            .is_some_and(|glyph| glyph.spacer);
        let past = past + usize::from(spacer);
        assert!(past <= grid.columns);
        let end = text.index_at(last.0, past);
        (start, end.max(start) - start)
    });
    text
}

impl Text {
    /// The text's length in UTF-16 units. Output: the length.
    pub fn units(&self) -> usize {
        self.units
    }

    /// The index of a cell; the right half of a wide character and a trimmed blank give the
    /// index its text starts at. Inputs: the row and the column (the column count is the
    /// row's end). Output: the UTF-16 index.
    pub fn index_at(&self, row: usize, column: usize) -> usize {
        assert!(row < self.offsets.len());
        assert!(column < self.offsets[row].len());
        let index = self.starts[row] + self.offsets[row][column];
        assert!(index <= self.units);
        index
    }

    /// The column an index inside a row falls on: the first column of the character holding
    /// it. Inputs: the row and the UTF-16 index from the row's start. Output: the column.
    fn column_at(&self, row: usize, local: usize) -> usize {
        let offsets = &self.offsets[row];
        let count = offsets.partition_point(|offset| *offset <= local);
        assert!(count >= 1, "a row starts at offset 0");
        let target = offsets[count - 1];
        let column = offsets.partition_point(|offset| *offset < target);
        assert!(column < offsets.len() && offsets[column] == target);
        column
    }

    /// The line an index is on; an index past the end is on the last line. Input: the
    /// UTF-16 index. Output: the line.
    pub fn line_for_index(&self, index: usize) -> usize {
        let count = self.starts.partition_point(|start| *start <= index);
        let line = count.saturating_sub(1);
        assert!(self.starts.is_empty() || line < self.starts.len());
        line
    }

    /// The range of one line with its newline. Input: the line. Output: the UTF-16 location
    /// and length, or none past the last line.
    pub fn range_for_line(&self, line: usize) -> Option<(usize, usize)> {
        let start = *self.starts.get(line)?;
        let end = self.starts.get(line + 1).copied().unwrap_or(self.units);
        assert!(start <= end && end <= self.units);
        Some((start, end - start))
    }

    /// The text of a range, clamped and widened to whole characters. Input: the UTF-16
    /// location and length. Output: the text and the range it covers, or none.
    pub fn string_for_range(&self, range: (usize, usize)) -> Option<(String, (usize, usize))> {
        slice_units(&self.string, range)
    }

    /// The cells the first line of a range covers, for its frame. Input: the UTF-16
    /// location and length. Output: the row and the first and past-the-last column, at
    /// least one cell wide, or none past the end.
    pub fn cells_for_range(&self, range: (usize, usize)) -> Option<(usize, usize, usize)> {
        if range.0 > self.units || self.starts.is_empty() {
            return None;
        }
        let row = self.line_for_index(range.0);
        let columns = self.offsets[row].len() - 1;
        let start = self.starts[row];
        let column = self.column_at(row, range.0 - start).min(columns - 1);
        let row_end = start + self.offsets[row][columns];
        let end = range.0.saturating_add(range.1).min(row_end).max(range.0) - start;
        let past = self.offsets[row].partition_point(|offset| *offset < end);
        let past = past.max(column + 1).min(columns);
        assert!(column < past && past <= columns);
        Some((row, column, past))
    }
}

/// The rectangle of a run of cells in view points (the view is flipped, y grows down).
/// Inputs: the geometry, the row, the first column and the width in cells. Output: x, y,
/// width and height.
pub fn cell_rect(
    geometry: Geometry,
    row: usize,
    column: usize,
    width: usize,
) -> (f64, f64, f64, f64) {
    let Geometry { cell, pad, scale } = geometry;
    assert!((1.0..=4.0).contains(&scale));
    assert!(cell.0 > 0.0 && cell.1 > 0.0 && pad >= 0.0 && width >= 1);
    let x = (pad + column as f64 * cell.0) / scale;
    let y = (pad + row as f64 * cell.1) / scale;
    let rect = (x, y, width as f64 * cell.0 / scale, cell.1 / scale);
    assert!(rect.2 > 0.0 && rect.3 > 0.0);
    rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::screen::GridSize;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    fn term(columns: usize, lines: usize, bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize { columns, lines };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn select(term: &mut Term<VoidListener>, from: (i32, usize), to: (i32, usize)) {
        let start = Point::new(Line(from.0), Column(from.1));
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(Point::new(Line(to.0), Column(to.1)), Side::Right);
        term.selection = Some(selection);
    }

    #[test]
    fn wide_characters_emoji_and_zero_width_map_to_utf16_offsets() {
        let term = term(10, 3, "a界😀e\u{301}".as_bytes());
        let grid = visible(&term);
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(grid.columns, 10);
        assert_eq!(grid.cursor, Some((0, 6)));
        assert!(grid.rows[0][2].spacer);
        assert_eq!(grid.rows[0][5].text, "e\u{301}");
        let text = build(&grid);
        assert_eq!(text.string, "a界😀e\u{301}\n\n");
        assert_eq!(text.units(), 8);
        let at: Vec<usize> = (0..=6).map(|column| text.index_at(0, column)).collect();
        assert_eq!(at, vec![0, 1, 1, 2, 2, 4, 6]);
        assert_eq!(text.index_at(0, 10), 6);
        assert_eq!(text.index_at(1, 0), 7);
        assert_eq!(text.index_at(2, 0), 8);
        assert_eq!(text.cursor, 6);
        assert_eq!(text.selection, None);
    }

    #[test]
    fn blanks_are_trimmed_except_up_to_the_cursor() {
        let typing = build(&visible(&term(10, 2, b"ab  ")));
        assert_eq!(typing.string, "ab  \n");
        assert_eq!(typing.cursor, 4);
        let next = build(&visible(&term(10, 3, b"ab   \r\nx")));
        assert_eq!(next.string, "ab\nx\n");
        assert_eq!(next.cursor, 4);
        assert_eq!(next.index_at(0, 7), 2);
        let parked = build(&visible(&term(10, 2, b"ab\x1b[1;6H")));
        assert_eq!(parked.string, "ab   \n");
        assert_eq!(parked.cursor, 5);
        let hidden = build(&visible(&term(10, 2, b"\x1b[?25lab")));
        assert_eq!(hidden.cursor, 2);
    }

    #[test]
    fn lines_and_ranges_follow_the_newlines() {
        let text = build(&visible(&term(10, 3, b"ab\r\nx")));
        assert_eq!(text.string, "ab\nx\n");
        let lines: Vec<usize> = (0..=7).map(|index| text.line_for_index(index)).collect();
        assert_eq!(lines, vec![0, 0, 0, 1, 1, 2, 2, 2]);
        assert_eq!(text.range_for_line(0), Some((0, 3)));
        assert_eq!(text.range_for_line(1), Some((3, 2)));
        assert_eq!(text.range_for_line(2), Some((5, 0)));
        assert_eq!(text.range_for_line(3), None);
        assert_eq!(
            text.string_for_range((1, 3)),
            Some(("b\nx".to_owned(), (1, 3)))
        );
        assert_eq!(
            text.string_for_range((4, 9)),
            Some(("\n".to_owned(), (4, 1)))
        );
        assert_eq!(text.string_for_range((6, 1)), None);
    }

    #[test]
    fn the_selection_maps_to_a_utf16_range_over_wide_cells_and_lines() {
        let mut wide = term(10, 2, "a界😀e".as_bytes());
        select(&mut wide, (0, 1), (0, 4));
        let text = build(&visible(&wide));
        assert_eq!(text.selection, Some((1, 3)));
        assert_eq!(
            text.string_for_range((1, 3)).map(|s| s.0),
            Some("界😀".to_owned())
        );
        let mut lines = term(10, 2, b"ab\r\nx");
        select(&mut lines, (0, 1), (1, 0));
        assert_eq!(build(&visible(&lines)).selection, Some((1, 3)));
        let mut tail = term(10, 2, b"ab\r\nx");
        select(&mut tail, (0, 0), (0, 9));
        assert_eq!(build(&visible(&tail)).selection, Some((0, 2)));
    }

    #[test]
    fn a_selection_ending_on_a_wide_characters_left_cell_keeps_it() {
        let mut wide = term(10, 2, "a界😀e".as_bytes());
        select(&mut wide, (0, 1), (0, 3));
        assert_eq!(visible(&wide).selection, Some(((0, 1), (0, 3))));
        assert_eq!(build(&visible(&wide)).selection, Some((1, 3)));
        select(&mut wide, (0, 0), (0, 1));
        assert_eq!(build(&visible(&wide)).selection, Some((0, 2)));
    }

    #[test]
    fn the_text_key_changes_exactly_when_the_text_voiceover_reads_does() {
        let inputs: [&[u8]; 12] = [
            b"ab",
            b"\x1b[31mab",
            b"ab\r\n\x1b[41m  \x1b[1;3H",
            b"ac",
            b"e",
            "e\u{301}".as_bytes(),
            b"ab  ",
            b"abc",
            b"abc\x1b[1;2H",
            b"\x1b[?25lab\x1b[1;6H",
            b"ab\r\n",
            b"a\r\nb",
        ];
        let terms: Vec<_> = inputs.iter().map(|bytes| term(10, 2, bytes)).collect();
        for a in &terms {
            for b in &terms {
                let same = build(&visible(a)).string == build(&visible(b)).string;
                assert_eq!(text_key(a) == text_key(b), same);
            }
        }
        let mut selected = term(10, 2, b"ab");
        select(&mut selected, (0, 0), (0, 1));
        assert_eq!(text_key(&selected), text_key(&terms[0]));
    }

    #[test]
    fn a_range_covers_cells_on_its_first_line() {
        let text = build(&visible(&term(10, 2, "a界😀e\r\nxy".as_bytes())));
        assert_eq!(text.cells_for_range((1, 3)), Some((0, 1, 5)));
        assert_eq!(text.cells_for_range((0, 0)), Some((0, 0, 1)));
        assert_eq!(text.cells_for_range((4, 3)), Some((0, 5, 6)));
        assert_eq!(text.cells_for_range((6, 1)), Some((1, 0, 1)));
        assert_eq!(text.cells_for_range((7, 1)), Some((1, 1, 2)));
        assert_eq!(text.cells_for_range((5, 0)), Some((0, 6, 7)));
        assert_eq!(text.cells_for_range((99, 1)), None);
    }

    #[test]
    fn cells_become_view_points_at_the_display_scale() {
        let geometry = Geometry {
            cell: (16.0, 32.0),
            pad: 8.0,
            scale: 2.0,
        };
        assert_eq!(cell_rect(geometry, 0, 0, 1), (4.0, 4.0, 8.0, 16.0));
        assert_eq!(cell_rect(geometry, 1, 2, 3), (20.0, 20.0, 24.0, 16.0));
    }
}
