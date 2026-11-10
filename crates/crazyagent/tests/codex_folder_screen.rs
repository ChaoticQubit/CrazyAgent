#![cfg(target_os = "macos")]

use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Session,
};
use crazyagent::harnesses::terminal_session::{Resume, TerminalSession};
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REPLY: &str = "STANDINREPLY";
const TURN_WAIT: Duration = Duration::from_secs(45);
const MARKER_WAIT: Duration = Duration::from_secs(10);
const MARKER_GRACE: Duration = Duration::from_secs(2);
const REQUEST_BYTES_MAX: usize = 64 << 20;
const CANDIDATES: [&str; 2] = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"];
const GIT: &str = "/Applications/Xcode.app/Contents/Developer/usr/bin/git";

/// One test's tmux server, ended when the test ends.
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

/// A dummy Codex home and a git repository under a private folder, a tmux host, a loopback
/// stand-in for the model and the real Codex wrapped so it can reach only that stand-in.
struct World {
    _root: tempfile::TempDir,
    _server: Server,
    base: PathBuf,
    host: Arc<dyn Host>,
    home: PathBuf,
    repo: PathBuf,
    codex: PathBuf,
    model: String,
}

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Answers one model request: a reply and a finished response for a POST, nothing for the
/// rest. Input: the connection. Output: none.
fn answer(mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0_u8; 65536];
    let (head_end, length) = loop {
        let Ok(count) = stream.read(&mut chunk) else {
            return;
        };
        if count == 0 {
            return;
        }
        seen.extend_from_slice(&chunk[..count]);
        assert!(seen.len() <= REQUEST_BYTES_MAX);
        let Some(end) = seen.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        break (end + 4, length);
    };
    while seen.len() < head_end + length {
        let Ok(count) = stream.read(&mut chunk) else {
            return;
        };
        if count == 0 {
            return;
        }
        seen.extend_from_slice(&chunk[..count]);
        assert!(seen.len() <= REQUEST_BYTES_MAX);
    }
    let reply = if seen.starts_with(b"POST") {
        let events = [
            serde_json::json!({"type": "response.created", "response": {"id": "r1"}}),
            serde_json::json!({"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "id": "m1", "content": [{"type": "output_text", "text": REPLY}]}}),
            serde_json::json!({"type": "response.completed", "response": {"id": "r1", "usage": {"input_tokens": 1, "input_tokens_details": null, "output_tokens": 1, "output_tokens_details": null, "total_tokens": 2}}}),
        ];
        let body: String = events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect();
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    } else {
        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
    };
    let _ = stream.write_all(reply.as_bytes());
}

fn model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Builds a world, or none when this machine has no real Codex or tmux.
fn world(codex_folder: bool) -> Option<World> {
    let real = CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.exists())
        .and_then(|path| std::fs::canonicalize(path).ok())?;
    let root = tempfile::Builder::new()
        .prefix("ca-cf-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    private(&base);
    let tmux = installed(&base)?;
    let folder = base.join(FOLDER);
    let server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let (home, repo, bin) = (base.join("home"), base.join("repo"), base.join("bin"));
    for path in [&home, &repo, &bin] {
        private(path);
    }
    std::fs::write(home.join("config.toml"), "project_doc_max_bytes = 0\n").unwrap();
    std::fs::write(
        home.join("auth.json"),
        "{\"auth_mode\":\"apikey\",\"OPENAI_API_KEY\":\"dummy-offline\"}",
    )
    .unwrap();
    std::fs::set_permissions(
        home.join("auth.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let done = crazyagent::spawn_gate::output(
        Command::new(GIT)
            .args(["init", "-q"])
            .current_dir(&repo)
            .env_clear()
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(done.status.success(), "{done:?}");
    if codex_folder {
        private(&repo.join(".codex"));
    }
    let owner_home = std::env::var("HOME").unwrap();
    let profile = format!(
        "(version 1)(allow default)(deny network-outbound (remote ip))(allow network-outbound (remote ip \"localhost:*\"))(deny file-read* (subpath \"{owner_home}/.codex\"))(deny file-write* (subpath \"{owner_home}/.codex\"))"
    );
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\nexec /usr/bin/sandbox-exec -p '{profile}' '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    Some(World {
        _root: root,
        _server: server,
        base,
        host,
        home,
        repo,
        codex,
        model: model(),
    })
}

fn request(world: &World, cwd: &Path, tools: bool) -> LaunchRequest {
    LaunchRequest {
        harness: "codex".into(),
        executable: world.codex.clone(),
        working_dir: cwd.to_path_buf(),
        account: AccountBinding::Url {
            home: world.home.clone(),
            base_url: world.model.clone(),
            key: None,
        },
        model: Some("stand-in-model".into()),
        tools,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    }
}

fn terminal(
    world: &World,
    name: &str,
    cwd: &Path,
    tools: bool,
    resume: Option<&Resume>,
) -> TerminalSession {
    TerminalSession::start_resuming(
        world.host.clone(),
        AgentId::new(name).unwrap(),
        request(world, cwd, tools),
        None,
        resume,
    )
    .unwrap()
}

/// Sends a prompt into a pane and reads the engine's own events until the turn ends.
/// Inputs: the session and the prompt. Output: the reply text.
fn turn(session: &mut TerminalSession, prompt: &str) -> String {
    session.send(prompt).unwrap();
    let until = Instant::now() + TURN_WAIT;
    let mut said = String::new();
    while Instant::now() < until {
        match session.next_event(Duration::from_millis(250)).unwrap() {
            Some(LaunchEvent::Text { text }) => said.push_str(&text),
            Some(LaunchEvent::TurnFinished { success }) => {
                assert!(success, "{prompt}: {said}");
                return said;
            }
            _ => {}
        }
    }
    panic!("no end of turn for {prompt}: {said:?}");
}

fn history_holds(world: &World, prompt: &str) -> bool {
    std::fs::read_to_string(world.home.join("history.jsonl"))
        .is_ok_and(|text| text.contains(prompt))
}

/// Plants a repository MCP server whose start creates `marker`, and saves a trusted line
/// for the repository in the dummy home, as an earlier launch may have left behind.
fn plant(world: &World, marker: &Path) {
    private(&world.repo.join(".codex"));
    std::fs::write(
        world.repo.join(".codex/config.toml"),
        format!(
            "[mcp_servers.dummy]\ncommand = \"/bin/sh\"\nargs = [\"-c\", \"/usr/bin/touch {}; /bin/sleep 20\"]\n",
            marker.display()
        ),
    )
    .unwrap();
    let saved = format!(
        "project_doc_max_bytes = 0\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
        world.repo.display()
    );
    std::fs::write(world.home.join("config.toml"), saved).unwrap();
}

/// Shows the plant is live: a plain `codex exec` with the saved trusted line and no entry
/// runs the repository's server. Inputs: the world, the launch folder and the marker.
/// Output: none; the marker is gone again afterwards.
fn control_runs_the_plant(world: &World, cwd: &Path, marker: &Path) {
    let provider = format!(
        "model_providers.standin={{name=\"standin\",base_url=\"{}\",wire_api=\"responses\"}}",
        world.model
    );
    let done = crazyagent::spawn_gate::output(
        Command::new(&world.codex)
            .args([
                "exec",
                "--skip-git-repo-check",
                "--color",
                "never",
                "-s",
                "read-only",
            ])
            .arg("-C")
            .arg(cwd)
            .args(["-c", "model_provider=\"standin\"", "-c", &provider, "hello"])
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &world.home)
            .env("CODEX_HOME", &world.home)
            .env("TMPDIR", &world.home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(done.status.success(), "{done:?}");
    let until = Instant::now() + MARKER_WAIT;
    while !marker.exists() {
        assert!(
            Instant::now() < until,
            "inconclusive: the plant did not run"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::remove_file(marker).unwrap();
}

macro_rules! real_world {
    ($codex_folder:expr) => {
        match world($codex_folder) {
            Some(world) => world,
            None => {
                println!("UNAVAILABLE: no real Codex or tmux on this machine");
                return;
            }
        }
    };
}

#[test]
fn the_first_prompt_reaches_codex_with_a_codex_folder_on_both_shapes() {
    for tools in [false, true] {
        let world = real_world!(true);
        let prompt = format!("first prompt with folder tools {tools}");
        let mut session = terminal(&world, "cf-a", &world.repo, tools, None);
        let said = turn(&mut session, &prompt);
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        assert!(history_holds(&world, &prompt), "{tools}");
        assert!(session.stop().is_ok());
    }
}

#[test]
fn the_first_prompt_reaches_codex_with_no_codex_folder_on_both_shapes() {
    for tools in [false, true] {
        let world = real_world!(false);
        let prompt = format!("first prompt without folder tools {tools}");
        let mut session = terminal(&world, "cf-b", &world.repo, tools, None);
        let said = turn(&mut session, &prompt);
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        assert!(history_holds(&world, &prompt), "{tools}");
        assert!(session.stop().is_ok());
    }
}

#[test]
fn a_resumed_launch_with_a_codex_folder_gets_its_prompt_on_both_shapes() {
    for tools in [false, true] {
        let world = real_world!(true);
        let mut first = terminal(&world, "cf-c", &world.repo, tools, None);
        let said = turn(&mut first, "first turn before the resume");
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        let resume = first
            .conversation()
            .expect("the first turn named its record");
        assert!(first.stop().is_ok());
        let prompt = format!("prompt after the resume tools {tools}");
        let mut again = terminal(&world, "cf-d", &world.repo, tools, Some(&resume));
        let said = turn(&mut again, &prompt);
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        assert!(history_holds(&world, &prompt), "{tools}");
        assert!(again.stop().is_ok());
    }
}

#[test]
fn a_saved_trusted_line_plus_the_entry_starts_nothing_from_a_subfolder_terminal() {
    for tools in [false, true] {
        let world = real_world!(true);
        let marker = world.base.join("marker");
        plant(&world, &marker);
        let sub = world.repo.join("sub");
        private(&sub);
        control_runs_the_plant(&world, &sub, &marker);
        let mut session = terminal(&world, "cf-e", &sub, tools, None);
        let said = turn(&mut session, "does the repository command run");
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        std::thread::sleep(MARKER_GRACE);
        assert!(!marker.exists(), "{tools}: the repository's command ran");
        assert!(session.stop().is_ok());
    }
}

#[test]
fn an_app_server_turn_runs_with_the_entry_and_trusts_and_starts_nothing() {
    for tools in [false, true] {
        let world = real_world!(true);
        let marker = world.base.join("marker");
        plant(&world, &marker);
        let sub = world.repo.join("sub");
        private(&sub);
        control_runs_the_plant(&world, &sub, &marker);
        let before = std::fs::read_to_string(world.home.join("config.toml")).unwrap();
        let mut session = Session::start(request(&world, &sub, tools)).unwrap();
        session.send("say the reply").unwrap();
        let until = Instant::now() + TURN_WAIT;
        let mut said = String::new();
        let mut finished = false;
        while Instant::now() < until && !finished {
            match session.next_event(Duration::from_millis(250)).unwrap() {
                Some(LaunchEvent::Text { text }) => said.push_str(&text),
                Some(LaunchEvent::TurnFinished { success }) => {
                    assert!(success, "{tools}: {said}");
                    finished = true;
                }
                _ => {}
            }
        }
        assert!(finished, "{tools}: no end of turn, got {said:?}");
        assert!(said.contains(REPLY), "{tools}: {said:?}");
        std::thread::sleep(MARKER_GRACE);
        assert!(!marker.exists(), "{tools}: the repository's command ran");
        assert_eq!(
            std::fs::read_to_string(world.home.join("config.toml")).unwrap(),
            before,
            "{tools}: the app server saved trust by itself"
        );
        assert!(session.stop().is_ok());
    }
}
