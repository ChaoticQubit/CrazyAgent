use super::{COMMIT_BYTES_MAX, Ime, MARKED_UNITS_MAX, text_inputs, units};
use crate::native_term::pane::Input;
use crate::native_term::screen::Preedit;

/// IMM32's GCS_COMPSTR: the composition string changed.
pub const COMPOSING: u32 = 0x0008;

/// IMM32's GCS_CURSORPOS: the caret inside the composition string moved.
pub const CARET: u32 = 0x0080;

/// IMM32's GCS_RESULTSTR: the input method committed a result.
pub const RESULT: u32 = 0x0800;

/// The bytes of IMM32's IMECHARPOSITION: its size, character offset, point, line height and
/// document rectangle (4 + 4 + 8 + 4 + 16).
pub const CHAR_POSITION_BYTES: u32 = 36;

/// The most UTF-16 units read for a composition string, the marked text's own cap.
pub const MARKED_READ_UNITS: usize = MARKED_UNITS_MAX;

/// The most UTF-16 units read for a result string: `COMMIT_BYTES_MAX` bytes of UTF-16.
pub const RESULT_READ_UNITS: usize = COMMIT_BYTES_MAX / 2;

/// What one WM_IME_COMPOSITION asks the view to read: the result, the composition string
/// and the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reads {
    pub result: bool,
    pub marked: bool,
    pub caret: bool,
}

/// What one composition message read: the committed result, the new composition string and
/// the caret, each only when the message asked for it.
#[derive(Debug, Default)]
pub struct Read {
    pub result: Option<String>,
    pub marked: Option<String>,
    pub caret: Option<i32>,
}

/// What a composition message did: the inputs to send and whether the marked text or its
/// caret changed, so the view asks for a redraw.
#[derive(Debug, Default, PartialEq)]
pub struct Composed {
    pub inputs: Vec<Input>,
    pub redraw: bool,
}

/// The reads a WM_IME_COMPOSITION's flags ask for. Input: the flags (its lParam). Output:
/// the reads.
pub fn reads(flags: u32) -> Reads {
    let _ = flags;
    Reads {
        result: false,
        marked: false,
        caret: false,
    }
}

/// The UTF-16 units to allocate for a string ImmGetCompositionStringW measured. Inputs: the
/// measured bytes and the cap in units. Output: the units, or none.
pub fn wanted_units(bytes: i32, cap: usize) -> Option<usize> {
    let _ = cap;
    usize::try_from(bytes).ok().map(|bytes| bytes / 2)
}

/// The UTF-16 units a read copied. Inputs: the copied bytes and the buffer's units. Output:
/// the units, or none.
pub fn copied_units(bytes: i32, buffer: usize) -> Option<usize> {
    let _ = buffer;
    usize::try_from(bytes).ok().map(|bytes| bytes / 2)
}

/// A composition string's text from its UTF-16 units. Input: the units. Output: the text.
pub fn text_of(read: &[u16]) -> String {
    String::from_utf16_lossy(read)
}

/// The caret GCS_CURSORPOS reads, inside the composition. Inputs: the read and the
/// composition's units. Output: the caret offset.
pub fn caret(read: i32, marked_units: usize) -> usize {
    let _ = marked_units;
    read.unsigned_abs() as usize
}

/// Whether an IMECHARPOSITION's dwSize holds the whole struct. Input: the size. Output:
/// whether the view may read and write it.
pub fn fits(size: u32) -> bool {
    let _ = size;
    true
}

/// The character offset IMR_QUERYCHARPOSITION may be answered for. Inputs: the asked
/// dwCharPos and the marked text. Output: the offset, or none.
pub fn char_offset(offset: u32, ime: &Ime) -> Option<usize> {
    let _ = ime;
    usize::try_from(offset).ok()
}

/// The cell a character of the marked text is drawn in. Inputs: the cursor cell, the drawn
/// preedit and the cells before the character. Output: the cell's row and column.
pub fn char_cell(
    cursor: (usize, usize),
    preedit: Option<Preedit>,
    before: usize,
) -> (usize, usize) {
    let _ = (preedit, before);
    cursor
}

/// Applies one composition message to a view's marked text. Inputs: the marked text,
/// whether the view is active and the read. Output: what it did.
pub fn compose(ime: &mut Ime, active: bool, read: Read) -> Composed {
    let _ = (active, text_inputs, units);
    let inputs = read
        .result
        .map(|text| vec![Input::Text(text.into_bytes())])
        .unwrap_or_default();
    if let Some(text) = read.marked {
        ime.set_marked(&text, (0, 0));
    }
    Composed {
        inputs,
        redraw: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crazyagent::terminal::INPUT_BYTES_MAX;

    fn marked(text: &str, caret: usize) -> Ime {
        let mut ime = Ime::default();
        assert!(ime.set_marked(text, (caret, 0)));
        ime
    }

    #[test]
    fn composition_flags_ask_for_the_result_the_string_and_the_caret() {
        let all = Reads {
            result: true,
            marked: true,
            caret: true,
        };
        assert_eq!(reads(RESULT | COMPOSING | CARET), all);
        let only = |result, marked, caret| Reads {
            result,
            marked,
            caret,
        };
        assert_eq!(
            reads(0),
            only(false, true, false),
            "no flags: the IME cleared it"
        );
        assert_eq!(reads(COMPOSING), only(false, true, false));
        assert_eq!(reads(CARET), only(false, false, true));
        assert_eq!(reads(RESULT), only(true, false, false));
        assert_eq!(reads(RESULT | COMPOSING), only(true, true, false));
        assert_eq!(
            reads(0x0010),
            only(false, true, false),
            "attributes alone re-read it"
        );
    }

    #[test]
    fn ime_string_sizes_are_refused_or_capped_before_any_allocation() {
        assert_eq!(
            wanted_units(-1, MARKED_READ_UNITS),
            None,
            "IMM_ERROR_NODATA"
        );
        assert_eq!(
            wanted_units(-2, MARKED_READ_UNITS),
            None,
            "IMM_ERROR_GENERAL"
        );
        assert_eq!(wanted_units(i32::MIN, RESULT_READ_UNITS), None);
        assert_eq!(
            wanted_units(7, MARKED_READ_UNITS),
            None,
            "an odd byte count"
        );
        assert_eq!(wanted_units(0, MARKED_READ_UNITS), Some(0));
        assert_eq!(wanted_units(6, MARKED_READ_UNITS), Some(3));
        let past_marked = i32::try_from(2 * (MARKED_UNITS_MAX + 1)).expect("small");
        assert_eq!(
            wanted_units(past_marked, MARKED_READ_UNITS),
            Some(MARKED_UNITS_MAX)
        );
        let past_result = i32::try_from(COMMIT_BYTES_MAX + 2).expect("small");
        assert_eq!(
            wanted_units(past_result, RESULT_READ_UNITS),
            Some(COMMIT_BYTES_MAX / 2)
        );
        assert_eq!(
            wanted_units(i32::MAX - 1, RESULT_READ_UNITS),
            Some(RESULT_READ_UNITS)
        );
        assert_eq!(copied_units(-1, 4), None);
        assert_eq!(copied_units(5, 4), None, "an odd byte count");
        assert_eq!(copied_units(10, 4), None, "more than the buffer holds");
        assert_eq!(copied_units(8, 4), Some(4));
        assert_eq!(copied_units(0, 4), Some(0));
        assert_eq!(text_of(&[0x61, 0xD83D]), "a", "a read cut inside a pair");
        assert_eq!(text_of(&[0xDE0A, 0x62]), "b");
        assert_eq!(text_of(&[0xD83D, 0xDE0A]), "\u{1F60A}");
    }

    #[test]
    fn the_caret_past_the_composition_clamps_to_its_end() {
        assert_eq!(caret(5, 3), 3);
        assert_eq!(caret(i32::MAX, 3), 3);
        assert_eq!(caret(-1, 3), 3, "an error puts it at the end");
        assert_eq!(caret(1, 3), 1);
        assert_eq!(caret(0, 0), 0);
        let mut ime = Ime::default();
        let read = Read {
            marked: Some("かな".into()),
            caret: Some(99),
            ..Read::default()
        };
        assert!(compose(&mut ime, true, read).redraw);
        assert_eq!(ime.selected_range(), Some((2, 0)));
    }

    #[test]
    fn a_char_position_query_too_small_or_past_the_marked_text_is_refused() {
        assert!(!fits(0));
        assert!(!fits(CHAR_POSITION_BYTES - 1));
        assert!(fits(CHAR_POSITION_BYTES));
        assert!(fits(CHAR_POSITION_BYTES + 8));
        let ime = marked("かな", 2);
        assert_eq!(char_offset(0, &ime), Some(0));
        assert_eq!(
            char_offset(2, &ime),
            Some(2),
            "the caret after the last character"
        );
        assert_eq!(char_offset(3, &ime), None);
        assert_eq!(char_offset(u32::MAX, &ime), None);
        assert_eq!(char_offset(0, &Ime::default()), None, "nothing is marked");
    }

    #[test]
    fn an_inactive_view_commits_nothing_and_keeps_no_marked_text() {
        let mut ime = marked("か", 1);
        let read = Read {
            result: Some("x".into()),
            marked: Some("かな".into()),
            caret: Some(2),
        };
        let done = compose(&mut ime, false, read);
        assert_eq!(done.inputs, Vec::new());
        assert!(done.redraw, "the marked text it held goes");
        assert!(!ime.composing());
        let read = Read {
            marked: Some("か".into()),
            ..Read::default()
        };
        let done = compose(&mut ime, false, read);
        assert_eq!(done, Composed::default());
        assert!(!ime.composing());
    }

    #[test]
    fn a_commit_with_controls_or_escape_goes_through_the_text_caps() {
        let mut ime = Ime::default();
        let typed = "a\u{1b}[2J\u{3}b";
        let read = Read {
            result: Some(typed.into()),
            ..Read::default()
        };
        let done = compose(&mut ime, true, read);
        assert_eq!(done.inputs, text_inputs(typed));
        assert_eq!(done.inputs, vec![Input::Text(typed.as_bytes().to_vec())]);
        let flood = "\u{1b}".repeat(COMMIT_BYTES_MAX + 10);
        let read = Read {
            result: Some(flood),
            ..Read::default()
        };
        let done = compose(&mut ime, true, read);
        let mut total = 0usize;
        for input in &done.inputs {
            let Input::Text(bytes) = input else {
                panic!("a commit is text only: {input:?}");
            };
            assert!(!bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX);
            total += bytes.len();
        }
        assert_eq!(total, COMMIT_BYTES_MAX);
    }

    #[test]
    fn a_result_commits_before_the_new_composition_marks() {
        let mut ime = marked("にほん", 3);
        let read = Read {
            result: Some("日本".into()),
            marked: Some("ご".into()),
            caret: Some(1),
        };
        let done = compose(&mut ime, true, read);
        assert_eq!(done.inputs, text_inputs("日本"));
        assert!(done.redraw);
        assert_eq!(ime.marked(), "ご");
        assert_eq!(ime.selected_range(), Some((1, 0)));
        let moved = Read {
            caret: Some(0),
            ..Read::default()
        };
        assert!(compose(&mut ime, true, moved).redraw);
        assert_eq!((ime.marked(), ime.selected_range()), ("ご", Some((0, 0))));
        let same = Read {
            caret: Some(0),
            ..Read::default()
        };
        assert_eq!(compose(&mut ime, true, same), Composed::default());
        let cleared = Read {
            marked: Some(String::new()),
            ..Read::default()
        };
        assert!(compose(&mut ime, true, cleared).redraw);
        assert!(!ime.composing());
    }

    #[test]
    fn character_cells_follow_the_drawn_preedit_and_stop_at_its_end() {
        let drawn = Preedit {
            row: 3,
            column: 5,
            cells: 4,
        };
        assert_eq!(char_cell((3, 5), Some(drawn), 0), (3, 5));
        assert_eq!(char_cell((3, 5), Some(drawn), 2), (3, 7));
        assert_eq!(
            char_cell((3, 5), Some(drawn), 4),
            (3, 9),
            "after the last cell"
        );
        assert_eq!(
            char_cell((3, 5), Some(drawn), 9),
            (3, 9),
            "clipped text stops at the end"
        );
        assert_eq!(
            char_cell((7, 1), None, 3),
            (7, 1),
            "nothing drawn: the cursor cell"
        );
    }
}
