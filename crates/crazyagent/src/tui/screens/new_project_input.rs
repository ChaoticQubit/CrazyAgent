use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;

/// Measures how many terminal cells a text takes.
/// Takes any text without control characters.
/// Returns the display width in cells.
pub(in crate::tui) fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

/// Measures how many terminal cells one character takes.
/// Takes the character.
/// Returns its display width in cells.
pub(in crate::tui) fn char_cells(character: char) -> usize {
    let mut bytes = [0u8; 4];
    cells(character.encode_utf8(&mut bytes))
}

/// One editable text value with a cursor, for single-line and multi-line fields.
/// Holds the text, the cursor as a character index and a byte limit.
/// Never used for secrets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct TextInput {
    text: String,
    cursor: usize,
    multiline: bool,
    limit: usize,
}

impl TextInput {
    /// Makes an empty one-line field.
    /// Takes the most bytes the field accepts.
    /// Returns the field.
    pub fn single(limit: usize) -> Self {
        assert!(limit > 0);
        Self {
            text: String::new(),
            cursor: 0,
            multiline: false,
            limit,
        }
    }

    /// Makes an empty multi-line field where Ctrl-J adds a line.
    /// Takes the most bytes the field accepts.
    /// Returns the field.
    pub fn multi(limit: usize) -> Self {
        Self {
            multiline: true,
            ..Self::single(limit)
        }
    }

    /// Gives the current text.
    /// Takes the field.
    /// Returns the text without control characters other than newlines.
    pub fn value(&self) -> &str {
        &self.text
    }

    /// Gives the cursor position as a character index.
    /// Takes the field.
    /// Returns a value from zero to the number of characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the text and puts the cursor at its end.
    /// Takes untrusted text; control characters are dropped.
    /// Returns nothing.
    pub fn set(&mut self, value: &str) {
        self.text.clear();
        self.cursor = 0;
        self.insert_str(value);
    }

    /// Inserts typed or pasted text at the cursor.
    /// Takes untrusted text; tabs become spaces, single-line fields drop newlines, other control characters are dropped.
    /// Returns nothing; text past the byte limit is not inserted.
    pub fn insert_str(&mut self, raw: &str) {
        let normalized = raw.replace("\r\n", "\n");
        for character in normalized.chars() {
            let character = match character {
                '\t' => ' ',
                '\n' if self.multiline => '\n',
                c if c.is_control() => continue,
                c => c,
            };
            if self.text.len() + character.len_utf8() > self.limit {
                break;
            }
            let at = self.byte_at(self.cursor);
            self.text.insert(at, character);
            self.cursor += 1;
        }
        self.assert_consistent();
    }

    /// Applies one text-editing key.
    /// Takes the key; letters, Backspace, Ctrl-W, Ctrl-U, arrows, Home, End, Ctrl-A, Ctrl-E and Ctrl-J in multi-line fields.
    /// Returns true when the key edited the text or moved the cursor.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let length = self.text.chars().count();
        match key.code {
            KeyCode::Char('j') if control && self.multiline => self.insert_str("\n"),
            KeyCode::Char('w') if control => self.delete_word(),
            KeyCode::Char('u') if control => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = length,
            KeyCode::Char(_) if control || key.modifiers.contains(KeyModifiers::ALT) => {
                return false;
            }
            KeyCode::Char(character) => self.insert_str(character.encode_utf8(&mut [0u8; 4])),
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_at(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Backspace => {}
            KeyCode::Delete if self.cursor < length => {
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(length),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = length,
            _ => return false,
        }
        self.assert_consistent();
        true
    }

    /// Wraps the text into display lines at word boundaries.
    /// Takes the width in cells, at least 1.
    /// Returns the lines and the cursor's line and column.
    pub fn wrap(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        assert!(width > 0);
        let characters: Vec<char> = self.text.chars().collect();
        let mut lines = Vec::new();
        let mut cursor = None;
        let mut start = 0;
        for paragraph in self.text.split('\n') {
            let count = paragraph.chars().count();
            let ranges = wrap_ranges(&characters[start..start + count], width);
            for (index, (from, to)) in ranges.iter().enumerate() {
                let line: String = characters[start + from..start + to].iter().collect();
                let next = ranges.get(index + 1).map_or(count, |(from, _)| *from);
                if cursor.is_none() && self.cursor >= start + from && self.cursor <= start + next {
                    let before: String = characters[start + from..self.cursor.min(start + to)]
                        .iter()
                        .collect();
                    let at_break = self.cursor > start + to;
                    cursor = Some(if at_break && index + 1 < ranges.len() {
                        (lines.len() + 1, 0)
                    } else {
                        (lines.len(), cells(&before))
                    });
                }
                lines.push(line);
            }
            start += count + 1;
        }
        let cursor = cursor.unwrap_or((lines.len().saturating_sub(1), 0));
        assert!(!lines.is_empty());
        assert!(cursor.0 < lines.len());
        (lines, cursor)
    }

    fn delete_word(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let mut from = self.cursor;
        while from > 0 && characters[from - 1].is_whitespace() {
            from -= 1;
        }
        while from > 0 && !characters[from - 1].is_whitespace() {
            from -= 1;
        }
        let (start, end) = (self.byte_at(from), self.byte_at(self.cursor));
        self.text.replace_range(start..end, "");
        self.cursor = from;
    }

    fn byte_at(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map_or(self.text.len(), |(at, _)| at)
    }

    fn assert_consistent(&self) {
        assert!(self.text.len() <= self.limit);
        assert!(self.cursor <= self.text.chars().count());
        assert!(
            self.text
                .chars()
                .all(|c| !c.is_control() || (self.multiline && c == '\n'))
        );
    }
}

fn wrap_ranges(characters: &[char], width: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut last_space = None;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let size = char_cells(character);
        if used + size > width && index > start {
            match last_space {
                Some(space) if space > start => {
                    ranges.push((start, space));
                    start = space + 1;
                }
                _ => {
                    ranges.push((start, index));
                    start = index;
                }
            }
            used = characters[start..index]
                .iter()
                .map(|c| char_cells(*c))
                .sum();
            last_space = None;
            continue;
        }
        if character == ' ' {
            last_space = Some(index);
        }
        used += size;
        index += 1;
    }
    ranges.push((start, characters.len()));
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn control(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    #[test]
    fn typing_moving_and_deleting_edit_at_the_cursor() {
        let mut input = TextInput::single(64);
        for character in "Tidepol".chars() {
            assert!(input.handle(key(KeyCode::Char(character))));
        }
        input.handle(key(KeyCode::Left));
        input.handle(key(KeyCode::Char('o')));
        assert_eq!(input.value(), "Tidepool");
        input.handle(control('a'));
        input.handle(key(KeyCode::Delete));
        assert_eq!(input.value(), "idepool");
        input.handle(control('e'));
        input.handle(key(KeyCode::Backspace));
        assert_eq!((input.value(), input.cursor()), ("idepoo", 6));
        input.set("buoy sensor dash");
        input.handle(control('w'));
        assert_eq!(input.value(), "buoy sensor ");
        input.handle(control('w'));
        assert_eq!(input.value(), "buoy ");
        input.handle(control('u'));
        assert_eq!((input.value(), input.cursor()), ("", 0));
        assert!(!input.handle(control('k')));
    }

    #[test]
    fn pasted_control_characters_never_reach_the_value() {
        let mut single = TextInput::single(64);
        single.insert_str("a\u{1b}[31mb\r\nc\td\u{7}");
        assert_eq!(single.value(), "a[31mbc d");
        let mut multi = TextInput::multi(64);
        multi.insert_str("one\r\ntwo\u{0}");
        assert!(multi.handle(control('j')));
        multi.insert_str("three");
        assert_eq!(multi.value(), "one\ntwo\nthree");
        assert!(!single.handle(control('j')));
        assert_eq!(single.value(), "a[31mbc d");
    }

    #[test]
    fn the_byte_limit_stops_input_at_a_character_boundary() {
        let mut input = TextInput::single(5);
        input.insert_str("abcd\u{e9}f");
        assert_eq!(input.value(), "abcd");
        input.handle(key(KeyCode::Char('x')));
        assert_eq!(input.value(), "abcdx");
        input.handle(key(KeyCode::Char('y')));
        assert_eq!(input.value(), "abcdx");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_places_the_cursor() {
        let mut input = TextInput::multi(512);
        input.set(
            "A live dashboard for marine researchers to watch sensor buoys and get alerts when readings look wrong.",
        );
        let (lines, cursor) = input.wrap(68);
        assert_eq!(
            lines,
            [
                "A live dashboard for marine researchers to watch sensor buoys and",
                "get alerts when readings look wrong."
            ]
        );
        assert_eq!(cursor, (1, 36));
        input.handle(control('a'));
        assert_eq!(input.wrap(68).1, (0, 0));
        let mut long = TextInput::single(512);
        long.set(&"x".repeat(150));
        let (lines, cursor) = long.wrap(68);
        assert_eq!(
            lines.iter().map(String::len).collect::<Vec<_>>(),
            [68, 68, 14]
        );
        assert_eq!(cursor, (2, 14));
        let mut wide = TextInput::single(64);
        wide.set("\u{6f22}\u{5b57}\u{6f22}");
        assert_eq!(wide.wrap(4).0, ["\u{6f22}\u{5b57}", "\u{6f22}"]);
        let mut lines_only = TextInput::multi(64);
        lines_only.set("ab\n\ncd");
        assert_eq!(
            lines_only.wrap(10),
            (vec!["ab".into(), "".into(), "cd".into()], (2, 2))
        );
    }
}
