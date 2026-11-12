use super::reconcile::{Ticker, reconcile};
use super::*;
use crate::config::{
    ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::protocol::CredentialRef;
use rusqlite::{Connection, params};

const NOW_MS: i64 = 1_800_000_000_000;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    crate::landing::open(folder.path()).unwrap();
    (folder, connection)
}

fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn nobody() -> ModelAccess {
    ModelAccess::default()
}

fn native(id: &str) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("anthropic"),
        harness: reference("claude"),
        protocol: reference("native"),
        account_ref: reference(&format!("{id}-account")),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("anthropic"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference(&format!("{id}-profile")),
        },
    }
}

fn pending(connection: &Connection) -> Vec<(NoticeKind, Option<ProjectId>, String)> {
    page(
        connection,
        &NoticeQuery {
            filter: NoticeFilter::Pending,
            project_id: None,
            kind: None,
            fold: false,
            before: None,
            after: None,
            limit: PAGE_MAX,
        },
    )
    .unwrap()
    .items
    .into_iter()
    .map(|item| (item.kind, item.project_id, item.text))
    .collect()
}

fn all(connection: &Connection) -> Vec<Notice> {
    page(
        connection,
        &NoticeQuery {
            filter: NoticeFilter::All,
            project_id: None,
            kind: None,
            fold: false,
            before: None,
            after: None,
            limit: PAGE_MAX,
        },
    )
    .unwrap()
    .items
}

fn ask(connection: &Connection, project_id: &str, coord: Option<i64>, text: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO questions_log(project,asker,source,question,options_json,status,turn_floor,created_ms,coord_message)
             VALUES (?1,'Master','master',?2,'[]','pending',0,?3,?4)",
            params![project_id, text, NOW_MS, coord],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn research(connection: &Connection, project_id: &str, status: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms)
             VALUES (?1,'competitor_research','claude-code',0,?2,?3)",
            params![project_id, status, NOW_MS],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn change_set(connection: &Connection, id: &str, project_id: &str, state: &str) {
    connection
        .execute(
            "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,worktree,
                git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,removed,conflicts,
                created_ms,updated_ms)
             VALUES (?1,?2,'sub','4','Sub-agent 4','/repo',?1,'/repo/.git','b','h',?3,'succeeded',
                't','Add the login form',3,10,2,'[]',?4,?4)",
            params![id, project_id, state, NOW_MS],
        )
        .unwrap();
}

fn suggest(connection: &Connection, id: &str, project_id: &str, reviewed: bool) {
    let text = format!("Memory {id}");
    let entry = crate::memory::KnowledgeEntry {
        id: crate::protocol::RequestId::new(id).unwrap(),
        project_id: project(project_id),
        version: 1,
        content_hash: crate::memory::hash(text.as_bytes()),
        text,
        status: crate::memory::KnowledgeStatus::Provisional,
        nomination_id: crate::protocol::RequestId::new(format!("{id}-nomination")).unwrap(),
        activity_ids: vec![crate::protocol::RequestId::new(format!("{id}-activity")).unwrap()],
        source_ids: vec![crate::protocol::RequestId::new(format!("{id}-source")).unwrap()],
        curator_session: crate::protocol::SessionId::new("curator").unwrap(),
        config_id: crate::protocol::ConfigId::new("c".repeat(64)).unwrap(),
        revision: crate::protocol::Cursor::new(1),
        human_reviewed: reviewed,
    };
    connection
        .execute(
            "INSERT INTO knowledge_entries VALUES (?1,1,?2,?3)",
            params![id, project_id, serde_json::to_string(&entry).unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO knowledge_heads VALUES (?1,1,?2)",
            params![id, project_id],
        )
        .unwrap();
}

fn activity(
    connection: &Connection,
    project_id: &str,
    kind: crate::activity::ActivityKind,
    at_ms: i64,
) {
    crate::activity::record(
        connection,
        &project(project_id),
        &crate::activity::NewActivity {
            kind,
            actor: "Sub-agent 2",
            agent: None,
            goal: None,
            text: "Sub-agent 2 finished Add the login form",
            change_set: None,
        },
        at_ms,
    )
    .unwrap();
}

#[test]
fn a_pending_question_opens_a_question_item_and_answering_it_resolves_the_item() {
    let (_folder, mut connection) = engine();
    let id = ask(&connection, "p1", None, "Which database should we use?");
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let items = all(&connection);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, NoticeKind::Question);
    assert!(items[0].text.contains("Which database should we use?"));
    assert_eq!(
        items[0].target,
        NoticeTarget::Questions {
            project_id: project("p1"),
            question: Some(id),
            agent: None,
        }
    );
    connection
        .execute(
            "UPDATE questions_log SET status='answered' WHERE id=?1",
            [id],
        )
        .unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    assert!(pending(&connection).is_empty());
    assert_eq!(all(&connection)[0].resolved_ms, Some(NOW_MS + 1));
}

#[test]
fn a_team_question_keeps_its_item_when_it_is_linked_into_the_questions_log() {
    let (_folder, mut connection) = engine();
    let first = ask(&connection, "p1", Some(41), "Ship on Friday?");
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let before = all(&connection);
    connection
        .execute(
            "UPDATE questions_log SET status='dismissed' WHERE id=?1",
            [first],
        )
        .unwrap();
    ask(&connection, "p1", Some(41), "Ship on Friday?");
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    let after = all(&connection);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].resolved_ms, None);
}

#[test]
fn research_waiting_for_a_pool_opens_one_item_per_project_until_it_can_run() {
    let (_folder, mut connection) = engine();
    let one = research(&connection, "p1", "needs_pool");
    let two = research(&connection, "p1", "needs_pool");
    research(&connection, "p2", "queued");
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let open = pending(&connection);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].0, NoticeKind::ResearchPool);
    assert_eq!(open[0].1, Some(project("p1")));
    assert!(open[0].2.starts_with("2 "));
    assert_eq!(
        all(&connection)[0].target,
        NoticeTarget::ProjectPool {
            project_id: project("p1")
        }
    );
    connection
        .execute(
            "UPDATE research_jobs SET status='queued' WHERE id IN (?1,?2)",
            params![one, two],
        )
        .unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    assert!(pending(&connection).is_empty());
}

#[test]
fn unreviewed_research_memories_open_one_item_per_project_that_counts_them() {
    let (_folder, mut connection) = engine();
    suggest(&connection, "m1", "p1", false);
    suggest(&connection, "m2", "p1", false);
    suggest(&connection, "m3", "p1", true);
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let open = pending(&connection);
    assert_eq!(
        open,
        [(
            NoticeKind::MemoryReview,
            Some(project("p1")),
            "2 suggested memories to review.".to_owned()
        )]
    );
    connection
        .execute("DELETE FROM knowledge_heads WHERE id IN ('m1','m2')", [])
        .unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    assert!(pending(&connection).is_empty());
}

#[test]
fn a_change_set_waiting_for_review_opens_an_item_until_it_is_accepted() {
    let (_folder, mut connection) = engine();
    let id = "0123456789abcdef0123456789abcdef";
    change_set(&connection, id, "p1", "pending");
    change_set(
        &connection,
        "fedcba9876543210fedcba9876543210",
        "p1",
        "discarded",
    );
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let open = pending(&connection);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].0, NoticeKind::ChangeSetReview);
    assert_eq!(
        all(&connection)[0].target,
        NoticeTarget::ChangeSet {
            project_id: project("p1"),
            change_set: Some(crate::landing::ChangeSetId::new(id).unwrap()),
        }
    );
    connection
        .execute(
            "UPDATE landing_change_sets SET state='accepted' WHERE id=?1",
            [id],
        )
        .unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    assert!(pending(&connection).is_empty());
}

#[test]
fn a_native_account_that_never_signed_in_opens_a_sign_in_item_until_it_leaves() {
    let (_folder, mut connection) = engine();
    let access = ModelAccess {
        accounts: vec![reference("a1-account")],
        connections: vec![native("a1")],
        pools: vec![],
    };
    reconcile(&mut connection, &access, NOW_MS).unwrap();
    let items = all(&connection);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, NoticeKind::AccountSignIn);
    assert_eq!(items[0].project_id, None);
    assert_eq!(
        items[0].target,
        NoticeTarget::AccountSignIn {
            account: reference("a1")
        }
    );
    reconcile(&mut connection, &nobody(), NOW_MS + 1).unwrap();
    assert!(pending(&connection).is_empty());
}

#[test]
fn finished_failed_and_stopped_tasks_become_info_items_once_and_backfilled_rows_do_not() {
    use crate::activity::ActivityKind;
    let (_folder, mut connection) = engine();
    activity(&connection, "p1", ActivityKind::TaskFinished, NOW_MS - 1);
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    assert!(all(&connection).is_empty());
    activity(&connection, "p1", ActivityKind::TaskFinished, NOW_MS + 1);
    activity(&connection, "p1", ActivityKind::TaskFailed, NOW_MS + 2);
    activity(&connection, "p1", ActivityKind::TaskStopped, NOW_MS + 3);
    activity(&connection, "p1", ActivityKind::TaskStarted, NOW_MS + 4);
    activity(
        &connection,
        "p1",
        ActivityKind::TaskFinished,
        NOW_MS - 3_600_000,
    );
    reconcile(&mut connection, &nobody(), NOW_MS + 5).unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 6).unwrap();
    let kinds: Vec<NoticeKind> = all(&connection).iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            NoticeKind::AgentStopped,
            NoticeKind::AgentFailed,
            NoticeKind::AgentFinished
        ]
    );
    assert!(
        all(&connection)
            .iter()
            .all(|item| item.class == NoticeClass::Info && item.read_ms.is_none())
    );
}

#[test]
fn change_set_activity_becomes_landed_conflict_and_rolled_back_items() {
    use crate::activity::ActivityKind;
    let (_folder, mut connection) = engine();
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    activity(&connection, "p1", ActivityKind::MergedIntoMain, NOW_MS + 1);
    activity(&connection, "p1", ActivityKind::Conflict, NOW_MS + 2);
    activity(&connection, "p1", ActivityKind::Undone, NOW_MS + 3);
    reconcile(&mut connection, &nobody(), NOW_MS + 4).unwrap();
    let kinds: Vec<NoticeKind> = all(&connection).iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            NoticeKind::ChangeSetRolledBack,
            NoticeKind::ChangeSetConflict,
            NoticeKind::ChangeSetLanded
        ]
    );
}

#[test]
fn team_alerts_become_info_items_and_team_question_notifications_are_left_to_the_questions() {
    use crate::team::events::{NotificationKind, notify};
    let (_folder, mut connection) = engine();
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let tx = connection.transaction().unwrap();
    notify(
        &tx,
        &project("p1"),
        NotificationKind::QueueFull,
        "The event queue is full.",
        NOW_MS + 1,
    )
    .unwrap();
    notify(
        &tx,
        &project("p1"),
        NotificationKind::Question,
        "Master asks a question.",
        NOW_MS + 2,
    )
    .unwrap();
    notify(
        &tx,
        &project("p1"),
        NotificationKind::WaitingOnYou,
        "Sub-agent 3 waits.",
        NOW_MS + 3,
    )
    .unwrap();
    tx.commit().unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 4).unwrap();
    let items = all(&connection);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, NoticeKind::TeamAlert);
    assert_eq!(items[0].text, "The event queue is full.");
    assert_eq!(
        items[0].target,
        NoticeTarget::Conversation {
            project_id: project("p1")
        }
    );
}

#[test]
fn an_info_kind_turned_off_is_skipped_by_the_import_and_never_stored_later() {
    use crate::activity::ActivityKind;
    let (_folder, mut connection) = engine();
    reconcile(&mut connection, &nobody(), NOW_MS).unwrap();
    let mut chosen = NoticeSettings::default();
    let at = NoticeKind::ALL
        .iter()
        .position(|kind| *kind == NoticeKind::AgentFinished)
        .unwrap();
    chosen.kinds[at].delivery = Delivery::Off;
    let tx = connection.transaction().unwrap();
    assert_eq!(
        apply(
            &tx,
            &NoticeAction::SaveSettings { settings: chosen },
            NOW_MS
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    activity(&connection, "p1", ActivityKind::TaskFinished, NOW_MS + 1);
    reconcile(&mut connection, &nobody(), NOW_MS + 2).unwrap();
    reconcile(&mut connection, &nobody(), NOW_MS + 3).unwrap();
    assert!(all(&connection).is_empty());
}

#[test]
fn the_ticker_reconciles_at_once_then_at_most_once_per_interval() {
    let (_folder, mut connection) = engine();
    let mut ticker = Ticker::default();
    assert!(ticker.tick(&mut connection, &nobody(), NOW_MS).unwrap());
    ask(&connection, "p1", None, "Which port?");
    assert!(
        !ticker
            .tick(&mut connection, &nobody(), NOW_MS + TICK_MS - 1)
            .unwrap()
    );
    assert!(pending(&connection).is_empty());
    assert!(
        ticker
            .tick(&mut connection, &nobody(), NOW_MS + TICK_MS)
            .unwrap()
    );
    assert_eq!(pending(&connection).len(), 1);
}
