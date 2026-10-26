#![cfg(unix)]

use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::paths::{Layout, Platform};
use crazyagent::service::{EngineCheck, READY_TIMEOUT, Service, Started};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_crazyagent");
const BARE_PATH: &str = "/usr/bin:/bin";

struct Engine(Child);

impl Drop for Engine {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            terminate(self.0.id());
            let deadline = Instant::now() + Duration::from_secs(20);
            while self.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
            }
        }
        self.0.wait().unwrap();
    }
}

struct Group(u32);

impl Drop for Group {
    fn drop(&mut self) {
        let pid = rustix::process::Pid::from_raw(self.0 as i32).unwrap();
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
    }
}

fn terminate(pid: u32) {
    let pid = rustix::process::Pid::from_raw(pid as i32).unwrap();
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn engine(args: &[&OsStr], home: Option<&Path>, path: &OsStr) -> Child {
    let mut command = Command::new(BIN);
    command
        .arg("engine")
        .args(args)
        .env("PATH", path)
        .env_remove("CRAZYAGENT_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = home {
        command.env("CRAZYAGENT_HOME", home);
    }
    crazyagent::spawn_gate::spawn(&mut command).unwrap()
}

fn ready(child: &mut Child) -> String {
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    receive.recv_timeout(READY_TIMEOUT).unwrap().unwrap()
}

fn stopped_stderr(mut engine: Engine) -> String {
    let mut stderr = engine.0.stderr.take().unwrap();
    drop(engine);
    let mut text = String::new();
    stderr.read_to_string(&mut text).unwrap();
    text
}

fn check(root: &Path) -> EngineCheck {
    runtime().block_on(crazyagent::service::check_engine(
        root,
        OwnerClientKind::Tui,
        Duration::from_secs(5),
    ))
}

fn ready_version() -> EngineCheck {
    EngineCheck::Ready {
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn wait_silent(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while crazyagent::service::answering(socket) {
        assert!(Instant::now() < deadline, "engine still answers");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn engine_without_arguments_prepares_the_default_layout_and_answers() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("CrazyAgent").join("engine");
    let mut child = engine(&[], Some(&root), OsStr::new(BARE_PATH));
    assert_eq!(ready(&mut child).trim(), "ready");
    let running = Engine(child);
    let layout = Layout::new(root.clone()).unwrap();
    for folder in [
        root.parent().unwrap().to_path_buf(),
        root.clone(),
        layout.logs(),
    ] {
        assert_eq!(mode(&folder), 0o700, "{}", folder.display());
    }
    assert_eq!(
        std::fs::read_to_string(layout.global_settings()).unwrap(),
        "version = 1\ntimeout_ms = 5000\n"
    );
    assert_eq!(mode(&layout.global_settings()), 0o600);
    assert_eq!(mode(&layout.project_settings()), 0o600);
    assert!(crazyagent::service::answering(&root.join("engine.sock")));
    assert_eq!(check(&root), ready_version());
    drop(running);
    wait_silent(&layout.socket());
}

#[test]
fn engine_home_flag_names_the_root_without_the_environment() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("flagged");
    let mut child = engine(
        &[OsStr::new("--home"), root.as_os_str()],
        None,
        OsStr::new(BARE_PATH),
    );
    assert_eq!(ready(&mut child).trim(), "ready");
    let running = Engine(child);
    assert_eq!(check(&root), ready_version());
    drop(running);
}

#[test]
fn a_path_harness_the_engine_cannot_register_is_skipped_not_fatal() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o775)).unwrap();
    let path = std::env::join_paths([bin.clone(), PathBuf::from("/usr/bin")]).unwrap();
    let root = temp.path().join("engine");
    let mut child = engine(&[], Some(&root), &path);
    assert_eq!(ready(&mut child).trim(), "ready");
    assert_eq!(check(&root), ready_version());
    let stderr = stopped_stderr(Engine(child));
    let codex = std::fs::canonicalize(&codex).unwrap();
    assert!(
        stderr.contains(&format!(
            "codex at {} could not be registered (native_auth_host_executable_unsafe)",
            codex.display()
        )),
        "{stderr}"
    );
    let explicit = crazyagent::spawn_gate::output(
        Command::new(BIN)
            .arg("engine")
            .arg("--home")
            .arg(temp.path().join("explicit"))
            .arg("--codex-executable")
            .arg(&codex)
            .env("PATH", BARE_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(!explicit.status.success());
    assert!(
        String::from_utf8_lossy(&explicit.stderr).contains("native_auth_host_executable_unsafe")
    );
}

fn unused_service(temp: &Path, root: &Path) -> Service {
    Service {
        platform: Platform::CURRENT,
        name: format!(
            "dev.crazyagent.test.unused.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
        executable: PathBuf::from(BIN),
        root: root.to_path_buf(),
        path: OsString::from(BARE_PATH),
        definition: temp.join("not-installed.plist"),
        user: None,
    }
}

#[test]
fn tui_auto_start_runs_a_detached_engine_and_waits_until_it_answers() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let layout = Layout::new(temp.path().join("engine")).unwrap();
    let service = unused_service(temp.path(), layout.root());
    let runtime = runtime();
    let started = runtime
        .block_on(crazyagent::service::ensure_running(
            &layout,
            &service,
            Path::new(BIN),
            Some(OsStr::new(BARE_PATH)),
            READY_TIMEOUT,
        ))
        .unwrap();
    let Started::Detached { pid } = started else {
        panic!("expected a detached engine, got {started:?}");
    };
    let group = Group(pid);
    assert!(crazyagent::service::answering(&layout.socket()));
    assert_eq!(check(layout.root()), ready_version());
    let again = runtime
        .block_on(crazyagent::service::ensure_running(
            &layout,
            &service,
            Path::new(BIN),
            Some(OsStr::new(BARE_PATH)),
            READY_TIMEOUT,
        ))
        .unwrap();
    assert_eq!(again, Started::AlreadyRunning);
    assert!(
        std::fs::read_to_string(layout.engine_log())
            .unwrap()
            .contains("ready")
    );
    assert_eq!(mode(&layout.engine_log()), 0o600);
    drop(group);
    wait_silent(&layout.socket());
}

#[test]
fn an_engine_that_stops_while_starting_names_its_log() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let layout = Layout::new(temp.path().join("engine")).unwrap();
    let service = unused_service(temp.path(), layout.root());
    let started = Instant::now();
    let error = runtime()
        .block_on(crazyagent::service::ensure_running(
            &layout,
            &service,
            Path::new("/usr/bin/false"),
            Some(OsStr::new(BARE_PATH)),
            READY_TIMEOUT,
        ))
        .unwrap_err();
    assert!(
        error.0.contains("the engine stopped while starting"),
        "{error}"
    );
    assert!(
        error.0.contains(&layout.engine_log().display().to_string()),
        "{error}"
    );
    assert!(started.elapsed() < READY_TIMEOUT);
}

#[cfg(target_os = "macos")]
struct Installed(Service);

#[cfg(target_os = "macos")]
impl Drop for Installed {
    fn drop(&mut self) {
        let uid = rustix::process::getuid().as_raw();
        let _ = crazyagent::spawn_gate::output(
            Command::new("/bin/launchctl")
                .arg("bootout")
                .arg(format!("gui/{uid}/{}", self.0.name))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        let _ = std::fs::remove_file(&self.0.definition);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn launchd_round_trip_runs_stops_starts_and_removes_a_uniquely_labelled_engine() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let layout = Layout::new(temp.path().join("engine")).unwrap();
    let label = format!(
        "dev.crazyagent.test.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let service = Service {
        platform: Platform::MacOs,
        name: label.clone(),
        executable: std::fs::canonicalize(BIN).unwrap(),
        root: layout.root().to_path_buf(),
        path: OsString::from(BARE_PATH),
        definition: temp.path().join(format!("{label}.plist")),
        user: None,
    };
    let installed = Installed(service.clone());
    let runtime = runtime();
    let wait = |phase: &str, since: u64| {
        runtime
            .block_on(crazyagent::service::wait_ready(
                &layout,
                READY_TIMEOUT,
                None,
                since,
            ))
            .unwrap_or_else(|error| {
                panic!(
                    "{phase}: {error}\nservice: {:?}\nlog:\n{}",
                    service.status(),
                    std::fs::read_to_string(layout.engine_log()).unwrap_or_default()
                )
            })
    };
    let since = crazyagent::service::log_length(&layout);
    service.install().unwrap();
    assert_eq!(
        std::fs::read_to_string(&service.definition).unwrap(),
        service.definition_text().unwrap()
    );
    wait("install", since);
    let status = service.status().unwrap();
    assert!(status.installed && status.running, "{status:?}");
    assert!(status.pid.is_some_and(|pid| pid > 1), "{status:?}");
    assert_eq!(check(layout.root()), ready_version());
    service.stop().unwrap();
    let status = service.status().unwrap();
    assert!(status.installed && !status.running, "{status:?}");
    wait_silent(&layout.socket());
    let since = crazyagent::service::log_length(&layout);
    assert!(since > 0);
    service.start().unwrap();
    wait("restart", since);
    assert!(service.status().unwrap().running);
    assert_eq!(check(layout.root()), ready_version());
    service.uninstall().unwrap();
    let status = service.status().unwrap();
    assert!(!status.installed && !status.running, "{status:?}");
    assert!(!service.definition.exists());
    wait_silent(&layout.socket());
    assert!(
        std::fs::read_to_string(layout.engine_log())
            .unwrap()
            .contains("ready")
    );
    drop(installed);
}

#[cfg(target_os = "macos")]
#[test]
fn service_status_for_another_data_root_names_its_own_label_not_the_default() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let root = temp.path().join("engine");
    let output = crazyagent::spawn_gate::output(
        Command::new(BIN)
            .args(["service", "status"])
            .env_clear()
            .env("HOME", &home)
            .env("CRAZYAGENT_HOME", &root)
            .env("PATH", BARE_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let label = stdout
        .lines()
        .find_map(|line| line.strip_prefix("service: "))
        .unwrap_or_else(|| panic!("{stdout}"));
    let hash = label
        .strip_prefix("dev.crazyagent.engine.")
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(hash.len(), 12, "{stdout}");
    assert!(
        stdout.contains(&format!(
            "definition: {} (not installed)",
            home.join("Library/LaunchAgents")
                .join(format!("{label}.plist"))
                .display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("running: no"), "{stdout}");
}
