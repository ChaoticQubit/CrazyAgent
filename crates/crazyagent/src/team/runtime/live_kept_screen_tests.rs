use super::super::TurnEnd;
use super::super::managed_agent::{AGENT, ManagedAgent, SignedIn, claude};
use super::TURN;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    OwnerClientKind, OwnerResponse, OwnerResult, TerminalOutput, TerminalSession,
};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind};
use crate::protocol::{ProjectId, ProjectSpec};
use crate::team::{AgentRole, AgentStatus, NodeKind};
use crate::terminal::{AgentId, Host, TerminalFrame};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

const PROJECT: &str = "alpha";
const ASK_SUM: &str = "What is 2468 plus 1357? Reply with only the number.";
const SUM: &str = "3825";
const SHOWN: Duration = Duration::from_secs(10);

/// A real engine on a fresh root, with its owner socket, its own terminal host and one
/// set-up project, which the test reads and attaches to as the app does. The runtime is
/// the last field, so a failed test drops the engine before the runtime it runs on.
struct OwnerEngine {
    service: EngineService,
    controller: OwnerController,
    host: Arc<dyn Host>,
    root: tempfile::TempDir,
    _home: tempfile::TempDir,
    _stop: tokio::sync::watch::Sender<bool>,
    runtime: tokio::runtime::Runtime,
}

/// A new owner-only folder under `/tmp`. Input: its name's prefix. Output: the folder.
fn private_dir(prefix: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Checks that the engine recorded one owner change. Input: its reply. Output: none.
fn applied(response: OwnerResponse) {
    match response {
        OwnerResponse::Applied { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("the owner change must be recorded: {other:?}"),
    }
}

/// Whether `seen` holds `wanted`. Inputs: the bytes and the text. Output: whether.
fn contains(seen: &[u8], wanted: &[u8]) -> bool {
    seen.windows(wanted.len()).any(|window| window == wanted)
}

/// Reads a terminal view's output into `seen` until it holds `wanted`, for up to
/// [`SHOWN`]. Inputs: the output, the bytes read so far and the text. Output: whether it
/// showed.
async fn read_until(output: &mut TerminalOutput, seen: &mut Vec<u8>, wanted: &[u8]) -> bool {
    tokio::time::timeout(SHOWN, async {
        while !contains(seen, wanted) {
            match output.next().await {
                Ok(TerminalFrame::Output { bytes }) => seen.extend_from_slice(&bytes),
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false)
}

/// The reason a terminal view closed with, read within [`SHOWN`]. Input: the output.
/// Output: the reason, or what came instead.
async fn closed(output: &mut TerminalOutput) -> String {
    tokio::time::timeout(SHOWN, async {
        loop {
            match output.next().await {
                Ok(TerminalFrame::Output { .. }) => {}
                Ok(TerminalFrame::Closed { reason }) => return reason,
                other => return format!("{other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| "no close in time".into())
}

impl OwnerEngine {
    /// Starts the engine, attaches the owner as the desktop app does and sets up the
    /// project. Input: none. Output: the engine.
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let root = private_dir("ca-live-kept-");
        let global = root.path().join("g.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        crate::terminal::tmux::installed(root.path()).expect("BLOCKED: tmux must be installed");
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = runtime
            .block_on(EngineService::start_with_settings_sources(
                root.path(),
                "e.sock",
                &global,
                &root.path().join("p.toml"),
                &stopped,
            ))
            .unwrap();
        let host = service
            .owner_runtime()
            .expect("the engine runs its owner runtime")
            .terminal()
            .unwrap();
        let home = private_dir("ca-live-kept-home-");
        let projects = home.path().join("Projects");
        std::fs::create_dir(&projects).unwrap();
        let controller = runtime.block_on(async {
            let mut controller =
                OwnerController::attach(root.path(), OwnerClientKind::Desktop, SHOWN)
                    .await
                    .unwrap();
            applied(
                controller
                    .set_projects_home(projects.to_str().unwrap().into(), 1)
                    .await
                    .unwrap(),
            );
            applied(
                controller
                    .setup_project(
                        ProjectId::new(PROJECT).unwrap(),
                        ProjectSpec {
                            name: "Alpha".into(),
                            repositories: vec![],
                        },
                        ProjectSetup {
                            about: "Watches sensor buoys and raises alerts.".into(),
                            tech_stack: vec!["rust".into()],
                            platforms: vec![Platform::Website],
                            research: vec![ResearchKind::MarketResearch],
                            pool: None,
                            team: None,
                            documents: vec![],
                        },
                        2,
                    )
                    .await
                    .unwrap(),
            );
            controller
        });
        Self {
            runtime,
            service,
            controller,
            host,
            root,
            _home: home,
            _stop: stop,
        }
    }

    /// Records in the engine's database the stopped sub-agent `sub` under a stopped lead,
    /// as a team that ran here leaves them, and shows the project's finished agents.
    /// Input: the sub-agent's id. Output: none.
    fn finished_sub(&self, sub: i64) {
        let connection = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        connection.busy_timeout(SHOWN).unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO team_finished_view(project,show) VALUES (?1,1)",
                [PROJECT],
            )
            .unwrap();
        let lead = sub + 1;
        for (id, parent) in [(lead, None), (sub, Some(lead))] {
            connection
                .execute(
                    "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
                     VALUES (?1,?2,'sub','claude-code',1,'stopped',1,1,?3)",
                    rusqlite::params![id, PROJECT, parent],
                )
                .unwrap();
        }
    }

    /// Stops the engine and its terminal server. Input: the engine. Output: none.
    fn stop(self) {
        let Self {
            runtime,
            mut service,
            controller,
            host,
            ..
        } = self;
        drop(controller);
        runtime
            .block_on(async { tokio::time::timeout(SHOWN, service.shutdown()).await })
            .unwrap()
            .unwrap();
        crate::terminal::quit(host.as_ref(), true).unwrap();
    }
}

/// N-D21: a real Claude Code sub-agent the team runtime runs on the engine's own terminal
/// host, on the owner's signed-in account, answers and ends; once its team shows finished
/// agents, its card in the engine's team graph still names its terminal, and attaching
/// shows its last screen with the answer under the finished label and then closes. A
/// failure at the card's terminal is the bug. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_finished_real_claude_sub_agent_s_card_opens_its_kept_last_screen() {
    let mut engine = OwnerEngine::start();
    let mut signed_in = SignedIn::claude();
    let agent = ManagedAgent::in_pane(
        "claude-code",
        &claude(),
        engine.host.clone(),
        AgentRole::Sub,
    );
    let profile = signed_in.acquire("claude-live-kept-screen");
    let (text, end, cause) = agent.run_turn(profile, ASK_SUM, TURN);
    println!("real claude sub-agent turn: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains(SUM), "{text}");
    assert!(
        !agent.home.join(".credentials.json").exists(),
        "the turn's credential copy left the sub-agent's home"
    );
    let finished = AgentId::team(AGENT).unwrap();
    assert_eq!(
        engine
            .host
            .input(&finished, b"x")
            .err()
            .map(|error| error.0),
        Some("terminal_agent_unknown".to_owned()),
        "the sub-agent's terminal ended with its turn"
    );

    engine.finished_sub(AGENT);
    let graph = engine
        .runtime
        .block_on(
            engine
                .controller
                .team_graph(ProjectId::new(PROJECT).unwrap()),
        )
        .unwrap();
    let node = graph
        .nodes
        .iter()
        .find(|node| node.id == finished.to_string())
        .unwrap_or_else(|| panic!("{finished} is drawn: {graph:?}"));
    assert_eq!(
        (node.kind, node.status),
        (NodeKind::Sub, Some(AgentStatus::Stopped))
    );
    assert_eq!(
        node.terminal,
        Some(finished.clone()),
        "the card of a finished sub-agent whose last screen is kept opens it"
    );

    let TerminalSession { mut output, .. } = engine
        .runtime
        .block_on(engine.controller.terminal_attach(finished))
        .unwrap();
    let mut seen = Vec::new();
    let shown = engine.runtime.block_on(async {
        read_until(&mut output, &mut seen, SUM.as_bytes()).await
            && read_until(&mut output, &mut seen, b" Finished at ").await
    });
    assert!(
        shown,
        "the card shows the sub-agent's last screen under its finished label: {}",
        String::from_utf8_lossy(&seen)
    );
    assert_eq!(
        engine.runtime.block_on(closed(&mut output)),
        "terminal_ended"
    );
    drop(agent);
    engine.stop();
}
