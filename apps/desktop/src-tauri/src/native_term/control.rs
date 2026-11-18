use super::codec::unescape;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The longest control-mode line read before the stream is called broken.
const LINE_BYTES_MAX: u64 = 8 << 20;

/// The most reply lines one command block may carry.
const REPLY_LINES_MAX: usize = 4096;

/// The most replies the client may still owe; more means tmux stopped answering.
const WANTS_MAX: usize = 1 << 16;

/// The only PATH the control client gets, the one the engine gives its own clients' tmux.
const CLIENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// What a command's reply is for: the attach line's own block, a view's pane facts, its
/// screen capture, a plain command or one piece of a paste.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WantKind {
    Startup,
    State,
    Capture,
    Command,
    Paste,
}

/// One reply the client is owed: the view that sent the command and what it is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Want {
    pub view: u64,
    pub kind: WantKind,
}

/// One event from the control client, stamped when its line was read.
pub enum Feed {
    Output {
        pane: String,
        bytes: Vec<u8>,
        at: Instant,
    },
    Reply {
        want: Want,
        lines: Vec<Vec<u8>>,
        ok: bool,
        at: Instant,
    },
    Written {
        view: u64,
        seq: u64,
        at: Instant,
    },
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

/// The flags field, the last word of a `%begin`, `%end` or `%error` line.
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

/// Takes the oldest reply owed. Input: the queue. Output: the want, or none when nothing
/// is owed, which a reply from this client never meets after the startup block.
fn owed(wants: &Mutex<VecDeque<Want>>) -> Option<Want> {
    let mut wants = wants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(wants.len() <= WANTS_MAX);
    wants.pop_front()
}

/// Reads the control stream until it ends: every pane's `%output`, unescaped, and each
/// reply to this client's commands (flag 1) matched to the oldest reply owed. A block ends
/// only at an `%end`/`%error` with its `%begin`'s tag, so reply text cannot end it, and a
/// block over `REPLY_LINES_MAX` lines is reported as failed. Inputs: the stream, the replies
/// owed and the sink, which returns false to stop. Output: none.
fn read_lines(
    stream: impl Read,
    wants: &Mutex<VecDeque<Want>>,
    mut sink: impl FnMut(Feed) -> bool,
) {
    let mut reader = BufReader::with_capacity(1 << 16, stream);
    let mut line = Vec::with_capacity(1 << 16);
    let mut block: Option<(Vec<u8>, Vec<Vec<u8>>, bool)> = None;
    loop {
        line.clear();
        let read = (&mut reader)
            .take(LINE_BYTES_MAX)
            .read_until(b'\n', &mut line);
        let at = Instant::now();
        if !matches!(read, Ok(n) if n > 0) || line.last() != Some(&b'\n') {
            break;
        }
        line.pop();
        if let Some((begin, lines, overflow)) = block.as_mut() {
            match parse(&line) {
                Notice::End { flags, ok, tag } if tag == begin.as_slice() => {
                    let ok = ok && !*overflow;
                    let lines = std::mem::take(lines);
                    block = None;
                    if flags & 1 == 1
                        && let Some(want) = owed(wants)
                        && !sink(Feed::Reply {
                            want,
                            lines,
                            ok,
                            at,
                        })
                    {
                        return;
                    }
                }
                _ if lines.len() < REPLY_LINES_MAX => lines.push(line.clone()),
                _ => *overflow = true,
            }
            continue;
        }
        let keep = match parse(&line) {
            Notice::Output { pane, escaped } => sink(Feed::Output {
                pane: pane.to_owned(),
                bytes: unescape(escaped),
                at,
            }),
            Notice::Begin { tag, .. } => {
                block = Some((tag.to_vec(), Vec::new(), false));
                true
            }
            Notice::Exit => break,
            _ => true,
        };
        if !keep {
            return;
        }
    }
    sink(Feed::Closed);
}

/// Writes queued command bytes to the `tmux -C` stdin in order, off the main thread, so a
/// large paste never blocks drawing; a batch asked to be reported is answered with
/// `Feed::Written` once tmux has taken all of it. Inputs: the stdin, the queue, the sink.
fn write_queue(
    mut stdin: ChildStdin,
    queue: mpsc::Receiver<(u64, u64, Vec<u8>, bool)>,
    mut sink: impl FnMut(Feed) -> bool,
) {
    for (view, seq, bytes, report) in queue {
        assert!(!bytes.is_empty());
        assert!(bytes.ends_with(b"\n"));
        if stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
        if report
            && !sink(Feed::Written {
                view,
                seq,
                at: Instant::now(),
            })
        {
            return;
        }
    }
}

/// The desktop's one tmux control client on the engine's own server, read by every native
/// view: each view's output is told apart by its pane id, and replies come back in the
/// order the commands were written.
pub struct Control {
    child: Child,
    writer: Option<mpsc::Sender<(u64, u64, Vec<u8>, bool)>>,
    thread: Option<JoinHandle<()>>,
    wants: Arc<Mutex<VecDeque<Want>>>,
    batches: u64,
}

impl Control {
    /// Starts the client the way the engine starts its own (`-u -f /dev/null -N`, cleared
    /// environment), so it can never start a server, attached to the engine's session with
    /// `ignore-size` so it never sizes a window, plus its reader and writer threads. Inputs:
    /// the tmux program, the checked engine socket and the sink. Output: the client.
    pub fn attach(
        tmux: &Path,
        socket: &Path,
        sink: impl FnMut(Feed) -> bool + Send + Clone + 'static,
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
        let startup = Want {
            view: 0,
            kind: WantKind::Startup,
        };
        let wants = Arc::new(Mutex::new(VecDeque::from([startup])));
        let read_wants = Arc::clone(&wants);
        let notify = sink.clone();
        std::thread::Builder::new()
            .name("native-tmux-read".into())
            .spawn(move || read_lines(stdout, &read_wants, sink))?;
        let (writer, queue) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("native-tmux-write".into())
            .spawn(move || write_queue(stdin, queue, notify))?;
        Ok(Control {
            child,
            writer: Some(writer),
            thread: Some(thread),
            wants,
            batches: 0,
        })
    }

    /// Records the replies a write is owed, then queues it, so the reader can never meet a
    /// reply before its want. Inputs: the view, what the replies are for, how many there are,
    /// the bytes and whether to report the write. Output: the batch number.
    fn queue(
        &mut self,
        view: u64,
        kind: WantKind,
        count: usize,
        bytes: Vec<u8>,
        report: bool,
    ) -> io::Result<u64> {
        assert!(count >= 1 && kind != WantKind::Startup);
        assert!(bytes.ends_with(b"\n"));
        {
            let mut wants = self
                .wants
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if wants.len() + count > WANTS_MAX {
                return Err(io::Error::other("tmux stopped answering"));
            }
            wants.extend(std::iter::repeat_n(Want { view, kind }, count));
        }
        self.batches += 1;
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| io::Error::other("writer closed"))?;
        writer
            .send((view, self.batches, bytes, report))
            .map_err(|_| io::Error::other("writer closed"))?;
        Ok(self.batches)
    }

    /// Queues one command line, which must end in its only newline. Inputs: the view, what
    /// the reply is for and the line. Output: whether the writer took it.
    pub fn send(&mut self, view: u64, kind: WantKind, line: &str) -> io::Result<()> {
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        self.queue(view, kind, 1, line.as_bytes().to_vec(), false)
            .map(|_| ())
    }

    /// Queues several command lines as one write, so tmux reads them as one batch;
    /// `Feed::Written` with the returned number follows once tmux has taken it all. Inputs:
    /// the view, what the replies are for and the lines. Output: the batch number.
    pub fn send_many(&mut self, view: u64, kind: WantKind, lines: &[String]) -> io::Result<u64> {
        assert!(!lines.is_empty());
        assert!(
            lines
                .iter()
                .all(|l| l.ends_with('\n') && l.matches('\n').count() == 1)
        );
        let joined: String = lines.concat();
        self.queue(view, kind, lines.len(), joined.into_bytes(), true)
    }

    /// The pid of the `tmux -C` child, counted with the terminal's own processes.
    pub fn child_pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer.send((0, 0, b"detach-client\n".to_vec(), false));
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
mod tests {
    use super::*;

    #[test]
    fn parse_lines() {
        assert_eq!(
            parse(b"%output %0 a\\033b"),
            Notice::Output {
                pane: "%0",
                escaped: b"a\\033b"
            }
        );
        assert_eq!(
            parse(b"%output %7 "),
            Notice::Output {
                pane: "%7",
                escaped: b""
            }
        );
        assert_eq!(
            parse(b"%begin 1794966129 323 1"),
            Notice::Begin {
                flags: 1,
                tag: b"1794966129 323 1"
            }
        );
        assert_eq!(
            parse(b"%end 1794966129 321 0"),
            Notice::End {
                flags: 0,
                ok: true,
                tag: b"1794966129 321 0"
            }
        );
        assert_eq!(
            parse(b"%error 1 2 1"),
            Notice::End {
                flags: 1,
                ok: false,
                tag: b"1 2 1"
            }
        );
        assert_eq!(parse(b"%exit"), Notice::Exit);
        assert_eq!(parse(b"%session-changed $0 crazyagent"), Notice::Other);
    }

    #[test]
    fn replies_go_to_the_oldest_want_after_startup() {
        let stream = "%begin 1 1 1\n%end 1 1 1\n%begin 9 7 1\nfirst\n%end 1 1 1\n\
                      second\n%end 9 7 1\n\
                      %output %3 x\n%begin 9 8 1\n%error 9 8 1\n";
        let startup = Want {
            view: 0,
            kind: WantKind::Startup,
        };
        let state = Want {
            view: 4,
            kind: WantKind::State,
        };
        let paste = Want {
            view: 5,
            kind: WantKind::Paste,
        };
        let wants = Mutex::new(VecDeque::from([startup, state, paste]));
        let mut feeds = Vec::new();
        read_lines(stream.as_bytes(), &wants, |feed| {
            feeds.push(feed);
            true
        });
        assert!(
            matches!(&feeds[0], Feed::Reply { want, ok: true, lines, .. }
                if *want == startup && lines.is_empty())
        );
        match &feeds[1] {
            Feed::Reply {
                want, lines, ok, ..
            } => {
                assert!(*ok && *want == state);
                assert_eq!(
                    lines,
                    &vec![
                        b"first".to_vec(),
                        b"%end 1 1 1".to_vec(),
                        b"second".to_vec()
                    ]
                );
            }
            _ => panic!("second feed is the state reply"),
        }
        assert!(
            matches!(&feeds[2], Feed::Output { pane, bytes, .. } if pane == "%3" && bytes == b"x")
        );
        assert!(matches!(&feeds[3], Feed::Reply { want, ok: false, .. } if *want == paste));
        assert!(matches!(&feeds[4], Feed::Closed));
    }
}
