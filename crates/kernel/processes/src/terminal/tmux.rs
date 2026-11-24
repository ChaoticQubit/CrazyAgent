use super::buffer::{Feed, buffer};
use super::{
    AgentId, Host, HostError, INPUT_BYTES_MAX, OWN_SERVER_UNAVAILABLE, PANES_LISTED_MAX,
    PASTE_BYTES_MAX, PaneAt, PaneInfo, PaneRef, SCROLLBACK_LINES, SIZE_MAX, SIZE_MIN,
    TEAMMATES_SERVER_REPLACED, TerminalStream, VIEWERS_MAX, end_except, failure, pane_id_valid,
};
use super::{LeadPaths, PaneCommand};
use crate::group::Ending;
use control::*;
#[cfg(all(test, target_os = "macos"))]
use groups::*;
use lines::*;
pub use servers::Servers;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SESSION: &str = "crazyagent";
pub const SOCKET: &str = "tmux.sock";
const HOLDER: &str = "_crazyagent";
/// How long a tmux may take to report its version, as setup allows.
const VERSION_READ_TIME: Duration = Duration::from_secs(10);
/// How many seconds one holder loop sleeps.
const HOLDER_LOOP_SECONDS: u32 = 2;
/// How many holder loops pass between two reads of the server's deadline: the modification
/// time of its socket, which the running engine keeps ahead (FBTMUX-P1). A server's keeper
/// reads the engine's deadline record as often (FBTMUX-P1-R1-L1).
const DEADLINE_CHECK_LOOPS: u32 = 30;
/// How many deadline reads in a row must find the deadline passed before the holder ends
/// its server, about five awake minutes, so an engine waking with the machine renews first.
const DEADLINE_LATE_CHECKS: u32 = 5;
/// The command that prints a file's modification time in seconds, following a socket alias.
#[cfg(target_os = "macos")]
const MODIFIED: &str = "stat -L -f %m";
#[cfg(not(target_os = "macos"))]
const MODIFIED: &str = "stat -L -c %Y";

/// The POSIX command of a server's holder session. It ends the server at once when its
/// socket is gone, and once a deadline was ever set ahead, after `late` reads in a row, one
/// every `loops` loops, find it passed; a server no engine renewed keeps only the socket
/// bound. Takes the read spacing and the count. Returns the command text.
fn holder_command(loops: u32, late: u32) -> String {
    assert!(loops >= 1 && late >= 1);
    format!(
        "s=${{TMUX%,*,*}}; i=0; a=0; l=0; while [ -S \"$s\" ]; do i=$((i+1)); if [ $i -ge {loops} ]; then i=0; d=$({MODIFIED} \"$s\" 2>/dev/null) && n=$(date +%s) && if [ \"$n\" -lt \"$d\" ]; then a=1; l=0; elif [ $a = 1 ]; then l=$((l+1)); fi; [ $l -ge {late} ] && break; fi; sleep {HOLDER_LOOP_SECONDS}; done; p=${{TMUX%,*}}; kill \"${{p##*,}}\""
    )
}

/// Moves one server's deadline (FBTMUX-P1): sets the modification time of its socket, which
/// its holder reads, never through a link and only on a socket of this user. Takes the
/// socket and the deadline. Returns Ok, also when no socket is there, or
/// `terminal_unavailable` when the time cannot be set.
fn renew_socket(socket: &Path, until: SystemTime) -> Result<(), HostError> {
    let Ok(metadata) = std::fs::symlink_metadata(socket) else {
        return Ok(());
    };
    if !metadata.file_type().is_socket() || metadata.uid() != user() {
        return Ok(());
    }
    let seconds = until
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .ok_or_else(|| failure("terminal_unavailable"))?;
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: 0,
            tv_nsec: rustix::fs::UTIME_OMIT,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
    };
    rustix::fs::utimensat(
        rustix::fs::CWD,
        socket,
        &times,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|_| failure("terminal_unavailable"))
}
/// The program that runs the POSIX holder command. Given to tmux as separate arguments, it
/// is started directly, not through `default-shell`, which is the owner's login shell and
/// may be fish, csh or nushell, none of which can read the holder.
const HOLDER_SHELL: [&str; 2] = ["/bin/sh", "-c"];
const SERVER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A native lead's teammates' server pin (PLC c): the pid that server first answered with at
/// one socket, none until read or kept, shared by every engine client on that socket and kept
/// in a record beside the socket's name, so it outlives the engine and a kill of the lead
/// (PC-L1).
struct ServerPin {
    root: PathBuf,
    lead: AgentId,
    socket: PathBuf,
    pid: Mutex<Option<u32>>,
    hooks: TmuxHooks,
}
const ENV_PROGRAM: &str = "/usr/bin/env -i";
/// The program a native-mode lead's own server starts the lead with: it runs the command in
/// the environment the server hands every pane, the lead's own variables plus the tmux
/// variables a teammate split needs, and keeps the command's first word out of tmux's shell.
const PANE_PROGRAM: &str = "/usr/bin/env";
/// The shell a native-mode lead's own server runs the commands its app splits off with,
/// instead of the owner's login shell, which may not read POSIX commands.
const LEAD_SHELL: &str = "/bin/sh";
/// The pane option naming the agent whose own app a pane of a lead's server runs, set by the
/// engine at spawn, since the lead's window also holds the panes its teammates split off.
const AGENT_TAG: &str = "@crazyagent";
/// The window option marking a window whose app pane the engine tagged, set with the tag, so
/// a window that lost its app pane never hands its one teammate pane to the app's prompts.
const APP_MARK: &str = "@crazyagent-app";
const TERMINAL_TYPE: &str = "xterm-256color";
const TERMINAL_DEFAULTS: [(&str, &str); 2] = [("TERM", TERMINAL_TYPE), ("COLORTERM", "truecolor")];
/// The lowest tmux version, major and minor, the host takes.
pub const VERSION_MIN: (u32, u32) = (3, 7);
const CAPTURE_WRITTEN: &str = "-p -e -N -T";
/// The most bytes one `send-keys -H` command types. tmux parses a command's arguments in time
/// that grows faster than their count and answers nothing else meanwhile: 4096 hex keys held
/// the server about 74 ms, 256 under 1 ms (fix11 NOTE-1032).
const SEND_KEYS_BYTES_MAX: usize = 256;
const REPLY_TIME: Duration = Duration::from_secs(10);
/// Ends every format the engine has tmux expand. tmux stops expanding a format that has run
/// for 100 ms of wall time, which a server stalled on a loaded machine reaches, and answers
/// with the text so far as a success; only a line ending with this mark was expanded whole.
const FORMAT_END: &str = " end";
const REPLY_CUT: &str = "terminal_reply_cut";
/// The most lines the reader drops after a cut attach capture while it looks for that
/// attach's marker line: every history and screen row a capture can hold, then the
/// capture's closing guard, the marker block's opening guard and the marker itself.
const DRAIN_LINES_MAX: usize = HISTORY_ROWS_MAX + u16::MAX as usize + 3;
/// How many random bytes an attach's marker holds, written as twice as many hex digits.
const MARKER_BYTES: usize = 16;
/// The most live views one tmux server feeds at once over all its panes: every view the
/// engine serves, so one agent per tile never meets it first; `VIEWERS_MAX` bounds one pane.
const SERVER_VIEWERS_MAX: usize = super::CONNECTIONS_MAX;
const PLACED: &str = "#{pane_id} #{pane_pid}";
const PANE_FIELDS: &str = "#{pane_id} #{pane_pid} #{pane_dead}";
const CLOSE_TIME: Duration = Duration::from_secs(2);
/// How long the processes of closed panes get to end on the hangup tmux sent them, as long
/// as the engine gives a headless agent after its stop signal.
const HANGUP_GRACE: Duration = Duration::from_secs(2);
/// How long a stop goes on killing what the closed panes left in their groups.
const GROUP_KILL_WAIT: Duration = Duration::from_secs(2);
/// The most reaper threads one host keeps; a stop past it first waits for the oldest.
const REAPERS_MAX: usize = 64;
const LINE_BYTES_MAX: usize = 1024 * 1024;
const REPLY_BYTES_MAX: usize = 32 * 1024 * 1024;
const COMMAND_BYTES_MAX: usize = 256 * 1024;
const ARGS_MAX: usize = 256;
const ENV_MAX: usize = 128;
const ENV_NAME_BYTES_MAX: usize = 128;

type Lines = Vec<Vec<u8>>;
type Answer = Result<Lines, HostError>;

/// How a confined pane is wrapped: the sandbox runner, the keeper's command words and the
/// names of its two variables, the pane shim's words and which variables reach the pane.
/// Built by crazyagent from its sandbox values until kernel/sandbox supplies it. Inputs: the
/// fields. Output: a value Tmux and its servers read.
#[derive(Debug, Clone, Copy)]
pub struct Confinement {
    pub sandbox_exec: &'static str,
    #[cfg(target_os = "macos")]
    pub keeper: [&'static str; 6],
    #[cfg(target_os = "macos")]
    pub keeper_deadline_variable: &'static str,
    #[cfg(target_os = "macos")]
    pub keeper_state_variable: &'static str,
    #[cfg(target_os = "macos")]
    pub pane_shim: [&'static str; 5],
    pub variable_allowed: fn(&OsStr, &OsStr) -> bool,
    #[cfg(target_os = "macos")]
    pub unkept_pane: fn(&[String]) -> Option<Vec<String>>,
}

/// The team answers the tmux host needs without naming the team code: which process is at
/// the other end of a socket, the machine's parent map, where an agent's profiles and server
/// state live, and the sockets and records of a native lead's servers. Built once by
/// crazyagent. Inputs: the fields. Output: a value Tmux and its servers read.
#[derive(Clone, Copy)]
pub struct TmuxHooks {
    pub peer_pid: fn(&std::os::unix::net::UnixStream) -> Option<u32>,
    pub process_parents: fn() -> Option<std::collections::HashMap<u32, u32>>,
    pub agent_profile: fn(&Path, i64) -> PathBuf,
    pub agent_teammates_profile: fn(&Path, i64) -> PathBuf,
    pub agent_servers_state: fn(&Path, i64, bool) -> PathBuf,
    pub lead: LeadPaths,
}

/// The engine's terminal host on macOS and Linux: one private tmux server driven in control mode.
/// The server's socket lives in the engine's own private folder and never the owner's tmux.
/// Agents keep running when the engine stops; a later host on the same folder finds them again.
/// The server ends itself, with its agents, once its socket is gone, such as when the engine
/// home is deleted, so no server outlives the folder that reaches it, and once the deadline
/// the running engine keeps setting ahead has passed, so no server outlives an engine that
/// never comes back by more than the owner's chosen time (FBTMUX-P1). A confined host drives a
/// native-mode lead's own server instead: the server and every client the engine starts on it
/// run inside the lead's Seatbelt profile, since a tmux server can make a client run commands.
/// A teammates host drives a native lead's teammates' server: the server starts inside the
/// teammates' profile, and the engine's clients connect inside the lead's.
pub struct Tmux {
    executable: Option<PathBuf>,
    folder: PathBuf,
    socket: PathBuf,
    confined: Option<PathBuf>,
    starts_in: Option<PathBuf>,
    pin: Option<Arc<ServerPin>>,
    control: Mutex<Option<Arc<Control>>>,
    spawning: Mutex<()>,
    reapers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    deadline_checks: (u32, u32),
    confinement: Confinement,
    hooks: TmuxHooks,
}

/// Finds the tmux program the engine uses in the given places, the answer setup shows: the
/// bundled copy when it runs, else the usual order ([`crate::finding::resolve_tmux`]).
/// Takes the places. Returns the real program, or none when tmux is not installed.
pub fn installed_at(places: &crate::finding::Places) -> Option<PathBuf> {
    crate::finding::resolve_tmux(places, crate::finding::SHIPS_BUNDLED_TMUX, runs)
        .ok()
        .map(|(tmux, _)| tmux.program)
}

/// A tmux file as (path, device, inode, size, change seconds and nanoseconds), so a file
/// replaced in place is read again.
type RanFile = (PathBuf, u64, u64, u64, i64, i64);

/// How many tmux files [`RAN`] keeps, one per path: the bundled one, with room for tests
/// that each read their own at the same time.
const RAN_MAX: usize = 4;

/// The tmux files that answered as running, a pass or a read that ran out of time, so
/// setup and every terminal start do not each wait on them again. A failed read is not
/// kept: a fix outside the file, such as a library installed, counts on the next ask.
static RAN: Mutex<Vec<RanFile>> = Mutex::new(Vec::new());

/// Whether a tmux runs: `-V` reports a version within the bound and that version is one the
/// terminal supports (3.7 or later, the first with the bracketed-paste flag the attach
/// reads). A read that runs out of time counts as running, since a new file can wait behind
/// the system's first-run checks; the version read setup does next reports it. Takes the
/// tmux. Returns Ok when it runs, else its version when that is a release too old, else none.
pub fn runs(tmux: &crate::finding::Resolved) -> Result<(), Option<String>> {
    runs_within(tmux, VERSION_READ_TIME)
}

/// Answers [`runs`] with the version read bounded by the given time, so a test can let a
/// read run out of time quickly. Takes the tmux and the bound. Returns the answer.
fn runs_within(tmux: &crate::finding::Resolved, time: Duration) -> Result<(), Option<String>> {
    assert!(!time.is_zero() && time <= VERSION_READ_TIME);
    let Ok(metadata) = std::fs::metadata(&tmux.program) else {
        return Err(None);
    };
    let file: RanFile = (
        tmux.program.clone(),
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    );
    let last = || {
        RAN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if last().contains(&file) {
        return Ok(());
    }
    let words = tmux.argv();
    let Some((program, rest)) = words.split_first() else {
        return Err(None);
    };
    let mut arguments = rest.to_vec();
    arguments.push("-V".into());
    let version = crate::run::run(
        Path::new(program),
        &arguments,
        &[],
        &std::env::temp_dir(),
        time,
    );
    let running = match version {
        Err(crate::run::InstallError(code)) if code == "setup_timeout" => Ok(()),
        Err(_) | Ok((false, _)) => Err(None),
        Ok((true, output)) => match crate::run::version_from(&output) {
            Ok(found) => match supported(found.as_str()) {
                Some(true) => Ok(()),
                Some(false) => Err(Some(found.as_str().to_owned())),
                None => Err(None),
            },
            Err(_) => Err(None),
        },
    };
    if running.is_ok() {
        let mut kept = last();
        kept.retain(|(path, ..)| *path != file.0);
        if kept.len() == RAN_MAX {
            kept.remove(0);
        }
        kept.push(file);
        assert!(kept.len() <= RAN_MAX);
    }
    running
}

impl Tmux {
    /// Opens the host on the engine's private tmux server; nothing starts until the first call.
    /// Takes the tmux program, none when tmux is missing, and the engine's private terminal folder.
    /// Returns the host; every call fails with `terminal_unavailable` while tmux is missing.
    pub fn open(
        executable: Option<PathBuf>,
        folder: &Path,
        confinement: Confinement,
        hooks: TmuxHooks,
    ) -> Self {
        Self {
            executable: executable.filter(|program| program.is_absolute()),
            folder: folder.to_path_buf(),
            socket: folder.join(SOCKET),
            confined: None,
            starts_in: None,
            pin: None,
            control: Mutex::new(None),
            spawning: Mutex::new(()),
            reapers: Mutex::new(Vec::new()),
            deadline_checks: (DEADLINE_CHECK_LOOPS, DEADLINE_LATE_CHECKS),
            confinement,
            hooks,
        }
    }

    /// Opens the host on a native-mode lead's own tmux server (see `super::harness_socket`); the
    /// server and each engine client on it start inside the lead's profile, and nothing starts
    /// until the first call. Takes the tmux program, the socket in the lead's private temp
    /// folder and the lead's installed profile. Returns the host, or `terminal_folder_unsafe`
    /// for a socket path with no folder.
    pub fn confined(
        executable: Option<PathBuf>,
        socket: &Path,
        profile: &Path,
        confinement: Confinement,
        hooks: TmuxHooks,
    ) -> Result<Self, HostError> {
        let folder = socket
            .parent()
            .filter(|_| socket.is_absolute() && profile.is_absolute())
            .ok_or_else(|| failure("terminal_folder_unsafe"))?;
        Ok(Self {
            executable: executable.filter(|program| program.is_absolute()),
            folder: folder.to_path_buf(),
            socket: socket.to_path_buf(),
            confined: Some(profile.to_path_buf()),
            starts_in: None,
            pin: None,
            control: Mutex::new(None),
            spawning: Mutex::new(()),
            reapers: Mutex::new(Vec::new()),
            deadline_checks: (DEADLINE_CHECK_LOOPS, DEADLINE_LATE_CHECKS),
            confinement,
            hooks,
        })
    }

    /// Opens the host on a native lead's teammates' own tmux server (see
    /// `super::teammates_socket`, rv-audit S1 F1): the server runs inside the teammates'
    /// profile, which cannot reach its socket, started under its keeper by `Servers`
    /// (FBTMUX-P1-R1-L1), and each engine client connects inside the lead's profile and never
    /// starts a server, so no server runs in the lead's profile with teammate panes. Each
    /// client it connects checks the server's pid against `pin`. Takes the tmux program, the
    /// socket, the lead's installed profile, the teammates' and the socket's pin. Returns the
    /// host, or `terminal_folder_unsafe` for a socket path with no folder.
    fn teammates(
        executable: Option<PathBuf>,
        socket: &Path,
        profile: &Path,
        server_profile: &Path,
        pin: Arc<ServerPin>,
        confinement: Confinement,
        hooks: TmuxHooks,
    ) -> Result<Self, HostError> {
        if !server_profile.is_absolute() || server_profile == profile {
            return Err(failure("terminal_folder_unsafe"));
        }
        Ok(Self {
            starts_in: Some(server_profile.to_path_buf()),
            pin: Some(pin),
            ..Self::confined(executable, socket, profile, confinement, hooks)?
        })
    }

    /// Has a server this host starts read its deadline every `loops` holder loops and end
    /// after `late` passed reads, so a row sees the bound within seconds. Takes the spacing
    /// and the count. Returns the host.
    #[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
    pub fn with_deadline_checks(mut self, loops: u32, late: u32) -> Self {
        assert!(loops >= 1 && late >= 1);
        self.deadline_checks = (loops, late);
        self
    }

    /// The command of the holder session a server this host starts runs.
    fn holder(&self) -> String {
        holder_command(self.deadline_checks.0, self.deadline_checks.1)
    }

    /// Gives the tmux server's socket. Takes the host.
    /// Returns `<folder>/tmux.sock`, or a lead's own socket for a confined host.
    pub fn socket(&self) -> PathBuf {
        self.socket.clone()
    }

    fn running(&self) -> bool {
        std::fs::symlink_metadata(self.socket())
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    }

    /// Waits until the private server stops listening after kill-server, then
    /// removes the socket file tmux leaves behind, so `running` reports no server
    /// and a later `alive` starts none. Output: Ok once it is gone, or
    /// `terminal_not_answering` when it still listens at the deadline.
    fn forget_server(&self) -> Result<(), HostError> {
        let socket = self.socket();
        let deadline = Instant::now() + REPLY_TIME;
        while crate::spawn_gate::connect_unix_std(&socket).is_ok() {
            if Instant::now() >= deadline {
                return Err(failure("terminal_not_answering"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !self.running() {
            return Ok(());
        }
        self.remove_socket()
    }

    /// Removes the socket file an ended server left behind. A teammates' socket stays: once
    /// it is gone a teammate still running could bind a server of its own there, which the
    /// lead's tmux clients would then obey (rv-audit S1 F1), and the lead's next start names
    /// a new one. Output: Ok once it is gone or kept, or `terminal_folder_unsafe` when it
    /// cannot be removed.
    fn remove_socket(&self) -> Result<(), HostError> {
        if self.starts_in.is_some() {
            return Ok(());
        }
        match std::fs::remove_file(self.socket()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure("terminal_folder_unsafe")),
        }
    }

    /// Probes the server's socket while holding the control slot, under which a spawn
    /// starts a server, so none binds the path between a refused probe and the removal.
    /// A refused probe means the server ended: its socket file is removed. Output: true
    /// while a server listens, false once the socket is gone or removed, or the error a
    /// connect gives for any other probe failure, such as running out of descriptors.
    fn listening(&self) -> Result<bool, HostError> {
        let _slot = self
            .control
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        match crate::spawn_gate::connect_unix_std(self.address()?) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                self.remove_socket()?;
                Ok(false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(failure("terminal_unavailable")),
        }
    }

    /// The process the kernel names as the listener bound at this host's socket, read from a
    /// raw connection, never a tmux client, so no reply of the server decides it. A link
    /// swapped in at the path reaches a listener bound under another name, so the name the
    /// kernel gives the peer must be the socket's own. tmux binds the whole address, so that
    /// name is padded with zero bytes after its end. Output: its pid, or none when nothing
    /// there answers or the listener is bound elsewhere.
    fn server_pid(&self) -> Option<u32> {
        let socket = self.address().ok()?;
        let stream = crate::spawn_gate::connect_unix_std(&socket).ok()?;
        let peer = stream.peer_addr().ok()?;
        let named = peer.as_pathname()?.as_os_str().as_bytes();
        if named.split(|byte| *byte == 0).next()? != socket.as_os_str().as_bytes() {
            return None;
        }
        (self.hooks.peer_pid)(&stream).filter(|pid| *pid > 1)
    }

    /// Keeps the panes whose first process the kernel reports as a child of the listener at
    /// this host's socket. tmux forks each pane's first process from its server, while a pane
    /// listing is only what the server says, and a native lead can swap in a listener of its
    /// own at that socket, so a stand-in can name no process outside the server bound at
    /// this address. Takes the panes. Returns those kept, none when the listener or the
    /// process table cannot be read.
    fn served(&self, panes: &[PaneInfo]) -> Vec<PaneInfo> {
        assert!(panes.len() <= PANES_LISTED_MAX);
        let (Some(server), Some(parents)) = (self.server_pid(), (self.hooks.process_parents)())
        else {
            return Vec::new();
        };
        panes
            .iter()
            .filter(|pane| parents.get(&pane.pid) == Some(&server))
            .cloned()
            .collect()
    }

    /// Gives the control connection the host already holds while it still reads.
    /// Output: that control, or none when there is none or it closed.
    fn open_control(&self) -> Option<Arc<Control>> {
        let slot = self.control.lock().ok()?;
        slot.as_ref().filter(|control| !control.closed()).cloned()
    }

    /// Lists the live agents when no open control holds the server, or one closed under
    /// the query, as after kill-server or a crash, so the server may be ended, still
    /// ending or alive. A server that refuses connections is forgotten and has none; a
    /// listening one is asked through a fresh control that cannot start a server, again
    /// every 10 ms while an ending server drops it, for at most `REPLY_TIME`. Output: the
    /// names in order, or the refusal still given at the deadline.
    fn alive_reconnecting(&self) -> Result<Vec<AgentId>, HostError> {
        let deadline = Instant::now() + REPLY_TIME;
        loop {
            if !self.running() || !self.listening()? {
                return Ok(Vec::new());
            }
            match self
                .control_with(false)
                .and_then(|control| windows(&control))
            {
                Err(error)
                    if matches!(error.0.as_str(), "terminal_closed" | "terminal_unavailable")
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                listed => return listed,
            }
        }
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
        self.control_with(true)
    }

    /// Gives the held control while it reads, or connects a new one, which starts the
    /// server only when `may_start` is set. Takes whether it may start one. Output: the
    /// control, or the code its connect failed with.
    fn control_with(&self, may_start: bool) -> Result<Arc<Control>, HostError> {
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
        let control = Arc::new(self.connect(may_start)?);
        *slot = Some(control.clone());
        Ok(control)
    }

    /// The address the host reaches its server at: the socket path, or the short alias of
    /// one too long for a socket address, which a confined host may not use. Output: the
    /// address, or `terminal_socket_path_long` or `terminal_folder_unsafe`.
    fn address(&self) -> Result<PathBuf, HostError> {
        let socket = crate::socket_address::address(&self.socket()).map_err(|error| {
            failure(if error.kind() == std::io::ErrorKind::InvalidInput {
                "terminal_socket_path_long"
            } else {
                "terminal_folder_unsafe"
            })
        })?;
        if self.confined.is_some() && socket != self.socket {
            return Err(failure("terminal_socket_path_long"));
        }
        Ok(socket)
    }

    /// Connects a control client to the server, starting the server with its holder
    /// session only when `may_start` is set; otherwise tmux -N refuses an ended server. A
    /// confined host always connects with -N, so its client, inside the lead's profile, never
    /// starts a server: a lead's own server and its teammates' server start under their
    /// keepers (KEEPER (b), FBTMUX-P1-R1-L1, `Servers`). Takes whether it may start one.
    /// Output: the control, or a plain code.
    fn connect(&self, may_start: bool) -> Result<Control, HostError> {
        let executable = self
            .executable
            .as_deref()
            .ok_or_else(|| failure("terminal_unavailable"))?;
        private_folder(&self.folder)?;
        let socket = self.address()?;
        if let Ok(metadata) = std::fs::symlink_metadata(&socket)
            && (!metadata.file_type().is_socket() || metadata.uid() != user())
        {
            return Err(failure("terminal_folder_unsafe"));
        }
        let mut command = match &self.confined {
            Some(profile) => {
                let mut command = Command::new(self.confinement.sandbox_exec);
                command.arg("-f").arg(profile).arg(executable);
                command
            }
            None => Command::new(executable),
        };
        let mut child = crate::spawn_gate::spawn(
            command
                .args(["-u", "-f", "/dev/null"])
                .args((!may_start || self.confined.is_some()).then_some("-N"))
                .arg("-S")
                .arg(&socket)
                .args(["-C", "new-session", "-A", "-s", SESSION, "-n", HOLDER])
                .args(HOLDER_SHELL)
                .arg(self.holder())
                .env_clear()
                .env("PATH", SERVER_PATH)
                .current_dir(&self.folder)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
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
            serial: CONTROL_SERIALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            #[cfg(test)]
            listed: std::sync::atomic::AtomicUsize::new(0),
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
            .ask(&format!("display-message -p '#{{version}}{FORMAT_END}'"))
            .map_err(|_| failure("terminal_unavailable"))?;
        if version
            .first()
            .and_then(|line| supported(&String::from_utf8_lossy(line)))
            != Some(true)
        {
            return Err(failure("terminal_tmux_unsupported"));
        }
        if let Some(pin) = &self.pin
            && let Err(refused) = pinned(&control, pin)
        {
            control.stop();
            return Err(refused);
        }
        for option in server_options(self.confined.is_some()) {
            control
                .run(&option)
                .map_err(|_| failure("terminal_unavailable"))?;
        }
        clear_unmarked_environment(&control)?;
        Ok(control)
    }

    /// The tmux target of an agent's own pane: its window on the engine's server, where it
    /// holds only the agent's app, or on a lead's own server the pane tagged with the agent's
    /// name, the lead's at spawn and each teammate's at `adopt_pane`, since teammates split
    /// the lead's window. Takes the control client and the agent. Returns the target, or
    /// `terminal_agent_unknown`.
    fn agent_target(&self, control: &Control, agent: &AgentId) -> Result<String, HostError> {
        if self.confined.is_none() {
            return Ok(target(agent));
        }
        let lines = control.ask(&format!(
            "list-panes -s -t {SESSION} -F '#{{pane_id}} #{{{AGENT_TAG}}}{FORMAT_END}'"
        ))?;
        lines
            .iter()
            .filter_map(|line| std::str::from_utf8(line).ok())
            .find_map(|line| {
                let (pane, tag) = line.split_once(' ')?;
                (tag == agent.as_str() && pane_id_valid(pane)).then(|| pane.to_owned())
            })
            .ok_or_else(|| failure("terminal_agent_unknown"))
    }

    /// The pane an agent's prompts go to: the one tagged with its
    /// name, searched on the engine's server in the agent's window only. Only a window no
    /// engine marked (`APP_MARK`) gives its one untagged pane, marked now; any other gives
    /// none, never a pane that may be a teammate's. Takes the control client and the agent.
    /// Returns the pane id, `APP_PANE_UNKNOWN`, or `terminal_agent_unknown` with no window.
    fn app_pane(&self, control: &Control, agent: &AgentId) -> Result<String, HostError> {
        if self.confined.is_some() {
            return self.agent_target(control, agent);
        }
        let lines = control.ask(&format!(
            "list-panes -t {} -F '#{{pane_id}} #{{{APP_MARK}}} #{{{AGENT_TAG}}}{FORMAT_END}'",
            target(agent)
        ))?;
        let panes: Vec<(&str, &str, &str)> = lines
            .iter()
            .filter_map(|line| {
                let mut fields = std::str::from_utf8(line).ok()?.splitn(3, ' ');
                Some((fields.next()?, fields.next()?, fields.next()?))
            })
            .filter(|(pane, ..)| pane_id_valid(pane))
            .take(PANES_LISTED_MAX)
            .collect();
        if let Some((pane, ..)) = panes.iter().find(|(.., tag)| *tag == agent.as_str()) {
            return Ok((*pane).to_owned());
        }
        let [(pane, "", "")] = panes.as_slice() else {
            return Err(failure(super::APP_PANE_UNKNOWN));
        };
        Self::mark_app(control, pane, agent)?;
        Ok((*pane).to_owned())
    }

    /// Marks a pane's window as one whose app the engine tagged, then tags the pane as the
    /// agent's app, so the one-pane fallback never takes a pane left beside it; a pane that
    /// closes between the two leaves a marked window, which refuses (CR-0351 F1). Takes the
    /// control client, the pane id and the agent. Returns once both are set, or tmux's error.
    fn mark_app(control: &Control, pane: &str, agent: &AgentId) -> Result<(), HostError> {
        assert!(pane_id_valid(pane), "only a pane id is tagged");
        control.run(&format!("set-option -w -t {pane} {APP_MARK} {agent}"))?;
        control.run(&format!("set-option -p -t {pane} {AGENT_TAG} {agent}"))?;
        Ok(())
    }

    /// Whether this lead server holds a pane tagged with the agent's name. Takes the agent.
    /// Returns false on the engine's server or when the server cannot answer.
    pub(super) fn holds(&self, agent: &AgentId) -> bool {
        self.confined.is_some()
            && self
                .live_control()
                .is_ok_and(|control| self.agent_target(&control, agent).is_ok())
    }

    /// Checks that a pane belongs to an agent terminal's own window; on a teammates' server
    /// the lead's anchor pane stands for the lead, not a teammate, and is refused. Takes the
    /// agent and the pane id. Returns the pane as a tmux target, or `terminal_pane_unknown`.
    fn member_pane(&self, agent: &AgentId, pane: &str) -> Result<String, HostError> {
        if !pane_id_valid(pane) || !self.panes(agent)?.iter().any(|listed| listed.id == pane) {
            return Err(failure("terminal_pane_unknown"));
        }
        if self.starts_in.is_some() && self.agent_target(&*self.live_control()?, agent)? == pane {
            return Err(failure("terminal_pane_unknown"));
        }
        Ok(pane.to_owned())
    }

    /// The pane tagged with an agent's name on a lead's server, as listed with its window.
    /// Takes the agent. Returns the pane, or `terminal_agent_unknown`.
    pub(super) fn own_pane(&self, agent: &AgentId) -> Result<PaneInfo, HostError> {
        let own = self.agent_target(&*self.live_control()?, agent)?;
        self.panes(agent)?
            .into_iter()
            .find(|pane| pane.id == own)
            .ok_or_else(|| failure("terminal_agent_unknown"))
    }

    /// Readies a native lead's teammates' server for the lead's start (rv-audit S1 F1): the
    /// server starts if none listens, the holder pane of a fresh one is tagged as the lead's
    /// anchor, and its environment becomes the lead's, which every teammate pane gets. Takes
    /// the lead and its command. Returns the variables the lead's pane starts with, so its
    /// app splits teammates off the anchor here: `TMUX` naming this server and `TMUX_PANE`
    /// the anchor. Fails with `terminal_command_failed` when no single anchor is found.
    pub(super) fn anchor_for(
        &self,
        lead: &AgentId,
        command: &PaneCommand,
    ) -> Result<Vec<(String, String)>, HostError> {
        assert!(
            self.starts_in.is_some(),
            "only a teammates' server holds a lead's anchor"
        );
        let control = self.control()?;
        let _one = self
            .spawning
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        let anchor = match self.agent_target(&control, lead) {
            Ok(anchor) => anchor,
            Err(_) => {
                let lines = control.ask(&format!(
                    "list-panes -t {SESSION}:={HOLDER} -F '#{{pane_id}}{FORMAT_END}'"
                ))?;
                let [only] = lines.as_slice() else {
                    return Err(failure("terminal_command_failed"));
                };
                let anchor = String::from_utf8_lossy(only).into_owned();
                if !pane_id_valid(&anchor) {
                    return Err(failure("terminal_command_failed"));
                }
                control.run(&format!("set-option -p -t {anchor} {AGENT_TAG} {lead}"))?;
                anchor
            }
        };
        Self::lead_environment(&control, command)?;
        let lines = control.ask(&format!(
            "display-message -p -t {anchor} '#{{pid}} #{{session_id}}{FORMAT_END}'"
        ))?;
        let text = lines
            .first()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .unwrap_or_default();
        let (pid, session) = text
            .split_once(' ')
            .and_then(|(pid, session)| {
                let pid = pid.parse::<u32>().ok().filter(|pid| *pid > 0)?;
                let session = session.strip_prefix('$')?.parse::<u32>().ok()?;
                Some((pid, session))
            })
            .ok_or_else(|| failure("terminal_command_failed"))?;
        let server = self
            .socket
            .to_str()
            .filter(|server| !server.contains(','))
            .ok_or_else(|| failure("terminal_folder_unsafe"))?;
        assert!(pane_id_valid(&anchor));
        Ok(vec![
            ("TMUX".to_owned(), format!("{server},{pid},{session}")),
            ("TMUX_PANE".to_owned(), anchor),
        ])
    }

    /// Hands a native-mode lead's environment to its own or its teammates' server before the
    /// lead starts: the server environment is emptied, then holds the terminal defaults and
    /// the lead's own checked variables, so the lead and every teammate pane it splits off
    /// get those and the tmux variables, and nothing of the engine's. Takes the control
    /// client and the command. Returns Ok, or `terminal_environment_unclean`.
    fn lead_environment(control: &Control, command: &PaneCommand) -> Result<(), HostError> {
        clear_environment(control)?;
        let defaults = TERMINAL_DEFAULTS
            .iter()
            .filter(|(default, _)| !command.env.iter().any(|(name, _)| name == default))
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()));
        for (name, value) in defaults.chain(command.env.iter().cloned()) {
            let line = format!("set-environment -g {} {}", quote(&name), quote(&value));
            if line.len() > COMMAND_BYTES_MAX {
                return Err(failure("terminal_command_invalid"));
            }
            control
                .run(&line)
                .map_err(|_| failure("terminal_environment_unclean"))?;
        }
        Ok(())
    }

    /// Hands bytes to one tmux target: a request `pastes` takes goes through a named tmux
    /// buffer in one paste, any other is typed as hex keys, one command after another, in
    /// order, all into the pane a window target names at the start when there are several.
    /// Takes the control client, the target and the bytes.
    /// Returns once tmux took them all, `terminal_input_bounds`, or the lookup's code.
    fn send_bytes(control: &Control, target: &str, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        if pastes(bytes) {
            return Self::send_pasted(control, target, bytes);
        }
        let mut lines = send_keys_lines(target, bytes);
        if lines.len() > 1 && !pane_id_valid(target) {
            let pane = Self::displayed(control, target, "#{pane_id}")?;
            if !pane_id_valid(&pane) {
                return Err(failure("terminal_command_failed"));
            }
            lines = send_keys_lines(&pane, bytes);
        }
        for line in lines {
            control.run(&line)?;
        }
        Ok(())
    }

    /// Pastes bytes into one tmux target through a buffer no other paste uses: `set-buffer`
    /// then `paste-buffer -d -r -S`, written back to back, so the pane gets the bytes
    /// unchanged in one write and the buffer is deleted; on a failure a best-effort
    /// `delete-buffer` follows, so the text does not stay on the server. Takes the control
    /// client, the target and bytes `pastes` takes. Returns once tmux took them all.
    fn send_pasted(control: &Control, target: &str, bytes: &[u8]) -> Result<(), HostError> {
        assert!(pastes(bytes));
        let buffer = paste_buffer_name();
        let lines = paste_buffer_lines(target, &buffer, bytes);
        assert_eq!(lines.len(), COMMANDS_SENT_MAX);
        let pasted = control.run_all(&lines);
        if pasted.is_err() {
            let _ = control.run(&format!("delete-buffer -b {buffer}"));
        }
        pasted
    }

    /// Pastes one owner paste into one tmux target through `owner_paste_lines` (T7), which
    /// brackets it only when the pane asked, with a best-effort `delete-buffer` on a failure.
    /// Takes the control client, the target and 1 to `PASTE_BYTES_MAX` keys from
    /// `paste_keys`, without a NUL. Returns once tmux took them all, `terminal_paste_bounds`,
    /// or tmux's refusal.
    fn send_paste(control: &Control, target: &str, keys: &[u8]) -> Result<(), HostError> {
        if keys.is_empty() || keys.len() > PASTE_BYTES_MAX || keys.contains(&0) {
            return Err(failure("terminal_paste_bounds"));
        }
        let buffer = paste_buffer_name();
        let lines = owner_paste_lines(target, &buffer, keys);
        assert_eq!(lines.len(), keys.len().div_ceil(INPUT_BYTES_MAX) + 1);
        let pasted = lines
            .iter()
            .try_for_each(|line| control.run(line).map(|_| ()));
        if pasted.is_err() {
            let _ = control.run(&format!("delete-buffer -b {buffer}"));
        }
        pasted
    }

    /// Reads whether bracketed paste is on in one tmux target. Takes the control client and
    /// the target. Returns the answer, or `terminal_agent_unknown` once the target is gone.
    fn paste_on(control: &Control, target: &str) -> Result<bool, HostError> {
        Ok(Self::displayed(control, target, "#{bracket_paste_flag}")? == "1")
    }

    /// Reads one format of one tmux target. `display-message` answers for the current pane,
    /// with success, when its target is gone (tmux 3.8), so the answer also names its pane
    /// and window, and one of them must be the target. Takes the control client, a `%N` pane
    /// or `session:=window` target, and a format whose text holds no space; the names come
    /// last and may. Returns the format's text, or `terminal_agent_unknown` once the target
    /// is gone.
    fn displayed(control: &Control, target: &str, format: &str) -> Result<String, HostError> {
        let named = "#{session_name}:=#{window_name}";
        let lines = control.ask(&format!(
            "display-message -p -t {target} '#{{pane_id}} {format} {named}{FORMAT_END}'"
        ))?;
        let unread = || failure("terminal_command_failed");
        let line = lines
            .first()
            .and_then(|line| std::str::from_utf8(line).ok())
            .ok_or_else(unread)?;
        let (pane, rest) = line.split_once(' ').ok_or_else(unread)?;
        let (text, window) = rest.split_once(' ').ok_or_else(unread)?;
        if pane != target && window != target {
            return Err(failure("terminal_agent_unknown"));
        }
        Ok(text.to_owned())
    }

    /// The held control client while it is the one that found a view's pane and still reads,
    /// so a command aimed at that pane id never reaches a pane of a newer server, which can
    /// reuse the id. Takes the view's pane. Returns the client, or none for a malformed id or
    /// another or closed client.
    fn control_at(&self, at: &PaneAt) -> Result<Option<Arc<Control>>, HostError> {
        if !pane_id_valid(&at.pane) {
            return Ok(None);
        }
        let held = self
            .control
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?
            .clone();
        Ok(held.filter(|control| control.serial == at.serial && !control.closed()))
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

fn release(version: &str) -> Option<(u32, u32)> {
    let version = version.trim();
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse::<u32>().ok()?, digits.parse::<u32>().ok()?))
}

/// Whether agent terminals support a tmux version, the second word of `tmux -V`: 3.7 or
/// later ([`VERSION_MIN`]). Input: the version. Output: the answer, or none when it is not
/// a release number.
pub fn supported(version: &str) -> Option<bool> {
    release(version).map(|found| found >= VERSION_MIN)
}

/// The oldest tmux version agent terminals support, as setup names it. Output: the
/// version, such as "3.7".
pub fn version_needed() -> String {
    format!("{}.{}", VERSION_MIN.0, VERSION_MIN.1)
}

impl Tmux {
    /// Has a server that runs in the foreground, as a lead's own server under its keeper
    /// does, end once its last session ends, as a server a client started does (FBTMUX-P1),
    /// since `tmux -D` turns exit-empty off. Its control, connecting, makes the holder
    /// session first. Output: Ok, or the code the control or the command failed with.
    pub(super) fn exit_when_empty(&self) -> Result<(), HostError> {
        self.control()?.run("set-option -s exit-empty on").map(drop)
    }

    /// Starts an agent's terminal as [`Host::spawn`] does, the pane of a lead's own server
    /// also starting with `pane_env` set over what the server hands it, such as the
    /// variables that point a native lead's app at its teammates' server. Takes the agent,
    /// the command and those variables, none off a lead's server. Returns where it runs, or
    /// the plain code.
    pub(super) fn spawn_with(
        &self,
        agent: &AgentId,
        command: &PaneCommand,
        pane_env: &[(String, String)],
    ) -> Result<PaneRef, HostError> {
        let line = match &self.confined {
            Some(profile) => confined_line(agent, command, profile, pane_env, &self.confinement)?,
            None if pane_env.is_empty() => spawn_line(agent, command, &self.confinement)?,
            None => return Err(failure("terminal_command_invalid")),
        };
        let control = self.control()?;
        let _one = self
            .spawning
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if windows(&control)?.contains(agent) {
            return Err(failure("terminal_agent_running"));
        }
        if self.confined.is_some() {
            Self::lead_environment(&control, command)?;
        }
        self.settle(&control, agent, control.run(&line))
    }

    /// Settles the agent window a spawn asked for: on any error, from the reply that made it
    /// on, it kills that window through an open control, reconnecting to the live server when
    /// a timeout closed this one, so a failed spawn leaves none. Takes the control, the agent
    /// and the reply. Returns the pane, or the code `placed` or the reply failed with.
    fn settle(
        &self,
        control: &Control,
        agent: &AgentId,
        made: Result<Vec<Vec<u8>>, HostError>,
    ) -> Result<PaneRef, HostError> {
        let settled = made.and_then(|lines| self.placed(control, agent, &lines));
        if settled.is_err() {
            let kill = format!("kill-window -t {}", target(agent));
            let _ = self.control_with(false).and_then(|open| open.run(&kill));
        }
        settled
    }

    /// Reads where a new agent window's pane runs from the reply that made it, or asks tmux
    /// when the reply holds no whole line, then checks the pane is served and tags it as the
    /// agent's app. Takes the control, the agent and the reply lines. Returns the pane, or
    /// `terminal_command_failed` or the lookup's code.
    fn placed(
        &self,
        control: &Control,
        agent: &AgentId,
        lines: &[Vec<u8>],
    ) -> Result<PaneRef, HostError> {
        let placed = match lines.first().and_then(|line| whole(line)) {
            Some(placed) => placed.to_vec(),
            None => control
                .ask(&format!(
                    "list-panes -t {} -F '{PLACED}{FORMAT_END}'",
                    target(agent)
                ))?
                .into_iter()
                .next()
                .unwrap_or_default(),
        };
        let text = String::from_utf8_lossy(&placed).into_owned();
        let (pane, pid) = text
            .split_once(' ')
            .ok_or_else(|| failure("terminal_command_failed"))?;
        let pid = pid
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 1 && pane.starts_with('%'))
            .ok_or_else(|| failure("terminal_command_failed"))?;
        let first = PaneInfo {
            id: pane.to_owned(),
            pid,
            dead: false,
        };
        let untagged = || !pane_id_valid(pane) || Self::mark_app(control, pane, agent).is_err();
        if self.served(&[first]).is_empty() || untagged() {
            return Err(failure("terminal_command_failed"));
        }
        Ok(PaneRef {
            agent: agent.clone(),
            pane: pane.to_owned(),
            pid,
        })
    }
}

impl Host for Tmux {
    fn spawn(&self, agent: &AgentId, command: &PaneCommand) -> Result<PaneRef, HostError> {
        self.spawn_with(agent, command, &[])
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        match self.capture(agent, None, HISTORY_ROWS_MAX, true) {
            Err(problem) if problem.0 == "terminal_reply_too_large" => {
                self.capture(agent, None, 0, true)
            }
            attached => attached,
        }
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.capture(agent, None, 0, false)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        Self::send_bytes(&control, &target, bytes)
    }

    fn input_at(&self, _agent: &AgentId, at: &PaneAt, bytes: &[u8]) -> Result<bool, HostError> {
        let Some(control) = self.control_at(at)? else {
            return Ok(false);
        };
        assert_eq!(
            control.serial, at.serial,
            "a pinned send uses the connection that found it"
        );
        Self::send_bytes(&control, &at.pane, bytes).map(|()| true)
    }

    fn own_pane_at(&self, agent: &AgentId) -> Result<PaneAt, HostError> {
        let control = self.live_control()?;
        let pane = if self.confined.is_some() {
            self.agent_target(&control, agent)?
        } else {
            Self::displayed(&control, &target(agent), "#{pane_id}")?
        };
        if !pane_id_valid(&pane) {
            return Err(failure("terminal_command_failed"));
        }
        Ok(pin_at(pane, control.serial))
    }

    fn app_pane_at(&self, agent: &AgentId) -> Result<PaneAt, HostError> {
        let control = self.live_control()?;
        let pane = self.app_pane(&control, agent)?;
        if !pane_id_valid(&pane) {
            return Err(failure("terminal_command_failed"));
        }
        Ok(pin_at(pane, control.serial))
    }

    fn input_live_at(&self, _agent: &AgentId, at: &PaneAt) -> Result<Option<bool>, HostError> {
        let Some(control) = self.control_at(at)? else {
            return Ok(None);
        };
        Self::paste_on(&control, &at.pane).map(Some)
    }

    fn tag_app(&self, agent: &AgentId, pid: u32) -> Result<(), HostError> {
        assert!(pid > 1, "a pane record keeps a real pid");
        let control = self.live_control()?;
        let app = self
            .panes(agent)?
            .into_iter()
            .find(|pane| pane.pid == pid && !pane.dead)
            .ok_or_else(|| failure(super::APP_PANE_UNKNOWN))?;
        Self::mark_app(&control, &app.id, agent)
    }

    fn paste_at(&self, _agent: &AgentId, at: &PaneAt, keys: &[u8]) -> Result<bool, HostError> {
        if keys.is_empty() || keys.len() > PASTE_BYTES_MAX {
            return Err(failure("terminal_paste_bounds"));
        }
        let Some(control) = self.control_at(at)? else {
            return Ok(false);
        };
        assert_eq!(
            control.serial, at.serial,
            "a pinned send uses the connection that found it"
        );
        Self::send_paste(&control, &at.pane, keys).map(|()| true)
    }

    fn resize_at(
        &self,
        _agent: &AgentId,
        at: &PaneAt,
        size: (u16, u16),
    ) -> Result<bool, HostError> {
        let (cols, rows) = size;
        let bounds = SIZE_MIN..=SIZE_MAX;
        if !bounds.contains(&cols) || !bounds.contains(&rows) {
            return Err(failure("terminal_size_bounds"));
        }
        let Some(control) = self.control_at(at)? else {
            return Ok(false);
        };
        assert_eq!(
            control.serial, at.serial,
            "a pinned send uses the connection that found it"
        );
        let line = format!("resize-window -t {} -x {cols} -y {rows}", at.pane);
        control.run(&line).map(|_| true)
    }

    /// Names the window a resize of the agent sizes: this server's socket and the window's
    /// id, so every view of a split team window counts once and two servers never share a
    /// key. Takes the agent. Returns the key, or `terminal_agent_unknown` once the window is
    /// gone.
    fn unit(&self, agent: &AgentId) -> Result<String, HostError> {
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        let window = Self::displayed(&control, &target, "#{window_id}")?;
        let id = window.strip_prefix('@').unwrap_or_default();
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(failure("terminal_command_failed"));
        }
        Ok(format!("{} {window}", self.socket.display()))
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        Self::paste_on(&control, &target)
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        let pane = self.member_pane(agent, pane)?;
        match self.capture(agent, Some(&pane), HISTORY_ROWS_MAX, true) {
            Err(problem) if problem.0 == "terminal_reply_too_large" => {
                self.capture(agent, Some(&pane), 0, true)
            }
            attached => attached,
        }
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        let pane = self.member_pane(agent, pane)?;
        self.capture(agent, Some(&pane), 0, false)
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        let pane = self.member_pane(agent, pane)?;
        Self::send_bytes(&*self.live_control()?, &pane, bytes)
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        let pane = self.member_pane(agent, pane)?;
        Self::paste_on(&*self.live_control()?, &pane)
    }

    fn paste(&self, agent: &AgentId, keys: &[u8]) -> Result<(), HostError> {
        if keys.is_empty() || keys.len() > PASTE_BYTES_MAX {
            return Err(failure("terminal_paste_bounds"));
        }
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        Self::send_paste(&control, &target, keys)
    }

    fn paste_pane(&self, agent: &AgentId, pane: &str, keys: &[u8]) -> Result<(), HostError> {
        if keys.is_empty() || keys.len() > PASTE_BYTES_MAX {
            return Err(failure("terminal_paste_bounds"));
        }
        let pane = self.member_pane(agent, pane)?;
        Self::send_paste(&*self.live_control()?, &pane, keys)
    }

    fn adopt_pane(&self, agent: &AgentId, lead: &AgentId, pane: &str) -> Result<(), HostError> {
        if self.confined.is_none() {
            return Err(failure(OWN_SERVER_UNAVAILABLE));
        }
        if agent == lead || agent.team_agent().is_none() {
            return Err(failure("terminal_agent_invalid"));
        }
        let pane = self.member_pane(lead, pane)?;
        let control = self.live_control()?;
        if self.agent_target(&control, lead)? == pane {
            return Err(failure("terminal_pane_unknown"));
        }
        control
            .run(&format!("set-option -p -t {pane} {AGENT_TAG} {agent}"))
            .map(drop)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        let activity = Self::displayed(&control, &target, "#{window_activity}")?
            .trim()
            .parse::<u64>()
            .map_err(|_| failure("terminal_command_failed"))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| failure("terminal_command_failed"))?
            .as_secs();
        Ok(Some(Duration::from_secs(now.saturating_sub(activity))))
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        let size = SIZE_MIN..=SIZE_MAX;
        if !size.contains(&cols) || !size.contains(&rows) {
            return Err(failure("terminal_size_bounds"));
        }
        let control = self.live_control()?;
        let target = self.agent_target(&control, agent)?;
        control
            .run(&format!("resize-window -t {target} -x {cols} -y {rows}"))
            .map(drop)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let control = self.live_control()?;
        if self.confined.is_some() {
            let pane = self.own_pane(agent)?;
            let line = format!("kill-pane -t {}", pane.id);
            return self.close_panes(&control, &[pane], &line);
        }
        let panes = self.panes(agent)?;
        self.close_panes(
            &control,
            &panes,
            &format!("kill-window -t {}", target(agent)),
        )
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        let control = self.live_control()?;
        let window = self.agent_target(&control, agent)?;
        let lines = control.ask(&format!(
            "list-panes -t {window} -F '{PANE_FIELDS}{FORMAT_END}'"
        ))?;
        let panes: Vec<PaneInfo> = lines
            .iter()
            .filter_map(|line| pane_info(line))
            .take(PANES_LISTED_MAX)
            .collect();
        assert!(panes.len() <= PANES_LISTED_MAX);
        Ok(panes)
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        let pane = self.member_pane(agent, pane)?;
        let panes = self.panes(agent)?;
        if panes.len() < 2 {
            return Err(failure("terminal_pane_unknown"));
        }
        let closed: Vec<PaneInfo> = panes
            .into_iter()
            .filter(|listed| listed.id == pane)
            .collect();
        self.close_panes(
            &*self.live_control()?,
            &closed,
            &format!("kill-pane -t {pane}"),
        )
    }
    fn unkept(&self, _agent: &AgentId) -> bool {
        self.confined.is_some()
    }

    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        let pane = if self.confined.is_some() {
            self.own_pane(agent)?
        } else {
            self.panes(agent)?
                .into_iter()
                .min_by_key(|pane| {
                    pane.id
                        .get(1..)
                        .and_then(|digits| digits.parse::<u32>().ok())
                        .unwrap_or(u32::MAX)
                })
                .ok_or_else(|| failure("terminal_command_failed"))?
        };
        (pane.pid > 1)
            .then(|| {
                self.served(std::slice::from_ref(&pane))
                    .first()
                    .map(|pane| pane.pid)
            })
            .ok_or_else(|| failure("terminal_command_failed"))
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        if self.executable.is_none() {
            return Err(failure("terminal_unavailable"));
        }
        if !self.running() {
            return Ok(Vec::new());
        }
        if let Some(control) = self.open_control() {
            match windows(&control) {
                Err(error) if error.0 == "terminal_closed" => {}
                listed => return listed,
            }
        }
        self.alive_reconnecting()
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        let ended = self.end_server(keep);
        self.join_reapers();
        ended
    }

    fn renew(&self, until: SystemTime) -> Result<(), HostError> {
        renew_socket(&self.socket, until)
    }
}

const _: () = assert!(SEND_KEYS_BYTES_MAX > super::RUN_CUT_BACK_BYTES_MAX);
const _: () = assert!(SEND_KEYS_BYTES_MAX <= INPUT_BYTES_MAX);

/// The control client the host drives tmux through and the reader that matches its replies.
mod control;
/// How the host ends the process groups of the panes it closes and waits for their reapers.
mod groups;
/// The tmux command lines the host builds and the screen it renders from a pane.
mod lines;
#[cfg(all(test, target_os = "macos"))]
#[path = "tmux/placement_tests.rs"]
mod placement_tests;
mod servers;

/// The tmux program a row runs, found through the resolver with the stub seed. Input: the
/// engine root. Output: the program, or none when tmux is not installed.
#[cfg(test)]
pub(crate) fn installed(engine_root: &Path) -> Option<PathBuf> {
    installed_at(&crate::finding::Places::from_environment(
        Some(engine_root),
        crate::finding::stub::stub_seed(),
    ))
}

/// Test copies of the seams crazyagent fills: the team state layout, the sandbox keeper
/// and shim words, the pane variable gate and the hooks a row opens its host with.
#[cfg(any(test, feature = "test-support"))]
pub mod test_seams;

#[cfg(all(test, target_os = "macos"))]
#[path = "tmux/reattach_tests.rs"]
mod reattach_tests;
#[cfg(test)]
#[path = "tmux/tests.rs"]
mod tests;
