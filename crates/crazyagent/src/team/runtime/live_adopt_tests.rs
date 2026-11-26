use super::*;

const NAMED: &str = "conversation-before-the-first-open";

/// Makes `to` the project's live master session, parking the live one first, as the
/// owner's first Conversation open, New session and Resume do. Takes the database and the
/// session id.
fn open_session(connection: &Connection, to: &str) {
    connection
        .execute(
            "UPDATE master_sessions SET phase='parked' WHERE project=?1 AND phase='configured'",
            [PROJECT],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO master_sessions(id,project,created,version,phase,body)
             VALUES (?1,?2,(SELECT COALESCE(MAX(created),0)+1 FROM master_sessions),
                     (SELECT COALESCE(MAX(created),0)+1 FROM master_sessions),'configured','{}')
             ON CONFLICT(id) DO UPDATE SET phase='configured'",
            params![to, PROJECT],
        )
        .unwrap();
}

/// Parks the project's live master session, as the owner's Close does. Takes the database.
fn close_session(connection: &Connection) {
    connection
        .execute(
            "UPDATE master_sessions SET phase='parked' WHERE project=?1 AND phase='configured'",
            [PROJECT],
        )
        .unwrap();
}

/// The conversation id kept for a session. Takes the database and the session. Returns
/// the id, or None when the session kept none.
fn kept(connection: &Connection, session: &str) -> Option<String> {
    connection
        .query_row(
            "SELECT conversation FROM team_master_conversations WHERE session=?1",
            [session],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

/// A briefed live master started by the Live terminal switch before any master session,
/// that already named its harness conversation, with the receiver of its orders. Takes
/// the fixture and the runtime. Returns the agent id and the orders.
fn sessionless_master(fixture: &Fixture, team: &mut TeamRuntime) -> (i64, Receiver<Order>) {
    let project = fixture.setup.project_id.clone();
    crate::team::master::set_live(&fixture.connection, &project, true).unwrap();
    let id = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let (mut worker, orders) = held_worker(&project, AgentRole::Master);
    worker.live = true;
    worker.briefed = true;
    worker.input_ready = true;
    worker.session = None;
    team.workers.insert(id, worker);
    let named = Resume {
        id: NAMED.into(),
        record: fixture.base.join("record.jsonl"),
    };
    team.apply(id, Report::Conversation(named)).unwrap();
    assert_eq!(team.workers[&id].session, None);
    (id, orders)
}

/// One keep-live step. Takes the fixture and the runtime.
fn step(fixture: &Fixture, team: &mut TeamRuntime) {
    team.keep_live(&fixture.setup, &fixture.access, &BTreeSet::new())
        .unwrap();
}

/// A live master the Live terminal switch started before any master session takes the
/// session the owner's first Conversation open makes, empty as it is: it keeps running,
/// unstopped, in that session, and the conversation it named is kept for it, so a later
/// restart of the session resumes it.
#[test]
fn a_sessionless_live_master_adopts_the_first_opened_session_without_a_restart() {
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime().with_tools_refresh(|| {});
    let (id, orders) = sessionless_master(&fixture, &mut team);

    open_session(&fixture.connection, "first-open");
    step(&fixture, &mut team);

    let worker = &team.workers[&id];
    assert_eq!(worker.stop_why, "", "no engine_stop:moved");
    assert!(!worker.stopping, "the master keeps running");
    assert!(orders.try_recv().is_err(), "no stop order");
    assert_eq!(worker.session.as_deref(), Some("first-open"));
    assert_eq!(
        kept(&fixture.connection, "first-open").as_deref(),
        Some(NAMED)
    );
    team.workers.clear();
}

/// Two opens back to back: the first adopts the empty session, the second, a different
/// session, is a real move and stops the idle master as before.
#[test]
fn a_second_open_after_an_adopt_still_stops_the_master_as_moved() {
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime().with_tools_refresh(|| {});
    let (id, orders) = sessionless_master(&fixture, &mut team);

    open_session(&fixture.connection, "first-open");
    step(&fixture, &mut team);
    open_session(&fixture.connection, "second-open");
    step(&fixture, &mut team);

    let worker = &team.workers[&id];
    assert!(worker.stopping);
    assert_eq!(worker.stop_why, "engine_stop:moved");
    assert!(matches!(orders.try_recv(), Ok(Order::Stop)));
    assert_eq!(
        kept(&fixture.connection, "second-open"),
        None,
        "the moved session keeps no conversation of the first"
    );
    team.workers.clear();
}

/// A session that already holds an owner turn is not adopted: the session-less master
/// stops as moved and starts again for it.
#[test]
fn a_session_with_an_owner_turn_is_not_adopted() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime().with_tools_refresh(|| {});
    let (id, orders) = sessionless_master(&fixture, &mut team);

    open_session(&fixture.connection, "with-turn");
    crate::master_sessions::mark_owner_turn(&fixture.connection, &project, "with-turn", "codex")
        .unwrap();
    step(&fixture, &mut team);

    let worker = &team.workers[&id];
    assert_eq!(worker.stop_why, "engine_stop:moved");
    assert!(matches!(orders.try_recv(), Ok(Order::Stop)));
    assert_eq!(kept(&fixture.connection, "with-turn"), None);
    team.workers.clear();
}

/// A session that already kept a conversation is not adopted: the session-less master
/// stops as moved, and the kept conversation stays the session's own.
#[test]
fn a_session_that_kept_a_conversation_is_not_adopted() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime().with_tools_refresh(|| {});
    let (id, orders) = sessionless_master(&fixture, &mut team);

    open_session(&fixture.connection, "kept-before");
    let earlier = super::super::master::SessionConversation {
        agent: id + 1,
        harness: "codex".into(),
        connection: ENDPOINT.into(),
        resume: Resume {
            id: "earlier".into(),
            record: fixture.base.join("earlier.jsonl"),
        },
    };
    super::super::master::keep_conversation(&fixture.connection, &project, "kept-before", &earlier)
        .unwrap();
    step(&fixture, &mut team);

    let worker = &team.workers[&id];
    assert_eq!(worker.stop_why, "engine_stop:moved");
    assert!(matches!(orders.try_recv(), Ok(Order::Stop)));
    assert_eq!(
        kept(&fixture.connection, "kept-before").as_deref(),
        Some("earlier")
    );
    team.workers.clear();
}

/// A master seated in a session stops as moved when the owner closes that session, and
/// when another session becomes the live one, as before.
#[test]
fn a_seated_master_stops_as_moved_on_close_or_on_another_session() {
    for close in [true, false] {
        let fixture = Fixture::new(1);
        let mut team = fixture.runtime().with_tools_refresh(|| {});
        let (id, orders) = sessionless_master(&fixture, &mut team);
        open_session(&fixture.connection, "seated");
        team.workers.get_mut(&id).unwrap().session = Some("seated".into());

        if close {
            close_session(&fixture.connection);
        } else {
            open_session(&fixture.connection, "another");
        }
        step(&fixture, &mut team);

        let worker = &team.workers[&id];
        assert_eq!(worker.stop_why, "engine_stop:moved", "close {close}");
        assert!(
            matches!(orders.try_recv(), Ok(Order::Stop)),
            "close {close}"
        );
        team.workers.clear();
    }
}
