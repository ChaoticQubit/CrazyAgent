use super::glyphs::Glyphs;
use super::keys::Key;
use super::text::{char_width, join, pad, row, wrap};
use super::theme::Theme;
use ratatui::layout::Position;
use ratatui::text::{Line, Span};

const VISIBLE_LINES_MAX: usize = 4;

/// A labeled text field: plain, multi-line or masked for secrets.
/// Input: built with `TextField::new` and edited with `key`. Output: drawn by
/// `lines` as label, one bracket line per text line, then a note or error.
/// A masked field never shows, logs or debug-prints its text.
pub struct TextField {
    pub label: String,
    value: String,
    cursor: usize,
    pub masked: bool,
    pub multiline: bool,
    pub counter: bool,
    pub max_chars: usize,
    pub placeholder: String,
    pub note: String,
    pub right: String,
    pub error: Option<String>,
}

impl std::fmt::Debug for TextField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextField")
            .field("label", &self.label)
            .field("chars", &self.value.chars().count())
            .field("masked", &self.masked)
            .finish_non_exhaustive()
    }
}

impl Drop for TextField {
    fn drop(&mut self) {
        if self.masked {
            wipe(&mut self.value);
        }
    }
}

fn wipe(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

impl TextField {
    /// A single-line field.
    /// Inputs: label and maximum characters. Output: an empty field.
    pub fn new(label: &str, max_chars: usize) -> Self {
        assert!(max_chars > 0);
        Self {
            label: label.to_owned(),
            value: String::new(),
            cursor: 0,
            masked: false,
            multiline: false,
            counter: false,
            max_chars,
            placeholder: String::new(),
            note: String::new(),
            right: String::new(),
            error: None,
        }
    }

    /// A masked field for an API key, code or other secret.
    /// Inputs: label and maximum characters. Output: an empty masked field with
    /// room reserved so typing never leaves copies in freed memory.
    #[cfg_attr(not(test), expect(dead_code, reason = "Add account fields"))]
    pub fn secret(label: &str, max_chars: usize) -> Self {
        let mut field = Self::new(label, max_chars);
        field.masked = true;
        field.value = String::with_capacity(max_chars * 4);
        field
    }

    /// Sets the muted placeholder shown while the field is empty and unfocused.
    /// Input: the text. Output: the field.
    pub fn placeholder(mut self, text: &str) -> Self {
        self.placeholder = text.to_owned();
        self
    }

    /// Sets the note line under the field.
    /// Input: the text. Output: the field.
    #[cfg_attr(not(test), expect(dead_code, reason = "Add account fields"))]
    pub fn note(mut self, text: &str) -> Self {
        self.note = text.to_owned();
        self
    }

    /// Sets the muted text at the right of the label, such as "Required".
    /// Input: the text. Output: the field.
    pub fn right(mut self, text: &str) -> Self {
        self.right = text.to_owned();
        self
    }

    /// Lets Ctrl-J add lines.
    /// Input: none. Output: the field.
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Shows "51 characters" at the right of the label while text is present.
    /// Input: none. Output: the field.
    #[cfg_attr(not(test), expect(dead_code, reason = "Add account fields"))]
    pub fn counted(mut self) -> Self {
        self.counter = true;
        self
    }

    /// The typed text of a plain field.
    /// Input: none. Output: the text; a masked field returns nothing here.
    pub fn value(&self) -> &str {
        assert!(!self.masked, "masked text leaves only through take_secret");
        &self.value
    }

    /// Number of characters typed.
    /// Input: none. Output: the count, also shown by the counter.
    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// Says whether nothing is typed.
    /// Input: none. Output: true for an empty field.
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Replaces the text, as when a form is prefilled.
    /// Input: the text. Output: none; the cursor moves to the end.
    pub fn set(&mut self, text: &str) {
        self.clear();
        self.insert(text);
    }

    /// Clears the text, wiping a masked value.
    /// Input: none. Output: none.
    pub fn clear(&mut self) {
        if self.masked {
            let mut wiped = std::mem::take(&mut self.value);
            wipe(&mut wiped);
            self.value = String::with_capacity(self.max_chars * 4);
        } else {
            self.value.clear();
        }
        self.cursor = 0;
    }

    /// Hands the typed secret over and empties the field.
    /// Input: none. Output: the bytes, for the engine call only.
    #[cfg_attr(not(test), expect(dead_code, reason = "Add account fields"))]
    pub fn take_secret(&mut self) -> Vec<u8> {
        assert!(self.masked);
        let bytes = self.value.as_bytes().to_vec();
        self.clear();
        bytes
    }

    fn byte_at(&self, cursor: usize) -> usize {
        self.value
            .char_indices()
            .nth(cursor)
            .map_or(self.value.len(), |(index, _)| index)
    }

    fn insert(&mut self, text: &str) {
        for character in text.chars() {
            let accepted = match character {
                '\n' => self.multiline,
                '\r' | '\t' => false,
                other => !other.is_control(),
            };
            if !accepted || self.len() >= self.max_chars {
                continue;
            }
            let at = self.byte_at(self.cursor);
            self.value.insert(at, character);
            self.cursor += 1;
        }
    }

    /// Applies one editing key.
    /// Input: the key. Output: true when the field used it; Enter, Tab, Esc and
    /// arrows that leave the field return false.
    pub fn key(&mut self, key: &Key) -> bool {
        let count = self.len();
        match key {
            Key::Char(character) => self.insert(&character.to_string()),
            Key::Paste(text) => {
                let text = if self.multiline {
                    text.replace("\r\n", "\n")
                } else {
                    text.replace(['\r', '\n'], "")
                };
                self.insert(&text);
            }
            Key::Ctrl('j') if self.multiline => self.insert("\n"),
            Key::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte_at(self.cursor);
                self.value.remove(at);
            }
            Key::Backspace => {}
            Key::Delete if self.cursor < count => {
                let at = self.byte_at(self.cursor);
                self.value.remove(at);
            }
            Key::Delete => {}
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(count),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = count,
            Key::Ctrl('u') => self.clear(),
            Key::Ctrl('w') => {
                let characters: Vec<char> = self.value.chars().collect();
                let mut start = self.cursor;
                while start > 0 && characters[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && characters[start - 1] != ' ' && characters[start - 1] != '\n' {
                    start -= 1;
                }
                let from = self.byte_at(start);
                let to = self.byte_at(self.cursor);
                self.value.replace_range(from..to, "");
                self.cursor = start;
            }
            _ => return false,
        }
        assert!(self.cursor <= self.len());
        true
    }

    fn display_lines(&self, inner: usize, glyphs: &Glyphs) -> (Vec<String>, usize, usize) {
        if self.masked {
            let dots = self.len().min(inner.saturating_sub(1));
            return (vec![glyphs.dot.repeat(dots)], 0, dots);
        }
        let before: String = self.value.chars().take(self.cursor).collect();
        let lines = wrap_keeping(&self.value, inner);
        let cursor_lines = wrap_keeping(&before, inner);
        let cursor_row = cursor_lines.len().saturating_sub(1);
        let mut cursor_column = cursor_lines
            .last()
            .map_or(0, |line| line.chars().map(char_width).sum());
        let (lines, cursor_row) = if cursor_column >= inner && !before.ends_with('\n') {
            cursor_column = 0;
            let mut lines = lines;
            if lines.len() == cursor_row + 1 {
                lines.push(String::new());
            }
            (lines, cursor_row + 1)
        } else {
            (lines, cursor_row)
        };
        (lines, cursor_row, cursor_column)
    }

    /// Draws the field.
    /// Inputs: width, whether it has focus, theme and glyphs. Output: the lines
    /// and, when focused, the cursor position relative to the first line.
    pub fn lines(
        &self,
        limit: usize,
        focused: bool,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        assert!(limit >= 6);
        let inner = limit - 4;
        let right = if self.counter && !self.is_empty() {
            format!("{} characters", self.len())
        } else {
            self.right.clone()
        };
        let mut out = vec![row(
            limit,
            Line::from(Span::styled(
                self.label.clone(),
                if focused { theme.accent } else { theme.heading },
            )),
            Line::from(Span::styled(right, theme.muted)),
            glyphs,
        )];
        let bracket = if focused { theme.accent } else { theme.muted };
        let (texts, cursor_row, cursor_column) = self.display_lines(inner, glyphs);
        let visible = if self.multiline { VISIBLE_LINES_MAX } else { 1 };
        let first = cursor_row.saturating_sub(visible - 1).min(texts.len().saturating_sub(1));
        let shown: Vec<String> = if texts.is_empty() {
            vec![String::new()]
        } else {
            texts.iter().skip(first).take(visible).cloned().collect()
        };
        let mut cursor = None;
        for (index, text) in shown.iter().enumerate() {
            let content = if text.is_empty() && self.is_empty() && !focused && !self.placeholder.is_empty() {
                Line::from(Span::styled(self.placeholder.clone(), theme.muted))
            } else {
                Line::raw(text.clone())
            };
            if focused && first + index == cursor_row {
                cursor = Some(Position::new(2 + cursor_column as u16, out.len() as u16));
            }
            out.push(join([
                Line::from(Span::styled("[", bracket)),
                Line::raw(" "),
                pad(content, inner, glyphs),
                Line::raw(" "),
                Line::from(Span::styled("]", bracket)),
            ]));
        }
        if let Some(error) = &self.error {
            out.push(Line::from(Span::styled(error.clone(), theme.error)));
        } else if !self.note.is_empty() {
            out.push(Line::from(Span::styled(self.note.clone(), theme.muted)));
        }
        (out, cursor)
    }
}

fn wrap_keeping(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let lines = wrap(paragraph, limit);
        if lines.is_empty() {
            out.push(String::new());
        } else {
            let trailing = paragraph.ends_with(' ');
            let count = lines.len();
            for (index, mut line) in lines.into_iter().enumerate() {
                if trailing && index + 1 == count {
                    line.push(' ');
                }
                out.push(line);
            }
        }
    }
    out
}

/// A row of buttons, the focused one as `[> Label ]` in the selection style.
/// Inputs: labels, the focused index if any, and the theme. Output: one line
/// with three spaces between buttons.
pub fn buttons(labels: &[&str], focused: Option<usize>, theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        if focused == Some(index) {
            spans.push(Span::styled(format!("[> {label} ]"), theme.selection));
        } else {
            spans.push(Span::raw(format!("[ {label} ]")));
        }
    }
    Line::from(spans)
}

/// One radio choice, `(*) Label` or `( ) Label`.
/// Inputs: whether chosen and the label. Output: the text.
pub fn radio(on: bool, label: &str) -> String {
    format!("{} {label}", if on { "(*)" } else { "( )" })
}

/// One checkbox, `[x] Label` or `[ ] Label`.
/// Inputs: whether checked and the label. Output: the text.
pub fn checkbox(on: bool, label: &str) -> String {
    format!("{} {label}", if on { "[x]" } else { "[ ]" })
}

/// One menu item: a `▌` bar and accent title when focused, a muted description.
/// Inputs: title, description, width, focus, theme and glyphs. Output: the
/// title line, wrapped description lines and a blank line.
#[cfg_attr(not(test), expect(dead_code, reason = "Add account sign-in methods"))]
pub fn menu_item(
    title: &str,
    description: &str,
    limit: usize,
    focused: bool,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Vec<Line<'static>> {
    let gutter = || {
        if focused {
            Span::styled(glyphs.bar, theme.accent)
        } else {
            Span::raw(" ")
        }
    };
    let mut out = vec![Line::from(vec![
        gutter(),
        Span::raw(" "),
        Span::styled(
            title.to_owned(),
            if focused { theme.accent } else { theme.heading },
        ),
    ])];
    for part in wrap(description, limit.saturating_sub(4).max(1)) {
        out.push(Line::from(vec![
            gutter(),
            Span::raw("   "),
            Span::styled(part, theme.muted),
        ]));
    }
    out.push(Line::default());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::{ASCII, UNICODE};
    use crate::tui::text::plain;
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn theme() -> Theme {
        Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark)
    }

    fn typed(field: &mut TextField, text: &str) {
        for character in text.chars() {
            assert!(field.key(&Key::Char(character)));
        }
    }

    #[test]
    fn editing_keys_move_delete_and_clear() {
        let mut field = TextField::new("Name", 100);
        typed(&mut field, "Mira Okafor");
        assert!(field.key(&Key::Ctrl('w')));
        assert_eq!(field.value(), "Mira ");
        typed(&mut field, "Ok");
        assert!(field.key(&Key::Home));
        assert!(field.key(&Key::Delete));
        assert_eq!(field.value(), "ira Ok");
        assert!(field.key(&Key::Ctrl('e')));
        assert!(field.key(&Key::Backspace));
        assert!(field.key(&Key::Left));
        typed(&mut field, "x");
        assert_eq!(field.value(), "ira xO");
        assert!(field.key(&Key::Ctrl('a')));
        typed(&mut field, "M");
        assert_eq!(field.value(), "Mira xO");
        assert!(field.key(&Key::Ctrl('u')));
        assert!(field.is_empty());
        assert!(!field.key(&Key::Enter));
        assert!(!field.key(&Key::Tab));
        assert!(!field.key(&Key::Esc));
        assert!(!field.key(&Key::Up));
    }

    #[test]
    fn paste_and_newlines_follow_the_field_kind() {
        let mut single = TextField::new("Name", 10);
        assert!(single.key(&Key::Paste("Mira\r\nOkafor and more".into())));
        assert_eq!(single.value(), "MiraOkafor");
        assert!(!single.key(&Key::Ctrl('j')));
        assert_eq!(single.value(), "MiraOkafor");
        let mut about = TextField::new("About", 100).multiline();
        assert!(about.key(&Key::Paste("one\r\ntwo".into())));
        assert!(about.key(&Key::Ctrl('j')));
        typed(&mut about, "three");
        assert_eq!(about.value(), "one\ntwo\nthree");
        let mut bounded = TextField::new("Code", 3);
        typed(&mut bounded, "abcdef");
        assert_eq!(bounded.value(), "abc");
        assert!(bounded.key(&Key::Paste("\u{1b}[31m".into())));
        assert_eq!(bounded.value(), "abc");
    }

    #[test]
    fn masked_fields_show_dots_count_and_hand_over_once() {
        let mut key = TextField::secret("API key", 8192).counted();
        assert!(key.key(&Key::Paste("sk-test-1234567890".into())));
        let (lines, cursor) = key.lines(64, true, &theme(), &UNICODE);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(text[0].trim_end(), format!("API key{}18 characters", " ".repeat(64 - 7 - 13)).trim_end());
        assert_eq!(text[1], format!("[ {}{} ]", "•".repeat(18), " ".repeat(42)));
        assert!(!text.join("\n").contains("sk-test"));
        assert_eq!(cursor, Some(Position::new(20, 1)));
        assert!(!format!("{key:?}").contains("sk-test"));
        let (ascii, _) = key.lines(64, false, &theme(), &ASCII);
        assert!(plain(&ascii[1]).starts_with("[ ******************"));
        assert_eq!(key.take_secret(), b"sk-test-1234567890");
        assert!(key.is_empty());
        let mut long = TextField::secret("API key", 8192).counted();
        assert!(long.key(&Key::Paste("x".repeat(70))));
        let (lines, cursor) = long.lines(64, true, &theme(), &UNICODE);
        assert_eq!(plain(&lines[1]), format!("[ {}  ]", "•".repeat(59)));
        assert!(plain(&lines[0]).ends_with("70 characters"));
        assert_eq!(cursor, Some(Position::new(61, 1)));
    }

    #[test]
    fn fields_draw_label_brackets_placeholder_and_error() {
        let mut name = TextField::new("Name", 100).right("Required");
        let (lines, cursor) = name.lines(20, false, &theme(), &UNICODE);
        assert_eq!(
            lines.iter().map(plain).collect::<Vec<_>>(),
            ["Name        Required", "[                  ]"]
        );
        assert_eq!(cursor, None);
        name.error = Some("Enter your name to continue.".into());
        let (lines, cursor) = name.lines(20, true, &theme(), &UNICODE);
        assert_eq!(plain(&lines[2]), "Enter your name to continue.");
        assert_eq!(cursor, Some(Position::new(2, 1)));
        let role = TextField::new("Role", 100).placeholder("For example");
        let (lines, _) = role.lines(20, false, &theme(), &UNICODE);
        assert_eq!(plain(&lines[1]), "[ For example      ]");
        let (lines, _) = role.lines(20, true, &theme(), &UNICODE);
        assert_eq!(plain(&lines[1]), "[                  ]");
    }

    #[test]
    fn multiline_fields_wrap_and_follow_the_cursor() {
        let mut bio = TextField::new("Short bio", 1000).multiline();
        bio.set("Building tools for marine researchers. Ten years of TypeScript, learning Rust.");
        let (lines, cursor) = bio.lines(72, true, &theme(), &UNICODE);
        assert_eq!(
            lines.iter().map(plain).map(|line| line.trim_end().to_owned()).collect::<Vec<_>>(),
            [
                "Short bio",
                "[ Building tools for marine researchers. Ten years of TypeScript,      ]",
                "[ learning Rust.                                                       ]"
            ]
        );
        assert_eq!(cursor, Some(Position::new(16, 2)));
        let mut tall = TextField::new("About", 1000).multiline();
        tall.set("1\n2\n3\n4\n5\n6");
        let (lines, cursor) = tall.lines(20, true, &theme(), &UNICODE);
        assert_eq!(lines.len(), 5);
        assert_eq!(plain(&lines[4]).trim_end(), "[ 6                ]".trim_end());
        assert_eq!(cursor, Some(Position::new(3, 4)));
        tall.key(&Key::Home);
        let (lines, cursor) = tall.lines(20, true, &theme(), &UNICODE);
        assert_eq!(plain(&lines[1]).trim_end(), "[ 1                ]".trim_end());
        assert_eq!(cursor, Some(Position::new(2, 1)));
    }

    #[test]
    fn buttons_and_menus_mark_focus_with_shape() {
        let line = buttons(&["Back", "Create project"], Some(1), &theme());
        assert_eq!(plain(&line), "[ Back ]   [> Create project ]");
        assert_eq!(line.spans[2].style, theme().selection);
        assert_eq!(radio(true, "Short"), "(*) Short");
        assert_eq!(checkbox(false, "Main"), "[ ] Main");
        let item = menu_item(
            "Sign in with ChatGPT",
            "Recommended. Uses your ChatGPT plan. You get a code to enter on any device.",
            76,
            true,
            &theme(),
            &UNICODE,
        );
        assert_eq!(
            item.iter().map(plain).collect::<Vec<_>>(),
            [
                "▌ Sign in with ChatGPT",
                "▌   Recommended. Uses your ChatGPT plan. You get a code to enter on any",
                "▌   device.",
                ""
            ]
        );
    }

    #[test]
    fn a_note_shows_under_the_field_until_an_error_replaces_it() {
        let mut field = TextField::new("Endpoint URL", 200).note("For example: http://127.0.0.1:11434/v1");
        let (lines, _) = field.lines(40, false, &theme(), &UNICODE);
        assert_eq!(plain(lines.last().unwrap()), "For example: http://127.0.0.1:11434/v1");
        field.error = Some("Enter a URL that starts with http.".into());
        let (lines, _) = field.lines(40, false, &theme(), &UNICODE);
        assert_eq!(plain(lines.last().unwrap()), "Enter a URL that starts with http.");
        assert!(!lines.iter().any(|line| plain(line).contains("For example")));
    }
}
