use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{AccountSecret, OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest, Session};
use crazyagent::launch_context;
use crazyagent::plugins::{Scope, ServerSpec};
use crazyagent::project_setup::ProjectSetup;
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const FIXTURE: &str = include_str!("fixtures/mcp_fixture.py");
const FIXTURE_SECRET: &str = "dummy-value";
const CODENAME: &str = "BLUE-HERON-77";
const REQUEST_BYTES_MAX: usize = 8 * 1024 * 1024;

type Bodies = Arc<Mutex<Vec<String>>>;

fn private_directory(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

fn applied(response: OwnerResponse) {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("owner mutation must commit: {other:?}"),
    }
}

fn installed(name: &str) -> PathBuf {
    fs::canonicalize(Path::new("/opt/homebrew/bin").join(name))
        .unwrap_or_else(|_| panic!("BLOCKED: {name} is not installed in /opt/homebrew/bin"))
}

fn record(stream: TcpStream, bodies: Bodies) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length.min(REQUEST_BYTES_MAX)];
    if reader.read_exact(&mut body).is_ok() {
        bodies
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&body).into_owned());
    }
    let mut stream = stream;
    let _ = stream.write_all(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}

fn capture() -> (u16, Bodies) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));
    let sink = bodies.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = sink.clone();
            std::thread::spawn(move || record(stream, sink));
        }
    });
    (port, bodies)
}

fn first_request(session: &mut Session, bodies: &Bodies) -> String {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(body) = bodies
            .lock()
            .unwrap()
            .iter()
            .find(|body| body.contains(CODENAME))
        {
            return body.clone();
        }
        if session.next_event(Duration::from_millis(500)).is_err() {
            break;
        }
    }
    let seen = bodies.lock().unwrap().len();
    panic!("no model request carried the project context ({seen} requests seen)");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_codex_and_claude_receive_the_approved_skill_server_and_context() {
    let root = tempfile::Builder::new()
        .prefix("ca-lctx-live-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let base = private_directory(root.path());
    let global = base.join("global.toml");
    fs::write(&global, SETTINGS).unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let (stop, stopped) = watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        &base,
        "e.sock",
        &global,
        &base.join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let mut owner = OwnerController::attach(&base, OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();
    let home_root = tempfile::Builder::new()
        .prefix("ca-lctx-live-home-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let home = home_root.path().canonicalize().unwrap();
    applied(
        owner
            .set_projects_home(home.to_str().unwrap().into(), 1)
            .await
            .unwrap(),
    );
    let project = ProjectId::new("live").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Live check".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: format!("The project codename is {CODENAME}."),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap(),
    );
    let picked = private_directory(&base.join("picked/meal-tips"));
    fs::write(
        picked.join("SKILL.md"),
        "---\nname: meal-tips\ndescription: Gives meal planning tips.\n---\n\nSay tips.\n",
    )
    .unwrap();
    owner
        .add_skill(Scope::Global, picked.display().to_string())
        .await
        .unwrap();
    let fixture = base.join("mcp_fixture.py");
    fs::write(&fixture, FIXTURE).unwrap();
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/python3".into(),
        args: vec![fixture.display().to_string()],
    };
    let approved = owner
        .add_mcp_server(
            Scope::Global,
            "approved-tools".into(),
            spec.clone(),
            vec![(
                "FIXTURE_TOKEN".into(),
                AccountSecret::new(FIXTURE_SECRET.as_bytes().to_vec()).unwrap(),
            )],
        )
        .await
        .unwrap();
    owner
        .approve_mcp_server(approved.id, spec.clone())
        .await
        .unwrap();
    owner
        .add_mcp_server(Scope::Global, "waiting-tools".into(), spec, vec![])
        .await
        .unwrap();

    let profile = base.join("agent.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let work = private_directory(&base.join("work"));
    assert!(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .arg(&work)
            .status()
            .unwrap()
            .success()
    );
    for (harness, binary, suffix) in [("codex", "codex", "/v1"), ("claude-code", "claude", "")] {
        let connection = rusqlite::Connection::open_with_flags(
            base.join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let context = launch_context::for_project(&connection, &project).unwrap();
        drop(connection);
        let (port, bodies) = capture();
        let home = private_directory(&base.join(format!("home-{harness}")));
        let mut session = Session::start_with_context(
            LaunchRequest {
                harness: harness.into(),
                executable: installed(binary),
                working_dir: work.clone(),
                account: AccountBinding::Url {
                    home: home.clone(),
                    base_url: format!("http://127.0.0.1:{port}{suffix}"),
                    key: None,
                },
                model: Some("qwen2.5:0.5b".into()),
                tools: true,
                sandbox_profile: Some(profile.clone()),
                limits: LaunchLimits {
                    run_time: Duration::from_secs(180),
                    ..LaunchLimits::default()
                },
            },
            context,
        )
        .unwrap();
        session
            .send("What is the project codename? Answer with the codename only.")
            .unwrap();
        let body = first_request(&mut session, &bodies);
        session.stop();

        assert!(
            body.contains("meal-tips"),
            "{harness}: the approved skill is offered"
        );
        let tool = if harness == "codex" {
            ["\"name\":\"authorized\"", "\"name\": \"authorized\""]
        } else {
            [
                "\"name\":\"mcp__approved-tools__authorized\"",
                "\"name\": \"mcp__approved-tools__authorized\"",
            ]
        };
        assert!(
            tool.iter().any(|name| body.contains(name)),
            "{harness}: the approved server started with its secret and its tools are offered"
        );
        assert!(
            !body.contains("waiting-tools"),
            "{harness}: a waiting server never starts"
        );
        assert!(
            !body.contains(FIXTURE_SECRET),
            "{harness}: the secret never reaches the model"
        );
        assert!(!home.join("config.toml").exists());
        assert!(!home.join("crazyagent").exists());
    }

    drop(owner);
    service.shutdown().await.unwrap();
    drop(stop);
}
