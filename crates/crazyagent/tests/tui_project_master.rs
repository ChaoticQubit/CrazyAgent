#![cfg(unix)]

use crazyagent::conversation::ConversationContent;
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerRequest, OwnerResponse, OwnerResult,
};
use crazyagent::master_session::{
    MasterBackend, MasterOwnerAction, MasterReason, MasterSessionPhase, MasterSessionView,
    MasterTurnPhase, MasterTurnView,
};
use crazyagent::protocol::{
    ClientId, Command, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
    WireRequest,
};
use crazyagent::routing::ResourceBudget;
use crazyagent::tui::app::{App, Focus, UiEvent};
use crazyagent::tui::views;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const NOTE: &str = "Owner note before explicit Open 686";
const BRIEF: &str = "Explicit owner brief before master 686";
const REPLACEMENT: &str = "Explicit replacement while master configured 686";
const TURN: &str = "Literal master turn 686 🦀";

fn type_text(app: &mut App, text: &str) {
    assert!(!text.is_empty());
    assert!(!text.chars().any(char::is_control));
    for character in text.chars() {
        app.reduce(UiEvent::Character(character)).unwrap();
    }
}

fn rendered(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(160, 42)).unwrap();
    terminal.draw(|frame| views::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..42 {
        for x in 0..160 {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(!text.is_empty());
    assert!(text.len() <= 65536);
    text
}

#[test]
fn configure_without_project_preserves_unassigned_draft() {
    let mut app = App::new();
    type_text(&mut app, "Unassigned draft 686 日本");
    if let Err(reason) = app.reduce(UiEvent::ConfigureMaster) {
        assert!(!reason.is_empty());
    }
    assert_eq!(app.focus(), Focus::Conversation);
    assert_eq!(app.draft(), "Unassigned draft 686 日本");
    assert!(app.take_command().is_none());
    let screen = rendered(&app);
    assert!(screen.contains("Configure master"), "{screen}");
    assert!(screen.contains("F2"), "{screen}");
    assert!(screen.contains("Select a project"), "{screen}");
    assert!(!screen.contains("Master: Configured"), "{screen}");
}

#[test]
fn configure_preserves_seven_foci_and_legacy_draft_keys() {
    let mut app = App::new();
    type_text(&mut app, "Literal nN] owner input 686");
    let _ = app.reduce(UiEvent::ConfigureMaster);
    for focus in [
        Focus::Projects,
        Focus::Settings,
        Focus::Plan,
        Focus::Supervision,
        Focus::Memory,
        Focus::Evidence,
        Focus::Conversation,
    ] {
        app.reduce(UiEvent::FocusNext).unwrap();
        assert_eq!(app.focus(), focus);
    }
    app.reduce(UiEvent::FocusPrevious).unwrap();
    assert_eq!(app.focus(), Focus::Evidence);
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::ToggleUnassigned).unwrap();
    assert_eq!(app.draft(), "Literal nN] owner input 686");
    app.reduce(UiEvent::ToggleUnassigned).unwrap();
    let before = app.draft().to_owned();
    assert!(app.reduce(UiEvent::ToggleBrief).is_err());
    assert_eq!(app.draft(), before);
    assert!(app.take_command().is_none());
    app.reduce(UiEvent::Backspace).unwrap();
    assert_eq!(app.draft(), "Literal nN] owner input 68");
}

#[tokio::test]
async fn unavailable_catalog_retains_setup_fields_and_project_draft() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = tempfile::Builder::new()
            .prefix("ca-tm686-unit-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let (_stop, stopped) = watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "engine.sock",
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client = Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap();
        let request = |id: &str, revision, command| WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("tui-master-686-unit").unwrap(),
            request_id: RequestId::new(id).unwrap(),
            expected_revision: Cursor::new(revision),
            command,
        };
        let created = client
            .request(request(
                "create-selected-project",
                0,
                Command::CreateProject {
                    project_id: ProjectId::new("p").unwrap(),
                    spec: ProjectSpec {
                        name: "Project 686".into(),
                        repositories: vec![],
                    },
                },
            ))
            .await
            .unwrap();
        assert_eq!(created.outcome, Outcome::Applied);
        let snapshot = client
            .request(request("actual-project-snapshot", 0, Command::Snapshot))
            .await
            .unwrap();
        let mut app = App::new();
        app.apply_reply(snapshot).unwrap();
        service.shutdown().await.unwrap();
        assert_eq!(app.selected_project().unwrap().project_id.as_str(), "p");
        type_text(&mut app, "Project draft retained 686");
        app.reduce(UiEvent::ConfigureMaster).unwrap();
        assert_eq!(app.focus(), Focus::Conversation);
        let screen = rendered(&app);
        assert!(screen.contains("Backend: RCLI"), "{screen}");
        for (field, value) in [
            ("slots", 1),
            ("cpu_millis", 1000),
            ("memory_mib", 512),
            ("processes", 4),
            ("output_bytes", 1048576),
            ("wall_time_ms", 60000),
            ("spend_microunits", 0),
        ] {
            assert!(screen.contains(&format!("{field}: {value}")), "{screen}");
        }
        for label in ["Native Codex", "Native Claude"] {
            app.reduce(UiEvent::SelectionNext).unwrap();
            assert!(rendered(&app).contains(&format!("Backend: {label}")));
        }
        for _ in 0..4 {
            app.reduce(UiEvent::FocusNext).unwrap();
        }
        assert_eq!(app.draft(), "1");
        app.reduce(UiEvent::Backspace).unwrap();
        app.reduce(UiEvent::Character('2')).unwrap();
        assert_eq!(app.draft(), "2");
        app.reduce(UiEvent::Cancel).unwrap();
        assert_eq!(app.draft(), "Project draft retained 686");
        app.reduce(UiEvent::ConfigureMaster).unwrap();
        assert!(rendered(&app).contains("Backend: Native Claude"));
        for _ in 0..4 {
            app.reduce(UiEvent::FocusNext).unwrap();
        }
        assert_eq!(app.draft(), "2");
        assert!(app.reduce(UiEvent::Submit).is_err());
        assert_eq!(app.draft(), "2");
        assert!(rendered(&app).contains("Settings"));
        assert!(app.take_command().is_none());
        app.reduce(UiEvent::Cancel).unwrap();
        assert_eq!(app.draft(), "Project draft retained 686");
    })
    .await
    .expect("real selected-project reducer fixture exceeded 20 seconds");
}

struct Driver {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Driver {
    fn start(root: &Path) -> Self {
        assert!(root.is_absolute());
        assert_eq!(
            fs::metadata(root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let stdout = root.join("driver.stdout");
        let stderr = root.join("driver.stderr");
        let private_log = |path: &Path| {
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .unwrap()
        };
        let child = ProcessCommand::new("python3")
            .env_remove("PYTHONOPTIMIZE")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qualification/tui_project_master.py"
            ))
            .arg(env!("CARGO_BIN_EXE_crazyagent"))
            .arg(root)
            .stdin(Stdio::null())
            .stdout(private_log(&stdout))
            .stderr(private_log(&stderr))
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(child.id() > 1);
        eprintln!(
            "master686 owned driver pid={} pgid={}",
            child.id(),
            child.id()
        );
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn output(&self) -> String {
        let mut text = String::new();
        for path in [&self.stdout, &self.stderr] {
            assert!(fs::metadata(path).unwrap().len() <= 1048576);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            text.push_str(&fs::read_to_string(path).unwrap());
        }
        text
    }

    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(100);
        loop {
            let output = self.output();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "actual master PTY failed: {status}\n{output}"
                );
                assert!(output.contains("PTY PASS master686"), "{output}");
                assert_eq!(
                    output.matches("owned-group=").count(),
                    output.matches("reaped-group=").count()
                );
                eprintln!("{output}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "master PTY exceeded 100 seconds\n{output}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn signal_group(group: u32, signal: &str) {
    assert!(group > 1);
    assert!(matches!(signal, "-TERM" | "-KILL"));
    let _ = ProcessCommand::new("/bin/kill")
        .args([signal, "--", &format!("-{group}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Drop for Driver {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            signal_group(self.child.id(), "-TERM");
            let deadline = Instant::now() + Duration::from_secs(12);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            signal_group(self.child.id(), "-KILL");
            let _ = self.child.wait();
            eprintln!("master686 driver required failure cleanup");
        }
        if let Ok(output) = fs::read_to_string(&self.stdout) {
            for group in output
                .lines()
                .filter_map(|line| line.strip_prefix("owned-group="))
            {
                if let Ok(group) = group.parse::<u32>()
                    && !output
                        .lines()
                        .any(|line| line == format!("reaped-group={group}"))
                {
                    signal_group(group, "-TERM");
                    signal_group(group, "-KILL");
                    eprintln!("master686 TUI group={group} required failure cleanup");
                }
            }
        }
    }
}

async fn read_owner(
    owner: &OwnerClient,
    query: OwnerRead,
) -> crazyagent::engine::owner::OwnerReadReply {
    let reply = owner
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("master686-independent-read").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read { query },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("independent actual owner read failed: {reply:?}");
    };
    assert!(state.revision.value() > 0);
    assert!(!state.config_id.as_str().is_empty());
    state
}

#[tokio::test]
async fn real_pty_master_bootstrap_preserves_provenance_and_reconciles_once() {
    let root = tempfile::Builder::new()
        .prefix("ca-tm686-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut driver = Driver::start(root.path());
    driver.finish();
    tokio::time::timeout(Duration::from_secs(20), async {
        let (_stop, stopped) = watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(), "engine.sock", &root.path().join("global.toml"),
            &root.path().join("project.toml"), &stopped,
        ).await.unwrap();
        let (owner, identity) = OwnerClient::discover(root.path(), OwnerClientKind::Desktop, Duration::from_secs(2)).await.unwrap();
        let settings = read_owner(&owner, OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools, cursor: None, limit: 16,
        }).await;
        let OwnerData::ModelAccessPage { source_hash, .. } = settings.data else {
            panic!("actual configured catalog source hash missing");
        };
        let database = Connection::open_with_flags(root.path().join("engine.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        database.pragma_update(None, "query_only", true).unwrap();
        let sessions: Vec<MasterSessionView> = database.prepare("SELECT body FROM master_sessions ORDER BY created LIMIT 4").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap()
            .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect();
        assert_eq!(sessions.len(), 3);
        let resources = ResourceBudget { slots: 2, cpu_millis: 900, memory_mib: 384, processes: 3,
            output_bytes: 524288, wall_time_ms: 45000, spend_microunits: 1 };
        for (((session, backend), account), target) in sessions.iter().zip([MasterBackend::Rcli, MasterBackend::Codex, MasterBackend::Claude])
            .zip(["api-account", "codex-account", "claude-account"])
            .zip(["sandbox-profile", "codex-connection", "claude-connection"]) {
            assert_eq!(session.project_id.as_str(), "p");
            assert_eq!(session.phase, MasterSessionPhase::Closed);
            assert_eq!(session.selection.backend, backend);
            assert_eq!(session.selection.pool_ref.as_str(), "master-pool");
            assert_eq!(session.selection.route.account_ref.as_str(), account);
            assert_eq!(session.selection.target_profile_ref.as_str(), target);
            assert_eq!(session.selection.route.required_target.as_ref().unwrap(), &session.selection.target_profile_ref);
            assert_eq!(session.selection.route.resources, resources);
            assert!(!session.selection.route.budget_ref.as_str().is_empty());
            assert_eq!(session.config_id, settings.config_id);
            assert_eq!(session.source_hash, source_hash);
            assert_eq!(session.owner.origin, "native_owner");
            assert_ne!(session.owner.engine_epoch, identity.engine_epoch());
            let original = OwnerRequest { protocol_version: PROTOCOL_VERSION, request_id: session.owner.operation_id.clone(),
                expected_revision: session.owner.revision_before.clone(), operation: OwnerOperation::Apply {
                    mutation: OwnerMutation::ProjectMaster { action: MasterOwnerAction::Open {
                        project_id: session.project_id.clone(), expected_config_id: session.config_id.clone(),
                        expected_source_hash: session.source_hash.clone(), selection: Box::new(session.selection.clone()),
                    } },
                } };
            assert_eq!(original.mutation_hash().unwrap(), session.owner.request_hash);
            let receipt: (String, String, i64, i64, i64, Vec<u8>) = database.query_row(
                "SELECT lower(hex(request_hash)),engine_epoch,revision_before,revision_after,event_cursor,result FROM owner_operations WHERE origin='native_owner' AND owner_id=?1 AND operation_id=?2",
                [session.owner.owner_id.as_str(), session.owner.operation_id.as_str()],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).unwrap();
            assert_eq!(receipt.0, session.owner.request_hash);
            assert_eq!(receipt.1, session.owner.engine_epoch);
            assert_eq!(u64::try_from(receipt.2).unwrap(), session.owner.revision_before.value());
            assert_eq!(u64::try_from(receipt.3).unwrap(), session.owner.revision_after.value());
            assert_eq!(u64::try_from(receipt.4).unwrap(), session.owner.event_cursor.as_ref().unwrap().value());
            assert_eq!(serde_json::from_slice::<OwnerResult>(&receipt.5).unwrap(), OwnerResult::Applied);
        }
        let stored_turn: String = database.query_row("SELECT body FROM master_turns", [], |row| row.get(0)).unwrap();
        let turn: MasterTurnView = serde_json::from_str(&stored_turn).unwrap();
        assert_eq!(turn.session_id, sessions[0].session_id);
        assert_eq!(turn.ordinal, 1);
        assert_eq!(turn.phase, MasterTurnPhase::Cancelled);
        assert_eq!(turn.reason, Some(MasterReason::Cancelled));
        assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
        let evidence: Value = serde_json::from_slice(&fs::read(root.path().join("actual-contention-operation.json")).unwrap()).unwrap();
        assert_eq!(evidence["operation_id"].as_str().unwrap(), turn.id.as_str());
        assert_eq!(evidence["input_cursor"].as_u64().unwrap(), turn.input_cursor.value());
        let history = read_owner(&owner, OwnerRead::ConversationHistory { project_id: ProjectId::new("p").unwrap(), cursor: None, limit: 32 }).await;
        let OwnerData::ConversationHistory { page } = history.data else { panic!("actual history absent"); };
        assert!(page.next.is_none());
        assert_eq!(page.entries.len(), 4);
        assert_eq!(page.entries.iter().map(|entry| entry.text.as_str()).collect::<Vec<_>>(), [NOTE, BRIEF, TURN, REPLACEMENT]);
        assert_eq!(page.entries[0].content, ConversationContent::Message);
        assert_eq!(page.entries[1].content, ConversationContent::Brief { version: 1 });
        assert_eq!(page.entries[3].content, ConversationContent::Brief { version: 2 });
        let input = &page.entries[2];
        assert_eq!(input.event_cursor, turn.input_cursor);
        assert_eq!(input.provenance.operation_id, turn.id);
        assert_eq!(input.provenance.request_hash, evidence["request_hash"].as_str().unwrap());
        assert_eq!(input.provenance.origin, "native_owner");
        let submit = OwnerRequest { protocol_version: PROTOCOL_VERSION, request_id: turn.id.clone(),
            expected_revision: input.provenance.revision_before.clone(), operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action: MasterOwnerAction::Submit {
                    project_id: sessions[0].project_id.clone(), session_id: sessions[0].session_id.clone(), text: TURN.into(),
                } },
            } };
        assert_eq!(submit.mutation_hash().unwrap(), input.provenance.request_hash);
        let readback = read_owner(&owner, OwnerRead::ProjectMaster { project_id: ProjectId::new("p").unwrap() }).await;
        let OwnerData::ProjectMaster { session } = readback.data else { panic!("actual restarted session absent"); };
        let session = session.map(|session| *session);
        assert_eq!(session.as_ref(), sessions.last());
        for session in &sessions {
            let turns = read_owner(&owner, OwnerRead::ProjectMasterTurns {
                project_id: session.project_id.clone(), session_id: session.session_id.clone(), cursor: None, limit: 32,
            }).await;
            let OwnerData::ProjectMasterTurns { page } = turns.data else { panic!("actual turn page absent"); };
            assert!(page.next.is_none());
            assert_eq!(page.turns, if session.selection.backend == MasterBackend::Rcli { vec![turn.clone()] } else { vec![] });
        }
        let other = read_owner(&owner, OwnerRead::ProjectMaster { project_id: ProjectId::new("q").unwrap() }).await;
        assert!(matches!(other.data, OwnerData::ProjectMaster { session: None }));
        let counts: (i64,i64,i64) = database.query_row("SELECT (SELECT COUNT(*) FROM master_turns),(SELECT COUNT(*) FROM tasks),(SELECT COUNT(*) FROM worktree_leases)", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(counts, (1,0,0));
        service.shutdown().await.unwrap();
    }).await.expect("independent owner readback exceeded 20 seconds");
}
