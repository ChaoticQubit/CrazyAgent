use super::buffer::{Feed, buffer};
use super::{
    AgentId, CHUNK_BYTES_MAX, Host, HostError, INPUT_BYTES_MAX, PaneRef, SIZE_MAX, SIZE_MIN,
    TerminalStream, VIEWERS_MAX, failure,
};
use crate::harnesses::interactive::InteractiveCommand;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const START_COLS: u16 = 80;
const START_ROWS: u16 = 24;
const SCROLLBACK_ROWS: usize = 0;
const _: () = assert!(SCROLLBACK_ROWS <= super::SCROLLBACK_LINES as usize);
const ARGV_MAX: usize = 256;
const ENV_MAX: usize = 256;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BASE_ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// The Windows terminal host: one ConPTY pseudo console per agent, kept inside the engine
/// process because Windows has no tmux. A terminal parser per agent keeps the current
/// screen, so a viewer that attaches gets the screen and then every later output chunk with
/// nothing lost or repeated. Terminals end with the engine, unlike tmux on macOS and Linux.
#[derive(Default)]
pub struct ConptyHost {
    panes: Mutex<BTreeMap<AgentId, Pane>>,
    next_pane: AtomicU64,
}

struct Pane {
    pane: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    view: Arc<Mutex<View>>,
}

struct View {
    parser: vt100::Parser,
    cols: u16,
    rows: u16,
    viewers: Vec<Feed>,
}

impl Pane {
    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn end(&mut self) {
        if self.running()
            && let Some(pid) = self.child.process_id()
        {
            end_tree(pid);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ConptyHost {
    /// An empty host with no terminals yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The host's own reference for one agent terminal, such as `conpty-3`, or none.
    pub fn pane(&self, agent: &AgentId) -> Option<String> {
        lock(&self.panes).get(agent).map(|pane| pane.pane.clone())
    }
}

impl Drop for ConptyHost {
    fn drop(&mut self) {
        let panes = self.panes.get_mut().unwrap_or_else(PoisonError::into_inner);
        for pane in panes.values_mut() {
            pane.end();
        }
    }
}

impl Host for ConptyHost {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        let (program, args) = command
            .argv
            .split_first()
            .ok_or_else(|| failure("terminal_command_invalid"))?;
        if command.argv.len() > ARGV_MAX
            || command.env.len() > ENV_MAX
            || !Path::new(program).is_absolute()
            || !command.cwd.is_absolute()
            || command.env.iter().any(|(name, value)| {
                !crate::sandbox::agent_variable_allowed(OsStr::new(name), OsStr::new(value))
            })
        {
            return Err(failure("terminal_command_invalid"));
        }
        let mut panes = lock(&self.panes);
        if panes.get_mut(agent).is_some_and(|pane| pane.running()) {
            return Err(failure("terminal_agent_running"));
        }
        let pair = portable_pty::native_pty_system()
            .openpty(size(START_COLS, START_ROWS))
            .map_err(|_| unavailable())?;
        let mut builder = CommandBuilder::new(program);
        builder.args(args);
        builder.cwd(&command.cwd);
        builder.env_clear();
        for name in BASE_ENV {
            if let Some(value) = std::env::var_os(name) {
                builder.env(name, value);
            }
        }
        for (key, value) in &command.env {
            builder.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|_| failure("terminal_spawn_failed"))?;
        drop(pair.slave);
        let streams = child.process_id().and_then(|pid| {
            let reader = pair.master.try_clone_reader().ok()?;
            let writer = pair.master.take_writer().ok()?;
            Some((pid, reader, writer))
        });
        let Some((pid, reader, writer)) = streams else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        };
        let view = Arc::new(Mutex::new(View {
            parser: vt100::Parser::new(START_ROWS, START_COLS, SCROLLBACK_ROWS),
            cols: START_COLS,
            rows: START_ROWS,
            viewers: Vec::new(),
        }));
        let feed = Arc::clone(&view);
        let pumped = std::thread::Builder::new()
            .name(format!("conpty-{agent}"))
            .spawn(move || pump(reader, &feed));
        if pumped.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
        let pane = format!("conpty-{}", self.next_pane.fetch_add(1, Ordering::Relaxed));
        let replaced = panes.insert(
            agent.clone(),
            Pane {
                pane: pane.clone(),
                master: pair.master,
                writer,
                child,
                view,
            },
        );
        if let Some(mut ended) = replaced {
            ended.end();
        }
        assert!(panes.contains_key(agent));
        Ok(PaneRef {
            agent: agent.clone(),
            pane,
            pid,
        })
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        let mut panes = lock(&self.panes);
        let pane = live(&mut panes, agent)?;
        let (sender, chunks) = buffer();
        let mut view = lock(&pane.view);
        view.viewers.retain(Feed::open);
        if view.viewers.len() >= VIEWERS_MAX {
            return Err(failure("terminal_viewers_full"));
        }
        let screen = view.parser.screen().state_formatted();
        view.viewers.push(sender);
        assert!(view.viewers.len() <= VIEWERS_MAX);
        Ok(TerminalStream {
            agent: agent.clone(),
            cols: view.cols,
            rows: view.rows,
            screen,
            chunks,
        })
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_too_long"));
        }
        let mut panes = lock(&self.panes);
        let pane = live(&mut panes, agent)?;
        pane.writer
            .write_all(bytes)
            .and_then(|()| pane.writer.flush())
            .map_err(|_| unavailable())
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        let sizes = SIZE_MIN..=SIZE_MAX;
        if !sizes.contains(&cols) || !sizes.contains(&rows) {
            return Err(failure("terminal_size_invalid"));
        }
        let mut panes = lock(&self.panes);
        let pane = live(&mut panes, agent)?;
        pane.master
            .resize(size(cols, rows))
            .map_err(|_| unavailable())?;
        let mut view = lock(&pane.view);
        view.parser.set_size(rows, cols);
        view.cols = cols;
        view.rows = rows;
        Ok(())
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let mut pane = lock(&self.panes)
            .remove(agent)
            .ok_or_else(|| failure("terminal_agent_unknown"))?;
        pane.end();
        assert!(!pane.running());
        Ok(())
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        let mut panes = lock(&self.panes);
        panes.retain(|_, pane| pane.running());
        Ok(panes.keys().cloned().collect())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn live<'a>(
    panes: &'a mut BTreeMap<AgentId, Pane>,
    agent: &AgentId,
) -> Result<&'a mut Pane, HostError> {
    let pane = panes
        .get_mut(agent)
        .ok_or_else(|| failure("terminal_agent_unknown"))?;
    if !pane.running() {
        return Err(failure("terminal_agent_unknown"));
    }
    Ok(pane)
}

fn unavailable() -> HostError {
    failure("terminal_unavailable")
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn pump(mut reader: Box<dyn Read + Send>, view: &Mutex<View>) {
    let mut buffer = vec![0u8; CHUNK_BYTES_MAX];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        assert!(read <= CHUNK_BYTES_MAX);
        let chunk = &buffer[..read];
        let mut view = lock(view);
        view.parser.process(chunk);
        view.viewers.retain(|viewer| viewer.push(chunk));
    }
    lock(view).viewers.clear();
}

fn end_tree(pid: u32) {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let _ = crate::spawn_gate::status(
        Command::new(root.join("System32").join("taskkill.exe"))
            .arg("/PID")
            .arg(pid.to_string())
            .args(["/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW),
    );
}
