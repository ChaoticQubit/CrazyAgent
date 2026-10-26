use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use crate::sandbox::{InstalledSandbox, ProjectSandbox, SandboxNetwork, SandboxRemote};
use rustix::process::Pid;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A stand-in harness that starts a descendant sharing its stdout and stderr, writes both
/// process ids into its working folder and then sleeps, so only a group kill ends it.
pub(in crate::engine) const PIPE_HOLDING_HARNESS: &str = "#!/bin/sh
/bin/sleep 30 &
echo $! > descendant.pid
echo $$ > leader.pid
exec /bin/sleep 30
";

/// A stand-in harness whose descendant moves into its own process group before it writes its
/// id, still holding the harness stdout and stderr, so no group kill can reach it.
pub(in crate::engine) const GROUP_LEAVING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -e 'setpgrp(0, 0) or exit 1; open(my $file, ">", "descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' &
echo $$ > leader.pid
exec /bin/sleep 30
"#;

/// The start of a stand-in harness that leaves what a Claude Code Bash tool shell can leave: a
/// process that started its own session with its own stdio, as each tool shell does, and a
/// `nohup` command whose shell already exited. It writes their ids into its working folder; a
/// test appends how the harness itself ends.
#[cfg(target_os = "macos")]
pub(in crate::engine) const DETACHING_HARNESS: &str = r#"#!/bin/sh
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec "/bin/sh", "-c", q(/usr/bin/nohup /bin/sleep 30 < /dev/null > /dev/null 2>&1 & echo $! > orphan.pid)' < /dev/null > /dev/null 2>&1
echo $$ > leader.pid
"#;

/// A real native profile custody in a private engine folder under `/private/tmp`, with a
/// project folder, for master host tests that need an actual profile lease and sandbox.
pub(in crate::engine) struct MasterFixture {
    pub(in crate::engine) root: tempfile::TempDir,
    pub(in crate::engine) project: PathBuf,
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
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
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
        Self {
            root,
            project,
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
        InstalledSandbox::install(sandbox, &self.root.path().join("master.sb")).unwrap()
    }
}

/// The two real processes a [`PIPE_HOLDING_HARNESS`] started: the harness itself and its
/// descendant. Dropping it kills both and the harness group, so a failing test leaves none.
pub(in crate::engine) struct HarnessProcesses {
    pub(in crate::engine) leader: Pid,
    pub(in crate::engine) descendant: Pid,
}

impl HarnessProcesses {
    /// Waits up to five seconds until the stand-in wrote both process ids into `folder`.
    /// Takes the stand-in's working folder. Returns both processes.
    pub(in crate::engine) fn wait(folder: &Path) -> Self {
        let read = |name: &str| {
            fs::read_to_string(folder.join(name))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .and_then(Pid::from_raw)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let (Some(leader), Some(descendant)) = (read("leader.pid"), read("descendant.pid")) {
                assert_ne!(leader, descendant);
                return Self { leader, descendant };
            }
            assert!(Instant::now() < deadline, "stand-in harness never started");
            std::thread::sleep(Duration::from_millis(10));
        }
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
            rustix::process::test_kill_process_group(self.leader),
            Err(rustix::io::Errno::SRCH),
            "harness group {:?} survived",
            self.leader
        );
    }
}

impl Drop for HarnessProcesses {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.descendant, rustix::process::Signal::KILL);
        let _ = rustix::process::kill_process_group(self.leader, rustix::process::Signal::KILL);
    }
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
    /// Waits up to five seconds until the stand-in wrote all three ids into `folder`, then
    /// checks the two detached ones left the harness group and are alive.
    /// Takes the stand-in's working folder. Returns the three processes.
    pub(in crate::engine) fn wait(folder: &Path) -> Self {
        let read = |name: &str| {
            fs::read_to_string(folder.join(name))
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
