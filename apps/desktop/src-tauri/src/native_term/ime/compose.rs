use super::{COMMIT_BYTES_MAX, Ime, MARKED_UNITS_MAX, units};
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

/// The flags and struct size above are IMM32's own, checked where IMM32 is built.
#[cfg(windows)]
const _: () = {
    use windows_sys::Win32::UI::Input::Ime::{
        GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, IMECHARPOSITION,
    };
    assert!(COMPOSING == GCS_COMPSTR);
    assert!(CARET == GCS_CURSORPOS);
    assert!(RESULT == GCS_RESULTSTR);
    assert!(size_of::<IMECHARPOSITION>() == CHAR_POSITION_BYTES as usize);
};

/// The reads a WM_IME_COMPOSITION's flags ask for: the result when one is committed, the
/// composition string when it changed or when the message names neither a result nor a
/// caret (an IME clearing its composition sends no flags), and the caret with the string or
/// alone. Input: the flags (its lParam). Output: the reads.
pub fn reads(flags: u32) -> Reads {
    let result = flags & RESULT != 0;
    let caret = flags & CARET != 0;
    let marked = flags & COMPOSING != 0 || flags & (RESULT | CARET) == 0;
    let reads = Reads {
        result,
        marked,
        caret,
    };
    assert!(reads.result || reads.marked || reads.caret);
    reads
}

/// The UTF-16 units to allocate for a string ImmGetCompositionStringW measured, held to a
/// cap before any allocation: none for an error (a negative result) or an odd byte count,
/// the cap for anything past it. Inputs: the measured bytes and the cap in units. Output:
/// the units, or none.
pub fn wanted_units(bytes: i32, cap: usize) -> Option<usize> {
    let bytes = usize::try_from(bytes).ok()?;
    if bytes % 2 != 0 {
        return None;
    }
    let wanted = (bytes / 2).min(cap);
    assert!(wanted <= cap);
    Some(wanted)
}

/// The UTF-16 units a read copied, never trusted past its buffer: none for an error, an odd
/// byte count or more units than the buffer holds. Inputs: the copied bytes and the
/// buffer's units. Output: the units, or none.
pub fn copied_units(bytes: i32, buffer: usize) -> Option<usize> {
    let bytes = usize::try_from(bytes).ok()?;
    let copied = (bytes % 2 == 0)
        .then_some(bytes / 2)
        .filter(|units| *units <= buffer)?;
    assert!(copied <= buffer);
    Some(copied)
}

/// A composition string's text from its UTF-16 units, any surrogate without its pair
/// dropped, since a read cut at its cap can end inside a pair. Input: the units. Output:
/// the text.
pub fn text_of(read: &[u16]) -> String {
    let text: String = char::decode_utf16(read.iter().copied())
        .filter_map(Result::ok)
        .collect();
    assert!(units(&text) <= read.len());
    text
}

/// The caret GCS_CURSORPOS reads, held inside the composition: an offset past its end, or
/// an error (a negative result), puts it at the end. Inputs: the read and the
/// composition's units. Output: the caret offset.
pub fn caret(read: i32, marked_units: usize) -> usize {
    let caret = usize::try_from(read).map_or(marked_units, |at| at.min(marked_units));
    assert!(caret <= marked_units);
    caret
}

/// Whether an IMECHARPOSITION's dwSize holds the whole struct, so the view may read its
/// offset and write its answer. Input: the size. Output: whether it fits.
pub fn fits(size: u32) -> bool {
    size >= CHAR_POSITION_BYTES
}

/// The character offset IMR_QUERYCHARPOSITION may be answered for: none with no marked
/// text or for an offset past it; its end is allowed, the caret after the last character.
/// Inputs: the asked dwCharPos and the marked text. Output: the offset, or none.
pub fn char_offset(offset: u32, ime: &Ime) -> Option<usize> {
    let (_, length) = ime.marked_range()?;
    let offset = usize::try_from(offset).ok().filter(|at| *at <= length)?;
    assert!(offset <= length);
    Some(offset)
}

/// The cell a character of the marked text is drawn in: the drawn preedit's row, and its
/// column plus the cells before the character, held to the cell after the drawn ones (the
/// screen clips marked text at the row's end); the cursor cell when nothing is drawn.
/// Inputs: the cursor cell, the drawn preedit and the cells before the character. Output:
/// the cell's row and column.
pub fn char_cell(
    cursor: (usize, usize),
    preedit: Option<Preedit>,
    before: usize,
) -> (usize, usize) {
    let Some(drawn) = preedit else {
        return cursor;
    };
    let column = drawn.column + before.min(drawn.cells);
    assert!(column >= drawn.column);
    assert!(column <= drawn.column + drawn.cells);
    (drawn.row, column)
}

/// Applies one composition message to a view's marked text. A view that is not active
/// sends nothing and keeps no marked text. Else a result commits first, through
/// `Ime::commit` and so `text_inputs` and its caps; then a new string marks with its caret
/// (at its end when unread), a caret alone moves, and with neither the caret stays. Inputs:
/// the marked text, whether the view is active and the read. Output: what it did.
pub fn compose(ime: &mut Ime, active: bool, read: Read) -> Composed {
    if !active {
        let redraw = ime.cancel();
        assert!(!ime.composing());
        return Composed {
            inputs: Vec::new(),
            redraw,
        };
    }
    let before = (ime.marked().to_owned(), ime.selected_range());
    let inputs = read
        .result
        .map(|text| ime.commit(&text))
        .unwrap_or_default();
    let (text, kept) = match read.marked {
        Some(text) => (text, None),
        None => (
            ime.marked().to_owned(),
            ime.selected_range().map(|(at, _)| at),
        ),
    };
    let length = units(&text);
    let at = match read.caret {
        Some(read) => caret(read, length),
        None => kept.unwrap_or(length),
    };
    assert!(at <= length);
    ime.set_marked(&text, (at, 0));
    assert!(ime.selected_range().is_none_or(|(at, _)| at <= length));
    let redraw = before != (ime.marked().to_owned(), ime.selected_range());
    Composed { inputs, redraw }
}

#[cfg(test)]
mod tests {
    use super::super::text_inputs;
    use super::*;
    use kernel_processes::terminal::INPUT_BYTES_MAX;

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
        assert_eq!(
            compose(&mut ime, true, Read::default()),
            Composed::default()
        );
        assert_eq!(
            ime.selected_range(),
            Some((0, 0)),
            "a refused read keeps the caret"
        );
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
