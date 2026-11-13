use super::super::custody::CustodySession;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeAuthAction, NativeAuthMethod, NativeProfileBinding};
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
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
        let child = crate::spawn_gate::spawn(
            Command::new(program)
                .args(args)
                .env_clear()
                .current_dir(&self.home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
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
    let mut child = crate::spawn_gate::spawn(
        Command::new("/bin/cat")
            .env_clear()
            .current_dir(&case.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0),
    )
    .unwrap();
    let input = child.stdin.take().unwrap();
    if let Err((error, mut child)) = case.session.as_mut().unwrap().retain_child(child) {
        drop(input);
        let _ = child.wait();
        panic!("retained child rejected: {error}");
    }
    drop(input);
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
    let temp = tempfile::tempdir().unwrap();
    let codex = std::fs::canonicalize(temp.path()).unwrap().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let command = super::command(&codex, &case.home).unwrap();
    assert_eq!(command.get_program(), codex.as_os_str());
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

/// An npm-built Codex is a script whose first line asks `env` for its interpreter. Its
/// sign-in child starts that interpreter as the tool resolver finds it, by its absolute file,
/// then the script, with PATH /usr/bin:/bin and no folder added; a native program starts as
/// itself. The stand-in asks for `sh`, which the resolver finds on every Mac.
#[test]
fn native_auth_sign_in_child_starts_an_env_script_through_its_resolved_interpreter() {
    let temp = tempfile::tempdir().unwrap();
    let prefix = std::fs::canonicalize(temp.path())
        .unwrap()
        .join("nvm/versions/node/v22.11.0");
    let script = prefix.join("lib/node_modules/@openai/codex/bin/codex.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "#!/usr/bin/env sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::write(prefix.join("bin/node"), "node").unwrap();
    let case = Case::new();
    let command = super::command(&script, &case.home).unwrap();
    let interpreter =
        crate::tools::resolve("sh", &crate::tools::Places::from_environment(None)).unwrap();
    assert_eq!(command.get_program(), interpreter.program.as_os_str());
    assert_eq!(command.get_args().next(), Some(script.as_os_str()));
    let path = command
        .get_envs()
        .find(|(name, _)| *name == std::ffi::OsStr::new("PATH"))
        .and_then(|(_, value)| value.map(|value| value.to_owned()))
        .unwrap();
    assert_eq!(path, "/usr/bin:/bin", "no npm prefix folder is added");
    let native = prefix.join("codex-native");
    std::fs::write(&native, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        super::command(&native, &case.home).unwrap().get_program(),
        native.as_os_str()
    );
}

#[test]
fn native_auth_codex_launch_rejects_relative_executable_or_profile() {
    assert!(super::command(std::path::Path::new("codex"), std::path::Path::new("/home")).is_err());
    assert!(super::command(std::path::Path::new("/codex"), std::path::Path::new("home")).is_err());
}

#[test]
fn native_auth_codex_device_code_wait_outlasts_the_fifteen_minute_code() {
    let device = NativeAuthAction::SignIn {
        method: NativeAuthMethod::CodexDeviceCode,
    };
    let wait = super::run_time(&device).unwrap() - super::EXPIRY_MARGIN;
    assert!(wait > Duration::from_secs(15 * 60), "{wait:?}");
    for (method, time) in [
        (NativeAuthMethod::CodexBrowser, 590),
        (NativeAuthMethod::CodexExisting, 60),
    ] {
        let action = NativeAuthAction::SignIn { method };
        assert_eq!(super::run_time(&action).unwrap(), Duration::from_secs(time));
    }
    assert_eq!(
        super::run_time(&NativeAuthAction::Logout).unwrap(),
        Duration::from_secs(5)
    );
    let claude = NativeAuthAction::SignIn {
        method: NativeAuthMethod::ClaudeOfficial,
    };
    assert!(super::run_time(&claude).is_err());
}

#[test]
fn native_auth_codex_stderr_tail_is_masked_for_the_engine_log() {
    let stderr = b"2026-11-05T14:53:30Z ERROR codex_login: code QX4F-9KTB at \
https://auth.openai.com/codex/device?user_code=QX4F9KTB for owner@example.test \
token sk-live-9a8b7c6d5e4f3a2b qx4f9ktb \x1b[31mred\x1b[0m\n\
ERROR codex_login: device auth timed out after 15 minutes\n";
    let text = super::masked(stderr, Some("QX4F-9KTB"));
    for secret in [
        "QX4F",
        "9KTB",
        "qx4f",
        "auth.openai.com",
        "owner@example.test",
        "example",
        "9a8b7c6d5e4f3a2b",
        "\x1b",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(
        text.ends_with("ERROR codex_login: device auth timed out after 15 minutes"),
        "{text}"
    );
    assert!(text.len() <= 512);
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    );
    let long = vec![b'a'; 64 * 1024];
    assert!(super::masked(&long, None).len() <= 512);
}

#[test]
fn native_auth_codex_dropped_notification_names_keep_only_short_plain_methods() {
    assert_eq!(
        super::dropped_name("account/login/completedV2", None),
        "account/login/completedV2"
    );
    assert_eq!(super::dropped_name("", None), "*");
    assert_eq!(super::dropped_name("account/login code=QX4F", None), "*");
    assert_eq!(super::dropped_name(&"m".repeat(65), None), "*");
    assert_eq!(super::dropped_name(&"m".repeat(64), None), "m".repeat(64));
}

#[test]
fn native_auth_codex_stderr_masks_a_letters_only_code_inside_joined_words() {
    let stderr = br#"ERROR codex_login: {"user_code":"WDJB-MJHT", code:WDJB-MJHT (code:WDJB-MJHT) Code:"wdjb-mjht" token=WDJBMJHT xWDJBx MJHT.
ERROR codex_login: device auth timed out after 15 minutes"#;
    let text = super::masked(stderr, Some("WDJB-MJHT"));
    for secret in ["WDJB", "MJHT", "wdjb", "mjht"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(
        text.ends_with("ERROR codex_login: device auth timed out after 15 minutes"),
        "{text}"
    );
    let unknown = super::masked(
        br#"{"user_code":"ABCD-EFGH"} code:KLMN-PQRS userCode=TUVW-XYZA user code: BCDF-GHJK ok"#,
        None,
    );
    for secret in [
        "ABCD", "EFGH", "KLMN", "PQRS", "TUVW", "XYZA", "BCDF", "GHJK",
    ] {
        assert!(!unknown.contains(secret), "{secret} in {unknown}");
    }
    assert!(unknown.ends_with(" ok"), "{unknown}");
}

#[test]
fn native_auth_codex_dropped_names_never_carry_a_letters_only_code() {
    let code = Some("WDJB-MJHT");
    for method in [
        "account/login/WDJB-MJHT",
        "account/login/wdjbmjht",
        "device/WDJB",
        "codeMJHT/x",
    ] {
        assert_eq!(super::dropped_name(method, code), "*", "{method}");
    }
    assert_eq!(
        super::dropped_name("account/login/completedV2", code),
        "account/login/completedV2"
    );
    assert_eq!(super::dropped_name("code/KLMN-PQRS", None), "*");
}

/// A sign-in or token renewal child whose harness file sits inside a project, where agents
/// can write, never starts unsandboxed: the command is refused with the tool resolver's
/// owner line for that file.
#[test]
fn a_sign_in_harness_inside_a_project_is_refused_with_the_resolvers_line() {
    let temp = tempfile::tempdir().unwrap();
    let project = std::fs::canonicalize(temp.path()).unwrap().join("project");
    std::fs::create_dir_all(project.join(".crazyagent")).unwrap();
    let entry = project.join("bin").join("codex");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let line = crate::tools::resolve_entry(&entry, &crate::tools::Places::from_environment(None))
        .unwrap_err()
        .line();
    let refused = crate::engine::native_auth_host::sign_in_command(&entry).unwrap_err();
    assert_eq!(refused.0, line);
}
