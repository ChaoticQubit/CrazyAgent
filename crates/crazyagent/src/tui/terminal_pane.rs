use super::keys::Key;
use super::theme::Depth;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

const SCROLLBACK_ROWS: usize = 0;
const PASTE_OPEN: &[u8] = b"\x1b[200~";
const PASTE_CLOSE: &[u8] = b"\x1b[201~";
const ESC: char = '\x1b';

/// The key that leaves an attached terminal for the team tree; every other key
/// goes to the agent.
pub const LEADER: Key = Key::Ctrl(']');

/// Turns one key into the bytes a terminal sends for it.
/// Inputs: the key, whether the program asked for application cursor keys and
/// whether it asked for bracketed paste. Output: the bytes; pasted newlines
/// become carriage returns and Esc is dropped from pasted text, so a paste can
/// neither end the bracket early nor send escape sequences.
pub fn encode(key: &Key, application_cursor: bool, bracketed_paste: bool) -> Vec<u8> {
    let cursor = |letter: u8| {
        if application_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    match key {
        Key::Char(character) => character.to_string().into_bytes(),
        Key::Ctrl(']') => vec![0x1d],
        Key::Ctrl(letter) => {
            assert!(
                letter.is_ascii_lowercase(),
                "Ctrl keys are lowercase letters or ]"
            );
            vec![*letter as u8 - b'a' + 1]
        }
        Key::Enter => vec![b'\r'],
        Key::Esc => vec![0x1b],
        Key::Tab => vec![b'\t'],
        Key::BackTab => b"\x1b[Z".to_vec(),
        Key::Backspace => vec![0x7f],
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Up => cursor(b'A'),
        Key::Down => cursor(b'B'),
        Key::Right => cursor(b'C'),
        Key::Left => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::ShiftLeft => b"\x1b[1;2D".to_vec(),
        Key::ShiftRight => b"\x1b[1;2C".to_vec(),
        Key::Paste(text) => paste(text, bracketed_paste),
    }
}

fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body: String = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .chars()
        .filter(|character| *character != ESC)
        .collect();
    assert!(!body.contains(ESC));
    if !bracketed {
        return body.into_bytes();
    }
    let mut bytes = PASTE_OPEN.to_vec();
    bytes.extend_from_slice(body.as_bytes());
    bytes.extend_from_slice(PASTE_CLOSE);
    bytes
}

/// One agent's terminal as the TUI shows it: a vt100 screen fed from the
/// engine's terminal stream.
/// Input: the captured screen, then raw output bytes. Output: cells drawn into
/// a buffer area, and the bytes each key sends in the program's current modes.
pub struct TerminalPane {
    parser: vt100::Parser,
}

impl TerminalPane {
    /// An empty screen of the given size.
    /// Inputs: rows and columns, both above zero. Output: the pane.
    pub fn new(rows: u16, cols: u16) -> Self {
        assert!(
            rows > 0 && cols > 0,
            "a terminal pane has at least one cell"
        );
        Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
        }
    }

    /// The screen size.
    /// Input: none. Output: (rows, columns).
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Changes the screen size.
    /// Inputs: rows and columns, both above zero. Output: true when the size
    /// changed, so the caller tells the engine.
    pub fn resize(&mut self, rows: u16, cols: u16) -> bool {
        assert!(
            rows > 0 && cols > 0,
            "a terminal pane has at least one cell"
        );
        if self.size() == (rows, cols) {
            return false;
        }
        self.parser.set_size(rows, cols);
        assert_eq!(self.size(), (rows, cols));
        true
    }

    /// Parses live output from the agent's terminal.
    /// Input: raw bytes, escape sequences included. Output: none.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Replaces the screen with a captured one; empty bytes give the fresh
    /// screen an attach starts from.
    /// Inputs: the captured rows separated by newlines, with their color
    /// sequences, and the zero-based (row, column) cursor when known.
    /// Output: none; rows beyond the pane height are dropped.
    pub fn show(&mut self, bytes: &[u8], cursor: Option<(u16, u16)>) {
        let (rows, cols) = self.size();
        self.parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
        for (row, line) in bytes
            .split(|byte| *byte == b'\n')
            .take(usize::from(rows))
            .enumerate()
        {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            self.parser
                .process(format!("\x1b[{};1H\x1b[0m", row + 1).as_bytes());
            self.parser.process(line);
        }
        self.parser.process(b"\x1b[0m");
        if let Some((row, col)) = cursor {
            self.parser
                .process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        }
    }

    /// The bytes one key sends, in the program's current cursor and paste modes.
    /// Input: the key. Output: the bytes.
    pub fn encode(&self, key: &Key) -> Vec<u8> {
        let screen = self.parser.screen();
        encode(key, screen.application_cursor(), screen.bracketed_paste())
    }

    /// Draws the screen into an area, clipped to both sizes.
    /// Inputs: the buffer, the area, the color depth and whether the pane has
    /// focus. Output: the cursor position while focused, shown and inside.
    pub fn draw(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        depth: Depth,
        focused: bool,
    ) -> Option<Position> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let height = rows.min(area.height);
        let width = cols.min(area.width);
        for y in 0..area.height {
            for x in 0..area.width {
                buffer[(area.x + x, area.y + y)].reset();
            }
        }
        for row in 0..height {
            for col in 0..width {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() || (cell.is_wide() && col + 1 >= width) {
                    continue;
                }
                let contents = cell.contents();
                let symbol = if contents.is_empty() {
                    " "
                } else {
                    contents.as_str()
                };
                buffer[(area.x + col, area.y + row)]
                    .set_symbol(symbol)
                    .set_style(style(cell, depth));
            }
        }
        let (row, col) = screen.cursor_position();
        (focused && !screen.hide_cursor() && row < height && col < width)
            .then_some(Position::new(area.x + col, area.y + row))
    }
}

fn style(cell: &vt100::Cell, depth: Depth) -> Style {
    let mut style = Style::default();
    if depth != Depth::NoColor {
        if let Some(color) = terminal_color(cell.fgcolor()) {
            style = style.fg(color);
        }
        if let Some(color) = terminal_color(cell.bgcolor()) {
            style = style.bg(color);
        }
    }
    for (on, modifier) in [
        (cell.bold(), Modifier::BOLD),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
    ] {
        if on {
            style = style.add_modifier(modifier);
        }
    }
    style
}

fn terminal_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(Color::Indexed(index)),
        vt100::Color::Rgb(red, green, blue) => Some(Color::Rgb(red, green, blue)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(pane: &TerminalPane, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        pane.draw(&mut buffer, area, Depth::NoColor, false);
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn cursor(pane: &TerminalPane, width: u16, height: u16) -> Option<Position> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        pane.draw(&mut buffer, area, Depth::NoColor, true)
    }

    #[test]
    fn recorded_output_with_line_breaks_draws_row_by_row() {
        let mut pane = TerminalPane::new(3, 10);
        pane.feed(b"hello\r\nworld");
        assert_eq!(
            rows(&pane, 10, 3),
            ["hello     ", "world     ", "          "]
        );
        assert_eq!(cursor(&pane, 10, 3), Some(Position::new(5, 1)));
    }

    #[test]
    fn cursor_moves_and_erases_redraw_the_right_cells() {
        let mut pane = TerminalPane::new(2, 10);
        pane.feed(b"abcdef\x1b[1;3HX\x1b[2;2HY");
        assert_eq!(rows(&pane, 10, 2), ["abXdef    ", " Y        "]);
        pane.feed(b"\x1b[2J");
        assert_eq!(rows(&pane, 10, 2), ["          ", "          "]);
    }

    #[test]
    fn colors_follow_the_depth_and_modifiers_always_show() {
        let mut pane = TerminalPane::new(1, 8);
        pane.feed(b"\x1b[1;31mred\x1b[0m ok");
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = Buffer::empty(area);
        pane.draw(&mut buffer, area, Depth::TrueColor, false);
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(1));
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(4, 0)].fg, Color::Reset);
        assert!(!buffer[(4, 0)].modifier.contains(Modifier::BOLD));

        let mut buffer = Buffer::empty(area);
        pane.draw(&mut buffer, area, Depth::NoColor, false);
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_captured_screen_replaces_the_pane_and_places_the_cursor() {
        let mut pane = TerminalPane::new(3, 12);
        pane.feed(b"old text");
        pane.show(b"$ ls\nsrc  tests\n", Some((2, 2)));
        assert_eq!(
            rows(&pane, 12, 3),
            ["$ ls        ", "src  tests  ", "            "]
        );
        assert_eq!(cursor(&pane, 12, 3), Some(Position::new(2, 2)));
        pane.feed(b"ok");
        assert_eq!(rows(&pane, 12, 3)[2], "  ok        ");
    }

    #[test]
    fn captured_rows_past_the_height_are_dropped() {
        let mut pane = TerminalPane::new(2, 4);
        pane.show(b"one\r\ntwo\r\nsix", None);
        assert_eq!(rows(&pane, 4, 2), ["one ", "two "]);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut pane = TerminalPane::new(1, 6);
        pane.feed("日本".as_bytes());
        assert_eq!(rows(&pane, 6, 1), ["日 本   "]);
    }

    #[test]
    fn drawing_clips_to_the_area_and_hides_an_outside_cursor() {
        let mut pane = TerminalPane::new(3, 10);
        pane.feed(b"hello");
        assert_eq!(rows(&pane, 4, 2), ["hell", "    "]);
        assert_eq!(cursor(&pane, 4, 2), None);
        assert_eq!(cursor(&pane, 10, 3), Some(Position::new(5, 0)));
        pane.feed(b"\x1b[?25l");
        assert_eq!(cursor(&pane, 10, 3), None);
    }

    #[test]
    fn resize_reports_only_real_changes() {
        let mut pane = TerminalPane::new(3, 10);
        assert!(!pane.resize(3, 10));
        assert!(pane.resize(2, 5));
        assert_eq!(pane.size(), (2, 5));
        assert!(!pane.resize(2, 5));
    }

    #[test]
    #[should_panic(expected = "a terminal pane has at least one cell")]
    fn a_pane_never_has_zero_rows() {
        TerminalPane::new(0, 10);
    }

    #[test]
    fn keys_encode_like_a_terminal_in_normal_modes() {
        let cases: [(Key, &[u8]); 22] = [
            (Key::Char('a'), b"a"),
            (Key::Char('é'), "é".as_bytes()),
            (Key::Ctrl('a'), b"\x01"),
            (Key::Ctrl('c'), b"\x03"),
            (Key::Ctrl('z'), b"\x1a"),
            (LEADER, b"\x1d"),
            (Key::Enter, b"\r"),
            (Key::Esc, b"\x1b"),
            (Key::Tab, b"\t"),
            (Key::BackTab, b"\x1b[Z"),
            (Key::Backspace, b"\x7f"),
            (Key::Delete, b"\x1b[3~"),
            (Key::Up, b"\x1b[A"),
            (Key::Down, b"\x1b[B"),
            (Key::Right, b"\x1b[C"),
            (Key::Left, b"\x1b[D"),
            (Key::Home, b"\x1b[H"),
            (Key::End, b"\x1b[F"),
            (Key::PageUp, b"\x1b[5~"),
            (Key::PageDown, b"\x1b[6~"),
            (Key::ShiftLeft, b"\x1b[1;2D"),
            (Key::ShiftRight, b"\x1b[1;2C"),
        ];
        for (key, bytes) in cases {
            assert_eq!(encode(&key, false, false), bytes, "{key:?}");
        }
    }

    #[test]
    fn application_cursor_mode_changes_only_the_cursor_keys() {
        assert_eq!(encode(&Key::Up, true, false), b"\x1bOA");
        assert_eq!(encode(&Key::Left, true, false), b"\x1bOD");
        assert_eq!(encode(&Key::Home, true, false), b"\x1bOH");
        assert_eq!(encode(&Key::End, true, false), b"\x1bOF");
        assert_eq!(encode(&Key::PageUp, true, false), b"\x1b[5~");
        assert_eq!(encode(&Key::Enter, true, false), b"\r");
    }

    #[test]
    fn pastes_send_carriage_returns_and_never_an_escape() {
        let text = "a\nb\r\nc\x1b[201~d".to_owned();
        assert_eq!(
            encode(&Key::Paste(text.clone()), false, false),
            b"a\rb\rc[201~d"
        );
        assert_eq!(
            encode(&Key::Paste(text), false, true),
            b"\x1b[200~a\rb\rc[201~d\x1b[201~"
        );
    }

    #[test]
    fn the_pane_encodes_in_the_modes_the_program_asked_for() {
        let mut pane = TerminalPane::new(2, 10);
        assert_eq!(pane.encode(&Key::Up), b"\x1b[A");
        assert_eq!(pane.encode(&Key::Paste("p".into())), b"p");
        pane.feed(b"\x1b[?1h\x1b[?2004h");
        assert_eq!(pane.encode(&Key::Up), b"\x1bOA");
        assert_eq!(pane.encode(&Key::Paste("p".into())), b"\x1b[200~p\x1b[201~");
        pane.feed(b"\x1b[?1l\x1b[?2004l");
        assert_eq!(pane.encode(&Key::Up), b"\x1b[A");
    }
}
