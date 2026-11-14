#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide;
use crazyagent::ide::fetch::BuildState;
use crazyagent::ide::terminal::SANDBOXED;
use crazyagent::protocol::{FRAME_BYTES_MAX, ProjectId};
use crazyagent::sandbox::ide_shell;
use crazyagent::terminal::TerminalRequest;
use e2e_support::{Engine, Owner, result, scratch};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const PROJECT: &str = "v2-terminal";
const BUILD: &str = "vscodium-1.143.17183";
const SERVER_DATA: &str = "server-data";

/// What the engine's own IDE start left for one project: the locked Remote settings it
/// wrote, the server data folder they were written for and the Seatbelt profile the
/// server ran under.
struct Started {
    settings: serde_json::Value,
    server_data: PathBuf,
    profile: PathBuf,
}

/// The running engine with one project set up through the owner protocol, a file in its
/// folder and a loopback listener the shells try to reach.
struct Terminal {
    engine: Engine,
    owner: Owner,
    project: ProjectId,
    folder: PathBuf,
    listener: TcpListener,
    disk: tempfile::TempDir,
}

/// Records the pinned IDE build kept in a `.local-state/ide-cache` above the crate as the
/// engine's ready build, the state a finished fetch records, and reads it back through the
/// owner protocol, so the owner's IDE open starts a real server from it. Inputs: the
/// engine and its owner. Output: none.
async fn ready_build(engine: &Engine, owner: &Owner) {
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|folder| folder.join(".local-state").join("ide-cache").join(BUILD))
        .find(|build| build.join("out").join("server-main.js").is_file())
        .expect("no pinned VSCodium reh-web build in a .local-state/ide-cache above the crate");
    let ready = BuildState::Ready {
        version: BUILD.trim_start_matches("vscodium-").to_owned(),
        root: fs::canonicalize(build).unwrap(),
    };
    let ide = engine.path().join("ide");
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&ide)
        .unwrap();
    fs::write(
        ide.join("build-state.json"),
        serde_json::to_vec(&ready).unwrap(),
    )
    .unwrap();
    let client = owner.controller.owner_client().unwrap();
    assert_eq!(client.ide_tab_state(false).await.unwrap().build, ready);
}

impl Terminal {
    /// Starts the engine, picks a projects home and creates the project. Input: a scratch
    /// prefix. Output: the case.
    async fn start(prefix: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-term-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(PROJECT, "Buoy terminal").await,
            OwnerResult::Applied
        );
        let project = ProjectId::new(PROJECT).unwrap();
        let folder = ide::workspace::folder(engine.path(), &project).unwrap();
        fs::write(folder.join("README.md"), "buoys\n").unwrap();
        let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        ready_build(&engine, &owner).await;
        Self {
            engine,
            owner,
            project,
            folder,
            listener,
            disk,
        }
    }

    /// The probe each terminal runs: whether it reads the engine's own database, whether
    /// it reads the project's file and whether it reaches the loopback listener, with the
    /// connect's own words. Output: the shell script.
    fn probe(&self) -> String {
        let database = self.engine.path().join("engine.db");
        let port = self.listener.local_addr().unwrap().port();
        format!(
            "if /bin/cat '{}' >/dev/null 2>&1; then echo READ-db; else echo DENIED-db; fi\n\
             if /bin/cat README.md >/dev/null 2>&1; then echo READ-project; else echo DENIED-project; fi\n\
             if /usr/bin/nc -z -v -G 2 127.0.0.1 {port} 2>&1; then echo NET-OPEN; else echo NET-CLOSED; fi\n",
            database.display()
        )
    }

    /// The locked Remote settings the engine's last IDE start wrote for the project, read
    /// back from the file. Output: the settings.
    fn settings(&self) -> serde_json::Value {
        let server_data = self
            .engine
            .path()
            .join("ide")
            .join(PROJECT)
            .join(SERVER_DATA);
        let machine = ide::settings::machine_folder(&server_data).join("settings.json");
        serde_json::from_str(&fs::read_to_string(machine).unwrap()).unwrap()
    }

    /// Opens the project's IDE through the owner protocol on the running engine and stops
    /// it again, then reads back what that start wrote. Output: the start's files.
    async fn open_ide(&self) -> Started {
        let client = self.owner.controller.owner_client().unwrap();
        let opened = client.ide_tab_open(self.project.clone()).await.unwrap();
        assert!(opened.url.starts_with("http://127.0.0.1:"), "{opened:?}");
        assert!(client.ide_tab_stop(self.project.clone()).await.unwrap());
        let folder = self.engine.path().join("ide").join(PROJECT);
        let server_data = folder.join(SERVER_DATA);
        let settings = self.settings();
        let profile = folder.join(ide::sandbox::PROFILE);
        assert!(
            profile.is_file(),
            "the server ran under {}",
            profile.display()
        );
        Started {
            settings,
            server_data,
            profile,
        }
    }
}

/// Runs a command and returns its standard output; a failed start fails the test.
fn output(command: &mut Command) -> String {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(command).unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The engine's owner socket, `<root>/<generation>/owner.sock`, which the built-in extension
/// reaches the IDE bridge on. Input: the engine root. Output: the one socket there.
fn owner_socket(root: &Path) -> PathBuf {
    let found: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("owner.sock"))
        .filter(|socket| socket.exists())
        .collect();
    assert_eq!(found.len(), 1, "one owner generation: {found:?}");
    found.into_iter().next().unwrap()
}

/// The bridge handshake the built-in extension sends first: the magic, slot 0 and a token
/// written as 64 lowercase hex digits. Input: the token text. Output: the 37 bytes.
fn handshake(token: &str) -> Vec<u8> {
    assert_eq!(token.len(), 64);
    let mut packet = ide::bridge::MAGIC.to_vec();
    packet.push(0);
    for at in (0..token.len()).step_by(2) {
        packet.push(u8::from_str_radix(&token[at..at + 2], 16).unwrap());
    }
    assert_eq!(packet.len(), 37);
    packet
}

/// Connects to the bridge with a handshake and sends one length-prefixed request for the
/// project. Inputs: the socket, the token and the call. Output: the open connection.
async fn send(socket: &Path, token: &str, call: serde_json::Value) -> UnixStream {
    let mut stream = crazyagent::spawn_gate::connect_unix(socket).await.unwrap();
    stream.write_all(&handshake(token)).await.unwrap();
    let body =
        serde_json::to_vec(&serde_json::json!({ "project": PROJECT, "call": call })).unwrap();
    stream
        .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
    stream
}

/// Reads one length-prefixed frame. Input: the connection. Output: its JSON.
async fn frame(stream: &mut UnixStream) -> serde_json::Value {
    let length = stream.read_u32().await.unwrap() as usize;
    assert!(length > 0 && length <= FRAME_BYTES_MAX);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Sends one terminal control frame. Inputs: the open terminal and the request.
async fn control(stream: &mut UnixStream, request: &TerminalRequest) {
    let body = serde_json::to_vec(request).unwrap();
    stream
        .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
}

/// Reads terminal output until it holds `wanted`, or with none until anything not blank
/// came, within 30 seconds. Inputs: the open terminal and the text. Output: all read.
async fn output_until(stream: &mut UnixStream, wanted: Option<&str>) -> String {
    let mut seen = Vec::new();
    let read = async {
        loop {
            let frame = frame(stream).await;
            assert_eq!(frame["kind"], "output", "{frame}");
            for byte in frame["bytes"].as_array().unwrap() {
                seen.push(u8::try_from(byte.as_u64().unwrap()).unwrap());
            }
            let done = match wanted {
                Some(wanted) => String::from_utf8_lossy(&seen).contains(wanted),
                None => seen.iter().any(|byte| !byte.is_ascii_whitespace()),
            };
            if done {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .expect("the wanted terminal output never came");
    String::from_utf8_lossy(&seen).into_owned()
}

/// The line typed into the Sandboxed terminal. Each answer is printed through shell
/// arithmetic, so the echoed input never holds an answer: where the shell starts, whether
/// it reads the engine's database, the project's file, the read-only folder and a folder
/// not granted, whether it writes the read-only folder and whether it reaches the
/// loopback listener. Inputs: the database, the granted and the ungranted folder, and
/// the port. Output: the line with its Enter.
fn sandboxed_probe(database: &Path, granted: &Path, other: &Path, port: u16) -> String {
    let test = |name: &str, command: String| {
        format!(
            "if {command} >/dev/null 2>&1; then echo {name}-$((1)); else echo {name}-$((0)); fi; "
        )
    };
    let mut line = String::from("echo cwd-$((1))=$(pwd -P); ");
    line.push_str(&test("db", format!("/bin/cat '{}'", database.display())));
    line.push_str(&test("project", "/bin/cat README.md".to_owned()));
    line.push_str(&test(
        "granted",
        format!("/bin/cat '{}/manual.txt'", granted.display()),
    ));
    line.push_str(&test(
        "granted-write",
        format!("/usr/bin/touch '{}/written'", granted.display()),
    ));
    line.push_str(&test(
        "other",
        format!("/bin/cat '{}/notes.txt'", other.display()),
    ));
    line.push_str(&test(
        "net",
        format!("/usr/bin/nc -z -G 2 127.0.0.1 {port}"),
    ));
    line.push_str("echo probe-$((40+2))\r");
    line
}

/// TSO-N1: the Sandboxed terminal is the IDE's default, and the one the extension opens
/// through the engine's bridge (`ide.terminal`, the engine's own launch and pane) starts in
/// the project folder under the agent sandbox: the engine's database denied, the project
/// and the owner's read-only folder readable, that folder not writable, a folder not
/// granted unreadable, and no network.
#[tokio::test(flavor = "multi_thread")]
async fn ide_terminal_sandboxed_is_the_default_and_runs_the_owner_shell_in_the_agent_sandbox() {
    let mut case = Terminal::start("ca-v2opt-term-sbx-").await;
    let granted = case.disk.path().join("shared");
    let other = case.disk.path().join("private");
    for (folder, file) in [(&granted, "manual.txt"), (&other, "notes.txt")] {
        fs::create_dir(folder).unwrap();
        fs::write(folder.join(file), "buoy manual\n").unwrap();
    }
    let granted = fs::canonicalize(granted).unwrap();
    let other = fs::canonicalize(other).unwrap();
    let shown = case
        .owner
        .controller
        .project_read_paths(case.project.clone())
        .await
        .unwrap();
    let generation = case.owner.next();
    let saved = case
        .owner
        .controller
        .set_project_read_paths(
            case.project.clone(),
            shown.version,
            vec![granted.display().to_string()],
            generation,
        )
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    let place = ide_shell::project_place(case.engine.path(), &case.project).unwrap();
    assert_eq!(place.extra_reads, std::slice::from_ref(&granted));

    let client = case.owner.controller.owner_client().unwrap();
    let opened = client.ide_tab_open(case.project.clone()).await.unwrap();
    assert!(opened.url.starts_with("http://127.0.0.1:"), "{opened:?}");
    let settings = case.settings();
    assert_eq!(
        settings["terminal.integrated.defaultProfile.osx"],
        serde_json::json!(SANDBOXED),
        "a new IDE terminal opens Sandboxed"
    );

    let socket = owner_socket(case.engine.path());
    let capability = case
        .engine
        .path()
        .join("ide")
        .join(PROJECT)
        .join(ide::server::CAPABILITY);
    let issued = fs::read_to_string(&capability).unwrap();
    let mut session = send(
        &socket,
        &issued,
        serde_json::json!({ "name": "ide.session", "args": {} }),
    )
    .await;
    let swapped = frame(&mut session).await;
    assert_eq!(swapped["kind"], "ok", "{swapped}");
    let token = swapped["data"]["token"].as_str().unwrap().to_owned();
    assert_ne!(token, issued);
    let mut terminal = send(
        &socket,
        &token,
        serde_json::json!({ "name": "ide.terminal", "args": { "cols": 200, "rows": 40, "level": "sandboxed" } }),
    )
    .await;
    let started = frame(&mut terminal).await;
    assert_eq!(started["kind"], "ok", "{started}");
    assert!(
        started["data"]["terminal"]
            .as_str()
            .unwrap()
            .starts_with("ide-")
    );
    output_until(&mut terminal, None).await;
    let port = case.listener.local_addr().unwrap().port();
    let typed = sandboxed_probe(
        &case.engine.path().join("engine.db"),
        &granted,
        &other,
        port,
    );
    control(
        &mut terminal,
        &TerminalRequest::Input {
            bytes: typed.into_bytes(),
        },
    )
    .await;
    let seen = output_until(&mut terminal, Some("probe-42")).await;
    for answer in [
        format!("cwd-1={}", case.folder.display()),
        "db-0".to_owned(),
        "project-1".to_owned(),
        "granted-1".to_owned(),
        "granted-write-0".to_owned(),
        "other-0".to_owned(),
        "net-0".to_owned(),
    ] {
        assert!(seen.contains(&answer), "{answer} missing in: {seen}");
    }
    assert!(!granted.join("written").exists());
    assert!(client.ide_tab_stop(case.project.clone()).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_terminal_tasks_run_the_owner_shell_inside_the_ide_sandbox_with_no_network() {
    let case = Terminal::start("ca-v2opt-term-ide-").await;
    let started = case.open_ide().await;
    let shell = ide_shell::owner_shell().unwrap();
    let profile = &started.settings["terminal.integrated.automationProfile.osx"];
    assert_eq!(profile["path"], serde_json::json!(shell.to_str().unwrap()));
    assert!(
        started.settings["terminal.integrated.profiles.osx"]
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_null),
        "a project below Full access is offered no shell hosted in the IDE server"
    );
    let arguments: Vec<String> = profile["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(arguments, ["-l"]);

    let text = fs::read_to_string(&started.profile).unwrap();
    let bind = "(local unix-socket (subpath \"";
    let at = text
        .find(bind)
        .expect("the profile binds sockets in the server's own temp folder")
        + bind.len();
    let tmp = PathBuf::from(&text[at..at + text[at..].find('"').unwrap()]);
    assert!(tmp.is_dir(), "the IDE start made the server's short temp folder");
    let mut command = ide::sandbox::command(&started.profile, Path::new(&shell));
    command
        .args(&arguments)
        .arg("-c")
        .arg(case.probe())
        .current_dir(&case.folder)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &started.server_data)
        .env("TMPDIR", &tmp)
        .env("TERM", "xterm-256color");
    let seen = output(&mut command);
    assert!(seen.contains("DENIED-db\n"), "{seen}");
    assert!(seen.contains("READ-project\n"), "{seen}");
    assert!(
        seen.contains("NET-CLOSED\n") && seen.contains("not permitted"),
        "tasks run inside the IDE server's own sandbox, which has no network: {seen}"
    );
}
