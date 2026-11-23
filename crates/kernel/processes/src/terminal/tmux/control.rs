use super::*;

#[cfg(test)]
mod read_tests;

pub(super) struct Control {
    pub(super) child: Mutex<Option<Child>>,
    pub(super) input: Mutex<Option<ChildStdin>>,
    pub(super) state: Arc<Mutex<State>>,
    pub(super) serial: u64,
    #[cfg(test)]
    pub(super) listed: std::sync::atomic::AtomicUsize,
}

/// The serial the next control client gets, unique within the process, so a pane id found
/// through one client is never used through a client of another server (`Tmux::input_at`).
pub(super) static CONTROL_SERIALS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[derive(Default)]
pub(super) struct State {
    pub(super) pending: VecDeque<Pending>,
    pub(super) viewers: Vec<Viewer>,
    pub(super) closed: bool,
}

pub(super) struct Viewer {
    pub(super) pane: Vec<u8>,
    pub(super) window: Vec<u8>,
    pub(super) feed: Feed,
}

pub(super) enum Pending {
    Startup,
    Reply(SyncSender<Answer>),
    Attach {
        agent: AgentId,
        located: Option<Lines>,
        history: usize,
        view: bool,
        reply: SyncSender<Result<TerminalStream, HostError>>,
        marker: Vec<u8>,
    },
    /// The capture and marker blocks still to come after an attach's located reply too large
    /// to keep: the capture's pane rows cannot be counted, so the reader drops every line up
    /// to the marker line, as it does for a cut capture.
    Marker(Vec<u8>),
    /// The block that prints the marker of an attach already answered: it holds no pane
    /// text, so the reader finds its end by its guard and drops it, whatever it printed.
    Spent,
}

/// Empties the private server's environment so nothing reaches a pane from it: no
/// variables copied from an attaching client (`update-environment` empty) and none left in
/// the global or session environment, then checks both are empty.
/// Input: the control client. Output: Ok, or `terminal_environment_unclean`.
pub(super) fn clear_environment(control: &Control) -> Result<(), HostError> {
    let unclean = || failure("terminal_environment_unclean");
    control
        .run("set-option -g update-environment \"\"")
        .map_err(|_| unclean())?;
    for (scope, unset) in [
        ("-g".to_owned(), "-gu".to_owned()),
        (format!("-t {SESSION}"), format!("-t {SESSION} -u")),
    ] {
        let shown = control
            .run(&format!("show-environment {scope}"))
            .map_err(|_| unclean())?;
        for name in variable_names(&shown) {
            control
                .run(&format!("set-environment {unset} {}", quote(&name)))
                .map_err(|_| unclean())?;
        }
        let left = control
            .run(&format!("show-environment {scope}"))
            .map_err(|_| unclean())?;
        if !variable_names(&left).is_empty() {
            return Err(unclean());
        }
    }
    let update = control
        .run("show-options -gv update-environment")
        .map_err(|_| unclean())?;
    if update.iter().any(|line| !line.is_empty()) {
        return Err(unclean());
    }
    Ok(())
}

/// Reads the variables a `show-environment` reply sets, skipping `-NAME` lines, which
/// mark a variable removed. Input: the reply lines. Output: the names, in order.
pub(super) fn variable_names(lines: &[Vec<u8>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| String::from_utf8_lossy(line))
        .filter(|line| !line.starts_with('-'))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The pin a lookup hands out: a pane id the caller already checked, and the serial of the
/// control connection that found it, which counts from 1. Inputs: the pane id and the
/// serial. Output: the `PaneAt` that later pinned reads and sends must match.
pub(super) fn pin_at(pane: String, serial: u64) -> PaneAt {
    assert!(serial > 0, "control serials start at 1");
    assert!(
        pane_id_valid(&pane),
        "a pin names a pane id, never a window target"
    );
    PaneAt { pane, serial }
}

/// Pins a teammates' server to the pid it first answered with (PLC c): a server under
/// another pid at the same socket, such as one a process bound there after the teammates'
/// server ended, is refused, so no listing or input of its is taken for the lead's team.
/// The pin is read from its record before the first check and recorded before the first pid
/// is taken (PC-L1). Takes the new control and the socket's pin. Returns Ok,
/// `terminal_command_failed` for an unreadable pid, `terminal_own_server_unavailable` for a
/// record that cannot be read or kept, or `terminal_teammates_server_replaced`.
pub(super) fn pinned(control: &Control, pin: &ServerPin) -> Result<(), HostError> {
    let lines = control.ask(&format!("display-message -p '#{{pid}}{FORMAT_END}'"))?;
    let pid = lines
        .first()
        .and_then(|line| std::str::from_utf8(line).ok())
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| failure("terminal_command_failed"))?;
    let mut kept = pin
        .pid
        .lock()
        .map_err(|_| failure("terminal_unavailable"))?;
    if kept.is_none() {
        *kept =
            super::super::teammates_server_pid(&pin.root, &pin.lead, &pin.socket, &pin.hooks.lead)?;
    }
    match *kept {
        None => {
            super::super::keep_teammates_server_pid(
                &pin.root,
                &pin.lead,
                &pin.socket,
                pid,
                &pin.hooks.lead,
            )?;
            *kept = Some(pid);
        }
        Some(first) if first != pid => return Err(failure(TEAMMATES_SERVER_REPLACED)),
        Some(_) => {}
    }
    assert_eq!(*kept, Some(pid));
    Ok(())
}

impl Drop for Control {
    fn drop(&mut self) {
        self.close();
    }
}

impl Control {
    pub(super) fn closed(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.closed,
            Err(_) => true,
        }
    }

    pub(super) fn send(&self, line: &str, pending: Pending) -> Result<(), HostError> {
        self.send_all(vec![(line, pending)])
    }

    /// Queues each command's pending reply in order and writes all the commands in one write
    /// under one hold of the client's input, so no other caller's command lands between them.
    /// Takes the commands with their pendings, at most `COMMANDS_SENT_MAX`. Returns once they
    /// are written, or the closed client's error.
    pub(super) fn send_all(&self, commands: Vec<(&str, Pending)>) -> Result<(), HostError> {
        assert!(!commands.is_empty());
        assert!(commands.len() <= COMMANDS_SENT_MAX);
        assert!(
            commands
                .iter()
                .all(|(line, _)| !line.contains('\n') && !line.contains('\r'))
        );
        let mut input = self.input.lock().map_err(|_| closed())?;
        let Some(writer) = input.as_mut() else {
            return Err(closed());
        };
        let mut bytes = Vec::with_capacity(commands.iter().map(|(line, _)| line.len() + 1).sum());
        {
            let mut state = self.state.lock().map_err(|_| closed())?;
            if state.closed {
                return Err(closed());
            }
            for (line, pending) in commands {
                state.pending.push_back(pending);
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
        }
        if writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_err()
        {
            *input = None;
            drop(input);
            self.stop();
            return Err(closed());
        }
        Ok(())
    }

    pub(super) fn wait<T>(&self, answer: &Receiver<Result<T, HostError>>) -> Result<T, HostError> {
        match answer.recv_timeout(REPLY_TIME) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                Err(failure("terminal_not_answering"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(closed()),
        }
    }

    /// Runs commands back to back: sends them all at once through `send_all`, then waits for
    /// each answer in order. Takes the commands. Returns once all succeeded, or the first error.
    pub(super) fn run_all(&self, lines: &[String]) -> Result<(), HostError> {
        assert!(!lines.is_empty());
        let mut answers = Vec::with_capacity(lines.len());
        let commands = lines
            .iter()
            .map(|line| {
                let (reply, answer) = sync_channel(1);
                answers.push(answer);
                (line.as_str(), Pending::Reply(reply))
            })
            .collect();
        self.send_all(commands)?;
        assert_eq!(answers.len(), lines.len());
        for answer in &answers {
            self.wait(answer)?;
        }
        Ok(())
    }

    pub(super) fn run(&self, line: &str) -> Answer {
        #[cfg(test)]
        if line.starts_with("list-panes") {
            self.listed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let (reply, answer) = sync_channel(1);
        self.send(line, Pending::Reply(reply))?;
        self.wait(&answer)
    }

    /// Runs a command that only reads, whose every reply line is a format ending with
    /// `FORMAT_END`, and asks again while tmux cut a line short, for at most `REPLY_TIME`.
    /// Takes the command. Returns the lines without the mark, or `terminal_not_answering`.
    pub(super) fn ask(&self, line: &str) -> Answer {
        let until = Instant::now() + REPLY_TIME;
        loop {
            let lines = self.run(line)?;
            let kept: Option<Lines> = lines
                .iter()
                .map(|line| whole(line).map(<[u8]>::to_vec))
                .collect();
            if let Some(kept) = kept {
                return Ok(kept);
            }
            if Instant::now() >= until {
                return Err(failure("terminal_not_answering"));
            }
        }
    }

    pub(super) fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            state.pending.clear();
            state.viewers.clear();
        }
        if let Ok(mut input) = self.input.lock() {
            *input = None;
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub(super) fn close(&self) {
        if let Ok(mut input) = self.input.lock() {
            *input = None;
        }
        let deadline = Instant::now() + CLOSE_TIME;
        if let Ok(mut slot) = self.child.lock()
            && let Some(child) = slot.as_mut()
        {
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.stop();
    }
}

pub(super) struct Located {
    pub(super) pane: Vec<u8>,
    pub(super) window: Vec<u8>,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) cursor: (u16, u16),
    pub(super) modes: Modes,
    pub(super) history: usize,
}

/// Reads an attach's pane line: its height and history size first, as [`counted`] reads
/// them, then its ids, cursor, width, input modes, and its screen and mouse modes. A screen
/// or mouse flag that an older tmux leaves empty reads as off. Takes the reply lines.
/// Returns the pane, or none for a line tmux cut short or a reply of another shape.
pub(super) fn located(lines: &[Vec<u8>]) -> Option<Located> {
    let text = std::str::from_utf8(whole(lines.first()?)?).ok()?;
    let (rows, history) = counted(lines)?;
    let fields: Vec<&str> = text.split(' ').collect();
    let [
        _,
        _,
        pane,
        window,
        x,
        y,
        cols,
        paste,
        keys,
        alternate,
        standard,
        button,
        all,
        sgr,
        utf8,
    ] = fields.as_slice()
    else {
        return None;
    };
    if !pane.starts_with('%') || !window.starts_with('@') {
        return None;
    }
    let number = |text: &str| text.parse::<u16>().ok();
    let flag = |text: &str| match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    let mode = |text: &str| {
        if text.is_empty() {
            Some(false)
        } else {
            flag(text)
        }
    };
    Some(Located {
        pane: pane.as_bytes().to_vec(),
        window: window.as_bytes().to_vec(),
        cols: number(cols)?,
        rows,
        cursor: (number(x)?, number(y)?),
        modes: Modes {
            bracketed_paste: flag(paste)?,
            cursor_keys: flag(keys)?,
            alternate_screen: mode(alternate)?,
            mouse_standard: mode(standard)?,
            mouse_button: mode(button)?,
            mouse_all: mode(all)?,
            mouse_utf8: mode(utf8)?,
            mouse_sgr: mode(sgr)?,
        },
        history,
    })
}

/// Reads the pane height and history size that open an attach's pane line, also from a line
/// tmux cut short: tmux stops a format at the first item it reaches once the time is up and
/// answers with the text so far, each value already in it whole, so both counts are whole
/// once the space after the second is there. Takes the reply lines. Returns the height and
/// the history size held to `HISTORY_ROWS_MAX`, or none for a line cut before that.
pub(super) fn counted(lines: &[Vec<u8>]) -> Option<(u16, usize)> {
    let mut fields = lines.first()?.splitn(3, |byte| *byte == b' ');
    let rows = std::str::from_utf8(fields.next()?)
        .ok()?
        .parse::<u16>()
        .ok()?;
    let history = std::str::from_utf8(fields.next()?).ok()?;
    let history = history.parse::<usize>().ok()?.min(HISTORY_ROWS_MAX);
    fields.next()?;
    Some((rows, history))
}

pub(super) fn viewer(
    state: &mut State,
    agent: AgentId,
    located_lines: &[Vec<u8>],
    capture: &[Vec<u8>],
    limit: usize,
    view: bool,
) -> Result<TerminalStream, HostError> {
    let Located {
        pane,
        window,
        cols,
        rows,
        cursor,
        modes,
        history,
    } = located(located_lines).ok_or_else(|| failure(REPLY_CUT))?;
    let history = history.min(limit);
    assert!(capture.len() <= history + usize::from(rows));
    let (earlier, screen) = capture.split_at(history.min(capture.len()));
    let (feed, receiver) = buffer();
    if view {
        state.viewers.retain(|viewer| viewer.feed.open());
        let of_pane = state
            .viewers
            .iter()
            .filter(|viewer| viewer.pane == pane)
            .count();
        if of_pane >= VIEWERS_MAX || state.viewers.len() >= SERVER_VIEWERS_MAX {
            return Err(failure("terminal_viewers_full"));
        }
        state.viewers.push(Viewer { pane, window, feed });
        assert!(state.viewers.len() <= SERVER_VIEWERS_MAX);
    }
    Ok(TerminalStream {
        agent,
        cols,
        rows,
        history: scrollback(earlier, rows),
        screen: render(screen, cols, rows, cursor, modes),
        chunks: receiver,
        at: None,
    })
}

pub(super) fn error_code(lines: &[Vec<u8>]) -> HostError {
    let missing = lines
        .iter()
        .any(|line| line.windows(10).any(|word| word == b"can't find"));
    failure(if missing {
        "terminal_agent_unknown"
    } else {
        "terminal_command_failed"
    })
}

/// Hands one closed reply block to the request at the front of the queue. `ran` is whether
/// the block closed with `%end`, since tmux skips the rest of a command line after an
/// `%error`: an attach whose located reply ran keeps its place for the capture, one whose
/// located reply was too large to keep leaves its capture and marker to be drained, and one
/// whose capture ran leaves its marker block to be dropped. Takes the state, the answer and
/// whether the command ran.
pub(super) fn finish(state: &mut State, answer: Answer, ran: bool) {
    let Some(front) = state.pending.pop_front() else {
        return;
    };
    match front {
        Pending::Startup | Pending::Marker(_) | Pending::Spent => {}
        Pending::Reply(reply) => {
            let _ = reply.send(answer);
        }
        Pending::Attach {
            agent,
            located: None,
            history,
            view,
            reply,
            marker,
        } => match answer {
            Ok(lines) => state.pending.push_front(Pending::Attach {
                agent,
                located: Some(lines),
                history,
                view,
                reply,
                marker,
            }),
            Err(error) => {
                let _ = reply.send(Err(error));
                if ran {
                    state.pending.push_front(Pending::Marker(marker));
                }
            }
        },
        Pending::Attach {
            agent,
            located: Some(lines),
            history,
            view,
            reply,
            ..
        } => {
            let opened =
                answer.and_then(|screen| viewer(state, agent, &lines, &screen, history, view));
            let _ = reply.send(opened);
            if ran {
                state.pending.push_front(Pending::Spent);
            }
        }
    }
}

pub(super) fn deliver(state: &mut State, pane: &[u8], bytes: &[u8]) {
    state.viewers.retain(|viewer| {
        if viewer.pane != pane {
            return viewer.feed.open();
        }
        viewer.feed.push(bytes)
    });
}

pub(super) fn guard<'a>(line: &'a [u8], word: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(word)?.strip_prefix(b" ")
}

/// How the reader finds the end of the reply block opening now: `Searched` looks for its
/// closing guard, `Counted` takes that many rows of an attach's capture as data first, and
/// `Drained` drops every line up to the attach's own marker line.
pub(super) enum Rows {
    Searched,
    Counted(usize),
    Drained(Vec<u8>),
}

/// How to read the reply block opening now, by the request at the front of the queue. An
/// attach's capture holds the history rows the attach read just before, then the screen
/// rows; capture-pane prints one line per pane row and the pane's own text can look like any
/// control line, including the block's closing guard, so those rows are counted as data,
/// never searched for a guard. The counts open the located line, so a line tmux cut after
/// both is still counted: that capture is read whole and the attach, failing on the cut line,
/// is asked again. An attach whose located reply was cut before both counts leaves the queue
/// failing with `REPLY_CUT`, so its caller asks again, and its capture is drained; so are the
/// blocks after a located reply too large to keep. Takes the state. Returns how to read the
/// block.
pub(super) fn opening(state: &mut State) -> Rows {
    match state.pending.pop_front() {
        Some(Pending::Marker(marker)) => Rows::Drained(marker),
        Some(Pending::Attach {
            agent,
            located: Some(lines),
            history,
            view,
            reply,
            marker,
        }) => {
            let Some((rows, found)) = counted(&lines) else {
                let _ = reply.send(Err(failure(REPLY_CUT)));
                return Rows::Drained(marker);
            };
            let rows = found.min(history) + usize::from(rows);
            state.pending.push_front(Pending::Attach {
                agent,
                located: Some(lines),
                history,
                view,
                reply,
                marker,
            });
            Rows::Counted(rows)
        }
        Some(other) => {
            state.pending.push_front(other);
            Rows::Searched
        }
        None => Rows::Searched,
    }
}

/// The lines the reader drops for one attach, from the opening guard of a capture whose rows
/// cannot be counted up to the `%end` after its marker line. `opened` is the last opening
/// guard seen, which once the marker shows is the marker block's own, since tmux prints that
/// block after every row of the capture; `found` is whether the marker showed, and `lines`
/// counts the lines dropped, at most `DRAIN_LINES_MAX`.
pub(super) struct Drain {
    pub(super) marker: Vec<u8>,
    pub(super) opened: Vec<u8>,
    pub(super) found: bool,
    pub(super) lines: usize,
}

/// Ends the views of the pane a dropped `%output` line names, so each reads `Chunk::Behind`
/// and attaches again, since output meant for it may be gone; any other line changes
/// nothing. Takes the state and the dropped line.
pub(super) fn lose(state: &mut State, line: &[u8]) {
    let Some(rest) = guard(line, b"%output") else {
        return;
    };
    let pane = rest.split(|byte| *byte == b' ').next().unwrap_or_default();
    state.viewers.retain(|viewer| {
        if viewer.pane != pane {
            return viewer.feed.open();
        }
        viewer.feed.lost()
    });
}

/// The pane ids a tmux window layout lists, as `%<id>`: each leaf cell is
/// `<width>x<height>,<x>,<y>,<id>`, while a split cell holds child cells in braces or brackets
/// after its own `<x>,<y>`. Takes the layout. Returns the ids, none for a layout not read.
pub(super) fn layout_panes(layout: &[u8]) -> Vec<Vec<u8>> {
    let words: Vec<&[u8]> = layout
        .split(|byte| matches!(byte, b',' | b'{' | b'}' | b'[' | b']'))
        .filter(|word| !word.is_empty())
        .collect();
    let mut panes = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if !word.contains(&b'x') {
            continue;
        }
        if let Some(id) = words.get(at + 3)
            && id.iter().all(u8::is_ascii_digit)
        {
            let mut pane = b"%".to_vec();
            pane.extend_from_slice(id);
            panes.push(pane);
        }
    }
    assert!(panes.len() <= layout.len());
    panes
}

/// One reply block the reader is inside: its opening guard, the lines kept, how many lines
/// and bytes it held so far, the rows an attach's capture holds, and whether it passed
/// `REPLY_BYTES_MAX` or held a line past `LINE_BYTES_MAX`, which fails only its own request.
pub(super) struct Block {
    pub(super) opened: Vec<u8>,
    pub(super) lines: Lines,
    pub(super) seen: usize,
    pub(super) bytes: usize,
    pub(super) rows: Option<usize>,
    pub(super) oversized: bool,
}

/// Reads one control line without its newline, keeping at most `LINE_BYTES_MAX` bytes of
/// it; the rest of a longer line is read and dropped. Takes the reader and an empty buffer.
/// Returns how many bytes were dropped, or none once the output ends.
pub(super) fn read_line(output: &mut impl BufRead, line: &mut Vec<u8>) -> Option<usize> {
    let read = output
        .by_ref()
        .take(LINE_BYTES_MAX as u64 + 1)
        .read_until(b'\n', line)
        .ok()?;
    if read == 0 {
        return None;
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        return Some(0);
    }
    if line.len() <= LINE_BYTES_MAX {
        return None;
    }
    let skipped = output
        .skip_until(b'\n')
        .ok()
        .filter(|skipped| *skipped > 0)?;
    let dropped = line.len() - LINE_BYTES_MAX + skipped - 1;
    line.truncate(LINE_BYTES_MAX);
    Some(dropped)
}

pub(super) fn read(output: ChildStdout, state: &Mutex<State>) {
    let mut output = BufReader::new(output);
    let mut block: Option<Block> = None;
    let mut drain: Option<Drain> = None;
    loop {
        let mut line = Vec::new();
        let Some(dropped) = read_line(&mut output, &mut line) else {
            break;
        };
        let Ok(mut state) = state.lock() else {
            break;
        };
        if let Some(dropping) = &mut drain {
            assert!(block.is_none());
            if dropping.found {
                if dropped == 0 && guard(&line, b"%end") == Some(dropping.opened.as_slice()) {
                    drain = None;
                    continue;
                }
                break;
            }
            dropping.lines += 1;
            if dropping.lines > DRAIN_LINES_MAX {
                break;
            }
            if dropped == 0 && line == dropping.marker {
                dropping.found = true;
            } else if let Some(opened) = guard(&line, b"%begin").filter(|_| dropped == 0) {
                dropping.opened = opened.to_vec();
            } else {
                lose(&mut state, &line);
            }
            continue;
        }
        if let Some(open) = &mut block {
            let counted = open.rows.is_some_and(|rows| open.seen < rows);
            let closing = !counted && dropped == 0;
            let ended = closing && guard(&line, b"%end") == Some(open.opened.as_slice());
            let failed = closing && guard(&line, b"%error") == Some(open.opened.as_slice());
            if ended || failed {
                let lines = std::mem::take(&mut open.lines);
                let answer = if open.oversized {
                    Err(failure("terminal_reply_too_large"))
                } else if ended {
                    Ok(lines)
                } else {
                    Err(error_code(&lines))
                };
                block = None;
                finish(&mut state, answer, ended);
                continue;
            }
            if open.rows.is_some() && !counted {
                break;
            }
            open.seen += 1;
            open.bytes = open.bytes.saturating_add(line.len() + dropped + 1);
            if dropped > 0 || open.bytes > REPLY_BYTES_MAX {
                open.oversized = true;
                open.lines = Vec::new();
            }
            if !open.oversized {
                open.lines.push(line);
            }
            continue;
        }
        if dropped > 0 {
            lose(&mut state, &line);
            continue;
        }
        if let Some(opened) = guard(&line, b"%begin") {
            let rows = match opening(&mut state) {
                Rows::Searched => None,
                Rows::Counted(rows) => Some(rows),
                Rows::Drained(marker) => {
                    assert!(!marker.is_empty() && !marker.contains(&b'\n'));
                    drain = Some(Drain {
                        marker,
                        opened: opened.to_vec(),
                        found: false,
                        lines: 0,
                    });
                    continue;
                }
            };
            block = Some(Block {
                opened: opened.to_vec(),
                lines: Vec::new(),
                seen: 0,
                bytes: 0,
                rows,
                oversized: false,
            });
        } else if let Some(rest) = guard(&line, b"%output") {
            let (pane, data) = rest
                .iter()
                .position(|byte| *byte == b' ')
                .map_or((rest, &b""[..]), |space| {
                    (&rest[..space], &rest[space + 1..])
                });
            deliver(&mut state, pane, &decode(data));
        } else if let Some(rest) =
            guard(&line, b"%window-close").or_else(|| guard(&line, b"%unlinked-window-close"))
        {
            let window = rest.split(|byte| *byte == b' ').next().unwrap_or_default();
            state.viewers.retain(|viewer| viewer.window != window);
        } else if let Some(rest) = guard(&line, b"%layout-change") {
            let mut fields = rest.split(|byte| *byte == b' ');
            let window = fields.next().unwrap_or_default();
            let panes = layout_panes(fields.next().unwrap_or_default());
            if !panes.is_empty() {
                state
                    .viewers
                    .retain(|viewer| viewer.window != window || panes.contains(&viewer.pane));
            }
        } else if line == b"%exit" || guard(&line, b"%exit").is_some() {
            break;
        }
    }
    if let Ok(mut state) = state.lock() {
        state.closed = true;
        state.pending.clear();
        state.viewers.clear();
    }
}

/// A fresh marker for one attach line: random bytes in hex, which no pane can have printed
/// before tmux runs the line, so the reader drops a cut capture up to it and no further.
/// Returns the marker, or `terminal_command_failed` when no random bytes are to be had.
pub(super) fn fresh_marker() -> Result<String, HostError> {
    let mut random = [0u8; MARKER_BYTES];
    getrandom::fill(&mut random).map_err(|_| failure("terminal_command_failed"))?;
    let mut text = String::with_capacity(MARKER_BYTES * 2);
    for byte in random {
        let _ = write!(text, "{byte:02x}");
    }
    assert_eq!(text.len(), MARKER_BYTES * 2);
    Ok(text)
}

pub(super) fn windows(control: &Control) -> Result<Vec<AgentId>, HostError> {
    let lines = control.ask(&format!(
        "list-windows -t {SESSION} -F '#{{window_name}}{FORMAT_END}'"
    ))?;
    let mut agents: Vec<AgentId> = lines
        .iter()
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|name| AgentId::new(name).ok())
        .collect();
    agents.sort();
    agents.dedup();
    Ok(agents)
}
