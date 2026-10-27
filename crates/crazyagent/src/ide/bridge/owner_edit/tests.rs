use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::ide::workspace::tests::{Case, add_project};
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LISTEN: &str = r#"while IFS= read -r line; do
  printf '%s\n' "$line" >> "$1"
done
"#;

/// The engine's private tmux server under a test root, killed when the test ends.
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
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

fn now() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    u64::try_from(elapsed.as_millis()).unwrap()
}

fn team(agent: i64) -> AgentId {
    AgentId::team(agent).unwrap()
}

fn heard(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn edits(case: &Case) -> i64 {
    case.db
        .query_row("SELECT COUNT(*) FROM landing_owner_edits", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn owner_edit_sends_the_agent_one_message_naming_the_file() {
    let mut case = Case::new();
    let tmux = installed(&case.root).expect("tmux must be installed on this machine");
    let _server = Server {
        tmux: tmux.clone(),
        socket: case.root.join(FOLDER).join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &case.root.join(FOLDER)));
    let agent = case.agent("sub", "working");
    let listened = case.root.join("heard.txt");
    host.spawn(
        &team(agent.id),
        &InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                LISTEN.into(),
                "sh".into(),
                listened.to_str().unwrap().into(),
            ],
            cwd: agent.worktree.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        },
    )
    .unwrap();
    let told = edit(
        &case.root,
        Some(host.as_ref()),
        &case.project,
        &team(agent.id),
        "src/lib.rs",
        now(),
    )
    .unwrap();
    assert!(told);
    let deadline = Instant::now() + Duration::from_secs(20);
    while heard(&listened).is_empty() {
        assert!(Instant::now() < deadline, "the agent heard nothing");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));
    let lines = heard(&listened);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("src/lib.rs"), "{lines:?}");
    assert_eq!(edits(&case), 1);
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.file()).unwrap()).unwrap();
    let file = format!("{}/src/lib.rs", agent.worktree.display());
    assert_eq!(
        body["settings"]["files.readonlyExclude"],
        serde_json::json!({ file: true })
    );
    let everything = format!("{}/**", agent.worktree.display());
    assert_eq!(
        body["settings"]["files.readonlyInclude"],
        serde_json::json!({ everything: true })
    );
}

#[test]
fn owner_edit_is_refused_outside_a_running_agent_of_the_project() {
    let mut case = Case::new();
    let stopped = case.agent("sub", "working");
    case.status(&stopped, "stopped");
    let idle = case.agent("sub", "working");
    case.status(&idle, "idle");
    let working = case.agent("sub", "working");
    let master = case.agent("master", "working");
    let other = case.root.join("other");
    std::fs::create_dir(&other).unwrap();
    add_project(&case.db, "beta", &other);
    let beta = ProjectId::new("beta").unwrap();
    let refused = |project: &ProjectId, agent: &AgentId| {
        edit(&case.root, None, project, agent, "src/lib.rs", now())
            .unwrap_err()
            .0
    };
    assert_eq!(
        refused(&case.project, &team(stopped.id)),
        "ide_agent_not_running"
    );
    assert_eq!(
        refused(&case.project, &team(idle.id)),
        "ide_agent_not_running"
    );
    assert_eq!(refused(&case.project, &team(9999)), "ide_agent_unknown");
    assert_eq!(
        refused(&case.project, &AgentId::new("codex-1").unwrap()),
        "ide_agent_unknown"
    );
    assert_eq!(refused(&beta, &team(working.id)), "ide_agent_unknown");
    assert_eq!(edits(&case), 0);
    assert!(
        !edit(
            &case.root,
            None,
            &case.project,
            &team(working.id),
            "src/lib.rs",
            now()
        )
        .unwrap()
    );
    assert!(
        !edit(
            &case.root,
            None,
            &case.project,
            &team(master.id),
            "README",
            now()
        )
        .unwrap()
    );
    assert_eq!(edits(&case), 2);
}
