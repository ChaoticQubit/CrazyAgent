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

/// The most replies the client may still owe: two per view.
const WANTS_MAX: usize = 2 * DESKTOP_VIEWS_MAX;

/// The only PATH the control client gets, the one the engine gives its own clients' tmux.
const CLIENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The pane facts read once at bind: its id, the target it answered for (tmux 3.8 answers a
/// missing window from the client's current one), its window's pane count, size, cursor and
/// the modes a capture does not carry.
pub const STATE_FORMAT: &str = "#{pane_id} #{session_name}:=#{window_name} #{window_panes} \
#{pane_width} #{pane_height} \
#{cursor_x} #{cursor_y} #{alternate_on} #{keypad_cursor_flag} #{cursor_flag} \
#{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag} #{mouse_sgr_flag}";

/// What a command's reply is for: a view's pane facts or its screen capture. The attach
/// line's own block carries flags 0 (tmux 3.8), so it is never owed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WantKind {
    State,
    Capture,
}

/// One reply the client is owed: the view that asked and what it is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    let (pane, rest) = text.split_once(' ')?;
    let (target, rest) = rest.split_once(' ')?;
    let n: Vec<usize> = rest
        .split(' ')
        .map(|w| w.parse().ok())
        .collect::<Option<_>>()?;
    let [
        panes,
        columns,
        lines,
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
    Asked(String),
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

    /// Takes one finished flag-1 reply: the oldest want gets it. Facts are kept for the
    /// capture; the capture binds the pane, whose earlier output the capture already holds.
    /// Inputs: the reply lines and whether tmux accepted the command. Output: the event, or
    /// none when nothing was owed or the view closed meanwhile.
    fn reply(&mut self, lines: Vec<Vec<u8>>, ok: bool) -> Option<Event> {
        let want = self.wants.pop_front()?;
        let slot = self.views.get(&want.view)?.clone();
        let view = want.view;
        let refused = |code| (None, Event::Refused { view, code });
        let (next, event) = match (want.kind, slot) {
            (WantKind::State, Slot::Asked(_)) if !ok => refused("native_terminal_window_unknown"),
            (WantKind::State, Slot::Asked(target)) => match lines
                .first()
                .and_then(|l| parse_state(l))
            {
                Some(state) if state.target != target => refused("native_terminal_window_unknown"),
                Some(state) => {
                    self.views.insert(view, Slot::Facts(state));
                    return None;
                }
                None => refused("native_terminal_pane_refused"),
            },
            (WantKind::Capture, Slot::Facts(state)) if ok => {
                let bytes = restore_bytes(&state, &lines);
                (
                    Some(Slot::Bound(state.pane)),
                    Event::Restored { view, bytes },
                )
            }
            (WantKind::Capture, Slot::Facts(_)) => refused("native_terminal_capture_failed"),
            _ => return None,
        };
        match next {
            Some(slot) => self.views.insert(view, slot),
            None => self.views.remove(&view),
        };
        assert_eq!(
            self.views.contains_key(&view),
            matches!(event, Event::Restored { .. })
        );
        Some(event)
    }
}

type Shared = Arc<Mutex<Binds>>;

fn locked(binds: &Mutex<Binds>) -> std::sync::MutexGuard<'_, Binds> {
    binds
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Reads the control stream until it ends: the output of bound panes only (any other
/// pane's output is skipped before it is unescaped), and each flag-1 reply matched to the
/// oldest want. A block ends only at an `%end`/`%error` with its `%begin`'s tag. Inputs:
/// the stream, the binds and the sink, which returns false to stop.
pub fn read_lines(stream: impl Read, binds: &Mutex<Binds>, mut sink: impl FnMut(Event) -> bool) {
    let mut reader = BufReader::with_capacity(1 << 16, stream);
    let mut line = Vec::with_capacity(1 << 16);
    let mut block: Option<(Vec<u8>, Vec<Vec<u8>>, bool)> = None;
    loop {
        line.clear();
        let read = (&mut reader)
            .take(LINE_BYTES_MAX)
            .read_until(b'\n', &mut line);
        if !matches!(read, Ok(n) if n > 0) || line.last() != Some(&b'\n') {
            break;
        }
        line.pop();
        let event = if let Some((begin, lines, overflow)) = block.as_mut() {
            match parse(&line) {
                Notice::End { flags, ok, tag } if tag == begin.as_slice() => {
                    let (ok, lines) = (ok && !*overflow, std::mem::take(lines));
                    block = None;
                    (flags & 1 == 1)
                        .then(|| locked(binds).reply(lines, ok))
                        .flatten()
                }
                _ if lines.len() < REPLY_LINES_MAX => {
                    lines.push(line.clone());
                    None
                }
                _ => {
                    *overflow = true;
                    None
                }
            }
        } else {
            match parse(&line) {
                Notice::Output { pane, escaped } => {
                    let views = locked(binds).readers(pane);
                    let bytes = (!views.is_empty()).then(|| unescape(escaped));
                    bytes.map(|bytes| Event::Output { views, bytes })
                }
                Notice::Begin { tag, .. } => {
                    block = Some((tag.to_vec(), Vec::new(), false));
                    None
                }
                Notice::Exit => break,
                _ => None,
            }
        };
        if event.is_some_and(|event| !sink(event)) {
            return;
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
        let mut child = Command::new(tmux)
            .args(["-u", "-f", "/dev/null", "-N", "-S"])
            .arg(socket)
            .args(["-C", "attach-session", "-t", &session, "-f", "ignore-size"])
            .env_clear()
            .env("PATH", CLIENT_PATH)
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
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

    /// Asks for one agent window's pane facts and capture for a view whose engine view is
    /// attached; the capture reply binds the pane. View ids are never reused. Inputs: the
    /// view and the agent's window name, already checked. Output: ok once asked, or a code.
    pub fn bind(&mut self, view: u64, window: &str) -> Result<(), &'static str> {
        assert!(view > 0);
        assert!(
            window
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let target = format!("{}:={window}", crazyagent::terminal::tmux::SESSION);
        let state = format!("display-message -p -t {target} \"{STATE_FORMAT}\"\n");
        let capture = format!("capture-pane -e -p -S -{HISTORY_LINES} -t {target}\n");
        let mut binds = locked(&self.binds);
        let full = binds.views.len() >= DESKTOP_VIEWS_MAX || binds.wants.len() + 2 > WANTS_MAX;
        if window.is_empty() || binds.views.contains_key(&view) || full {
            return Err("native_terminal_bind_refused");
        }
        let writer = self
            .writer
            .as_ref()
            .ok_or("native_terminal_control_closed")?;
        writer
            .send([state, capture].concat().into_bytes())
            .map_err(|_| "native_terminal_control_closed")?;
        binds.views.insert(view, Slot::Asked(target));
        binds.wants.push_back(Want {
            view,
            kind: WantKind::State,
        });
        binds.wants.push_back(Want {
            view,
            kind: WantKind::Capture,
        });
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
