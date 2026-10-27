use super::*;
use crate::config::{EffectiveConfig, resolve_config};
use crate::engine::Engine;
use crate::engine::owner::OwnerMutation;
use crate::memory::{
    ActivityInput, CaptureCategory, KnowledgeAction, KnowledgeRole, NominationInput, Principal,
    ReviewDecision,
};
use crate::protocol::{
    BaselineId, ClientId, Command, CredentialRef, Cursor, ExecutionContext, PROTOCOL_VERSION,
    ProjectSpec, SessionId, SnapshotId, TaskId, WireRequest,
};
use std::os::unix::fs::PermissionsExt;

fn id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

struct Store {
    root: tempfile::TempDir,
    engine: Engine,
    config: EffectiveConfig,
    revision: u64,
    sequence: u64,
}

impl Store {
    async fn open() -> Self {
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.path().join("g"), "allowed_destinations=['local']\n").unwrap();
        let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap();
        Self {
            root,
            engine,
            config,
            revision: 0,
            sequence: 0,
        }
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("db")).unwrap()
    }

    async fn command(&mut self, command: Command) -> Outcome {
        self.sequence += 1;
        let reply = self
            .engine
            .apply(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("ide-sidebar").unwrap(),
                request_id: id(&format!("op-{}", self.sequence)),
                expected_revision: Cursor::new(self.revision),
                command,
            })
            .await
            .unwrap();
        self.revision = reply.revision.value();
        reply.outcome
    }

    async fn applied(&mut self, action: KnowledgeAction) {
        assert_eq!(
            self.command(Command::Knowledge { action }).await,
            Outcome::Applied
        );
    }

    async fn read(&mut self, query: OwnerRead) -> OwnerData {
        let command = query.command().unwrap().unwrap();
        OwnerData::Domain {
            outcome: Box::new(self.command(command).await),
        }
    }

    async fn write(&mut self, mutation: OwnerMutation) -> Outcome {
        self.command(mutation.command().unwrap()).await
    }

    async fn admit(&mut self, project_id: &str, principal: &str, role: KnowledgeRole) {
        let project_id = project(project_id);
        let task_id = TaskId::new(principal).unwrap();
        let created = self
            .command(Command::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: project_id.as_str().into(),
                    repositories: vec![],
                },
            })
            .await;
        assert!(created == Outcome::Applied || matches!(created, Outcome::Rejected { .. }));
        let pinned = self
            .command(Command::PinTask {
                context: ExecutionContext {
                    project_id: project_id.clone(),
                    task_id: task_id.clone(),
                    config_id: self.config.id.clone(),
                    snapshot_id: SnapshotId::new("snapshot").unwrap(),
                    baseline_id: BaselineId::new("baseline").unwrap(),
                    allowed_destinations: self.config.settings.allowed_destinations.clone(),
                    budget_ref: CredentialRef::new("budget").unwrap(),
                },
            })
            .await;
        assert_eq!(pinned, Outcome::Applied);
        self.applied(KnowledgeAction::Admit {
            principal: Principal {
                id: id(principal),
                project_id,
                task_id,
                session_id: SessionId::new(principal).unwrap(),
                role,
            },
        })
        .await;
    }

    async fn seed(&mut self, project_id: &str, entries: &[&str]) {
        let worker = format!("{project_id}-worker");
        let curator = format!("{project_id}-curator");
        let activity = format!("{project_id}-activity");
        let nomination = format!("{project_id}-nomination");
        self.admit(project_id, &worker, KnowledgeRole::Contributor)
            .await;
        self.admit(project_id, &curator, KnowledgeRole::Curator)
            .await;
        self.applied(KnowledgeAction::Capture {
            principal_id: id(&worker),
            activity: ActivityInput {
                id: id(&activity),
                source_id: id(&format!("{project_id}-source")),
                category: CaptureCategory::ToolOutput,
                text: "the build runs with make check".into(),
                gaps: vec!["native private reasoning unavailable".into()],
            },
        })
        .await;
        self.applied(KnowledgeAction::Nominate {
            principal_id: id(&worker),
            nomination: NominationInput {
                id: id(&nomination),
                activity_ids: vec![id(&activity)],
                text: "candidate build fact".into(),
            },
        })
        .await;
        for entry in entries {
            self.applied(KnowledgeAction::Publish {
                principal_id: id(&curator),
                nomination_id: id(&nomination),
                entry_id: id(entry),
                text: format!("curated fact {entry}"),
            })
            .await;
        }
    }

    async fn memories(&mut self, project_id: &ProjectId, after: Option<&str>) -> Vec<IdeMemory> {
        let data = self.read(list(project_id, after.map(id))).await;
        listed(project_id, &data).unwrap()
    }

    async fn plan(
        &mut self,
        project_id: &ProjectId,
        decide: &Decide,
    ) -> Result<Plan, &'static str> {
        let data = self.read(check(decide).unwrap()).await;
        plan(project_id, decide, &data)
    }
}

fn decide(id: &str, revision: u64, decision: Decision) -> Decide {
    Decide {
        id: self::id(id),
        revision,
        decision,
    }
}

/// The sidebar's Approve and Reject never write from the IDE: each becomes an ask, with its
/// decision, that the desktop app shows until the owner's own review moves that revision,
/// and an ask expires. A decision on an older revision writes nothing and returns the stale
/// reply with the current revision and its writer, and another project's memory is
/// unknown.
#[tokio::test]
async fn ide_memory_approval_uses_engine_approval_with_the_revision() {
    let mut store = Store::open().await;
    store.seed("ide-p", &["fact", "other"]).await;
    store.seed("ide-q", &["q-fact"]).await;
    let p = project("ide-p");
    let q = project("ide-q");

    let shown = store.memories(&p, None).await;
    assert_eq!(
        shown,
        [
            IdeMemory {
                id: "fact".into(),
                revision: 1,
                text: "curated fact fact".into(),
                state: MemoryState::Pending,
                reviewed: false,
            },
            IdeMemory {
                id: "other".into(),
                revision: 1,
                text: "curated fact other".into(),
                state: MemoryState::Pending,
                reviewed: false,
            },
        ]
    );
    let next = store.memories(&p, Some("fact")).await;
    assert_eq!(
        next.iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>(),
        ["other"]
    );

    let approve = decide("fact", 1, Decision::Approve);
    assert_eq!(
        store.plan(&p, &approve).await,
        Ok(Plan::Ask {
            entry_id: id("fact"),
            version: 1,
            decision: Decision::Approve,
        }),
        "an approval from the IDE is only asked, never written"
    );
    assert_eq!(store.memories(&p, None).await, shown);
    let database = store.database();
    let now = 1_000_000;
    ask(&database, &p, &id("fact"), 1, Decision::Reject, now).unwrap();
    ask(&database, &p, &id("fact"), 1, Decision::Approve, now + 1).unwrap();
    let waiting = asks(&database, &p, now + 2).unwrap();
    assert_eq!(
        waiting
            .iter()
            .map(|ask| {
                (
                    ask.entry.id.as_str(),
                    ask.entry.version,
                    ask.entry.text.as_str(),
                    ask.decision,
                )
            })
            .collect::<Vec<_>>(),
        [("fact", 1, "curated fact fact", Decision::Approve)],
        "a newer ask for the same memory replaces the older one"
    );
    assert!(asks(&database, &q, now + 2).unwrap().is_empty());
    assert!(
        asks(&database, &p, now + 1 + ASK_TTL_MS + 1)
            .unwrap()
            .is_empty(),
        "an ask expires"
    );
    let many: Vec<String> = (0..=ASKS_PER_PROJECT_MAX)
        .map(|n| format!("r{n}"))
        .collect();
    store
        .seed(
            "ide-r",
            &many.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await;
    let r = project("ide-r");
    for (n, entry) in many.iter().enumerate() {
        ask(
            &database,
            &r,
            &id(entry),
            1,
            Decision::Approve,
            now + i64::try_from(n).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        asks(&database, &r, now + 100)
            .unwrap()
            .iter()
            .map(|ask| ask.entry.id.as_str().to_owned())
            .collect::<Vec<_>>(),
        many[1..],
        "a project keeps its newest asks only"
    );

    let confirmed = OwnerMutation::Knowledge {
        review: HumanReview::Review {
            entry_id: id("fact"),
            version: 1,
            decision: ReviewDecision::Keep,
        },
    };
    assert_eq!(store.write(confirmed).await, Outcome::Applied);
    let approved = store.memories(&p, None).await;
    assert_eq!(approved[0].revision, 2);
    assert_eq!(approved[0].state, MemoryState::Approved);
    assert!(approved[0].reviewed);
    assert!(
        asks(&database, &p, now + 2).unwrap().is_empty(),
        "an ask the owner confirmed leaves the desktop list"
    );

    let late = decide("fact", 1, Decision::Reject);
    let Plan::Stale(stale) = store.plan(&p, &late).await.unwrap() else {
        panic!("an older revision is stale");
    };
    assert_eq!(
        serde_json::to_value(stale_reply(&stale)).unwrap(),
        serde_json::json!({
            "kind": "stale",
            "code": "state_stale",
            "version": 1,
            "current": 2,
            "writer": "owner",
        })
    );
    assert_eq!(
        store.memories(&p, None).await[0],
        approved[0],
        "a stale decision writes nothing"
    );

    let reject = decide("other", 1, Decision::Reject);
    assert_eq!(
        store.plan(&p, &reject).await,
        Ok(Plan::Ask {
            entry_id: id("other"),
            version: 1,
            decision: Decision::Reject,
        }),
        "a rejection from the IDE is only asked too"
    );
    ask(&database, &p, &id("other"), 1, Decision::Reject, now + 3).unwrap();
    assert_eq!(
        asks(&database, &p, now + 4)
            .unwrap()
            .iter()
            .map(|ask| (ask.entry.id.as_str(), ask.decision))
            .collect::<Vec<_>>(),
        [("other", Decision::Reject)]
    );
    assert_eq!(
        store.memories(&p, None).await[1].state,
        MemoryState::Pending,
        "an asked rejection writes nothing"
    );
    let refused = OwnerMutation::Knowledge {
        review: HumanReview::Review {
            entry_id: id("other"),
            version: 1,
            decision: ReviewDecision::Reject,
        },
    };
    assert_eq!(store.write(refused).await, Outcome::Applied);
    assert!(
        asks(&database, &p, now + 4).unwrap().is_empty(),
        "a rejection the owner confirmed leaves the desktop list"
    );
    assert_eq!(
        store.plan(&p, &reject).await,
        Ok(Plan::Stale(Stale {
            version: 1,
            current: 2,
            writer: Writer::Owner,
        }))
    );
    assert_eq!(
        store.memories(&p, None).await[1].state,
        MemoryState::Rejected
    );

    assert_eq!(
        store
            .plan(&p, &decide("q-fact", 1, Decision::Approve))
            .await,
        Err(UNKNOWN),
        "another project's memory is not this project's to decide"
    );
    assert_eq!(
        store.memories(&q, None).await[0].state,
        MemoryState::Pending
    );
    assert_eq!(
        store.plan(&p, &decide("fact", 9, Decision::Approve)).await,
        Err(UNKNOWN)
    );
    assert_eq!(check(&decide("fact", 0, Decision::Approve)), Err(INVALID));
    store.engine.close().await.unwrap();
}

/// A table an earlier build made without the decision column gains it at the next engine
/// open: the old asks read as approvals, and a new ask records its own decision.
#[tokio::test]
async fn an_ask_table_from_an_earlier_build_gains_the_decision_column() {
    let mut store = Store::open().await;
    store.seed("ide-old", &["fact"]).await;
    let old = project("ide-old");
    let database = store.database();
    let now = 1_000_000;
    database
        .execute_batch(
            "DROP TABLE ide_asks;
             CREATE TABLE ide_asks (
                project TEXT NOT NULL,
                entry TEXT NOT NULL,
                version INTEGER NOT NULL CHECK (version > 0),
                asked_ms INTEGER NOT NULL,
                PRIMARY KEY (project, entry)
             ) WITHOUT ROWID;",
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO ide_asks(project,entry,version,asked_ms) VALUES ('ide-old','fact',1,?1)",
            [now],
        )
        .unwrap();

    initialize(&database).unwrap();
    initialize(&database).unwrap();
    let decided = |database: &rusqlite::Connection, at: i64| {
        asks(database, &old, at)
            .unwrap()
            .iter()
            .map(|ask| (ask.entry.id.as_str().to_owned(), ask.decision))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        decided(&database, now + 1),
        [("fact".to_owned(), Decision::Approve)],
        "an ask from the earlier build reads as the approval it was"
    );
    ask(&database, &old, &id("fact"), 1, Decision::Reject, now + 2).unwrap();
    assert_eq!(
        decided(&database, now + 3),
        [("fact".to_owned(), Decision::Reject)]
    );
    store.engine.close().await.unwrap();
}
