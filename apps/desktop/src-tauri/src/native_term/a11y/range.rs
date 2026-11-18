use super::{Geometry, Text};

/// A text unit a UI Automation range moves or expands by: one cell's characters, one line
/// with its newline, or the whole text. UIA's other units map onto these (Format and Word
/// onto Line, Paragraph and Page onto Document).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Character,
    Line,
    Document,
}

impl Text {
    /// An endpoint held over a text change: clamped to the text and moved back to the start
    /// of the character it falls in. Input: a UTF-16 index. Output: a character boundary.
    pub fn snap(&self, index: usize) -> usize {
        index
    }

    /// The unit holding an index, as start and end; at the text's end a Character is empty
    /// there and a Line is the last line. Inputs: the UTF-16 index and the unit. Output: the
    /// two endpoints, start at or before end.
    pub fn enclosing(&self, index: usize, unit: Unit) -> (usize, usize) {
        let _ = unit;
        (index, index)
    }

    /// Moves an endpoint by whole units, stopping at either end of the text. Inputs: the
    /// UTF-16 index, the unit and the signed count. Output: the new index and how many units
    /// it moved, with the count's sign.
    pub fn moved(&self, index: usize, unit: Unit, count: i32) -> (usize, i32) {
        let _ = (unit, count);
        (index, 0)
    }

    /// The cells of a range line by line, for its bounding rectangles: each line holding at
    /// least one of its characters, the newline left out. Inputs: the start and end. Output:
    /// row, first and past-the-last column per line; none for an empty range.
    pub fn lines_of(&self, start: usize, end: usize) -> Vec<(usize, usize, usize)> {
        let _ = (start, end);
        Vec::new()
    }

    /// The selection as start and end, or an empty range at the cursor when nothing is
    /// selected. Output: the two endpoints.
    pub fn marked(&self) -> (usize, usize) {
        (0, 0)
    }
}

/// The cell nearest a point in view points (y grows down), for a range from a point: inside
/// the grid the cell under it, outside it the nearest edge cell, a point that is not finite
/// the first cell. Inputs: the geometry, the point, and the grid's rows and columns. Output:
/// the row and column.
pub fn cell_near(
    geometry: Geometry,
    point: (f64, f64),
    rows: usize,
    columns: usize,
) -> (usize, usize) {
    let _ = (geometry, point, rows, columns);
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::super::{build, visible};
    use super::*;
    use crate::native_term::screen::GridSize;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    fn text_of(columns: usize, lines: usize, bytes: &[u8]) -> Text {
        build(&visible(&term(columns, lines, bytes)))
    }

    fn term(columns: usize, lines: usize, bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize { columns, lines };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn characters_step_over_wide_cells_surrogate_pairs_and_marks() {
        let text = text_of(10, 3, "a界😀e\u{301}".as_bytes());
        assert_eq!(text.string, "a界😀e\u{301}\n\n");
        assert_eq!(text.moved(0, Unit::Character, 4), (6, 4));
        assert_eq!(text.moved(6, Unit::Character, 5), (8, 2));
        assert_eq!(text.moved(8, Unit::Character, -3), (4, -3));
        assert_eq!(text.moved(0, Unit::Character, -1), (0, 0));
        assert_eq!(text.snap(3), 2);
        assert_eq!(text.snap(5), 4);
        assert_eq!(text.snap(99), 8);
        assert_eq!(text.enclosing(3, Unit::Character), (2, 4));
        assert_eq!(text.enclosing(6, Unit::Character), (6, 7));
        assert_eq!(text.enclosing(8, Unit::Character), (8, 8));
    }

    #[test]
    fn lines_and_the_document_move_and_expand_with_their_newlines() {
        let text = text_of(10, 3, b"ab\r\ncd");
        assert_eq!(text.string, "ab\ncd\n");
        assert_eq!(text.enclosing(4, Unit::Line), (3, 6));
        assert_eq!(text.enclosing(2, Unit::Line), (0, 3));
        assert_eq!(text.enclosing(6, Unit::Line), (6, 6));
        assert_eq!(text.moved(0, Unit::Line, 1), (3, 1));
        assert_eq!(text.moved(0, Unit::Line, 5), (6, 2));
        assert_eq!(text.moved(6, Unit::Line, -1), (3, -1));
        assert_eq!(text.moved(4, Unit::Line, -1), (3, -1));
        assert_eq!(text.enclosing(2, Unit::Document), (0, 6));
        assert_eq!(text.moved(2, Unit::Document, 1), (6, 1));
        assert_eq!(text.moved(2, Unit::Document, -1), (0, -1));
        assert_eq!(text.moved(0, Unit::Document, -1), (0, 0));
    }

    #[test]
    fn a_count_past_the_text_stops_at_its_ends_and_an_old_index_snaps_into_a_shorter_text() {
        let text = text_of(10, 2, b"abc");
        assert_eq!(text.string, "abc\n");
        assert_eq!(text.moved(0, Unit::Character, i32::MAX), (4, 4));
        assert_eq!(text.moved(4, Unit::Character, i32::MIN), (0, -4));
        assert_eq!(text.moved(40, Unit::Line, -1), (0, -1));
        assert_eq!(text.snap(40), 4);
        let empty = Text::default();
        assert_eq!(empty.moved(0, Unit::Line, 3), (0, 0));
        assert_eq!(empty.enclosing(5, Unit::Line), (0, 0));
        assert_eq!(empty.lines_of(0, 5), Vec::new());
    }

    #[test]
    fn bounds_go_line_by_line_and_leave_the_newline_out() {
        let text = text_of(10, 3, b"ab\r\ncd");
        assert_eq!(text.lines_of(0, 6), vec![(0, 0, 2), (1, 0, 2)]);
        assert_eq!(text.lines_of(1, 4), vec![(0, 1, 2), (1, 0, 1)]);
        assert_eq!(text.lines_of(2, 3), Vec::new());
        assert_eq!(text.lines_of(3, 3), Vec::new());
    }

    #[test]
    fn the_selection_or_an_empty_range_at_the_cursor_is_what_is_marked() {
        let text = text_of(10, 3, b"ab\r\ncd");
        assert_eq!(text.cursor, 5);
        assert_eq!(text.marked(), (5, 5));
        let mut held = term(10, 3, b"ab\r\ncd");
        let start = Point::new(Line(0), Column(1));
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(Point::new(Line(1), Column(0)), Side::Right);
        held.selection = Some(selection);
        let selected = build(&visible(&held));
        assert_eq!(selected.marked(), (1, 4));
    }

    #[test]
    fn a_point_takes_the_cell_under_it_or_the_nearest_edge_cell() {
        let geometry = Geometry {
            cell: (16.0, 32.0),
            pad: 0.0,
            scale: 2.0,
        };
        assert_eq!(cell_near(geometry, (8.0, 16.0), 3, 10), (1, 1));
        assert_eq!(cell_near(geometry, (7.9, 15.9), 3, 10), (0, 0));
        assert_eq!(cell_near(geometry, (-5.0, -5.0), 3, 10), (0, 0));
        assert_eq!(cell_near(geometry, (1e9, 1e9), 3, 10), (2, 9));
        assert_eq!(cell_near(geometry, (f64::NAN, 20.0), 3, 10), (0, 0));
        assert_eq!(cell_near(geometry, (f64::INFINITY, 20.0), 3, 10), (0, 0));
    }
}
