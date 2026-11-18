use super::codec;
use super::control::{Control, WantKind};
use super::screen::{self, GridSize};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use std::collections::VecDeque;

/// The pane facts read once at attach: its id, its window's pane count, size, cursor and the
/// modes a capture does not carry.
pub const STATE_FORMAT: &str = "#{pane_id} #{window_panes} #{pane_width} #{pane_height} \
#{cursor_x} #{cursor_y} #{alternate_on} #{keypad_cursor_flag} #{cursor_flag} \
#{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag} #{mouse_sgr_flag}";

/// The largest pane side accepted, in cells.
pub const GRID_MAX: usize = 1000;

/// History rows read at attach, below the reader's reply-line bound with a full screen.
const HISTORY_LINES: usize = 2000;

/// The most wheel lines one event turns into reports.
const WHEEL_LINES_MAX: u32 = 50;

/// The most timed inputs a view waits on at once; older ones are reported unfinished.
const PROBES_MAX: usize = 256;

/// The longest text a timed input may wait to see on the screen.
pub const EXPECT_CHARS_MAX: usize = 256;

/// The pane's facts from the attach's `display-message`.
pub struct PaneState {
    pub pane: String,
    columns: usize,
    lines: usize,
    cursor_x: usize,
    cursor_y: usize,
    alternate: bool,
    app_cursor: bool,
    cursor_visible: bool,
    mouse: [bool; 4],
}

/// Parses the attach's pane facts; a window with more than one pane is refused, since an
/// engine-server agent window holds only its agent. Input: the reply line. Output: the
/// facts, or none.
pub fn parse_state(line: &[u8]) -> Option<PaneState> {
    let text = std::str::from_utf8(line).ok()?;
    let (pane, rest) = text.split_once(' ')?;
    let n: Vec<usize> = rest
        .split(' ')
        .map(|w| w.parse().ok())
        .collect::<Option<_>>()?;
    let [
        panes,
        columns,
        lines,
        cursor_x,
        cursor_y,
        alternate,
        app_cursor,
        visible,
        m0,
        m1,
        m2,
        m3,
    ] = n[..]
    else {
        return None;
    };
    let fits = (1..=GRID_MAX).contains(&columns) && (1..=GRID_MAX).contains(&lines);
    let plain = codec::pane_id_valid(pane) && panes == 1;
    (fits && plain && cursor_x <= columns && cursor_y < lines).then(|| PaneState {
        pane: pane.to_owned(),
        columns,
        lines,
        cursor_x,
        cursor_y,
        alternate: alternate == 1,
        app_cursor: app_cursor == 1,
        cursor_visible: visible == 1,
        mouse: [m0 == 1, m1 == 1, m2 == 1, m3 == 1],
    })
}

/// The bytes that put a fresh terminal into the pane's captured state: history and screen
/// rows, modes (cursor keys, mouse) and cursor. Inputs: the pane state and the
/// `capture-pane -e -p -S` rows. Output: bytes.
pub fn restore_bytes(state: &PaneState, rows: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.iter().map(|r| r.len() + 2).sum::<usize>() + 96);
    if state.alternate {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    out.extend_from_slice(b"\x1b[H\x1b[2J");
    for (index, row) in rows.iter().take(HISTORY_LINES + state.lines).enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(row);
    }
    out.extend_from_slice(b"\x1b[0m");
    if state.app_cursor {
        out.extend_from_slice(b"\x1b[?1h");
    }
    for (on, mode) in state.mouse.iter().zip(["1000", "1002", "1003", "1006"]) {
        if *on {
            out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
        }
    }
    if !state.cursor_visible {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out.extend_from_slice(
        format!("\x1b[{};{}H", state.cursor_y + 1, state.cursor_x + 1).as_bytes(),
    );
    assert!(out.len() >= 6);
    assert!(out.starts_with(b"\x1b["));
    out
}

/// A key with no text of its own, from the native view or the page's test hook.
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

/// One input to a native view, from AppKit or from the page's test hook. Cells are column
/// and line on the visible screen; `option` is the Option key, which forces a local
/// selection while the pane reads the mouse; `probe` marks a test-hook copy or paste, which
/// uses the private probe pasteboard instead of the owner's.
pub enum Input {
    Text(Vec<u8>),
    Key(Named),
    Wheel { lines: i32, cell: (usize, usize) },
    Press { cell: (usize, usize), option: bool },
    Drag { cell: (usize, usize), option: bool },
    Release { cell: (usize, usize), option: bool },
    Copy { probe: bool },
    Paste { text: String },
}

/// One timed input: what it was, when it began (wall ms), when its command went to tmux
/// and tmux took it all, when the pane's first output after it came, when the screen showed
/// the text it waits for, and when the frame showing that was handed to the display.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Probe {
    pub seq: u64,
    pub kind: String,
    pub t0: f64,
    pub bytes: usize,
    pub sent: Option<f64>,
    pub written: Option<f64>,
    pub accepted: Option<f64>,
    pub output: Option<f64>,
    pub render: Option<f64>,
    pub frame: Option<f64>,
    pub local: bool,
    pub failed: bool,
    #[serde(skip)]
    pub expect: Option<String>,
    #[serde(skip)]
    pub batch: u64,
    #[serde(skip)]
    pub replies: usize,
}

/// The wall clock in milliseconds, the clock `performance.timeOrigin + now()` reads.
pub fn wall_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64() * 1000.0)
}

/// One native view's mirror of its agent's pane: the terminal fed by the capture and then
/// by `%output`, its local selection and scrollback, and the timed inputs it waits on.
pub struct Pane {
    pub view: u64,
    pub target: String,
    pub state: Option<PaneState>,
    pub term: Option<Term<VoidListener>>,
    parser: Processor,
    pub ready: bool,
    pub dirty: bool,
    button_down: bool,
    pastes: u64,
    pub probes: VecDeque<Probe>,
}

impl Pane {
    /// A mirror of an engine-server agent window, empty until its facts and capture come
    /// back. Inputs: the view id and the agent's window name, already checked. Output: the
    /// pane.
    pub fn new(view: u64, name: &str) -> Pane {
        assert!(view > 0);
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let target = format!("{}:={name}", crazyagent::terminal::tmux::SESSION);
        Pane {
            view,
            target,
            state: None,
            term: None,
            parser: Processor::new(),
            ready: false,
            dirty: false,
            button_down: false,
            pastes: 0,
            probes: VecDeque::new(),
        }
    }

    /// Asks tmux for the pane's facts and its screen with history. Input: the client.
    /// Output: whether both commands were queued.
    pub fn request(&mut self, control: &mut Control) -> std::io::Result<()> {
        assert!(self.term.is_none());
        let state = format!("display-message -p -t {} \"{STATE_FORMAT}\"\n", self.target);
        control.send(self.view, WantKind::State, &state)?;
        let capture = format!(
            "capture-pane -e -p -S -{HISTORY_LINES} -t {}\n",
            self.target
        );
        control.send(self.view, WantKind::Capture, &capture)
    }

    /// Takes the facts reply: makes the terminal at the pane's size. Inputs: the reply lines
    /// and whether tmux accepted the command. Output: the pane id, or why the attach failed.
    pub fn on_state(&mut self, lines: &[Vec<u8>], ok: bool) -> Result<String, &'static str> {
        let state = lines.first().filter(|_| ok).and_then(|l| parse_state(l));
        let state = state.ok_or("native_terminal_window_unknown")?;
        let size = GridSize {
            columns: state.columns,
            lines: state.lines,
        };
        self.term = Some(Term::new(Config::default(), &size, VoidListener));
        let pane = state.pane.clone();
        self.state = Some(state);
        assert!(codec::pane_id_valid(&pane));
        Ok(pane)
    }

    /// Takes the capture reply: replays the screen and modes into the terminal. Inputs: the
    /// reply lines and whether tmux accepted the command. Output: whether the mirror is ready.
    pub fn on_capture(&mut self, lines: &[Vec<u8>], ok: bool) -> bool {
        let (Some(term), Some(state)) = (self.term.as_mut(), self.state.as_ref()) else {
            return false;
        };
        if !ok {
            return false;
        }
        self.parser.advance(term, &restore_bytes(state, lines));
        self.ready = true;
        self.dirty = true;
        true
    }

    /// Feeds live output into the terminal and stamps the timed inputs waiting on it.
    /// Inputs: the bytes. Output: whether the terminal took them.
    pub fn on_output(&mut self, bytes: &[u8]) -> bool {
        let Some(term) = self.term.as_mut().filter(|_| self.ready) else {
            return false;
        };
        self.parser.advance(term, bytes);
        self.dirty = true;
        let now = wall_ms();
        for probe in self
            .probes
            .iter_mut()
            .filter(|p| p.sent.is_some() && p.output.is_none())
        {
            probe.output = Some(now);
        }
        self.mark_rendered(now);
        true
    }

    /// Marks rendered every timed input whose output came and whose text, if any, is now on
    /// the screen. Input: the wall time.
    fn mark_rendered(&mut self, now: f64) {
        let waiting = self
            .probes
            .iter()
            .any(|p| p.output.is_some() && p.render.is_none());
        if !waiting {
            return;
        }
        let rows = self
            .term
            .as_ref()
            .map(|t| screen::text_rows(&screen::frame(t)))
            .unwrap_or_default();
        for probe in self
            .probes
            .iter_mut()
            .filter(|p| p.output.is_some() && p.render.is_none())
        {
            let shown = probe
                .expect
                .as_ref()
                .is_none_or(|text| rows.iter().any(|row| row.contains(text.as_str())));
            if shown {
                probe.render = Some(now);
            }
        }
    }

    /// Stamps the frame time on every timed input already rendered and hands them back,
    /// finished. Input: the wall time the frame was handed to the display. Output: the
    /// finished inputs.
    pub fn on_frame(&mut self, now: f64) -> Vec<Probe> {
        let mut done = Vec::new();
        let mut waiting = VecDeque::with_capacity(self.probes.len());
        for mut probe in self.probes.drain(..) {
            if probe.render.is_some() {
                probe.frame = Some(now);
                done.push(probe);
            } else {
                waiting.push_back(probe);
            }
        }
        self.probes = waiting;
        assert!(self.probes.len() <= PROBES_MAX);
        done
    }

    /// Starts timing an input; when too many wait, the oldest is handed back unfinished.
    /// Inputs: the probe. Output: the input dropped to make room, if any.
    pub fn track(&mut self, probe: Probe) -> Option<Probe> {
        assert!(
            probe
                .expect
                .as_ref()
                .is_none_or(|t| t.chars().count() <= EXPECT_CHARS_MAX)
        );
        let dropped = (self.probes.len() >= PROBES_MAX)
            .then(|| self.probes.pop_front())
            .flatten();
        self.probes.push_back(probe);
        assert!(self.probes.len() <= PROBES_MAX);
        dropped
    }

    fn last_probe(&mut self, seq: u64) -> Option<&mut Probe> {
        self.probes.iter_mut().rev().find(|p| p.seq == seq)
    }

    /// Stamps a timed input's command as written whole, by its batch. Input: the batch.
    pub fn on_written(&mut self, batch: u64) {
        let now = wall_ms();
        if let Some(probe) = self
            .probes
            .iter_mut()
            .find(|p| p.batch == batch && p.batch > 0)
        {
            probe.written = Some(now);
        }
    }

    /// Takes one reply to a paste piece: the timed paste waiting on replies counts it down,
    /// fails on a refusal and is accepted at its last reply. Input: whether tmux accepted it.
    pub fn on_paste_reply(&mut self, ok: bool) {
        let now = wall_ms();
        if let Some(probe) = self.probes.iter_mut().find(|p| p.replies > 0) {
            probe.failed |= !ok;
            probe.replies -= 1;
            if probe.replies == 0 {
                probe.accepted = Some(now);
            }
        }
    }

    fn app_cursor(&self) -> bool {
        self.term
            .as_ref()
            .is_some_and(|t| t.mode().contains(TermMode::APP_CURSOR))
    }

    fn mouse_mode(&self) -> bool {
        self.term
            .as_ref()
            .is_some_and(|t| t.mode().intersects(TermMode::MOUSE_MODE))
    }

    /// The pane id commands for this view address, once the facts came back.
    pub fn pane_id(&self) -> Option<String> {
        self.state.as_ref().map(|s| s.pane.clone())
    }

    /// Sends bytes as `send-keys -H` lines. Inputs: the client and the bytes. Output: the
    /// batch number, or why it was not queued.
    fn send_bytes(&mut self, control: &mut Control, bytes: &[u8]) -> std::io::Result<u64> {
        let pane = self
            .pane_id()
            .ok_or_else(|| std::io::Error::other("pane not ready"))?;
        let lines = codec::send_keys_lines(&pane, bytes);
        assert!(!lines.is_empty() || bytes.is_empty());
        if lines.is_empty() {
            return Ok(0);
        }
        control.send_many(self.view, WantKind::Command, &lines)
    }
}

/// What one input did: sent a command (with its batch), changed only the local screen, or
/// nothing.
#[derive(Debug, PartialEq)]
pub enum Effect {
    Sent(u64),
    Local,
    Nothing,
}

impl Pane {
    /// Applies one input: keys and text as `send-keys`, the wheel and mouse as SGR reports
    /// when the pane reads the mouse, else local scrollback and selection, copy to the
    /// pasteboard, paste through a named tmux buffer. Inputs: the client, the input and the
    /// pasteboards. Output: what it did, or why it failed.
    pub fn apply(
        &mut self,
        control: &mut Control,
        input: Input,
        clips: &super::Clips,
    ) -> std::io::Result<Effect> {
        if !self.ready {
            return Ok(Effect::Nothing);
        }
        match input {
            Input::Text(bytes) => self.typed(control, &bytes),
            Input::Key(key) => {
                let bytes = named_bytes(key, self.app_cursor());
                self.typed(control, &bytes)
            }
            Input::Wheel { lines, cell } => self.wheel(control, lines, cell),
            Input::Press { cell, option } => self.button(control, true, cell, option),
            Input::Release { cell, option } => self.button(control, false, cell, option),
            Input::Drag { cell, option } => self.drag(control, cell, option),
            Input::Copy { probe } => Ok(self.copy(if probe { &clips.probe } else { &clips.user })),
            Input::Paste { text } => self.paste(control, &text),
        }
    }

    fn typed(&mut self, control: &mut Control, bytes: &[u8]) -> std::io::Result<Effect> {
        if let Some(term) = self.term.as_mut()
            && term.grid().display_offset() > 0
        {
            term.scroll_display(Scroll::Bottom);
            self.dirty = true;
        }
        Ok(Effect::Sent(self.send_bytes(control, bytes)?))
    }

    fn wheel(
        &mut self,
        control: &mut Control,
        lines: i32,
        cell: (usize, usize),
    ) -> std::io::Result<Effect> {
        let Some(term) = self.term.as_mut() else {
            return Ok(Effect::Nothing);
        };
        if lines == 0 {
            return Ok(Effect::Nothing);
        }
        let mode = *term.mode();
        let count = lines.unsigned_abs().min(WHEEL_LINES_MAX);
        let cell = (
            cell.0.min(term.columns() - 1),
            cell.1.min(term.screen_lines() - 1),
        );
        if mode.intersects(TermMode::MOUSE_MODE) {
            let report = codec::sgr_wheel(lines > 0, cell.0, cell.1);
            let bytes: Vec<u8> = (0..count).flat_map(|_| report.iter().copied()).collect();
            return Ok(Effect::Sent(self.send_bytes(control, &bytes)?));
        }
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let arrow: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let bytes: Vec<u8> = (0..count).flat_map(|_| arrow.iter().copied()).collect();
            return Ok(Effect::Sent(self.send_bytes(control, &bytes)?));
        }
        let before = term.grid().display_offset();
        let after = codec::scroll_clamp(before, lines, term.grid().history_size());
        if after != before {
            term.scroll_display(Scroll::Delta(after as i32 - before as i32));
        }
        self.dirty = true;
        assert!(term.grid().display_offset() <= term.grid().history_size());
        Ok(Effect::Local)
    }

    fn grid_point(&self, cell: (usize, usize)) -> Option<Point> {
        let term = self.term.as_ref()?;
        let (column, line) = (
            cell.0.min(term.columns() - 1),
            cell.1.min(term.screen_lines() - 1),
        );
        let offset = term.grid().display_offset() as i32;
        Some(Point::new(Line(line as i32 - offset), Column(column)))
    }

    fn button(
        &mut self,
        control: &mut Control,
        press: bool,
        cell: (usize, usize),
        option: bool,
    ) -> std::io::Result<Effect> {
        let Some(term) = self.term.as_ref() else {
            return Ok(Effect::Nothing);
        };
        let cell = (
            cell.0.min(term.columns() - 1),
            cell.1.min(term.screen_lines() - 1),
        );
        self.button_down = press;
        if self.mouse_mode() && !option {
            let bytes = codec::sgr_button(0, cell.0, cell.1, press);
            return Ok(Effect::Sent(self.send_bytes(control, &bytes)?));
        }
        if press {
            let point = self.grid_point(cell);
            if let (Some(point), Some(term)) = (point, self.term.as_mut()) {
                term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
            }
            self.dirty = true;
        }
        Ok(Effect::Local)
    }

    fn drag(
        &mut self,
        control: &mut Control,
        cell: (usize, usize),
        option: bool,
    ) -> std::io::Result<Effect> {
        if !self.button_down {
            return Ok(Effect::Nothing);
        }
        let Some(term) = self.term.as_ref() else {
            return Ok(Effect::Nothing);
        };
        let cell = (
            cell.0.min(term.columns() - 1),
            cell.1.min(term.screen_lines() - 1),
        );
        if self.mouse_mode() && !option {
            let bytes = codec::sgr_button(32, cell.0, cell.1, true);
            return Ok(Effect::Sent(self.send_bytes(control, &bytes)?));
        }
        let point = self.grid_point(cell);
        if let (Some(point), Some(selection)) =
            (point, self.term.as_mut().and_then(|t| t.selection.as_mut()))
        {
            selection.update(point, Side::Right);
        }
        self.dirty = true;
        Ok(Effect::Local)
    }

    /// Copies the local selection to a pasteboard, bounded by `COPY_BYTES_MAX`. Input: the
    /// pasteboard. Output: local when copied, nothing when there was no selection.
    fn copy(&mut self, clip: &super::clip::Clip) -> Effect {
        let Some(text) = self.term.as_ref().and_then(|t| t.selection_to_string()) else {
            return Effect::Nothing;
        };
        if text.is_empty() || text.len() > codec::COPY_BYTES_MAX {
            return Effect::Nothing;
        }
        if clip.write(&text) {
            Effect::Local
        } else {
            Effect::Nothing
        }
    }

    /// Pastes text, normalized, as one `set-buffer` + `paste-buffer -d -p -r -S` pair on a
    /// buffer named for this process and paste, so tmux brackets it only when the pane asked
    /// (T7); a text with a NUL goes as 256-byte `send-keys` runs, bracketed from the mirrored
    /// mode. Inputs: the client and the text. Output: the batch number.
    fn paste(&mut self, control: &mut Control, text: &str) -> std::io::Result<Effect> {
        let pane = self
            .pane_id()
            .ok_or_else(|| std::io::Error::other("pane not ready"))?;
        if text.is_empty() || text.len() > codec::COPY_BYTES_MAX {
            return Ok(Effect::Nothing);
        }
        let bracketed = self
            .term
            .as_ref()
            .is_some_and(|t| t.mode().contains(TermMode::BRACKETED_PASTE));
        let nul = text.as_bytes().contains(&0);
        let bytes = codec::paste_bytes(text.as_bytes(), nul && bracketed);
        let lines: Vec<String> = if nul {
            codec::paste_runs(&bytes)
                .iter()
                .flat_map(|run| codec::send_keys_lines(&pane, run))
                .collect()
        } else {
            self.pastes += 1;
            let name = format!("ctm-{}-{}", std::process::id(), self.pastes);
            vec![
                codec::set_buffer_line(&name, &bytes),
                codec::paste_buffer_line(&name, &pane),
            ]
        };
        assert!(!lines.is_empty());
        let batch = control.send_many(self.view, WantKind::Paste, &lines)?;
        if let Some(probe) = self
            .probes
            .back_mut()
            .filter(|p| p.kind == "paste" && p.batch == 0)
        {
            probe.replies = lines.len();
        }
        Ok(Effect::Sent(batch))
    }

    /// Fits the mirror and the agent's window to a grid, only when it changed, with the
    /// engine's own `resize-window` line. Inputs: the client and the grid. Output: whether a
    /// resize was sent.
    pub fn resize(&mut self, control: &mut Control, grid: (usize, usize)) -> std::io::Result<bool> {
        let Some(term) = self.term.as_mut() else {
            return Ok(false);
        };
        if grid == (term.columns(), term.screen_lines()) {
            return Ok(false);
        }
        assert!(grid.0 >= 2 && grid.1 >= 2 && grid.0 <= GRID_MAX && grid.1 <= GRID_MAX);
        term.resize(GridSize {
            columns: grid.0,
            lines: grid.1,
        });
        self.dirty = true;
        let line = format!(
            "resize-window -t {} -x {} -y {}\n",
            self.target, grid.0, grid.1
        );
        control.send(self.view, WantKind::Command, &line)?;
        Ok(true)
    }

    /// Stamps a timed input with what its effect did, by its sequence number. Inputs: the
    /// sequence number and the effect.
    pub fn stamp(&mut self, seq: u64, effect: &Effect) {
        let now = wall_ms();
        let Some(probe) = self.last_probe(seq) else {
            return;
        };
        match effect {
            Effect::Sent(batch) => {
                probe.sent = Some(now);
                probe.batch = *batch;
            }
            Effect::Local => {
                probe.local = true;
                probe.sent = Some(now);
                probe.output = Some(now);
                probe.render = Some(now);
            }
            Effect::Nothing => probe.failed = true,
        }
        assert!(probe.seq == seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_line_parses_and_refuses_split_windows() {
        let line = b"%4 1 80 24 3 5 1 1 1 0 0 0 1";
        let state = parse_state(line).expect("state parses");
        assert_eq!(
            (state.pane.as_str(), state.columns, state.lines),
            ("%4", 80, 24)
        );
        assert!(state.alternate && state.app_cursor && state.mouse[3]);
        assert!(parse_state(b"%4 2 80 24 3 5 1 1 1 0 0 0 1").is_none());
        assert!(parse_state(b"4 1 80 24 3 5 1 1 1 0 0 0 1").is_none());
        assert!(parse_state(b"%4 1 0 24 3 5 1 1 1 0 0 0 1").is_none());
    }

    #[test]
    fn restore_replays_modes_and_ends_at_the_cursor() {
        let state = parse_state(b"%1 1 10 3 2 1 0 1 0 1 0 0 1").expect("state parses");
        let bytes = restore_bytes(&state, &[b"a".to_vec(), b"b".to_vec()]);
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(text.starts_with("\x1b[H\x1b[2Ja\r\nb\x1b[0m\x1b[?1h\x1b[?1000h\x1b[?1006h"));
        assert!(text.ends_with("\x1b[?25l\x1b[2;3H"));
    }

    #[test]
    fn named_keys_follow_cursor_mode() {
        assert_eq!(named_bytes(Named::Up, false), b"\x1b[A");
        assert_eq!(named_bytes(Named::Up, true), b"\x1bOA");
        assert_eq!(named_bytes(Named::BackTab, true), b"\x1b[Z");
        assert_eq!(named_bytes(Named::Delete, false), b"\x1b[3~");
    }
}
