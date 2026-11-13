use super::buffer::{Feed, buffer, skipped_marker};
pub use super::harness_socket;
use super::{
    AgentId, Host, HostError, INPUT_BYTES_MAX, OWN_SERVER_UNAVAILABLE, PANES_LISTED_MAX, PaneInfo,
    PaneRef, SCROLLBACK_LINES, SIZE_MAX, SIZE_MIN, TEAMMATES_SERVER_REPLACED, TerminalStream,
    VIEWERS_MAX, end_except, failure, pane_id_valid,
};
use crate::harnesses::interactive::InteractiveCommand;
use crate::harnesses::launch::Ending;
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
/// How many 2 second holder loops pass between two reads of the server's deadline: the
/// modification time of its socket, which the running engine keeps ahead (FBTMUX-P1).
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
        "s=${{TMUX%,*,*}}; i=0; a=0; l=0; while [ -S \"$s\" ]; do i=$((i+1)); if [ $i -ge {loops} ]; then i=0; d=$({MODIFIED} \"$s\" 2>/dev/null) && n=$(date +%s) && if [ \"$n\" -lt \"$d\" ]; then a=1; l=0; elif [ $a = 1 ]; then l=$((l+1)); fi; [ $l -ge {late} ] && break; fi; sleep 2; done; p=${{TMUX%,*}}; kill \"${{p##*,}}\""
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
const TERMINAL_TYPE: &str = "xterm-256color";
const TERMINAL_DEFAULTS: [(&str, &str); 2] = [("TERM", TERMINAL_TYPE), ("COLORTERM", "truecolor")];
const VERSION_MIN: (u32, u32) = (3, 2);
const CAPTURE_WRITTEN_MIN: (u32, u32) = (3, 4);
const CAPTURE_WRITTEN: &str = "-p -e -N -T";
const CAPTURE_TRIMMED: &str = "-p -e";
const REPLY_TIME: Duration = Duration::from_secs(10);
/// Ends every format the engine has tmux expand. tmux stops expanding a format that has run
/// for 100 ms of wall time, which a server stalled on a loaded machine reaches, and answers
/// with the text so far as a success; only a line ending with this mark was expanded whole.
const FORMAT_END: &str = " end";
const REPLY_CUT: &str = "terminal_reply_cut";
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
}

struct Control {
    child: Mutex<Option<Child>>,
    input: Mutex<Option<ChildStdin>>,
    state: Arc<Mutex<State>>,
    capture: &'static str,
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
        history: usize,
        view: bool,
        reply: SyncSender<Result<TerminalStream, HostError>>,
    },
}

/// Finds the tmux program the engine uses, anew on each call, through the one tool resolver:
/// the app's folder, `<root>/setup/bin`, then everywhere [`crate::tools::Places`] looks.
/// Takes the engine root. Returns the real program, or none when tmux is not installed.
pub fn installed(engine_root: &Path) -> Option<PathBuf> {
    installed_at(&crate::tools::Places::from_environment(Some(engine_root)))
}

/// Finds the tmux program the engine uses in the given places, the answer setup shows: the
/// bundled copy when it runs, else the usual order ([`crate::tools::resolve_tmux`]).
/// Takes the places. Returns the real program, or none when tmux is not installed.
pub fn installed_at(places: &crate::tools::Places) -> Option<PathBuf> {
    crate::tools::resolve_tmux(places, crate::tools::SHIPS_BUNDLED_TMUX, runs)
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
/// terminal supports (3.2 or later). A read that runs out of time counts as running, since a
/// new file can wait behind the system's first-run checks; the version read setup does next
/// reports it. Takes the tmux. Returns the answer.
pub fn runs(tmux: &crate::tools::Resolved) -> bool {
    runs_within(tmux, VERSION_READ_TIME)
}

/// Answers [`runs`] with the version read bounded by the given time, so a test can let a
/// read run out of time quickly. Takes the tmux and the bound. Returns the answer.
fn runs_within(tmux: &crate::tools::Resolved, time: Duration) -> bool {
    assert!(!time.is_zero() && time <= VERSION_READ_TIME);
    let Ok(metadata) = std::fs::metadata(&tmux.program) else {
        return false;
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
        return true;
    }
    let words = tmux.argv();
    let Some((program, rest)) = words.split_first() else {
        return false;
    };
    let mut arguments = rest.to_vec();
    arguments.push("-V".into());
    let version = crate::setup::process::run(
        Path::new(program),
        &arguments,
        &[],
        &std::env::temp_dir(),
        time,
    );
    let running = match version {
        Err(crate::setup::InstallError(code)) => code == "setup_timeout",
        Ok((false, _)) => false,
        Ok((true, output)) => crate::setup::process::version_from(&output)
            .is_ok_and(|found| supported(found.as_str()) == Some(true)),
    };
    if running {
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
    pub fn open(executable: Option<PathBuf>, folder: &Path) -> Self {
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
        }
    }

    /// Opens the host on a native-mode lead's own tmux server (see [`harness_socket`]); the
    /// server and each engine client on it start inside the lead's profile, and nothing starts
    /// until the first call. Takes the tmux program, the socket in the lead's private temp
    /// folder and the lead's installed profile. Returns the host, or `terminal_folder_unsafe`
    /// for a socket path with no folder.
    pub fn confined(
        executable: Option<PathBuf>,
        socket: &Path,
        profile: &Path,
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
        })
    }

    /// Opens the host on a native lead's teammates' own tmux server (see
    /// [`super::teammates_socket`], rv-audit S1 F1): the server starts inside the teammates'
    /// profile, which cannot reach its socket, and each engine client connects inside the
    /// lead's profile and never starts a server, so no server runs in the lead's profile with
    /// teammate panes. Each client it connects checks the server's pid against `pin`.
    /// Nothing starts until the first call. Takes the tmux program, the socket, the lead's
    /// installed profile, the teammates' and the socket's pin. Returns the host, or
    /// `terminal_folder_unsafe` for a socket path with no folder.
    fn teammates(
        executable: Option<PathBuf>,
        socket: &Path,
        profile: &Path,
        server_profile: &Path,
        pin: Arc<ServerPin>,
    ) -> Result<Self, HostError> {
        if !server_profile.is_absolute() || server_profile == profile {
            return Err(failure("terminal_folder_unsafe"));
        }
        Ok(Self {
            starts_in: Some(server_profile.to_path_buf()),
            pin: Some(pin),
            ..Self::confined(executable, socket, profile)?
        })
    }

    /// Has a server this host starts read its deadline every `loops` holder loops and end
    /// after `late` passed reads, so a row sees the bound within seconds. Takes the spacing
    /// and the count. Returns the host.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn with_deadline_checks(mut self, loops: u32, late: u32) -> Self {
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
        crate::team::caller::peer_pid(&stream).filter(|pid| *pid > 1)
    }

    /// Keeps the panes whose first process the kernel reports as a child of the listener at
    /// this host's socket. tmux forks each pane's first process from its server, while a pane
    /// listing is only what the server says, and a native lead can swap in a listener of its
    /// own at that socket, so a stand-in can name no process outside the server bound at
    /// this address. Takes the panes. Returns those kept, none when the listener or the
    /// process table cannot be read.
    fn served(&self, panes: &[PaneInfo]) -> Vec<PaneInfo> {
        assert!(panes.len() <= PANES_LISTED_MAX);
        let (Some(server), Some(parents)) =
            (self.server_pid(), crate::team::caller::process_parents())
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

    /// Starts a teammates' server with its holder session inside the teammates' profile when
    /// no socket exists yet (rv-audit S1 F1); a raw connect probe, never a tmux client,
    /// looks. A socket an ended server left is never removed or served again (see
    /// [`Self::remove_socket`]). Takes the tmux program, the socket and the teammates'
    /// profile. Returns Ok once a server was found or started, or `terminal_unavailable`.
    fn start_in(&self, executable: &Path, socket: &Path, profile: &Path) -> Result<(), HostError> {
        match crate::spawn_gate::connect_unix_std(socket) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure("terminal_unavailable")),
        }
        let started = crate::spawn_gate::status(
            Command::new(crate::sandbox::SANDBOX_EXEC)
                .arg("-f")
                .arg(profile)
                .arg(executable)
                .args(["-u", "-f", "/dev/null", "-S"])
                .arg(socket)
                .args(["new-session", "-d", "-s", SESSION, "-n", HOLDER])
                .args(HOLDER_SHELL)
                .arg(self.holder())
                .env_clear()
                .env("PATH", SERVER_PATH)
                .current_dir(&self.folder)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .map_err(|_| failure("terminal_unavailable"))?;
        if !started.success() {
            return Err(failure("terminal_unavailable"));
        }
        Ok(())
    }

    /// Connects a control client to the server, starting the server with its holder
    /// session only when `may_start` is set; otherwise tmux -N refuses an ended server. A
    /// confined host always connects with -N, so its client, inside the lead's profile, never
    /// starts a server: a teammates host starts its server through [`Self::start_in`], and a
    /// lead's own server starts under its keeper (KEEPER (b), `Servers`). Takes whether it
    /// may start one. Output: the control, or a plain code.
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
        if may_start && let Some(server_profile) = &self.starts_in {
            self.start_in(executable, &socket, server_profile)?;
        }
        let mut command = match &self.confined {
            Some(profile) => {
                let mut command = Command::new(crate::sandbox::SANDBOX_EXEC);
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
        let mut control = Control {
            child: Mutex::new(Some(child)),
            input: Mutex::new(Some(input)),
            state,
            capture: CAPTURE_TRIMMED,
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
        let Some(capture) = version
            .first()
            .and_then(|line| capture_flags(&String::from_utf8_lossy(line)))
        else {
            return Err(failure("terminal_tmux_unsupported"));
        };
        control.capture = capture;
        if let Some(pin) = &self.pin
            && let Err(refused) = pinned(&control, pin)
        {
            control.stop();
            return Err(refused);
        }
        for option in [
            format!("set-option -g default-terminal {TERMINAL_TYPE}"),
            "set-option -gw allow-rename off".into(),
            "set-option -gw automatic-rename off".into(),
            "set-option -gw remain-on-exit off".into(),
            format!("set-option -g history-limit {SCROLLBACK_LINES}"),
        ]
        .into_iter()
        .chain(
            self.confined
                .is_some()
                .then(|| format!("set-option -g default-shell {LEAD_SHELL}")),
        ) {
            control
                .run(&option)
                .map_err(|_| failure("terminal_unavailable"))?;
        }
        clear_environment(&control)?;
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
        command: &InteractiveCommand,
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
    fn lead_environment(control: &Control, command: &InteractiveCommand) -> Result<(), HostError> {
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

    /// Types bytes into one tmux target as hex keys. Takes the control client, the target and
    /// the bytes. Returns once tmux took them, or `terminal_input_bounds`.
    fn send_bytes(control: &Control, target: &str, bytes: &[u8]) -> Result<(), HostError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mut line = format!("send-keys -t {target} -H");
        for byte in bytes {
            let _ = write!(line, " {byte:02x}");
        }
        control.run(&line).map(drop)
    }

    /// Reads whether bracketed paste is on in one tmux target. Takes the control client and
    /// the target. Returns the answer.
    fn paste_on(control: &Control, target: &str) -> Result<bool, HostError> {
        let lines = control.ask(&format!(
            "display-message -p -t {target} '#{{bracket_paste_flag}}{FORMAT_END}'"
        ))?;
        Ok(lines.first().is_some_and(|line| line.as_slice() == b"1"))
    }
}

/// Empties the private server's environment so nothing reaches a pane from it: no
/// variables copied from an attaching client (`update-environment` empty) and none left in
/// the global or session environment, then checks both are empty.
/// Input: the control client. Output: Ok, or `terminal_environment_unclean`.
fn clear_environment(control: &Control) -> Result<(), HostError> {
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
fn variable_names(lines: &[Vec<u8>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| String::from_utf8_lossy(line))
        .filter(|line| !line.starts_with('-'))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Pins a teammates' server to the pid it first answered with (PLC c): a server under
/// another pid at the same socket, such as one a process bound there after the teammates'
/// server ended, is refused, so no listing or input of its is taken for the lead's team.
/// The pin is read from its record before the first check and recorded before the first pid
/// is taken (PC-L1). Takes the new control and the socket's pin. Returns Ok,
/// `terminal_command_failed` for an unreadable pid, `terminal_own_server_unavailable` for a
/// record that cannot be read or kept, or `terminal_teammates_server_replaced`.
fn pinned(control: &Control, pin: &ServerPin) -> Result<(), HostError> {
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
        *kept = super::teammates_server_pid(&pin.root, &pin.lead, &pin.socket)?;
    }
    match *kept {
        None => {
            super::keep_teammates_server_pid(&pin.root, &pin.lead, &pin.socket, pid)?;
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

    /// Runs a command that only reads, whose every reply line is a format ending with
    /// `FORMAT_END`, and asks again while tmux cut a line short, for at most `REPLY_TIME`.
    /// Takes the command. Returns the lines without the mark, or `terminal_not_answering`.
    fn ask(&self, line: &str) -> Answer {
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

fn release(version: &str) -> Option<(u32, u32)> {
    let version = version.trim();
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse::<u32>().ok()?, digits.parse::<u32>().ok()?))
}

fn supported(version: &str) -> Option<bool> {
    release(version).map(|found| found >= VERSION_MIN)
}

/// Picks the `capture-pane` flags for the screen sent on attach. From tmux 3.4 the screen
/// keeps every cell the program wrote, trailing spaces included, and no unused cell; older
/// tmux trims trailing spaces and the cursor position still shows where the program is.
/// Takes the tmux version report. Returns the flags, or none for an unsupported tmux.
fn capture_flags(version: &str) -> Option<&'static str> {
    if !supported(version)? {
        return None;
    }
    Some(if release(version)? >= CAPTURE_WRITTEN_MIN {
        CAPTURE_WRITTEN
    } else {
        CAPTURE_TRIMMED
    })
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

/// Checks a command for a new pane: bounded argv and environment, an absolute plain working
/// folder, a first word tmux cannot read as an option or assignment, and variables an agent
/// may carry. Takes the command. Returns its working folder as text, or
/// `terminal_command_invalid`.
fn checked(command: &InteractiveCommand) -> Result<&str, HostError> {
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
        || command.env.iter().any(|(name, value)| {
            !env_name(name)
                || value.contains('\0')
                || !crate::sandbox::agent_variable_allowed(OsStr::new(name), OsStr::new(value))
        })
    {
        return Err(invalid());
    }
    Ok(cwd)
}

fn spawn_line(agent: &AgentId, command: &InteractiveCommand) -> Result<String, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = checked(command)?;
    let mut line = format!(
        "new-window -d -P -F '{PLACED}{FORMAT_END}' -n {agent} -c {} -t {SESSION}:",
        quote(cwd)
    );
    line.push_str(" -- ");
    line.push_str(ENV_PROGRAM);
    for (default, value) in TERMINAL_DEFAULTS {
        if !command.env.iter().any(|(name, _)| name == default) {
            line.push(' ');
            line.push_str(&quote(&format!("{default}={value}")));
        }
    }
    for (name, value) in &command.env {
        line.push(' ');
        line.push_str(&quote(&format!("{name}={value}")));
    }
    for arg in &command.argv {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    if line.len() > COMMAND_BYTES_MAX {
        return Err(invalid());
    }
    Ok(line)
}

/// Builds the new-window line that starts a native-mode lead in its own server. The server
/// already runs inside the lead's profile, so the command's leading `sandbox-exec -f
/// <profile>` must name that same profile and is dropped, and the environment comes from the
/// server (see `Tmux::lead_environment`) with `pane_env` set over it in the lead's pane only.
/// The lead's window becomes the session's current one, since tmux puts a split the lead's
/// app runs without a target into the current window, which would otherwise be the holder.
/// A command the engine put under its pane keeper is taken back out of it first
/// (`crate::sandbox::unkept_pane`): the keeper cannot run inside the server's sandbox.
/// Takes the agent, the command, the server's profile and those variables. Returns the line,
/// or `terminal_command_invalid` for any other command or a variable name not plain.
fn confined_line(
    agent: &AgentId,
    command: &InteractiveCommand,
    profile: &Path,
    pane_env: &[(String, String)],
) -> Result<String, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = checked(command)?;
    #[cfg(target_os = "macos")]
    let unkept = crate::sandbox::unkept_pane(&command.argv);
    #[cfg(not(target_os = "macos"))]
    let unkept: Option<Vec<String>> = None;
    let argv = unkept.as_deref().unwrap_or(&command.argv);
    let profile = profile.to_str().ok_or_else(invalid)?;
    let confined = [crate::sandbox::SANDBOX_EXEC, "-f", profile];
    if argv.len() <= confined.len()
        || argv[..confined.len()]
            .iter()
            .zip(confined)
            .any(|(arg, wanted)| arg != wanted)
    {
        return Err(invalid());
    }
    let program = &argv[confined.len()..];
    let first = &program[0];
    if first.is_empty() || first.starts_with('-') || first.contains('=') {
        return Err(invalid());
    }
    let mut line = format!(
        "new-window -P -F '{PLACED}{FORMAT_END}' -n {agent} -c {} -t {SESSION}: -- {PANE_PROGRAM}",
        quote(cwd)
    );
    for (name, value) in pane_env {
        let plain = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit());
        if !plain || value.contains('\0') {
            return Err(invalid());
        }
        line.push(' ');
        line.push_str(&quote(&format!("{name}={value}")));
    }
    for arg in program {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    if line.len() > COMMAND_BYTES_MAX {
        return Err(invalid());
    }
    Ok(line)
}

/// The most pane history rows an attach sends ahead of the screen: all that tmux keeps.
const HISTORY_ROWS_MAX: usize = SCROLLBACK_LINES as usize;
const BRACKETED_PASTE_ON: &[u8] = b"\x1b[?2004h";
const CURSOR_KEYS_ON: &[u8] = b"\x1b[?1h";
const ALTERNATE_SCREEN_ON: &[u8] = b"\x1b[?1049h";
const MOUSE_STANDARD_ON: &[u8] = b"\x1b[?1000h";
const MOUSE_BUTTON_ON: &[u8] = b"\x1b[?1002h";
const MOUSE_ALL_ON: &[u8] = b"\x1b[?1003h";
const MOUSE_UTF8_ON: &[u8] = b"\x1b[?1005h";
const MOUSE_SGR_ON: &[u8] = b"\x1b[?1006h";

/// The pane's modes a new viewer must take on so it shows the screen and sends keys, pastes
/// and mouse reports as a terminal attached from the start would. An app drawing on its
/// alternate screen with mouse reporting on, such as a full-screen agent that keeps its own
/// scrollback, then gets the wheel, instead of the view scrolling history the app no longer
/// shows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Modes {
    bracketed_paste: bool,
    cursor_keys: bool,
    alternate_screen: bool,
    mouse_standard: bool,
    mouse_button: bool,
    mouse_all: bool,
    mouse_utf8: bool,
    mouse_sgr: bool,
}

fn render(screen: &[Vec<u8>], cols: u16, rows: u16, cursor: (u16, u16), modes: Modes) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (on, sequence) in [
        (modes.alternate_screen, ALTERNATE_SCREEN_ON),
        (modes.bracketed_paste, BRACKETED_PASTE_ON),
        (modes.cursor_keys, CURSOR_KEYS_ON),
        (modes.mouse_standard, MOUSE_STANDARD_ON),
        (modes.mouse_button, MOUSE_BUTTON_ON),
        (modes.mouse_all, MOUSE_ALL_ON),
        (modes.mouse_utf8, MOUSE_UTF8_ON),
        (modes.mouse_sgr, MOUSE_SGR_ON),
    ] {
        if on {
            bytes.extend_from_slice(sequence);
        }
    }
    bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
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

/// Puts the pane's history rows into a fresh view's scrollback before its screen is drawn:
/// each row is written from the top, then `rows - 1` more line feeds scroll every one of
/// them off a view of `rows` rows. Inputs: the history rows, oldest first, and the view
/// height. Output: the bytes, or none for no history.
fn scrollback(history: &[Vec<u8>], rows: u16) -> Vec<u8> {
    if history.is_empty() {
        return Vec::new();
    }
    let mut bytes = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    for line in history {
        bytes.extend_from_slice(line);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\x1b[0m");
    bytes.extend(std::iter::repeat_n(
        b'\n',
        usize::from(rows.saturating_sub(1)),
    ));
    bytes
}

struct Located {
    pane: Vec<u8>,
    window: Vec<u8>,
    cols: u16,
    rows: u16,
    cursor: (u16, u16),
    modes: Modes,
    history: usize,
}

/// Takes one reply line of a format ending with `FORMAT_END`. Returns the line without the
/// mark, or none when tmux cut its expansion short.
fn whole(line: &[u8]) -> Option<&[u8]> {
    line.strip_suffix(FORMAT_END.as_bytes())
}

/// Reads one `PANE_FIELDS` reply line: the pane id, its first process id and whether that
/// process ended. Takes the line without its end mark. Returns the pane, or none for a line
/// of another shape.
fn pane_info(line: &[u8]) -> Option<PaneInfo> {
    let text = std::str::from_utf8(line).ok()?;
    let mut fields = text.split(' ');
    let (Some(id), Some(pid), Some(dead), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid > 0)?;
    let dead = match dead {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    pane_id_valid(id).then(|| PaneInfo {
        id: id.to_owned(),
        pid,
        dead,
    })
}

impl Tmux {
    /// Closes panes with one tmux command, then ends what they leave in their process groups
    /// (FINDING bridge orphan): tmux only hangs up a closing pane, which a busy Codex
    /// app-server the tools-on bridge started in the background outlives, still working on
    /// the user's account. `kill`, `kill_pane` and the server's close go through here. Takes
    /// the control, the open panes the command closes and the command. Returns the command's
    /// answer at once; a reaper ends the groups (r1 SG-L2).
    fn close_panes(
        &self,
        control: &Control,
        panes: &[PaneInfo],
        line: &str,
    ) -> Result<(), HostError> {
        let groups = pane_groups(&self.served(panes));
        let closed = control.run(line).map(drop);
        self.reap(groups);
        closed
    }

    /// Ends groups on a thread of their own, so a caller such as the team tick never waits
    /// out the hangup grace and stops in a row share one grace; `close` waits for every
    /// reaper. Past `REAPERS_MAX`, or when no thread starts, the caller waits instead.
    /// Takes the holds on the groups, which the reaper gets once its thread runs.
    fn reap(&self, groups: Vec<Ending>) {
        if groups.is_empty() {
            return;
        }
        let Ok(mut reapers) = self.reapers.lock() else {
            end_groups(groups);
            return;
        };
        reapers.retain(|reaper| !reaper.is_finished());
        if reapers.len() >= REAPERS_MAX {
            let _ = reapers.remove(0).join();
        }
        let (hand, take) = std::sync::mpsc::channel::<Vec<Ending>>();
        match std::thread::Builder::new()
            .name("tmux-reaper".into())
            .spawn(move || {
                if let Ok(groups) = take.recv() {
                    end_groups(groups);
                }
            }) {
            Ok(reaper) => {
                reapers.push(reaper);
                if let Err(std::sync::mpsc::SendError(groups)) = hand.send(groups) {
                    end_groups(groups);
                }
            }
            Err(_) => end_groups(groups),
        }
        assert!(reapers.len() <= REAPERS_MAX);
    }

    /// Waits until every reaper this host started has ended its groups. Takes the host.
    fn join_reapers(&self) {
        let reapers = self
            .reapers
            .lock()
            .map(|mut reapers| std::mem::take(&mut *reapers))
            .unwrap_or_default();
        for reaper in reapers {
            let _ = reaper.join();
        }
    }

    /// Ends every terminal not in `keep` and, with none kept, the server itself, unless the
    /// listing found it ended, as a lead's own server its keeper ended; `close` runs it and
    /// then waits for the reapers. Takes the names to keep. Returns the first failure.
    fn end_server(&self, keep: &[AgentId]) -> Result<(), HostError> {
        if self.executable.is_none() || !self.running() {
            return Ok(());
        }
        end_except(self, keep)?;
        if !keep.is_empty() || !self.running() {
            return Ok(());
        }
        let control = self.control_with(self.confined.is_none())?;
        let panes: Vec<PaneInfo> = control
            .ask(&format!("list-panes -a -F '{PANE_FIELDS}{FORMAT_END}'"))
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| pane_info(line))
                    .take(PANES_LISTED_MAX)
                    .collect()
            })
            .unwrap_or_default();
        let _ = self.close_panes(&control, &panes, "kill-server");
        control.close();
        if let Ok(mut slot) = self.control.lock() {
            *slot = None;
        }
        self.forget_server()
    }
}

/// The process groups of open panes, read before they close: tmux starts each pane's first
/// process as the leader of its own session and group, where the processes it starts in the
/// background stay. A pane whose first process ended or leads no group of its own gives
/// none, so a stop never signals a group it did not read from its pane. A group another
/// path holds, as the session stop that ends this pane, is left to that path, so one group
/// is signalled from one path only (CB-2 one signal). Takes the panes. Returns the holds on
/// their groups.
fn pane_groups(panes: &[PaneInfo]) -> Vec<Ending> {
    assert!(panes.len() <= PANES_LISTED_MAX);
    let own = rustix::process::getpgrp();
    panes
        .iter()
        .filter(|pane| !pane.dead)
        .filter_map(|pane| {
            let leader = rustix::process::Pid::from_raw(i32::try_from(pane.pid).ok()?)?;
            let leads = leader.as_raw_nonzero().get() > 1
                && leader != own
                && rustix::process::getpgid(Some(leader)) == Ok(leader);
            if leads { Ending::claim(pane.pid) } else { None }
        })
        .collect()
}

/// Waits for the groups of closed panes to end on the hangup, for at most `HANGUP_GRACE`,
/// then kills what is left until no group answers, for at most `GROUP_KILL_WAIT`. macOS
/// answers EPERM for a group whose last member it just reaped, so only ESRCH ends a group,
/// and a group that answered ESRCH once is never tested or signalled again: its number may
/// lead a new group by then (r1 SG-L1); its hold is marked gone and dropped then, so the
/// number stays refused to later paths (CB-2 r1 F1). Takes the holds `pane_groups` took
/// before their panes closed.
fn end_groups(groups: Vec<Ending>) {
    end_groups_with(
        groups,
        |group| rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH),
        |group| {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        },
    );
}

/// Ends the groups as `end_groups` describes, through `left`, which tells whether a group
/// still answers, and `kill`, which sends it KILL. Takes the holds and both calls.
fn end_groups_with(
    groups: Vec<Ending>,
    mut left: impl FnMut(rustix::process::Pid) -> bool,
    mut kill: impl FnMut(rustix::process::Pid),
) {
    let mut groups = groups;
    let hung_up = Instant::now() + HANGUP_GRACE;
    loop {
        keep_left(&mut groups, &mut left);
        if groups.is_empty() || Instant::now() >= hung_up {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let killed = Instant::now() + GROUP_KILL_WAIT;
    while !groups.is_empty() && Instant::now() < killed {
        for group in &groups {
            kill(group.pid());
        }
        std::thread::sleep(Duration::from_millis(10));
        keep_left(&mut groups, &mut left);
    }
}

/// Keeps the holds whose group still answers `left` and marks every other one gone, so
/// its number stays refused to later paths once dropped (CB-2 r1 F1). Takes the holds and
/// the test.
fn keep_left(groups: &mut Vec<Ending>, left: &mut impl FnMut(rustix::process::Pid) -> bool) {
    groups.retain_mut(|group| {
        let answers = left(group.pid());
        if !answers {
            group.gone();
        }
        answers
    });
}

/// Reads an attach's pane line: its ids, cursor, size, input modes, history size, and its
/// screen and mouse modes. A screen or mouse flag that an older tmux leaves empty reads as
/// off. Takes the reply lines. Returns the pane, or none for a reply of another shape.
fn located(lines: &[Vec<u8>]) -> Option<Located> {
    let text = std::str::from_utf8(whole(lines.first()?)?).ok()?;
    let fields: Vec<&str> = text.split(' ').collect();
    let [
        pane,
        window,
        x,
        y,
        cols,
        rows,
        paste,
        keys,
        history,
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
        rows: number(rows)?,
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
        history: history.parse::<usize>().ok()?.min(HISTORY_ROWS_MAX),
    })
}

fn viewer(
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
        if state.viewers.len() >= VIEWERS_MAX {
            return Err(failure("terminal_viewers_full"));
        }
        state.viewers.push(Viewer { pane, window, feed });
        assert!(state.viewers.len() <= VIEWERS_MAX);
    }
    Ok(TerminalStream {
        agent,
        cols,
        rows,
        history: scrollback(earlier, rows),
        screen: render(screen, cols, rows, cursor, modes),
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
            history,
            view,
            reply,
        } => match answer {
            Ok(lines) => state.pending.push_front(Pending::Attach {
                agent,
                located: Some(lines),
                history,
                view,
                reply,
            }),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        },
        Pending::Attach {
            agent,
            located: Some(lines),
            history,
            view,
            reply,
        } => {
            let opened =
                answer.and_then(|screen| viewer(state, agent, &lines, &screen, history, view));
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

/// How many rows the reply block opening now holds, when it is an attach's capture: the
/// history rows the attach read just before, then the screen rows.
/// capture-pane prints one line per pane row and the pane's own text can look like any
/// control line, including the block's closing guard, so those rows are counted as data
/// rather than searched for a guard. Takes the state. Returns the history and pane height
/// the attach read just before, or none for every other reply.
fn capture_rows(state: &State) -> Option<usize> {
    match state.pending.front()? {
        Pending::Attach {
            located: Some(lines),
            history,
            ..
        } => located(lines).map(|found| found.history.min(*history) + usize::from(found.rows)),
        _ => None,
    }
}

/// One reply block the reader is inside: its opening guard, the lines kept, how many lines
/// and bytes it held so far, the rows an attach's capture holds, and whether it passed
/// `REPLY_BYTES_MAX` or held a line past `LINE_BYTES_MAX`, which fails only its own request.
struct Block {
    opened: Vec<u8>,
    lines: Lines,
    seen: usize,
    bytes: usize,
    rows: Option<usize>,
    oversized: bool,
}

/// Reads one control line without its newline, keeping at most `LINE_BYTES_MAX` bytes of
/// it; the rest of a longer line is read and dropped. Takes the reader and an empty buffer.
/// Returns how many bytes were dropped, or none once the output ends.
fn read_line(output: &mut impl BufRead, line: &mut Vec<u8>) -> Option<usize> {
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

fn read(output: ChildStdout, state: &Mutex<State>) {
    let mut output = BufReader::new(output);
    let mut block: Option<Block> = None;
    loop {
        let mut line = Vec::new();
        let Some(dropped) = read_line(&mut output, &mut line) else {
            break;
        };
        let Ok(mut state) = state.lock() else {
            break;
        };
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
                finish(&mut state, answer);
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
            if let Some(rest) = guard(&line, b"%output") {
                let pane = rest.split(|byte| *byte == b' ').next().unwrap_or_default();
                let lost = u64::try_from(line.len() + dropped).unwrap_or(u64::MAX);
                deliver(&mut state, pane, &skipped_marker(lost));
            }
            continue;
        }
        if let Some(opened) = guard(&line, b"%begin") {
            block = Some(Block {
                opened: opened.to_vec(),
                lines: Vec::new(),
                seen: 0,
                bytes: 0,
                rows: capture_rows(&state),
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

impl Tmux {
    /// Reads an agent terminal's screen and at most `history` rows above it in one tmux
    /// command block, and opens a viewer on its live output when `view` is set.
    /// Takes the name, the history row limit and whether to view. Returns the stream.
    fn capture(
        &self,
        agent: &AgentId,
        pane: Option<&str>,
        history: usize,
        view: bool,
    ) -> Result<TerminalStream, HostError> {
        assert!(history <= HISTORY_ROWS_MAX);
        let control = self.live_control()?;
        let pane = match pane {
            Some(pane) => Some(pane.to_owned()),
            None if self.confined.is_some() => Some(self.agent_target(&control, agent)?),
            None => None,
        };
        let (target, filter) = match &pane {
            Some(pane) => {
                assert!(pane_id_valid(pane));
                (pane.clone(), format!("#{{==:#{{pane_id}},{pane}}}"))
            }
            None => (target(agent), "#{pane_active}".to_owned()),
        };
        let start = if history == 0 {
            "0".to_owned()
        } else {
            format!("-{history}")
        };
        let line = format!(
            "list-panes -t {target} -f '{filter}' -F '#{{pane_id}} #{{window_id}} #{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{pane_height}} #{{bracket_paste_flag}} #{{keypad_cursor_flag}} #{{history_size}} #{{alternate_on}} #{{mouse_standard_flag}} #{{mouse_button_flag}} #{{mouse_all_flag}} #{{mouse_sgr_flag}} #{{mouse_utf8_flag}}{FORMAT_END}' ; capture-pane {} -S {start} -t {target}",
            control.capture
        );
        let until = Instant::now() + REPLY_TIME;
        loop {
            let (reply, answer) = sync_channel(1);
            control.send(
                &line,
                Pending::Attach {
                    agent: agent.clone(),
                    located: None,
                    history,
                    view,
                    reply,
                },
            )?;
            match control.wait(&answer) {
                Err(cut) if cut.0 == REPLY_CUT && Instant::now() < until => {}
                Err(cut) if cut.0 == REPLY_CUT => return Err(failure("terminal_not_answering")),
                answered => return answered,
            }
        }
    }
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
        command: &InteractiveCommand,
        pane_env: &[(String, String)],
    ) -> Result<PaneRef, HostError> {
        let line = match &self.confined {
            Some(profile) => confined_line(agent, command, profile, pane_env)?,
            None if pane_env.is_empty() => spawn_line(agent, command)?,
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
        let lines = control.run(&line)?;
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
        if self.served(&[first]).is_empty() {
            let _ = control.run(&format!("kill-window -t {}", target(agent)));
            return Err(failure("terminal_command_failed"));
        }
        if self.confined.is_some() {
            let tagged = pane_id_valid(pane)
                && control
                    .run(&format!("set-option -p -t {pane} {AGENT_TAG} {agent}"))
                    .is_ok();
            if !tagged {
                let _ = control.run(&format!("kill-window -t {}", target(agent)));
                return Err(failure("terminal_command_failed"));
            }
        }
        Ok(PaneRef {
            agent: agent.clone(),
            pane: pane.to_owned(),
            pid,
        })
    }
}

impl Host for Tmux {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
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
        let lines = control.ask(&format!(
            "display-message -p -t {target} '#{{window_activity}}{FORMAT_END}'"
        ))?;
        let activity = lines
            .first()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .ok_or_else(|| failure("terminal_command_failed"))?;
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

mod servers;

#[cfg(test)]
#[path = "tmux/tests.rs"]
mod tests;
