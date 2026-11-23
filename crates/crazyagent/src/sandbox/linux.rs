use super::ProjectSandbox;
use ::landlock::RulesetStatus;
use crazyagent_linux_supervisor::rules::{GitOwn, Held, is_git};
use kernel_sandbox::error::{SandboxError, invalid};
use kernel_sandbox::hidden_paths::HIDDEN_MAX;
use kernel_sandbox::net::{SandboxNetwork, SandboxRemote};
use kernel_sandbox::paths::clean_absolute;
use kernel_sandbox::ports::BRIDGE_PORT_MIN;
use kernel_sandbox::ports::{OWNER_PORTS, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX};
use kernel_sandbox::profile::command::Sandbox;
use kernel_sandbox::profile::consts::{
    ADMIN_LINKS, ADMIN_WRITES, GIT_FALLBACK, GIT_STATE_FILES, GIT_STATE_TREES,
    LINUX_HARNESS_UNCHECKED, LINUX_LIMITED_NETWORK_OFF, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK,
    LINUX_RESOLVER_NEEDS_UDP, MULTI_PACK_INDEX, STORE_ALTERNATES,
};
use kernel_sandbox::profile::env::{Routed, agent_process_env};
use kernel_sandbox::settings::{LinuxSockets, LinuxSupervisor};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kernel_sandbox::linux::helper::egress::SLIRP_PATHS;
use kernel_sandbox::linux::helper::landlock::*;
use kernel_sandbox::linux::helper::*;

/// The Landlock helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-landlock";

/// The Landlock helper for the engine executable at `engine`: the one beside it. Only in a
/// debug build, an executable whose own folder is cargo's `deps`, as a test binary's is, with
/// no helper beside it takes cargo's build one folder up, the one candidate, and logs it.
/// Input: the executable's absolute path. Output: the helper's path.
pub fn helper_for(engine: &Path) -> PathBuf {
    assert!(engine.is_absolute());
    let beside = engine.with_file_name(HELPER_NAME);
    #[cfg(debug_assertions)]
    if let Some(built) = engine
        .parent()
        .filter(|folder| folder.file_name() == Some(OsStr::new("deps")))
        .map(|deps| deps.with_file_name(HELPER_NAME))
        && !beside.is_file()
        && built.is_file()
    {
        eprintln!("linux_helper_cargo_build: {}", built.display());
        return built;
    }
    beside
}

/// The Landlock helper for this engine, as [`helper_for`] picks it. Output: the path, or the
/// error reading the engine's own executable path.
pub fn helper_beside_engine() -> std::io::Result<PathBuf> {
    Ok(helper_for(&std::env::current_exe()?))
}

const AGENT_PATH: &str =
    "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin:/home/linuxbrew/.linuxbrew/bin";
/// Where AppArmor says whether it keeps unprivileged programs out of user namespaces, as on
/// Ubuntu 23.10 and newer: `1` when it does ([`restricts`]).
const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
/// How long [`host_now`] keeps what it read: a policy view asks on every read, and each
/// answer starts the helper twice.
const HOST_AGE_MAX: Duration = Duration::from_secs(60);
/// The longest the call supervisor probe may run: its one wait is a 2 s connect timeout,
/// reached only when a guard does not hold (rv-sec3 linux-t4b N4).
const SUPERVISOR_PROBE_TIME_MAX: Duration = Duration::from_secs(10);
/// How long the routed-network probe may run: slirp4netns's ready wait and the rules.
const ROUTE_PROBE_TIME_MAX: Duration = Duration::from_secs(15);
/// How the name of each private folder a supervisor probe runs in starts ([`private_folder`]).
const PROBE_FOLDER_PREFIX: &str = "supervisor-probe-";
/// The most names [`private_folder`] tries: each is 128 random bits, so only a name someone
/// else already made, or a broken random source, ever takes one.
const PROBE_FOLDER_TRIES_MAX: usize = 4;
/// How often [`passes_within`] looks whether its probe has ended.
const PROBE_POLL: Duration = Duration::from_millis(20);
const RESOLVER_ROOT: &str = "/run/systemd/resolve";
const HELPER_MISSING: &str = "CrazyAgent's sandbox helper is missing, and no Linux agent \
     starts without it; reinstall CrazyAgent.";
/// How a refusal starts when the probe failed for any reason but the kernel's answer.
const PROBE_FAILED: &str = "the sandbox check failed";
/// The most bytes of the probe's own reason a refusal carries.
const PROBE_REASON_MAX: usize = 200;
/// The most bytes of a failed probe's stderr that are read and logged ([`probe_said`]).
const PROBE_SAID_MAX: usize = 4096;
/// The launch refusal when this computer's nest check passed ([`nest_held`]) but the launch's
/// own nest probe failed (rv-sa241 SBX-2-R2 LOW-1), so no agent runs on the shared network
/// with Landlock rules alone while the Agent access page says it is nested.
const LINUX_NEST_NOT_STARTED: &str = "bubblewrap did not start this agent's sandbox the way \
     this computer's check found it does; try again, or restart CrazyAgent to check this \
     computer again";
const DNS_PORT: u16 = 53;
const DNS_OVER_TCP: &str = "use-vc";
const LIMITED_NETWORK_OFF: &str = "the Linux limited-network sockets setting is 'Do not start \
     limited-network agents'; choose 'TCP only' or 'TCP ports only' in Settings";
const RESOLVER_NEEDS_UDP: &str = "this harness cannot look up names over TCP, which the Linux \
     limited-network sockets setting 'TCP only' needs; choose 'TCP ports only' in Settings, which \
     lets UDP and other sockets reach any host";
const HARNESS_UNCHECKED: &str = "this harness starts through a script CrazyAgent cannot check for \
     the Linux limited-network sockets setting 'TCP only'; start it from a program, or a script \
     whose first line names a program by its full path, or choose 'TCP ports only' in Settings, \
     which lets UDP and other sockets reach any host";
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/opt",
    RESOLVER_ROOT,
    "/home/linuxbrew/.linuxbrew",
    "/nix/store",
    "/sys/devices/system/cpu",
    "/sys/fs/cgroup",
];

/// The entries of a project's `.git` that the owner's and the engine's git follow to other
/// repository data, which the Seatbelt profile keeps read-only: no agent writes or plants them.
/// `objects` is granted entry by entry instead, without its `info` folder.
const GIT_FOLLOWED: &[&str] = &[
    "commondir",
    "config.worktree",
    "info",
    "objects",
    "shallow",
    "worktrees",
];

const CONFINED_UNAVAILABLE: &str = "an agent confined to its own worktree cannot run on Linux yet: \
     neither Landlock nor bubblewrap can keep the worktree's git links and the existing git \
     objects read-only inside the git folders it must write";

const CONFINED_NEEDS_LANDLOCK: &str = "an agent confined to its own worktree or clone needs \
     Landlock file rules on Linux (Linux 6.2 or newer), and this Linux runs agents under \
     bubblewrap alone, which cannot give the git folders their own rights";

/// Why an agent whose project hides a folder does not start under Landlock alone: a right
/// to list a folder covers every folder below it, so the hidden folder's names would stay
/// in view. It names what makes hidden folders work on this computer (leader 12:25).
pub const HIDDEN_NEEDS_NAMESPACES: &str = "hidden folders on Linux need bubblewrap to start \
     agents in namespaces of their own, and this computer does not let it: install \
     bubblewrap and allow unprivileged user namespaces, then start the agent again";

/// Why an agent whose project hides a folder does not start while this computer mounts that
/// folder's disk a second time where it shows the folder: the empty cover lies on one path,
/// and the second one leads past it (rv-code4 F1, #622, farm run 44410206144).
pub const HIDDEN_MOUNTED_AGAIN: &str = "a hidden folder's disk is mounted a second time where \
     it shows that folder, and an agent could reach the folder there: remove that mount or \
     stop hiding the folder, then start the agent again";

/// Why an agent whose project hides a folder does not start when this computer's mount list
/// cannot be read plainly: no second mount of the folder's disk can then be ruled out.
const HIDDEN_MOUNTS_UNREAD: &str = "hidden folders on Linux need this computer's mount list, \
     and it could not be read";

const LANDLOCK_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/shm",
];

/// The Landlock rules of one engine git step on an agent's linked worktree, the Linux form
/// of the macOS step profile (SECAUDIT unconfined-git-add-reads-through-raced-symlink,
/// unconfined-read-tree-u): it reads the system, `/proc` and `read`, and writes the devices
/// and `write`, with no network rule and no kept port, and dies with this engine process, so
/// its git never outlives the lock the engine took for it (RV-DS241-142); an engine that is
/// process 1 takes every process down with it. Inputs: the absolute places the step reads
/// and writes. Output: the policy.
pub(crate) fn git_step_policy(read: &[PathBuf], write: &[PathBuf]) -> LandlockPolicy {
    assert!(read.iter().chain(write).all(|path| path.is_absolute()));
    let policy = LandlockPolicy {
        read: SYSTEM_READ_ROOTS
            .iter()
            .chain(&["/proc"])
            .map(PathBuf::from)
            .chain(read.iter().cloned())
            .collect(),
        write: LANDLOCK_DEVICES
            .iter()
            .map(PathBuf::from)
            .chain(write.iter().cloned())
            .collect(),
        die_with: Some(std::process::id()).filter(|engine| *engine > 1),
        ..LandlockPolicy::default()
    };
    assert!(policy.read.len() + policy.write.len() + policy.git_write.len() <= HELPER_PATHS_MAX);
    assert!(!policy.limit_network && policy.keep_free.is_empty());
    policy
}

/// Tells whether this kernel holds the helper's file rules, as a landing git step needs
/// them: the helper's probe with no network flag. Input: the helper. Output: true only when
/// it is an absolute file and its probe exits 0.
pub(crate) fn holds_file_rules(helper: &Path) -> bool {
    clean_absolute(helper)
        && helper.is_file()
        && kernel_processes::spawn_gate::status(
            Command::new(helper)
                .arg(PROBE)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|status| status.success())
}

/// The Linux mechanism holding a project agent's boundary: the `crazyagent-landlock` helper,
/// that helper inside bubblewrap's user and process namespaces where this machine makes
/// them (`Nested`), or bubblewrap where the kernel cannot fully enforce its Landlock rules,
/// with the helper started inside it to install the socket filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxBackend {
    Landlock { helper: PathBuf },
    Nested { bwrap: PathBuf, helper: PathBuf },
    Bubblewrap { bwrap: PathBuf, helper: PathBuf },
}

/// The Linux boundary for one project agent. It keeps the [`ProjectSandbox`] policy on both
/// backends: the project folder is read-only, its existing entries are writable except the
/// protected paths, symbolic links, the git metadata git follows (`GIT_FOLLOWED`) and the
/// git state the Seatbelt profile denies ([`GIT_STATE_TREES`], [`GIT_STATE_FILES`]), and
/// the agent's home, temp dir and worktrees are writable. A policy confined to one worktree
/// is refused, since neither backend can hold its git denies. Landlock limits outbound TCP to the model endpoint ports
/// and, under the owner's [`LinuxSockets`] setting, the socket filter refuses every other kind
/// of network socket or no limited-network agent starts; bubblewrap cannot limit ports, so a
/// tools-off agent needs Landlock. An open network needs Landlock as well: its bind rules keep
/// the agent's listeners off the owner's fixed ports, the kept local model ports and the
/// model proxy ports (`kept_free`), which bubblewrap cannot do.
#[derive(Debug)]
pub struct LinuxSandbox {
    sandbox: ProjectSandbox,
    backend: LinuxBackend,
    supervision: Option<Supervision>,
    routed: Option<Routed>,
}

/// What the call supervisor holds for a launch on this host beyond the guards every
/// supervised launch carries: `signals` where neither a process namespace nor the
/// kernel's signal scope keeps the agent's signals to itself (DEC-179 T4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Supervision {
    signals: bool,
}

impl LinuxSandbox {
    /// Binds the policy to one backend. Input: the policy and the backend's absolute files. Output:
    /// the sandbox, `Invalid` for a bad executable, or `Unavailable` on bubblewrap for any network
    /// but a closed one with no host port, for a one-worktree policy, a limited network the sockets
    /// setting refuses, off bubblewrap a kept port Linux could hand out ([`kept_outside_picked`]),
    /// on Landlock a hidden folder, or hidden folders with no private mount tree ([`cuts`]).
    pub fn new(sandbox: ProjectSandbox, backend: LinuxBackend) -> Result<Self, SandboxError> {
        admitted(&sandbox)?;
        if matches!(backend, LinuxBackend::Landlock { .. }) && !sandbox.hidden.is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_NAMESPACES.into()));
        }
        mounts_hold(&sandbox)?;
        let (launcher, inside) = match &backend {
            LinuxBackend::Landlock { helper } => (helper, None),
            LinuxBackend::Nested { bwrap, helper } => (helper, Some(bwrap)),
            LinuxBackend::Bubblewrap { bwrap, helper } => (bwrap, Some(helper)),
        };
        if std::iter::once(launcher)
            .chain(inside)
            .any(|executable| !clean_absolute(executable) || !executable.is_file())
        {
            return Err(invalid("sandbox executable must be an absolute file"));
        }
        if let LinuxBackend::Nested { bwrap, .. } = &backend
            && !trusted(bwrap)
        {
            return Err(invalid(
                "bubblewrap must have no set-ID bit and belong to root or this user",
            ));
        }
        if matches!(backend, LinuxBackend::Bubblewrap { .. }) {
            match sandbox.network() {
                SandboxNetwork::Open => {
                    return Err(SandboxError::Unavailable(
                        LINUX_OPEN_NETWORK_NEEDS_LANDLOCK.into(),
                    ));
                }
                SandboxNetwork::Only(remotes) if !remotes.is_empty() => {
                    return Err(SandboxError::Unavailable(
                        "limiting an agent's network to a few ports needs Landlock network \
                         rules (Linux 6.7 or newer); bubblewrap cannot limit ports"
                            .into(),
                    ));
                }
                SandboxNetwork::Only(_) => {}
            }
        } else {
            kept_outside_picked(&kept_free(&sandbox), read_picked_ports().as_ref())
                .map_err(SandboxError::Unavailable)?;
        }
        if !sandbox.hidden.is_empty() && !cuts(&backend) {
            return Err(SandboxError::Unavailable(HIDDEN_MOUNTS_NOT_PRIVATE.into()));
        }
        Ok(Self {
            sandbox,
            backend,
            supervision: None,
            routed: None,
        })
    }

    /// Puts this launch under the call supervisor when the owner's setting is On and the
    /// probe in the engine's data folder holds ([`supervises`]); bubblewrap alone never runs
    /// it. Off keeps the launch as it was; so does a failed probe, which says so on stderr and
    /// marks the kept host unsupervised ([`forget_supervisor`]). Inputs: the choice and the
    /// data folder, none where no probe may run. Output: the sandbox.
    pub fn supervise(mut self, calls: LinuxSupervisor, data: Option<&Path>) -> Self {
        assert!(self.supervision.is_none());
        if calls == LinuxSupervisor::Off {
            return self;
        }
        let root = rustix::process::geteuid().is_root();
        let holds = |helper: &PathBuf, bwrap: Option<&Path>| {
            data.is_some_and(|data| supervises(helper, bwrap, data, root))
        };
        let supervision = match &self.backend {
            LinuxBackend::Landlock { helper } if holds(helper, None) => {
                let scoped = passes(Command::new(helper).arg(SCOPED));
                Some(Supervision { signals: !scoped })
            }
            LinuxBackend::Nested { bwrap, helper } if holds(helper, Some(bwrap.as_path())) => {
                Some(Supervision { signals: false })
            }
            LinuxBackend::Landlock { .. } | LinuxBackend::Nested { .. } => {
                eprintln!("linux_supervisor_not_started: this launch runs unsupervised");
                forget_supervisor(&HOST_READ);
                None
            }
            LinuxBackend::Bubblewrap { .. } => None,
        };
        self.supervision = supervision;
        self
    }

    /// The policy with the guards this launch's supervision carries, none without one,
    /// checked as the helper checks it ([`guard_fits`]). A nested launch never carries the
    /// signal guard. Inputs: the policy and the places its supervisor holds (DEC-185 S2).
    /// Output: the policy, or `Invalid` for more own git folders or held places than the
    /// helper takes.
    fn with_guard(
        &self,
        policy: LandlockPolicy,
        held: &Held,
    ) -> Result<LandlockPolicy, SandboxError> {
        let Some(supervision) = self.supervision else {
            return Ok(policy);
        };
        let guarded = LandlockPolicy {
            guard: Guard {
                held: Some(held.clone()).filter(|held| !held.is_empty()),
                ..guard(&self.sandbox, supervision)
            },
            ..policy
        };
        assert!(!guarded.nested || !guarded.guard.signals);
        assert!(guarded.guard != Guard::default());
        guard_fits(&guarded).map_err(|reason| invalid(&reason))?;
        Ok(guarded)
    }

    /// Picks the backend this machine holds: Landlock when the helper exists and its probe
    /// passes for the policy's network needs, nested in bubblewrap where this machine nests
    /// ([`nest_held`], the answer the Agent access page reads) and the same probe passes
    /// inside its namespaces ([`nests`]), otherwise bubblewrap from a fixed path with the
    /// helper started inside it, so no bubblewrap agent starts without the helper. An open
    /// network probes the TCP bind rules that keep ports free and never falls back to
    /// bubblewrap. Input: the project policy and the helper executable. Output: the
    /// sandbox, or `Unavailable` when no backend can hold it or the nest does not start.
    pub fn detect(sandbox: ProjectSandbox, helper: &Path) -> Result<Self, SandboxError> {
        let bwraps: Vec<&Path> = BWRAP_PATHS.iter().map(Path::new).collect();
        Self::detect_among(sandbox, helper, &bwraps)
    }

    /// Picks the backend as [`LinuxSandbox::detect`] does, with bubblewrap looked for only
    /// at `bwraps`, so a caller with none pins the plain Landlock backend. Inputs: the
    /// project policy, the helper executable and the bubblewrap places, in order. Output:
    /// the sandbox, or `Unavailable` naming a missing helper, the kernel's answer or the
    /// probe's own reason ([`probe_refusal`]) when no backend can hold it.
    pub fn detect_among(
        sandbox: ProjectSandbox,
        helper: &Path,
        bwraps: &[&Path],
    ) -> Result<Self, SandboxError> {
        admitted(&sandbox)?;
        if !clean_absolute(helper) || !helper.is_file() {
            eprintln!("linux_helper_missing: expected at {}", helper.display());
            return Err(SandboxError::Unavailable(HELPER_MISSING.into()));
        }
        let mut probe = Command::new(helper);
        probe.arg(PROBE);
        let open = matches!(sandbox.network(), SandboxNetwork::Open);
        if open {
            probe.arg(KEEP_FREE).arg(OWNER_PORTS[0].to_string());
        } else {
            probe.arg(LIMIT_NETWORK);
            if sandbox.linux_sockets() == LinuxSockets::TcpPorts {
                probe.arg(OPEN_SOCKETS);
            }
        }
        let flags: Vec<OsString> = probe.get_args().map(OsStr::to_os_string).collect();
        let failed = probe_failure(&mut probe, &mut std::io::stderr());
        if failed.is_none() {
            let helper = helper.to_path_buf();
            let nest = bwraps
                .iter()
                .find(|bwrap| bwrap.is_file())
                .filter(|bwrap| nest_held(bwrap, &helper));
            if let Some(bwrap) = nest
                && !nests(bwrap, &helper, &flags)
            {
                eprintln!(
                    "linux_nest_not_started: this agent is refused, not moved to Landlock alone"
                );
                return Err(SandboxError::Unavailable(LINUX_NEST_NOT_STARTED.into()));
            }
            let backend = match nest {
                Some(bwrap) => LinuxBackend::Nested {
                    bwrap: bwrap.to_path_buf(),
                    helper,
                },
                None => LinuxBackend::Landlock { helper },
            };
            let mut detected = Self::new(sandbox, backend)?;
            if let LinuxBackend::Nested { bwrap, helper } = &detected.backend
                && let Some(routed) = detected.sandbox.routed()
            {
                if route_held(bwrap, helper, false) {
                    detected.routed = Some(routed);
                } else {
                    eprintln!("linux_route_not_started: this agent keeps the shared network");
                }
            }
            assert!(detected.routed.is_none() || detected.sandbox.bridged_ports().is_none());
            return Ok(detected);
        }
        let named = failed.and_then(|reason| probe_refusal(&reason));
        if open {
            let reason = named.unwrap_or_else(|| LINUX_OPEN_NETWORK_NEEDS_LANDLOCK.into());
            return Err(SandboxError::Unavailable(reason));
        }
        let limited =
            matches!(sandbox.network(), SandboxNetwork::Only(remotes) if !remotes.is_empty());
        if let Some(reason) = named.as_ref().filter(|_| limited) {
            return Err(SandboxError::Unavailable(reason.clone()));
        }
        match bwraps.iter().find(|path| path.is_file()) {
            Some(bwrap) => Self::new(
                sandbox,
                LinuxBackend::Bubblewrap {
                    bwrap: bwrap.to_path_buf(),
                    helper: helper.to_path_buf(),
                },
            ),
            None => Err(SandboxError::Unavailable(named.unwrap_or_else(|| {
                "this Linux cannot fully enforce Landlock file rules (Linux 6.2 or newer) and \
                 has no bubblewrap; install bubblewrap"
                    .into()
            }))),
        }
    }

    /// The backend this sandbox launches through.
    pub fn backend(&self) -> &LinuxBackend {
        &self.backend
    }

    /// Whether stopping a run in this sandbox ends every process it started, those in a
    /// session of their own included: bubblewrap's process namespace ends with its outer
    /// process ([`NEST_ARGS`]), while under the plain Landlock helper such a process leaves
    /// the group a stop kills and outlives the run (LINUX-KILL-GROUP). Output: the answer.
    pub fn ends_every_session(&self) -> bool {
        !matches!(self.backend, LinuxBackend::Landlock { .. })
    }

    /// The project policy this sandbox holds.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The network of its own this sandbox gives its agent, as detection found this machine
    /// holds it ([`ProjectSandbox::routed`]); none when the agent keeps the shared network.
    pub fn routed(&self) -> Option<&Routed> {
        self.routed.as_ref()
    }

    /// Tells whether this launch holds the full Linux sandbox ([`super::linux_holds_fully`]):
    /// the nested backend, the call supervisor this launch starts, and its network limits
    /// held, a port on any host through the network of its own detection found. No input.
    /// Output: the answer.
    pub fn holds_fully(&self) -> bool {
        let nested = matches!(self.backend, LinuxBackend::Nested { .. });
        assert!(self.routed.is_none() || nested);
        let supervised = self.supervision.is_some();
        super::linux_holds_fully(nested, supervised, self.routed.is_some(), &self.sandbox)
    }

    /// The program every wrapped command starts: the Landlock helper or bwrap.
    pub fn launcher(&self) -> &Path {
        match &self.backend {
            LinuxBackend::Landlock { helper } | LinuxBackend::Nested { helper, .. } => helper,
            LinuxBackend::Bubblewrap { bwrap, .. } => bwrap,
        }
    }
}

/// Runs a probe as [`passes`] does, but ends it once `time_max` has gone by; the harness
/// the call supervisor probe starts dies with it. Inputs: the probe and its longest run.
/// Output: true only when it exited 0 in time.
fn passes_within(probe: &mut Command, time_max: Duration) -> bool {
    let started = Instant::now();
    let quiet = probe
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = kernel_processes::spawn_gate::spawn(quiet) else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < time_max => std::thread::sleep(PROBE_POLL),
            Ok(None) | Err(_) => break,
        }
    }
    if child.kill().is_ok() {
        let reaped = child.wait();
        assert!(
            reaped.is_ok(),
            "a killed probe is this process's child to reap"
        );
    }
    false
}

/// Makes a folder only this user may open under a random name in `data`, the engine's data
/// folder, for one supervisor probe, so no stale, concurrent or planted folder takes its
/// name (rv-secreview linux-t4c F1). Input: the absolute data folder. Output: the new empty
/// folder, or none when no name was free or the data folder refused it.
fn private_folder(data: &Path) -> Option<PathBuf> {
    assert!(data.is_absolute());
    for _ in 0..PROBE_FOLDER_TRIES_MAX {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(name.len(), 32);
        let folder = data.join(format!("{PROBE_FOLDER_PREFIX}{name}"));
        match fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => return Some(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Tells whether the call supervisor holds every guard here as a launch starts it: the
/// helper's probe ([`probe_supervisor`]), plain or inside bubblewrap's namespaces, in a new
/// private folder under the engine's data folder ([`private_folder`]), removed after. Never
/// as root (rv-sec3 linux-t4b N1). Inputs: the helper, the bubblewrap launches nest under,
/// the data folder and whether this runs as root. Output: true only when the probe held.
fn supervises(helper: &Path, bwrap: Option<&Path>, data: &Path, root: bool) -> bool {
    if root || !(clean_absolute(helper) && helper.is_file() && clean_absolute(data)) {
        return false;
    }
    let mut probe = Command::new(helper);
    match bwrap {
        None => {}
        Some(bwrap) if clean_absolute(bwrap) && trusted(bwrap) => {
            probe.arg(NEST).arg("--").arg(bwrap);
            probe.args(NEST_ARGS).arg("--").arg(helper);
        }
        Some(_) => return false,
    }
    let Some(folder) = private_folder(data) else {
        return false;
    };
    assert!(folder.starts_with(data));
    probe.arg(PROBE_SUPERVISOR).arg(&folder);
    let held = private_empty(&folder) && passes_within(&mut probe, SUPERVISOR_PROBE_TIME_MAX);
    if let Err(error) = fs::remove_dir_all(&folder) {
        eprintln!(
            "linux_supervisor_probe_folder_kept {}: {error}",
            folder.display()
        );
    }
    held
}

/// Runs a probe ([`probe_said`]) and, when it failed, writes what it said to `log` as one
/// line ([`probe_log`]). A log that cannot take the line is passed over, since the refusal
/// still names the reason. Inputs: the probe command and the log, the engine's stderr.
/// Output: none when it ran and exited 0, else its reason ([`probe_reason`]).
fn probe_failure(probe: &mut Command, log: &mut impl Write) -> Option<String> {
    let (said, ended) = probe_said(probe)?;
    let flags: Vec<&OsStr> = probe.get_args().collect();
    let _ = writeln!(log, "{}", probe_log(&flags, &ended, &said));
    Some(probe_reason(&said, &ended))
}

/// Runs a probe with no environment and no input, keeping at most [`PROBE_SAID_MAX`] bytes of
/// what it says on stderr. The pipe closes before the wait, so a probe that says more gets
/// `EPIPE`, or `SIGPIPE` where it keeps that signal's default, and ends rather than blocking.
/// Input: the probe command. Output: none when it ran and exited 0, else its stderr and how
/// it ended.
fn probe_said(probe: &mut Command) -> Option<(String, String)> {
    let started = kernel_processes::spawn_gate::spawn(
        probe
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let mut child = match started {
        Ok(child) => child,
        Err(error) => return Some((String::new(), error.to_string())),
    };
    let mut bytes = Vec::new();
    let read = child
        .stderr
        .take()
        .map(|pipe| pipe.take(PROBE_SAID_MAX as u64).read_to_end(&mut bytes));
    let status = match child.wait() {
        Ok(status) if status.success() => return None,
        Ok(status) => status.to_string(),
        Err(error) => error.to_string(),
    };
    let ended = match read {
        Some(Err(error)) => format!("{status}, stderr unread: {error}"),
        Some(Ok(_)) | None => status,
    };
    let text = String::from_utf8_lossy(&bytes);
    let said = text[..text.floor_char_boundary(PROBE_SAID_MAX)].to_owned();
    assert!(said.len() <= PROBE_SAID_MAX);
    Some((said, ended))
}

/// The log line for a failed probe, naming its flags, how it ended and what it said on
/// stderr, which is quoted with its control characters escaped so it stays one line. Inputs:
/// the flags, how it ended and what it said. Output: the line, without its newline.
fn probe_log(flags: &[&OsStr], ended: &str, said: &str) -> String {
    format!("linux_probe_failed: {flags:?} {ended}: {said:?}")
}

/// The reason a refusal carries for a failed probe: the first line it said, or how it ended
/// when that line is blank, with control characters dropped and cut to [`PROBE_REASON_MAX`]
/// bytes on a character boundary. Inputs: its stderr and how it ended. Output: the reason.
fn probe_reason(said: &str, ended: &str) -> String {
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let first = clean(said.lines().next().unwrap_or_default());
    let source = if first.trim().is_empty() {
        clean(ended)
    } else {
        first
    };
    let mut cut = PROBE_REASON_MAX.min(source.len());
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    source[..cut].to_owned()
}

/// The refusal a failed probe names, unless the probe gave the kernel's answer that it holds
/// the ruleset only in part or not at all, for which the kernel refusals stay. Input: the
/// probe's reason ([`probe_reason`]). Output: the refusal, or none for the kernel's answer.
fn probe_refusal(reason: &str) -> Option<String> {
    let kernel = [RulesetStatus::PartiallyEnforced, RulesetStatus::NotEnforced]
        .iter()
        .any(|status| reason == format!("crazyagent-landlock: {HOLD_REFUSED} ({status:?})"));
    (!kernel).then(|| format!("{PROBE_FAILED}: {reason}"))
}

/// Runs a probe with no environment and no input or output. Input: the probe command.
/// Output: true only when it ran and exited 0.
fn passes(probe: &mut Command) -> bool {
    kernel_processes::spawn_gate::status(
        probe
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
}

/// Tells whether the helper gets a mount tree of its own where this backend starts it, by
/// starting it there with a private tree and no cover but `/proc` ([`cut`]): the nested
/// probe ([`nests`]), or bubblewrap's closed box with the socket filter before `/bin/true`.
/// Input: the backend. Output: true only when that start exits 0; never on plain Landlock.
fn cuts(backend: &LinuxBackend) -> bool {
    match backend {
        LinuxBackend::Landlock { .. } => false,
        LinuxBackend::Nested { bwrap, helper } => {
            nests(bwrap, helper, &[PROBE.into(), PRIVATE_MOUNTS.into()])
        }
        LinuxBackend::Bubblewrap { bwrap, helper } => {
            let bare = Grants {
                read: Vec::new(),
                write: Vec::new(),
                git_write: Vec::new(),
                guarded: Held::default(),
                hidden: Vec::new(),
            };
            let closed = SandboxNetwork::Only(Vec::new());
            let mut probe = bwrap_command(bwrap, helper, &bare, Path::new("/"), &closed);
            probe.args([PRIVATE_MOUNTS, "--", "/bin/true"]);
            passes(&mut probe)
        }
    }
}

/// Tells whether a bubblewrap is one the helper may nest an agent under ([`trusted_file`]).
/// A host whose bubblewrap fails this stays on plain Landlock with its gap lines. Input:
/// its path. Output: true when it may.
fn trusted(bwrap: &Path) -> bool {
    trusted_file(bwrap).is_some()
}

/// Tells whether this machine starts the helper inside bubblewrap's namespaces and holds
/// the same probe there, by running the widest nest a launch asks for: the helper,
/// bubblewrap with [`NEST_ARGS`] and a network of its own with one port bridged, then the
/// helper's probe. A bubblewrap that only exists proves nothing: Ubuntu 24.04 refuses it
/// the user namespace. A bubblewrap [`trusted`] refuses is never started. Inputs:
/// bubblewrap, the helper and the probe's arguments. Output: true only when the nested
/// probe exits 0.
fn nests(bwrap: &Path, helper: &Path, flags: &[OsString]) -> bool {
    assert!(flags.first().is_some_and(|flag| flag == PROBE));
    clean_absolute(bwrap)
        && trusted(bwrap)
        && passes(
            Command::new(helper)
                .arg(bridge::BRIDGE)
                .arg(BRIDGE_PORT_MIN.to_string())
                .arg(NEST)
                .arg("--")
                .arg(bwrap)
                .args(NEST_ARGS)
                .arg(bridge::UNSHARE_NET)
                .arg("--")
                .arg(helper)
                .args(flags),
        )
}

/// Tells whether this machine gives a nested agent a network of its own that leaves only
/// through its route ports: the widest routed nest a launch asks for, one route port and
/// one bridged port, starts the helper's probe inside it within [`ROUTE_PROBE_TIME_MAX`];
/// that probe passes only on the setup child's go byte ([`egress::started`]). Asked through
/// [`route_held`] only. Inputs: bubblewrap and the helper. Output: true when it exits 0.
fn routes(bwrap: &Path, helper: &Path) -> bool {
    clean_absolute(bwrap)
        && trusted(bwrap)
        && passes_within(
            Command::new(helper)
                .arg(egress::ROUTE)
                .arg("1")
                .arg(bridge::BRIDGE)
                .arg(BRIDGE_PORT_MIN.to_string())
                .arg(NEST)
                .arg("--")
                .arg(bwrap)
                .args(NEST_ARGS)
                .arg(bridge::UNSHARE_NET)
                .arg("--")
                .arg(helper)
                .arg(PROBE),
            ROUTE_PROBE_TIME_MAX,
        )
}

/// The route answer [`kept_route`] keeps: the bubblewrap and the helper it was asked for,
/// and whether the probe passed.
type KeptRoute = Mutex<Option<(PathBuf, PathBuf, bool)>>;

/// Tells whether a nested agent gets a routed network of its own from one kept answer that
/// the Agent access page, the launch notice and the launch all read (rv-code2 SX2-2,
/// SX2B-1): false when `refused`, a true kept for this bubblewrap and helper for the
/// engine's life, a kept false until `again` asks it anew (rv-sa241 SBX-2-R2 N1), else the
/// probe's, kept, with no lock held while it runs and a true kept meanwhile left in place.
/// Inputs: the cell, bubblewrap, the helper, the refusal, `again`, the probe. Output: it.
fn kept_route(
    kept: &KeptRoute,
    bwrap: &Path,
    helper: &Path,
    refused: bool,
    again: bool,
    probe: impl FnOnce() -> bool,
) -> bool {
    if refused {
        return false;
    }
    let held = kept.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some((asked_bwrap, asked_helper, answer)) = held
        && asked_bwrap == bwrap
        && asked_helper == helper
        && (answer || !again)
    {
        return answer;
    }
    let answer = probe();
    let mut cell = kept.lock().unwrap_or_else(PoisonError::into_inner);
    let found_meanwhile = cell
        .as_ref()
        .is_some_and(|(asked_bwrap, asked_helper, routed)| {
            *routed && asked_bwrap == bwrap && asked_helper == helper
        });
    if found_meanwhile {
        return true;
    }
    *cell = Some((bwrap.to_path_buf(), helper.to_path_buf(), answer));
    answer
}

/// What [`route_held`] answered, read by the host facts and every launch alike.
static ROUTE_READ: KeptRoute = Mutex::new(None);

/// [`kept_route`] for this machine: refused as root (rv-sa241 NOTE-2), where slirp4netns
/// would run with root's network before it joins the agent's. The tools check runs inside
/// the probe, so a kept true never changes for a key: a tool removed after it gives a routed
/// launch whose setup fails, which the go byte refuses. Inputs: bubblewrap, the helper and
/// `again`, which the host read sets to ask a kept false anew and a launch leaves unset, so
/// a launch never waits on the probe for a kept answer. Output: true when it is routed.
fn route_held(bwrap: &Path, helper: &Path, again: bool) -> bool {
    let root = rustix::process::geteuid().is_root();
    let probe = || {
        let tools = SLIRP_PATHS
            .iter()
            .chain(egress::IPTABLES_PATHS.iter())
            .all(|path| Path::new(path).is_file());
        tools && routes(bwrap, helper)
    };
    kept_route(&ROUTE_READ, bwrap, helper, root, again, probe)
}

/// The bubblewrap and the helper [`kept_nest`] found a nest for.
type KeptNest = Mutex<Option<(PathBuf, PathBuf)>>;

/// Tells whether this machine nests the helper in bubblewrap from one answer the Agent access
/// page and every launch read (rv-sa241 SBX-2-R2 LOW-1): a nest found for this bubblewrap and
/// helper is kept for the engine's life, and one not found is asked again each time, so a
/// machine that starts to nest is seen at its next read. No lock is held while the probe
/// runs, and a failed probe reads the cell again, so a nest another caller found meanwhile
/// holds. Inputs: the cell, bubblewrap, the helper, the probe. Output: true when it nests.
fn kept_nest(kept: &KeptNest, bwrap: &Path, helper: &Path, probe: impl FnOnce() -> bool) -> bool {
    let found = || {
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(asked_bwrap, asked_helper)| {
                asked_bwrap == bwrap && asked_helper == helper
            })
    };
    if found() {
        return true;
    }
    if probe() {
        let asked = (bwrap.to_path_buf(), helper.to_path_buf());
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(asked);
        return true;
    }
    found()
}

/// What [`nest_held`] found, read by the host facts and every launch alike.
static NEST_READ: KeptNest = Mutex::new(None);

/// [`kept_nest`] for this machine, asked with the probe alone ([`nests`]); a launch then
/// runs its own flags' probe inside the nest and is refused when that fails. Inputs:
/// bubblewrap and the helper. Output: true when the helper nests in it.
fn nest_held(bwrap: &Path, helper: &Path) -> bool {
    kept_nest(&NEST_READ, bwrap, helper, || {
        nests(bwrap, helper, &[PROBE.into()])
    })
}

/// Tells whether AppArmor keeps unprivileged programs out of user namespaces, from what
/// [`APPARMOR_USERNS`] reads: only `1`, with or without its newline, does. Input: the text,
/// none where the file cannot be read (no AppArmor, or an older one). Output: true when it
/// restricts them.
fn restricts(setting: Option<&str>) -> bool {
    setting.is_some_and(|text| text.strip_suffix('\n').unwrap_or(text) == "1")
}

/// Reads what this machine holds for an agent beyond its Landlock rules: whether the
/// kernel scopes signals (the helper's `--scoped`), whether a launch gives the agent a
/// process namespace ([`nest_held`], the answer every launch reads, or the plain probe
/// fails), whether the call supervisor
/// holds ([`supervises`]), whether a nested agent gets a routed network of its own
/// ([`route_held`], the answer every launch reads), whether this runs as root and whether
/// AppArmor's user namespace restriction, which never covers root, is why nothing nests
/// ([`restricts`]). Inputs: the helper and the engine's data folder, none where no probe
/// may run. Output: the facts.
pub fn read_host(helper: &Path, data: Option<&Path>) -> kernel_sandbox::host::LinuxHost {
    let usable = clean_absolute(helper) && helper.is_file();
    let scoped = usable && passes(Command::new(helper).arg(SCOPED));
    let held = usable && passes(Command::new(helper).arg(PROBE));
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file());
    let apart = |bwrap: &Path| !held || nest_held(bwrap, helper);
    let nested = usable && bwrap.is_some_and(apart);
    let root = rustix::process::geteuid().is_root();
    let nest = bwrap.filter(|_| nested);
    let supervised = held && data.is_some_and(|data| supervises(helper, nest, data, root));
    let routed = held && nest.is_some_and(|bwrap| route_held(bwrap, helper, true));
    let setting = fs::read_to_string(APPARMOR_USERNS).ok();
    let armored = !nested && !root && restricts(setting.as_deref());
    assert!(!routed || nested);
    assert!(!(root && routed));
    assert!(!(root && supervised));
    assert!(!(armored && nested));
    assert!(!(armored && root));
    kernel_sandbox::host::LinuxHost {
        nested,
        scoped,
        supervised,
        root,
        armored,
        routed,
    }
}

/// Tells whether a project may hold a git repository other than its own top-level `.git`:
/// an entry of any kind that is `.git` to some filesystem below the top, or one at the top
/// not spelled `.git`. A folder it cannot read, or more entries than it reads, answers yes.
/// Links are not followed. Input: the project folder. Output: true when one may be there.
pub(crate) fn holds_other_git(project: &Path) -> bool {
    let mut folders = vec![project.to_path_buf()];
    let mut seen: usize = 0;
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            return true;
        };
        for entry in entries {
            seen += 1;
            let Ok(entry) = entry else {
                return true;
            };
            if seen > GIT_SEARCH_ENTRIES_MAX {
                return true;
            }
            let name = entry.file_name();
            if is_git(&name) {
                let own = folder == project && name.as_os_str() == OsStr::new(".git");
                if !own {
                    return true;
                }
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            }
            assert!(folders.len() <= seen);
        }
    }
    assert!(seen <= GIT_SEARCH_ENTRIES_MAX);
    false
}

/// The most entries [`holds_other_git`] reads in a project before it answers yes: a read
/// runs inside the engine's read of the project's Agent access page.
const GIT_SEARCH_ENTRIES_MAX: usize = 4096;

/// What is kept of this machine and when it was read.
type KeptHost = Mutex<Option<(Instant, kernel_sandbox::host::LinuxHost)>>;

/// What [`host_within`] last read of this machine, which a launch whose own probe failed
/// marks unsupervised ([`forget_supervisor`]).
static HOST_READ: KeptHost = Mutex::new(None);

/// [`read_host`] for the helper launches take ([`helper_beside_engine`]) and the engine's
/// data folder, read again once what is kept is older than `age_max`, or with no age only
/// when nothing is kept. The lock is not held while the helper runs, so a reader of what is
/// kept never waits on it. Input: the age. Output: the facts, each false when it cannot be
/// asked.
fn host_within(age_max: Option<Duration>) -> kernel_sandbox::host::LinuxHost {
    let kept = *HOST_READ.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, host)) = kept
        && age_max.is_none_or(|age_max| at.elapsed() < age_max)
    {
        return host;
    }
    let helper = helper_beside_engine();
    let host = read_host(
        &helper.unwrap_or_default(),
        kernel_processes::finding::engine_root(),
    );
    *HOST_READ.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), host));
    host
}

/// Marks what is kept of this machine as running no call supervisor, after a launch with
/// the setting On found it does not hold (rv-secreview linux-t4c F2), so the Agent access
/// page shows the wider lines until the next read. Nothing kept stays nothing. Input: what
/// is kept.
fn forget_supervisor(kept: &KeptHost) {
    let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, host)) = held.as_mut() {
        host.supervised = false;
    }
    assert!(held.as_ref().is_none_or(|(_, host)| !host.supervised));
}

/// What this machine holds, read again once what is kept is older than [`HOST_AGE_MAX`]:
/// it may start the helper and bubblewrap, so it is not for a database transaction. No
/// input. Output: the facts as [`read_host`] gives them.
pub(crate) fn host_now() -> kernel_sandbox::host::LinuxHost {
    host_within(Some(HOST_AGE_MAX))
}

/// What this machine holds as it was last read, however long ago, read now only when it
/// never was: after [`host_now`] it starts nothing. No input. Output: the facts as
/// [`read_host`] gives them.
pub(crate) fn host_kept() -> kernel_sandbox::host::LinuxHost {
    host_within(None)
}

/// Refuses a policy neither Linux backend starts: one confined to its own worktree, or a
/// limited network under [`LinuxSockets::Refuse`]. Input: the policy. Output: Ok, or the
/// plain `Unavailable` reason.
fn admitted(sandbox: &ProjectSandbox) -> Result<(), SandboxError> {
    if sandbox.checkout.is_some() || sandbox.clone.is_some() {
        return Err(SandboxError::Unavailable(CONFINED_UNAVAILABLE.into()));
    }
    if matches!(sandbox.network(), SandboxNetwork::Only(_))
        && sandbox.linux_sockets() == LinuxSockets::Refuse
    {
        return Err(SandboxError::Unavailable(LIMITED_NETWORK_OFF.into()));
    }
    Ok(())
}

impl Sandbox for LinuxSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        match self.sandbox.linux_refusal(program) {
            None => {}
            Some(LINUX_LIMITED_NETWORK_OFF) => {
                return Err(SandboxError::Unavailable(LIMITED_NETWORK_OFF.into()));
            }
            Some(LINUX_HARNESS_UNCHECKED) => {
                return Err(SandboxError::Unavailable(HARNESS_UNCHECKED.into()));
            }
            Some(code) => {
                assert_eq!(code, LINUX_RESOLVER_NEEDS_UDP);
                return Err(SandboxError::Unavailable(RESOLVER_NEEDS_UDP.into()));
            }
        }
        let cwd = harness
            .get_current_dir()
            .unwrap_or(&self.sandbox.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.sandbox.project_root)
                .chain(self.sandbox.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        let grants = Grants::of(&self.sandbox)?;
        let confined = self.sandbox.checkout.is_some() || self.sandbox.clone.is_some();
        if confined && matches!(self.backend, LinuxBackend::Bubblewrap { .. }) {
            return Err(SandboxError::Unavailable(CONFINED_NEEDS_LANDLOCK.into()));
        }
        let unheld = !grants.guarded.is_empty() && self.supervision.is_none();
        if unheld {
            return Err(SandboxError::Unavailable(CONFINED_UNAVAILABLE.into()));
        }
        let mut command = match &self.backend {
            LinuxBackend::Landlock { helper } => {
                let policy =
                    self.with_guard(landlock_policy(&grants, &self.sandbox), &grants.guarded)?;
                assert!(holds_all(&policy, &grants.guarded));
                let mut command = Command::new(helper);
                command.args(policy.args());
                command
            }
            LinuxBackend::Nested { bwrap, helper } => {
                let policy = self.with_guard(
                    LandlockPolicy {
                        nested: true,
                        cut: private_tree(&grants.hidden).map(<[PathBuf]>::to_vec),
                        ..landlock_policy(&grants, &self.sandbox)
                    },
                    &grants.guarded,
                )?;
                assert!(holds_all(&policy, &grants.guarded));
                let (bridged, routes) = match &self.routed {
                    Some(routed) => (routed.bridged.clone(), routed.any.clone()),
                    None => (self.sandbox.bridged_ports().unwrap_or_default(), Vec::new()),
                };
                nest_command(
                    bwrap,
                    helper,
                    &policy,
                    &grants.hidden,
                    cwd,
                    &bridged,
                    &routes,
                )
            }
            LinuxBackend::Bubblewrap { bwrap, helper } => {
                bwrap_command(bwrap, helper, &grants, cwd, self.sandbox.network())
            }
        };
        let mut base = vec![
            ("HOME", self.sandbox.agent_home().as_os_str()),
            ("TMPDIR", self.sandbox.agent_tmp().as_os_str()),
            ("PATH", OsStr::new(AGENT_PATH)),
            ("LANG", OsStr::new("C")),
            ("LC_ALL", OsStr::new("C")),
        ];
        if self.sandbox.resolves_over_tcp() {
            base.push(("RES_OPTIONS", OsStr::new(DNS_OVER_TCP)));
        }
        let env = agent_process_env(&base, harness.get_envs())?;
        command
            .arg("--")
            .arg(program)
            .args(harness.get_args())
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(cwd)
            .process_group(0);
        assert!(command.get_args().any(|arg| arg == "--"));
        Ok(command)
    }
}

/// What a launch's Landlock rules grant: folders read, folders written whole, git folders
/// written with [`git_folder_rights`], and `guarded`, what Landlock cannot keep read-only
/// under those grants, as the call supervisor holds it (protected paths in a git grant,
/// [`ADMIN_WRITES`] and [`ADMIN_LINKS`] in the admin folder, the worktree's `.git` link and
/// a clone's store names, [`clone_store`]), which that supervisor must hold before a
/// confined launch starts, the nested binds a second layer (DEC-185 S1, S2).
struct Grants {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    git_write: Vec<PathBuf>,
    guarded: Held,
    /// The hidden folders, each made empty first when absent so a folder made during the
    /// run lands under the cover (rv-sec2 F2), each covered by bubblewrap with an empty
    /// read-only folder; no write grant covers any hidden folder ([`split_above`]).
    hidden: Vec<PathBuf>,
}

impl Grants {
    fn of(sandbox: &ProjectSandbox) -> Result<Self, SandboxError> {
        mounts_hold(sandbox)?;
        let project = sandbox.project_root();
        let git = project.join(".git");
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let protected = sandbox.protected_paths();
        let objects = git.join("objects");
        let objects_info = objects.join("info");
        let followed: Vec<PathBuf> = GIT_FOLLOWED
            .iter()
            .chain(&GIT_STATE_TREES)
            .chain(&GIT_STATE_FILES)
            .map(|name| git.join(name))
            .collect();
        let followed: Vec<&PathBuf> = followed.iter().collect();
        let folder = |path: &Path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
        let checkout = sandbox.checkout.as_ref();
        let mut write = Vec::new();
        let mut git_write = Vec::new();
        for root in sandbox.writable_roots() {
            if checkout.is_some_and(|checkout| *root == checkout.objects) {
                continue;
            }
            if checkout.is_some_and(|checkout| *root == checkout.admin) {
                git_write.push(root.clone());
                continue;
            }
            if root != project {
                write.push(root.clone());
                continue;
            }
            writable_children(project, &[&git, &state], &protected, &mut write)?;
            if folder(&git) {
                writable_children(&git, &followed, &protected, &mut write)?;
                if folder(&objects) {
                    writable_children(&objects, &[&objects_info], &protected, &mut write)?;
                }
            }
            if folder(&state) {
                writable_children(&state, &[], &protected, &mut write)?;
            }
        }
        for hidden in &sandbox.hidden {
            split_above(hidden, &protected, &mut write)?;
        }
        if git_write
            .iter()
            .any(|root| sandbox.hidden.iter().any(|hidden| hidden.starts_with(root)))
        {
            return Err(invalid("a git folder grant holds a hidden folder"));
        }
        let mut hidden = Vec::new();
        for path in &sandbox.hidden {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            if !fs::symlink_metadata(path)?.is_dir() {
                return Err(invalid("a hidden folder is no longer a plain folder"));
            }
            hidden.push(path.clone());
        }
        assert_eq!(hidden.len(), sandbox.hidden.len());
        assert!(hidden.len() <= HIDDEN_MAX);
        assert!(write.iter().all(|path| {
            sandbox
                .hidden
                .iter()
                .all(|hidden| !hidden.starts_with(path))
        }));
        assert!(
            write
                .iter()
                .all(|path| !followed.contains(&path) && *path != objects_info)
        );
        let mut read = vec![project.to_path_buf()];
        read.extend(
            sandbox
                .read_roots()
                .iter()
                .filter(|root| *root != project)
                .cloned(),
        );
        if read.len() + write.len() + git_write.len() > GRANTS_MAX {
            return Err(invalid("project folder has too many entries to grant"));
        }
        let covers =
            |path: &PathBuf, item: &PathBuf| path.starts_with(item) || item.starts_with(path);
        let inside = |path: &PathBuf, item: &PathBuf| path.starts_with(item);
        if write
            .iter()
            .any(|path| protected.iter().any(|item| covers(path, item)))
            || git_write
                .iter()
                .any(|path| protected.iter().any(|item| inside(path, item)))
        {
            return Err(invalid("a writable root sits inside a protected path"));
        }
        let mut guarded = Held {
            trees: protected
                .iter()
                .filter(|item| git_write.iter().any(|root| item.starts_with(root)))
                .cloned()
                .collect(),
            ..Held::default()
        };
        if let Some(checkout) = checkout {
            if git_write.contains(&checkout.admin) {
                let held = ADMIN_WRITES.iter().chain(&ADMIN_LINKS);
                guarded
                    .trees
                    .extend(held.map(|name| checkout.admin.join(name)));
            }
            if write.contains(&checkout.worktree) {
                guarded.trees.push(checkout.worktree.join(".git"));
            }
        }
        if let Some(clone) = sandbox.clone.as_ref() {
            let store = clone_store(clone);
            guarded.entries.extend(store.entries);
            guarded.trees.extend(store.trees);
            guarded.stores.extend(store.stores);
        }
        let secrets = sandbox.secret_paths();
        let carved = sandbox.carved_home.as_ref();
        assert!(read.iter().chain(&write).chain(&git_write).all(|grant| {
            secrets.iter().all(|secret| {
                carved.is_some_and(|(home, folder)| home == grant && folder == secret)
                    || (!grant.starts_with(secret) && !secret.starts_with(grant))
            })
        }));
        Ok(Self {
            read,
            write,
            git_write,
            guarded,
            hidden,
        })
    }
}

/// Refuses a launch whose hidden folders a second mount of their disk shows ([`mounted_again`]),
/// reading this computer's mount list only when the policy hides a folder, at install and at
/// wrap. A later mount stays out, since the helper cuts the agent's tree first ([`cut`]); stated
/// limit (leader 19:09 Q6): one made between this read and that cut still reaches the agent.
/// Input: the policy. Output: Ok, or the refusal.
fn mounts_hold(sandbox: &ProjectSandbox) -> Result<(), SandboxError> {
    if sandbox.hidden.is_empty() {
        return Ok(());
    }
    let text = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|_| SandboxError::Unavailable(HIDDEN_MOUNTS_UNREAD.into()))?;
    mounted_again(&sandbox.hidden, &text)
}

/// Refuses hidden folders whose disk the mount list shows mounted a second time at the
/// folder, above it or inside it, wherever that mount sits: a folder's own mount is the
/// deepest one holding its path. Inputs: the hidden folders and the mount list text.
/// Output: Ok, or the refusal; a list that does not read plainly refuses too.
fn mounted_again(hidden: &[PathBuf], text: &str) -> Result<(), SandboxError> {
    let unread = || SandboxError::Unavailable(HIDDEN_MOUNTS_UNREAD.into());
    let mounts = mounts(text).ok_or_else(unread)?;
    for folder in hidden {
        assert!(folder.is_absolute());
        let own = mounts
            .iter()
            .filter(|mount| folder.starts_with(&mount.point))
            .max_by_key(|mount| mount.point.components().count())
            .filter(|mount| mount.root.is_absolute())
            .ok_or_else(unread)?;
        let below = folder
            .strip_prefix(&own.point)
            .expect("a folder's own mount holds its path");
        let inside = own.root.join(below);
        let shows = |root: &Path| root.starts_with(&inside) || inside.starts_with(root);
        if mounts
            .iter()
            .any(|mount| mount.id != own.id && mount.device == own.device && shows(&mount.root))
        {
            return Err(SandboxError::Unavailable(HIDDEN_MOUNTED_AGAIN.into()));
        }
    }
    Ok(())
}

/// Splits every write grant that covers the hidden folder `folder` so that none does: the
/// grant goes, each folder from it down to `folder`'s parent keeps only what the read grants
/// give, its other existing entries are granted as [`writable_children`] grants them, and
/// `folder` gets nothing, so it cannot be written, made, moved or carried off by a move
/// above it. Inputs: the folder, the protected paths and the grants. Output: Ok, or why not.
fn split_above(
    folder: &Path,
    protected: &[PathBuf],
    write: &mut Vec<PathBuf>,
) -> Result<(), SandboxError> {
    let covering: Vec<PathBuf> = write
        .iter()
        .filter(|grant| folder.starts_with(grant))
        .cloned()
        .collect();
    write.retain(|grant| !folder.starts_with(grant));
    for grant in covering {
        let mut above = grant;
        while above != folder {
            let step = folder
                .strip_prefix(&above)
                .ok()
                .and_then(|rest| rest.components().next())
                .ok_or_else(|| invalid("a hidden folder lies outside its grant"))?;
            let next = above.join(step);
            writable_children(&above, &[&next], protected, write)?;
            above = next;
        }
    }
    assert!(write.iter().all(|grant| !folder.starts_with(grant)));
    Ok(())
}

fn writable_children(
    dir: &Path,
    skip: &[&PathBuf],
    protected: &[PathBuf],
    write: &mut Vec<PathBuf>,
) -> Result<(), SandboxError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() || skip.contains(&&path) || protected.contains(&path) {
            continue;
        }
        if kind.is_dir() || kind.is_file() {
            write.push(path);
        }
        if write.len() > GRANTS_MAX {
            return Err(invalid("project folder has too many entries to grant"));
        }
    }
    Ok(())
}

/// The guards a supervised launch carries (DEC-179 T4c): the loopback ports of a limited
/// network under [`LinuxSockets::TcpOnly`], each then reached on this computer only, but a
/// port the agent may also reach on any host; the agent's own git folders
/// ([`git_own`]); and signals where [`Supervision`] says nothing else holds them. Inputs:
/// the policy and what the supervisor holds here. Output: the guard.
fn guard(sandbox: &ProjectSandbox, supervision: Supervision) -> Guard {
    let loopback = match sandbox.network() {
        SandboxNetwork::Only(remotes) if sandbox.linux_sockets() == LinuxSockets::TcpOnly => {
            let anywhere = |port: u16| {
                remotes.contains(&SandboxRemote::AnyHost(port))
                    || (sandbox.resolves_over_tcp() && port == DNS_PORT)
            };
            ports(remotes)
                .into_iter()
                .filter(|port| {
                    remotes.contains(&SandboxRemote::Loopback(*port)) && !anywhere(*port)
                })
                .collect()
        }
        SandboxNetwork::Only(_) | SandboxNetwork::Open => Vec::new(),
    };
    let held = Guard {
        loopback,
        git: Some(git_own(sandbox)),
        signals: supervision.signals,
        held: None,
    };
    assert!(!held.loopback.contains(&0));
    assert!(held.loopback.windows(2).all(|pair| pair[0] < pair[1]));
    held
}

/// Tells whether a launch policy's supervisor holds every guarded place, the rule that lets
/// a confined launch past the tripwire (DEC-185 S2). Inputs: the policy and the guarded
/// places. Output: true when there are none, or the supervisor holds exactly them.
fn holds_all(policy: &LandlockPolicy, guarded: &Held) -> bool {
    guarded.is_empty() || policy.guard.held.as_ref() == Some(guarded)
}

/// What the call supervisor holds in a clone, the names macOS keeps by `clone_store_denies`
/// (DEC-185 S2b): as entries the git folder, its `commondir`, the store with its `info` and
/// `pack` folders, the alternates, the 256 loose-object folders and the multi-pack-index;
/// whole the fallback repository and the index's chain folder; the store as a store.
/// Input: the clone's real root. Output: the held places.
fn clone_store(clone: &Path) -> Held {
    let git = clone.join(".git");
    let objects = git.join("objects");
    let (info, pack) = (objects.join("info"), objects.join("pack"));
    let [commondir, _] = ADMIN_LINKS;
    let mut entries = vec![
        git.clone(),
        git.join(commondir),
        objects.clone(),
        info.clone(),
        pack.clone(),
        pack.join(MULTI_PACK_INDEX),
    ];
    entries.extend(STORE_ALTERNATES.map(|name| info.join(name)));
    entries.extend((0..=u8::MAX).map(|byte| objects.join(format!("{byte:02x}"))));
    let held = Held {
        entries,
        trees: vec![
            clone.join(GIT_FALLBACK),
            pack.join(format!("{MULTI_PACK_INDEX}.d")),
        ],
        stores: vec![objects],
    };
    assert_eq!(held.entries.len(), 264);
    held
}

/// The folders an agent may make a `.git` entry in, as the macOS boundary's nested git
/// rule allows them: the top of each working tree it works in, each folder directly inside
/// the engine's worktree folders (when it adds worktrees) or inside a setup folder, and for
/// a linked worktree the project's `.git` its admin folder sits in, where Landlock lets it
/// write the admin folder alone (DEC-185 S2). Input: the policy. Output: the agent's own
/// git folders.
fn git_own(sandbox: &ProjectSandbox) -> GitOwn {
    let (working, _) = sandbox.git_deny_roots();
    let mut kids = Vec::new();
    if sandbox.adds_worktrees {
        let project = sandbox.project_root();
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        kids.extend([project.join("worktrees"), state.join("worktrees")]);
    }
    if let Some(setup) = &sandbox.setup {
        kids.push(setup.folder.clone());
    }
    let common = sandbox.checkout.as_ref().and_then(|checkout| {
        let plain = |folder: &&Path| folder.file_name() == Some(OsStr::new(".git"));
        checkout
            .admin
            .ancestors()
            .find(plain)
            .map(Path::to_path_buf)
    });
    let own = GitOwn {
        trees: working.into_iter().map(Path::to_path_buf).collect(),
        kids,
        exact: common.into_iter().collect(),
    };
    assert!(!own.trees.is_empty());
    assert!(
        sandbox
            .clone
            .as_ref()
            .is_none_or(|clone| own.trees.contains(clone))
    );
    own
}

fn landlock_policy(grants: &Grants, sandbox: &ProjectSandbox) -> LandlockPolicy {
    let mut read: Vec<PathBuf> = SYSTEM_READ_ROOTS
        .iter()
        .chain(&["/proc"])
        .map(PathBuf::from)
        .collect();
    read.extend(grants.read.iter().cloned());
    let mut write: Vec<PathBuf> = LANDLOCK_DEVICES.iter().map(PathBuf::from).collect();
    write.extend(grants.write.iter().cloned());
    let (limit_network, mut connect) = match sandbox.network() {
        SandboxNetwork::Open => (false, Vec::new()),
        SandboxNetwork::Only(remotes) => (true, ports(remotes)),
    };
    if sandbox.resolves_over_tcp() {
        connect.push(DNS_PORT);
        connect.sort_unstable();
        connect.dedup();
    }
    let open_sockets = limit_network && sandbox.linux_sockets() == LinuxSockets::TcpPorts;
    let keep_free = kept_free(sandbox);
    let git_write = grants.git_write.clone();
    assert!(
        read.len() + write.len() + git_write.len() <= HELPER_PATHS_MAX,
        "the helper accepts every policy the engine writes"
    );
    assert!(
        !keep_free.is_empty(),
        "every agent is held to the ports Linux picks"
    );
    LandlockPolicy {
        read,
        write,
        git_write,
        connect,
        limit_network,
        open_sockets,
        keep_free,
        die_with: None,
        nested: false,
        guard: Guard::default(),
        cut: None,
    }
}

/// The TCP ports no agent listens on: the owner's fixed ports, the kept local model ports
/// and every model proxy and model relay port, as the macOS boundary keeps them. Input: the policy. Output:
/// those ports, rising and each once.
fn kept_free(sandbox: &ProjectSandbox) -> Vec<u16> {
    let mut ports: Vec<u16> = OWNER_PORTS
        .iter()
        .chain(&sandbox.kept_ports)
        .copied()
        .chain(PROXY_PORTS)
        .chain(RELAY_PORTS)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    assert!(OWNER_PORTS.iter().all(|port| ports.contains(port)));
    assert!(!ports.contains(&0));
    assert!(ports.len() <= KEEP_FREE_MAX);
    ports
}

fn ports(remotes: &[SandboxRemote]) -> Vec<u16> {
    let mut ports: Vec<u16> = remotes
        .iter()
        .map(|remote| match remote {
            SandboxRemote::Loopback(port) | SandboxRemote::AnyHost(port) => *port,
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The command for a nested agent up to its harness: the helper asked to start bubblewrap
/// ([`NEST`]) with [`NEST_ARGS`] in the working folder, and inside it the helper again with
/// the policy. With route or bridged ports the route flags lead, then the bridge flags, and
/// bubblewrap gives the agent a network of its own; an empty read-only folder covers each
/// existing hidden folder. Inputs: bubblewrap, the helper, the nested policy, the hidden
/// folders, the working folder, the bridged and route ports. Output: the command up to `--`.
fn nest_command(
    bwrap: &Path,
    helper: &Path,
    policy: &LandlockPolicy,
    hidden: &[PathBuf],
    cwd: &Path,
    bridged: &[u16],
    routes: &[u16],
) -> Command {
    assert!(policy.nested);
    assert!(policy.die_with.is_none());
    assert!(routes.len() <= REMOTES_MAX);
    assert!(hidden.len() <= HIDDEN_MAX);
    let mut command = Command::new(helper);
    for port in routes {
        command.arg(egress::ROUTE).arg(port.to_string());
    }
    for port in bridged {
        command.arg(bridge::BRIDGE).arg(port.to_string());
    }
    command.arg(NEST).arg("--").arg(bwrap).args(NEST_ARGS);
    if !bridged.is_empty() || !routes.is_empty() {
        command.arg(bridge::UNSHARE_NET);
    }
    for folder in hidden {
        command
            .arg("--tmpfs")
            .arg(folder)
            .arg("--remount-ro")
            .arg(folder);
    }
    command
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg(helper)
        .args(policy.args());
    assert_eq!(command.get_args().filter(|arg| *arg == NEST).count(), 1);
    command
}

/// The bubblewrap command for a closed agent up to its harness: its own namespaces, the system
/// folders read-only but the name resolver's ([`RESOLVER_ROOT`]), the grants, an empty read-only
/// folder over each existing hidden folder, then the helper read-only for the socket filter
/// ([`FILTER_SOCKETS`]), with a private mount tree first when it hides folders ([`cut`]). Inputs:
/// bwrap, the helper, the grants, the working folder and the network. Output: the command.
fn bwrap_command(
    bwrap: &Path,
    helper: &Path,
    grants: &Grants,
    cwd: &Path,
    network: &SandboxNetwork,
) -> Command {
    assert!(
        grants.git_write.is_empty(),
        "no confined layout starts on bubblewrap"
    );
    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    if matches!(network, SandboxNetwork::Only(remotes) if remotes.is_empty()) {
        command.arg("--unshare-net");
    }
    let system: Vec<&str> = SYSTEM_READ_ROOTS
        .iter()
        .copied()
        .filter(|root| *root != RESOLVER_ROOT)
        .collect();
    assert_eq!(system.len() + 1, SYSTEM_READ_ROOTS.len());
    for root in system {
        command.args(["--ro-bind-try", root, root]);
    }
    for root in &grants.read {
        command.arg("--ro-bind").arg(root).arg(root);
    }
    for root in &grants.write {
        command.arg("--bind").arg(root).arg(root);
    }
    for folder in &grants.hidden {
        command
            .arg("--tmpfs")
            .arg(folder)
            .arg("--remount-ro")
            .arg(folder);
    }
    command.arg("--ro-bind").arg(helper).arg(helper);
    command.arg("--chdir").arg(cwd);
    command.arg("--").arg(helper).arg(FILTER_SOCKETS);
    command.args(cut_args(private_tree(&grants.hidden)));
    command
}

/// The covers a launch asks the helper's private mount tree for: its hidden folders, or no
/// private tree when it hides none (leader 19:09 Q1). Input: the hidden folders. Output:
/// the covers, or none.
fn private_tree(hidden: &[PathBuf]) -> Option<&[PathBuf]> {
    (!hidden.is_empty()).then_some(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_sandbox::linux::helper::bridge::Bridge;
    use kernel_sandbox::ports::PICKED_PORTS_START;
    use std::os::unix::fs::MetadataExt;

    fn helper() -> PathBuf {
        Path::new("/opt/crazyagent").join(HELPER_NAME)
    }

    #[test]
    fn bubblewrap_starts_the_agent_in_a_new_session() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let command = bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &SandboxNetwork::Open,
        );
        assert!(command.get_args().any(|arg| arg == "--new-session"));
    }

    #[test]
    fn bubblewrap_cuts_off_the_network_only_when_nothing_is_allowed() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let unshares = |network: SandboxNetwork| {
            bwrap_command(
                Path::new(BWRAP_PATHS[0]),
                &helper(),
                &grants,
                Path::new("/tmp"),
                &network,
            )
            .get_args()
            .any(|arg| arg == "--unshare-net")
        };
        assert!(unshares(SandboxNetwork::Only(Vec::new())));
        assert!(!unshares(SandboxNetwork::Open));
    }

    fn project(network: SandboxNetwork) -> (tempfile::TempDir, ProjectSandbox) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let folders = ["project", "home", "tmp"].map(|name| root.join(name));
        for folder in &folders {
            fs::create_dir(folder).unwrap();
        }
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &folders[0],
            &folders[1],
            &folders[2],
            network,
        )
        .unwrap();
        (temp, sandbox)
    }

    fn git(folder: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(folder)
            .args([
                "-c",
                "user.name=row",
                "-c",
                "user.email=row@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null());
        let status = kernel_processes::spawn_gate::status(&mut command).unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A project with one commit and a linked worktree in its worktrees folder, then the
    /// sandbox over it with the agent's home and temp folders. No input. Output: the temp
    /// folder, the project, the sandbox and the worktree.
    fn linked() -> (tempfile::TempDir, PathBuf, ProjectSandbox, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let folders = ["project", "home", "tmp"].map(|name| root.join(name));
        for folder in &folders {
            fs::create_dir(folder).unwrap();
        }
        let project = folders[0].clone();
        fs::write(project.join("owner.txt"), "owner\n").unwrap();
        git(&project, &["init", "-q"]);
        git(&project, &["add", "owner.txt"]);
        git(&project, &["commit", "-q", "-m", "start"]);
        fs::create_dir(project.join("worktrees")).unwrap();
        let worktree = project.join("worktrees").join("agent-1");
        let path = worktree.to_str().unwrap();
        git(
            &project,
            &["worktree", "add", "-q", "--detach", path, "HEAD"],
        );
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &project,
            &folders[1],
            &folders[2],
            SandboxNetwork::Open,
        )
        .unwrap();
        (temp, project, sandbox, worktree)
    }

    #[test]
    fn a_linked_worktree_writes_its_tree_its_admin_folder_without_links_and_no_object() {
        let (_temp, project, sandbox, worktree) = linked();
        let confined = sandbox.confine_to_worktree(&worktree).unwrap();
        let git = project.join(".git");
        let admin = git.join("worktrees").join("agent-1");
        let objects = git.join("objects");
        let granted = Grants::of(&confined);
        assert!(granted.is_ok(), "{:?}", granted.as_ref().err());
        let grants = granted.unwrap();
        assert!(grants.write.contains(&worktree));
        assert_eq!(grants.git_write, vec![admin.clone()]);
        assert!(
            !grants.write.contains(&admin),
            "the admin folder has the git tier only"
        );
        for path in grants.write.iter().chain(&grants.git_write) {
            assert!(
                !path.starts_with(&objects) && !objects.starts_with(path),
                "{} reaches the object store",
                path.display()
            );
            assert!(
                *path == admin || !path.starts_with(&git),
                "{} is in the project's git folder",
                path.display()
            );
        }
        assert!(
            grants.read.contains(&project),
            "the object store is read through the project"
        );
        assert!(grants.guarded.entries.is_empty() && grants.guarded.stores.is_empty());
        let mut guarded = grants.guarded.trees.clone();
        guarded.sort();
        let mut held = ["HEAD", "commondir", "gitdir", "config.worktree", "modules"]
            .map(|name| admin.join(name))
            .to_vec();
        held.push(worktree.join(".git"));
        held.sort();
        assert_eq!(
            guarded, held,
            "every entry macOS keeps by name under the two grants is left to the supervisor and the binds"
        );
    }

    #[test]
    fn a_confined_launch_on_the_bubblewrap_backend_is_refused_with_its_own_line() {
        let (_temp, _project, sandbox, worktree) = linked();
        let launch = LinuxSandbox {
            sandbox: sandbox.confine_to_worktree(&worktree).unwrap(),
            backend: LinuxBackend::Bubblewrap {
                bwrap: PathBuf::from(BWRAP_PATHS[0]),
                helper: helper(),
            },
            supervision: None,
            routed: None,
        };
        let mut harness = Command::new("/bin/true");
        harness.current_dir(&worktree);
        let refused = launch.wrap(&harness).err();
        assert!(
            matches!(&refused, Some(SandboxError::Unavailable(reason))
                if reason == CONFINED_NEEDS_LANDLOCK),
            "{refused:?}"
        );
    }

    #[test]
    fn a_launch_whose_git_grant_covers_a_protected_path_nothing_holds_is_refused() {
        let (_temp, _project, sandbox, worktree) = linked();
        let launch = LinuxSandbox {
            sandbox: sandbox.confine_to_worktree(&worktree).unwrap(),
            backend: LinuxBackend::Landlock { helper: helper() },
            supervision: None,
            routed: None,
        };
        let mut harness = Command::new("/bin/true");
        harness.current_dir(&worktree);
        let refused = launch.wrap(&harness).err();
        assert!(
            matches!(&refused, Some(SandboxError::Unavailable(reason))
                if reason == CONFINED_UNAVAILABLE),
            "{refused:?}"
        );
    }

    /// LINUX-KILL-GROUP: only the plain Landlock backend leaves a process in a session of its
    /// own running after a stop's group kill, so only its runs have their lineage checked;
    /// both bubblewrap backends end every process with their process namespace.
    #[test]
    fn only_the_plain_landlock_backend_leaves_a_session_of_its_own_running() {
        let (_temp, _project, sandbox, _worktree) = linked();
        let ends = |backend: LinuxBackend| {
            LinuxSandbox {
                sandbox: sandbox.clone(),
                backend,
                supervision: None,
                routed: None,
            }
            .ends_every_session()
        };
        let bwrap = PathBuf::from("/usr/bin/bwrap");

        assert!(!ends(LinuxBackend::Landlock { helper: helper() }));
        assert!(ends(LinuxBackend::Nested {
            bwrap: bwrap.clone(),
            helper: helper(),
        }));
        assert!(ends(LinuxBackend::Bubblewrap {
            bwrap,
            helper: helper(),
        }));
    }

    fn supervised_launch(sandbox: ProjectSandbox) -> LinuxSandbox {
        LinuxSandbox {
            sandbox,
            backend: LinuxBackend::Landlock { helper: helper() },
            supervision: Some(Supervision { signals: false }),
            routed: None,
        }
    }

    fn flag_values(words: &[OsString], flag: &str) -> Vec<PathBuf> {
        words
            .windows(2)
            .filter(|pair| pair[0] == flag)
            .map(|pair| PathBuf::from(&pair[1]))
            .collect()
    }

    #[test]
    fn a_supervised_linked_worktree_launches_with_every_guarded_entry_held() {
        let (_temp, project, sandbox, worktree) = linked();
        let confined = sandbox.confine_to_worktree(&worktree).unwrap();
        let mut guarded = Grants::of(&confined).unwrap().guarded.trees;
        guarded.sort();
        let launch = supervised_launch(confined);
        let mut harness = Command::new("/bin/true");
        harness.current_dir(&worktree);
        let wrapped = launch.wrap(&harness);
        assert!(wrapped.is_ok(), "{:?}", wrapped.as_ref().err());
        let words: Vec<OsString> = wrapped
            .unwrap()
            .get_args()
            .map(OsStr::to_os_string)
            .collect();
        let mut held = flag_values(&words, GUARD_HELD_TREE);
        held.extend(flag_values(&words, GUARD_HELD));
        held.sort();
        assert_eq!(
            held, guarded,
            "the supervisor holds the list the tripwire reads"
        );
        assert_eq!(flag_values(&words, OWN_TREE), vec![worktree.clone()]);
        assert_eq!(flag_values(&words, OWN_GIT), vec![project.join(".git")]);
    }

    #[test]
    fn a_linked_worktree_agent_makes_entries_in_its_admin_folder_and_no_other_git() {
        let (_temp, project, sandbox, worktree) = linked();
        let confined = sandbox.confine_to_worktree(&worktree).unwrap();
        let own = git_own(&confined);
        let admin = project.join(".git").join("worktrees").join("agent-1");
        for made in [
            admin.join("index.lock"),
            admin.join("logs").join("HEAD"),
            worktree.join("src").join("main.rs"),
        ] {
            assert!(own.allows(&made), "{}", made.display());
        }
        for refused in [
            worktree.join("vendor").join(".git"),
            admin.join("modules").join("sub").join(".git"),
            project.join("other").join(".git").join("config"),
        ] {
            assert!(!own.allows(&refused), "{}", refused.display());
        }
    }

    #[test]
    fn a_supervised_clone_launches_with_its_store_held() {
        let (_temp, project, sandbox, _) = linked();
        let clone = project.join("worktrees").join("agent-2");
        fs::create_dir(&clone).unwrap();
        git(&clone, &["init", "-q"]);
        let confined = sandbox.confine_to_clone(&clone).unwrap();
        let guarded = Grants::of(&confined).unwrap().guarded;
        let launch = supervised_launch(confined);
        let mut harness = Command::new("/bin/true");
        harness.current_dir(&clone);
        let wrapped = launch.wrap(&harness);
        assert!(wrapped.is_ok(), "{:?}", wrapped.as_ref().err());
        let words: Vec<OsString> = wrapped
            .unwrap()
            .get_args()
            .map(OsStr::to_os_string)
            .collect();
        assert_eq!(flag_values(&words, GUARD_HELD), guarded.entries);
        assert_eq!(flag_values(&words, GUARD_HELD_TREE), guarded.trees);
        assert_eq!(flag_values(&words, GUARD_HELD_STORE), guarded.stores);
        assert!(
            flag_values(&words, OWN_TREE).contains(&clone),
            "the clone's own top-level .git is the agent's"
        );
        let parsed = match HelperRequest::parse(words.clone()) {
            Ok(HelperRequest::Run { policy, .. }) => policy.guard.held,
            other => panic!("the wrapped words do not read back: {other:?}"),
        };
        assert_eq!(
            parsed.as_ref(),
            Some(&guarded),
            "every held place reads back"
        );
        let objects = clone.join(".git").join("objects");
        assert_eq!(guarded.stores, vec![objects.clone()]);
        assert!(guarded.entries.contains(&objects.join("ff")));
        let unsupervised = LinuxSandbox {
            supervision: None,
            ..launch
        };
        let refused = unsupervised.wrap(&harness).err();
        assert!(
            matches!(&refused, Some(SandboxError::Unavailable(reason))
                if reason == CONFINED_UNAVAILABLE),
            "an unsupervised clone holds nothing: {refused:?}"
        );
    }

    #[test]
    fn a_clone_writes_its_whole_clone_and_has_no_git_folder_grant() {
        let (_temp, project, sandbox, _) = linked();
        let clone = project.join("worktrees").join("agent-2");
        fs::create_dir(&clone).unwrap();
        git(&clone, &["init", "-q"]);
        let confined = sandbox.confine_to_clone(&clone).unwrap();
        let grants = Grants::of(&confined).unwrap();
        assert!(grants.write.contains(&clone));
        assert!(grants.git_write.is_empty());
        let git_folder = clone.join(".git");
        let objects = git_folder.join("objects");
        let (info, pack) = (objects.join("info"), objects.join("pack"));
        let mut entries = vec![
            git_folder.clone(),
            git_folder.join("commondir"),
            objects.clone(),
            info.clone(),
            info.join("alternates"),
            info.join("http-alternates"),
            pack.clone(),
            pack.join("multi-pack-index"),
        ];
        entries.extend((0..=u8::MAX).map(|byte| objects.join(format!("{byte:02x}"))));
        entries.sort();
        let mut trees = vec![clone.join(".git.git"), pack.join("multi-pack-index.d")];
        trees.sort();
        let mut guarded = grants.guarded.clone();
        guarded.entries.sort();
        guarded.trees.sort();
        assert_eq!(guarded.entries, entries, "each store name macOS keeps");
        assert_eq!(guarded.trees, trees);
        assert_eq!(
            guarded.stores,
            vec![objects],
            "no symlink or folder put in the store"
        );
        let git = project.join(".git");
        assert!(grants.write.iter().all(|path| !path.starts_with(&git)));
    }

    #[test]
    fn a_clone_on_the_bubblewrap_backend_is_refused_with_the_landlock_line() {
        let (_temp, project, sandbox, _) = linked();
        let clone = project.join("worktrees").join("agent-2");
        fs::create_dir(&clone).unwrap();
        git(&clone, &["init", "-q"]);
        let launch = LinuxSandbox {
            sandbox: sandbox.confine_to_clone(&clone).unwrap(),
            backend: LinuxBackend::Bubblewrap {
                bwrap: PathBuf::from(BWRAP_PATHS[0]),
                helper: helper(),
            },
            supervision: None,
            routed: None,
        };
        let mut harness = Command::new("/bin/true");
        harness.current_dir(&clone);
        let refused = launch.wrap(&harness).err();
        assert!(
            matches!(&refused, Some(SandboxError::Unavailable(reason))
                if reason == CONFINED_NEEDS_LANDLOCK),
            "{refused:?}"
        );
    }

    #[test]
    fn a_git_folder_grant_reaches_the_helper_and_counts_toward_its_limit() {
        let folder = PathBuf::from("/p/.git/worktrees/agent-1");
        let policy = LandlockPolicy {
            read: vec![PathBuf::from("/usr")],
            git_write: vec![folder.clone()],
            ..LandlockPolicy::default()
        };
        let args = policy.args();
        let at = args.iter().position(|arg| arg == GIT_WRITE);
        assert_eq!(at.map(|at| &args[at + 1]), Some(&OsString::from(&folder)));
        let mut run = args;
        run.extend([OsString::from("--"), OsString::from("/bin/true")]);
        let parsed = match HelperRequest::parse(run) {
            Ok(HelperRequest::Run { policy, .. }) => Some(*policy),
            _ => None,
        };
        assert_eq!(parsed, Some(policy));
        let many = LandlockPolicy {
            git_write: vec![folder; HELPER_PATHS_MAX + 1],
            ..LandlockPolicy::default()
        };
        let mut run = many.args();
        run.extend([OsString::from("--"), OsString::from("/bin/true")]);
        assert_eq!(
            HelperRequest::parse(run).err(),
            Some("too many path grants".to_owned()),
            "the git tier counts toward the helper's path limit"
        );
    }

    #[test]
    #[should_panic(expected = "no confined layout starts on bubblewrap")]
    fn bubblewrap_never_takes_a_git_folder_grant() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: vec![PathBuf::from("/p/.git/worktrees/agent-1")],
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let _command = bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &SandboxNetwork::Open,
        );
    }

    #[test]
    fn an_open_network_agent_binds_every_port_but_the_kept_ones() {
        let keep_free = [1455, 11434, 41300];
        let ports = bindable(&keep_free);
        assert_eq!(ports.len(), usize::from(u16::MAX) + 1 - keep_free.len());
        assert_eq!(ports[..3], [0, 1, 2]);
        assert_eq!(ports.last(), Some(&u16::MAX));
        for port in keep_free {
            assert!(!ports.contains(&port), "{port}");
            assert!(ports.contains(&(port - 1)), "{port}");
            assert!(ports.contains(&(port + 1)), "{port}");
        }
        assert_eq!(bindable(&[]).len(), usize::from(u16::MAX) + 1);
    }

    #[test]
    fn an_open_network_policy_keeps_the_owner_model_and_proxy_ports_free() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let sandbox = sandbox.keep_ports(&[8080, 11434, 1234]).unwrap();
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let policy = landlock_policy(&grants, &sandbox);
        assert!(!policy.limit_network);
        let mut expected = vec![1234, 1455, 8080, 11434];
        expected.extend(PROXY_PORTS);
        expected.extend(kernel_sandbox::ports::RELAY_PORTS);
        assert_eq!(policy.keep_free, expected);
        assert_eq!(policy.keep_free.len(), 4 + 256);
        assert_eq!(
            HelperRequest::parse(
                policy
                    .args()
                    .into_iter()
                    .chain(["--".into(), "/bin/true".into()])
                    .collect()
            ),
            Ok(HelperRequest::Run {
                policy: Box::new(policy),
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
    }

    #[test]
    fn an_open_network_is_refused_when_linux_could_hand_out_a_kept_port() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let usual = PICKED_PORTS_START..=60_999;
        let kept = kept_free(&sandbox.clone().keep_ports(&[8080, 18_080]).unwrap());
        assert_eq!(
            kept_outside_picked(&kept, Some(&usual)),
            Ok(()),
            "the owner ports and the whole proxy block lie below the ports Linux hands out"
        );
        let inside = kept_free(&sandbox.clone().keep_ports(&[40_000]).unwrap());
        let reason = kept_outside_picked(&inside, Some(&usual)).unwrap_err();
        assert!(reason.starts_with("port 40000 is kept free"), "{reason}");
        assert!(reason.contains("ports 32768 to 60999"), "{reason}");
        let moved = kept_outside_picked(&kept, Some(&(1024..=65_535))).unwrap_err();
        assert!(moved.starts_with("port 1455 is kept free"), "{moved}");
        assert_eq!(
            kept_outside_picked(&kept, None),
            Err(PICKED_PORTS_UNREADABLE.to_owned()),
            "a range that cannot be read starts no open-network agent"
        );
        assert!(kept_outside_picked(&[], None).is_err());
    }

    #[test]
    fn a_kept_port_inside_this_machines_picked_ports_refuses_every_landlock_launch_only() {
        let picked = read_picked_ports().expect("Linux test machines show their port range");
        let inside = (*picked.start()).max(1);
        let helper = std::env::current_exe().unwrap();
        let landlock = || LinuxBackend::Landlock {
            helper: helper.clone(),
        };
        let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let closed = sandbox.keep_ports(&[inside]).unwrap();
        let bubblewrap = LinuxBackend::Bubblewrap {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        assert!(
            LinuxSandbox::new(closed, bubblewrap).is_ok(),
            "bubblewrap starts a closed agent in a network of its own"
        );
        for network in [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        ] {
            let (_kept, sandbox) = project(network.clone());
            let sandbox = sandbox.keep_ports(&[inside]).unwrap();
            match LinuxSandbox::new(sandbox, landlock()) {
                Err(SandboxError::Unavailable(reason)) => {
                    assert!(reason.contains(&format!("port {inside} ")), "{reason}");
                }
                other => panic!(
                    "a kept port Linux could pick must refuse the launch on {network:?}: {other:?}"
                ),
            }
        }
    }

    #[test]
    fn a_limited_network_policy_names_the_kept_ports_for_the_helper_to_check() {
        let (_temp, sandbox) = project(SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]));
        let sandbox = sandbox.keep_ports(&[8080]).unwrap();
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let policy = landlock_policy(&grants, &sandbox);
        assert!(policy.limit_network);
        let mut expected = vec![1455, 8080, 11434];
        expected.extend(PROXY_PORTS);
        expected.extend(kernel_sandbox::ports::RELAY_PORTS);
        assert_eq!(policy.keep_free, expected);
        assert_eq!(
            HelperRequest::parse(
                policy
                    .args()
                    .into_iter()
                    .chain(["--".into(), "/bin/true".into()])
                    .collect()
            ),
            Ok(HelperRequest::Run {
                policy: Box::new(policy),
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
    }

    #[test]
    fn the_helper_refuses_kept_ports_that_do_not_fit_its_policy() {
        let parse = |args: &[&str]| HelperRequest::parse(args.iter().map(OsString::from).collect());
        assert_eq!(
            parse(&[PROBE, KEEP_FREE, "1455"]),
            Ok(HelperRequest::Probe {
                limit_network: false,
                open_sockets: false,
                keep_free: true,
                cut: None,
            })
        );
        assert_eq!(
            parse(&[PROBE]),
            Ok(HelperRequest::Probe {
                limit_network: false,
                open_sockets: false,
                keep_free: false,
                cut: None,
            })
        );
        assert_eq!(
            parse(&[PROBE, LIMIT_NETWORK, KEEP_FREE, "1455"]),
            Ok(HelperRequest::Probe {
                limit_network: true,
                open_sockets: false,
                keep_free: true,
                cut: None,
            })
        );
        assert_eq!(
            parse(&[LIMIT_NETWORK, KEEP_FREE, "1455", "--", "/bin/true"]),
            Ok(HelperRequest::Run {
                policy: Box::new(LandlockPolicy {
                    limit_network: true,
                    keep_free: vec![1455],
                    ..LandlockPolicy::default()
                }),
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
        for refused in [
            &[KEEP_FREE, "0", "--", "/bin/true"][..],
            &[KEEP_FREE, "--", "/bin/true"],
            &[PROBE, KEEP_FREE],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        let many: Vec<OsString> = (1..=KEEP_FREE_MAX + 1)
            .flat_map(|port| [OsString::from(KEEP_FREE), OsString::from(port.to_string())])
            .chain(["--".into(), "/bin/true".into()])
            .collect();
        assert_eq!(
            HelperRequest::parse(many),
            Err("too many kept ports".into())
        );
    }

    #[test]
    fn bubblewrap_starts_no_open_network_agent_since_it_keeps_no_port_free() {
        let bwrap = std::env::current_exe().unwrap();
        let backend = || LinuxBackend::Bubblewrap {
            bwrap: bwrap.clone(),
            helper: bwrap.clone(),
        };
        let (_open, sandbox) = project(SandboxNetwork::Open);
        match LinuxSandbox::new(sandbox, backend()) {
            Err(SandboxError::Unavailable(reason)) => {
                assert_eq!(reason, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK);
            }
            other => panic!("an open network on bubblewrap must be refused: {other:?}"),
        }
        let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        assert!(LinuxSandbox::new(sandbox, backend()).is_ok());
    }

    /// The refusal a detection gives, for the rows that expect one. Input: the detection.
    /// Output: its reason; a sandbox or another error fails the row.
    fn refusal(detected: Result<LinuxSandbox, SandboxError>) -> String {
        match detected {
            Err(SandboxError::Unavailable(reason)) => reason,
            other => panic!("a refusal was expected: {other:?}"),
        }
    }

    /// V243-LINUX-KEEP-FREE-REFUSAL-NAME: a probe that says the kernel cannot hold the
    /// boundary keeps the kernel refusals; any other failure is named, its first line cut to
    /// 200 bytes with no control characters. A network with ports never widens to open; a
    /// closed network keeps its bubblewrap fallback, and without bubblewrap names the reason.
    #[test]
    fn a_failed_probe_names_its_own_reason_and_only_an_enforcement_answer_names_the_kernel() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bwrap = std::env::current_exe().unwrap();
        let fake = |name: &str, said: &str| {
            let path = root.join(name);
            fs::write(&path, format!("#!/bin/sh\nprintf '{said}' >&2\nexit 3\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let detected = |helper: &Path, network: SandboxNetwork| {
            let (_folders, sandbox) = project(network);
            LinuxSandbox::detect_among(sandbox, helper, &[bwrap.as_path()])
        };
        let ports = || SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]);
        let falls_back = |helper: &Path| {
            let closed = detected(helper, SandboxNetwork::Only(Vec::new()));
            matches!(
                closed.as_ref().map(LinuxSandbox::backend),
                Ok(LinuxBackend::Bubblewrap { .. })
            )
        };
        let no_bubblewrap = |helper: &Path| {
            let (_folders, sandbox) = project(SandboxNetwork::Only(Vec::new()));
            refusal(LinuxSandbox::detect_among(sandbox, helper, &[]))
        };
        for status in ["PartiallyEnforced", "NotEnforced"] {
            let said =
                format!("crazyagent-landlock: this kernel cannot hold the boundary ({status})\\n");
            let helper = fake(status, &said);
            assert_eq!(
                refusal(detected(&helper, SandboxNetwork::Open)),
                LINUX_OPEN_NETWORK_NEEDS_LANDLOCK
            );
            let limited = refusal(detected(&helper, ports()));
            assert!(
                limited.starts_with("limiting an agent's network to a few ports needs Landlock"),
                "{limited}"
            );
            assert!(falls_back(&helper), "{status}");
            let pinned = no_bubblewrap(&helper);
            assert!(
                pinned.starts_with("this Linux cannot fully enforce Landlock file rules"),
                "{pinned}"
            );
        }
        let other = fake(
            "other",
            "crazyagent-landlock: seccomp\\007 filter refused\\nsecond line\\n",
        );
        let named = "the sandbox check failed: crazyagent-landlock: seccomp filter refused";
        assert_eq!(refusal(detected(&other, SandboxNetwork::Open)), named);
        assert_eq!(refusal(detected(&other, ports())), named);
        assert!(falls_back(&other));
        assert_eq!(no_bubblewrap(&other), named);
        let silent = fake("silent", "");
        assert_eq!(
            refusal(detected(&silent, SandboxNetwork::Open)),
            "the sandbox check failed: exit status: 3"
        );
        let wide = fake("wide", &"é".repeat(150));
        let cut = refusal(detected(&wide, SandboxNetwork::Open));
        let reason = cut.strip_prefix("the sandbox check failed: ").unwrap();
        assert!(
            reason.len() <= 200 && reason.len() > 190,
            "{}",
            reason.len()
        );
        assert!(reason.chars().all(|letter| letter == 'é'));
    }

    /// V243-LINUX-PROBE-STDERR-CAP: a failed probe that says far more than it should is read
    /// only to [`PROBE_SAID_MAX`] bytes and still ends with its own status; bytes that are not
    /// UTF-8 stay within the cap once replaced.
    #[test]
    fn a_failed_probe_is_read_only_to_its_cap_and_still_ends() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let fake = |name: &str, says: &str| {
            let path = temp.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{says} >&2\nexit 3\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let ascii = fake("ascii", "yes x | head -c 1048576");
        let (said, ended) = probe_said(&mut Command::new(&ascii)).unwrap();
        assert_eq!(said.len(), PROBE_SAID_MAX);
        assert!(said.starts_with("x\nx\n"));
        assert_eq!(ended, "exit status: 3");
        let raw = fake("raw", "head -c 1048576 /dev/zero | tr '\\000' '\\377'");
        let (said, ended) = probe_said(&mut Command::new(&raw)).unwrap();
        assert!(
            said.len() <= PROBE_SAID_MAX && said.len() > PROBE_SAID_MAX - 4,
            "{}",
            said.len()
        );
        assert!(
            said.chars()
                .all(|letter| letter == char::REPLACEMENT_CHARACTER)
        );
        assert_eq!(ended, "exit status: 3");
    }

    /// V243-LINUX-PROBE-STDERR-CAP: what a failed probe said goes to the log as one line, with
    /// its newlines and other control characters escaped, so a probe cannot write log lines
    /// of its own or terminal escapes.
    #[test]
    fn a_failed_probe_is_logged_on_one_line_with_its_control_characters_escaped() {
        let line = probe_log(
            &[OsStr::new("--probe")],
            "exit status: 3",
            "first\nsecond\u{7}\u{1b}[31m red\r",
        );
        assert!(!line.chars().any(char::is_control), "{line:?}");
        assert!(
            line.starts_with(r#"linux_probe_failed: ["--probe"] exit status: 3: "#),
            "{line:?}"
        );
        assert!(
            line.contains(r"first\nsecond\u{7}\u{1b}[31m red\r"),
            "{line:?}"
        );
    }

    /// V243-LINUX-PROBE-STDERR-CAP (#592 F1): what a failed probe that says far too much
    /// writes to the log is one line, built from at most [`PROBE_SAID_MAX`] bytes, with its
    /// control characters escaped. Writing what it said raw fails this row.
    #[test]
    fn a_failed_probe_writes_one_escaped_line_to_the_log() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let talker = temp.path().join("talker");
        let says = "yes \"$(printf 'first\\033[31m red\\r')\" | head -c 1048576 >&2";
        fs::write(&talker, format!("#!/bin/sh\n{says}\nexit 3\n")).unwrap();
        fs::set_permissions(&talker, fs::Permissions::from_mode(0o755)).unwrap();
        let mut log = Vec::new();
        let reason = probe_failure(Command::new(&talker).arg("--probe"), &mut log);
        assert_eq!(reason.as_deref(), Some("first[31m red"));
        let line = String::from_utf8(log).unwrap();
        let body = line
            .strip_suffix('\n')
            .expect("the log line ends in a newline");
        let start: String = body.chars().take(120).collect();
        assert!(!body.chars().any(char::is_control), "{start:?}");
        assert!(
            body.starts_with(
                r#"linux_probe_failed: ["--probe"] exit status: 3: "first\u{1b}[31m red\r\n"#
            ),
            "{start:?}"
        );
        assert!(body.len() <= 6 * PROBE_SAID_MAX + 64, "{}", body.len());
    }

    #[test]
    fn a_closed_bubblewrap_agent_gets_no_name_resolver_and_starts_behind_the_socket_filter() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden: Vec::new(),
        };
        let command = bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &SandboxNetwork::Only(Vec::new()),
        );
        let args: Vec<&OsStr> = command.get_args().collect();
        assert!(
            !args.contains(&OsStr::new("/run/systemd/resolve")),
            "{args:?}"
        );
        let [split, helper, filter] = args[args.len() - 3..] else {
            panic!("{args:?}");
        };
        assert_eq!(split, "--", "{args:?}");
        assert_eq!(
            Path::new(helper).file_name(),
            Some(OsStr::new(HELPER_NAME)),
            "{args:?}"
        );
        assert_eq!(filter, "--filter-sockets", "{args:?}");
        let bound = args
            .windows(3)
            .any(|three| three[0] == "--ro-bind" && three[1] == helper && three[2] == helper);
        assert!(bound, "the helper is bound into the namespace: {args:?}");
    }

    #[test]
    fn a_nested_agent_starts_through_the_helper_then_bubblewrap_then_the_helper_again() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let nested = LinuxSandbox::new(
            sandbox,
            LinuxBackend::Nested {
                bwrap: helper.clone(),
                helper: helper.clone(),
            },
        )
        .unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let command = nested.wrap(&harness).unwrap();
        assert_eq!(command.get_program(), helper.as_os_str());
        let args: Vec<&OsStr> = command.get_args().collect();
        let mut head = vec![OsStr::new(NEST), OsStr::new("--"), helper.as_os_str()];
        head.extend(NEST_ARGS.map(OsStr::new));
        head.extend([
            OsStr::new("--chdir"),
            cwd.as_os_str(),
            OsStr::new("--"),
            helper.as_os_str(),
        ]);
        assert_eq!(args.get(..head.len()), Some(&head[..]), "{args:?}");
        let [policy @ .., split, program, flag, script] = &args[head.len()..] else {
            panic!("{args:?}");
        };
        assert_eq!(*split, "--", "{args:?}");
        assert_eq!(*program, "/bin/sh", "{args:?}");
        assert_eq!(*flag, "-c", "{args:?}");
        assert_eq!(*script, "true", "{args:?}");
        let grants = Grants::of(nested.sandbox()).unwrap();
        let mut expected = landlock_policy(&grants, nested.sandbox());
        expected.nested = true;
        let written: Vec<OsString> = policy.iter().map(|arg| arg.to_os_string()).collect();
        assert_eq!(written, expected.args(), "{args:?}");
        assert!(written.contains(&OsString::from(NESTED)), "{args:?}");
        for absent in ["--new-session", "--unshare-net"] {
            assert!(!args.contains(&OsStr::new(absent)), "{absent}: {args:?}");
        }
        assert_eq!(args.iter().filter(|arg| **arg == NEST).count(), 1);
    }

    #[test]
    fn only_a_loopback_only_tcp_only_agent_with_ports_from_1024_up_is_bridged() {
        use SandboxRemote::{AnyHost, Loopback};
        let bridged = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            sandbox.limit_sockets(sockets).bridged_ports()
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(
            bridged(&[Loopback(11434), Loopback(8080)], default),
            Some(vec![8080, 11434])
        );
        assert_eq!(bridged(&[Loopback(1024)], default), Some(vec![1024]));
        assert_eq!(bridged(&[Loopback(8080), AnyHost(443)], default), None);
        assert_eq!(bridged(&[Loopback(8080), Loopback(1023)], default), None);
        assert_eq!(bridged(&[], default), None);
        assert_eq!(bridged(&[Loopback(8080)], LinuxSockets::TcpPorts), None);
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.bridged_ports(), None);
    }

    #[test]
    fn only_a_tcp_only_agent_with_a_port_on_any_host_and_loopback_ports_from_1024_up_is_routed() {
        use SandboxRemote::{AnyHost, Loopback};
        let routed = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            let sandbox = sandbox.limit_sockets(sockets);
            let routed = sandbox.routed();
            assert!(routed.is_none() || sandbox.bridged_ports().is_none());
            routed.map(|routed| (routed.any, routed.bridged))
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(routed(&[AnyHost(443)], default), Some((vec![443], vec![])));
        assert_eq!(
            routed(&[Loopback(18080), AnyHost(8443), AnyHost(443)], default),
            Some((vec![443, 8443], vec![18080]))
        );
        assert_eq!(
            routed(&[AnyHost(443), Loopback(1024)], default),
            Some((vec![443], vec![1024]))
        );
        assert_eq!(routed(&[AnyHost(443), Loopback(1023)], default), None);
        assert_eq!(routed(&[AnyHost(443), Loopback(53)], default), None);
        assert_eq!(
            routed(&[Loopback(8080)], default),
            None,
            "the bridge's case"
        );
        assert_eq!(routed(&[], default), None);
        assert_eq!(routed(&[AnyHost(443)], LinuxSockets::TcpPorts), None);
        assert_eq!(routed(&[AnyHost(443)], LinuxSockets::Refuse), None);
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.routed(), None);
    }

    #[test]
    fn a_routed_agent_starts_with_its_route_flags_first_and_a_network_of_its_own() {
        use bridge::{BRIDGE, UNSHARE_NET};
        let (_temp, sandbox) = project(SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(18080),
        ]));
        let cwd = sandbox.project_root().to_path_buf();
        let grants = Grants::of(&sandbox).unwrap();
        let policy = LandlockPolicy {
            nested: true,
            ..landlock_policy(&grants, &sandbox)
        };
        let bwrap = Path::new(BWRAP_PATHS[0]);
        let helper = helper();
        let words = |bridged: &[u16], routes: &[u16]| -> Vec<OsString> {
            nest_command(bwrap, &helper, &policy, &[], &cwd, bridged, routes)
                .get_args()
                .map(OsStr::to_os_string)
                .collect()
        };
        let expected = |head: &[&str]| -> Vec<OsString> {
            let mut expected: Vec<OsString> = head.iter().map(OsString::from).collect();
            expected.extend([NEST, "--"].map(OsString::from));
            expected.push(bwrap.into());
            expected.extend(NEST_ARGS.map(OsString::from));
            expected.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
            expected.push(cwd.clone().into());
            expected.push("--".into());
            expected.push(helper.clone().into());
            expected.extend(policy.args());
            expected
        };
        let both = [egress::ROUTE, "443", BRIDGE, "18080"];
        assert_eq!(words(&[18080], &[443]), expected(&both));
        let routes = [egress::ROUTE, "443", egress::ROUTE, "8443"];
        assert_eq!(words(&[], &[443, 8443]), expected(&routes));
        for routed in [words(&[18080], &[443]), words(&[], &[443])] {
            for flag in egress::DESCRIPTOR_FLAGS {
                assert!(
                    !routed.iter().any(|word| word == flag),
                    "{flag}: {routed:?}"
                );
            }
        }
    }

    #[test]
    fn route_flags_lead_only_a_nest_that_unshares_the_network_and_no_caller_names_its_descriptors()
    {
        use bridge::{BRIDGE, UNSHARE_NET};
        let nest = |head: &[&str], options: &[&str]| -> Vec<OsString> {
            let mut args: Vec<OsString> = head.iter().map(OsString::from).collect();
            args.extend([NEST, "--", BWRAP_PATHS[0]].map(OsString::from));
            args.extend(NEST_ARGS.map(OsString::from));
            args.extend(options.iter().map(OsString::from));
            args.push("--".into());
            args.push(helper().into());
            args.push(PROBE.into());
            args
        };
        let read = |args: Vec<OsString>| {
            read_request(args).map(|(routes, bridge, _, _)| (routes, bridge.map(|held| held.ports)))
        };
        let route = [egress::ROUTE, "443"];
        let bridged = [BRIDGE, "18080"];
        assert_eq!(read(nest(&route, &[UNSHARE_NET])), Ok((vec![443], None)));
        let both = [egress::ROUTE, "443", egress::ROUTE, "8443", BRIDGE, "18080"];
        assert_eq!(
            read(nest(&both, &[UNSHARE_NET])),
            Ok((vec![443, 8443], Some(vec![18080])))
        );
        assert_eq!(read(nest(&[], &[])), Ok((Vec::new(), None)));
        assert!(
            read(nest(&route, &[])).is_err(),
            "a route needs --unshare-net"
        );
        assert!(
            read(nest(&[], &[UNSHARE_NET])).is_err(),
            "--unshare-net needs a port"
        );
        let mut probe: Vec<OsString> = route.iter().map(OsString::from).collect();
        probe.push(PROBE.into());
        assert!(read(probe).is_err(), "a route leads a nest only");
        for options in [
            &[UNSHARE_NET, "--info-fd", "5"][..],
            &[UNSHARE_NET, "--block-fd", "6"],
            &[UNSHARE_NET, "--ro-bind-data", "7", "/etc/resolv.conf"],
            &["--info-fd", "5", UNSHARE_NET],
            &[UNSHARE_NET, "--args", "5"],
            &[UNSHARE_NET, "--userns-block-fd", "5"],
            &[UNSHARE_NET, "--sync-fd", "5"],
            &[UNSHARE_NET, "--json-status-fd", "5"],
        ] {
            assert!(read(nest(&route, options)).is_err(), "{options:?}");
            assert!(read(nest(&bridged, options)).is_err(), "{options:?}");
        }
    }

    #[test]
    fn a_slirp4netns_or_bubblewrap_that_others_may_write_to_is_never_run() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |mode: u32| {
            let path = root.join(format!("slirp4netns-{mode:o}"));
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        assert!(trusted(&moded(0o755)));
        assert!(trusted(&moded(0o700)));
        for refused in [0o757, 0o775, 0o777, 0o722] {
            assert!(!trusted(&moded(refused)), "{refused:o}");
        }
    }

    #[test]
    fn a_set_user_or_set_group_id_bubblewrap_is_never_nested_under() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |name: &str, mode: u32| {
            let path = root.join(name);
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let plain = moded("plain", 0o755);
        let marked = [moded("setuid", 0o4755), moded("setgid", 0o2755)];
        let agreeing = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|folder| folder.join("true"))
            .find(|path| path.is_file())
            .unwrap();
        let probe = [OsString::from(PROBE)];
        assert!(nests(&plain, &agreeing, &probe), "the control");
        assert!(trusted(&plain));
        assert!(trusted(Path::new("/bin/sh")), "a program root owns");
        assert!(!trusted(&root.join("absent")));
        assert!(!trusted(&root), "a folder");
        for bwrap in &marked {
            assert!(!trusted(bwrap), "{}", bwrap.display());
            let (_temp, sandbox) = project(SandboxNetwork::Open);
            let backend = LinuxBackend::Nested {
                bwrap: bwrap.clone(),
                helper: plain.clone(),
            };
            let refused = LinuxSandbox::new(sandbox, backend).is_err();
            assert!(refused, "{}", bwrap.display());
            assert!(!nests(bwrap, &agreeing, &probe), "{}", bwrap.display());
        }
    }

    /// rv-sa241 SBX-2-R2 LOW-1: where this computer's nest check passes, a launch whose own
    /// nest probe fails is refused with words to try again or restart, instead of running on
    /// the shared network with Landlock rules only while the Agent access page says it is
    /// nested; where the nest check fails too, the launch is a Landlock one, as the page says.
    #[test]
    fn a_launch_whose_own_nest_probe_fails_where_this_computer_nests_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let place = temp.path().canonicalize().unwrap();
        let bwrap = place.join("bwrap");
        fs::write(&bwrap, b"").unwrap();
        fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(trusted(&bwrap));
        let fake = |name: &str, failing: &str| {
            let helper = place.join(name);
            let script = format!(
                "#!/bin/sh\nnest=0\nfailing=0\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    \
                 {NEST}) nest=1 ;;\n    {failing}) failing=1 ;;\n  esac\ndone\n\
                 [ \"$nest\" = 1 ] && [ \"$failing\" = 1 ] && exit 1\nexit 0\n"
            );
            fs::write(&helper, script).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            helper
        };
        let flagged = fake("flagged", KEEP_FREE);
        let neither = fake("neither", PROBE);
        assert!(
            nests(&bwrap, &flagged, &[PROBE.into()]),
            "this computer nests"
        );
        let (_project, sandbox) = project(SandboxNetwork::Open);
        let answer = LinuxSandbox::detect_among(sandbox.clone(), &flagged, &[bwrap.as_path()]);
        let shown = match &answer {
            Ok(sandbox) => format!("{:?}", sandbox.backend()),
            Err(error) => error.to_string(),
        };
        assert!(
            matches!(
                &answer,
                Err(SandboxError::Unavailable(reason)) if reason.contains("restart CrazyAgent")
            ),
            "a launch that cannot nest where this computer nests is refused, never run on \
             Landlock alone: {shown}"
        );
        assert!(
            !nests(&bwrap, &neither, &[PROBE.into()]),
            "this computer does not nest"
        );
        let landlock = LinuxSandbox::detect_among(sandbox, &neither, &[bwrap.as_path()]).unwrap();
        assert!(
            matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
            "where nothing nests the launch is a Landlock one, as the page says"
        );
    }

    #[test]
    fn an_agent_kept_on_this_network_by_a_low_port_names_the_lowest_one() {
        use SandboxRemote::{AnyHost, Loopback};
        let low = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            let sandbox = sandbox.limit_sockets(sockets);
            assert!(sandbox.unbridged_port().is_none() || sandbox.bridged_ports().is_none());
            sandbox.unbridged_port()
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(
            low(&[Loopback(8080), Loopback(443), Loopback(80)], default),
            Some(80)
        );
        assert_eq!(low(&[Loopback(1023)], default), Some(1023));
        assert_eq!(low(&[Loopback(1024)], default), None, "bridged");
        assert_eq!(
            low(&[Loopback(80), AnyHost(443)], default),
            None,
            "a host port"
        );
        assert_eq!(low(&[], default), None);
        assert_eq!(
            low(&[Loopback(80)], LinuxSockets::TcpPorts),
            None,
            "the setting"
        );
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.unbridged_port(), None);
    }

    #[test]
    fn a_project_may_hold_another_git_repository_below_its_top_or_where_none_is_ruled_out() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git/hooks")).unwrap();
        fs::write(project.join(".git/config"), "").unwrap();
        fs::create_dir_all(project.join("src/deep")).unwrap();
        fs::write(project.join("src/deep/.gitignore"), "").unwrap();
        fs::write(project.join("a.git"), "").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(outside.join(".git")).unwrap();
        symlink(&outside, project.join("linked")).unwrap();
        assert!(
            !holds_other_git(&project),
            "its own .git, and a link is not followed"
        );
        let found = |make: &dyn Fn(&Path), undo: &dyn Fn(&Path)| {
            make(&project);
            let held = holds_other_git(&project);
            undo(&project);
            assert!(!holds_other_git(&project));
            held
        };
        for name in ["vendor/lib/.git", "vendor/.GIT", "vendor/.git.", ".Git"] {
            let folder = |project: &Path| fs::create_dir_all(project.join(name)).unwrap();
            let gone = |project: &Path| fs::remove_dir(project.join(name)).unwrap();
            assert!(found(&folder, &gone), "{name}");
        }
        fs::create_dir_all(project.join("vendor/lib")).unwrap();
        let link = |project: &Path| {
            fs::write(project.join("vendor/lib/.git"), "gitdir: x").unwrap();
        };
        let unlink = |project: &Path| fs::remove_file(project.join("vendor/lib/.git")).unwrap();
        assert!(
            found(&link, &unlink),
            "a .git file is rewritten without a create"
        );
        let pointer = |project: &Path| symlink(&outside, project.join("vendor/.git")).unwrap();
        let unpoint = |project: &Path| fs::remove_file(project.join("vendor/.git")).unwrap();
        assert!(found(&pointer, &unpoint), "a link named .git counts");
        let mode = |project: &Path, bits| {
            let permissions = fs::Permissions::from_mode(bits);
            fs::set_permissions(project.join("src/deep"), permissions).unwrap();
        };
        let locked = |project: &Path| mode(project, 0o000);
        let unlocked = |project: &Path| mode(project, 0o755);
        if !rustix::process::geteuid().is_root() {
            assert!(found(&locked, &unlocked), "a folder it cannot read");
        }
        let many = |project: &Path| {
            fs::create_dir(project.join("many")).unwrap();
            for index in 0..GIT_SEARCH_ENTRIES_MAX {
                fs::write(project.join("many").join(index.to_string()), "").unwrap();
            }
        };
        let few = |project: &Path| fs::remove_dir_all(project.join("many")).unwrap();
        assert!(found(&many, &few), "more entries than it reads");
        assert!(
            holds_other_git(&temp.path().join("missing")),
            "no folder to read"
        );
    }

    #[test]
    fn a_launch_left_on_this_network_by_a_low_port_says_so_with_the_port() {
        use super::super::{LinuxHost, linux_unbridged_line, linux_unbridged_notice_on};
        use SandboxRemote::{AnyHost, Loopback};
        use kernel_sandbox::policy::AgentNetwork;
        let host = |nested| {
            Some(LinuxHost {
                nested,
                scoped: false,
                supervised: false,
                root: false,
                armored: false,
                routed: false,
            })
        };
        let launched = |remotes: &[SandboxRemote]| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            sandbox
        };
        let said = |nested, sandbox: &ProjectSandbox| {
            linux_unbridged_notice_on(host(nested), AgentNetwork::None, sandbox)
        };
        let low = launched(&[Loopback(11434), Loopback(80)]);
        assert_eq!(said(true, &low), Some(linux_unbridged_line(80)));
        assert!(said(true, &low).unwrap().contains("port 80 is below 1024"));
        assert_eq!(said(false, &low), None, "no nest");
        assert_eq!(
            linux_unbridged_notice_on(None, AgentNetwork::None, &low),
            None
        );
        assert_eq!(said(true, &launched(&[Loopback(11434)])), None, "bridged");
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(said(true, &open), None);
        let plain = Some(super::super::LINUX_PORTS_ANY_HOST_LINE.to_owned());
        let unrouted = Some(super::super::LINUX_UNROUTED_LINE.to_owned());
        let hosted = launched(&[AnyHost(443), Loopback(8080)]);
        assert_eq!(
            said(true, &hosted),
            unrouted,
            "a model on a host, tools off"
        );
        assert_eq!(
            said(false, &hosted),
            None,
            "the page says it without a nest"
        );
        let ports = launched(&[Loopback(8080)]).limit_sockets(LinuxSockets::TcpPorts);
        assert_eq!(said(true, &ports), plain);
        let any_host = Some(super::super::LINUX_UNROUTED_ANY_HOST_LINE.to_owned());
        assert_eq!(
            said(true, &launched(&[AnyHost(443)])),
            any_host,
            "unrouted, any host"
        );
        assert_eq!(said(true, &launched(&[])), None);
        for choice in [AgentNetwork::Https, AgentNetwork::Open] {
            let said = |sandbox| linux_unbridged_notice_on(host(true), choice, sandbox);
            assert_eq!(said(&hosted), unrouted, "{choice:?}: no network of its own");
            assert_eq!(said(&ports), None, "{choice:?}: the page says it");
            assert_eq!(said(&low), Some(linux_unbridged_line(80)), "{choice:?}");
        }
    }

    #[test]
    fn every_unrouted_launch_that_could_route_says_why_and_a_low_port_names_itself() {
        use super::super::{LINUX_UNROUTED_ANY_HOST_LINE, LINUX_UNROUTED_LINE, LinuxHost};
        use super::super::{linux_unbridged_line, linux_unbridged_notice_on};
        use SandboxRemote::{AnyHost, Loopback};
        use kernel_sandbox::policy::AgentNetwork;
        let host = |routed, supervised| {
            Some(LinuxHost {
                nested: true,
                scoped: false,
                supervised,
                root: false,
                armored: false,
                routed,
            })
        };
        let launched = |remotes: &[SandboxRemote]| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            sandbox
        };
        let said = |held, sandbox: &ProjectSandbox| {
            linux_unbridged_notice_on(held, AgentNetwork::Https, sandbox)
        };
        let wide = Some(LINUX_UNROUTED_LINE.to_owned());
        let any_host = Some(LINUX_UNROUTED_ANY_HOST_LINE.to_owned());
        let model = launched(&[AnyHost(443), Loopback(18080)]);
        let remote = launched(&[AnyHost(443)]);
        assert!(model.routed().is_some() && remote.routed().is_some());
        assert_eq!(said(host(false, false), &model), wide);
        assert_eq!(
            said(host(false, true), &model),
            any_host,
            "loopback kept by the supervisor"
        );
        assert_eq!(
            said(host(false, false), &remote),
            any_host,
            "no loopback port"
        );
        assert_eq!(said(host(false, true), &remote), any_host);
        for supervised in [false, true] {
            assert_eq!(said(host(true, supervised), &model), None, "routed");
            assert_eq!(said(host(true, supervised), &remote), None, "routed");
        }
        for line in [LINUX_UNROUTED_LINE, LINUX_UNROUTED_ANY_HOST_LINE] {
            let why = "On this Linux, this agent has no network of its own: ";
            assert!(line.starts_with(why), "{line}");
            assert!(
                line.contains("AppArmor") && line.contains("as root"),
                "{line}"
            );
        }
        assert!(LINUX_UNROUTED_LINE.contains("on every host, this computer included."));
        assert!(LINUX_UNROUTED_ANY_HOST_LINE.contains("on any host include this computer"));
        let low = launched(&[AnyHost(443), Loopback(18080), Loopback(80), Loopback(53)]);
        assert!(low.routed().is_none());
        let named = Some(linux_unbridged_line(53));
        assert_eq!(
            said(host(true, false), &low),
            named,
            "the low port keeps it unrouted"
        );
        assert_eq!(said(host(true, true), &low), named);
        assert_eq!(
            said(host(false, false), &low),
            None,
            "the page says it on this host"
        );
    }

    #[test]
    fn the_route_answer_is_asked_once_per_engine_and_never_when_refused() {
        let kept: KeptRoute = Mutex::new(None);
        let probes = &std::cell::Cell::new(0u32);
        let probe = |answer: bool| {
            move || {
                probes.set(probes.get() + 1);
                answer
            }
        };
        let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
        assert!(
            !kept_route(&kept, bwrap, &helper, true, false, probe(true)),
            "as root"
        );
        assert_eq!(probes.get(), 0, "a refusal starts no probe");
        assert!(kept_route(&kept, bwrap, &helper, false, false, probe(true)));
        assert!(
            kept_route(&kept, bwrap, &helper, false, false, probe(false)),
            "kept"
        );
        assert_eq!(probes.get(), 1, "page, notice and launch read one answer");
        assert!(
            !kept_route(&kept, bwrap, &helper, true, false, probe(true)),
            "refused while kept"
        );
        assert!(
            kept_route(&kept, bwrap, &helper, false, false, probe(false)),
            "still kept"
        );
        assert_eq!(probes.get(), 1);
        let other = Path::new("/usr/local/bin/bwrap");
        assert!(
            !kept_route(&kept, other, &helper, false, false, probe(false)),
            "another bubblewrap"
        );
        assert!(
            !kept_route(&kept, other, &helper, false, false, probe(true)),
            "a failed answer too"
        );
        assert_eq!(probes.get(), 2);
    }

    /// rv-sa241 SBX-2-R2 N1: the host read asks a kept false route answer again, so one
    /// failed probe (a timeout while the engine starts under load) leaves launches unrouted
    /// only until the next host read. The owner's read of the project catalog or a project's
    /// agent policy runs that read at most once per `HOST_AGE_MAX`, and an engine nobody
    /// reads keeps the false answer until a restart. A launch reads what is kept and never
    /// waits on the probe, a true answer is never asked again, and a false probe never
    /// replaces a true another caller kept meanwhile.
    #[test]
    fn a_false_route_answer_is_asked_again_by_the_host_read_and_a_true_one_is_kept() {
        let kept: KeptRoute = Mutex::new(None);
        let probes = &std::cell::Cell::new(0u32);
        let probe = |answer: bool| {
            move || {
                probes.set(probes.get() + 1);
                answer
            }
        };
        let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
        assert!(
            !kept_route(&kept, bwrap, &helper, false, false, probe(false)),
            "the first probe failed"
        );
        assert!(
            !kept_route(&kept, bwrap, &helper, false, false, probe(true)),
            "a launch reads the kept false"
        );
        assert_eq!(probes.get(), 1, "a launch never asks a kept answer again");
        assert!(
            kept_route(&kept, bwrap, &helper, false, true, probe(true)),
            "the host read asks a false answer again"
        );
        assert_eq!(probes.get(), 2);
        assert!(
            kept_route(&kept, bwrap, &helper, false, false, probe(false)),
            "a launch reads the new true"
        );
        assert!(
            kept_route(&kept, bwrap, &helper, false, true, probe(false)),
            "a true answer is never asked again"
        );
        assert_eq!(probes.get(), 2);
        assert!(
            !kept_route(&kept, bwrap, &helper, true, true, probe(true)),
            "refused as root whatever is kept"
        );
        assert_eq!(probes.get(), 2);
        let asked = (bwrap.to_path_buf(), helper.clone());
        let racing: KeptRoute = Mutex::new(Some((asked.0.clone(), asked.1.clone(), false)));
        let found_meanwhile = || {
            *racing.lock().unwrap() = Some((asked.0.clone(), asked.1.clone(), true));
            false
        };
        assert!(
            kept_route(&racing, bwrap, &helper, false, true, found_meanwhile),
            "a true another caller kept while this probe failed holds"
        );
        assert!(
            kept_route(&racing, bwrap, &helper, false, false, probe(false)),
            "and stays kept"
        );
    }

    /// rv-sa241 SBX-2-R2 LOW-1: the nest answer the page and every launch read keeps a nest
    /// found for the same bubblewrap and helper without asking again, and asks again after a
    /// nest was not found and for another bubblewrap, whose miss keeps the found one.
    #[test]
    fn a_found_nest_is_kept_and_a_missing_one_is_asked_again() {
        let kept: KeptNest = Mutex::new(None);
        let probes = &std::cell::Cell::new(0u32);
        let probe = |answer: bool| {
            move || {
                probes.set(probes.get() + 1);
                answer
            }
        };
        let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
        assert!(!kept_nest(&kept, bwrap, &helper, probe(false)));
        assert!(
            kept_nest(&kept, bwrap, &helper, probe(true)),
            "a missing nest is asked again"
        );
        assert!(
            kept_nest(&kept, bwrap, &helper, probe(false)),
            "a found one is kept"
        );
        assert_eq!(
            probes.get(),
            2,
            "page, notice and launch read one found nest"
        );
        let other = Path::new("/usr/local/bin/bwrap");
        assert!(
            !kept_nest(&kept, other, &helper, probe(false)),
            "another bubblewrap is asked"
        );
        assert!(
            kept_nest(&kept, bwrap, &helper, probe(false)),
            "its miss keeps the found one"
        );
        assert_eq!(probes.get(), 3);
        let racing: KeptNest = Mutex::new(None);
        let found_meanwhile = || {
            *racing.lock().unwrap() = Some((bwrap.to_path_buf(), helper.clone()));
            false
        };
        assert!(
            kept_nest(&racing, bwrap, &helper, found_meanwhile),
            "a nest another caller found while this probe failed holds"
        );
    }

    #[test]
    fn a_launch_that_could_have_a_network_of_its_own_says_why_it_has_none() {
        use super::super::{LINUX_UNROUTED_LINE, LinuxHost, linux_unbridged_notice_on};
        use SandboxRemote::{AnyHost, Loopback};
        use kernel_sandbox::policy::AgentNetwork;
        let host = |nested, routed| {
            Some(LinuxHost {
                nested,
                scoped: false,
                supervised: false,
                root: false,
                armored: false,
                routed,
            })
        };
        let (_temp, eligible) = project(SandboxNetwork::Only(vec![AnyHost(443), Loopback(18080)]));
        assert!(eligible.routed().is_some());
        let unrouted = Some(LINUX_UNROUTED_LINE.to_owned());
        for choice in AgentNetwork::ALL {
            let said = |held| linux_unbridged_notice_on(held, choice, &eligible);
            assert_eq!(said(host(true, false)), unrouted, "{choice:?}");
            assert_eq!(said(host(true, true)), None, "{choice:?}: routed");
            assert_eq!(said(host(false, false)), None, "{choice:?}: no nest");
        }
        assert!(LINUX_UNROUTED_LINE.contains("has no network of its own: "));
        let ports = eligible.limit_sockets(LinuxSockets::TcpPorts);
        assert!(ports.routed().is_none());
        let said = linux_unbridged_notice_on(host(true, false), AgentNetwork::Https, &ports);
        assert_eq!(said, None, "not routed-eligible, the page says it");
    }

    #[test]
    fn the_bubblewrap_a_nest_starts_is_checked_as_the_file_it_opened() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |name: &str, mode: u32| {
            let path = root.join(name);
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let plain = moded("plain", 0o755);
        let unread = moded("unread", 0o711);
        let setuid = moded("setuid", 0o4755);
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&setuid, &linked).unwrap();
        for held in [&plain, &unread] {
            let file = trusted_file(held).unwrap_or_else(|| panic!("{}", held.display()));
            let opened = file.metadata().unwrap();
            let named = fs::metadata(held).unwrap();
            assert_eq!((opened.dev(), opened.ino()), (named.dev(), named.ino()));
        }
        for refused in [&setuid, &moded("setgid", 0o2755), &linked, &root] {
            assert!(trusted_file(refused).is_none(), "{}", refused.display());
            assert!(!trusted(refused), "{}", refused.display());
        }
        assert!(trusted_file(&root.join("absent")).is_none());
        let file = trusted_file(&plain).unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o4755)).unwrap();
        let mode = file.metadata().unwrap().mode();
        assert_ne!(
            mode & SET_ID_BITS,
            0,
            "the open file is the one a swap changes"
        );
    }

    fn nested_words(network: SandboxNetwork) -> (PathBuf, PathBuf, Vec<OsString>) {
        let (_temp, sandbox) = project(network);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let backend = LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        let nested = LinuxSandbox::new(sandbox, backend).unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let command = nested.wrap(&harness).unwrap();
        let words = command.get_args().map(OsStr::to_os_string).collect();
        (helper, cwd, words)
    }

    #[test]
    fn a_loopback_only_nested_agent_starts_with_a_network_of_its_own_and_its_ports_bridged() {
        use bridge::{BRIDGE, BRIDGE_FD, UNSHARE_NET};
        let loopback = [
            SandboxRemote::Loopback(11434),
            SandboxRemote::Loopback(8080),
        ];
        let (helper, cwd, words) = nested_words(SandboxNetwork::Only(loopback.to_vec()));
        let mut head: Vec<OsString> = [BRIDGE, "8080", BRIDGE, "11434", NEST, "--"]
            .map(OsString::from)
            .to_vec();
        head.push(helper.clone().into());
        head.extend(NEST_ARGS.map(OsString::from));
        head.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
        head.push(cwd.into());
        head.push("--".into());
        head.push(helper.into());
        assert_eq!(words.get(..head.len()), Some(&head[..]), "{words:?}");
        let inside = &words[head.len()..];
        let written = |word: &OsString| word == BRIDGE || word == BRIDGE_FD;
        assert!(
            !inside.iter().any(written),
            "the nest writes them: {words:?}"
        );
        let (bridge, rest) = Bridge::leading(words.clone()).unwrap();
        let asked = Bridge {
            fd: None,
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(asked));
        assert_eq!(rest.first(), Some(&OsString::from(NEST)));
        let low = [SandboxRemote::Loopback(8080), SandboxRemote::Loopback(443)];
        for network in [
            SandboxNetwork::Only(low.to_vec()),
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Open,
        ] {
            let (_, _, words) = nested_words(network.clone());
            assert_eq!(words.first(), Some(&OsString::from(NEST)), "{network:?}");
            assert!(!words.iter().any(|word| word == UNSHARE_NET), "{network:?}");
        }
    }

    #[test]
    fn a_nested_agent_may_have_an_open_network_and_keeps_the_kept_port_check() {
        let picked = read_picked_ports().expect("Linux test machines show their port range");
        let inside = (*picked.start()).max(1);
        let helper = std::env::current_exe().unwrap();
        let nested = || LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        let (_open, sandbox) = project(SandboxNetwork::Open);
        assert!(
            LinuxSandbox::new(sandbox, nested()).is_ok(),
            "a nested agent is a Landlock agent, which keeps ports free on an open network"
        );
        let (_kept, sandbox) = project(SandboxNetwork::Open);
        match LinuxSandbox::new(sandbox.keep_ports(&[inside]).unwrap(), nested()) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains(&format!("port {inside} ")), "{reason}");
            }
            other => panic!("a kept port Linux could pick must refuse the nest: {other:?}"),
        }
    }

    /// rv-code4 F1 (#622): a second mount of a hidden folder's disk that shows the folder, a
    /// folder above it or one inside it refuses the launch wherever it is mounted (farm run
    /// 44410206144 read the folder through one); another disk, a folder beside it and the
    /// folder's own mount do not, and a mount list that does not read plainly refuses too.
    #[test]
    fn a_second_mount_of_a_hidden_folders_disk_refuses_the_launch() {
        let line = |id: u32, device: &str, root: &str, point: &str| {
            format!("{id} 1 {device} {root} {point} rw,relatime shared:1 - ext4 /dev/sda1 rw\n")
        };
        let base = line(21, "8:1", "/", "/") + &line(22, "8:2", "/", "/boot");
        let reason = |hidden: &str, text: &str| match mounted_again(&[PathBuf::from(hidden)], text)
        {
            Ok(()) => None,
            Err(SandboxError::Unavailable(reason)) => Some(reason),
            Err(other) => panic!("{other:?}"),
        };
        let folder = "/home/u/work/notes/private";
        let again = Some(HIDDEN_MOUNTED_AGAIN.to_owned());
        let unread = Some(HIDDEN_MOUNTS_UNREAD.to_owned());
        assert_eq!(reason(folder, &base), None);
        let seconds = [
            (line(23, "8:1", "/home/u", "/mnt/alias"), &again),
            (line(23, "8:1", "/", "/srv/root"), &again),
            (line(23, "8:1", folder, "/srv/view"), &again),
            (
                line(23, "8:1", "/home/u/work/notes/private/sub", "/srv/part"),
                &again,
            ),
            (
                line(23, "8:1", "/home/u", "/home/u/work/worktrees/alias"),
                &again,
            ),
            (line(23, "8:1", "/home/u", "/mnt/my\\040alias"), &again),
            (
                line(23, "8:1", "/home/u/work/context", "/srv/context"),
                &None,
            ),
            (
                line(23, "8:1", "/home/u/work/my\\040notes", "/srv/notes"),
                &None,
            ),
            (line(23, "8:9", "/home/u", "/mnt/other"), &None),
            (line(23, "0:4", "net:[4026532]", "/run/netns/one"), &None),
        ];
        for (second, expected) in seconds {
            assert_eq!(
                reason(folder, &(base.clone() + &second)),
                *expected,
                "{second}"
            );
        }
        let spaced = base.clone() + &line(23, "8:1", "/home/u/my\\040work", "/srv/spaced");
        assert_eq!(reason("/home/u/my work/private", &spaced), again);
        let own = line(21, "8:1", "/", "/") + &line(24, "8:3", "/u", "/home");
        assert_eq!(reason(folder, &own), None);
        assert_eq!(
            reason(folder, &(own + &line(25, "8:3", "/", "/mnt/home"))),
            again
        );
        for broken in [
            String::new(),
            base.clone() + "garbage\n",
            base.clone() + &line(23, "8:1", "/a\\0", "/b"),
            base.clone() + &line(23, "8:1", "/a\\08x", "/b"),
            base.clone() + &line(23, "eight", "/a", "/b"),
            base.clone() + &line(23, "8:1", "/a", "relative"),
        ] {
            assert_eq!(reason(folder, &broken), unread, "{broken}");
        }
    }

    /// V24x-LINUX-NEST-MOUNT-PROPAGATION: a launch with hidden folders asks the helper for a
    /// private tree with each folder as a cover on both hiding backends, the nested policy
    /// reading back from its own words, and a launch without them asks for none.
    #[test]
    fn a_launch_with_hidden_folders_asks_the_helper_for_a_private_tree_on_both_backends() {
        let folder = PathBuf::from("/w/project/private");
        let policy = LandlockPolicy {
            read: vec![PathBuf::from("/usr")],
            nested: true,
            cut: Some(vec![folder.clone()]),
            ..LandlockPolicy::default()
        };
        let args = policy.args();
        let at = args
            .iter()
            .position(|arg| arg == PRIVATE_MOUNTS)
            .expect("the nested run asks for a private tree");
        assert_eq!(
            args[at + 1..at + 3],
            [OsString::from(COVER), folder.clone().into_os_string()]
        );
        let mut words = args.clone();
        words.extend([OsString::from("--"), OsString::from("/bin/true")]);
        match HelperRequest::parse(words) {
            Ok(HelperRequest::Run { policy: back, .. }) => assert_eq!(*back, policy),
            other => panic!("the nested policy reads back: {other:?}"),
        }
        let plain = LandlockPolicy {
            cut: None,
            ..policy.clone()
        };
        assert!(!plain.args().iter().any(|arg| arg == PRIVATE_MOUNTS));
        let grants = |hidden: Vec<PathBuf>| Grants {
            read: Vec::new(),
            write: Vec::new(),
            git_write: Vec::new(),
            guarded: Held::default(),
            hidden,
        };
        let tail = |grants: &Grants| -> Vec<OsString> {
            bwrap_command(
                Path::new(BWRAP_PATHS[0]),
                &helper(),
                grants,
                Path::new("/tmp"),
                &SandboxNetwork::Open,
            )
            .get_args()
            .skip_while(|arg| *arg != FILTER_SOCKETS)
            .map(OsStr::to_os_string)
            .collect()
        };
        assert_eq!(
            tail(&grants(vec![folder.clone()])),
            [
                OsString::from(FILTER_SOCKETS),
                OsString::from(PRIVATE_MOUNTS),
                OsString::from(COVER),
                folder.into_os_string(),
            ]
        );
        assert_eq!(tail(&grants(Vec::new())), [OsString::from(FILTER_SOCKETS)]);
    }

    fn guarded() -> LandlockPolicy {
        LandlockPolicy {
            read: vec![PathBuf::from("/usr")],
            connect: vec![443, 8080],
            limit_network: true,
            guard: Guard {
                loopback: vec![8080],
                git: Some(GitOwn {
                    trees: vec![PathBuf::from("/p")],
                    kids: vec![PathBuf::from("/p/worktrees")],
                    exact: vec![PathBuf::from("/p/modules/.git")],
                }),
                signals: true,
                held: Some(Held {
                    entries: vec![PathBuf::from("/p/.git/worktrees/a/HEAD")],
                    trees: vec![PathBuf::from("/p/.git/worktrees/a/modules")],
                    stores: vec![PathBuf::from("/c/.git/objects")],
                }),
            },
            ..LandlockPolicy::default()
        }
    }

    #[test]
    fn a_guarded_policy_reads_back_from_its_own_arguments() {
        let policy = guarded();
        let mut words = policy.args();
        let guard: Vec<OsString> = [
            GUARD_LOOPBACK,
            "8080",
            GUARD_GIT,
            OWN_TREE,
            "/p",
            OWN_KIDS,
            "/p/worktrees",
            OWN_GIT,
            "/p/modules/.git",
            GUARD_SIGNALS,
            GUARD_HELD,
            "/p/.git/worktrees/a/HEAD",
            GUARD_HELD_TREE,
            "/p/.git/worktrees/a/modules",
            GUARD_HELD_STORE,
            "/c/.git/objects",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert!(words.ends_with(&guard), "{words:?}");
        words.extend(["--".into(), "/bin/true".into()]);
        assert_eq!(
            HelperRequest::parse(words),
            Ok(HelperRequest::Run {
                policy: Box::new(policy),
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
        assert!(LandlockPolicy::default().guard.args().is_empty());
    }

    #[test]
    fn each_guard_stands_alone_and_a_guard_that_does_not_fit_its_policy_is_refused() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        let held = |words: &[&str]| match parse(words) {
            Ok(HelperRequest::Run { policy, .. }) => Some(policy.guard),
            _ => None,
        };
        let signals = Guard {
            signals: true,
            ..Guard::default()
        };
        let git = Guard {
            git: Some(GitOwn::default()),
            ..Guard::default()
        };
        assert_eq!(held(&[GUARD_SIGNALS, "--", "/bin/true"]), Some(signals));
        assert_eq!(held(&[GUARD_GIT, "--", "/bin/true"]), Some(git));
        assert_eq!(held(&["--", "/bin/true"]), Some(Guard::default()));
        let tree = Guard {
            held: Some(Held {
                entries: Vec::new(),
                trees: vec![PathBuf::from("/p/t")],
                stores: Vec::new(),
            }),
            ..Guard::default()
        };
        assert_eq!(
            held(&[GUARD_HELD_TREE, "/p/t", "--", "/bin/true"]),
            Some(tree)
        );
        let store = Guard {
            held: Some(Held {
                stores: vec![PathBuf::from("/c/s")],
                ..Held::default()
            }),
            ..Guard::default()
        };
        assert_eq!(
            held(&[GUARD_HELD_STORE, "/c/s", "--", "/bin/true"]),
            Some(store)
        );
        for refused in [
            &[OWN_TREE, "/p", "--", "/bin/true"][..],
            &[OWN_KIDS, "/p", GUARD_GIT, "--", "/bin/true"],
            &[GUARD_GIT, OWN_GIT, "p/.git", "--", "/bin/true"],
            &[GUARD_GIT, GUARD_GIT, "--", "/bin/true"],
            &[GUARD_LOOPBACK, "8080", "--", "/bin/true"],
            &[LIMIT_NETWORK, GUARD_LOOPBACK, "8080", "--", "/bin/true"],
            &[GUARD_SIGNALS, DIE_WITH, "4242", "--", "/bin/true"],
            &[PROBE, GUARD_SIGNALS],
            &[PROBE, GUARD_GIT],
            &[FILTER_SOCKETS, GUARD_SIGNALS, "--", "/bin/true"],
            &[GUARD_HELD, "p/.git/worktrees/a/HEAD", "--", "/bin/true"],
            &[GUARD_HELD_TREE, "--", "/bin/true"],
            &[GUARD_HELD, "/p/a", DIE_WITH, "4242", "--", "/bin/true"],
            &[PROBE, GUARD_HELD, "/p/a"],
            &[GUARD_HELD_STORE, "c/s", "--", "/bin/true"],
            &[PROBE, GUARD_HELD_STORE, "/c/s"],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        let limited = [LIMIT_NETWORK, CONNECT, "8080", GUARD_LOOPBACK];
        let ported = |more: &[&'static str]| -> Vec<&'static str> {
            [&limited[..], more, &["--", "/bin/true"][..]].concat()
        };
        for refused in [&["0"][..], &["8081"], &["8080", OPEN_SOCKETS]] {
            assert!(parse(&ported(refused)).is_err(), "{refused:?}");
        }
        let fits = held(&ported(&["8080"]));
        assert_eq!(fits.map(|guard| guard.loopback), Some(vec![8080]));
    }

    #[test]
    fn a_guard_keeps_its_ports_on_this_machine_and_leaves_the_others_to_every_host() {
        let policy = guarded();
        let held = policy.guard.supervised(&policy.connect);
        let reach = held
            .reach
            .as_ref()
            .expect("a guarded port is a guarded connect");
        assert_eq!(reach.any, [443]);
        assert_eq!(reach.loopback, [8080]);
        assert_eq!(held.git, policy.guard.git);
        assert!(held.signals);
        let alone = Guard {
            signals: true,
            ..Guard::default()
        };
        let held = alone.supervised(&policy.connect);
        assert_eq!((held.reach, held.git, held.signals), (None, None, true));
    }

    /// Each supervisor probe gets a new folder only this user may open, under a name no
    /// stale, concurrent or planted folder takes, and none where the data folder refuses
    /// one (rv-secreview linux-t4c F1).
    #[test]
    fn each_supervisor_probe_gets_a_new_private_folder_under_the_engine_data_folder() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let first = private_folder(&data).expect("a folder in the data folder");
        let second = private_folder(&data).expect("a second folder beside it");
        assert_ne!(first, second);
        for folder in [&first, &second] {
            assert_eq!(folder.parent(), Some(data.as_path()));
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(PROBE_FOLDER_PREFIX), "{name}");
            assert!(private_empty(folder), "{}", folder.display());
        }
        let file = data.join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(private_folder(&file), None);
        assert_eq!(private_folder(&data.join("missing")), None);
    }

    /// The helper runs its supervisor probe only in the private folder the engine made for
    /// it: a link to it, a folder others may open, one that holds anything, a file or no
    /// folder at all fails it (rv-secreview linux-t4c F1).
    #[test]
    fn the_supervisor_probe_takes_only_an_empty_folder_only_this_user_may_open() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let private = data.join("private");
        fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        assert!(private_empty(&private));
        let linked = data.join("linked");
        symlink(&private, &linked).unwrap();
        assert!(!private_empty(&linked), "a link to it");
        fs::write(private.join("made"), "").unwrap();
        assert!(!private_empty(&private), "a folder that holds a file");
        fs::remove_file(private.join("made")).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!private_empty(&private), "a folder others may open");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(private_empty(&private));
        let file = data.join("file");
        fs::write(&file, "").unwrap();
        assert!(!private_empty(&file), "a file");
        assert!(!private_empty(&data.join("missing")), "no folder");
    }

    /// The engine makes the probe's folder, hands it to the helper and removes it whatever
    /// the probe answered, so a failed or killed probe leaves nothing a later one trips on,
    /// two probes at once each get their own, and as root no probe runs (rv-secreview
    /// linux-t4c F1, rv-code4 linux-t4c F1).
    #[test]
    fn a_supervisor_probe_runs_in_its_own_folder_the_engine_removes_after() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let place = temp.path().canonicalize().unwrap();
        let data = place.join("data");
        fs::create_dir(&data).unwrap();
        let seen = place.join("seen");
        let fake = |name: &str, answer: u8| {
            let helper = place.join(name);
            let script = format!(
                "#!/bin/sh\n[ \"$1\" = {PROBE_SUPERVISOR} ] && [ -d \"$2\" ] || exit 9\n\
                 printf '%s\\n' \"$2\" >> '{}'\n: > \"$2/made\"\nexit {answer}\n",
                seen.display()
            );
            fs::write(&helper, script).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            helper
        };
        let holding = fake("holding", 0);
        let failing = fake("failing", 1);
        assert!(supervises(&holding, None, &data, false));
        assert!(!supervises(&failing, None, &data, false));
        let both = std::thread::scope(|scope| {
            let one = scope.spawn(|| supervises(&holding, None, &data, false));
            let two = scope.spawn(|| supervises(&holding, None, &data, false));
            [one.join().unwrap(), two.join().unwrap()]
        });
        assert_eq!(both, [true, true]);
        let told = fs::read_to_string(&seen).unwrap();
        let folders: std::collections::BTreeSet<&str> = told.lines().collect();
        assert_eq!((told.lines().count(), folders.len()), (4, 4), "{told}");
        for folder in &folders {
            assert!(Path::new(folder).starts_with(&data), "{folder}");
        }
        assert_eq!(
            fs::read_dir(&data).unwrap().count(),
            0,
            "every folder removed"
        );
        assert!(!supervises(&holding, None, &data, true), "never as root");
        assert_eq!(
            fs::read_to_string(&seen).unwrap(),
            told,
            "as root no probe runs"
        );
        assert_eq!(
            fs::read_dir(&data).unwrap().count(),
            0,
            "and no folder is made"
        );
    }

    /// The helper refuses every variable that makes glibc load code, the dynamic loader's
    /// and gconv's, and no other: a name that only starts with `LD`, holds `LD_` later, or
    /// differs in case still passes (DEC-179 SBX-3 fold, rv-sa241 MED-1, rv-ds241 F2).
    #[test]
    fn the_helper_refuses_only_the_variables_that_load_code() {
        for name in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_LIBRARY_PATH",
            "LD_BIND_NOW",
            "GLIBC_TUNABLES",
            "GCONV_PATH",
        ] {
            assert!(loader_variable(OsStr::new(name)), "{name}");
        }
        for name in [
            "LDFLAGS",
            "OLD_PWD",
            "PATH",
            "ld_preload",
            "GLIBC_TUNABLES_X",
            "",
        ] {
            assert!(!loader_variable(OsStr::new(name)), "{name}");
        }
    }

    /// A launch whose own probe fails with the setting On runs without the supervisor and
    /// marks the kept host as running none, so the next Agent access page read shows the
    /// wider lines; nothing kept stays nothing (rv-secreview linux-t4c F2).
    #[test]
    fn a_launch_whose_probe_fails_marks_the_kept_host_unsupervised() {
        use super::super::LinuxHost;
        let host = LinuxHost {
            nested: true,
            scoped: true,
            supervised: true,
            root: false,
            armored: false,
            routed: false,
        };
        let at = Instant::now();
        let kept: KeptHost = Mutex::new(Some((at, host)));
        forget_supervisor(&kept);
        let marked = LinuxHost {
            supervised: false,
            ..host
        };
        assert_eq!(*kept.lock().unwrap(), Some((at, marked)));
        let nothing: KeptHost = Mutex::new(None);
        forget_supervisor(&nothing);
        assert_eq!(*nothing.lock().unwrap(), None);
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let (_project, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let failing = LinuxBackend::Landlock {
            helper: PathBuf::from("/bin/false"),
        };
        let launch = LinuxSandbox::new(sandbox, failing).unwrap();
        let unsupervised = launch.supervise(LinuxSupervisor::On, Some(data.as_path()));
        assert_eq!(unsupervised.supervision, None);
        assert_eq!(fs::read_dir(&data).unwrap().count(), 0);
    }

    /// A supervised launch guards the loopback ports of a limited network under TCP only,
    /// but a port it may also reach on any host or look names up on, always its own git
    /// folders, and signals only where the host holds nothing else (DEC-179 T4c).
    #[test]
    fn a_supervised_launch_carries_each_guard_its_policy_and_host_need() {
        use SandboxRemote::{AnyHost, Loopback};
        let remotes = vec![Loopback(8080), Loopback(443), AnyHost(443), Loopback(53)];
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes));
        assert!(sandbox.resolves_over_tcp());
        let own = GitOwn {
            trees: vec![sandbox.project_root().to_path_buf()],
            ..GitOwn::default()
        };
        let alone = Supervision { signals: true };
        let expected = Guard {
            loopback: vec![8080],
            git: Some(own),
            signals: true,
            held: None,
        };
        assert_eq!(guard(&sandbox, alone), expected);
        let nested = Guard {
            signals: false,
            ..expected.clone()
        };
        assert_eq!(guard(&sandbox, Supervision { signals: false }), nested);
        let ports = sandbox.clone().limit_sockets(LinuxSockets::TcpPorts);
        let any_host = Guard {
            loopback: Vec::new(),
            ..expected
        };
        assert_eq!(guard(&ports, alone), any_host);
        let (_open, open) = project(SandboxNetwork::Open);
        let held = guard(&open, alone);
        assert!(held.loopback.is_empty(), "{held:?}");
        assert!(
            held.git
                .is_some_and(|git| git.trees == [open.project_root()])
        );
    }

    /// The git folders a supervised agent owns are the ones the macOS boundary leaves it:
    /// the top of each tree it works in and, for the engine's worktree step, each folder in
    /// the two worktree folders (DEC-179 T4c, RV-DS241-85b).
    #[test]
    fn a_supervised_agent_owns_the_git_of_each_tree_it_works_in_and_of_each_engine_worktree() {
        let (temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let project = sandbox.project_root().to_path_buf();
        let alone = GitOwn {
            trees: vec![project.clone()],
            ..GitOwn::default()
        };
        assert_eq!(git_own(&sandbox), alone);
        let lease = temp.path().canonicalize().unwrap().join("lease");
        fs::create_dir(&lease).unwrap();
        let worked = sandbox.allow_worktree(&lease).unwrap().adding_worktrees();
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let expected = GitOwn {
            trees: vec![project.clone(), lease],
            kids: vec![project.join("worktrees"), state.join("worktrees")],
            exact: Vec::new(),
        };
        assert_eq!(git_own(&worked), expected);
    }

    /// The helper gets guard flags only for a launch the call supervisor holds, and a
    /// nested one carries no signal guard: its process namespace holds signals.
    #[test]
    fn only_a_supervised_launch_hands_the_helper_its_guards() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let backend = LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper,
        };
        let plain = LinuxSandbox::new(sandbox, backend).unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let words = |sandbox: &LinuxSandbox| -> Vec<OsString> {
            sandbox
                .wrap(&harness)
                .unwrap()
                .get_args()
                .map(OsStr::to_os_string)
                .collect()
        };
        assert!(!words(&plain).contains(&GUARD_GIT.into()));
        let supervised = LinuxSandbox {
            supervision: Some(Supervision { signals: false }),
            ..plain
        };
        let held = words(&supervised);
        let git = [GUARD_GIT, OWN_TREE].map(OsString::from);
        let at = held
            .windows(2)
            .position(|pair| pair == git)
            .expect("the git guard");
        assert_eq!(held[at + 2], cwd.as_os_str());
        assert!(!held.contains(&GUARD_SIGNALS.into()));
        assert!(!held.contains(&GUARD_LOOPBACK.into()));
    }

    #[test]
    fn no_agent_is_granted_the_git_state_the_owners_git_reads_and_runs() {
        let (_temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let git = sandbox.project_root().join(".git");
        let trees = [
            "rebase-merge",
            "rebase-apply",
            "remotes",
            "branches",
            "sequencer",
            "refs",
        ];
        for name in trees {
            fs::create_dir_all(git.join(name)).unwrap();
        }
        for name in ["index", "index.lock", "HEAD"] {
            fs::write(git.join(name), "state\n").unwrap();
        }
        let grants = Grants::of(&sandbox).unwrap();
        for name in [
            "index",
            "index.lock",
            "rebase-merge",
            "rebase-apply",
            "remotes",
            "branches",
            "sequencer",
        ] {
            assert!(
                !grants.write.contains(&git.join(name)),
                "{name} is writable"
            );
        }
        for name in ["HEAD", "refs"] {
            assert!(
                grants.write.contains(&git.join(name)),
                "{name} is not writable"
            );
        }
    }

    mod helper_paths;
}
