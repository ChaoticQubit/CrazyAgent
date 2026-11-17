use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId};
use crate::sandbox::{
    InstalledSandbox, LinuxStrict, LinuxSupervisor, ProjectSandbox, SandboxNetwork, SandboxRemote,
};
use rustix::process::Pid;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The folder inside a [`MasterFixture`] project that every stand-in writes its files into.
/// The fixture makes it before any sandbox is built: the Linux boundary lets an agent write
/// only into what the project root already holds, never a new entry at its top.
pub(in crate::engine) const RUN_FOLDER: &str = "run";

/// The most parents [`host_id`] follows from a process before it gives up on reaching the
/// host's child.
const PARENTS_MAX: usize = 64;

/// The most `/proc` entries one scan reads: the Linux limit on process ids.
#[cfg(target_os = "linux")]
const PROCESSES_MAX: usize = 1 << 22;

/// A stand-in harness that starts a descendant sharing its stdout and stderr, writes both
/// process ids into the `run` folder of its working folder and then sleeps, so only a group
/// kill ends it.
pub(in crate::engine) const PIPE_HOLDING_HARNESS: &str = "#!/bin/sh
/bin/sleep 30 &
echo $! > run/descendant.pid
echo $$ > run/leader.pid
exec /bin/sleep 30
";

/// A stand-in harness whose descendant moves into its own process group before it writes its
/// id, still holding the harness stdout and stderr, so no group kill can reach it.
pub(in crate::engine) const GROUP_LEAVING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -e 'setpgrp(0, 0) or exit 1; open(my $file, ">", "run/descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' &
echo $$ > run/leader.pid
exec /bin/sleep 30
"#;

/// A stand-in harness whose group keeps a process the engine can neither signal nor reap: a
/// descendant in its own group starts a child that joins the harness group and exits, never
/// reaped, then writes its own id and sleeps. macOS answers a probe of that group with EPERM.
#[cfg(target_os = "macos")]
pub(in crate::engine) const ZOMBIE_HOLDING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -e 'my $group = getpgrp(); setpgrp(0, 0) or exit 1; pipe(my $read, my $write) or exit 1; my $child = fork(); defined $child or exit 1; if ($child == 0) { close($read); setpgrp(0, $group) or exit 1; exit 0 } close($write); my $gone = <$read>; open(my $file, ">", "run/descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
echo $$ > run/leader.pid
exec /bin/sleep 30
"#;

/// The start of a stand-in harness that leaves what a Claude Code Bash tool shell can leave: a
/// process that started its own session with its own stdio, as each tool shell does, and a
/// `nohup` command whose shell already exited. It writes their ids into the `run` folder of its
/// working folder; a test appends how the harness itself ends.
#[cfg(target_os = "macos")]
pub(in crate::engine) const DETACHING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "run/descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec "/bin/sh", "-c", q(/usr/bin/nohup /bin/sleep 30 < /dev/null > /dev/null 2>&1 & echo $! > run/orphan.pid)' < /dev/null > /dev/null 2>&1
echo $$ > run/leader.pid
"#;

/// The start of a stand-in harness whose child lives five seconds and forks a process that
/// starts its own session and writes its id, so once that child exits the process has no
/// parent but launchd, shares nothing with the harness and runs a macOS program whose
/// environment `ps -E` hides: only a follow from the start still knows it at the stop. A test
/// appends how the harness itself ends.
#[cfg(target_os = "macos")]
pub(in crate::engine) const ORPHANING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -mPOSIX -e 'defined(my $child = fork()) or exit 1; if ($child == 0) { POSIX::setsid() > 0 or exit 1; open(my $file, ">", "run/orphan.pid") or exit 1; print $file "$$\n"; close($file); sleep 60; exit 0 } sleep 5' < /dev/null > /dev/null 2>&1 &
echo $$ > run/leader.pid
"#;

/// A real native profile custody in a private engine folder under the canonical `/tmp`, with a
/// project folder that holds a [`RUN_FOLDER`], for master host tests that need an actual
/// profile lease and sandbox.
pub(in crate::engine) struct MasterFixture {
    pub(in crate::engine) root: tempfile::TempDir,
    pub(in crate::engine) project: PathBuf,
    pub(in crate::engine) project_id: ProjectId,
    pub(in crate::engine) connection: ModelConnection,
    custody: NativeProfileCustody,
    host: HostStamp,
    binding: NativeProfileBinding,
}

impl MasterFixture {
    /// Creates the engine folder, its database, the project and the prepared managed profile.
    /// Takes the harness name the connection names. Returns the fixture.
    pub(in crate::engine) fn new(harness: &str) -> Self {
        let reference = |value: &str| CredentialRef::new(value).unwrap();
        let root = tempfile::Builder::new()
            .prefix("ca-master-group-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::create_dir(project.join(RUN_FOLDER)).unwrap();
        let mut custody =
            NativeProfileCustody::capture(&database, &fs::metadata(&database).unwrap()).unwrap();
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let connection = ModelConnection {
            id: reference("group-master-connection"),
            provider: reference("group-master-provider"),
            harness: reference(harness),
            protocol: reference("native"),
            account_ref: reference("group-master-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "native-default".into(),
                author: reference("group-master-provider"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("group-master-profile"),
            },
        };
        let (custody_id, _) = custody.prepare(&host, &connection).unwrap();
        let binding = NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection: connection.clone(),
            custody_id,
        };
        let project_id =
            ProjectId::new(root.path().file_name().unwrap().to_str().unwrap()).unwrap();
        Self {
            root,
            project,
            project_id,
            connection,
            custody,
            host,
            binding,
        }
    }

    /// Acquires the shared execution lease of the profile for one operation.
    /// Takes the operation id and its generation. Returns the lease, or the engine's refusal
    /// while a sign-in, refresh or logout holds the profile.
    pub(in crate::engine) fn lease(
        &mut self,
        operation: &str,
        generation: u64,
    ) -> Result<NativeProfileUse, EngineError> {
        self.acquire(operation, generation, NativeProfileUsePurpose::Execution)
    }

    /// Acquires the exclusive sign-in lease of the profile, which no running agent shares.
    /// Takes the operation id and its generation. Returns the lease, or
    /// `native_profile_in_use` while any execution lease still holds the profile.
    pub(in crate::engine) fn sign_in(
        &mut self,
        operation: &str,
        generation: u64,
    ) -> Result<NativeProfileUse, EngineError> {
        self.acquire(
            operation,
            generation,
            NativeProfileUsePurpose::Authentication,
        )
    }

    /// Acquires one lease of the profile for one operation and purpose.
    /// Takes the operation id, its generation and the purpose. Returns the custody's result.
    fn acquire(
        &mut self,
        operation: &str,
        generation: u64,
        purpose: NativeProfileUsePurpose,
    ) -> Result<NativeProfileUse, EngineError> {
        self.custody.acquire_use(
            &self.host,
            &self.binding,
            &RequestId::new(operation).unwrap(),
            generation,
            purpose,
        )
    }

    /// Writes an executable stand-in harness script outside every sandbox writable root and
    /// runs it once with `first-exec`, which a guard line answers with code 3 before the body,
    /// so its first-exec assessment is over before a host launch is timed.
    /// Takes its file name and a `#!/bin/sh` body. Returns its canonical path.
    pub(in crate::engine) fn stand_in(&self, name: &str, body: &str) -> PathBuf {
        let rest = body.strip_prefix("#!/bin/sh\n").unwrap();
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = bin.join(name);
        fs::write(
            &script,
            format!("#!/bin/sh\n[ \"$1\" = first-exec ] && exit 3\n{rest}"),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let first = crate::spawn_gate::first_run(
            std::process::Command::new(&script)
                .arg("first-exec")
                .current_dir(&bin),
        );
        assert_eq!(first.code(), Some(3));
        fs::canonicalize(script).unwrap()
    }

    /// Installs the project sandbox for a leased profile home, also reading `readable`.
    /// Takes the home and the extra readable folder. Returns the installed sandbox.
    pub(in crate::engine) fn sandbox(&self, home: &Path, readable: &Path) -> InstalledSandbox {
        let tmp = self.root.path().join("agent-tmp");
        fs::create_dir_all(&tmp).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
        let sandbox = ProjectSandbox::new(
            &fs::canonicalize(&self.project).unwrap(),
            &fs::canonicalize(home).unwrap(),
            &fs::canonicalize(&tmp).unwrap(),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        )
        .unwrap()
        .allow_read(readable)
        .unwrap();
        let profile = self.root.path().join("master.sb");
        InstalledSandbox::install(sandbox, &profile, LinuxSupervisor::On, LinuxStrict::Off).unwrap()
    }

    /// The check a stop of a confined master in this fixture runs on what left its group:
    /// `Sandboxed` where the Linux backend leaves such a process running, `Unneeded` where a
    /// process namespace or the platform ends it. Takes nothing. Returns the check.
    #[cfg(not(target_os = "macos"))]
    pub(in crate::engine) fn lineage_check(&mut self) -> crate::sandbox::LineageCheck {
        let profile = self.lease("fixture-lineage-check", 1).unwrap();
        let home = profile.profile_path().unwrap();
        let readable = self.root.path().join("bin");
        fs::create_dir_all(&readable).unwrap();
        let check = self.sandbox(&home, &readable).lineage_check();
        assert_ne!(check, crate::sandbox::LineageCheck::FullAccess);
        check
    }
}

/// The two real processes a [`PIPE_HOLDING_HARNESS`] started, by host id: the harness itself
/// and its descendant, with the process group the harness runs in: the group of the host's
/// child, which is the harness itself or the bubblewrap whose process namespace holds it.
/// Dropping it kills the descendant and the group, so a failing test leaves none.
pub(in crate::engine) struct HarnessProcesses {
    pub(in crate::engine) leader: Pid,
    pub(in crate::engine) descendant: Pid,
    pub(in crate::engine) group: Pid,
}

impl HarnessProcesses {
    /// Waits up to five seconds until the host named its child and the stand-in wrote both
    /// process ids into the [`RUN_FOLDER`] of `folder`, maps each to its host id under that
    /// child and checks the harness runs in the child's group. Takes the stand-in's working
    /// folder and the host's child, none before it started. Returns both processes and the
    /// group. Panics when the harness left that group, which a stop of the host then misses.
    pub(in crate::engine) fn wait(folder: &Path, child: impl Fn() -> Option<u32>) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let (Some(id), Some(leader), Some(descendant)) = (
                child(),
                written(folder, "leader.pid"),
                written(folder, "descendant.pid"),
            ) {
                let leader = on_host(leader, id);
                let descendant = on_host(descendant, id);
                assert_ne!(leader, descendant);
                let group = Pid::from_raw(i32::try_from(id).unwrap()).unwrap();
                assert_eq!(rustix::process::getpgid(Some(leader)), Ok(group));
                assert_ne!(
                    group,
                    rustix::process::getpgrp(),
                    "the host's child is in ours"
                );
                return Self {
                    leader,
                    descendant,
                    group,
                };
            }
            assert!(Instant::now() < deadline, "stand-in harness never started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Asserts how a stop of the host left a harness whose descendant left the harness group,
    /// by the boundary's check. Where the backend leaves that process running (`Sandboxed`),
    /// the stop reports `uncertain`, the harness and its group are gone and the descendant
    /// lives on. Where a process namespace ends it, the stop is clean and within two seconds
    /// nothing is left. Takes the check, the stop's result and the uncertain cleanup code.
    #[cfg(not(target_os = "macos"))]
    pub(in crate::engine) fn assert_stopped_by(
        &self,
        check: crate::sandbox::LineageCheck,
        stopped: Result<(), String>,
        uncertain: &str,
    ) {
        if check == crate::sandbox::LineageCheck::Sandboxed {
            assert_eq!(stopped, Err(uncertain.to_owned()));
            self.assert_harness_gone();
            assert_eq!(rustix::process::test_kill_process(self.descendant), Ok(()));
            return;
        }
        assert_eq!(stopped, Ok(()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while rustix::process::test_kill_process(self.descendant) != Err(rustix::io::Errno::SRCH) {
            assert!(
                Instant::now() < deadline,
                "{:?} outlived its namespace",
                self.descendant
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.assert_gone();
    }

    /// Asserts that neither process nor any member of the harness group is left.
    /// Takes nothing. Panics with the surviving process otherwise.
    pub(in crate::engine) fn assert_gone(&self) {
        assert_eq!(
            rustix::process::test_kill_process(self.descendant),
            Err(rustix::io::Errno::SRCH),
            "descendant {:?} survived",
            self.descendant
        );
        self.assert_harness_gone();
    }

    /// Asserts that the harness and every member of its group are gone.
    /// Takes nothing. Panics with the surviving process otherwise.
    pub(in crate::engine) fn assert_harness_gone(&self) {
        assert_eq!(
            rustix::process::test_kill_process(self.leader),
            Err(rustix::io::Errno::SRCH),
            "harness {:?} survived",
            self.leader
        );
        assert_eq!(
            rustix::process::test_kill_process_group(self.group),
            Err(rustix::io::Errno::SRCH),
            "harness group {:?} survived",
            self.group
        );
    }
}

impl Drop for HarnessProcesses {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.descendant, rustix::process::Signal::KILL);
        let _ = rustix::process::kill_process_group(self.group, rustix::process::Signal::KILL);
    }
}

/// Kills the host's own child while a waiter already blocks on it, so the waiter, not the
/// host, reaps it, after which the host can never reap its harness. That child is the
/// harness itself, or the bubblewrap whose process namespace holds it. The host polls its
/// child too, so it can still win: the caller then starts over. Takes the stand-in's working
/// folder and the host's child, none before it started. Returns whether the waiter reaped
/// the child.
pub(in crate::engine) fn reap_before_the_host(
    folder: &Path,
    child: impl Fn() -> Option<u32>,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    let child = loop {
        if let (Some(id), Some(_)) = (child(), written(folder, "leader.pid")) {
            break Pid::from_raw(i32::try_from(id).unwrap()).unwrap();
        }
        assert!(Instant::now() < deadline, "stand-in harness never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(child.as_raw_nonzero().get() > 1);
    let waiter = std::thread::spawn(move || {
        rustix::process::waitpid(Some(child), rustix::process::WaitOptions::empty())
    });
    std::thread::sleep(Duration::from_millis(50));
    rustix::process::kill_process(child, rustix::process::Signal::KILL).unwrap();
    match waiter.join().unwrap() {
        Ok(Some((reaped, _))) => {
            assert_eq!(reaped, child);
            true
        }
        Err(rustix::io::Errno::CHILD) => false,
        waited => panic!("waiting for the host's child failed: {waited:?}"),
    }
}

/// Reads a process id a stand-in wrote into the [`RUN_FOLDER`] of `folder`. Takes the folder
/// and the file name. Returns the id as the stand-in saw it, or none before it was written.
fn written(folder: &Path, name: &str) -> Option<i32> {
    let text = fs::read_to_string(folder.join(RUN_FOLDER).join(name)).ok()?;
    text.trim().parse::<i32>().ok().filter(|id| *id > 0)
}

/// Maps an id a stand-in wrote from inside its process namespace to its host id by a scan of
/// this host's `/proc` ([`host_id`]). Takes the written id and the host's child. Returns the
/// process. Panics unless exactly one process under the child has that id: the raw number
/// can name an unrelated host process, so it is never signalled.
#[cfg(target_os = "linux")]
fn on_host(inner: i32, child: u32) -> Pid {
    let statuses: Vec<(i32, String)> = fs::read_dir("/proc")
        .unwrap()
        .take(PROCESSES_MAX)
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let id = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let status = fs::read_to_string(entry.path().join("status")).ok()?;
            Some((id, status))
        })
        .collect();
    let child = i32::try_from(child).unwrap();
    let host = host_id(inner, child, &statuses).unwrap_or_else(|| {
        panic!("no single process under the host's child {child} has namespace id {inner}")
    });
    Pid::from_raw(host).unwrap()
}

/// Takes the id a stand-in wrote as its host id, as no process namespace exists here.
/// Takes the written id and the host's child. Returns the process.
#[cfg(not(target_os = "linux"))]
fn on_host(inner: i32, child: u32) -> Pid {
    assert!(child > 1);
    Pid::from_raw(inner).unwrap()
}

/// Finds the host id of the process a stand-in named by its id inside its process namespace:
/// the one process whose innermost `NSpid` is `inner` and whose `PPid` chain reaches `child`.
/// Outside a namespace `NSpid` holds the host id alone, so the same rule holds there. Takes
/// the inner id, the host's child and each process's host id with its `/proc` status text.
/// Returns the host id, or none unless exactly one process matches.
fn host_id(inner: i32, child: i32, statuses: &[(i32, String)]) -> Option<i32> {
    assert!(inner > 0);
    assert!(child > 1);
    let field = |status: &str, name: &str| -> Option<Vec<i32>> {
        let line = status.lines().find_map(|line| line.strip_prefix(name))?;
        line.split_whitespace()
            .map(|word| word.parse().ok())
            .collect()
    };
    let parent = |id: i32| {
        let (_, status) = statuses.iter().find(|(known, _)| *known == id)?;
        field(status, "PPid:")?.first().copied()
    };
    let under = |id: i32| {
        let mut current = id;
        for _ in 0..PARENTS_MAX {
            if current == child {
                return true;
            }
            match parent(current) {
                Some(next) if next > 0 => current = next,
                _ => return false,
            }
        }
        false
    };
    let innermost = |status: &str| field(status, "NSpid:").and_then(|ids| ids.last().copied());
    let mut found = statuses
        .iter()
        .filter(|(id, status)| innermost(status) == Some(inner) && under(*id))
        .map(|(id, _)| *id);
    let host = found.next()?;
    if found.next().is_some() {
        return None;
    }
    assert!(host > 1);
    Some(host)
}

/// R2 (farm 011, 013 to 017): a stand-in in a process namespace writes the id it sees there.
/// Only the one process under the host's child with that innermost id is taken. A process
/// elsewhere on the host with the same innermost id never is, and none or two give nothing.
#[test]
fn a_namespace_id_a_stand_in_wrote_names_only_its_one_process_under_the_host_child() {
    let status = |parent: i32, ids: &str| format!("Name:\tsleep\nPPid:\t{parent}\nNSpid:\t{ids}\n");
    let engine = (700, status(1, "700"));
    let nest = (8000, status(700, "8000"));
    let nest_init = (8001, status(8000, "8001\t1"));
    let harness = (8972, status(8001, "8972\t2"));
    let kernel_thread = (2, status(0, "2"));
    let other_sandbox = (9100, status(1, "9100\t2"));
    let host = [
        engine,
        nest.clone(),
        nest_init.clone(),
        harness,
        kernel_thread,
        other_sandbox,
    ];

    assert_eq!(host_id(2, 8000, &host), Some(8972));
    assert_eq!(host_id(8000, 8000, &host), Some(8000));
    assert_eq!(host_id(2, 9100, &host), Some(9100));
    assert_eq!(host_id(3, 8000, &host), None);
    assert_eq!(host_id(2, 4242, &host), None);

    let mut twice = host.to_vec();
    twice.push((8973, status(nest_init.0, "8973\t2")));
    assert_eq!(host_id(2, 8000, &twice), None);

    let unparsed = [nest, (8974, "PPid:\t8000\nNSpid:\tx\t2\n".to_owned())];
    assert_eq!(host_id(2, 8000, &unparsed), None);
}

/// The three real processes a [`DETACHING_HARNESS`] started: the harness, the process in its
/// own session and the `nohup` orphan. Dropping it kills all three, so a failing test leaves
/// none.
#[cfg(target_os = "macos")]
pub(in crate::engine) struct DetachedProcesses {
    pub(in crate::engine) leader: Pid,
    pub(in crate::engine) session: Pid,
    pub(in crate::engine) orphan: Pid,
}

#[cfg(target_os = "macos")]
impl DetachedProcesses {
    /// Waits up to five seconds until the stand-in wrote all three ids into the [`RUN_FOLDER`]
    /// of `folder`, then checks the two detached ones left the harness group and are alive.
    /// Takes the stand-in's working folder. Returns the three processes.
    pub(in crate::engine) fn wait(folder: &Path) -> Self {
        let read = |name: &str| {
            fs::read_to_string(folder.join(RUN_FOLDER).join(name))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .and_then(Pid::from_raw)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let (Some(leader), Some(session), Some(orphan)) = (
                read("leader.pid"),
                read("descendant.pid"),
                read("orphan.pid"),
            ) {
                let started = Self {
                    leader,
                    session,
                    orphan,
                };
                for detached in [session, orphan] {
                    assert_ne!(rustix::process::getpgid(Some(detached)), Ok(leader));
                    assert_eq!(rustix::process::test_kill_process(detached), Ok(()));
                }
                return started;
            }
            assert!(Instant::now() < deadline, "stand-in harness never started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Asserts that within one second none of the three processes is left.
    /// Takes nothing. Panics with the surviving process otherwise.
    pub(in crate::engine) fn assert_gone(&self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        for process in [self.leader, self.session, self.orphan] {
            while rustix::process::test_kill_process(process) != Err(rustix::io::Errno::SRCH) {
                assert!(Instant::now() < deadline, "process {process:?} survived");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for DetachedProcesses {
    fn drop(&mut self) {
        for process in [self.session, self.orphan, self.leader] {
            let _ = rustix::process::kill_process(process, rustix::process::Signal::KILL);
        }
    }
}

/// The process an [`ORPHANING_HARNESS`] left in a session of its own with launchd as its
/// parent. Dropping it kills it, so a failing test leaves none.
#[cfg(target_os = "macos")]
pub(in crate::engine) struct Orphan(pub(in crate::engine) Pid);

#[cfg(target_os = "macos")]
impl Orphan {
    /// Waits up to fifteen seconds until the stand-in wrote its id and the orphan's into the
    /// [`RUN_FOLDER`] of `folder` and launchd is the orphan's parent, then checks the orphan
    /// left the harness session and is alive. Takes the stand-in's working folder. Returns the
    /// orphan.
    pub(in crate::engine) fn wait(folder: &Path) -> Self {
        let read = |name: &str| {
            fs::read_to_string(folder.join(RUN_FOLDER).join(name))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .and_then(Pid::from_raw)
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let (Some(leader), Some(orphan)) = (read("leader.pid"), read("orphan.pid"))
                && parent_of(orphan) == Some(1)
            {
                let orphan = Self(orphan);
                assert_ne!(rustix::process::getsid(Some(orphan.0)), Ok(leader));
                assert_eq!(rustix::process::test_kill_process(orphan.0), Ok(()));
                return orphan;
            }
            assert!(Instant::now() < deadline, "the stand-in left no orphan");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Orphan {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::KILL);
    }
}

/// Reads a process's parent id with `ps`. Takes the process. Returns the parent, or none once
/// it has ended.
#[cfg(target_os = "macos")]
fn parent_of(process: Pid) -> Option<i32> {
    let listing = crate::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &process.as_raw_nonzero().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .ok()?;
    String::from_utf8_lossy(&listing.stdout).trim().parse().ok()
}
