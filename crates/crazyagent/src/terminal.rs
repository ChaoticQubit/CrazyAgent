use crate::engine::EngineError;
use kernel_processes::terminal::{
    APP_PANE_UNKNOWN, AgentId, CHUNK_BYTES_MAX, CONNECTIONS_MAX, Host, HostError, INPUT_BYTES_MAX,
    OWN_SERVER_UNAVAILABLE, PANES_LISTED_MAX, PASTE_BYTES_MAX, PASTE_SETTLE, PINS_UNSUPPORTED,
    PROMPT_BYTES_MAX, PaneAt, PaneCommand, PaneInfo, PaneRef, RENEW_EVERY, RUN_CUT_BACK_BYTES_MAX,
    SCROLLBACK_LINES, SIZE_MAX, SIZE_MIN, TEAMMATES_SERVER_ENDED, TEAMMATES_SERVER_REPLACED,
    TerminalStream, VIEWER_BUFFER_BYTES_MAX, VIEWERS_MAX, buffer, end_except, input_runs,
    input_runs_max, pane_id_valid,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

mod kept;
pub(crate) use kept::KEPT_MAX;
#[cfg(unix)]
pub mod tmux;

pub const FOLDER: &str = "terminal";
pub const HARNESS_SOCKET: &str = "tmux.sock";
/// The start of a native lead's teammates' socket name: each lead start names a new one,
/// `teammates-` and 16 hex digits then `.sock`, beside the lead's own.
pub const TEAMMATES_SOCKET_PREFIX: &str = "teammates-";
#[cfg(unix)]
const TEAMMATES_SOCKET_SUFFIX: &str = ".sock";
#[cfg(unix)]
const TEAMMATES_NONCE_BYTES: usize = 8;
#[cfg(unix)]
const TEAMMATES_NAME_BYTES: usize =
    TEAMMATES_SOCKET_PREFIX.len() + 2 * TEAMMATES_NONCE_BYTES + TEAMMATES_SOCKET_SUFFIX.len();
/// The most bytes a teammates' server pid record holds: the socket's name, a space and the
/// ten digits of the largest pid.
#[cfg(unix)]
const TEAMMATES_PIN_BYTES_MAX: usize = TEAMMATES_NAME_BYTES + 1 + 10;
/// What a teammates' server pid record holds after the socket's name until the server first
/// answers.
#[cfg(unix)]
const TEAMMATES_PIN_PENDING: &str = "pending";
/// The most digits a lead's servers' deadline record holds, as a server keeper
/// reads it: every time a system clock gives fits (FBTMUX-P1-R1-L1).
#[cfg(unix)]
const SERVERS_DEADLINE_DIGITS_MAX: usize = 19;
const HARNESS_SOCKET_BYTES_MAX: usize = 104;
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

fn failure(code: &str) -> HostError {
    HostError(code.into())
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
    let engine = tmux::Tmux::open(program.clone(), &engine_root.join(FOLDER));
    Arc::new(kept::LastScreenHost::new(tmux::Servers::new(
        engine,
        engine_root,
        program,
    )))
}

/// The socket of a native-mode lead's own tmux server: `tmux.sock` in the lead's short private
/// temp folder, which only that lead's sandbox may bind or reach, never the engine's server.
/// The engine reaches it only through a tmux client started inside the lead's own profile,
/// since a tmux server can make any client run a command; never connect a plain client.
/// Takes the engine root and the lead. Returns the path, or `terminal_agent_invalid` for a
/// name that is not a team agent.
pub fn harness_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    lead_temp_socket(engine_root, lead, HARNESS_SOCKET)
}

/// The socket of a native lead's teammates' own tmux server (rv-audit S1 F1), in the lead's
/// short private temp folder under the name its latest start chose (see
/// [`renew_teammates_socket`]). That server and every teammate pane it forks run in the
/// teammates' profile, which serves this socket but never reaches or unlinks it, nor the
/// lead's own; the lead and the engine's clients reach it inside the lead's profile. Takes
/// the engine root and the lead. Returns the path, `terminal_agent_invalid` for a name that
/// is not a team agent, or `terminal_own_server_unavailable` when no plain name is kept.
#[cfg(unix)]
pub fn teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    use std::os::unix::fs::MetadataExt;
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let kept = crate::team::runtime::agent_teammates_socket_name(&engine_root.join("team"), id);
    let unavailable = || failure(OWN_SERVER_UNAVAILABLE);
    let plain = std::fs::symlink_metadata(&kept).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.uid() == rustix::process::getuid().as_raw()
            && u64::try_from(TEAMMATES_NAME_BYTES).is_ok_and(|bytes| metadata.len() == bytes)
    });
    if !plain {
        return Err(unavailable());
    }
    let name = std::fs::read_to_string(&kept).map_err(|_| unavailable())?;
    if !teammates_name_valid(&name) {
        return Err(unavailable());
    }
    lead_temp_socket(engine_root, lead, &name)
}

/// Names a new socket for a native lead's teammates' server as the lead starts and keeps
/// the name in the engine's own profiles folder, so no path a teammate's profile ever
/// served is reused: a teammate still running from an earlier start cannot bind a listener
/// where the lead's tmux clients now connect. The pin record is marked pending for the new
/// name in the same step, so one that later goes missing or names another socket is refused
/// (PLCC-L1). Takes the engine root and the lead, whose profiles folder must exist. Returns
/// the new path, or a plain code.
#[cfg(unix)]
pub fn renew_teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let mut nonce = [0_u8; TEAMMATES_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| failure(OWN_SERVER_UNAVAILABLE))?;
    let digits: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = format!("{TEAMMATES_SOCKET_PREFIX}{digits}{TEAMMATES_SOCKET_SUFFIX}");
    assert!(teammates_name_valid(&name));
    keep_record(
        &crate::team::runtime::agent_teammates_socket_name(&engine_root.join("team"), id),
        &name,
    )?;
    let pending = format!("{name} {TEAMMATES_PIN_PENDING}");
    assert!(pending.len() <= TEAMMATES_PIN_BYTES_MAX);
    keep_record(
        &crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id),
        &pending,
    )?;
    lead_temp_socket(engine_root, lead, &name)
}

/// Writes one engine record in the profiles folder no agent writes, in place of any earlier
/// one: a staged file only this user reads, under a random name of its own so no other
/// writer's staged file is met or removed (RV-DS241-22), renamed over the record so no
/// reader sees part of it. Takes the record path and its text. Returns Ok, or
/// `terminal_own_server_unavailable`.
#[cfg(unix)]
fn keep_record(kept: &Path, text: &str) -> Result<(), HostError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    const STAGING_NONCE_BYTES: usize = 16;
    let mut nonce = [0_u8; STAGING_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| failure(OWN_SERVER_UNAVAILABLE))?;
    let digits: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(digits.len(), 2 * STAGING_NONCE_BYTES);
    let staging = kept.with_extension(format!("{digits}.tmp"));
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&staging)
    else {
        return Err(failure(OWN_SERVER_UNAVAILABLE));
    };
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&staging, kept));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
        return Err(failure(OWN_SERVER_UNAVAILABLE));
    }
    Ok(())
}

/// The pid a native lead's teammates' server at one socket was pinned to (PLC c, PC-L1),
/// read without following a link from the record beside the socket's name, which outlives
/// the engine and a kill of the lead. Takes the engine root, the lead and the socket.
/// Returns the pid, none while the record is pending for this socket, or
/// `terminal_own_server_unavailable` for a record that is missing, names another socket or
/// is not this user's plain file as [`renew_teammates_socket`] and
/// [`keep_teammates_server_pid`] write one, so such a record refuses every server (PLCC-L1).
#[cfg(unix)]
fn teammates_server_pid(
    engine_root: &Path,
    lead: &AgentId,
    socket: &Path,
) -> Result<Option<u32>, HostError> {
    use std::os::unix::fs::MetadataExt;
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let kept = crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id);
    let unavailable = || failure(OWN_SERVER_UNAVAILABLE);
    let metadata = std::fs::symlink_metadata(&kept).map_err(|_| unavailable())?;
    let plain = metadata.is_file()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && u64::try_from(TEAMMATES_PIN_BYTES_MAX).is_ok_and(|bytes| metadata.len() <= bytes);
    if !plain {
        return Err(unavailable());
    }
    let text = std::fs::read_to_string(&kept).map_err(|_| unavailable())?;
    let (name, value) = text
        .split_once(' ')
        .filter(|(name, value)| {
            teammates_name_valid(name)
                && (*value == TEAMMATES_PIN_PENDING
                    || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())))
        })
        .ok_or_else(unavailable)?;
    if socket.file_name().and_then(|kept| kept.to_str()) != Some(name) {
        return Err(unavailable());
    }
    if value == TEAMMATES_PIN_PENDING {
        return Ok(None);
    }
    let pid = value
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(unavailable)?;
    Ok(Some(pid))
}

/// Records the pid a native lead's teammates' server at one socket first answered with
/// (PLC c, PC-L1), beside the socket's name in the engine's profiles folder, in place of
/// the pending mark its start left. Takes the engine root, the lead, the socket and the
/// pid. Returns Ok, or `terminal_own_server_unavailable`.
#[cfg(unix)]
fn keep_teammates_server_pid(
    engine_root: &Path,
    lead: &AgentId,
    socket: &Path,
    pid: u32,
) -> Result<(), HostError> {
    assert!(pid > 0);
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let name = socket
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| teammates_name_valid(name))
        .ok_or_else(|| failure(OWN_SERVER_UNAVAILABLE))?;
    let text = format!("{name} {pid}");
    assert!(text.len() <= TEAMMATES_PIN_BYTES_MAX);
    keep_record(
        &crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id),
        &text,
    )
}

/// Moves a native lead's servers' deadline (FBTMUX-P1-R1-L1): keeps it, in whole seconds
/// since the epoch, in the record beside the lead's profiles that no agent writes and that
/// each of its servers' keepers reads outside the sandboxes. Takes the engine root, the lead
/// and the deadline. Returns Ok, `terminal_unavailable` for a time before the epoch, or
/// `terminal_own_server_unavailable` when the record cannot be kept.
#[cfg(unix)]
fn keep_servers_deadline(
    engine_root: &Path,
    lead: &AgentId,
    until: SystemTime,
) -> Result<(), HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let seconds = until
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| failure("terminal_unavailable"))?
        .as_secs();
    let text = seconds.to_string();
    assert!(!text.is_empty() && text.len() <= SERVERS_DEADLINE_DIGITS_MAX);
    keep_record(
        &crate::team::runtime::agent_servers_deadline(&engine_root.join("team"), id),
        &text,
    )
}

/// Whether a text is a teammates' socket name as [`renew_teammates_socket`] makes one.
/// Takes the text, read from a file. Returns the answer.
#[cfg(unix)]
fn teammates_name_valid(name: &str) -> bool {
    name.len() == TEAMMATES_NAME_BYTES
        && name
            .strip_prefix(TEAMMATES_SOCKET_PREFIX)
            .and_then(|rest| rest.strip_suffix(TEAMMATES_SOCKET_SUFFIX))
            .is_some_and(|digits| {
                digits.len() == 2 * TEAMMATES_NONCE_BYTES
                    && digits
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
}

/// One named socket in a team agent's short private temp folder. Takes the engine root, the
/// agent and the socket name. Returns the path, or `terminal_agent_invalid` for a name that
/// is not a team agent.
fn lead_temp_socket(engine_root: &Path, lead: &AgentId, name: &str) -> Result<PathBuf, HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let home = crate::team::runtime::agent_home(&engine_root.join("team"), id);
    let socket = kernel_sandbox::toolchains::tmp_path(&home).join(name);
    assert!(socket.is_absolute() && socket.as_os_str().len() <= HARNESS_SOCKET_BYTES_MAX);
    Ok(socket)
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
    let engine = tmux::Tmux::open(program.clone(), &engine_root.join(FOLDER))
        .with_deadline_checks(loops, late);
    Arc::new(kept::LastScreenHost::new(
        tmux::Servers::new(engine, engine_root, program).with_deadline_checks(loops, late),
    ))
}

/// Opens this platform's terminal host for an engine: the ConPTY host on Windows, keeping the
/// last screen of each terminal it ends.
/// Takes the engine root, unused because ConPTY needs no server or socket.
/// Returns that the host can run, and the host.
#[cfg(target_os = "windows")]
pub fn platform(_engine_root: &Path) -> (bool, Arc<dyn Host>) {
    let host: Arc<dyn Host> = Arc::new(kept::LastScreenHost::new(
        kernel_processes::terminal::windows::ConptyHost::new(
            crate::sandbox::agent_variable_allowed,
        ),
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

/// The keys that type one owner prompt into an agent's app, before its Enter. Control
/// characters other than newline and tab are dropped, so the prompt cannot press keys or end
/// a paste. With bracketed paste on, the text goes as one paste with carriage returns between
/// lines, as a terminal pastes; with it off every line break would be an Enter, so lines join
/// with spaces. Takes the text and whether the app turned bracketed paste on.
/// Returns the keys, or none for blank text or text over `PROMPT_BYTES_MAX` bytes.
pub fn prompt_keys(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.len() > PROMPT_BYTES_MAX {
        return None;
    }
    let clean: String = text
        .replace("\r\n", "\n")
        .chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    let keys = if bracketed {
        format!("\x1b[200~{}\x1b[201~", clean.replace('\n', "\r"))
    } else {
        clean
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert!(!keys.contains('\n'));
    Some(keys.into_bytes())
}

/// The keys one owner paste types, filtered here at the trust boundary as the desktop's
/// `pasteText` filters it: every control character (C0, DEL and C1) is dropped except tab,
/// line feed and carriage return, then CR LF and LF become CR, as a terminal sends Enter, so
/// a paste can never press Escape, end a bracketed paste or send a Ctrl key. Takes the
/// pasted text. Returns the keys, empty when nothing is left.
pub fn paste_keys(text: &str) -> Vec<u8> {
    let kept: String = text
        .chars()
        .filter(|character| matches!(character, '\t' | '\n' | '\r') || !character.is_control())
        .collect();
    let keys = kept.replace("\r\n", "\r").replace('\n', "\r");
    assert!(!keys.contains('\x1b') && !keys.contains('\n') && !keys.contains('\0'));
    assert!(keys.len() <= text.len());
    keys.into_bytes()
}

/// Types one prompt into an agent's app as one submission: a paste when the app turned
/// bracketed paste on, then Enter once the paste settled, all into the app's own pane, found
/// once (V243-PASTE-INTERLEAVE-N0), or by name on a host with no pane ids. Takes the host, the
/// terminal name and the text. Returns whether it was typed; blank or oversized text types
/// nothing.
pub fn type_prompt(host: &dyn Host, agent: &AgentId, text: &str) -> Result<bool, HostError> {
    if let Some(at) = request_pin(host, agent)? {
        return prompt_pinned(host, agent, &at, text, APP_PANE_UNKNOWN);
    }
    let bracketed = host.input_live(agent)?;
    prompt_into(text, bracketed, |keys| host.input(agent, keys))
}

/// Types one prompt into a view's pinned pane, as `type_prompt` does, never by name. Takes the
/// host, the terminal name, the pane and the text. Returns as `type_prompt`, or
/// `terminal_agent_unknown` once that pane is no longer one the host can type into, which ends
/// the view so it opens again with a fresh pin, as its other pinned sends do.
#[cfg(unix)]
pub(crate) fn type_prompt_at(
    host: &dyn Host,
    agent: &AgentId,
    at: &PaneAt,
    text: &str,
) -> Result<bool, HostError> {
    prompt_pinned(host, agent, at, text, "terminal_agent_unknown")
}

/// Types one prompt's keys and Enter into a pinned pane, the paste mode read from that pane.
/// Takes the host, the name, the pin, the text and the code a stale pin answers. Returns as
/// `type_prompt`, or that code once the pin is stale.
fn prompt_pinned(
    host: &dyn Host,
    agent: &AgentId,
    at: &PaneAt,
    text: &str,
    stale: &'static str,
) -> Result<bool, HostError> {
    assert!(pane_id_valid(&at.pane), "a pin is a pane id");
    assert!(
        at.serial > 0,
        "a pin names the control connection that found it"
    );
    let Some(bracketed) = host.input_live_at(agent, at)? else {
        return Err(failure(stale));
    };
    prompt_into(text, bracketed, |keys| {
        type_piece(host, agent, Some(at), keys, stale)
    })
}

/// The pane one engine prompt goes to, found once so every piece of it lands in one pane
/// (V243-PASTE-INTERLEAVE-N0): the agent's app pane, or none only on a host that answers
/// `PINS_UNSUPPORTED`, which types by name. Takes the host and the name. Returns the pin,
/// none, or the host's error, such as `APP_PANE_UNKNOWN` or a failed connect.
pub(crate) fn request_pin(host: &dyn Host, agent: &AgentId) -> Result<Option<PaneAt>, HostError> {
    match host.app_pane_at(agent) {
        Ok(at) => Ok(Some(at)),
        Err(problem) if problem.0 == PINS_UNSUPPORTED => Ok(None),
        Err(problem) => Err(problem),
    }
}

/// Types one piece of a request into the pane the request was pinned to, or by name with no
/// pin. Takes the host, the name, the pin, at most `INPUT_BYTES_MAX` bytes and the code a
/// stale pin answers: `APP_PANE_UNKNOWN` for a request's pin, `terminal_agent_unknown` for a
/// view's. Returns once typed, or that code, never typing by name instead.
pub(crate) fn type_piece(
    host: &dyn Host,
    agent: &AgentId,
    pin: Option<&PaneAt>,
    bytes: &[u8],
    stale: &'static str,
) -> Result<(), HostError> {
    assert!(
        bytes.len() <= INPUT_BYTES_MAX,
        "a piece fits one input request"
    );
    assert!(
        matches!(stale, APP_PANE_UNKNOWN | "terminal_agent_unknown"),
        "a stale pin refuses the request or ends the view"
    );
    let Some(at) = pin else {
        return host.input(agent, bytes);
    };
    assert!(pane_id_valid(&at.pane), "a pin is a pane id");
    match host.input_at(agent, at, bytes)? {
        true => Ok(()),
        false => Err(failure(stale)),
    }
}

/// Types one prompt's keys as one submission through the given typing: the keys in chunks of
/// at most `INPUT_BYTES_MAX`, a settle when they went as a paste, then Enter. Takes the text,
/// whether the app turned bracketed paste on and the typing. Returns whether it was typed;
/// blank or oversized text types nothing.
fn prompt_into(
    text: &str,
    bracketed: bool,
    mut typing: impl FnMut(&[u8]) -> Result<(), HostError>,
) -> Result<bool, HostError> {
    let Some(keys) = prompt_keys(text, bracketed) else {
        return Ok(false);
    };
    for chunk in keys.chunks(INPUT_BYTES_MAX) {
        typing(chunk)?;
    }
    if bracketed {
        std::thread::sleep(PASTE_SETTLE);
    }
    typing(b"\r")?;
    Ok(true)
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
