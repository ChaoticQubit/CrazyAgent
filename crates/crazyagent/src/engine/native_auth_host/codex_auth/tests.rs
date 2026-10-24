use super::super::custody::CustodySession;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Case {
    _root: tempfile::TempDir,
    session: Option<CustodySession>,
    home: PathBuf,
}

struct GroupFallback(rustix::process::Pid);

impl Drop for GroupFallback {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-auth-leaf-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut custody =
            NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap())
                .unwrap();
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let connection = ModelConnection {
            id: reference("leaf-connection"),
            provider: reference("leaf-provider"),
            harness: reference("codex"),
            protocol: reference("native"),
            account_ref: reference("leaf-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "leaf-model".into(),
                author: reference("leaf-author"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("leaf-profile"),
            },
        };
        let (custody_id, _) = custody.prepare(&host, &connection).unwrap();
        let binding = NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection,
            custody_id,
        };
        let guard = custody
            .acquire_use(
                &host,
                &binding,
                &RequestId::new("leaf-sign-in").unwrap(),
                1,
                NativeProfileUsePurpose::Authentication,
            )
            .unwrap();
        let home = root
            .path()
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(b"leaf-profile")))
            .join("home");
        Self {
            _root: root,
            session: Some(CustodySession::new(guard)),
            home,
        }
    }

    fn retain(&mut self, program: &str, args: &[&str]) {
        let child = Command::new(program)
            .args(args)
            .env_clear()
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        if let Err((error, mut child)) = self.session.as_mut().unwrap().retain_child(child) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("retained child rejected: {error}");
        }
    }
}

#[test]
fn native_auth_codex_profile_path_is_the_held_physical_home() {
    let case = Case::new();
    assert_eq!(
        case.session.as_ref().unwrap().profile_path().unwrap(),
        case.home
    );
    assert!(case.home.is_dir());
}

#[test]
fn native_auth_codex_child_access_requires_an_actual_retained_child() {
    let mut case = Case::new();
    assert_eq!(
        case.session.as_mut().unwrap().child_mut().unwrap_err().0,
        "native_auth_child_not_retained"
    );
    case.retain("/bin/sleep", &["30"]);
    let child = case.session.as_mut().unwrap().child_mut().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    let direct = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    assert_eq!(rustix::process::getpgid(Some(direct)).unwrap(), direct);
}

#[test]
fn native_auth_codex_retirement_rejects_an_unreaped_live_child() {
    let mut case = Case::new();
    case.retain("/bin/sleep", &["30"]);
    let result = case.session.take().unwrap().seal_after_child_reaped();
    assert!(matches!(result, Err(error) if error.0 == "native_auth_child_cleanup_uncertain"));
}

#[test]
fn native_auth_codex_retirement_requires_a_spawned_child() {
    let mut case = Case::new();
    let result = case.session.take().unwrap().seal_after_child_reaped();
    assert!(matches!(result, Err(error) if error.0 == "native_auth_child_not_retained"));
}

#[test]
fn native_auth_codex_retirement_accepts_a_reaped_child_and_absent_group() {
    let mut case = Case::new();
    case.retain("/usr/bin/true", &[]);
    let child = case.session.as_mut().unwrap().child_mut().unwrap();
    assert!(child.wait().unwrap().success());
    let direct = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    assert_eq!(
        rustix::process::test_kill_process_group(direct),
        Err(rustix::io::Errno::SRCH)
    );
    assert!(
        case.session
            .take()
            .unwrap()
            .seal_after_child_reaped()
            .is_ok()
    );
}

#[test]
fn native_auth_codex_error_cleanup_reaps_the_real_owned_group_before_releasing_profile() {
    let mut case = Case::new();
    let marker = case.home.join("child-started");
    case.retain(
        "/bin/sh",
        &[
            "-c",
            "/bin/sleep 30 & printf ready > \"$1\"; wait",
            "leaf-cleanup",
            marker.to_str().unwrap(),
        ],
    );
    let direct = case.session.as_mut().unwrap().child_mut().unwrap().id();
    let group = rustix::process::Pid::from_raw(i32::try_from(direct).unwrap()).unwrap();
    let _fallback = GroupFallback(group);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.is_file() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(rustix::process::test_kill_process_group(group).is_ok());
    drop(case.session.take().unwrap());
    assert_eq!(
        rustix::process::test_kill_process_group(group),
        Err(rustix::io::Errno::SRCH)
    );
    assert!(matches!(
        rustix::process::waitpid(Some(group), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn native_auth_codex_launch_uses_official_stdio_and_only_explicit_environment() {
    let case = Case::new();
    let command = super::command(std::path::Path::new("/qualified/codex"), &case.home).unwrap();
    assert_eq!(command.get_program(), "/qualified/codex");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "app-server",
            "--listen",
            "stdio://"
        ]
    );
    let environment = command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.unwrap().to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        environment[std::ffi::OsStr::new("CODEX_HOME")],
        case.home.as_os_str()
    );
    assert!(!environment.contains_key(std::ffi::OsStr::new("OPENAI_API_KEY")));
    assert!(!environment.contains_key(std::ffi::OsStr::new("CHATGPT_API_KEY")));
    assert_eq!(command.get_current_dir(), Some(case.home.as_path()));
}

#[test]
fn native_auth_codex_launch_rejects_relative_executable_or_profile() {
    assert!(super::command(std::path::Path::new("codex"), std::path::Path::new("/home")).is_err());
    assert!(super::command(std::path::Path::new("/codex"), std::path::Path::new("home")).is_err());
}
