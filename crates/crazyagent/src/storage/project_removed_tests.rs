use super::*;
use crate::engine::owner::{OwnerMutation, OwnerOperation, OwnerRequest, OwnerResult, Started};
use crate::project_setup::SetupAction;
use crate::project_setup::removed::event::{self, ProjectRemoved};
use crate::protocol::{Command, Cursor, PROTOCOL_VERSION, ProjectId, RequestId};
use crate::state_log::replay::{replay, rows};
use crate::state_log::{Aggregate, fence};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const REMOVED: &str = "removed";
const OTHER: &str = "other";

/// The second part of the removal's projection (v2.5 L14b): activity, coordination, GitHub
/// rule proposals, the IDE Run setting, the schedule and usage, on this module's store,
/// rebuilt from the log equal to the live store, and the engine's open writes off the log.
mod areas;
/// The removal's redaction of the project's ide events and its ide rows (v2.5 L7, #309 N1).
mod ide_events;

/// Each area the removal's projection clears, with the rows [`seed`] gives one project
/// there: three agents (stopped, failed, working), each with a pane, own instructions and
/// sight of the hidden folder, and one row in every other area.
const SEEDED: [(&str, i64); 11] = [
    ("agent_texts", 3),
    ("questions_log", 1),
    ("project_hidden_paths", 1),
    ("team_hidden_seers", 3),
    ("project_agent_policy", 1),
    ("access_grant_log", 1),
    ("access_requests", 1),
    ("team_panes", 3),
    ("team_agents", 3),
    ("onboarding", 1),
    ("repository_admissions", 1),
];

/// An engine store opened as the engine opens it, with its owner started and its host
/// running in this process, holding the projects [`REMOVED`] and [`OTHER`].
struct Store {
    root: tempfile::TempDir,
    path: PathBuf,
    config: EffectiveConfig,
    db: Connection,
    _lock: EngineLock,
    started: Started,
}

/// Opens a fresh [`Store`]. Output: the store.
fn store() -> Store {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("db");
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (mut db, lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    fence::host_started(&started.host);
    for project in [REMOVED, OTHER] {
        let id = ProjectId::new(project).unwrap();
        let body = serde_json::to_string(&crate::protocol::StoredProject {
            project_id: id,
            spec: crate::protocol::ProjectSpec {
                name: project.into(),
                repositories: vec![],
            },
        })
        .unwrap();
        db.execute(
            "INSERT INTO projects(id, body) VALUES (?1, ?2)",
            params![project, body],
        )
        .unwrap();
    }
    Store {
        root,
        path,
        config,
        db,
        _lock: lock,
        started,
    }
}

/// Gives a project the rows of [`SEEDED`] through the writers that own them, or as their
/// tables hold them once created. Inputs: the store, the project and its first agent id.
/// Output: none.
fn seed(db: &mut Connection, project: &str, first: i64) {
    let id = ProjectId::new(project).unwrap();
    let agents = [
        (first, "stopped"),
        (first + 1, "failed"),
        (first + 2, "working"),
    ];
    let tx = db.transaction().unwrap();
    for (agent, status) in agents {
        tx.execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (?1,?2,'team','codex',1,?3,0,0)",
            params![agent, project, status],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO team_panes(agent, record) VALUES (?1, '{}')",
            [agent],
        )
        .unwrap();
        let owner = crate::storage::versioned::Writer::Owner {
            force_after_conflict: false,
        };
        crate::storage::agent_text::save(&tx, &id, agent, "Keep answers short.\n", 0, &owner)
            .unwrap();
    }
    crate::questions::forget_project(&tx, &id).unwrap();
    tx.execute(
        "INSERT INTO questions_log(project,asker,source,question,options_json,status,turn_floor,created_ms)
         VALUES (?1,'master','master','Ship it?','[]','pending',0,1)",
        [project],
    )
    .unwrap();
    crate::sandbox::hidden_paths::save(&tx, project, &[PathBuf::from("/hidden")]).unwrap();
    crate::sandbox::policy::remove(&tx, project).unwrap();
    tx.execute(
        "INSERT INTO project_agent_policy(project, body) VALUES (?1, '{}')",
        [project],
    )
    .unwrap();
    crate::sandbox::grants::forget_project(&tx, project).unwrap();
    tx.execute(
        "INSERT INTO access_grant_log(project,at_ms,action,grant_json,by)
         VALUES (?1,1,'granted','{}','owner')",
        [project],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO access_requests(project,asker,grant_json,reason,created_ms)
         VALUES (?1,'agent','{}','read',1)",
        [project],
    )
    .unwrap();
    crate::onboarding::initialize(&tx).unwrap();
    tx.execute(
        "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
         VALUES (?1,?2,'pool','done','done','{}',1,1)",
        params![project, format!("/repo/{project}")],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO repository_admissions(project,repository,body) VALUES (?1,'/repo','{}')",
        [project],
    )
    .unwrap();
    tx.commit().unwrap();
    let seers = BTreeSet::from([first, first + 1, first + 2]);
    crate::sandbox::hidden_paths::set_seers(db, project, &seers).unwrap();
}

/// The rows a project holds in each area of [`SEEDED`], its panes counted by its agent ids.
/// Inputs: the store, the project and its first agent id. Output: (table, rows) per area.
fn held(db: &Connection, project: &str, first: i64) -> Vec<(&'static str, i64)> {
    SEEDED
        .iter()
        .map(|(table, _)| {
            let count = if *table == "team_panes" {
                db.query_row(
                    "SELECT count(*) FROM team_panes WHERE agent BETWEEN ?1 AND ?1 + 2",
                    [first],
                    |row| row.get(0),
                )
            } else {
                db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE project=?1"),
                    [project],
                    |row| row.get(0),
                )
            };
            (*table, count.unwrap())
        })
        .collect()
}

/// What a removed project keeps of [`SEEDED`]: its working agent and that agent's pane.
/// Output: (table, rows) per area.
fn kept_after_removal() -> Vec<(&'static str, i64)> {
    SEEDED
        .iter()
        .map(|(table, _)| {
            let live = matches!(*table, "team_agents" | "team_panes");
            (*table, i64::from(live))
        })
        .collect()
}

/// Every logged event as seq, aggregate, kind, revision and body; none when the store has
/// no log yet. Input: the store. Output: the events in seq order.
fn logged(db: &Connection) -> Vec<(i64, String, String, i64, Vec<u8>)> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return Vec::new();
    }
    db.prepare("SELECT seq, aggregate, kind, revision, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// An owner request for one mutation at the store's current revision. Inputs: the store,
/// the request id and the mutation. Output: the request.
fn request(db: &mut Connection, id: &str, mutation: OwnerMutation) -> OwnerRequest {
    let revision = owner_revision(&db.transaction().unwrap()).unwrap();
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation: OwnerOperation::Apply { mutation },
    }
}

/// The owner's Remove from dashboard of [`REMOVED`]. Input: the store. Output: the request.
fn remove_request(db: &mut Connection) -> OwnerRequest {
    let project_id = ProjectId::new(REMOVED).unwrap();
    request(
        db,
        "remove-project",
        OwnerMutation::RemoveProject { project_id },
    )
}

/// Replays the live store's removals into a copy taken before them, skipping one seq when
/// asked, as a projection that misses an event would. Inputs: the live store, the copy and
/// the seq to skip. Output: the copy with the log applied.
fn rebuild(live: &Connection, copy: &Path, skip: Option<u64>) -> Connection {
    let mut rebuilt = Connection::open(copy).unwrap();
    let tx = rebuilt.transaction().unwrap();
    replay(live, Aggregate::Projects, 0, |logged| {
        assert_eq!(logged.kind, event::KIND);
        if Some(logged.seq) == skip {
            return Ok(());
        }
        event::apply(&tx, &logged.body)
    })
    .unwrap();
    tx.commit().unwrap();
    rebuilt
}

/// v2.5 L14a, L1-N1 a: a removal's body is its project and whether its notes are kept,
/// nothing else, so it stays small whatever the project held; only an applied Remove from
/// dashboard or a Delete everything with its effect is a removal; and the projection refuses
/// a body that is not one before it deletes anything.
#[test]
fn a_project_removed_body_holds_only_its_project_and_whether_notes_are_kept() {
    let id = ProjectId::new(REMOVED).unwrap();
    let removed = ProjectRemoved {
        project: id.clone(),
        kept: true,
    };
    assert_eq!(
        removed.body().unwrap(),
        br#"{"project":"removed","kept":true}"#
    );
    let setup = |action| Command::ProjectSetup { action };
    let remove = setup(SetupAction::Remove {
        project_id: id.clone(),
    });
    let delete = setup(SetupAction::Delete {
        project_id: id.clone(),
        name: REMOVED.into(),
    });
    assert_eq!(event::removal(&remove, false), Some(removed.clone()));
    assert_eq!(
        event::removal(&delete, true),
        Some(ProjectRemoved {
            project: id.clone(),
            kept: false,
        })
    );
    assert_eq!(event::removal(&delete, false), None, "no effect, no delete");
    assert_eq!(event::removal(&remove, true), None);
    let other = setup(SetupAction::CancelResearch {
        project_id: id,
        job: 1,
    });
    assert_eq!(event::removal(&other, false), None);

    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    let tx = s.db.transaction().unwrap();
    for body in [
        &br#"{"project":"removed","kept":true,"folder":"/x"}"#[..],
        br#"{"project":"no spaces","kept":true}"#,
        b"{}",
        b"not json",
    ] {
        assert_eq!(
            event::apply(&tx, body)
                .err()
                .map(|error| error.0)
                .as_deref(),
            Some("project_removed_body"),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    drop(tx);
    assert_eq!(held(&s.db, REMOVED, 10), SEEDED.to_vec());
}

/// v2.5 L14a: Remove from dashboard logs one `ProjectRemoved` at the request's new
/// revision, with the notes kept, and its projection leaves the project no row in any area
/// but its working agent's; the other project keeps all of its rows. Replaying the log into
/// a copy taken before the removal rebuilds every area equal, and a projection that skips
/// the removal does not.
#[test]
fn remove_from_dashboard_logs_project_removed_and_its_replay_leaves_no_row_of_the_project() {
    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    seed(&mut s.db, OTHER, 20);
    let copy = s.root.path().join("copy.db");
    let missed = s.root.path().join("missed.db");
    for path in [&copy, &missed] {
        s.db.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
    }
    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply(&mut s.db, &s.config, &s.path, admission, &request).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);

    let events = logged(&s.db);
    assert_eq!(events.len(), 1, "one removal, one event");
    let (seq, aggregate, kind, revision, body) = &events[0];
    assert_eq!(
        (aggregate.as_str(), kind.as_str(), *revision as u64),
        ("projects", event::KIND, applied.current_revision.value())
    );
    assert_eq!(body.as_slice(), br#"{"project":"removed","kept":true}"#);
    assert_eq!(held(&s.db, REMOVED, 10), kept_after_removal());
    assert_eq!(held(&s.db, OTHER, 20), SEEDED.to_vec());

    let rebuilt = rebuild(&s.db, &copy, None);
    for (table, _) in SEEDED {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(&s.db, table).unwrap(),
            "{table} rebuilds equal"
        );
    }
    let skipped = rebuild(&s.db, &missed, Some(*seq as u64));
    assert!(
        SEEDED
            .iter()
            .any(|(table, _)| rows(&skipped, table).unwrap() != rows(&s.db, table).unwrap()),
        "a projection that skips the removal must not rebuild equal"
    );
}

/// v2.5 L14a: Delete everything, its disk effect done, logs one `ProjectRemoved` with the
/// notes forgotten, and the projection clears the same areas.
#[test]
fn delete_everything_logs_project_removed_with_its_notes_forgotten() {
    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    let project_id = ProjectId::new(REMOVED).unwrap();
    let mutation = OwnerMutation::DeleteProject {
        project_id,
        name: REMOVED.into(),
    };
    let request = request(&mut s.db, "delete-project", mutation);
    let admission = &s.started.owners[0].admission;
    let done = Ok(Vec::new());
    let applied = owner_apply_with(
        &mut s.db,
        &s.config,
        &s.path,
        admission,
        &request,
        Some(Effect::Delete(&done)),
    )
    .unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    let events = logged(&s.db);
    assert_eq!(events.len(), 1, "one removal, one event");
    let (_, aggregate, kind, revision, body) = &events[0];
    assert_eq!(
        (aggregate.as_str(), kind.as_str(), *revision as u64),
        ("projects", event::KIND, applied.current_revision.value())
    );
    assert_eq!(body.as_slice(), br#"{"project":"removed","kept":false}"#);
    assert_eq!(held(&s.db, REMOVED, 10), kept_after_removal());
}

/// v2.5 L14a: a removal the engine refuses, of an unknown project or at a stale revision,
/// logs nothing and deletes nothing.
#[test]
fn a_refused_or_stale_removal_logs_nothing() {
    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    let admission = s.started.owners[0].admission.clone();
    let project_id = ProjectId::new("absent").unwrap();
    let unknown = request(
        &mut s.db,
        "remove-absent",
        OwnerMutation::RemoveProject { project_id },
    );
    let refused = owner_apply(&mut s.db, &s.config, &s.path, &admission, &unknown).unwrap();
    assert_eq!(
        refused.result,
        OwnerResult::Rejected {
            code: "invalid_project".into()
        }
    );
    let mut stale = remove_request(&mut s.db);
    stale.expected_revision = Cursor::new(stale.expected_revision.value() + 1);
    let refused = owner_apply(&mut s.db, &s.config, &s.path, &admission, &stale).unwrap();
    assert_eq!(
        refused.result,
        OwnerResult::Rejected {
            code: "stale_revision".into()
        }
    );
    assert!(logged(&s.db).is_empty());
    assert_eq!(held(&s.db, REMOVED, 10), SEEDED.to_vec());
}

/// v2.5 L14a, L3: a removal in a process whose engine host is not running cannot append its
/// event, so the whole request is refused: the project, its rows and the log stay as they
/// were.
#[test]
fn a_removal_without_its_engine_host_here_is_refused_and_changes_nothing() {
    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    fence::host_stopped(&s.started.host);
    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let refused = owner_apply(&mut s.db, &s.config, &s.path, admission, &request);
    assert_eq!(
        refused.err().map(|error| error.0).as_deref(),
        Some("state_log_fence_out_of_process")
    );
    assert!(logged(&s.db).is_empty());
    assert_eq!(held(&s.db, REMOVED, 10), SEEDED.to_vec());
    let present: bool =
        s.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [REMOVED],
            |row| row.get(0),
        )
        .unwrap();
    assert!(present, "the refused removal keeps the project");
}

/// v2.5 L6 Q10: a removal tombstones the removed project's `RepositoryAdmitted` events,
/// keeping the other project's and its own `ProjectRemoved`, and the worktrees replay brings
/// back no admission of the removed project, even from a snapshot run taken before the
/// removal that still holds it, since replay applies the removal's admissions delete at its
/// place in the log. The other project's admission replays as it stands.
#[test]
fn a_removal_tombstones_its_project_s_admissions_and_their_replay_brings_none_back() {
    use crate::worktrees::log::{self, Admitted, Event};
    let mut s = store();
    let engine = || fence::Fence::Engine(s.started.host.clone());
    let tx = s.db.transaction().unwrap();
    let mut appender = crate::state_log::Appender::own(&tx, engine()).unwrap();
    for project in [REMOVED, OTHER] {
        let admitted = Admitted {
            project: project.into(),
            repository: format!("/repo/{project}"),
            body: "{}".into(),
        };
        log::record(&mut appender, 1, &Event::Admitted(admitted)).unwrap();
    }
    tx.commit().unwrap();
    let tx = s.db.transaction().unwrap();
    let mut appender = crate::state_log::Appender::own(&tx, engine()).unwrap();
    log::snapshot(&mut appender, 2).unwrap();
    tx.commit().unwrap();

    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply(&mut s.db, &s.config, &s.path, admission, &request).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);

    let events = logged(&s.db);
    let admitted: Vec<&Vec<u8>> = events
        .iter()
        .filter(|(_, _, kind, _, _)| kind == log::KIND_ADMITTED)
        .map(|(_, _, _, _, body)| body)
        .collect();
    assert_eq!(admitted.len(), 2);
    assert_eq!(admitted[0].as_slice(), crate::state_log::TOMBSTONE);
    assert!(String::from_utf8_lossy(admitted[1]).contains("/repo/other"));
    let (_, _, kind, _, body) = events.last().unwrap();
    assert_eq!(kind, event::KIND, "the removal keeps its own event");
    assert_ne!(body.as_slice(), crate::state_log::TOMBSTONE);

    let live = rows(&s.db, "repository_admissions").unwrap();
    assert_eq!(live.len(), 1, "only the other project's admission stands");
    let tx = s.db.transaction().unwrap();
    log::rebuild(&tx).unwrap();
    assert_eq!(rows(&tx, "repository_admissions").unwrap(), live);
}
