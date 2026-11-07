use super::*;
use crate::config::{AccountPool, ModelAccessEdit};
use crate::engine::owner::{ModelAccessCollection, Started};
use crate::master_session::MasterSelection;
use crate::master_sessions::{MasterSessionSummary, ResumePlan, SESSIONS_LISTED_MAX};
use crate::protocol::{CredentialRef, ProjectSpec};
use crate::routing::{ResourceBudget, RouteRequest};
use crate::storage::{Job, Storage};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::oneshot;

const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["session-profile"]
[routing]
approved_accounts = ["session-account", "native-account"]
[model_access]
accounts = ["session-account", "native-account"]
[[model_access.connections]]
id = "session-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "session-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.connections]]
id = "native-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "native-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "native-profile" }
[[model_access.pools]]
id = "session-pool"
members = ["session-connection"]
projects = ["session-project"]
roles = ["project_leader"]
fallback_allowed = false
[[model_access.pools]]
id = "native-pool"
members = ["native-connection"]
projects = ["session-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

struct Case {
    storage: Storage,
    started: Started,
    project: ProjectId,
    root: tempfile::TempDir,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

async fn start(root: &Path) -> (Storage, Started) {
    let (storage, _) = Storage::open_with_settings_sources(
        &root.join("engine.db"),
        &root.join("global.toml"),
        &root.join("project.toml"),
    )
    .await
    .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("owner start queue unavailable"));
    let started = receive.await.unwrap().unwrap();
    (storage, started)
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ses-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CONFIG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let repository = root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        std::fs::write(repository.join("README.md"), "Session repository\n").unwrap();
        let (storage, started) = start(root.path()).await;
        let case = Self {
            storage,
            started,
            project: ProjectId::new("session-project").unwrap(),
            root,
        };
        let created = case
            .apply(
                "create-session-project",
                OwnerMutation::CreateProject {
                    project_id: case.project.clone(),
                    spec: ProjectSpec {
                        name: "Session project".into(),
                        repositories: vec![repository.to_str().unwrap().into()],
                    },
                },
            )
            .await;
        assert_eq!(created, OwnerResult::Applied);
        case
    }

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerRead(
                self.started.owners[0].admission.clone(),
                query,
                send,
            ))
            .unwrap_or_else(|_| panic!("owner read queue unavailable"));
        receive.await.unwrap().unwrap()
    }

    async fn apply(&self, id: &str, mutation: OwnerMutation) -> OwnerResult {
        let basis = self.read(OwnerRead::Capabilities).await;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerApply(
                self.started.owners[0].admission.clone(),
                OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: RequestId::new(id).unwrap(),
                    expected_revision: basis.revision,
                    operation: OwnerOperation::Apply { mutation },
                },
                send,
            ))
            .unwrap_or_else(|_| panic!("owner apply queue unavailable"));
        receive.await.unwrap().unwrap().result
    }

    async fn master(&self, id: &str, action: MasterOwnerAction) -> OwnerResult {
        self.apply(id, OwnerMutation::ProjectMaster { action })
            .await
    }

    async fn basis(&self) -> (ConfigId, String, Box<MasterSelection>) {
        self.basis_for(MasterBackend::Rcli).await
    }

    async fn basis_for(&self, backend: MasterBackend) -> (ConfigId, String, Box<MasterSelection>) {
        let (pool, account) = match backend {
            MasterBackend::Rcli => ("session-pool", "session-account"),
            MasterBackend::Codex | MasterBackend::Claude => ("native-pool", "native-account"),
        };
        let basis = self
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            })
            .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("model source basis missing");
        };
        let target = reference("session-profile");
        let selection = MasterSelection {
            backend,
            pool_ref: reference(pool),
            target_profile_ref: target.clone(),
            route: RouteRequest {
                preferred_target: Some(target.clone()),
                required_target: Some(target),
                required_capabilities: vec![],
                account_ref: reference(account),
                budget_ref: reference("session-budget"),
                resources: ResourceBudget::unit(),
            },
        };
        (basis.config_id, source_hash, Box::new(selection))
    }

    async fn open(&self, id: &str) -> OwnerResult {
        let (expected_config_id, expected_source_hash, selection) = self.basis().await;
        self.master(
            id,
            MasterOwnerAction::Open {
                project_id: self.project.clone(),
                expected_config_id,
                expected_source_hash,
                selection,
            },
        )
        .await
    }

    async fn renew(&self, id: &str, cancel_running: bool) -> OwnerResult {
        self.renew_with(id, cancel_running, MasterBackend::Rcli)
            .await
    }

    async fn renew_with(
        &self,
        id: &str,
        cancel_running: bool,
        backend: MasterBackend,
    ) -> OwnerResult {
        let (expected_config_id, expected_source_hash, selection) = self.basis_for(backend).await;
        self.master(
            id,
            MasterOwnerAction::NewSession {
                project_id: self.project.clone(),
                expected_config_id,
                expected_source_hash,
                selection,
                cancel_running,
            },
        )
        .await
    }

    async fn resume(&self, id: &str, session: &SessionId, cancel_running: bool) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Resume {
                project_id: self.project.clone(),
                session_id: session.clone(),
                cancel_running,
            },
        )
        .await
    }

    async fn submit(&self, id: &str, session: &SessionId, text: &str) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Submit {
                project_id: self.project.clone(),
                session_id: session.clone(),
                text: text.into(),
            },
        )
        .await
    }

    async fn delete(&self, id: &str, session: &SessionId) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::DeleteSession {
                project_id: self.project.clone(),
                session_id: session.clone(),
            },
        )
        .await
    }

    fn used_in_terminal(&self, session: &SessionId) {
        crate::master_sessions::mark_owner_turn(&self.db(), &self.project, session.as_str())
            .unwrap();
    }

    async fn shown(&self) -> MasterSessionView {
        let state = self
            .read(OwnerRead::ProjectMaster {
                project_id: self.project.clone(),
            })
            .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("a master session is shown");
        };
        *session
    }

    async fn sessions(&self) -> Vec<MasterSessionSummary> {
        let state = self
            .read(OwnerRead::ProjectMasterSessions {
                project_id: self.project.clone(),
            })
            .await;
        let OwnerData::ProjectMasterSessions { sessions } = state.data else {
            panic!("the session list is read");
        };
        sessions
    }

    async fn turns(&self, session: &SessionId) -> Vec<MasterTurnView> {
        let state = self
            .read(OwnerRead::ProjectMasterTurns {
                project_id: self.project.clone(),
                session_id: session.clone(),
                cursor: None,
                limit: 32,
            })
            .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("the session's turns are read");
        };
        page.turns
    }

    async fn reopen(self) -> Self {
        self.reopen_with(None).await
    }

    async fn reopen_with(self, settings: Option<&str>) -> Self {
        let project = self.project.clone();
        let root = self.close().await;
        if let Some(settings) = settings {
            let global = root.path().join("global.toml");
            std::fs::write(&global, settings).unwrap();
            std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (storage, started) = start(root.path()).await;
        Self {
            storage,
            started,
            project,
            root,
        }
    }

    async fn close(self) -> tempfile::TempDir {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerStop(self.started.host, send))
            .unwrap_or_else(|_| panic!("owner stop queue unavailable"));
        receive.await.unwrap().unwrap();
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .unwrap_or_else(|_| panic!("storage close queue unavailable"));
        receive.await.unwrap().unwrap();
        self.root
    }

    fn db(&self) -> Connection {
        let db = Connection::open(self.root.path().join("engine.db")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(2)).unwrap();
        db
    }

    fn phases(&self) -> Vec<(String, String)> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT id,phase FROM master_sessions WHERE project=?1 ORDER BY created")
            .unwrap();
        statement
            .query_map([self.project.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn snapshot(case: &Case) -> Vec<(String, String, i64, String)> {
    let db = case.db();
    let mut statement = db
        .prepare("SELECT id,phase,version,body FROM master_sessions ORDER BY created")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn live_count(phases: &[(String, String)]) -> usize {
    phases
        .iter()
        .filter(|(_, phase)| phase == "configured")
        .count()
}

#[tokio::test]
async fn new_session_parks_the_live_one_and_opens_a_fresh_one_atomically() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit(
            "first-message",
            &first.session_id,
            "Plan the onboarding page"
        )
        .await,
        OwnerResult::Applied
    );
    let before = case.phases();
    let refused = case.renew("renew-unconfirmed", false).await;
    assert_eq!(
        refused,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(
        case.phases(),
        before,
        "a refused New session changes nothing"
    );
    let queued = case.turns(&first.session_id).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].phase, MasterTurnPhase::Queued);

    assert_eq!(
        case.renew("renew-confirmed", true).await,
        OwnerResult::Applied
    );
    let phases = case.phases();
    assert_eq!(phases.len(), 2);
    assert_eq!(live_count(&phases), 1);
    assert_eq!(
        phases[0],
        (first.session_id.as_str().into(), "parked".into())
    );
    assert_eq!(phases[1].1, "configured");
    let second = case.shown().await;
    assert_eq!(second.session_id.as_str(), phases[1].0);
    assert_eq!(second.phase, MasterSessionPhase::Configured);
    assert_eq!(second.selection, first.selection);
    let cancelled = case.turns(&first.session_id).await;
    assert_eq!(cancelled[0].phase, MasterTurnPhase::Cancelled);
    assert_eq!(cancelled[0].reason, Some(MasterReason::Cancelled));

    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![first.session_id.clone()],
        "the fresh live session holds no owner turn, so it is not listed"
    );
    case.used_in_terminal(&second.session_id);
    let listed = case.sessions().await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].session_id, second.session_id);
    assert_eq!(listed[0].phase, MasterSessionPhase::Configured);
    assert_eq!(
        listed[0].turns, 1,
        "the turn run in the live terminal counts as the session's message"
    );
    assert!(listed[0].started_ms.is_some());
    assert_eq!(listed[1].session_id, first.session_id);
    assert_eq!(listed[1].phase, MasterSessionPhase::Parked);
    assert_eq!(listed[1].turns, 1);
    assert_eq!(
        listed[1].first_line.as_deref(),
        Some("Plan the onboarding page")
    );
    assert_eq!(
        listed[1].last_line.as_deref(),
        Some("Plan the onboarding page")
    );
    assert_eq!(listed[1].results_waiting, 0);

    let again = case.open("open-while-live").await;
    assert_eq!(
        again,
        OwnerResult::Rejected {
            code: "master_session_active".into()
        },
        "Open never parks a live session; only New session does"
    );
    case.close().await;
}

#[tokio::test]
async fn resume_swaps_the_live_session_atomically_and_keeps_every_turn() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.master(
            "close-first",
            MasterOwnerAction::Close {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
            },
        )
        .await,
        OwnerResult::Applied
    );
    let ended = case.shown().await;
    assert_eq!(ended.session_id, first.session_id);
    assert_eq!(
        ended.phase,
        MasterSessionPhase::Parked,
        "End conversation parks, so the session stays resumable"
    );
    assert_eq!(
        case.renew("renew-after-end", false).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        case.submit(
            "second-message",
            &second.session_id,
            "Write the release notes"
        )
        .await,
        OwnerResult::Applied
    );

    let before = case.phases();
    assert_eq!(
        case.resume("resume-unconfirmed", &first.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(case.phases(), before);
    assert_eq!(
        case.resume("resume-confirmed", &first.session_id, true)
            .await,
        OwnerResult::Applied
    );
    let resumed = case.shown().await;
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
    assert_eq!(resumed.current_turn, None);
    let phases = case.phases();
    assert_eq!(live_count(&phases), 1);
    assert_eq!(
        phases[1],
        (second.session_id.as_str().into(), "parked".into())
    );
    let kept = case.turns(&second.session_id).await;
    assert_eq!(kept.len(), 1, "the parked session keeps its turn");
    assert_eq!(kept[0].phase, MasterTurnPhase::Cancelled);

    assert_eq!(
        case.resume("resume-live", &first.session_id, false).await,
        OwnerResult::Rejected {
            code: "master_session_live".into()
        }
    );
    assert_eq!(
        case.submit("resumed-message", &first.session_id, "Continue the plan")
            .await,
        OwnerResult::Applied,
        "a resumed session takes new messages"
    );
    let listed = case.sessions().await;
    assert_eq!(listed[0].session_id, first.session_id);
    assert_eq!(listed[0].turns, 1);
    assert_eq!(listed[1].session_id, second.session_id);
    assert_eq!(
        listed[1].first_line.as_deref(),
        Some("Write the release notes")
    );

    let db = case.db();
    let ResumePlan::Fresh { seed, note } = crate::master_sessions::resume_harness(
        &db,
        &second.project_id,
        &second.session_id,
        "codex",
        &|_| true,
    )
    .unwrap() else {
        panic!("a session with earlier turns and no kept conversation starts fresh");
    };
    assert!(seed.contains("Owner: Write the release notes"), "{seed}");
    assert!(seed.contains("(No reply was received.)"), "{seed}");
    assert_eq!(note, crate::master_sessions::FRESH_NOTE);
    case.close().await;
}

#[tokio::test]
async fn the_session_list_is_bounded_and_newest_activity_first() {
    let case = Case::new().await;
    assert_eq!(case.open("open-0").await, OwnerResult::Applied);
    let total = SESSIONS_LISTED_MAX + 2;
    for index in 1..total {
        case.used_in_terminal(&case.shown().await.session_id);
        assert_eq!(
            case.renew(&format!("renew-{index}"), false).await,
            OwnerResult::Applied,
            "renew {index}"
        );
    }
    case.used_in_terminal(&case.shown().await.session_id);
    let phases = case.phases();
    assert_eq!(phases.len(), total);
    assert_eq!(live_count(&phases), 1);
    let oldest = SessionId::new(phases[0].0.clone()).unwrap();
    assert_eq!(
        case.resume("resume-oldest", &oldest, false).await,
        OwnerResult::Applied
    );
    let listed = case.sessions().await;
    assert_eq!(listed.len(), SESSIONS_LISTED_MAX);
    assert_eq!(listed[0].session_id, oldest, "the live session comes first");
    assert_eq!(listed[0].phase, MasterSessionPhase::Configured);
    assert_eq!(
        listed[1].session_id.as_str(),
        phases[total - 1].0,
        "the session parked by the resume was active last"
    );
    for pair in listed[1..].windows(2) {
        assert!(
            (pair[0].last_activity.value(), pair[0].created.value())
                > (pair[1].last_activity.value(), pair[1].created.value())
        );
    }
    assert!(
        listed
            .iter()
            .skip(1)
            .all(|summary| summary.phase == MasterSessionPhase::Parked)
    );
    assert!(serde_json::to_vec(&listed).unwrap().len() <= crate::master_sessions::LIST_BYTES_MAX);
    case.close().await;
}

#[tokio::test]
async fn results_of_a_parked_session_wait_for_it_and_its_agents_are_its_own() {
    let case = Case::new().await;
    assert_eq!(case.open("open-first").await, OwnerResult::Applied);
    let first = case.shown().await;
    case.used_in_terminal(&first.session_id);
    assert_eq!(
        case.renew("renew-second", false).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    case.used_in_terminal(&second.session_id);
    let db = case.db();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,session) VALUES (?1,'sub','codex',0,'idle',1,1,?2)",
        params![case.project.as_str(), first.session_id.as_str()],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    crate::team::names::assign_persona(&db, &case.project, agent).unwrap();
    for (prompt, status, delivered) in [
        ("Draft the copy", "done", 0),
        ("Check the links", "failed", 0),
        ("Old step", "done", 1),
        ("Still running", "running", 0),
    ] {
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,delivered,created_ms,finished_ms,session) VALUES (?1,'master','codex',0,?2,?3,?4,'copy drafted',?5,1,2,?6)",
            params![case.project.as_str(), prompt, status, agent, delivered, first.session_id.as_str()],
        )
        .unwrap();
    }
    let listed = case.sessions().await;
    let parked = listed
        .iter()
        .find(|summary| summary.session_id == first.session_id)
        .unwrap();
    assert_eq!(parked.results_waiting, 2);
    assert_eq!(parked.agents.len(), 1);
    let live = listed
        .iter()
        .find(|summary| summary.session_id == second.session_id)
        .unwrap();
    assert_eq!(live.results_waiting, 0);
    assert!(live.agents.is_empty());

    let state = crate::master_sessions::session_state(&db, &case.project, &first.session_id)
        .unwrap()
        .unwrap();
    assert!(state.contains(&parked.agents[0]), "{state}");
    assert!(state.contains("Last result: copy drafted"), "{state}");
    assert!(state.contains("Still running"), "{state}");
    assert_eq!(
        crate::master_sessions::session_state(&db, &case.project, &second.session_id)
            .unwrap()
            .map(|state| state.contains(&parked.agents[0])),
        Some(false),
        "another session's agents are not listed as its own"
    );
    case.close().await;
}

#[tokio::test]
async fn upgrading_a_version_3_database_makes_ended_sessions_resumable() {
    let case = Case::new().await;
    assert_eq!(case.open("open-zero").await, OwnerResult::Applied);
    case.used_in_terminal(&case.shown().await.session_id);
    assert_eq!(
        case.renew_with("open-first", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );
    let first = case.shown().await;
    assert_eq!(first.selection.backend, MasterBackend::Claude);
    assert_eq!(
        case.submit(
            "first-message",
            &first.session_id,
            "Sketch the settings page"
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-second", true).await, OwnerResult::Applied);
    let second = case.shown().await;
    let root = case.close().await;
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(&format!(
        "ALTER TABLE master_sessions RENAME TO master_sessions_now;
         {UNPARKED_SESSIONS};
         INSERT INTO master_sessions SELECT id,project,created,version,
             CASE phase WHEN 'parked' THEN 'closed' ELSE phase END,
             CASE phase WHEN 'parked' THEN json_set(body,'$.phase','closed') ELSE body END
             FROM master_sessions_now;
         DROP TABLE master_sessions_now;
         {CURRENT}; {LATEST};
         ALTER TABLE team_tasks DROP COLUMN session;
         ALTER TABLE team_agents DROP COLUMN session;
         PRAGMA user_version = 3;"
    ))
    .unwrap();
    let closed: String = db
        .query_row(
            "SELECT phase FROM master_sessions WHERE id=?1",
            [first.session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(closed, "closed");
    drop(db);

    let (storage, started) = start(root.path()).await;
    let case = Case {
        storage,
        started,
        project: ProjectId::new("session-project").unwrap(),
        root,
    };
    let db = case.db();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let latest = crate::storage::migrations::STEPS.last().unwrap().to;
    assert_eq!(version, latest);
    let schema: String = db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='master_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, SESSIONS);
    for table in ["team_tasks", "team_agents"] {
        let present: bool = db
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='session')"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(present, "{table} records its owning session");
    }
    assert!(
        root_backups(&case)
            .iter()
            .any(|name| name.starts_with(&format!("3-to-{latest}-"))),
        "the upgrade kept a backup"
    );
    drop(db);
    let phases = case.phases();
    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0].1, "parked");
    assert_eq!(
        phases[1],
        (first.session_id.as_str().into(), "parked".into())
    );
    assert_eq!(
        phases[2],
        (second.session_id.as_str().into(), "configured".into())
    );
    assert_eq!(
        case.resume("resume-upgraded", &first.session_id, false)
            .await,
        OwnerResult::Applied
    );
    let resumed = case.shown().await;
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
    assert_eq!(case.turns(&first.session_id).await.len(), 1);
    case.close().await;
}

fn root_backups(case: &Case) -> Vec<String> {
    std::fs::read_dir(case.root.path().join(crate::storage::migrations::BACKUPS))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect()
}

fn git(folder: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Session Test",
                "-c",
                "user.email=session@test.invalid",
            ])
            .args(args)
            .current_dir(folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn parking_switching_and_new_sessions_never_touch_a_dirty_master_worktree() {
    let case = Case::new().await;
    let repository = case.root.path().join("repository");
    git(&repository, &["init", "-q", "-b", "main"]);
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-q", "-m", "start"]);
    let worktree = case.root.path().join("master-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "master-work",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::write(worktree.join("draft.md"), "Unsaved master draft\n").unwrap();
    std::fs::write(worktree.join("README.md"), "Edited by the master\n").unwrap();
    let dirty = git(&worktree, &["status", "--porcelain"]);
    assert!(
        dirty.contains("draft.md") && dirty.contains("README.md"),
        "{dirty}"
    );

    assert_eq!(case.open("open-dirty").await, OwnerResult::Applied);
    let first = case.shown().await;
    case.used_in_terminal(&first.session_id);
    assert_eq!(case.renew("renew-dirty", false).await, OwnerResult::Applied);
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        case.resume("resume-dirty", &first.session_id, false).await,
        OwnerResult::Applied
    );
    assert_eq!(
        case.master(
            "park-dirty",
            MasterOwnerAction::Close {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
            },
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(live_count(&case.phases()), 0);

    assert_eq!(
        std::fs::read_to_string(worktree.join("draft.md")).unwrap(),
        "Unsaved master draft\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "Edited by the master\n"
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), dirty);
    let listed = git(&repository, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(worktree.to_str().unwrap()), "{listed}");
    case.close().await;
}

#[tokio::test]
async fn resume_refuses_a_session_the_current_settings_or_engine_run_no_longer_admit() {
    let case = Case::new().await;
    assert_eq!(case.open("open-rcli").await, OwnerResult::Applied);
    let rcli = case.shown().await;
    case.used_in_terminal(&rcli.session_id);
    assert_eq!(
        case.renew_with("renew-native", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );
    let native = case.shown().await;
    case.used_in_terminal(&native.session_id);
    assert_eq!(case.renew("renew-last", false).await, OwnerResult::Applied);
    let case = case.reopen().await;

    let before = snapshot(&case);
    assert_eq!(
        case.resume("resume-stale-rcli", &rcli.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        },
        "an RCLI session cannot move to a new engine run, so a send would refuse it too"
    );
    assert_eq!(snapshot(&case), before, "a refused Resume changes nothing");

    assert_eq!(
        case.resume("resume-native", &native.session_id, false)
            .await,
        OwnerResult::Applied,
        "a native session moves to the new engine run as a send does"
    );
    assert_eq!(case.shown().await.session_id, native.session_id);
    assert_eq!(
        case.renew_with("renew-after-native", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );

    let changed = CONFIG.replace("configured-claude-model", "replaced-claude-model");
    assert_ne!(changed, CONFIG);
    let case = case.reopen_with(Some(&changed)).await;
    let before = snapshot(&case);
    assert_eq!(
        case.resume("resume-out-of-scope", &native.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        },
        "the session's own pool changed, so it no longer runs under the current settings"
    );
    assert_eq!(snapshot(&case), before, "a refused Resume changes nothing");
    case.close().await;
}

/// Has the owner replace the native pool with one serving the given projects, its
/// members, roles, fallback and quota policy as in CONFIG. Inputs: the case, the edit id
/// and the projects. Output: the owner's result.
async fn replace_native_pool(case: &Case, id: &str, projects: &[&str]) -> OwnerResult {
    let (expected_config_id, expected_source_hash, _) = case.basis().await;
    case.apply(
        id,
        OwnerMutation::ModelAccess {
            expected_config_id,
            expected_source_hash,
            edit: ModelAccessEdit::ReplacePool {
                pool: AccountPool {
                    id: reference("native-pool"),
                    name: None,
                    members: vec![reference("native-connection")],
                    projects: projects.iter().copied().map(reference).collect(),
                    roles: vec![reference("project_leader")],
                    fallback_allowed: false,
                    provider_quota_policy: Default::default(),
                },
            },
        },
    )
    .await
}

/// Parks a used native session behind a fresh one, then has the owner replace the native
/// pool with one serving the given projects, as adding a folder again did in run 7 (N-D15).
/// A second project exists first so the pool can name it, and one unchanged pool edit runs
/// before any session opens: every pool edit writes the assigned pools' approvals and
/// destinations into the settings (config.rs:912-975), which CONFIG lacks for the native
/// connection. Inputs: the case and the projects. Output: the parked session.
async fn parked_native_after_pool_edit(case: &Case, projects: &[&str]) -> SessionId {
    let repository = case.root.path().join("joined-repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::write(repository.join("README.md"), "Joined repository\n").unwrap();
    assert_eq!(
        case.apply(
            "create-joined-project",
            OwnerMutation::CreateProject {
                project_id: ProjectId::new("joined-project").unwrap(),
                spec: ProjectSpec {
                    name: "Joined project".into(),
                    repositories: vec![repository.to_str().unwrap().into()],
                },
            },
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(
        replace_native_pool(case, "settle-native-pool", &["session-project"]).await,
        OwnerResult::Applied
    );
    assert_eq!(case.open("open-rcli").await, OwnerResult::Applied);
    let rcli = case.shown().await;
    case.used_in_terminal(&rcli.session_id);
    assert_eq!(
        case.renew_with("renew-native", false, MasterBackend::Claude)
            .await,
        OwnerResult::Applied
    );
    let native = case.shown().await;
    case.used_in_terminal(&native.session_id);
    assert_eq!(case.renew("renew-last", false).await, OwnerResult::Applied);
    let (bound, _, _) = case.basis().await;
    assert_eq!(native.config_id, bound);
    assert_eq!(
        replace_native_pool(case, "replace-native-pool", projects).await,
        OwnerResult::Applied
    );
    let (after, _, _) = case.basis().await;
    assert_ne!(after, bound, "the pool edit changes the settings");
    native.session_id
}

#[tokio::test]
async fn resume_continues_a_session_after_another_project_joins_its_pool() {
    let case = Case::new().await;
    let native = parked_native_after_pool_edit(&case, &["session-project", "joined-project"]).await;
    assert_eq!(
        case.resume("resume-joined", &native, false).await,
        OwnerResult::Applied,
        "another project joining the session's pool leaves what the session runs on as it was"
    );
    assert_eq!(case.shown().await.session_id, native);
    case.close().await;
}

#[tokio::test]
async fn resume_refuses_a_session_whose_own_project_left_its_pool() {
    let case = Case::new().await;
    let native = parked_native_after_pool_edit(&case, &["joined-project"]).await;
    let before = snapshot(&case);
    assert_eq!(
        case.resume("resume-left", &native, false).await,
        OwnerResult::Rejected {
            code: "master_source_conflict".into()
        },
        "the session's own project left its pool, so it no longer runs under the current settings"
    );
    assert_eq!(snapshot(&case), before, "a refused Resume changes nothing");
    case.close().await;
}

#[tokio::test]
async fn a_parked_session_s_owner_messages_are_read_back_for_its_turns_only() {
    let case = Case::new().await;
    assert_eq!(case.open("open-history").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit("history-message", &first.session_id, "Plan the first slice")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        case.renew("renew-history", true).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    assert_eq!(
        case.submit("other-message", &second.session_id, "Add a dark theme")
            .await,
        OwnerResult::Applied
    );
    let own = case.turns(&first.session_id).await[0].input_cursor.clone();
    let other = case.turns(&second.session_id).await[0].input_cursor.clone();
    let read = |cursors: Vec<Cursor>| {
        case.read(OwnerRead::ProjectMasterInputs {
            project_id: case.project.clone(),
            session_id: first.session_id.clone(),
            cursors,
        })
    };
    let OwnerData::ProjectMasterInputs { entries } =
        read(vec![other.clone(), own.clone()]).await.data
    else {
        panic!("the owner messages are read");
    };
    assert_eq!(
        entries.len(),
        1,
        "another session's message is not this session's history"
    );
    assert_eq!(entries[0].event_cursor, own);
    assert_eq!(entries[0].text, "Plan the first slice");

    let too_many = vec![own.clone(); usize::from(crate::conversation::PAGE_ITEMS_MAX) + 1];
    let (send, receive) = oneshot::channel();
    case.storage
        .sender
        .try_send(Job::OwnerRead(
            case.started.owners[0].admission.clone(),
            OwnerRead::ProjectMasterInputs {
                project_id: case.project.clone(),
                session_id: first.session_id.clone(),
                cursors: too_many,
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("owner read queue unavailable"));
    assert_eq!(
        receive.await.unwrap().unwrap_err().0,
        "master_inputs_bounds"
    );
    case.close().await;
}

#[tokio::test]
async fn a_master_turn_s_input_outlives_conversation_pruning() {
    let case = Case::new().await;
    assert_eq!(case.open("open-capacity").await, OwnerResult::Applied);
    let session = case.shown().await;
    assert_eq!(
        case.submit(
            "capacity-message",
            &session.session_id,
            "Plan the first slice"
        )
        .await,
        OwnerResult::Applied
    );
    let turn = case.turns(&session.session_id).await.remove(0);
    for n in 0..=crate::conversation::ENTRIES_MAX {
        let appended = case
            .apply(
                &format!("capacity-{n}"),
                OwnerMutation::Conversation {
                    mutation: ConversationMutation::AppendMessage {
                        project_id: case.project.clone(),
                        text: format!("filler {n}"),
                    },
                },
            )
            .await;
        assert_eq!(appended, OwnerResult::Applied);
    }
    let db = case.db();
    let oldest: Vec<String> = db
        .prepare(
            "SELECT text FROM conversation_entries WHERE project=?1 ORDER BY event_cursor LIMIT 3",
        )
        .unwrap()
        .query_map([case.project.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        oldest,
        ["Plan the first slice", "filler 1", "filler 2"],
        "the oldest plain message went and the turn's input stayed"
    );
    let total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM conversation_entries WHERE project=?1",
            [case.project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, crate::conversation::ENTRIES_MAX as i64 + 1);
    drop(db);
    assert_eq!(case.turns(&session.session_id).await, vec![turn.clone()]);
    let OwnerData::ProjectMasterInputs { entries } = case
        .read(OwnerRead::ProjectMasterInputs {
            project_id: case.project.clone(),
            session_id: session.session_id.clone(),
            cursors: vec![turn.input_cursor.clone()],
        })
        .await
        .data
    else {
        panic!("the owner messages are read");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "Plan the first slice");
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    assert_eq!(
        turn_message(&tx, &session, &turn).unwrap().as_deref(),
        Some("Plan the first slice")
    );
    drop(tx);
    case.close().await;
}

#[tokio::test]
async fn turns_whose_input_an_older_engine_pruned_still_read_and_end() {
    let case = Case::new().await;
    assert_eq!(case.open("open-pruned").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert_eq!(
        case.submit("pruned-message", &first.session_id, "Plan the first slice")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-pruned", true).await, OwnerResult::Applied);
    let second = case.shown().await;
    assert_eq!(
        case.submit("queued-message", &second.session_id, "Add a dark theme")
            .await,
        OwnerResult::Applied
    );
    let ended = case.turns(&first.session_id).await.remove(0);
    let queued = case.turns(&second.session_id).await.remove(0);
    assert_eq!(ended.phase, MasterTurnPhase::Cancelled);
    assert_eq!(queued.phase, MasterTurnPhase::Queued);
    let db = case.db();
    for turn in [&ended, &queued] {
        assert_eq!(
            db.execute(
                "DELETE FROM conversation_entries WHERE event_cursor=?1",
                [signed(turn.input_cursor.value()).unwrap()],
            )
            .unwrap(),
            1,
            "an older engine pruned the turn's input"
        );
    }
    drop(db);
    assert_eq!(case.turns(&first.session_id).await, vec![ended.clone()]);
    assert_eq!(case.turns(&second.session_id).await, vec![queued.clone()]);
    let second = case.shown().await;
    assert_eq!(second.current_turn, Some(queued.id.clone()));
    let OwnerData::ProjectMasterInputs { entries } = case
        .read(OwnerRead::ProjectMasterInputs {
            project_id: case.project.clone(),
            session_id: first.session_id.clone(),
            cursors: vec![ended.input_cursor.clone()],
        })
        .await
        .data
    else {
        panic!("the owner messages are read");
    };
    assert!(
        entries.is_empty(),
        "a pruned input is left out, not refused"
    );
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    assert_eq!(turn_message(&tx, &first, &ended).unwrap(), None);
    assert_eq!(
        prompt(&tx, &second, &queued).unwrap_err().0,
        "master_input_provenance",
        "a turn about to run still needs its message"
    );
    drop(tx);
    drop(db);
    assert_eq!(
        case.renew("renew-after-prune", true).await,
        OwnerResult::Applied,
        "the session still parks, cancelling its turn"
    );
    assert_eq!(
        case.turns(&second.session_id).await[0].phase,
        MasterTurnPhase::Cancelled
    );
    case.close().await;
}

/// D6: while the live master answers a turn in its terminal (`team_live_masters` live, an
/// agent and an open turn), New session and Resume are refused with `master_turn_running`
/// and change nothing, unless the owner confirmed stopping that reply; once the turn
/// closed, Resume applies.
#[tokio::test]
async fn a_live_master_turn_holds_new_session_and_resume_until_the_owner_stops_it() {
    let case = Case::new().await;
    assert_eq!(case.open("open-live").await, OwnerResult::Applied);
    let first = case.shown().await;
    let db = case.db();
    crate::team::master::initialize(&db).unwrap();
    crate::master_sessions::mark_owner_turn(&db, &case.project, first.session_id.as_str()).unwrap();
    db.execute(
        "INSERT INTO team_live_masters(project,live,agent,turn_open) VALUES (?1,1,7,1)
         ON CONFLICT(project) DO UPDATE SET live=1, agent=7, turn_open=1",
        [case.project.as_str()],
    )
    .unwrap();
    assert!(crate::team::master::turn_running(&db, &case.project).unwrap());

    let before = case.phases();
    assert_eq!(
        case.renew("renew-live-turn", false).await,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(
        case.phases(),
        before,
        "a refused New session changes nothing"
    );
    assert_eq!(
        case.renew("renew-live-turn-stopped", true).await,
        OwnerResult::Applied
    );
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    crate::master_sessions::mark_owner_turn(&db, &case.project, second.session_id.as_str())
        .unwrap();

    let before = case.phases();
    assert_eq!(
        case.resume("resume-live-turn", &first.session_id, false)
            .await,
        OwnerResult::Rejected {
            code: "master_turn_running".into()
        }
    );
    assert_eq!(case.phases(), before, "a refused Resume changes nothing");
    db.execute(
        "UPDATE team_live_masters SET turn_open=0 WHERE project=?1",
        [case.project.as_str()],
    )
    .unwrap();
    assert!(!crate::team::master::turn_running(&db, &case.project).unwrap());
    assert_eq!(
        case.resume("resume-closed-turn", &first.session_id, false)
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.shown().await.session_id, first.session_id);
}

fn listed_ids(listed: &[MasterSessionSummary]) -> Vec<SessionId> {
    listed
        .iter()
        .map(|summary| summary.session_id.clone())
        .collect()
}

fn count(case: &Case, sql: &str, session: &SessionId) -> i64 {
    case.db()
        .query_row(sql, [session.as_str()], |row| row.get(0))
        .unwrap()
}

fn keep_conversation(case: &Case, session: &SessionId, agent: i64, id: &str, record: &Path) {
    case.db()
        .execute(
            "INSERT INTO team_master_conversations(session,project,agent,harness,connection,conversation,record)
             VALUES (?1,?2,?3,'claude-code','native-connection',?4,?5)",
            rusqlite::params![
                session.as_str(),
                case.project.as_str(),
                agent,
                id,
                record.to_str().unwrap()
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn new_session_on_a_session_with_no_owner_turn_is_refused_and_blank_sessions_are_not_listed()
{
    let case = Case::new().await;
    assert_eq!(case.open("open-blank").await, OwnerResult::Applied);
    let first = case.shown().await;
    assert!(
        case.sessions().await.is_empty(),
        "a live session with no owner turn is not listed"
    );
    let before = snapshot(&case);
    for (id, cancel_running) in [("renew-blank", false), ("renew-blank-again", true)] {
        assert_eq!(
            case.renew(id, cancel_running).await,
            OwnerResult::Rejected {
                code: "master_session_unused".into()
            }
        );
    }
    assert_eq!(
        snapshot(&case),
        before,
        "a refused New session creates nothing"
    );
    assert!(case.sessions().await.is_empty());

    assert_eq!(
        case.submit("first-line", &first.session_id, "Draft the release notes")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![first.session_id.clone()]
    );
    assert_eq!(case.renew("renew-used", true).await, OwnerResult::Applied);
    let second = case.shown().await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![first.session_id.clone()],
        "the fresh live session is not listed before its first turn"
    );
    assert_eq!(
        case.renew("renew-fresh", true).await,
        OwnerResult::Rejected {
            code: "master_session_unused".into()
        }
    );
    assert_eq!(
        case.submit("second-line", &second.session_id, "Check the changelog")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![second.session_id.clone(), first.session_id.clone()]
    );
    assert_eq!(case.phases().len(), 2);
    case.close().await;
}

#[tokio::test]
async fn upgrading_keeps_sessions_used_only_in_the_live_terminal_listed() {
    let case = Case::new().await;
    assert_eq!(case.open("open-typed").await, OwnerResult::Applied);
    let typed = case.shown().await;
    assert_eq!(
        case.submit("typed-line", &typed.session_id, "Sketch the settings page")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        case.renew("renew-terminal", true).await,
        OwnerResult::Applied
    );
    let terminal = case.shown().await;
    let record = case
        .root
        .path()
        .join("team/agents/agent-3/home/terminal.jsonl");
    keep_conversation(
        &case,
        &terminal.session_id,
        3,
        "terminal-conversation",
        &record,
    );
    write_record(&record, "Plan the import page", IMPORT_REPLY);
    keep_conversation(
        &case,
        &typed.session_id,
        2,
        "typed-conversation",
        &case
            .root
            .path()
            .join("team/agents/agent-2/home/typed.jsonl"),
    );
    let project = case.project.clone();
    let root = case.close().await;
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch("DROP TABLE session_owner_turns").unwrap();
    drop(db);

    let (storage, started) = start(root.path()).await;
    let case = Case {
        storage,
        started,
        project,
        root,
    };
    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![terminal.session_id.clone(), typed.session_id.clone()],
        "a session that only kept a harness conversation stays listed after the upgrade"
    );
    assert_eq!(
        case.sessions()
            .await
            .iter()
            .map(|summary| summary.turns)
            .collect::<Vec<_>>(),
        vec![1, 1],
        "a kept conversation counts as one terminal turn only for a session with no engine turn"
    );
    assert_eq!(
        case.renew("renew-after-upgrade", true).await,
        OwnerResult::Applied,
        "the backfilled live session counts as used"
    );
    let fresh = case.shown().await;
    assert!(
        !listed_ids(&case.sessions().await).contains(&fresh.session_id),
        "a session with neither a turn nor a kept conversation is not listed"
    );
    assert_eq!(listed_ids(&case.sessions().await).len(), 2);
    let listed = case.sessions().await;
    let parked = summary_of(&listed, &terminal.session_id);
    assert_eq!(parked.phase, MasterSessionPhase::Parked);
    assert_eq!(
        parked.last_line.as_deref(),
        Some(IMPORT_REPLY),
        "a parked session used only in the terminal shows the newest reply of its kept conversation"
    );
    case.close().await;
}

const IMPORT_REPLY: &str = "The import plan is in docs/import.md.";

/// Writes a Claude Code conversation record of one typed line and one reply, as the harness
/// keeps it. Takes the record path, the typed line and the reply.
fn write_record(record: &Path, typed: &str, reply: &str) {
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let lines = [
        serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}}),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(record, text).unwrap();
}

fn summary_of<'a>(
    listed: &'a [MasterSessionSummary],
    session: &SessionId,
) -> &'a MasterSessionSummary {
    listed
        .iter()
        .find(|summary| &summary.session_id == session)
        .unwrap_or_else(|| panic!("{} is listed", session.as_str()))
}

/// N-D14: the Sessions list counts every turn run in a session's live terminal as a message,
/// added to its engine-run turns, also when an 18.14 engine recorded it in the two-column
/// table: a row of a session with no engine turn counts as the one turn it proves, a row of
/// a session with engine turns may be a kept conversation's backfill and adds none (ND14-L1),
/// and later turns count on.
#[tokio::test]
async fn terminal_turns_count_as_the_session_s_messages_also_when_recorded_on_18_14() {
    let case = Case::new().await;
    assert_eq!(case.open("open-terminal").await, OwnerResult::Applied);
    let terminal = case.shown().await;
    case.used_in_terminal(&terminal.session_id);
    case.used_in_terminal(&terminal.session_id);
    let listed = case.sessions().await;
    assert_eq!(listed_ids(&listed), vec![terminal.session_id.clone()]);
    assert_eq!(listed[0].phase, MasterSessionPhase::Configured);
    assert_eq!(listed[0].turns, 2, "each terminal turn counts once");
    assert_eq!(
        case.renew("renew-terminal", true).await,
        OwnerResult::Applied
    );
    let live = case.shown().await;
    assert_eq!(
        case.submit("typed-line", &live.session_id, "Outline the export page")
            .await,
        OwnerResult::Applied
    );
    case.used_in_terminal(&live.session_id);
    assert_eq!(
        turns_of(&case.sessions().await, &live.session_id),
        2,
        "an engine-run turn adds to the terminal turns"
    );
    let record = case
        .root
        .path()
        .join("team/agents/agent-4/home/terminal.jsonl");
    keep_conversation(
        &case,
        &terminal.session_id,
        4,
        "terminal-conversation",
        &record,
    );
    write_record(&record, "Plan the import page", IMPORT_REPLY);

    let project = case.project.clone();
    let root = case.close().await;
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(
        "DROP TABLE session_owner_turns;
         CREATE TABLE session_owner_turns (
             session TEXT PRIMARY KEY CHECK(length(session) BETWEEN 1 AND 128),
             project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128)
         );",
    )
    .unwrap();
    for session in [&terminal.session_id, &live.session_id] {
        db.execute(
            "INSERT INTO session_owner_turns(session,project) VALUES (?1,?2)",
            [session.as_str(), project.as_str()],
        )
        .unwrap();
    }
    drop(db);
    let (storage, started) = start(root.path()).await;
    let case = Case {
        storage,
        started,
        project,
        root,
    };
    let listed = case.sessions().await;
    assert_eq!(
        turns_of(&listed, &terminal.session_id),
        1,
        "the 18.14 row of a session with no engine turn counts as one terminal turn"
    );
    assert_eq!(
        turns_of(&listed, &live.session_id),
        1,
        "the 18.14 row of a session with engine turns adds no turn to them"
    );
    let parked = summary_of(&listed, &terminal.session_id);
    assert_eq!(parked.phase, MasterSessionPhase::Parked);
    assert_eq!(
        parked.last_line.as_deref(),
        Some(IMPORT_REPLY),
        "a parked session an 18.14 engine recorded from the terminal shows its newest reply"
    );
    case.used_in_terminal(&live.session_id);
    let listed = case.sessions().await;
    assert_eq!(listed.len(), 2);
    assert_eq!(
        turns_of(&listed, &live.session_id),
        2,
        "a later terminal turn counts on"
    );
    case.close().await;
}

fn turns_of(listed: &[MasterSessionSummary], session: &SessionId) -> u64 {
    listed
        .iter()
        .find(|summary| &summary.session_id == session)
        .unwrap_or_else(|| panic!("{} is listed", session.as_str()))
        .turns
}

#[tokio::test]
async fn delete_removes_a_parked_session_and_its_transcript_inside_the_master_home_only() {
    let case = Case::new().await;
    assert_eq!(case.open("open-kept").await, OwnerResult::Applied);
    let kept = case.shown().await;
    assert_eq!(
        case.submit("kept-line", &kept.session_id, "Write the import guide")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-linked", true).await, OwnerResult::Applied);
    let linked = case.shown().await;
    assert_eq!(
        case.submit("linked-line", &linked.session_id, "Review the export flow")
            .await,
        OwnerResult::Applied
    );
    assert_eq!(case.renew("renew-live", true).await, OwnerResult::Applied);
    let live = case.shown().await;
    assert_eq!(
        case.submit("live-line", &live.session_id, "Plan the next sprint")
            .await,
        OwnerResult::Applied
    );

    let root = case.root.path().to_path_buf();
    let home = crate::team::runtime::agent_home(&root.join("team"), 4);
    let projects = home.join(".claude/projects/-session-repository");
    std::fs::create_dir_all(projects.join("conversation-kept/subagents")).unwrap();
    let record = projects.join("conversation-kept.jsonl");
    std::fs::write(&record, "{\"type\":\"user\"}\n").unwrap();
    std::fs::write(
        projects.join("conversation-kept/subagents/agent-a.jsonl"),
        "{}\n",
    )
    .unwrap();
    let outside = root.join("outside/.claude/projects/-session-repository");
    std::fs::create_dir_all(outside.join("conversation-kept")).unwrap();
    std::fs::write(outside.join("conversation-kept.jsonl"), "outside\n").unwrap();
    std::fs::write(outside.join("conversation-linked.jsonl"), "outside\n").unwrap();
    keep_conversation(&case, &kept.session_id, 4, "conversation-kept", &record);
    std::os::unix::fs::symlink(&outside, home.join(".claude/projects/linked")).unwrap();
    let through_link = home.join(".claude/projects/linked/conversation-linked.jsonl");
    keep_conversation(
        &case,
        &linked.session_id,
        4,
        "conversation-linked",
        &through_link,
    );
    let inputs: Vec<i64> = {
        let db = case.db();
        let mut statement = db
            .prepare("SELECT input FROM master_turns WHERE session=?1")
            .unwrap();
        statement
            .query_map([kept.session_id.as_str()], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(inputs.len(), 1);

    let before = snapshot(&case);
    let linked_turns = count(
        &case,
        "SELECT COUNT(*) FROM master_turns WHERE session=?1",
        &linked.session_id,
    );
    assert_eq!(
        case.delete("delete-linked", &linked.session_id).await,
        OwnerResult::Rejected {
            code: "master_session_files_kept".into()
        }
    );
    assert_eq!(
        snapshot(&case),
        before,
        "a refused Delete changes no session"
    );
    assert_eq!(
        count(
            &case,
            "SELECT COUNT(*) FROM master_turns WHERE session=?1",
            &linked.session_id
        ),
        linked_turns
    );
    assert_eq!(
        count(
            &case,
            "SELECT COUNT(*) FROM team_master_conversations WHERE session=?1",
            &linked.session_id
        ),
        1
    );
    assert!(outside.join("conversation-linked.jsonl").is_file());
    assert_eq!(
        case.delete("delete-live", &live.session_id).await,
        OwnerResult::Rejected {
            code: "master_session_live".into()
        }
    );
    assert_eq!(snapshot(&case), before);

    assert_eq!(
        case.delete("delete-kept", &kept.session_id).await,
        OwnerResult::Applied
    );
    assert!(!record.exists(), "the kept record is removed");
    assert!(
        !projects.join("conversation-kept").exists(),
        "the same-id folder is removed"
    );
    assert!(projects.is_dir(), "the master home itself stays");
    assert_eq!(
        std::fs::read_to_string(outside.join("conversation-kept.jsonl")).unwrap(),
        "outside\n",
        "a same-named record outside the home is untouched"
    );
    assert!(outside.join("conversation-kept").is_dir());
    assert_eq!(
        listed_ids(&case.sessions().await),
        vec![live.session_id.clone(), linked.session_id.clone()]
    );
    let phases = case.phases();
    assert_eq!(
        phases[0],
        (kept.session_id.as_str().into(), "closed".into())
    );
    for table in [
        "SELECT COUNT(*) FROM master_turns WHERE session=?1",
        "SELECT COUNT(*) FROM master_preparations WHERE session=?1",
        "SELECT COUNT(*) FROM team_master_conversations WHERE session=?1",
        "SELECT COUNT(*) FROM session_owner_turns WHERE session=?1",
        "SELECT COUNT(*) FROM session_marks WHERE session=?1",
    ] {
        assert_eq!(count(&case, table, &kept.session_id), 0, "{table}");
    }
    let db = case.db();
    let messages: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM conversation_entries WHERE kind='message' AND event_cursor=?1",
            [inputs[0]],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);
    assert_eq!(
        messages, 0,
        "the deleted session's owner messages are removed"
    );
    assert_eq!(
        case.resume("resume-deleted", &kept.session_id, true).await,
        OwnerResult::Rejected {
            code: "master_session_closed".into()
        }
    );
    assert_eq!(
        case.delete("delete-again", &kept.session_id).await,
        OwnerResult::Applied,
        "deleting a deleted session again finds nothing left to remove"
    );
    case.close().await;
}
