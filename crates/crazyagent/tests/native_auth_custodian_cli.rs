use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const CHILD_BOUND: Duration = Duration::from_secs(15);
const OUTPUT_BOUND: usize = 4096;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match self.0.try_wait() {
                    Ok(Some(_)) | Err(_) => return,
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        }
    }
}

struct Observation {
    status: ExitStatus,
    input_delivered: bool,
    stdout: Vec<u8>,
    stderr: String,
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-custodian-cli-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn bounded_stream<R: Read>(stream: R) -> Vec<u8> {
    let mut bytes = Vec::new();
    stream
        .take((OUTPUT_BOUND + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn observe(root: &Path, arguments: &[OsString], input: &[u8], close_input: bool) -> Observation {
    assert!(input.len() <= 33);
    let mut child = OwnedChild(
        std::process::Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(arguments)
            .env_clear()
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "en_US.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || bounded_stream(stdout));
    let stderr = std::thread::spawn(move || bounded_stream(stderr));
    let mut writer = child.0.stdin.take();
    let input_delivered = writer.as_mut().unwrap().write_all(input).is_ok();
    if close_input {
        drop(writer.take());
    }
    let deadline = Instant::now() + CHILD_BOUND;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "custodian CLI exceeded child deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(writer);
    assert!(child.0.try_wait().unwrap().is_some());
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(
        stdout.len() <= OUTPUT_BOUND,
        "custodian CLI stdout exceeded bound"
    );
    assert!(
        stderr.len() <= OUTPUT_BOUND,
        "custodian CLI stderr exceeded bound"
    );
    Observation {
        status,
        input_delivered,
        stdout,
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

fn arguments(root: &Path) -> Vec<OsString> {
    vec!["native-auth-custodian".into(), root.as_os_str().to_owned()]
}

fn assert_rejection(observed: Observation, expected: &str) {
    assert_eq!(observed.status.code(), Some(1));
    assert!(observed.input_delivered);
    assert!(observed.stdout.is_empty());
    assert_eq!(observed.stderr.trim(), expected);
}

#[test]
fn native_auth_custodian_cli_rejects_argument_shapes_before_bootstrap() {
    let root = private_root();
    let missing = vec![OsString::from("native-auth-custodian")];
    let mut extra = arguments(root.path());
    extra.push("extra".into());
    let excessive = vec![OsString::from("native-auth-custodian"); 12];
    for args in [missing, extra, excessive] {
        assert_rejection(
            observe(root.path(), &args, &[], true),
            "usage: crazyagent native-auth-custodian RUNTIME_DIR",
        );
    }
}

#[test]
fn native_auth_custodian_cli_rejects_empty_bootstrap() {
    let root = private_root();
    assert_rejection(
        observe(root.path(), &arguments(root.path()), &[], true),
        "native_auth_bootstrap_truncated",
    );
}

#[test]
fn native_auth_custodian_cli_rejects_excess_bootstrap() {
    let root = private_root();
    assert_rejection(
        observe(root.path(), &arguments(root.path()), &[0xff; 33], true),
        "native_auth_bootstrap_excess",
    );
}

#[test]
fn native_auth_custodian_cli_token_without_owner_grants_nothing() {
    let root = private_root();
    assert_rejection(
        observe(root.path(), &arguments(root.path()), &[0xff; 32], true),
        "owner_locator_missing",
    );
}

#[test]
fn native_auth_custodian_cli_live_bootstrap_without_eof_times_out() {
    let root = private_root();
    assert_rejection(
        observe(root.path(), &arguments(root.path()), &[0xff; 32], false),
        "native_auth_bootstrap_timeout",
    );
}

#[test]
fn native_auth_custodian_cli_rejects_regular_stdin_without_changing_its_flags() {
    let root = private_root();
    let path = root.path().join("input");
    std::fs::write(&path, [0xff; 32]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let input = std::fs::File::open(path).unwrap();
    let original_flags = rustix::fs::fcntl_getfl(&input).unwrap();
    let mut child = OwnedChild(
        std::process::Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(arguments(root.path()))
            .env_clear()
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::from(input.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || bounded_stream(stdout));
    let stderr = std::thread::spawn(move || bounded_stream(stderr));
    let deadline = Instant::now() + CHILD_BOUND;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "custodian CLI exceeded child deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(stdout.len() <= OUTPUT_BOUND);
    assert!(stderr.len() <= OUTPUT_BOUND);
    assert_eq!(status.code(), Some(1));
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).unwrap().trim(),
        "native_auth_bootstrap_input_not_pipe"
    );
    assert_eq!(rustix::fs::fcntl_getfl(&input).unwrap(), original_flags);
}

#[test]
fn engine_codex_executable_relative_path_rejects_before_config_or_runtime_io() {
    let root = private_root();
    let runtime = root.path().join("runtime-not-created");
    let base: Vec<OsString> = vec![
        "engine".into(),
        runtime.as_os_str().to_owned(),
        root.path().join("absent-global.toml").into_os_string(),
        root.path().join("absent-project.toml").into_os_string(),
        "e.sock".into(),
    ];
    for suffix in [
        vec!["--codex-executable", "relative-codex"],
        vec![
            "--writable-settings",
            "--codex-executable",
            "relative-codex",
        ],
        vec![
            "--codex-executable",
            "relative-codex",
            "--writable-settings",
        ],
    ] {
        let mut args = base.clone();
        args.extend(suffix.into_iter().map(OsString::from));
        assert_rejection(
            observe(root.path(), &args, &[], true),
            "native_auth_host_executable_unsafe",
        );
        assert!(!runtime.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn native_auth_custodian_cli_explicit_codex_keeps_private_bootstrap_requirement() {
    let root = private_root();
    let mut args = arguments(root.path());
    args.extend([
        OsString::from("--codex-executable"),
        root.path().join("codex-not-opened").into_os_string(),
        OsString::from("--codex-sha256"),
        OsString::from("0".repeat(64)),
    ]);
    assert_rejection(
        observe(root.path(), &args, &[], true),
        "native_auth_bootstrap_truncated",
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn native_auth_custodian_cli_explicit_codex_rejects_invalid_pin_before_bootstrap() {
    let root = private_root();
    for (executable, digest, expected) in [
        (
            OsString::from("relative-codex"),
            "0".repeat(64),
            "native_auth_host_executable_unsafe",
        ),
        (
            root.path().join("codex").into_os_string(),
            "0".repeat(63),
            "native_auth_host_executable_digest_invalid",
        ),
        (
            root.path().join("codex").into_os_string(),
            "A".repeat(64),
            "native_auth_host_executable_digest_invalid",
        ),
    ] {
        let mut args = arguments(root.path());
        args.extend([
            OsString::from("--codex-executable"),
            executable,
            OsString::from("--codex-sha256"),
            digest.into(),
        ]);
        assert_rejection(observe(root.path(), &args, &[], true), expected);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
