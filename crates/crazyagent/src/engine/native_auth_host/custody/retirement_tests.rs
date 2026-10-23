use super::{CustodySession, Retirement, cleanup_direct_child};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use rustix::fs::{FlockOperation, flock};
use rustix::process::Pid;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_TIME: Duration = Duration::from_secs(2);

struct Case {
    session: Option<CustodySession>,
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
            .prefix("ca-retirement-")
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
            id: reference("retirement-connection"),
            provider: reference("retirement-provider"),
            harness: reference("codex"),
            protocol: reference("native"),
            account_ref: reference("retirement-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "retirement-model".into(),
                author: reference("retirement-author"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("retirement-profile"),
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
        let operation = RequestId::new("retirement-operation").unwrap();
        let guard = custody
            .acquire_use(
                &host,
                &binding,
                &operation,
                1,
                NativeProfileUsePurpose::Authentication,
            )
            .unwrap();
        let profile = root.path().join("native-profiles").join(key);
        Self {
            session: Some(CustodySession::new(guard)),
            host,
            binding,
            operation,
            home: profile.join("home"),
            manifest: profile.join("identity.json"),
            root,
        }
    }

    fn no_effect(&mut self) -> Retirement {
        self.session.take().unwrap().seal_without_effect().unwrap()
    }

    fn child_reaped(&mut self) -> Retirement {
        let mut child = Command::new("/bin/cat")
            .env_clear()
            .current_dir(&self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let direct = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let session = self.session.as_mut().unwrap();
        if let Err((error, mut rejected)) = session.retain_child(child) {
            drop(input);
            cleanup_direct_child(&mut rejected, Instant::now() + CHILD_TIME)
                .unwrap()
                .expect("rejected real child must be reaped");
            panic!("retained real child rejected: {error}");
        }
        assert_eq!(rustix::process::getpgid(Some(direct)).unwrap(), direct);
        assert!(session.child_mut().unwrap().try_wait().unwrap().is_none());
        drop(input);
        let deadline = Instant::now() + CHILD_TIME;
        loop {
            if let Some(status) = session.child_mut().unwrap().try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real cat child exceeded its bound"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            rustix::process::test_kill_process_group(direct),
            Err(rustix::io::Errno::SRCH)
        );
        self.session
            .take()
            .unwrap()
            .seal_after_child_reaped()
            .unwrap()
    }

    fn validate(&self, retirement: &Retirement, after_child: bool) -> Result<(), EngineError> {
        if after_child {
            retirement.revalidate_after_child_reaped(
                &self.host,
                &self.binding,
                &self.operation,
                1,
                NativeProfileUsePurpose::Authentication,
            )
        } else {
            retirement.revalidate(
                &self.host,
                &self.binding,
                &self.operation,
                1,
                NativeProfileUsePurpose::Authentication,
            )
        }
    }

    fn probe(&self) -> File {
        File::open(&self.manifest).unwrap()
    }
}

fn assert_locked(probe: &File) {
    assert!(matches!(
        flock(probe, FlockOperation::NonBlockingLockExclusive),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN
    ));
}

/// Proves the guard released its lock, waiting at most two seconds for a
/// concurrent test's fork-to-exec window to close its inherited descriptor.
/// Input: an independent descriptor on the guarded manifest.
fn assert_released(probe: &File) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match flock(probe, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(error)
                if (error == rustix::io::Errno::WOULDBLOCK
                    || error == rustix::io::Errno::AGAIN)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => panic!("guard lock was not released: {error:?}"),
        }
    }
    flock(probe, FlockOperation::Unlock).unwrap();
}

#[test]
fn no_effect_receipt_accepts_no_effect_validation_and_retains_physical_guard() {
    let mut case = Case::new();
    let probe = case.probe();
    assert_locked(&probe);
    let retirement = case.no_effect();
    case.validate(&retirement, false).unwrap();
    assert_locked(&probe);
    drop(retirement);
    assert_released(&probe);
}

#[test]
fn no_effect_receipt_refuses_post_reap_validation_without_releasing_guard() {
    let mut case = Case::new();
    let probe = case.probe();
    let retirement = case.no_effect();
    assert_eq!(
        case.validate(&retirement, true).unwrap_err().0,
        "native_auth_child_cleanup_uncertain"
    );
    assert_locked(&probe);
    case.validate(&retirement, false).unwrap();
    drop(retirement);
    assert_released(&probe);
}

#[test]
fn reaped_child_receipt_accepts_post_reap_validation_and_retains_physical_guard() {
    let mut case = Case::new();
    let probe = case.probe();
    let retirement = case.child_reaped();
    case.validate(&retirement, true).unwrap();
    assert_locked(&probe);
    drop(retirement);
    assert_released(&probe);
}

#[test]
fn reaped_child_receipt_refuses_no_effect_validation_without_releasing_guard() {
    let mut case = Case::new();
    let probe = case.probe();
    let retirement = case.child_reaped();
    assert_eq!(
        case.validate(&retirement, false).unwrap_err().0,
        "native_auth_child_effect_requires_retirement"
    );
    assert_locked(&probe);
    case.validate(&retirement, true).unwrap();
    drop(retirement);
    assert_released(&probe);
}

#[test]
fn post_reap_validation_preserves_every_operation_identity_join() {
    let mut case = Case::new();
    let retirement = case.child_reaped();
    for changed in 0..7 {
        let mut host = case.host.clone();
        let mut binding = case.binding.clone();
        let mut operation = case.operation.clone();
        let mut generation = 1;
        let mut purpose = NativeProfileUsePurpose::Authentication;
        match changed {
            0 => host.db_instance = "e".repeat(64),
            1 => host.engine_epoch = "e".repeat(64),
            2 => binding.custody_id = "e".repeat(64),
            3 => binding.connection.account_ref = reference("different-account"),
            4 => operation = RequestId::new("different-operation").unwrap(),
            5 => generation = 2,
            6 => purpose = NativeProfileUsePurpose::Logout,
            _ => unreachable!(),
        }
        assert_eq!(
            retirement
                .revalidate_after_child_reaped(&host, &binding, &operation, generation, purpose)
                .unwrap_err()
                .0,
            "native_profile_binding_conflict"
        );
    }
    case.validate(&retirement, true).unwrap();
}

#[test]
fn both_retirement_kinds_reject_replaced_home_and_keep_the_original_guard() {
    for after_child in [false, true] {
        let mut case = Case::new();
        let probe = case.probe();
        let retirement = if after_child {
            case.child_reaped()
        } else {
            case.no_effect()
        };
        fs::rename(&case.home, case.root.path().join("original-home")).unwrap();
        fs::create_dir(&case.home).unwrap();
        fs::set_permissions(&case.home, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(case.validate(&retirement, after_child).is_err());
        assert_locked(&probe);
        drop(retirement);
        assert_released(&probe);
    }
}
