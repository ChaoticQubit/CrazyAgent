use crate::engine::EngineError;
#[cfg(unix)]
use kernel_processes::terminal::RENEW_EVERY;
use kernel_processes::terminal::kept::LastScreenHost;
#[cfg(unix)]
use kernel_processes::terminal::tmux::{Servers, Tmux};
use kernel_processes::terminal::{
    APP_PANE_UNKNOWN, AgentId, CHUNK_BYTES_MAX, Host, HostError, PROMPT_BYTES_MAX, PaneAt,
    PaneCommand, PaneInfo, PaneRef, TerminalStream,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

#[cfg(unix)]
pub mod tmux;

pub const FOLDER: &str = "terminal";
pub const PANES_PER_PROJECT_MAX: usize = crate::config::POOL_MEMBERS_MAX;

const _: () = assert!(PROMPT_BYTES_MAX == crate::team::DIRECT_INPUT_BYTES_MAX);
const _: () = assert!(CHUNK_BYTES_MAX * 4 + 256 < crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(PANES_PER_PROJECT_MAX == 15);

/// Converts into the engine's own error with the same code.
impl From<HostError> for EngineError {
    fn from(error: HostError) -> Self {
        EngineError(error.0)
    }
}

/// The local clock text a kept screen's label shows, the time its terminal ended. Output:
/// the text.
fn kept_clock() -> String {
    use crate::tui::clock::{clock, local_offset, now_ms};
    clock(now_ms(), local_offset())
}

/// A host that starts each agent through `Host::spawn_own_server`, in a tmux server of its
/// own inside the agent's sandbox; every other call goes to the host unchanged.
struct OwnServer(Arc<dyn Host>);

/// Wraps a host so the agents it spawns start as native-mode leads, each in its own tmux
/// server inside its sandbox (see `Host::spawn_own_server`). Takes the engine's host.
/// Returns the wrapped host; the engine routes every later call to the lead's own server.
pub fn own_server(host: Arc<dyn Host>) -> Arc<dyn Host> {
    Arc::new(OwnServer(host))
}

impl Host for OwnServer {
    fn spawn(&self, agent: &AgentId, command: &PaneCommand) -> Result<PaneRef, HostError> {
        self.0.spawn_own_server(agent, command)
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.0.attach(agent)
    }

    fn screen(&self, agent: &AgentId) -> Result<Vec<u8>, HostError> {
        self.0.screen(agent)
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.0.snapshot(agent)
    }

    fn keep_last(&self, agent: &AgentId) {
        self.0.keep_last(agent)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.0.input(agent, bytes)
    }
    fn input_at(&self, agent: &AgentId, at: &PaneAt, bytes: &[u8]) -> Result<bool, HostError> {
        self.0.input_at(agent, at, bytes)
    }

    fn own_pane_at(&self, agent: &AgentId) -> Result<PaneAt, HostError> {
        self.0.own_pane_at(agent)
    }

    fn tag_app(&self, agent: &AgentId, pid: u32) -> Result<(), HostError> {
        self.0.tag_app(agent, pid)
    }

    fn app_pane_at(&self, agent: &AgentId) -> Result<PaneAt, HostError> {
        self.0.app_pane_at(agent)
    }

    fn paste_at(&self, agent: &AgentId, at: &PaneAt, keys: &[u8]) -> Result<bool, HostError> {
        self.0.paste_at(agent, at, keys)
    }

    fn resize_at(&self, agent: &AgentId, at: &PaneAt, size: (u16, u16)) -> Result<bool, HostError> {
        self.0.resize_at(agent, at, size)
    }

    fn unit(&self, agent: &AgentId) -> Result<String, HostError> {
        self.0.unit(agent)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.0.input_live(agent)
    }

    fn input_live_at(&self, agent: &AgentId, at: &PaneAt) -> Result<Option<bool>, HostError> {
        self.0.input_live_at(agent, at)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        self.0.quiet(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.0.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        self.0.kill(agent)
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        self.0.panes(agent)
    }

    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        self.0.pane_pid(agent)
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.0.unkept(agent)
    }

    fn end_kept_server(&self, agent: &AgentId) -> Option<bool> {
        self.0.end_kept_server(agent)
    }

    fn server_keepers(&self, agent: &AgentId) -> Vec<(u32, bool)> {
        self.0.server_keepers(agent)
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        self.0.kill_pane(agent, pane)
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.0.attach_pane(agent, pane)
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.0.snapshot_pane(agent, pane)
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.0.input_pane(agent, pane, bytes)
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        self.0.input_live_pane(agent, pane)
    }

    fn paste(&self, agent: &AgentId, keys: &[u8]) -> Result<(), HostError> {
        self.0.paste(agent, keys)
    }

    fn paste_pane(&self, agent: &AgentId, pane: &str, keys: &[u8]) -> Result<(), HostError> {
        self.0.paste_pane(agent, pane, keys)
    }

    fn adopt_pane(&self, agent: &AgentId, lead: &AgentId, pane: &str) -> Result<(), HostError> {
        self.0.adopt_pane(agent, lead, pane)
    }

    fn spawn_own_server(
        &self,
        agent: &AgentId,
        command: &PaneCommand,
    ) -> Result<PaneRef, HostError> {
        self.0.spawn_own_server(agent, command)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.0.alive()
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        self.0.close(keep)
    }

    fn renew(&self, until: SystemTime) -> Result<(), HostError> {
        self.0.renew(until)
    }
}

#[cfg(unix)]
/// The engine's renewal of its terminal servers' deadlines while it runs (FBTMUX-P1). Each
/// pass reads the owner's agents on quit setting again and sets every server's deadline that
/// far ahead, so the holder in a server whose engine never comes back, and for a native
/// lead's servers their keepers outside the sandboxes (FBTMUX-P1-R1-L1), end it once the time
/// is up, and an engine that comes back in time renews it before adopting its agents.
pub struct Renewal {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
/// Starts renewing the deadlines of a host's servers at once and every [`RENEW_EVERY`].
/// A pass whose setting cannot be read renews with the default setting and says why (see
/// [`renew_once`]). Takes the host and the engine root that holds the engine database.
/// Returns the running renewal, or None when its thread cannot start.
pub fn renew_while_running(host: Arc<dyn Host>, root: PathBuf) -> Option<Renewal> {
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = stop.clone();
    let thread = std::thread::Builder::new()
        .name("terminal-renewal".into())
        .spawn(move || {
            let (stopped, wake) = &*signal;
            loop {
                if let Err(problem) = renew_once(host.as_ref(), &root, SystemTime::now()) {
                    eprintln!(
                        "terminal_deadline_not_renewed cause={}",
                        crate::protocol::plain_code(&problem.0)
                    );
                }
                let guard = stopped
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let (guard, _) = wake
                    .wait_timeout_while(guard, RENEW_EVERY, |stop| !*stop)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *guard {
                    return;
                }
            }
        })
        .ok()?;
    Some(Renewal {
        stop,
        thread: Some(thread),
    })
}

#[cfg(unix)]
/// Renews the deadlines of a host's servers once from the owner's saved setting, or from the
/// default setting when the saved one cannot be read, logged, so a running engine never lets
/// its own servers reach a deadline it set earlier (FBTMUX-P1 r1 L2). Takes the host, the
/// engine root and the current time. Returns Ok, or the host's first failure.
pub fn renew_once(host: &dyn Host, root: &Path, now: SystemTime) -> Result<(), EngineError> {
    let settings = saved_quit_settings(root).unwrap_or_else(|problem| {
        eprintln!("terminal_deadline_from_default cause={}", problem.0);
        crate::quit_settings::QuitSettings::default()
    });
    host.renew(settings.deadline(now))
        .map_err(|problem| EngineError(problem.0))
}

#[cfg(unix)]
/// Reads the owner's agents on quit setting from the engine database. Takes the engine root.
/// Returns the setting, or the database failure or `quit_settings_invalid`.
fn saved_quit_settings(root: &Path) -> Result<crate::quit_settings::QuitSettings, EngineError> {
    let db = crate::landing::open(root)?;
    crate::quit_settings::initialize(&db)?;
    crate::quit_settings::load(&db)
}

#[cfg(unix)]
impl Renewal {
    /// Stops renewing and waits for the thread; the deadlines last set stay. Takes the
    /// renewal.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
impl Drop for Renewal {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Turns a terminal code into the sentence clients show, such as for a refused attach.
/// Takes the code from `HostError`, `TerminalFrame::Rejected` or `TerminalFrame::Closed`.
/// Returns plain words without internal terms.
pub fn plain_reason(code: &str) -> &'static str {
    match code {
        "terminal_unavailable" => "Agent terminals need tmux. Run setup to install it.",
        "terminal_tmux_unsupported" => {
            "The installed tmux is too old for agent terminals. Run setup to update it."
        }
        "terminal_agent_unknown" | "terminal_ended" => {
            "This agent has stopped, so its terminal is closed."
        }
        "terminal_agent_running" => "This agent already has a terminal.",
        "terminal_agent_invalid" => "That is not an agent terminal.",
        "terminal_pane_unknown" => "That teammate's terminal has already closed.",
        APP_PANE_UNKNOWN => {
            "This agent's own pane could not be found, so the message was not sent."
        }
        "terminal_project_full" => {
            "This project already runs 15 agents, the most one project may run at once."
        }
        "terminal_viewers_full" => {
            "This terminal is open in too many places. Close one of them first."
        }
        "terminal_input_bounds" | "terminal_input_too_long" => {
            "That is too much to type at once. Send it in smaller parts."
        }
        "terminal_size_bounds" | "terminal_size_invalid" => "That terminal size is out of range.",
        "terminal_reply_too_large" => {
            "This terminal holds too much to show at once. The agent keeps running."
        }
        "terminal_detached" => "You closed this view. The agent keeps running.",
        "terminal_viewer_gone" | "terminal_viewer_slow" => {
            "This view stopped reading, so it was closed. Open the terminal again."
        }
        "terminal_socket_path_long" | "terminal_folder_unsafe" => {
            "The engine's terminal folder cannot be used safely."
        }
        "terminal_not_answering" | "terminal_closed" | "terminal_attach_timeout" => {
            "The terminal host did not answer. Try again."
        }
        "owner_grant_stale_or_revoked" => {
            "This app's access to the engine changed. Open the terminal again."
        }
        _ => "The terminal could not be used.",
    }
}

/// Opens this platform's terminal host for an engine: the private tmux server on macOS and Linux,
/// keeping the last screen of each terminal it ends.
/// Takes the engine root; tmux keeps its socket in `<root>/terminal` and is looked for there first.
/// Returns whether the host can run now, and the host; one that cannot reports `terminal_unavailable`.
#[cfg(unix)]
pub fn platform(engine_root: &Path) -> (bool, Arc<dyn Host>) {
    let found = tmux::installed(engine_root);
    (found.is_some(), tmux_host(engine_root, found))
}

/// Opens the engine's terminal host on one tmux program, keeping the last screen of each
/// terminal it ends: the engine's private server, and each native-mode lead's own server
/// inside that lead's sandbox (see [`harness_socket`]). Takes the engine root and the
/// program, none when tmux is missing. Returns the host; nothing starts until its first call.
#[cfg(unix)]
pub fn tmux_host(engine_root: &Path, program: Option<PathBuf>) -> Arc<dyn Host> {
    let engine = Tmux::open(
        program.clone(),
        &engine_root.join(FOLDER),
        crate::sandbox::confinement(),
        tmux::tmux_hooks(),
    );
    Arc::new(LastScreenHost::new(
        Servers::new(engine, engine_root, program),
        kept_clock,
    ))
}

/// Opens the engine's terminal host as [`tmux_host`] does, with every server it starts
/// reading its deadline every `loops` holder loops and ending after `late` passed reads, so
/// a row sees the quit limit within seconds. Takes the engine root, the program, the spacing
/// and the count. Returns the host.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn tmux_host_checking(
    engine_root: &Path,
    program: Option<PathBuf>,
    loops: u32,
    late: u32,
) -> Arc<dyn Host> {
    let engine = Tmux::open(
        program.clone(),
        &engine_root.join(FOLDER),
        crate::sandbox::confinement(),
        tmux::tmux_hooks(),
    )
    .with_deadline_checks(loops, late);
    Arc::new(LastScreenHost::new(
        Servers::new(engine, engine_root, program).with_deadline_checks(loops, late),
        kept_clock,
    ))
}

/// Opens this platform's terminal host for an engine: the ConPTY host on Windows, keeping the
/// last screen of each terminal it ends.
/// Takes the engine root, unused because ConPTY needs no server or socket.
/// Returns that the host can run, and the host.
#[cfg(target_os = "windows")]
pub fn platform(_engine_root: &Path) -> (bool, Arc<dyn Host>) {
    let host: Arc<dyn Host> = Arc::new(LastScreenHost::new(
        kernel_processes::terminal::windows::ConptyHost::new(
            kernel_sandbox::profile::env::agent_variable_allowed,
        ),
        kept_clock,
    ));
    (true, host)
}

/// What a viewer sends on an owner terminal connection.
/// The first frame is `Attach`, or `AttachPane` for one pane of an agent's own window, such
/// as a native teammate's, or `AttachSized` for either one whose window is first sized to the
/// viewer, so the screen sent and all output after it fit a viewer that does not reflow;
/// `Input`, `Prompt`, `Resize` and `Detach` follow on the same
/// connection and go to that pane. `Input` bytes, at most `INPUT_BYTES_MAX`, are typed as given and never kept.
/// `Prompt` text, at most `PROMPT_BYTES_MAX`, is the owner's explicit message: it is typed as
/// one submission, and a sub-agent's master later hears a note of it. `Scroll` bytes, bound
/// as `Input`, are keys the viewer made from the owner's mouse wheel, such as the arrow keys a
/// full-screen app reads in place of a scroll; they are typed as `Input` is but never count
/// as the owner typing, which holds engine input to a live master back. A `Scroll` holding
/// anything but up and down arrow keys and mouse or focus reports counts as `Input` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalRequest {
    Attach {
        agent_id: AgentId,
    },
    AttachPane {
        agent_id: AgentId,
        pane: String,
    },
    AttachSized {
        agent_id: AgentId,
        pane: Option<String>,
        cols: u16,
        rows: u16,
    },
    /// Opens a view of the agent's terminal, or one pane of its window, sized to `cols` by
    /// `rows` and sending no output, for a viewer that draws the pane from elsewhere. With
    /// `follows`, it sizes the window only while no view without it is open; the key is
    /// left out when false, so the request reads as before to an engine without it.
    AttachInput {
        agent_id: AgentId,
        pane: Option<String>,
        cols: u16,
        rows: u16,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        follows: bool,
    },
    Input {
        bytes: Vec<u8>,
    },
    /// One chunk of 1 to `INPUT_BYTES_MAX` bytes of an owner paste of at most
    /// `PASTE_BYTES_MAX`, the last one marked; the engine filters the whole and pastes it once.
    Paste {
        bytes: Vec<u8>,
        last: bool,
    },
    Scroll {
        bytes: Vec<u8>,
    },
    Prompt {
        text: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Detach,
}

/// What the engine sends on an owner terminal connection.
/// `Attached` comes first with the size, then `Output` chunks: the screen, then live output.
/// `Closed` ends a stream that ran; `Rejected` ends one that never attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalFrame {
    Attached {
        agent_id: AgentId,
        cols: u16,
        rows: u16,
    },
    Output {
        bytes: Vec<u8>,
    },
    Closed {
        reason: String,
    },
    Rejected {
        code: String,
    },
}

#[cfg(test)]
#[path = "terminal/tests.rs"]
mod tests;
