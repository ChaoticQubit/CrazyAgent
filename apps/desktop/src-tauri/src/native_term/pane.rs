use super::codec;
use super::screen::GridSize;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use crazyagent::terminal::{INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN};

/// The largest pane side accepted, in cells: the engine transport's own bound.
pub const GRID_MAX: usize = SIZE_MAX as usize;

/// The smallest pane side accepted, in cells.
pub const GRID_MIN: usize = SIZE_MIN as usize;

const _: () = assert!(GRID_MAX == 1000 && GRID_MIN == 2);

/// The most wheel lines one event turns into keys or reports.
const WHEEL_LINES_MAX: u32 = 50;

const _: () = assert!(WHEEL_LINES_MAX as usize * 16 <= INPUT_BYTES_MAX);

/// A key with no text of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Named {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    BackTab,
}

/// The bytes a named key sends, by the pane's cursor-key mode. Inputs: the key and whether
/// the pane asked for application cursor keys. Output: bytes.
pub fn named_bytes(key: Named, app_cursor: bool) -> Vec<u8> {
    let arrow = |c: u8| {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let bytes = match key {
        Named::Up => arrow(b'A'),
        Named::Down => arrow(b'B'),
        Named::Right => arrow(b'C'),
        Named::Left => arrow(b'D'),
        Named::Home => b"\x1b[H".to_vec(),
        Named::End => b"\x1b[F".to_vec(),
        Named::PageUp => b"\x1b[5~".to_vec(),
        Named::PageDown => b"\x1b[6~".to_vec(),
        Named::Delete => b"\x1b[3~".to_vec(),
        Named::BackTab => b"\x1b[Z".to_vec(),
    };
    assert!(bytes.len() >= 3);
    assert_eq!(bytes[0], 0x1b);
    bytes
}

/// One input to a native view from its host. Cells are column and line on the visible
/// screen; `option` is the Option key, which forces a local selection while the pane reads
/// the mouse. Text is at most `INPUT_BYTES_MAX` bytes (the host splits longer typing).
#[derive(Debug, PartialEq)]
pub enum Input {
    Text(Vec<u8>),
    Key(Named),
    Wheel { lines: i32, cell: (usize, usize) },
    Press { cell: (usize, usize), option: bool },
    Drag { cell: (usize, usize), option: bool },
    Release { cell: (usize, usize), option: bool },
    Paste { text: String },
}

/// What one input asks of the host: bytes for the agent (`wheel` when only the wheel made
/// them, which the engine never counts as the owner typing), a paste of the owner's text, a
/// change to the local screen only, or nothing.
#[derive(Debug, PartialEq)]
pub enum Action {
    Input { bytes: Vec<u8>, wheel: bool },
    Paste { text: String },
    Local,
    Nothing,
}

/// One native view's mirror of its agent's pane: the terminal fed by the attach and then by
/// live output, its local selection and scrollback, and whether its own keys go through.
pub struct Pane {
    pub view: u64,
    term: Option<Term<VoidListener>>,
    parser: Processor,
    pub dirty: bool,
    button_down: bool,
    keys: bool,
}

impl Pane {
    /// A mirror with no terminal until its attach comes. Input: the view id. Output: the pane.
    pub fn new(view: u64) -> Pane {
        assert!(view > 0);
        let pane = Pane {
            view,
            term: None,
            parser: Processor::new(),
            dirty: false,
            button_down: false,
            keys: true,
        };
        assert!(!pane.ready());
        pane
    }

    /// Whether the attach came, so output and input apply.
    pub fn ready(&self) -> bool {
        self.term.is_some()
    }

    /// The terminal, once attached, for drawing and reading.
    pub fn term(&self) -> Option<&Term<VoidListener>> {
        self.term.as_ref()
    }

    /// Takes the attach: makes the terminal at the pane's size; the screen and modes follow
    /// as output. A second attach starts over. Inputs: the columns and rows. Output: whether
    /// the size was inside the bounds.
    pub fn on_attached(&mut self, columns: u16, rows: u16) -> bool {
        let (columns, lines) = (usize::from(columns), usize::from(rows));
        let fits =
            (GRID_MIN..=GRID_MAX).contains(&columns) && (GRID_MIN..=GRID_MAX).contains(&lines);
        if !fits {
            return false;
        }
        let size = GridSize { columns, lines };
        self.term = Some(Term::new(Config::default(), &size, VoidListener));
        self.parser = Processor::new();
        self.button_down = false;
        self.dirty = true;
        assert!(self.ready());
        assert_eq!(
            self.term.as_ref().map(|t| (t.columns(), t.screen_lines())),
            Some((columns, lines))
        );
        true
    }

    /// Feeds output into the terminal. Input: the bytes. Output: whether the terminal took
    /// them (none before the attach).
    pub fn on_output(&mut self, bytes: &[u8]) -> bool {
        let Some(term) = self.term.as_mut() else {
            return false;
        };
        self.parser.advance(term, bytes);
        self.dirty = true;
        true
    }

    /// Turns the view's own keys on or off; off, it still scrolls and selects locally but
    /// asks for no bytes and no paste. Input: whether keys go through.
    pub fn set_keys(&mut self, on: bool) {
        self.keys = on;
        if !on {
            self.button_down = false;
        }
        assert_eq!(self.keys, on);
    }

    /// Whether the pane reads the mouse, so a plain drag goes to the agent.
    pub fn mouse_mode(&self) -> bool {
        self.term
            .as_ref()
            .is_some_and(|t| t.mode().intersects(TermMode::MOUSE_MODE))
    }

    /// Whether the view is scrolled off the bottom of its output.
    pub fn behind(&self) -> bool {
        self.term
            .as_ref()
            .is_some_and(|t| t.grid().display_offset() > 0)
    }

    /// Scrolls back to the bottom of the output. Output: whether it moved.
    pub fn jump(&mut self) -> bool {
        let Some(term) = self.term.as_mut() else {
            return false;
        };
        if term.grid().display_offset() == 0 {
            return false;
        }
        term.scroll_display(Scroll::Bottom);
        self.dirty = true;
        assert_eq!(term.grid().display_offset(), 0);
        true
    }

    fn app_cursor(&self) -> bool {
        self.term
            .as_ref()
            .is_some_and(|t| t.mode().contains(TermMode::APP_CURSOR))
    }

    fn reads_mouse(&self, option: bool) -> bool {
        self.keys && !option && self.mouse_mode()
    }
}

impl Pane {
    /// Applies one input: keys and text ask for bytes, the wheel and mouse ask for reports
    /// when the pane reads the mouse (else they scroll and select locally), a paste asks for
    /// the owner's text unchanged. Input: the input. Output: what the host must do.
    pub fn apply(&mut self, input: Input) -> Action {
        if !self.ready() {
            return Action::Nothing;
        }
        let action = match input {
            Input::Text(bytes) => self.typed(bytes),
            Input::Key(key) => self.typed(named_bytes(key, self.app_cursor())),
            Input::Wheel { lines, cell } => self.wheel(lines, cell),
            Input::Press { cell, option } => self.button(true, cell, option),
            Input::Release { cell, option } => self.button(false, cell, option),
            Input::Drag { cell, option } => self.drag(cell, option),
            Input::Paste { text } => self.paste(text),
        };
        if let Action::Input { bytes, .. } = &action {
            assert!(!bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX);
        }
        action
    }

    fn to_bottom(&mut self) {
        if let Some(term) = self.term.as_mut()
            && term.grid().display_offset() > 0
        {
            term.scroll_display(Scroll::Bottom);
            self.dirty = true;
        }
    }

    fn typed(&mut self, bytes: Vec<u8>) -> Action {
        assert!(bytes.len() <= INPUT_BYTES_MAX);
        if !self.keys || bytes.is_empty() {
            return Action::Nothing;
        }
        self.to_bottom();
        Action::Input {
            bytes,
            wheel: false,
        }
    }

    fn paste(&mut self, text: String) -> Action {
        if !self.keys || text.is_empty() || text.len() > codec::COPY_BYTES_MAX {
            return Action::Nothing;
        }
        self.to_bottom();
        assert!(!text.is_empty());
        Action::Paste { text }
    }

    fn clamp(&self, cell: (usize, usize)) -> Option<(usize, usize)> {
        let term = self.term.as_ref()?;
        Some((
            cell.0.min(term.columns() - 1),
            cell.1.min(term.screen_lines() - 1),
        ))
    }

    fn wheel(&mut self, lines: i32, cell: (usize, usize)) -> Action {
        let (Some(cell), Some(mode)) = (self.clamp(cell), self.term.as_ref().map(|t| *t.mode()))
        else {
            return Action::Nothing;
        };
        if lines == 0 {
            return Action::Nothing;
        }
        let count = lines.unsigned_abs().min(WHEEL_LINES_MAX);
        if self.reads_mouse(false) {
            let report = codec::sgr_wheel(lines > 0, cell.0, cell.1);
            let bytes: Vec<u8> = (0..count).flat_map(|_| report.iter().copied()).collect();
            return Action::Input { bytes, wheel: true };
        }
        if self.keys
            && mode.contains(TermMode::ALT_SCREEN)
            && mode.contains(TermMode::ALTERNATE_SCROLL)
        {
            let arrow: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let bytes: Vec<u8> = (0..count).flat_map(|_| arrow.iter().copied()).collect();
            return Action::Input { bytes, wheel: true };
        }
        let Some(term) = self.term.as_mut() else {
            return Action::Nothing;
        };
        let before = term.grid().display_offset();
        let after = codec::scroll_clamp(before, lines, term.grid().history_size());
        if after != before {
            term.scroll_display(Scroll::Delta(after as i32 - before as i32));
        }
        self.dirty = true;
        assert!(term.grid().display_offset() <= term.grid().history_size());
        Action::Local
    }

    fn grid_point(&self, cell: (usize, usize)) -> Option<Point> {
        let term = self.term.as_ref()?;
        let (column, line) = self.clamp(cell)?;
        let offset = term.grid().display_offset() as i32;
        Some(Point::new(Line(line as i32 - offset), Column(column)))
    }

    fn button(&mut self, press: bool, cell: (usize, usize), option: bool) -> Action {
        let Some(cell) = self.clamp(cell) else {
            return Action::Nothing;
        };
        self.button_down = press;
        if self.reads_mouse(option) {
            let bytes = codec::sgr_button(0, cell.0, cell.1, press);
            return Action::Input {
                bytes,
                wheel: false,
            };
        }
        if press {
            let point = self.grid_point(cell);
            if let (Some(point), Some(term)) = (point, self.term.as_mut()) {
                term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
            }
            self.dirty = true;
        }
        assert_eq!(self.button_down, press);
        Action::Local
    }

    fn drag(&mut self, cell: (usize, usize), option: bool) -> Action {
        if !self.button_down {
            return Action::Nothing;
        }
        let Some(cell) = self.clamp(cell) else {
            return Action::Nothing;
        };
        if self.reads_mouse(option) {
            let bytes = codec::sgr_button(32, cell.0, cell.1, true);
            return Action::Input {
                bytes,
                wheel: false,
            };
        }
        let point = self.grid_point(cell);
        if let (Some(point), Some(selection)) =
            (point, self.term.as_mut().and_then(|t| t.selection.as_mut()))
        {
            selection.update(point, Side::Right);
        }
        self.dirty = true;
        assert!(self.button_down);
        Action::Local
    }
}

impl Pane {
    /// The text a copy takes: the selection, or the whole output the view holds (history
    /// and screen). Input: whether to take the whole output. Output: the text, or a plain
    /// code when there is nothing to copy or it is over `COPY_BYTES_MAX`.
    pub fn copy(&self, output: bool) -> Result<String, &'static str> {
        let term = self.term.as_ref().ok_or("native_terminal_copy_empty")?;
        let text = if output {
            let top = Point::new(Line(-(term.grid().history_size() as i32)), Column(0));
            let bottom = Point::new(
                Line(term.screen_lines() as i32 - 1),
                Column(term.columns() - 1),
            );
            let mut all = Selection::new(SelectionType::Lines, top, Side::Left);
            all.update(bottom, Side::Right);
            all.to_range(term)
                .map(|range| term.bounds_to_string(range.start, range.end))
        } else {
            term.selection_to_string()
        };
        let text = text
            .filter(|t| !t.trim().is_empty())
            .ok_or("native_terminal_copy_empty")?;
        if text.len() > codec::COPY_BYTES_MAX {
            return Err("native_terminal_copy_too_large");
        }
        assert!(!text.is_empty() && text.len() <= codec::COPY_BYTES_MAX);
        Ok(text)
    }

    /// Fits the mirror to a grid, only when it changed; the host sends the size to the
    /// engine. Input: the grid. Output: the new size, or none when unchanged or before the
    /// attach.
    pub fn resize(&mut self, grid: (usize, usize)) -> Option<(u16, u16)> {
        assert!((GRID_MIN..=GRID_MAX).contains(&grid.0) && (GRID_MIN..=GRID_MAX).contains(&grid.1));
        let term = self.term.as_mut()?;
        if grid == (term.columns(), term.screen_lines()) {
            return None;
        }
        term.resize(GridSize {
            columns: grid.0,
            lines: grid.1,
        });
        self.dirty = true;
        let size = (u16::try_from(grid.0).ok()?, u16::try_from(grid.1).ok()?);
        assert_eq!((term.columns(), term.screen_lines()), grid);
        Some(size)
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
