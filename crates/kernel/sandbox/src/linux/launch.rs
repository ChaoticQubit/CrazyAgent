use crate::error::{SandboxError, invalid};
use crate::hidden_paths::HIDDEN_MAX;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::paths::clean_absolute;
use crate::ports::BRIDGE_PORT_MIN;
use crate::ports::{OWNER_PORTS, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX};
use crate::profile::consts::{
    ADMIN_LINKS, ADMIN_WRITES, GIT_FALLBACK, GIT_STATE_FILES, GIT_STATE_TREES,
    LINUX_HARNESS_UNCHECKED, LINUX_LIMITED_NETWORK_OFF, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK,
    LINUX_RESOLVER_NEEDS_UDP, MULTI_PACK_INDEX, STORE_ALTERNATES,
};
use crate::profile::env::{Handed, Routed, agent_process_env, agent_process_env_handing};
use crate::profile::{ProjectSandbox, command::Sandbox, linux_holds_fully};
use crate::settings::{LinuxSockets, LinuxSupervisor};
use ::landlock::RulesetStatus;
use crazyagent_linux_supervisor::rules::{GitOwn, Held, is_git};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::linux::helper::egress::SLIRP_PATHS;
use crate::linux::helper::landlock::*;
use crate::linux::helper::*;

/// What this computer holds and how it was read: the probes, the kept routes and nests,
/// the host read and the other-git read, all through the sandbox helper.
pub mod host;
use host::*;

/// The Landlock helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-landlock";

/// The Landlock helper for the engine executable at `engine`: the one beside it. Only in a
/// debug build, an executable whose own folder is cargo's `deps`, as a test binary's is, with
/// no helper beside it takes cargo's build one folder up, the one candidate, and logs it.
/// Input: the executable's absolute path. Output: the helper's path.
pub(crate) fn helper_for(engine: &Path) -> PathBuf {
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
pub(crate) const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
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
pub(crate) const HELPER_MISSING: &str = "CrazyAgent's sandbox helper is missing, and no Linux agent \
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

pub(crate) const CONFINED_UNAVAILABLE: &str = "an agent confined to its own worktree cannot run on Linux yet: \
     neither Landlock nor bubblewrap can keep the worktree's git links and the existing git \
     objects read-only inside the git folders it must write";

pub(crate) const CONFINED_NEEDS_LANDLOCK: &str = "an agent confined to its own worktree or clone needs \
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
pub fn git_step_policy(read: &[PathBuf], write: &[PathBuf]) -> LandlockPolicy {
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
pub fn holds_file_rules(helper: &Path) -> bool {
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
pub struct Supervision {
    pub signals: bool,
}

impl LinuxSandbox {
    /// Test helper: assembles a launch from its parts with none of [`LinuxSandbox::new`]'s
    /// checks, so a row can wrap a command for a policy or backend this machine would refuse.
    /// Inputs: the policy, the backend, the supervision and the route. Output: the launch.
    #[cfg(test)]
    pub(crate) fn assembled(
        sandbox: ProjectSandbox,
        backend: LinuxBackend,
        supervision: Option<Supervision>,
        routed: Option<Routed>,
    ) -> Self {
        LinuxSandbox {
            sandbox,
            backend,
            supervision,
            routed,
        }
    }

    /// Test helper: the same launch with another supervision. Input: the supervision, `None`
    /// for an unsupervised launch. Output: the launch.
    #[cfg(test)]
    pub(crate) fn with_supervision(self, supervision: Option<Supervision>) -> Self {
        LinuxSandbox {
            supervision,
            ..self
        }
    }

    /// Test helper: the supervision this launch runs under. Output: the supervision, `None`
    /// when no call supervisor runs.
    #[cfg(test)]
    pub(crate) fn supervision(&self) -> Option<Supervision> {
        self.supervision
    }

    /// Binds the policy to one backend. Input: the policy and the backend's absolute files. Output:
    /// the sandbox, `Invalid` for a bad executable, or `Unavailable` on bubblewrap for any network
    /// but a closed one with no host port, for a one-worktree policy, a limited network the sockets
    /// setting refuses, off bubblewrap a kept port Linux could hand out ([`kept_outside_picked`]),
    /// on Landlock a hidden folder, or hidden folders with no private mount tree ([`cuts`]).
    pub fn new(sandbox: ProjectSandbox, backend: LinuxBackend) -> Result<Self, SandboxError> {
        admitted(&sandbox)?;
        if matches!(backend, LinuxBackend::Landlock { .. }) && !sandbox.hidden().is_empty() {
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
        if !sandbox.hidden().is_empty() && !cuts(&backend) {
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
    pub(crate) fn ends_every_session(&self) -> bool {
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

    /// Tells whether this launch holds the full Linux sandbox ([`linux_holds_fully`]):
    /// the nested backend, the call supervisor this launch starts, and its network limits
    /// held, a port on any host through the network of its own detection found. No input.
    /// Output: the answer.
    pub fn holds_fully(&self) -> bool {
        let nested = matches!(self.backend, LinuxBackend::Nested { .. });
        assert!(self.routed.is_none() || nested);
        let supervised = self.supervision.is_some();
        linux_holds_fully(nested, supervised, self.routed.is_some(), &self.sandbox)
    }

    /// The program every wrapped command starts: the Landlock helper or bwrap.
    pub fn launcher(&self) -> &Path {
        match &self.backend {
            LinuxBackend::Landlock { helper } | LinuxBackend::Nested { helper, .. } => helper,
            LinuxBackend::Bubblewrap { bwrap, .. } => bwrap,
        }
    }

    /// Rebuilds a harness command as [`Sandbox::wrap`] does for a launch that hands its
    /// account over: the harness variables `handed` names may hold its one value. Inputs:
    /// the harness command and the launch's exception. Output: the command, or the refusal.
    pub fn wrap_handing(
        &self,
        harness: &Command,
        handed: Handed<'_>,
    ) -> Result<Command, SandboxError> {
        self.wrapped(harness, Some(handed))
    }

    /// Rebuilds a harness command to run in this sandbox, its environment built with the
    /// launch's exception to the variable guard when it has one. Inputs: the harness command
    /// and the exception, if any. Output: the command, or the refusal.
    fn wrapped(
        &self,
        harness: &Command,
        handed: Option<Handed<'_>>,
    ) -> Result<Command, SandboxError> {
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
            .unwrap_or(self.sandbox.project_root());
        if !clean_absolute(cwd)
            || !std::iter::once(self.sandbox.project_root())
                .chain(self.sandbox.worktrees().iter().map(PathBuf::as_path))
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        let grants = Grants::of(&self.sandbox)?;
        let confined = self.sandbox.checkout().is_some() || self.sandbox.clone_root().is_some();
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
        let env = match handed {
            Some(handed) => agent_process_env_handing(&base, harness.get_envs(), handed)?,
            None => agent_process_env(&base, harness.get_envs())?,
        };
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

impl Sandbox for LinuxSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        self.wrapped(harness, None)
    }
}

/// What a launch's Landlock rules grant: folders read, folders written whole, git folders
/// written with [`git_folder_rights`], and `guarded`, what Landlock cannot keep read-only
/// under those grants, as the call supervisor holds it (protected paths in a git grant,
/// [`ADMIN_WRITES`] and [`ADMIN_LINKS`] in the admin folder, the worktree's `.git` link and
/// a clone's store names, [`clone_store`]), which that supervisor must hold before a
/// confined launch starts, the nested binds a second layer (DEC-185 S1, S2).
pub struct Grants {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub git_write: Vec<PathBuf>,
    pub guarded: Held,
    /// The hidden folders, each made empty first when absent so a folder made during the
    /// run lands under the cover (rv-sec2 F2), each covered by bubblewrap with an empty
    /// read-only folder; no write grant covers any hidden folder ([`split_above`]).
    pub hidden: Vec<PathBuf>,
}

impl Grants {
    pub fn of(sandbox: &ProjectSandbox) -> Result<Self, SandboxError> {
        mounts_hold(sandbox)?;
        let project = sandbox.project_root();
        let git = project.join(".git");
        let state = project.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
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
        let checkout = sandbox.checkout();
        let mut write = Vec::new();
        let mut git_write = Vec::new();
        for root in sandbox.writable_roots() {
            if checkout.is_some_and(|checkout| root == checkout.objects()) {
                continue;
            }
            if checkout.is_some_and(|checkout| root == checkout.admin()) {
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
        for hidden in sandbox.hidden() {
            split_above(hidden, &protected, &mut write)?;
        }
        if git_write
            .iter()
            .any(|root| sandbox.hidden().iter().any(|path| path.starts_with(root)))
        {
            return Err(invalid("a git folder grant holds a hidden folder"));
        }
        let mut hidden = Vec::new();
        for path in sandbox.hidden() {
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
        assert_eq!(hidden.len(), sandbox.hidden().len());
        assert!(hidden.len() <= HIDDEN_MAX);
        assert!(write.iter().all(|path| {
            sandbox
                .hidden()
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
            if git_write.contains(checkout.admin()) {
                let held = ADMIN_WRITES.iter().chain(&ADMIN_LINKS);
                guarded
                    .trees
                    .extend(held.map(|name| checkout.admin().join(name)));
            }
            if write.contains(checkout.worktree()) {
                guarded.trees.push(checkout.worktree().join(".git"));
            }
        }
        if let Some(clone) = sandbox.clone_root() {
            let store = clone_store(clone);
            guarded.entries.extend(store.entries);
            guarded.trees.extend(store.trees);
            guarded.stores.extend(store.stores);
        }
        let secrets = sandbox.secret_paths();
        let carved = sandbox.carved_home();
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
    if sandbox.hidden().is_empty() {
        return Ok(());
    }
    let text = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|_| SandboxError::Unavailable(HIDDEN_MOUNTS_UNREAD.into()))?;
    mounted_again(sandbox.hidden(), &text)
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
pub fn guard(sandbox: &ProjectSandbox, supervision: Supervision) -> Guard {
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
pub(crate) fn git_own(sandbox: &ProjectSandbox) -> GitOwn {
    let (working, _) = sandbox.git_deny_roots();
    let mut kids = Vec::new();
    if sandbox.adds_worktrees() {
        let project = sandbox.project_root();
        let state = project.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
        kids.extend([project.join("worktrees"), state.join("worktrees")]);
    }
    if let Some(setup) = sandbox.teammates_setup() {
        kids.push(setup.folder().clone());
    }
    let common = sandbox.checkout().and_then(|checkout| {
        let plain = |folder: &&Path| folder.file_name() == Some(OsStr::new(".git"));
        checkout
            .admin()
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
            .clone_root()
            .is_none_or(|clone| own.trees.contains(clone))
    );
    own
}

pub(crate) fn landlock_policy(grants: &Grants, sandbox: &ProjectSandbox) -> LandlockPolicy {
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
pub(crate) fn kept_free(sandbox: &ProjectSandbox) -> Vec<u16> {
    let mut ports: Vec<u16> = OWNER_PORTS
        .iter()
        .chain(sandbox.kept_ports())
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

pub(crate) fn ports(remotes: &[SandboxRemote]) -> Vec<u16> {
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
pub(crate) fn nest_command(
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
mod engine_rows;
#[cfg(test)]
mod tests;
