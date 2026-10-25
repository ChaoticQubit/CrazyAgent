#![cfg(target_os = "macos")]

use crazyagent::harnesses::interactive::KEY_FILE;
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crazyagent::harnesses::terminal_session::TerminalSession;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEY: &str = "sk-stand-in-5521";
const STAND_IN: &str = "#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const SLOW_STAND_IN: &str = "#!/bin/sh\nprintf 'stand-in starting\\n'\n/bin/sleep 3\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn typed_until(file: &Path, wanted: &[u8]) -> Vec<u8> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let seen = std::fs::read(file).unwrap_or_default();
        if seen.windows(wanted.len()).any(|window| window == wanted) || Instant::now() >= until {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_shell_stand_in_in_a_real_tmux_pane_gets_the_prompt_as_one_paste_and_stops_with_its_key_file() {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, STAND_IN).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root_path.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable,
        working_dir: work,
        account: AccountBinding::ApiKey {
            home: home.clone(),
            key: Secret::new(KEY.into()).unwrap(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new("stand-in-1").unwrap();
    let mut session = TerminalSession::start(host.clone(), agent.clone(), request, None).unwrap();
    assert!(session.process_group() > 1);
    assert!(host.alive().unwrap().contains(&agent));
    match session.next_event(Duration::from_secs(1)).unwrap() {
        Some(LaunchEvent::Ready {
            native_session: Some(id),
        }) => assert_eq!(id.len(), 36),
        other => panic!("expected Ready with the session id, got {other:?}"),
    }
    let key_file = home.join(KEY_FILE);
    let placeholder = std::fs::read_to_string(&key_file).unwrap();
    assert!(
        crazyagent::model_gateway::relay::is_placeholder(&placeholder),
        "the pane's key file holds the relay token, never the key"
    );
    assert!(!placeholder.contains(KEY));
    assert_eq!(
        std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    session.send("hello\x1b world\nsecond line").unwrap();
    let pasted = b"\x1b[200~hello world\nsecond line\x1b[201~\r";
    let seen = typed_until(&home.join("typed"), pasted);
    assert!(
        seen.windows(pasted.len()).any(|window| window == pasted),
        "{seen:?}"
    );
    assert_eq!(session.send("again").unwrap_err().0, "launch_busy");
    session.interrupt().unwrap();
    let seen = typed_until(&home.join("typed"), b"\r\x1b");
    assert!(seen.ends_with(b"\r\x1b"), "{seen:?}");
    assert_eq!(
        session.next_event(Duration::from_millis(300)).unwrap(),
        None
    );

    assert_eq!(session.stop(), None);
    assert!(!host.alive().unwrap().contains(&agent));
    assert!(!key_file.exists());
    let config = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(config.contains("hasCompletedOnboarding"));
}

#[test]
fn the_first_prompt_waits_until_the_app_reads_keys_itself() {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, SLOW_STAND_IN).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root_path.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable,
        working_dir: work,
        account: AccountBinding::ApiKey {
            home: home.clone(),
            key: Secret::new(KEY.into()).unwrap(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new("stand-in-2").unwrap();
    let mut session = TerminalSession::start(host.clone(), agent, request, None).unwrap();
    let started = Instant::now();
    session.send("first prompt").unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the prompt goes in once the app reads keys, not after the whole ready wait"
    );
    let pasted = b"\x1b[200~first prompt\x1b[201~\r";
    let seen = typed_until(&home.join("typed"), pasted);
    assert!(
        seen.ends_with(pasted),
        "a prompt typed before the app reads keys loses its Enter: {seen:?}"
    );
    assert_eq!(session.stop(), None);
}
