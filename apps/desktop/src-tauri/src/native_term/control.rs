use super::codec::{self, unescape};
use super::pane::{GRID_MAX, GRID_MIN};
use crazyagent::terminal::DESKTOP_VIEWS_MAX;
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

/// The longest control-mode line read before the stream is called broken.
const LINE_BYTES_MAX: u64 = 8 << 20;

/// History rows read at bind.
const HISTORY_LINES: usize = 2000;

/// The most reply lines one command block may carry: the history and a full screen.
const REPLY_LINES_MAX: usize = HISTORY_LINES + GRID_MAX + 1;

/// The most lines dropped while draining one capture whose rows could not be counted: its
/// history, the tallest pane tmux allows and the guard lines up to the marker's `%end`.
const DRAIN_LINES_MAX: usize = HISTORY_LINES + u16::MAX as usize + 3;

/// Random bytes in a marker, printed in hex after each capture so no pane can print it.
const MARKER_BYTES: usize = 16;

/// The most replies the client may still owe: locate, capture and marker per view.
const WANTS_MAX: usize = 3 * DESKTOP_VIEWS_MAX;

/// The only PATH the control client gets, the one the engine gives its own clients' tmux.
const CLIENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The pane facts read once at bind, the capture's row counts first so a line tmux cut
/// short still counts them: height, history size, pane id, the target it answered for, its
/// window's pane count, width, cursor and the modes a capture does not carry.
pub const STATE_FORMAT: &str = "#{pane_height} #{history_size} \
#{pane_id} #{session_name}:=#{window_name} #{window_panes} #{pane_width} \
#{cursor_x} #{cursor_y} #{alternate_on} #{keypad_cursor_flag} #{cursor_flag} \
#{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag} #{mouse_sgr_flag}";

/// What a command's reply is for: a view's locate (its pane facts, asked for this target),
/// its capture (the rows counted from the locate, none for a locate cut before its counts)
/// or the marker printed after them. The attach line's block (flags 0) is never owed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WantKind {
    State(String),
    Capture(Option<usize>),
    Marker(Vec<u8>),
}

/// One reply the client is owed: the view that asked and what it is for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Want {
    pub view: u64,
    pub kind: WantKind,
}

/// One event from the reader: a view's restored screen (its first bytes), live output for
/// the views bound to a pane, a view the direct path refused with a plain code, or the end
/// of the stream, which ends the direct path of every view.
#[derive(Debug, PartialEq)]
pub enum Event {
    Restored { view: u64, bytes: Vec<u8> },
    Output { views: Vec<u64>, bytes: Vec<u8> },
    Refused { view: u64, code: &'static str },
    Closed,
}

/// One parsed control-mode line.
#[derive(Debug, PartialEq)]
pub enum Notice<'a> {
    Output { pane: &'a str, escaped: &'a [u8] },
    Begin { flags: u32, tag: &'a [u8] },
    End { flags: u32, ok: bool, tag: &'a [u8] },
    Exit,
    Other,
}

fn flags_of(rest: &[u8]) -> u32 {
    let last = rest.rsplit(|b| *b == b' ').next().unwrap_or(b"");
    std::str::from_utf8(last)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Parses one control-mode line without its newline. Input: the line. Output: what it says.
pub fn parse(line: &[u8]) -> Notice<'_> {
    if let Some(rest) = line.strip_prefix(b"%output ") {
        let split = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
        let Ok(pane) = std::str::from_utf8(&rest[..split]) else {
            return Notice::Other;
        };
        let escaped = rest.get(split + 1..).unwrap_or(b"");
        return Notice::Output { pane, escaped };
    }
    if let Some(rest) = line.strip_prefix(b"%begin ") {
        return Notice::Begin {
            flags: flags_of(rest),
            tag: rest,
        };
    }
    if let Some(rest) = line.strip_prefix(b"%end ") {
        return Notice::End {
            flags: flags_of(rest),
            ok: true,
            tag: rest,
        };
    }
    if let Some(rest) = line.strip_prefix(b"%error ") {
        return Notice::End {
            flags: flags_of(rest),
            ok: false,
            tag: rest,
        };
    }
    if line == b"%exit" || line.starts_with(b"%exit ") {
        return Notice::Exit;
    }
    Notice::Other
}

/// The pane facts from a bind's `display-message`.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneState {
    pub pane: String,
    pub target: String,
    pub columns: usize,
    pub lines: usize,
    cursor_x: usize,
    cursor_y: usize,
    alternate: bool,
    app_cursor: bool,
    cursor_visible: bool,
    mouse: [bool; 4],
}

/// Parses a bind's pane facts; a window with more than one pane is refused, since an
/// engine-server agent window holds only its agent. Input: the reply line. Output: the
/// facts, or none. The caller checks the target.
pub fn parse_state(line: &[u8]) -> Option<PaneState> {
    let text = std::str::from_utf8(line).ok()?;
    let words: Vec<&str> = text.split(' ').collect();
    let [height, history, pane, target, rest @ ..] = &words[..] else {
        return None;
    };
    let (pane, target) = (*pane, *target);
    let n: Vec<usize> = [height, history]
        .into_iter()
        .chain(rest)
        .map(|w| w.parse().ok())
        .collect::<Option<_>>()?;
    let [
        lines,
        _history,
        panes,
        columns,
        x,
        y,
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
    let sides = [columns, lines];
    let fits = sides
        .iter()
        .all(|side| (GRID_MIN..=GRID_MAX).contains(side));
    let plain = codec::pane_id_valid(pane) && panes == 1;
    let state = (fits && plain && x <= columns && y < lines).then(|| PaneState {
        pane: pane.to_owned(),
        target: target.to_owned(),
        columns,
        lines,
        cursor_x: x,
        cursor_y: y,
        alternate: alternate == 1,
        app_cursor: app_cursor == 1,
        cursor_visible: visible == 1,
        mouse: [m0 == 1, m1 == 1, m2 == 1, m3 == 1],
    })?;
    assert!(codec::pane_id_valid(&state.pane));
    Some(state)
}

/// The rows a capture holds, from the counts that open its locate line. tmux may cut a format
/// short under load, but each value in the text is whole, so both counts are whole once the
/// space after the second is there. Input: the locate reply. Output: the pane height plus
/// the history held to `HISTORY_LINES`, or none for a line cut before both counts.
fn counted(lines: &[Vec<u8>]) -> Option<usize> {
    let mut fields = lines.first()?.splitn(3, |byte| *byte == b' ');
    let height: usize = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    let history: usize = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    fields.next()?;
    let rows = height.saturating_add(history.min(HISTORY_LINES));
    assert!(rows >= height);
    assert!(rows - height <= HISTORY_LINES);
    Some(rows)
}

/// A fresh marker: random bytes in hex, which no pane can print ahead of time. Output: the
/// marker, or `native_terminal_bind_refused` when the system gives no randomness.
fn fresh_marker() -> Result<String, &'static str> {
    let mut random = [0u8; MARKER_BYTES];
    getrandom::fill(&mut random).map_err(|_| "native_terminal_bind_refused")?;
    let text: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(text.len(), MARKER_BYTES * 2);
    assert!(text.bytes().all(|byte| byte.is_ascii_hexdigit()));
    Ok(text)
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
    let cursor = format!("\x1b[{};{}H", state.cursor_y + 1, state.cursor_x + 1);
    out.extend_from_slice(cursor.as_bytes());
    assert!(out.len() >= 6);
    assert!(out.starts_with(b"\x1b["));
    out
}

/// Where one view's direct path stands: asked, its pane facts back, or bound to its pane
/// after its capture, the only state whose pane output is read.
#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Asked,
    Facts(PaneState),
    Bound(String),
}

/// The live views, their slots and the owed replies, shared by the reader and the host
/// under one lock. A view the host closed is gone from here, so a late reply binds nothing.
#[derive(Debug, Default)]
pub struct Binds {
    wants: VecDeque<Want>,
    views: HashMap<u64, Slot>,
}

/// How a reply block opening now is read: its closing guard searched for, that many
/// capture rows taken as data first, or every line dropped up to this marker line.
#[derive(Debug, PartialEq)]
enum Opening {
    Searched,
    Counted(usize),
    Drained(Vec<u8>),
}

/// The stream ended or fell out of step with the replies owed, so the reader stops.
#[derive(Debug, PartialEq)]
struct Stop;

impl Binds {
    /// The views bound to a pane, none for a pane no view reads.
    fn readers(&self, pane: &str) -> Vec<u64> {
        let mut views: Vec<u64> = self
            .views
            .iter()
            .filter(|(_, slot)| matches!(slot, Slot::Bound(p) if p == pane))
            .map(|(view, _)| *view)
            .collect();
        views.sort_unstable();
        assert!(views.len() <= DESKTOP_VIEWS_MAX);
        views
    }

    /// How to read the flag-1 block opening now. A capture's rows are the pane's own text and
    /// can look like any control line, its closing guard included, so they are counted as
    /// data, never searched; a capture whose locate was cut before its counts is drained up
    /// to its marker. Output: how to read the block.
    fn opening(&self) -> Opening {
        let Some(front) = self.wants.front() else {
            return Opening::Searched;
        };
        let opening = match (&front.kind, self.wants.get(1)) {
            (WantKind::Capture(Some(rows)), _) => Opening::Counted(*rows),
            (WantKind::Capture(None), Some(next)) => match &next.kind {
                WantKind::Marker(marker) if next.view == front.view => {
                    Opening::Drained(marker.clone())
                }
                _ => unreachable!("a capture is owed before its own marker"),
            },
            (WantKind::Capture(None), None) => unreachable!("a capture is owed before its marker"),
            _ => Opening::Searched,
        };
        assert!(opening == Opening::Searched || matches!(front.kind, WantKind::Capture(_)));
        opening
    }

    /// Drops the next `count` wants, each owed to this view: the replies tmux skipped.
    fn skip(&mut self, view: u64, count: usize) {
        assert!(count == 1 || count == 2);
        for _ in 0..count {
            let skipped = self.wants.pop_front();
            assert!(skipped.is_some_and(|want| want.view == view));
        }
    }

    /// Takes one finished flag-1 reply for the oldest want. tmux skips the rest of a command
    /// line after an `%error`, so a failed locate drops its capture and marker and a failed
    /// capture its marker. Inputs: the lines, whether it closed with `%end` and whether every
    /// line was kept. Output: the event, or Stop for a marker reply that is not the marker.
    fn reply(
        &mut self,
        lines: Vec<Vec<u8>>,
        ran: bool,
        whole: bool,
    ) -> Result<Option<Event>, Stop> {
        let Some(want) = self.wants.pop_front() else {
            return Ok(None);
        };
        let view = want.view;
        match want.kind {
            WantKind::State(target) => Ok(self.located(view, &target, &lines, (ran, whole))),
            WantKind::Marker(marker) if ran && lines.len() == 1 && lines[0] == marker => Ok(None),
            WantKind::Marker(_) => Err(Stop),
            WantKind::Capture(_) => {
                if !ran {
                    self.skip(view, 1);
                }
                Ok(self.captured(view, &lines, ran && whole))
            }
        }
    }

    /// Takes a locate reply: one that ran sets its capture's row count, then keeps the facts
    /// for the capture or refuses the view; a failed one (no such window) drops its capture
    /// and marker and refuses the view. Inputs: the view, the target asked, the lines and
    /// (ran, whole). Output: the refusal, if any.
    fn located(
        &mut self,
        view: u64,
        target: &str,
        lines: &[Vec<u8>],
        (ran, whole): (bool, bool),
    ) -> Option<Event> {
        if ran {
            let rows = counted(lines);
            match self.wants.front_mut() {
                Some(Want {
                    view: owed,
                    kind: WantKind::Capture(count),
                }) if *owed == view => *count = rows,
                _ => unreachable!("a locate is followed by its own capture"),
            }
        } else {
            self.skip(view, 2);
        }
        if self.views.get(&view) != Some(&Slot::Asked) {
            return None;
        }
        let state = lines.first().and_then(|line| parse_state(line));
        let code = match state.filter(|_| ran && whole) {
            _ if !ran => "native_terminal_window_unknown",
            Some(state) if state.target != target => "native_terminal_window_unknown",
            Some(state) => {
                self.views.insert(view, Slot::Facts(state));
                return None;
            }
            None => "native_terminal_pane_refused",
        };
        self.views.remove(&view);
        assert!(!self.views.contains_key(&view));
        Some(Event::Refused { view, code })
    }

    /// Takes a capture reply for a view whose facts are back: a whole one binds the pane and
    /// restores the screen, anything else refuses the view. Inputs: the view, the rows and
    /// whether the capture ran whole. Output: the event, or none for a view gone meanwhile.
    fn captured(&mut self, view: u64, lines: &[Vec<u8>], whole: bool) -> Option<Event> {
        let Some(Slot::Facts(state)) = self.views.get(&view).cloned() else {
            return None;
        };
        let event = if whole {
            let bytes = restore_bytes(&state, lines);
            self.views.insert(view, Slot::Bound(state.pane));
            Event::Restored { view, bytes }
        } else {
            self.views.remove(&view);
            Event::Refused {
                view,
                code: "native_terminal_capture_failed",
            }
        };
        let restored = matches!(event, Event::Restored { .. });
        assert_eq!(self.views.contains_key(&view), restored);
        Some(event)
    }

    /// Ends the drain of a capture whose rows could not be counted: its capture and marker
    /// wants go, and a view still waiting on it is refused. Output: the refusal, if any.
    fn drained(&mut self) -> Option<Event> {
        let front = self.wants.front()?;
        assert!(matches!(front.kind, WantKind::Capture(None)));
        let view = front.view;
        self.skip(view, 2);
        let waiting = matches!(self.views.get(&view), Some(Slot::Facts(_)));
        if waiting {
            self.views.remove(&view);
        }
        assert!(!matches!(self.views.get(&view), Some(Slot::Facts(_))));
        waiting.then_some(Event::Refused {
            view,
            code: "native_terminal_capture_failed",
        })
    }

    /// Ends the direct path of each view reading a pane whose `%output` line a drain dropped,
    /// since output meant for it may be gone. Input: the pane. Output: the refusals.
    fn lose(&mut self, pane: &str) -> Vec<Event> {
        let views = self.readers(pane);
        for view in &views {
            self.views.remove(view);
        }
        assert!(self.readers(pane).is_empty());
        views
            .into_iter()
            .map(|view| Event::Refused {
                view,
                code: "native_terminal_output_lost",
            })
            .collect()
    }
}

type Shared = Arc<Mutex<Binds>>;

fn locked(binds: &Mutex<Binds>) -> std::sync::MutexGuard<'_, Binds> {
    binds
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One reply block being read: its begin tag and flags, its lines (up to
/// `REPLY_LINES_MAX`), how many it had, and the capture rows it takes as data first.
struct Block {
    tag: Vec<u8>,
    flags: u32,
    lines: Vec<Vec<u8>>,
    seen: usize,
    rows: Option<usize>,
}

/// The lines dropped for a capture whose rows cannot be counted, up to the `%end` after its
/// marker line: `opened` is the last begin tag seen, the marker block's own once the marker
/// shows, since tmux prints that block after every capture row; `lines` counts the drop.
struct Drain {
    marker: Vec<u8>,
    opened: Vec<u8>,
    found: bool,
    lines: usize,
}

/// Where the reader stands: inside a reply block, draining a capture, or between them.
#[derive(Default)]
struct Reader {
    block: Option<Block>,
    drain: Option<Drain>,
}

impl Reader {
    /// Reads one line. Between blocks only bound `%output`, `%begin` and the end of the
    /// stream count; a guard there is out of step. Inputs: the line and the binds. Output:
    /// its events, or Stop.
    fn line(&mut self, line: &[u8], binds: &Mutex<Binds>) -> Result<Vec<Event>, Stop> {
        assert!(self.block.is_none() || self.drain.is_none());
        if self.drain.is_some() {
            return self.drain_line(line, binds);
        }
        if self.block.is_some() {
            return self.block_line(line, binds);
        }
        match parse(line) {
            Notice::Output { pane, escaped } => {
                let views = locked(binds).readers(pane);
                let bytes = (!views.is_empty()).then(|| unescape(escaped));
                Ok(bytes
                    .map(|bytes| Event::Output { views, bytes })
                    .into_iter()
                    .collect())
            }
            Notice::Begin { flags, tag } => {
                let owed = flags & 1 == 1;
                let opening = if owed {
                    locked(binds).opening()
                } else {
                    Opening::Searched
                };
                self.open(tag, flags, opening);
                Ok(Vec::new())
            }
            Notice::End { .. } | Notice::Exit => Err(Stop),
            Notice::Other => Ok(Vec::new()),
        }
    }

    fn open(&mut self, tag: &[u8], flags: u32, opening: Opening) {
        let rows = match opening {
            Opening::Searched => None,
            Opening::Counted(rows) => Some(rows),
            Opening::Drained(marker) => {
                assert!(!marker.is_empty() && !marker.contains(&b'\n'));
                self.drain = Some(Drain {
                    marker,
                    opened: tag.to_vec(),
                    found: false,
                    lines: 0,
                });
                return;
            }
        };
        self.block = Some(Block {
            tag: tag.to_vec(),
            flags,
            lines: Vec::new(),
            seen: 0,
            rows,
        });
        assert!(self.drain.is_none());
    }

    /// One line inside a block: a capture's rows are data until counted; after them, as in
    /// any other block, only the block's own `%end`/`%error` closes it, and anything else
    /// after a counted capture is out of step. Inputs: the line and the binds. Output: events.
    fn block_line(&mut self, line: &[u8], binds: &Mutex<Binds>) -> Result<Vec<Event>, Stop> {
        let block = self.block.as_mut().expect("a block is open");
        let counted = block.rows.is_some_and(|rows| block.seen < rows);
        let closing = match parse(line) {
            Notice::End { ok, tag, .. } if !counted && tag == block.tag.as_slice() => Some(ok),
            _ => None,
        };
        let Some(ran) = closing else {
            if block.rows.is_some() && !counted {
                return Err(Stop);
            }
            block.seen += 1;
            if block.lines.len() < REPLY_LINES_MAX {
                block.lines.push(line.to_vec());
            }
            return Ok(Vec::new());
        };
        let done = self.block.take().expect("a block is open");
        let whole = done.seen == done.lines.len();
        if done.flags & 1 == 0 {
            return Ok(Vec::new());
        }
        let event = locked(binds).reply(done.lines, ran, whole)?;
        Ok(event.into_iter().collect())
    }

    /// One line of a drain: dropped until the marker shows, then only the marker block's
    /// `%end` ends the drain. A dropped `%output` line ends the direct path of the views
    /// reading its pane. Inputs: the line and the binds. Output: events, or Stop.
    fn drain_line(&mut self, line: &[u8], binds: &Mutex<Binds>) -> Result<Vec<Event>, Stop> {
        let drain = self.drain.as_mut().expect("a drain is open");
        if drain.found {
            let opened = drain.opened.as_slice();
            let closed = matches!(parse(line), Notice::End { ok: true, tag, .. } if tag == opened);
            if !closed {
                return Err(Stop);
            }
            self.drain = None;
            return Ok(locked(binds).drained().into_iter().collect());
        }
        drain.lines += 1;
        if drain.lines > DRAIN_LINES_MAX {
            return Err(Stop);
        }
        if line == drain.marker.as_slice() {
            drain.found = true;
            return Ok(Vec::new());
        }
        match parse(line) {
            Notice::Begin { tag, .. } => {
                drain.opened = tag.to_vec();
                Ok(Vec::new())
            }
            Notice::Output { pane, .. } => Ok(locked(binds).lose(pane)),
            _ => Ok(Vec::new()),
        }
    }
}

/// Reads the control stream until it ends or falls out of step: the output of bound panes
/// only (any other pane's output is skipped before it is unescaped) and each flag-1 reply
/// matched to the oldest want, a capture's rows counted as data. Inputs: the stream, the
/// binds and the sink, which returns false to stop.
pub fn read_lines(stream: impl Read, binds: &Mutex<Binds>, mut sink: impl FnMut(Event) -> bool) {
    let mut reader = BufReader::with_capacity(1 << 16, stream);
    let mut line = Vec::with_capacity(1 << 16);
    let mut place = Reader::default();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(LINE_BYTES_MAX)
            .read_until(b'\n', &mut line);
        if !matches!(read, Ok(n) if n > 0) || line.last() != Some(&b'\n') {
            break;
        }
        line.pop();
        let Ok(events) = place.line(&line, binds) else {
            break;
        };
        for event in events {
            if !sink(event) {
                return;
            }
        }
    }
    sink(Event::Closed);
}

fn write_lines(mut stdin: ChildStdin, queue: mpsc::Receiver<Vec<u8>>) {
    for bytes in queue {
        assert!(bytes.ends_with(b"\n"));
        if stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

/// The desktop's one tmux control client on the engine's own server, output only: it asks
/// for nothing but each bound view's pane facts and capture, and reads only the output of
/// panes a live view bound. Input never goes through it.
pub struct Control {
    child: Child,
    writer: Option<mpsc::Sender<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
    binds: Shared,
}

impl Control {
    /// Starts the client the way the engine starts its own (`-u -f /dev/null -N`, cleared
    /// environment), so it can never start a server, attached to the engine's session with
    /// `ignore-size` so it never sizes a window. Inputs: the tmux program, the checked
    /// engine socket and the sink, called on the reader thread. Output: the client.
    pub fn attach(
        tmux: &Path,
        socket: &Path,
        sink: impl FnMut(Event) -> bool + Send + 'static,
    ) -> io::Result<Control> {
        assert!(tmux.is_absolute());
        assert!(socket.is_absolute());
        let folder = socket
            .parent()
            .ok_or_else(|| io::Error::other("socket has no folder"))?;
        let session = format!("={}", crazyagent::terminal::tmux::SESSION);
        let mut command = Command::new(tmux);
        command
            .args(["-u", "-f", "/dev/null", "-N", "-S"])
            .arg(socket)
            .args(["-C", "attach-session", "-t", &session, "-f", "ignore-size"])
            .env_clear()
            .env("PATH", CLIENT_PATH)
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crazyagent::spawn_gate::spawn(&mut command)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let binds: Shared = Arc::new(Mutex::new(Binds::default()));
        let reader_binds = Arc::clone(&binds);
        std::thread::Builder::new()
            .name("native-tmux-read".into())
            .spawn(move || read_lines(stdout, &reader_binds, sink))?;
        let (writer, queue) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("native-tmux-write".into())
            .spawn(move || write_lines(stdin, queue))?;
        let writer = Some(writer);
        Ok(Control {
            child,
            writer,
            thread: Some(thread),
            binds,
        })
    }

    /// Asks for one agent window's pane facts, capture and a fresh marker on one command line,
    /// so tmux runs them back to back and the capture's rows match the counts; the capture
    /// reply binds the pane. View ids are never reused. Inputs: the view and the checked
    /// window name. Output: ok once asked, or a code.
    pub fn bind(&mut self, view: u64, window: &str) -> Result<(), &'static str> {
        assert!(view > 0);
        assert!(
            window
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let target = format!("{}:={window}", crazyagent::terminal::tmux::SESSION);
        let marker = fresh_marker()?;
        let line = format!(
            "list-panes -t {target} -f '#{{pane_active}}' -F \"{STATE_FORMAT}\" ; \
             capture-pane -e -p -S -{HISTORY_LINES} -t {target} ; display-message -p '{marker}'\n"
        );
        let mut binds = locked(&self.binds);
        let full = binds.views.len() >= DESKTOP_VIEWS_MAX || binds.wants.len() + 3 > WANTS_MAX;
        if window.is_empty() || binds.views.contains_key(&view) || full {
            return Err("native_terminal_bind_refused");
        }
        let writer = self.writer.as_ref().expect("the writer lives until drop");
        writer
            .send(line.into_bytes())
            .map_err(|_| "native_terminal_control_closed")?;
        binds.views.insert(view, Slot::Asked);
        binds.wants.extend([
            Want {
                view,
                kind: WantKind::State(target),
            },
            Want {
                view,
                kind: WantKind::Capture(None),
            },
            Want {
                view,
                kind: WantKind::Marker(marker.into_bytes()),
            },
        ]);
        assert!(binds.wants.len() <= WANTS_MAX);
        Ok(())
    }

    /// Ends a view's direct path, as any engine Ended does: its pane's output is no longer
    /// read for it, and a reply still owed to it binds nothing. Input: the view. Output:
    /// whether it was live.
    pub fn close(&mut self, view: u64) -> bool {
        let mut binds = locked(&self.binds);
        let was = binds.views.remove(&view).is_some();
        assert!(!binds.views.contains_key(&view));
        was
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer.send(b"detach-client\n".to_vec());
        }
        let mut exited = false;
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
