use super::buffer::{Feed, buffer};
use super::{
    AgentId, Host, HostError, INPUT_BYTES_MAX, PaneRef, SCROLLBACK_LINES, SIZE_MAX, SIZE_MIN,
    TerminalStream, VIEWERS_MAX, end_except, failure,
};
use crate::harnesses::interactive::InteractiveCommand;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SESSION: &str = "crazyagent";
pub const SOCKET: &str = "tmux.sock";
pub const SOCKET_PATH_BYTES_MAX: usize = 100;
const HOLDER: &str = "_crazyagent";
const SERVER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const ENV_PROGRAM: &str = "/usr/bin/env -u TMUX -u TMUX_PANE";
const TERMINAL_TYPE: &str = "xterm-256color";
const SEARCH: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const VERSION_MIN: (u32, u32) = (3, 2);
const REPLY_TIME: Duration = Duration::from_secs(10);
const CLOSE_TIME: Duration = Duration::from_secs(2);
const LINE_BYTES_MAX: usize = 1024 * 1024;
const REPLY_BYTES_MAX: usize = 32 * 1024 * 1024;
const COMMAND_BYTES_MAX: usize = 256 * 1024;
const ARGS_MAX: usize = 256;
const ENV_MAX: usize = 128;
const ENV_NAME_BYTES_MAX: usize = 128;

type Lines = Vec<Vec<u8>>;
type Answer = Result<Lines, HostError>;

/// The engine's terminal host on macOS and Linux: one private tmux server driven in control mode.
/// The server's socket lives in the engine's own private folder and never the owner's tmux.
/// Agents keep running when the engine stops; a later host on the same folder finds them again.
pub struct Tmux {
    executable: Option<PathBuf>,
    folder: PathBuf,
    control: Mutex<Option<Arc<Control>>>,
    spawning: Mutex<()>,
}

struct Control {
    child: Mutex<Option<Child>>,
    input: Mutex<Option<ChildStdin>>,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Pending>,
    viewers: Vec<Viewer>,
    closed: bool,
}

struct Viewer {
    pane: Vec<u8>,
    window: Vec<u8>,
    feed: Feed,
}

enum Pending {
    Startup,
    Reply(SyncSender<Answer>),
    Attach {
        agent: AgentId,
        located: Option<Lines>,
        reply: SyncSender<Result<TerminalStream, HostError>>,
    },
}

/// Finds the tmux program the engine uses: setup's own copy first, then the usual system folders.
/// Takes the engine root and searches `<root>/setup/bin`, Homebrew, `/usr/local/bin`, `/usr/bin`, `/bin`.
/// Returns the canonical program, or none when tmux is not installed.
pub fn installed(engine_root: &Path) -> Option<PathBuf> {
    std::iter::once(engine_root.join("setup/bin"))
        .chain(SEARCH.iter().map(PathBuf::from))
        .map(|folder| folder.join("tmux"))
        .find(|program| {
            std::fs::metadata(program)
                .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
        })
        .and_then(|program| program.canonicalize().ok())
}

impl Tmux {
    /// Opens the host on the engine's private tmux server; nothing starts until the first call.
    /// Takes the tmux program, none when tmux is missing, and the engine's private terminal folder.
    /// Returns the host; every call fails with `terminal_unavailable` while tmux is missing.
    pub fn open(executable: Option<PathBuf>, folder: &Path) -> Self {
        Self {
            executable: executable.filter(|program| program.is_absolute()),
            folder: folder.to_path_buf(),
            control: Mutex::new(None),
            spawning: Mutex::new(()),
        }
    }

    /// Gives the private tmux server's socket. Takes the host.
    /// Returns `<folder>/tmux.sock`.
    pub fn socket(&self) -> PathBuf {
        self.folder.join(SOCKET)
    }

    fn running(&self) -> bool {
        std::fs::symlink_metadata(self.socket())
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    }

    fn live_control(&self) -> Result<Arc<Control>, HostError> {
        if self.executable.is_none() {
            return Err(failure("terminal_unavailable"));
        }
        if !self.running() {
            return Err(failure("terminal_agent_unknown"));
        }
        self.control()
    }

    fn control(&self) -> Result<Arc<Control>, HostError> {
        let mut slot = self
            .control
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if let Some(control) = slot.as_ref()
            && !control.closed()
        {
            return Ok(control.clone());
        }
        *slot = None;
        let control = Arc::new(self.connect()?);
        *slot = Some(control.clone());
        Ok(control)
    }

    fn connect(&self) -> Result<Control, HostError> {
        let executable = self
            .executable
            .as_deref()
            .ok_or_else(|| failure("terminal_unavailable"))?;
        private_folder(&self.folder)?;
        let socket = self.socket();
        if socket.as_os_str().len() > SOCKET_PATH_BYTES_MAX {
            return Err(failure("terminal_socket_path_long"));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&socket)
            && (!metadata.file_type().is_socket() || metadata.uid() != user())
        {
            return Err(failure("terminal_folder_unsafe"));
        }
        let mut child = Command::new(executable)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&socket)
            .args([
                "-C",
                "new-session",
                "-A",
                "-s",
                SESSION,
                "-n",
                HOLDER,
                "cat",
            ])
            .env_clear()
            .env("PATH", SERVER_PATH)
            .current_dir(&self.folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| failure("terminal_unavailable"))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure("terminal_unavailable"));
        };
        let state = Arc::new(Mutex::new(State {
            pending: VecDeque::from([Pending::Startup]),
            ..State::default()
        }));
        let reader = state.clone();
        let control = Control {
            child: Mutex::new(Some(child)),
            input: Mutex::new(Some(input)),
            state,
        };
        if std::thread::Builder::new()
            .name("terminal-tmux".into())
            .spawn(move || read(output, &reader))
            .is_err()
        {
            control.stop();
            return Err(failure("terminal_unavailable"));
        }
        let version = control
            .run("display-message -p '#{version}'")
            .map_err(|_| failure("terminal_unavailable"))?;
        if !version
            .first()
            .and_then(|line| supported(&String::from_utf8_lossy(line)))
            .unwrap_or(false)
        {
            return Err(failure("terminal_tmux_unsupported"));
        }
        for option in [
            format!("set-option -g default-terminal {TERMINAL_TYPE}"),
            "set-option -gw allow-rename off".into(),
            "set-option -gw automatic-rename off".into(),
            "set-option -gw remain-on-exit off".into(),
            format!("set-option -g history-limit {SCROLLBACK_LINES}"),
        ] {
            control
                .run(&option)
                .map_err(|_| failure("terminal_unavailable"))?;
        }
        Ok(control)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.close();
    }
}

impl Control {
    fn closed(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.closed,
            Err(_) => true,
        }
    }

    fn send(&self, line: &str, pending: Pending) -> Result<(), HostError> {
        assert!(!line.contains('\n') && !line.contains('\r'));
        let mut input = self.input.lock().map_err(|_| closed())?;
        let Some(writer) = input.as_mut() else {
            return Err(closed());
        };
        {
            let mut state = self.state.lock().map_err(|_| closed())?;
            if state.closed {
                return Err(closed());
            }
            state.pending.push_back(pending);
        }
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
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

    fn wait<T>(&self, answer: &Receiver<Result<T, HostError>>) -> Result<T, HostError> {
        match answer.recv_timeout(REPLY_TIME) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                Err(failure("terminal_not_answering"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(closed()),
        }
    }

    fn run(&self, line: &str) -> Answer {
        let (reply, answer) = sync_channel(1);
        self.send(line, Pending::Reply(reply))?;
        self.wait(&answer)
    }

    fn stop(&self) {
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

    fn close(&self) {
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

fn closed() -> HostError {
    failure("terminal_closed")
}

fn user() -> u32 {
    rustix::process::getuid().as_raw()
}

fn private_folder(folder: &Path) -> Result<(), HostError> {
    let unsafe_folder = || failure("terminal_folder_unsafe");
    if !folder.is_absolute() {
        return Err(unsafe_folder());
    }
    match std::fs::DirBuilder::new().mode(0o700).create(folder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(unsafe_folder()),
    }
    let metadata = std::fs::symlink_metadata(folder).map_err(|_| unsafe_folder())?;
    if !metadata.is_dir() || metadata.uid() != user() || metadata.mode() & 0o077 != 0 {
        return Err(unsafe_folder());
    }
    Ok(())
}

fn supported(version: &str) -> Option<bool> {
    let version = version.trim();
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let found = (major.parse::<u32>().ok()?, digits.parse::<u32>().ok()?);
    Some(found >= VERSION_MIN)
}

fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' | '"' | '$' => {
                quoted.push('\\');
                quoted.push(character);
            }
            control if (control as u32) < 0x20 || control as u32 == 0x7f => {
                let _ = write!(quoted, "\\{:03o}", control as u32);
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn decode(data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len());
    let mut at = 0;
    while at < data.len() {
        let octal = data
            .get(at + 1..at + 4)
            .filter(|digits| data[at] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .and_then(|digits| {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                u8::try_from(value).ok()
            });
        match octal {
            Some(byte) => {
                bytes.push(byte);
                at += 4;
            }
            None => {
                bytes.push(data[at]);
                at += 1;
            }
        }
    }
    assert!(bytes.len() <= data.len());
    bytes
}

fn target(agent: &AgentId) -> String {
    format!("{SESSION}:={agent}")
}

fn spawn_line(agent: &AgentId, command: &InteractiveCommand) -> Result<String, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = command.cwd.to_str().ok_or_else(invalid)?;
    let first = command.argv.first().ok_or_else(invalid)?;
    let env_name = |name: &str| {
        !name.is_empty()
            && name.len() <= ENV_NAME_BYTES_MAX
            && !name.as_bytes()[0].is_ascii_digit()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if command.argv.len() > ARGS_MAX
        || command.env.len() > ENV_MAX
        || !command.cwd.is_absolute()
        || cwd.contains(['#', '\0'])
        || first.is_empty()
        || first.starts_with('-')
        || first.contains('=')
        || command.argv.iter().any(|arg| arg.contains('\0'))
        || command
            .env
            .iter()
            .any(|(name, value)| !env_name(name) || value.contains('\0'))
    {
        return Err(invalid());
    }
    let mut line = format!(
        "new-window -d -P -F '#{{pane_id}} #{{pane_pid}}' -n {agent} -c {} -t {SESSION}:",
        quote(cwd)
    );
    for (name, value) in &command.env {
        line.push_str(" -e ");
        line.push_str(&quote(&format!("{name}={value}")));
    }
    line.push_str(" -- ");
    line.push_str(ENV_PROGRAM);
    for arg in &command.argv {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    if line.len() > COMMAND_BYTES_MAX {
        return Err(invalid());
    }
    Ok(line)
}

fn render(screen: &[Vec<u8>], cols: u16, rows: u16, cursor: (u16, u16)) -> Vec<u8> {
    let mut bytes = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    for (row, line) in screen.iter().take(usize::from(rows)).enumerate() {
        if row > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(line);
    }
    let column = cursor.0.min(cols.saturating_sub(1)) + 1;
    let line = cursor.1.min(rows.saturating_sub(1)) + 1;
    bytes.extend_from_slice(format!("\x1b[0m\x1b[{line};{column}H").as_bytes());
    bytes
}

struct Located {
    pane: Vec<u8>,
    window: Vec<u8>,
    cols: u16,
    rows: u16,
    cursor: (u16, u16),
}

fn located(lines: &[Vec<u8>]) -> Option<Located> {
    let text = std::str::from_utf8(lines.first()?).ok()?;
    let fields: Vec<&str> = text.split(' ').collect();
    let [pane, window, x, y, cols, rows] = fields.as_slice() else {
        return None;
    };
    if !pane.starts_with('%') || !window.starts_with('@') {
        return None;
    }
    let number = |text: &str| text.parse::<u16>().ok();
    Some(Located {
        pane: pane.as_bytes().to_vec(),
        window: window.as_bytes().to_vec(),
        cols: number(cols)?,
        rows: number(rows)?,
        cursor: (number(x)?, number(y)?),
    })
}

fn viewer(
    state: &mut State,
    agent: AgentId,
    located_lines: &[Vec<u8>],
    screen: &[Vec<u8>],
) -> Result<TerminalStream, HostError> {
    let Located {
        pane,
        window,
        cols,
        rows,
        cursor,
    } = located(located_lines).ok_or_else(|| failure("terminal_command_failed"))?;
    state.viewers.retain(|viewer| viewer.feed.open());
    if state.viewers.len() >= VIEWERS_MAX {
        return Err(failure("terminal_viewers_full"));
    }
    let (feed, receiver) = buffer();
    state.viewers.push(Viewer { pane, window, feed });
    assert!(state.viewers.len() <= VIEWERS_MAX);
    Ok(TerminalStream {
        agent,
        cols,
        rows,
        screen: render(screen, cols, rows, cursor),
        chunks: receiver,
    })
}

fn error_code(lines: &[Vec<u8>]) -> HostError {
    let missing = lines
        .iter()
        .any(|line| line.windows(10).any(|word| word == b"can't find"));
    failure(if missing {
        "terminal_agent_unknown"
    } else {
        "terminal_command_failed"
    })
}

fn finish(state: &mut State, answer: Answer) {
    let Some(front) = state.pending.pop_front() else {
        return;
    };
    match front {
        Pending::Startup => {}
        Pending::Reply(reply) => {
            let _ = reply.send(answer);
        }
        Pending::Attach {
            agent,
            located: None,
            reply,
        } => match answer {
            Ok(lines) => state.pending.push_front(Pending::Attach {
                agent,
                located: Some(lines),
                reply,
            }),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        },
        Pending::Attach {
            agent,
            located: Some(lines),
            reply,
        } => {
            let opened = answer.and_then(|screen| viewer(state, agent, &lines, &screen));
            let _ = reply.send(opened);
        }
    }
}

fn deliver(state: &mut State, pane: &[u8], bytes: &[u8]) {
    state.viewers.retain(|viewer| {
        if viewer.pane != pane {
            return viewer.feed.open();
        }
        viewer.feed.push(bytes)
    });
}

fn guard<'a>(line: &'a [u8], word: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(word)?.strip_prefix(b" ")
}

fn read(output: ChildStdout, state: &Mutex<State>) {
    let mut output = BufReader::new(output);
    let mut block: Option<(Vec<u8>, Lines, usize)> = None;
    loop {
        let mut line = Vec::new();
        match output
            .by_ref()
            .take(LINE_BYTES_MAX as u64 + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.pop() != Some(b'\n') {
            break;
        }
        let Ok(mut state) = state.lock() else {
            break;
        };
        if let Some((opened, lines, bytes)) = &mut block {
            let ended = guard(&line, b"%end").filter(|rest| *rest == opened.as_slice());
            let failed = guard(&line, b"%error").filter(|rest| *rest == opened.as_slice());
            if ended.is_some() || failed.is_some() {
                let lines = std::mem::take(lines);
                let answer = if ended.is_some() {
                    Ok(lines)
                } else {
                    Err(error_code(&lines))
                };
                block = None;
                finish(&mut state, answer);
                continue;
            }
            *bytes += line.len() + 1;
            if *bytes > REPLY_BYTES_MAX {
                break;
            }
            lines.push(line);
            continue;
        }
        if let Some(opened) = guard(&line, b"%begin") {
            block = Some((opened.to_vec(), Vec::new(), 0));
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

fn windows(control: &Control) -> Result<Vec<AgentId>, HostError> {
    let lines = control.run(&format!("list-windows -t {SESSION} -F '#{{window_name}}'"))?;
    let mut agents: Vec<AgentId> = lines
        .iter()
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|name| AgentId::new(name).ok())
        .collect();
    agents.sort();
    agents.dedup();
    Ok(agents)
}

impl Host for Tmux {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        let line = spawn_line(agent, command)?;
        let control = self.control()?;
        let _one = self
            .spawning
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if windows(&control)?.contains(agent) {
            return Err(failure("terminal_agent_running"));
        }
        let lines = control.run(&line)?;
        let text = lines
            .first()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .unwrap_or_default();
        let (pane, pid) = text
            .split_once(' ')
            .ok_or_else(|| failure("terminal_command_failed"))?;
        let pid = pid
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0 && pane.starts_with('%'))
            .ok_or_else(|| failure("terminal_command_failed"))?;
        Ok(PaneRef {
            agent: agent.clone(),
            pane: pane.to_owned(),
            pid,
        })
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        let control = self.live_control()?;
        let target = target(agent);
        let line = format!(
            "display-message -p -t {target} '#{{pane_id}} #{{window_id}} #{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{pane_height}}' ; capture-pane -p -e -t {target}"
        );
        let (reply, answer) = sync_channel(1);
        control.send(
            &line,
            Pending::Attach {
                agent: agent.clone(),
                located: None,
                reply,
            },
        )?;
        control.wait(&answer)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mut line = format!("send-keys -t {} -H", target(agent));
        for byte in bytes {
            let _ = write!(line, " {byte:02x}");
        }
        self.live_control()?.run(&line).map(drop)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        let size = SIZE_MIN..=SIZE_MAX;
        if !size.contains(&cols) || !size.contains(&rows) {
            return Err(failure("terminal_size_bounds"));
        }
        self.live_control()?
            .run(&format!(
                "resize-window -t {} -x {cols} -y {rows}",
                target(agent)
            ))
            .map(drop)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        self.live_control()?
            .run(&format!("kill-window -t {}", target(agent)))
            .map(drop)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        if self.executable.is_none() {
            return Err(failure("terminal_unavailable"));
        }
        if !self.running() {
            return Ok(Vec::new());
        }
        let control = self.control()?;
        windows(&control)
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        if self.executable.is_none() || !self.running() {
            return Ok(());
        }
        end_except(self, keep)?;
        if !keep.is_empty() {
            return Ok(());
        }
        let control = self.control()?;
        let (reply, _answer) = sync_channel(1);
        let _ = control.send("kill-server", Pending::Reply(reply));
        control.close();
        if let Ok(mut slot) = self.control.lock() {
            *slot = None;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tmux/tests.rs"]
mod tests;
