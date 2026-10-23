use super::super::RegisteredNativeAuthHost;
use super::super::custody::{CustodySession, Retirement, cleanup_owned_group};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use rustix::fs::{FlockOperation, flock};
use rustix::process::Pid;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_TIME: Duration = Duration::from_secs(2);

struct Case {
    session: Option<CustodySession>,
    custody: NativeProfileCustody,
    host: HostStamp,
    binding: NativeProfileBinding,
    operation: RequestId,
    home: PathBuf,
    manifest: PathBuf,
    root: tempfile::TempDir,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-claude-lifecycle-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        let mut custody =
            NativeProfileCustody::capture(&database, &fs::metadata(&database).unwrap()).unwrap();
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let connection = ModelConnection {
            id: reference("claude-lifecycle-connection"),
            provider: reference("anthropic"),
            harness: reference("claude"),
            protocol: reference("native"),
            account_ref: reference("claude-lifecycle-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "claude-lifecycle-model".into(),
                author: reference("anthropic"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("claude-lifecycle-profile"),
            },
        };
        let (custody_id, key) = custody.prepare(&host, &connection).unwrap();
        let binding = NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection,
            custody_id,
        };
        let operation = RequestId::new("claude-lifecycle-operation").unwrap();
        let guard = custody
            .acquire_use(
                &host,
                &binding,
                &operation,
                7,
                NativeProfileUsePurpose::Authentication,
            )
            .unwrap();
        let profile = root.path().join("native-profiles").join(key);
        Self {
            session: Some(CustodySession::new(guard)),
            custody,
            host,
            binding,
            operation,
            home: profile.join("home"),
            manifest: profile.join("identity.json"),
            root,
        }
    }

    fn session(&mut self) -> &mut CustodySession {
        self.session.as_mut().unwrap()
    }

    fn spawn(&mut self, mut command: Command) -> (ChildStdin, Pid) {
        let mut child = command
            .env_clear()
            .current_dir(&self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let group = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        assert_eq!(rustix::process::getpgid(Some(group)).unwrap(), group);
        if let Err((error, mut child)) = self.session().retain_child(child) {
            drop(input);
            assert!(cleanup_owned_group(&mut child, group, Instant::now() + CHILD_TIME).unwrap());
            panic!("real process fixture could not be retained: {error}");
        }
        assert!(
            self.session()
                .child_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none()
        );
        (input, group)
    }

    fn cat(&mut self) -> (ChildStdin, Pid) {
        self.spawn(Command::new("/bin/cat"))
    }

    fn reap(&mut self, input: ChildStdin, group: Pid) {
        drop(input);
        let deadline = Instant::now() + CHILD_TIME;
        loop {
            if let Some(status) = self.session().child_mut().unwrap().try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real child exceeded its retirement bound"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            rustix::process::test_kill_process_group(group),
            Err(rustix::io::Errno::SRCH)
        );
    }

    fn locked(&self) {
        let manifest = File::open(&self.manifest).unwrap();
        let result = flock(&manifest, FlockOperation::NonBlockingLockExclusive);
        assert!(result.is_err_and(|error| {
            error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN
        }));
    }

    fn two_children(&mut self) -> Retirement {
        let (input, group) = self.cat();
        self.reap(input, group);
        self.session().prepare_status_child().unwrap();
        let (input, group) = self.cat();
        self.reap(input, group);
        self.session
            .take()
            .unwrap()
            .seal_after_child_reaped()
            .unwrap()
    }
}

#[test]
fn claude_auth_lifecycle_retains_one_physical_guard_across_two_real_children() {
    let mut case = Case::new();
    let original_home = case.session().profile_path().unwrap();
    let (input, group) = case.cat();
    case.locked();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    assert_eq!(case.session().profile_path().unwrap(), original_home);
    case.locked();
    let competing = case.custody.acquire_use(
        &case.host,
        &case.binding,
        &RequestId::new("competing-claude-operation").unwrap(),
        8,
        NativeProfileUsePurpose::Authentication,
    );
    assert!(matches!(competing, Err(error) if error.0 == "native_profile_in_use"));
    let (input, group) = case.cat();
    case.reap(input, group);
    let retirement = case
        .session
        .take()
        .unwrap()
        .seal_after_child_reaped()
        .unwrap();
    retirement
        .revalidate_after_child_reaped(
            &case.host,
            &case.binding,
            &case.operation,
            7,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    case.locked();
    drop(retirement);
    let next = case.custody.acquire_use(
        &case.host,
        &case.binding,
        &RequestId::new("next-claude-operation").unwrap(),
        8,
        NativeProfileUsePurpose::Authentication,
    );
    assert!(next.is_ok());
}

#[test]
fn claude_auth_lifecycle_rejects_status_handoff_without_a_retained_first_child() {
    let mut case = Case::new();
    assert!(case.session().prepare_status_child().is_err());
    let retirement = case.session.take().unwrap().seal_without_effect().unwrap();
    retirement
        .revalidate(
            &case.host,
            &case.binding,
            &case.operation,
            7,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
}

#[test]
fn claude_auth_lifecycle_refuses_status_handoff_while_first_child_is_alive() {
    let mut case = Case::new();
    let (input, group) = case.cat();
    assert!(case.session().prepare_status_child().is_err());
    assert!(
        case.session()
            .child_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
    );
    case.locked();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    case.locked();
}

#[test]
fn claude_auth_lifecycle_refuses_status_handoff_until_first_group_is_absent() {
    let mut case = Case::new();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "read ignored; /bin/sleep 30 </dev/null & exit 0"]);
    let (mut input, group) = case.spawn(command);
    input.write_all(b"continue\n").unwrap();
    drop(input);
    let deadline = Instant::now() + CHILD_TIME;
    loop {
        if let Some(status) = case.session().child_mut().unwrap().try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "real shell child did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(rustix::process::test_kill_process_group(group).is_ok());
    assert!(case.session().prepare_status_child().is_err());
    case.locked();
    assert!(
        cleanup_owned_group(
            case.session().child_mut().unwrap(),
            group,
            Instant::now() + CHILD_TIME,
        )
        .unwrap()
    );
    case.session().prepare_status_child().unwrap();
}

#[test]
fn claude_auth_lifecycle_admits_only_one_final_status_child() {
    let mut case = Case::new();
    let (input, group) = case.cat();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    assert!(case.session().prepare_status_child().is_err());
    let (input, group) = case.cat();
    case.reap(input, group);
    assert!(case.session().prepare_status_child().is_err());
    let retirement = case
        .session
        .take()
        .unwrap()
        .seal_after_child_reaped()
        .unwrap();
    assert!(
        retirement
            .revalidate(
                &case.host,
                &case.binding,
                &case.operation,
                7,
                NativeProfileUsePurpose::Authentication,
            )
            .is_err()
    );
}

#[test]
fn claude_auth_lifecycle_never_relabels_first_child_effect_as_no_effect() {
    let mut case = Case::new();
    let (input, group) = case.cat();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    let result = case.session.take().unwrap().seal_without_effect();
    assert!(matches!(
        result,
        Err(error) if error.0 == "native_auth_child_effect_requires_retirement"
    ));
}

#[test]
fn claude_auth_lifecycle_can_retire_first_child_when_cancelled_before_status_spawn() {
    let mut case = Case::new();
    let (input, group) = case.cat();
    let original_id = case.session().child_mut().unwrap().id();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    assert_eq!(case.session().child_mut().unwrap().id(), original_id);
    assert!(
        case.session()
            .child_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_some()
    );
    let retirement = case
        .session
        .take()
        .unwrap()
        .seal_after_child_reaped()
        .unwrap();
    retirement
        .revalidate_after_child_reaped(
            &case.host,
            &case.binding,
            &case.operation,
            7,
            NativeProfileUsePurpose::Authentication,
        )
        .unwrap();
    case.locked();
}

#[test]
fn claude_auth_lifecycle_rechecks_physical_home_before_status_admission() {
    let mut case = Case::new();
    let (input, group) = case.cat();
    case.reap(input, group);
    fs::rename(&case.home, case.root.path().join("original-held-home")).unwrap();
    fs::create_dir(&case.home).unwrap();
    fs::set_permissions(&case.home, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(case.session().prepare_status_child().is_err());
    assert!(case.session().profile_path().is_err());
    case.locked();
}

#[test]
fn claude_auth_lifecycle_retirement_keeps_account_operation_and_generation_binding() {
    let mut case = Case::new();
    let retirement = case.two_children();
    for (operation, generation, binding) in [
        (
            RequestId::new("other-operation").unwrap(),
            7,
            case.binding.clone(),
        ),
        (case.operation.clone(), 8, case.binding.clone()),
        (case.operation.clone(), 7, {
            let mut other = case.binding.clone();
            other.connection.account_ref = reference("other-account");
            other
        }),
    ] {
        assert!(
            retirement
                .revalidate_after_child_reaped(
                    &case.host,
                    &binding,
                    &operation,
                    generation,
                    NativeProfileUsePurpose::Authentication,
                )
                .is_err()
        );
    }
    case.locked();
}

#[test]
fn claude_auth_lifecycle_retained_executable_rejects_replacement_before_status_child() {
    let mut case = Case::new();
    let executable = case.root.path().join("registered-cat");
    fs::write(&executable, b"#!/bin/sh\nexec /bin/cat\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let registered = RegisteredNativeAuthHost::capture(&executable).unwrap();
    let digest = registered.verified_sha256().unwrap();
    let (input, group) = case.spawn(Command::new(registered.verified_path().unwrap()));
    let original_id = case.session().child_mut().unwrap().id();
    case.reap(input, group);
    case.session().prepare_status_child().unwrap();
    assert_eq!(registered.verified_sha256().unwrap(), digest);
    fs::rename(
        &executable,
        case.root.path().join("original-registered-cat"),
    )
    .unwrap();
    fs::write(&executable, b"#!/bin/sh\nexec /bin/cat\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(registered.verified_path().is_err());
    assert_eq!(case.session().child_mut().unwrap().id(), original_id);
    assert!(
        case.session()
            .child_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_some()
    );
    case.locked();
}
