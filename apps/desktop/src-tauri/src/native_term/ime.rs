use super::pane::Input;
use crazyagent::terminal::INPUT_BYTES_MAX;
use unicode_width::UnicodeWidthChar;

/// The most UTF-16 units of marked text a view keeps; an input method never composes this
/// much, and a longer marked text is kept cut at a character boundary.
pub const MARKED_UNITS_MAX: usize = 1024;

/// The most bytes one committed text sends (dictation and the emoji picker commit text,
/// typing never this much); a longer commit is cut at a character boundary.
pub const COMMIT_BYTES_MAX: usize = 1 << 20;

/// Apple's private-use range for function keys, which never reaches a pane as text.
const FUNCTION_CHARS: std::ops::RangeInclusive<char> = '\u{F700}'..='\u{F8FF}';

/// The modifier keys of one key event that decide its route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub command: bool,
    pub control: bool,
    pub option: bool,
}

/// Where one key event goes: nowhere (Command keys go through the key equivalents), to the
/// pane at once as the key map's bytes (Control bytes, Option as Meta), or through the input
/// method, whose passed-back commands (Return, Shift+Enter, Tab, arrows) send the key map's
/// bytes too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Ignore,
    Direct,
    Interpret,
}

/// The route of one key event: Command keys are ignored; while composing every other key
/// belongs to the input method; else Control and Option keys go straight to the key map and
/// the rest go through the input method. Inputs: the modifiers and whether marked text was
/// shown before the key. Output: the route.
pub fn route(mods: Mods, composing: bool) -> Route {
    let route = if mods.command {
        Route::Ignore
    } else if composing {
        Route::Interpret
    } else if mods.control || mods.option {
        Route::Direct
    } else {
        Route::Interpret
    };
    assert!(!mods.command || route == Route::Ignore);
    assert!(!composing || matches!(route, Route::Ignore | Route::Interpret));
    assert!(route != Route::Direct || mods.control || mods.option);
    route
}

/// What the input method did with one interpreted key: whether it inserted or marked text,
/// whether it passed a command back (Return, Tab, Backspace, Escape, arrows arrive so), and
/// whether marked text was shown before and after the key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Took {
    pub handled: bool,
    pub command: bool,
    pub before: bool,
    pub after: bool,
}

/// Whether an interpreted key still sends its own bytes once the input method returned:
/// never while marked text stays shown; after a composition only when the input method
/// passed a command back (Korean commits the syllable, then Return or an arrow goes on);
/// otherwise when it passed a command or took nothing. Input: what it did. Output: true to
/// send the key's own bytes after any committed text.
pub fn own_bytes(took: Took) -> bool {
    let sends = if took.after {
        false
    } else if took.before {
        took.command
    } else {
        took.command || !took.handled
    };
    assert!(!sends || !took.after);
    assert!(!sends || took.command || !took.handled);
    sends
}

/// The text inputs one committed text makes: Apple's private-use function characters
/// dropped, the rest cut at character boundaries into inputs of at most `INPUT_BYTES_MAX`
/// bytes, so nothing is lost up to `COMMIT_BYTES_MAX` in all; text past that bound is not
/// sent. Input: the text. Output: the inputs, none for an empty text.
pub fn text_inputs(text: &str) -> Vec<Input> {
    let chunk_max = INPUT_BYTES_MAX;
    let mut inputs = Vec::new();
    let mut chunk = String::new();
    let mut total = 0usize;
    for c in text.chars().filter(|c| !FUNCTION_CHARS.contains(c)) {
        if total + c.len_utf8() > COMMIT_BYTES_MAX {
            break;
        }
        if chunk.len() + c.len_utf8() > chunk_max {
            inputs.push(Input::Text(std::mem::take(&mut chunk).into_bytes()));
        }
        chunk.push(c);
        total += c.len_utf8();
    }
    if !chunk.is_empty() {
        inputs.push(Input::Text(chunk.into_bytes()));
    }
    assert!(total <= COMMIT_BYTES_MAX);
    assert!(inputs.iter().all(|input| matches!(
        input,
        Input::Text(bytes) if !bytes.is_empty() && bytes.len() <= chunk_max
    )));
    inputs
}

/// The cells a character takes in the terminal grid: 0 when zero-width, else 1 or 2, since
/// the grid draws every character wider than one cell (U+17D8 is 3) across two cells.
/// Input: the character. Output: its cells.
pub fn char_cells(c: char) -> usize {
    c.width().unwrap_or(0).min(2)
}

/// The UTF-16 length of a text, the unit of every AppKit text range. Input: the text.
/// Output: its length in UTF-16 units.
pub fn units(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The part of a text a UTF-16 range asks for, clamped to the text and widened to whole
/// characters. Inputs: the text and the location and length. Output: the slice and the
/// range it covers, or none when the location is past the end, or at the end with a length.
pub fn slice_units(text: &str, range: (usize, usize)) -> Option<(String, (usize, usize))> {
    let total = units(text);
    let (location, length) = range;
    if location > total || (location == total && length > 0) {
        return None;
    }
    let end = location.saturating_add(length).min(total);
    let mut out = String::new();
    let (mut first, mut last) = (location, location);
    let mut at = 0usize;
    for c in text.chars() {
        let next = at + c.len_utf16();
        if next > location && at < end {
            if out.is_empty() {
                first = at;
            }
            out.push(c);
            last = next;
        }
        at = next;
    }
    assert_eq!(at, total);
    assert!(first <= location && last >= end);
    assert_eq!(units(&out), last - first);
    Some((out, (first, last - first)))
}

/// One view's marked text: what the input method composes, and the part of it the input
/// method selects, as a UTF-16 location and length inside the marked text.
#[derive(Debug, Default)]
pub struct Ime {
    marked: String,
    selected: (usize, usize),
}

impl Ime {
    /// Whether marked text is shown. Output: true while composing.
    pub fn composing(&self) -> bool {
        !self.marked.is_empty()
    }

    /// The marked text. Output: the text, empty when not composing.
    pub fn marked(&self) -> &str {
        &self.marked
    }

    /// Sets the marked text an input method composes; an empty text ends the composition
    /// with nothing sent. Inputs: the text and the selected UTF-16 range inside it. Output:
    /// whether marked text is shown after.
    pub fn set_marked(&mut self, text: &str, selected: (usize, usize)) -> bool {
        let mut kept = String::new();
        let mut total = 0usize;
        for c in text.chars() {
            if total + c.len_utf16() > MARKED_UNITS_MAX {
                break;
            }
            kept.push(c);
            total += c.len_utf16();
        }
        assert_eq!(units(&kept), total);
        let location = selected.0.min(total);
        let length = selected.1.min(total - location);
        self.marked = kept;
        self.selected = (location, length);
        assert!(self.selected.0 + self.selected.1 <= total);
        self.composing()
    }

    /// Commits a text the input method inserted, ending any composition. Input: the text.
    /// Output: the inputs to send.
    pub fn commit(&mut self, text: &str) -> Vec<Input> {
        self.cancel();
        assert!(!self.composing());
        text_inputs(text)
    }

    /// Commits the marked text itself (the input method asked to unmark it). Output: the
    /// inputs to send, none when not composing.
    pub fn unmark(&mut self) -> Vec<Input> {
        let marked = std::mem::take(&mut self.marked);
        self.selected = (0, 0);
        assert!(!self.composing());
        text_inputs(&marked)
    }

    /// Drops the marked text with nothing sent (the view lost focus, hid, closed or was
    /// clicked). Output: whether it was composing.
    pub fn cancel(&mut self) -> bool {
        let was = self.composing();
        self.marked.clear();
        self.selected = (0, 0);
        assert!(!self.composing());
        was
    }

    /// The marked range of the view's document, which is the marked text alone. Output:
    /// (0, length in UTF-16 units), or none when not composing.
    pub fn marked_range(&self) -> Option<(usize, usize)> {
        self.composing().then(|| (0, units(&self.marked)))
    }

    /// The selected range inside the marked text. Output: the UTF-16 location and length,
    /// or none when not composing.
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        self.composing().then_some(self.selected)
    }

    /// The part of the marked text a range asks for, clamped to it. Input: the UTF-16
    /// location and length. Output: the text and the range it covers, or none when the range
    /// misses the marked text.
    pub fn substring(&self, range: (usize, usize)) -> Option<(String, (usize, usize))> {
        if !self.composing() {
            return None;
        }
        slice_units(&self.marked, range)
    }

    /// How many terminal cells the marked text before a UTF-16 offset takes, so the input
    /// method's candidate window follows the composition; a character the offset falls inside
    /// counts whole. Input: the offset. Output: cells.
    pub fn cells_before(&self, offset: usize) -> usize {
        let mut at = 0usize;
        let mut cells = 0usize;
        for c in self.marked.chars() {
            if at >= offset {
                break;
            }
            at += c.len_utf16();
            cells += char_cells(c);
        }
        assert!(cells <= 2 * units(&self.marked));
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(input: &Input) -> &[u8] {
        match input {
            Input::Text(bytes) => bytes,
            _ => panic!("a text input"),
        }
    }

    fn mods(command: bool, control: bool, option: bool) -> Mods {
        Mods {
            command,
            control,
            option,
        }
    }

    #[test]
    fn keys_route_by_modifiers_and_composition() {
        assert_eq!(route(mods(false, false, false), false), Route::Interpret);
        assert_eq!(route(mods(false, true, false), false), Route::Direct);
        assert_eq!(route(mods(false, false, true), false), Route::Direct);
        assert_eq!(route(mods(false, true, true), false), Route::Direct);
        assert_eq!(route(mods(true, false, false), false), Route::Ignore);
        assert_eq!(route(mods(true, true, true), false), Route::Ignore);
        assert_eq!(route(mods(false, false, false), true), Route::Interpret);
        assert_eq!(route(mods(false, true, false), true), Route::Interpret);
        assert_eq!(route(mods(false, false, true), true), Route::Interpret);
        assert_eq!(route(mods(true, false, false), true), Route::Ignore);
    }

    #[test]
    fn a_key_sends_its_own_bytes_once_unless_the_input_method_kept_it() {
        let took = |handled, command, before, after| Took {
            handled,
            command,
            before,
            after,
        };
        assert!(
            own_bytes(took(false, true, false, false)),
            "Return, no composition"
        );
        assert!(
            own_bytes(took(false, false, false, false)),
            "a key nobody took"
        );
        assert!(
            !own_bytes(took(true, false, false, false)),
            "a typed letter"
        );
        assert!(
            !own_bytes(took(true, false, false, true)),
            "a dead key starts marking"
        );
        assert!(
            own_bytes(took(true, true, true, false)),
            "Korean: syllable, then Return"
        );
        assert!(
            !own_bytes(took(true, false, true, false)),
            "Japanese Return commits only"
        );
        assert!(
            !own_bytes(took(true, false, true, true)),
            "a conversion step"
        );
        assert!(
            !own_bytes(took(false, false, true, true)),
            "a candidate arrow"
        );
        assert!(
            !own_bytes(took(false, true, true, true)),
            "a command while still marked"
        );
        assert!(!own_bytes(took(true, false, true, false)), "Escape cancels");
    }

    #[test]
    fn committed_text_is_cut_at_character_boundaries_with_nothing_lost() {
        assert!(text_inputs("").is_empty());
        let one = text_inputs("日本語😀");
        assert_eq!(one.len(), 1);
        assert_eq!(text(&one[0]), "日本語😀".as_bytes());
        assert!(text_inputs("\u{F700}\u{F8FF}").is_empty());
        assert_eq!(text(&text_inputs("a\u{F701}b")[0]), b"ab");
        let long = "界".repeat(INPUT_BYTES_MAX);
        let parts = text_inputs(&long);
        assert!(parts.len() >= 3);
        let mut joined = Vec::new();
        for part in &parts {
            let bytes = text(part);
            assert!(!bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX);
            assert!(std::str::from_utf8(bytes).is_ok());
            joined.extend_from_slice(bytes);
        }
        assert_eq!(joined, long.as_bytes());
        let huge = "a".repeat(COMMIT_BYTES_MAX + 10);
        let sent: usize = text_inputs(&huge).iter().map(|part| text(part).len()).sum();
        assert_eq!(sent, COMMIT_BYTES_MAX);
    }

    #[test]
    fn marked_text_ranges_are_utf16_and_end_with_commit_or_cancel() {
        let mut ime = Ime::default();
        assert!(!ime.composing());
        assert_eq!(ime.marked_range(), None);
        assert_eq!(ime.selected_range(), None);
        assert!(ime.set_marked("に", (1, 0)));
        assert!(ime.set_marked("にほ😀", (4, 0)));
        assert_eq!(ime.marked_range(), Some((0, 4)));
        assert_eq!(ime.selected_range(), Some((4, 0)));
        assert!(ime.set_marked("ab", (9, 9)));
        assert_eq!(ime.selected_range(), Some((2, 0)));
        let sent = ime.commit("日本");
        assert!(!ime.composing());
        assert_eq!(sent.len(), 1);
        assert_eq!(text(&sent[0]), "日本".as_bytes());
        assert!(ime.set_marked("´", (1, 0)));
        assert!(!ime.set_marked("", (0, 0)));
        assert!(!ime.composing());
        assert!(ime.unmark().is_empty());
        assert!(ime.set_marked("かな", (2, 0)));
        let unmarked = ime.unmark();
        assert_eq!(text(&unmarked[0]), "かな".as_bytes());
        assert!(!ime.composing());
        assert!(ime.set_marked("か", (1, 0)));
        assert!(ime.cancel());
        assert!(!ime.cancel());
        assert!(!ime.composing());
    }

    #[test]
    fn marked_text_is_kept_bounded_and_sliced_by_utf16_range() {
        let mut ime = Ime::default();
        assert!(ime.set_marked(&"😀".repeat(MARKED_UNITS_MAX), (0, 0)));
        assert!(units(ime.marked()) <= MARKED_UNITS_MAX);
        assert_eq!(units(ime.marked()) % 2, 0);
        assert!(ime.set_marked("a😀b", (0, 0)));
        assert_eq!(ime.substring((1, 2)), Some(("😀".to_owned(), (1, 2))));
        assert_eq!(ime.substring((3, 10)), Some(("b".to_owned(), (3, 1))));
        assert_eq!(ime.substring((2, 1)), Some(("😀".to_owned(), (1, 2))));
        assert_eq!(ime.substring((4, 1)), None);
        assert!(ime.cancel());
        assert_eq!(ime.substring((0, 1)), None);
    }

    #[test]
    fn candidate_window_follows_wide_marked_cells() {
        let mut ime = Ime::default();
        assert!(ime.set_marked("aに😀b", (0, 0)));
        assert_eq!(ime.cells_before(0), 0);
        assert_eq!(ime.cells_before(1), 1);
        assert_eq!(ime.cells_before(2), 3);
        assert_eq!(ime.cells_before(4), 5);
        assert_eq!(ime.cells_before(5), 6);
        assert_eq!(ime.cells_before(99), 6);
    }

    #[test]
    fn a_marked_character_wider_than_two_cells_counts_two_like_the_grid() {
        assert_eq!('\u{17D8}'.width(), Some(3));
        let mut ime = Ime::default();
        assert!(ime.set_marked("\u{17D8}", (0, 0)));
        assert_eq!(ime.cells_before(1), 2);
        assert!(ime.set_marked("a\u{17D8}b", (0, 0)));
        assert_eq!(ime.cells_before(2), 3);
        assert_eq!(ime.cells_before(3), 4);
    }
}
