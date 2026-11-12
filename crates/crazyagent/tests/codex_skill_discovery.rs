#![cfg(target_os = "macos")]

use crazyagent::adapters::codex::{CodexProtocol, Event};
use crazyagent::agent_git::Layout;
use crazyagent::client_controller::OwnerController;
use crazyagent::coord::delivery::codex;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchLimits, LaunchRequest, command_with_context,
};
use crazyagent::launch_context;
use crazyagent::plugins::Scope;
use crazyagent::project_setup::ProjectSetup;
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const SKILLS: [&str; 2] = ["meal-tips", "release-notes"];
const LIST_ID: &str = "skills-list";
const REPLY_TIME_MAX: Duration = Duration::from_secs(30);

/// Finds the real Codex on PATH and checks it is a release the engine supports: one of
/// `codex::VERSIONS` or a later minor the range rule lets run with a warning (F38).
/// Input: nothing. Output: the canonical executable; panics UNAVAILABLE when Codex is
/// absent or its release is one the bridge refuses.
fn installed_codex() -> PathBuf {
    let executable = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("codex"))
        .find(|candidate| candidate.is_file())
        .map(|candidate| fs::canonicalize(candidate).unwrap())
        .expect("UNAVAILABLE: no codex executable on PATH");
    let output = crazyagent::spawn_gate::output(
        Command::new(&executable)
            .arg("--version")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let release = version
        .trim()
        .strip_prefix("codex-cli ")
        .and_then(|text| {
            let mut parts = text.split(['.', '-']).map(str::parse::<u64>);
            Some((parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?))
        });
    assert!(
        release.is_some_and(|release| codex::support(release) != codex::Support::Refused),
        "UNAVAILABLE: {version:?} is a Codex release the bridge refuses"
    );
    executable
}

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

fn skill_folder(root: &Path, name: &str) -> String {
    let path = root.join("picked").join(name);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Teaches {name}.\n---\n\nSteps for {name}.\n"),
    )
    .unwrap();
    path.display().to_string()
}

fn closed_port() -> u16 {
    let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn send(input: &mut ChildStdin, frame: &[u8]) {
    input.write_all(frame).unwrap();
    input.flush().unwrap();
}

fn next_line(lines: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    lines
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect("codex app-server replied in time")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_supported_codex_discovers_every_skill_the_engine_copies_into_the_agent_home() {
    let root = tempfile::Builder::new()
        .prefix("ca-codex-skills-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
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
    let projects_root = tempfile::Builder::new()
        .prefix("ca-codex-skills-home-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let projects = projects_root.path().canonicalize().unwrap();
    applied(
        owner
            .set_projects_home(projects.to_str().unwrap().into(), 1)
            .await
            .unwrap(),
    );
    let project = ProjectId::new("skills").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Skill discovery".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Checks which copied skills Codex finds.".into(),
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
    for name in SKILLS {
        owner
            .add_skill(Scope::Global, skill_folder(&base, name))
            .await
            .unwrap();
    }
    let connection = rusqlite::Connection::open_with_flags(
        base.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let context = launch_context::for_project(&connection, &project, Layout::Linked).unwrap();
    drop(connection);

    let profile = base.join("agent.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let work = private_directory(&base.join("work"));
    let home = private_directory(&base.join("codex-home"));
    let (mut command, _installed) = command_with_context(
        &LaunchRequest {
            harness: "codex".into(),
            executable: installed_codex(),
            working_dir: work.clone(),
            account: AccountBinding::Url {
                home: home.clone(),
                base_url: format!("http://127.0.0.1:{}/v1", closed_port()),
                key: None,
            },
            model: Some("qwen2.5:0.5b".into()),
            tools: true,
            sandbox_profile: Some(profile),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        },
        &context,
    )
    .unwrap();
    assert!(
        command.get_args().any(|argument| argument == "app-server"),
        "the engine launches Codex as its app server"
    );
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(output);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + REPLY_TIME_MAX;
    let mut protocol = CodexProtocol::new();
    send(&mut input, &protocol.initialize().unwrap());
    while !protocol
        .receive(&next_line(&lines, deadline))
        .expect("real codex frame decodes")
        .iter()
        .any(|event| matches!(event, Event::Initialized))
    {}
    send(&mut input, &protocol.initialized().unwrap());
    let mut request = serde_json::to_vec(&serde_json::json!({
        "id": LIST_ID,
        "method": "skills/list",
        "params": { "cwds": [work], "forceReload": true },
    }))
    .unwrap();
    request.push(b'\n');
    send(&mut input, &request);
    let reply = loop {
        let frame: serde_json::Value =
            serde_json::from_slice(&next_line(&lines, deadline)).unwrap();
        if frame["id"] == LIST_ID {
            break frame;
        }
    };
    let _ = child.kill();
    let _ = child.wait();

    let copies = home.join("skills").join("crazyagent");
    let entries = reply["result"]["data"]
        .as_array()
        .unwrap_or_else(|| panic!("skills/list answered without data: {reply}"));
    let found: BTreeSet<&str> = entries
        .iter()
        .flat_map(|entry| entry["skills"].as_array().into_iter().flatten())
        .filter(|skill| {
            skill["enabled"] == true
                && skill["path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).starts_with(&copies))
        })
        .filter_map(|skill| skill["name"].as_str())
        .collect();
    assert_eq!(
        found,
        BTreeSet::from(SKILLS),
        "Codex lists every skill the engine copied under {}, enabled: {reply}",
        copies.display()
    );

    drop(owner);
    service.shutdown().await.unwrap();
    drop(stop);
}
