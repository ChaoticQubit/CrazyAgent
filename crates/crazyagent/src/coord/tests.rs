#[cfg(unix)]
use super::settings::{
    self, CoordSettings, Delivery, HarnessKind, Nudges, Overlaps, QuestionsTo, RoleSkills,
    SkillRole, SubagentMode, TaskStart,
};
#[cfg(unix)]
use super::store::Change;
#[cfg(unix)]
use super::tool_switches::{self, Switch};
use super::*;
#[cfg(unix)]
use rusqlite::{Connection, params};

#[cfg(unix)]
const PROJECT: &str = "coord-units";
#[cfg(unix)]
const OTHER: &str = "coord-other";
#[cfg(unix)]
const NOW: i64 = 1_000_000_000;
#[cfg(unix)]
const MINUTE: i64 = 60_000;

#[cfg(unix)]
fn db() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    for project in [PROJECT, OTHER] {
        let row = crate::protocol::StoredProject {
            project_id: ProjectId::new(project).unwrap(),
            spec: crate::protocol::ProjectSpec {
                name: project.into(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![project, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
    }
    connection
}

#[cfg(unix)]
fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

#[cfg(unix)]
fn agent_in(
    connection: &Connection,
    project: &str,
    role: &str,
    parent: Option<AgentId>,
    status: &str,
) -> AgentId {
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent) VALUES (?1,?2,'codex',1,?3,?4,?4,?5)",
            params![project, role, status, NOW, parent],
        )
        .unwrap();
    connection.last_insert_rowid()
}

#[cfg(unix)]
fn agent(connection: &Connection, role: &str, parent: Option<AgentId>) -> AgentId {
    agent_in(connection, PROJECT, role, parent, "working")
}

fn code(result: Result<impl std::fmt::Debug, crate::engine::EngineError>) -> String {
    result.unwrap_err().0
}

#[cfg(unix)]
fn note(from: Party, to: Party, kind: MessageKind, body: &str) -> NewMessage {
    NewMessage {
        project: project(),
        from,
        to,
        kind,
        question: None,
        work: None,
        reply_to: None,
        body: body.into(),
        idempotency_key: None,
    }
}

#[cfg(unix)]
fn question(from: AgentId, target: QuestionFor, severity: Severity, body: &str) -> NewMessage {
    NewMessage {
        question: Some((target, severity)),
        ..note(
            Party::Agent(from),
            Party::Owner,
            MessageKind::Question,
            body,
        )
    }
}

#[cfg(unix)]
fn routed(connection: &Connection, message: NewMessage) -> Party {
    let id = send(connection, message, NOW).unwrap();
    super::message(connection, id).unwrap().to
}

#[cfg(unix)]
#[test]
fn a_prompt_goes_to_its_agent_and_only_reply_answers_it() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let id = send(
        &connection,
        note(
            Party::Owner,
            Party::Agent(sub),
            MessageKind::Prompt,
            "Write the docs.",
        ),
        NOW,
    )
    .unwrap();
    let sent = super::message(&connection, id).unwrap();
    assert_eq!(
        (sent.from, sent.to, sent.kind, sent.receipt),
        (
            Party::Owner,
            Party::Agent(sub),
            MessageKind::Prompt,
            Receipt::Sent
        )
    );
    assert_eq!(pending(&connection, sub, 10).unwrap(), vec![sent]);
    assert_eq!(
        code(send(
            &connection,
            note(Party::Agent(sub), Party::Owner, MessageKind::Reply, "Done."),
            NOW
        )),
        "coord_reply_invalid"
    );
    let answer = reply(&connection, Party::Agent(sub), id, "Done.".into(), NOW + 1).unwrap();
    let answered = super::message(&connection, answer).unwrap();
    assert_eq!(
        (answered.to, answered.kind, answered.reply_to),
        (Party::Owner, MessageKind::Reply, Some(id))
    );
    assert_eq!(
        super::message(&connection, id).unwrap().receipt,
        Receipt::Replied
    );
    let noted = send(
        &connection,
        note(Party::Owner, Party::Agent(sub), MessageKind::Note, "FYI."),
        NOW,
    )
    .unwrap();
    assert_eq!(
        code(reply(
            &connection,
            Party::Agent(sub),
            noted,
            "Ok.".into(),
            NOW
        )),
        "coord_reply_invalid"
    );
    let other = agent(&connection, "sub", None);
    assert_eq!(
        code(reply(
            &connection,
            Party::Agent(other),
            id,
            "Me too.".into(),
            NOW
        )),
        "coord_reply_not_addressed"
    );
}

#[cfg(unix)]
#[test]
fn a_question_for_the_owner_is_answered_only_by_the_owner() {
    let connection = db();
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    let asked = question(sub, QuestionFor::Me, Severity::Normal, "May I delete it?");
    assert_eq!(routed(&connection, asked.clone()), Party::Owner);
    let id = send(&connection, asked, NOW).unwrap();
    assert_eq!(
        code(reply(
            &connection,
            Party::Agent(lead),
            id,
            "Yes.".into(),
            NOW
        )),
        "coord_question_for_owner"
    );
    let answer = reply(&connection, Party::Owner, id, "Yes.".into(), NOW).unwrap();
    assert_eq!(
        super::message(&connection, answer).unwrap().to,
        Party::Agent(sub)
    );
    assert_eq!(
        super::message(&connection, id).unwrap().receipt,
        Receipt::Replied
    );
}

#[cfg(unix)]
#[test]
fn a_question_for_the_lead_goes_up_the_team_and_for_the_master_needs_one() {
    let connection = db();
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    assert_eq!(
        code(send(
            &connection,
            question(lead, QuestionFor::Master, Severity::Low, "Which branch?"),
            NOW
        )),
        "coord_master_absent"
    );
    let stopped = agent_in(&connection, PROJECT, "master", None, "stopped");
    assert_eq!(
        code(send(
            &connection,
            question(lead, QuestionFor::Lead, Severity::Low, "Which branch?"),
            NOW
        )),
        "coord_master_absent",
        "a stopped master {stopped} is no master"
    );
    let master = agent(&connection, "master", None);
    assert_eq!(
        routed(
            &connection,
            question(sub, QuestionFor::Lead, Severity::Low, "Q1")
        ),
        Party::Agent(lead)
    );
    assert_eq!(
        routed(
            &connection,
            question(lead, QuestionFor::Lead, Severity::Low, "Q2")
        ),
        Party::Agent(master)
    );
    assert_eq!(
        routed(
            &connection,
            question(sub, QuestionFor::Master, Severity::Low, "Q3")
        ),
        Party::Agent(master)
    );
    assert_eq!(
        routed(
            &connection,
            question(master, QuestionFor::Lead, Severity::Low, "Q4")
        ),
        Party::Owner
    );
    assert_eq!(
        routed(
            &connection,
            question(master, QuestionFor::Master, Severity::Low, "Q5")
        ),
        Party::Owner
    );
    settings::write(
        &connection,
        &project(),
        &CoordSettings {
            questions_to: QuestionsTo::Me,
            ..CoordSettings::default()
        },
    )
    .unwrap();
    assert_eq!(
        routed(
            &connection,
            question(sub, QuestionFor::Lead, Severity::Low, "Q6")
        ),
        Party::Owner
    );
    assert_eq!(
        routed(
            &connection,
            question(lead, QuestionFor::Lead, Severity::Low, "Q7")
        ),
        Party::Agent(master)
    );
}

#[cfg(unix)]
#[test]
fn a_question_passed_up_keeps_its_severity_and_its_addressee() {
    let connection = db();
    let master = agent(&connection, "master", None);
    let lead = agent(&connection, "sub", None);
    let sibling = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    let first = send(
        &connection,
        question(
            sub,
            QuestionFor::Lead,
            Severity::High,
            "Can I drop the table?",
        ),
        NOW,
    )
    .unwrap();
    let passed = |from: AgentId, severity: Severity, earlier: MessageId| NewMessage {
        reply_to: Some(earlier),
        ..question(from, QuestionFor::Master, severity, "Passed up.")
    };
    assert_eq!(
        code(send(
            &connection,
            passed(lead, Severity::Normal, first),
            NOW
        )),
        "coord_question_severity_lowered"
    );
    assert_eq!(
        code(send(
            &connection,
            passed(sibling, Severity::High, first),
            NOW
        )),
        "coord_question_forward_invalid"
    );
    assert_eq!(
        routed(&connection, passed(lead, Severity::High, first)),
        Party::Agent(master)
    );
    let mine = send(
        &connection,
        question(sub, QuestionFor::Me, Severity::Low, "Owner only."),
        NOW,
    )
    .unwrap();
    assert_eq!(
        code(send(&connection, passed(lead, Severity::Low, mine), NOW)),
        "coord_question_for_owner"
    );
    let prompt = send(
        &connection,
        note(
            Party::Owner,
            Party::Agent(lead),
            MessageKind::Prompt,
            "Do it.",
        ),
        NOW,
    )
    .unwrap();
    assert_eq!(
        code(send(&connection, passed(lead, Severity::Low, prompt), NOW)),
        "coord_question_forward_invalid"
    );
}

#[cfg(unix)]
#[test]
fn a_message_is_checked_and_a_repeated_key_returns_the_first_id() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let elsewhere = agent_in(&connection, OTHER, "sub", None, "working");
    let keyed = |from: Party, to: Party, key: &str| NewMessage {
        idempotency_key: Some(key.into()),
        ..note(from, to, MessageKind::Note, "Once.")
    };
    let first = send(
        &connection,
        keyed(Party::Agent(sub), Party::Owner, "k-1"),
        NOW,
    )
    .unwrap();
    assert_eq!(
        send(
            &connection,
            keyed(Party::Agent(sub), Party::Owner, "k-1"),
            NOW
        )
        .unwrap(),
        first
    );
    let owner = send(
        &connection,
        keyed(Party::Owner, Party::Agent(sub), "k-1"),
        NOW,
    )
    .unwrap();
    assert_ne!(owner, first);
    assert_eq!(
        send(
            &connection,
            keyed(Party::Owner, Party::Agent(sub), "k-1"),
            NOW
        )
        .unwrap(),
        owner
    );
    for key in ["", "has space", "k".repeat(KEY_BYTES_MAX + 1).as_str()] {
        assert_eq!(
            code(send(
                &connection,
                keyed(Party::Owner, Party::Agent(sub), key),
                NOW
            )),
            "coord_key_invalid"
        );
    }
    let tagged = NewMessage {
        question: Some((QuestionFor::Me, Severity::Low)),
        ..note(Party::Agent(sub), Party::Owner, MessageKind::Note, "Tags.")
    };
    assert_eq!(
        code(send(&connection, tagged, NOW)),
        "coord_question_tags_invalid"
    );
    assert_eq!(
        code(send(
            &connection,
            note(
                Party::Agent(sub),
                Party::Owner,
                MessageKind::Question,
                "No tags?"
            ),
            NOW
        )),
        "coord_question_tags_missing"
    );
    assert_eq!(
        code(send(
            &connection,
            note(
                Party::Agent(sub),
                Party::Agent(sub),
                MessageKind::Note,
                "Me."
            ),
            NOW
        )),
        "coord_message_self"
    );
    assert_eq!(
        code(send(
            &connection,
            note(
                Party::Agent(sub),
                Party::Engine,
                MessageKind::Note,
                "Engine."
            ),
            NOW
        )),
        "coord_recipient_invalid"
    );
    assert_eq!(
        code(send(
            &connection,
            note(
                Party::Owner,
                Party::Agent(elsewhere),
                MessageKind::Note,
                "Far."
            ),
            NOW
        )),
        "coord_agent_unknown"
    );
    for body in [
        String::new(),
        "  ".into(),
        "bell\u{7}".into(),
        "x".repeat(BODY_BYTES_MAX + 1),
    ] {
        assert_eq!(
            code(send(
                &connection,
                note(Party::Owner, Party::Agent(sub), MessageKind::Note, &body),
                NOW
            )),
            "coord_text_invalid"
        );
    }
    let unknown_work = NewMessage {
        work: Some(999),
        ..note(Party::Owner, Party::Agent(sub), MessageKind::Note, "Work.")
    };
    assert_eq!(
        code(send(&connection, unknown_work, NOW)),
        "coord_work_unknown"
    );
}

#[cfg(unix)]
#[test]
fn a_receipt_only_moves_forward_and_a_dead_agent_s_unread_mail_is_requeued() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let post = |body: &str| {
        send(
            &connection,
            note(Party::Owner, Party::Agent(sub), MessageKind::Prompt, body),
            NOW,
        )
        .unwrap()
    };
    let receipt = |id| super::message(&connection, id).unwrap().receipt;
    let walked = post("Walk.");
    for next in [
        Receipt::Delivered,
        Receipt::Delivered,
        Receipt::Read,
        Receipt::Replied,
    ] {
        mark(&connection, walked, next, NOW).unwrap();
        assert_eq!(receipt(walked), next);
    }
    let read = post("Read.");
    mark(&connection, read, Receipt::Read, NOW).unwrap();
    assert_eq!(
        code(mark(&connection, read, Receipt::Delivered, NOW)),
        "coord_receipt_stale"
    );
    assert_eq!(
        code(mark(&connection, read, Receipt::Failed, NOW)),
        "coord_receipt_stale"
    );
    let failed = post("Failed.");
    mark(&connection, failed, Receipt::Failed, NOW).unwrap();
    assert_eq!(
        code(mark(&connection, failed, Receipt::Read, NOW)),
        "coord_receipt_stale"
    );
    let delivered = post("Delivered.");
    mark(&connection, delivered, Receipt::Delivered, NOW).unwrap();
    let typed = post("Typed.");
    mark(&connection, typed, Receipt::Typed, NOW).unwrap();
    let waiting = post("Waiting.");
    assert_eq!(requeue_dead(&connection, sub, NOW + 1).unwrap(), 2);
    for (id, expected) in [
        (delivered, Receipt::Sent),
        (typed, Receipt::Sent),
        (waiting, Receipt::Sent),
        (read, Receipt::Read),
        (walked, Receipt::Replied),
        (failed, Receipt::Failed),
    ] {
        assert_eq!(receipt(id), expected, "{id}");
    }
    assert!(Receipt::Sent.unread() && Receipt::Typed.unread() && !Receipt::Read.unread());
}

#[cfg(unix)]
#[test]
fn pending_lists_only_sent_mail_oldest_first_up_to_its_cap() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let ids: Vec<MessageId> = (0..PENDING_MAX + 6)
        .map(|n| {
            send(
                &connection,
                note(
                    Party::Owner,
                    Party::Agent(sub),
                    MessageKind::Note,
                    &format!("n{n}"),
                ),
                NOW,
            )
            .unwrap()
        })
        .collect();
    mark(&connection, ids[0], Receipt::Delivered, NOW).unwrap();
    let listed: Vec<MessageId> = pending(&connection, sub, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(listed, ids[1..=PENDING_MAX].to_vec());
    assert_eq!(pending(&connection, sub, 3).unwrap().len(), 3);
    assert_eq!(unread(&connection, sub, usize::MAX).unwrap()[0].id, ids[0]);
}

#[cfg(unix)]
fn audits(connection: &Connection, work: WorkId) -> Vec<(String, i64, i64)> {
    let mut statement = connection
        .prepare("SELECT action,from_version,to_version FROM coord_audit WHERE work=?1 ORDER BY id")
        .unwrap();
    statement
        .query_map([work], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[cfg(unix)]
fn held(connection: &Connection, agent: AgentId, work: WorkId) -> usize {
    connection
        .query_row(
            "SELECT COUNT(*) FROM coord_claims WHERE agent=?1 AND work=?2 AND released IS NULL",
            params![agent, work],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
}

#[cfg(unix)]
#[test]
fn work_moves_through_its_transfer_table_one_version_at_a_time() {
    let connection = db();
    let master = agent(&connection, "master", None);
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    let other = agent(&connection, "sub", None);
    let stopped = agent_in(&connection, PROJECT, "sub", None, "stopped");
    let (work_id, overlaps) = start_work(
        &connection,
        &project(),
        lead,
        "Docs".into(),
        vec!["./docs//guide.md".into(), "docs/guide.md".into()],
        NOW,
    )
    .unwrap();
    assert!(overlaps.is_empty());
    let started = work(&connection, work_id).unwrap();
    assert_eq!(
        (
            started.owner,
            started.state,
            started.version,
            started.claims
        ),
        (
            Some(lead),
            WorkState::Active,
            1,
            vec!["docs/guide.md".to_owned()]
        )
    );
    assert_eq!(audits(&connection, work_id), vec![("start".into(), 0, 1)]);
    let state = || {
        let found = work(&connection, work_id).unwrap();
        (found.state, found.owner, found.version)
    };
    let move_by = |version, action, by| transfer(&connection, work_id, version, action, by, NOW);

    assert_eq!(
        code(move_by(
            1,
            Transfer::Assign { to: stopped },
            Party::Agent(lead)
        )),
        "coord_agent_stopped"
    );
    assert_eq!(
        code(move_by(
            1,
            Transfer::Assign { to: sub },
            Party::Agent(other)
        )),
        "coord_transfer_refused"
    );
    assert_eq!(
        code(move_by(
            1,
            Transfer::Assign { to: lead },
            Party::Agent(lead)
        )),
        "coord_transfer_invalid"
    );
    assert_eq!(
        code(move_by(1, Transfer::Accept, Party::Agent(lead))),
        "coord_transfer_invalid"
    );
    assert_eq!(
        code(move_by(1, Transfer::Reopen, Party::Agent(lead))),
        "coord_transfer_invalid"
    );
    assert_eq!(
        code(move_by(
            1,
            Transfer::Handoff { to: other },
            Party::Agent(sub)
        )),
        "coord_transfer_refused"
    );
    assert_eq!(state(), (WorkState::Active, Some(lead), 1));

    assert_eq!(
        move_by(1, Transfer::Assign { to: sub }, Party::Agent(lead)).unwrap(),
        2
    );
    assert_eq!(state(), (WorkState::Assigned, Some(sub), 2));
    assert_eq!(
        held(&connection, lead, work_id),
        0,
        "the old owner's claims are released"
    );
    assert_eq!(
        code(move_by(1, Transfer::Accept, Party::Agent(sub))),
        "coord_work_stale"
    );
    assert_eq!(
        code(move_by(2, Transfer::Accept, Party::Agent(lead))),
        "coord_transfer_refused"
    );
    assert_eq!(
        code(move_by(2, Transfer::Release, Party::Agent(sub))),
        "coord_transfer_invalid"
    );
    assert_eq!(move_by(2, Transfer::Decline, Party::Agent(sub)).unwrap(), 3);
    assert_eq!(state(), (WorkState::Declined, Some(sub), 3));
    assert_eq!(
        code(move_by(3, Transfer::Reopen, Party::Agent(other))),
        "coord_transfer_refused"
    );
    assert_eq!(
        move_by(3, Transfer::Reopen, Party::Agent(master)).unwrap(),
        4
    );
    assert_eq!(state(), (WorkState::Active, Some(sub), 4));
    assert_eq!(
        move_by(4, Transfer::Handoff { to: other }, Party::Agent(sub)).unwrap(),
        5
    );
    assert_eq!(state(), (WorkState::Assigned, Some(other), 5));
    assert_eq!(
        move_by(5, Transfer::Accept, Party::Agent(other)).unwrap(),
        6
    );
    assert_eq!(state(), (WorkState::Accepted, Some(other), 6));
    assert_eq!(
        code(move_by(6, Transfer::Release, Party::Agent(lead))),
        "coord_transfer_refused"
    );
    assert_eq!(move_by(6, Transfer::Release, Party::Owner).unwrap(), 7);
    assert_eq!(state(), (WorkState::Open, None, 7));
    assert_eq!(
        code(move_by(7, Transfer::Reopen, Party::Owner)),
        "coord_transfer_invalid"
    );
    assert_eq!(
        move_by(7, Transfer::Assign { to: sub }, Party::Agent(master)).unwrap(),
        8
    );
    let actions: Vec<String> = audits(&connection, work_id)
        .into_iter()
        .map(|row| row.0)
        .collect();
    assert_eq!(
        actions,
        [
            "start", "assign", "decline", "reopen", "handoff", "accept", "release", "assign"
        ]
    );
    assert!(
        audits(&connection, work_id)
            .iter()
            .all(|(_, from, to)| *to == from + 1)
    );
}

/// OS-8: an agent's claim on work it finished as review-ready keeps overlapping another
/// agent that starts on the same file until the change set holding it lands, when
/// `release_set` releases it; then the same start overlaps nothing.
#[cfg(unix)]
#[test]
fn a_review_ready_claim_keeps_overlapping_until_the_work_lands() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_work(
        &connection,
        &project(),
        grace,
        "Dependencies".into(),
        vec!["pyproject.toml".into()],
        NOW,
    )
    .unwrap();
    assert_eq!(
        finish(
            &connection,
            grace_work,
            1,
            Finish::ReviewReady,
            "Green.".into(),
            grace,
            NOW,
        )
        .unwrap(),
        2
    );
    let start = |title: &str| {
        start_work(
            &connection,
            &project(),
            ada,
            title.into(),
            vec!["pyproject.toml".into()],
            NOW,
        )
        .unwrap()
    };
    let (ada_work, overlaps) = start("CLI");
    assert_eq!(
        overlaps.len(),
        1,
        "Grace's edits have not landed yet: {overlaps:?}"
    );
    assert_eq!(overlaps[0].theirs.agent, grace);
    assert_eq!(overlaps[0].theirs.work, grace_work);
    transfer(
        &connection,
        ada_work,
        1,
        Transfer::Release,
        Party::Agent(ada),
        NOW,
    )
    .unwrap();

    let set = "0123456789abcdef0123456789abcdef";
    assert_eq!(hold_for_set(&connection, grace, set).unwrap(), 1);
    assert_eq!(release_set(&connection, set, NOW).unwrap(), 1);
    assert_eq!(held(&connection, grace, grace_work), 0);
    let (_, overlaps) = start("CLI again");
    assert!(
        overlaps.is_empty(),
        "the landed work overlaps nothing: {overlaps:?}"
    );
}

/// The two change set ids the hold rows below use.
#[cfg(unix)]
const SETS: [&str; 2] = [
    "0123456789abcdef0123456789abcdef",
    "fedcba9876543210fedcba9876543210",
];

/// Grace's review-ready work on pyproject.toml with its claim held for the first set.
#[cfg(unix)]
fn held_review(connection: &Connection) -> (AgentId, WorkId) {
    let grace = agent(connection, "sub", None);
    let (work_id, _) = start_work(
        connection,
        &project(),
        grace,
        "Dependencies".into(),
        vec!["pyproject.toml".into()],
        NOW,
    )
    .unwrap();
    finish(
        connection,
        work_id,
        1,
        Finish::ReviewReady,
        "Green.".into(),
        grace,
        NOW,
    )
    .unwrap();
    assert_eq!(hold_for_set(connection, grace, SETS[0]).unwrap(), 1);
    (grace, work_id)
}

/// OS-8: reopening ended work takes its claim off the change set that held it, so that set
/// landing later frees none of the reopened work's later edits, which the next set holds.
#[cfg(unix)]
#[test]
fn reopened_work_s_claims_wait_on_the_next_change_set_not_the_earlier_one() {
    let connection = db();
    let (grace, work_id) = held_review(&connection);
    assert_eq!(
        transfer(
            &connection,
            work_id,
            2,
            Transfer::Reopen,
            Party::Agent(grace),
            NOW,
        )
        .unwrap(),
        3
    );
    finish(
        &connection,
        work_id,
        3,
        Finish::ReviewReady,
        "Green again.".into(),
        grace,
        NOW,
    )
    .unwrap();
    assert_eq!(
        hold_for_set(&connection, grace, SETS[1]).unwrap(),
        1,
        "the reopened work's claim waits on the next set"
    );
    assert_eq!(
        release_set(&connection, SETS[0], NOW).unwrap(),
        0,
        "the earlier set's landing frees none of the later edits"
    );
    assert_eq!(held(&connection, grace, work_id), 1);
    assert_eq!(release_set(&connection, SETS[1], NOW).unwrap(), 1);
    assert_eq!(held(&connection, grace, work_id), 0);
}

/// A change-set hold names its set: an empty set id stops at the assertion.
#[cfg(unix)]
#[test]
#[should_panic(expected = "!set.is_empty()")]
fn holding_claims_for_no_change_set_trips_the_assertion() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let _ = hold_for_set(&connection, grace, "");
}

/// A set's release frees exactly the claims it marked: a hold row marked released while
/// its claim still holds is a broken store, and the release stops at its assertion.
#[cfg(unix)]
#[test]
#[should_panic(expected = "a set's release frees exactly the claims it marked")]
fn a_release_meeting_a_released_hold_on_a_held_claim_trips_the_assertion() {
    let connection = db();
    held_review(&connection);
    connection
        .execute("UPDATE coord_held SET released=?1", [NOW])
        .unwrap();
    let _ = release_set(&connection, SETS[0], NOW);
}

/// A restore takes back exactly the claims its set released: a released hold row whose
/// claim already holds is a broken store, and the restore stops at its assertion.
#[cfg(unix)]
#[test]
#[should_panic(expected = "a restore takes back exactly the claims its set released")]
fn a_restore_meeting_a_released_hold_on_a_held_claim_trips_the_assertion() {
    let connection = db();
    held_review(&connection);
    connection
        .execute("UPDATE coord_held SET released=?1", [NOW])
        .unwrap();
    let _ = restore_set(&connection, SETS[0]);
}

/// Adds `count` claims `built/<i>.rs` to one agent's work, as a long backlog would.
#[cfg(unix)]
fn add_claims(connection: &Connection, who: AgentId, work: WorkId, count: i64) {
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?4)
             INSERT INTO coord_claims(project,agent,work,pattern,created)
             SELECT ?1,?2,?3,'built/'||i||'.rs',?5 FROM n",
            params![PROJECT, who, work, count, NOW],
        )
        .unwrap();
}

/// Sets one team agent's status, as the runtime does when it stops or resumes it.
#[cfg(unix)]
fn status_of(connection: &Connection, who: AgentId, status: &str) {
    connection
        .execute(
            "UPDATE team_agents SET status=?2 WHERE id=?1",
            params![who, status],
        )
        .unwrap();
}

/// OS-8 r2 M1: one agent's flood of claimed work fills only its own share: past its live
/// claims limit its starts are refused with `coord_agent_claims_full`, and another agent
/// still starts work.
#[cfg(unix)]
#[test]
fn one_agent_s_flood_of_claimed_work_refuses_only_that_agent() {
    let connection = db();
    let flooder = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let mut refused = 0;
    for unit in 0..64 {
        let claims = (0..32)
            .map(|file| format!("flood/{unit}/{file}.rs"))
            .collect();
        if let Err(problem) = start_work(
            &connection,
            &project(),
            flooder,
            format!("Flood {unit}"),
            claims,
            NOW,
        ) {
            assert_eq!(problem.0, "coord_agent_claims_full");
            refused += 1;
        }
    }
    assert_eq!(refused, 60, "four full units fill one agent's share");
    let (_, overlaps) = start_work(
        &connection,
        &project(),
        ada,
        "Lexer".into(),
        vec!["src/lexer.rs".into()],
        NOW,
    )
    .unwrap();
    assert!(overlaps.is_empty(), "{overlaps:?}");
}

/// OS-8 r2 M1: a stopped agent's claims on work it was still on leave the overlap check, so
/// built-up claims of agents that never run again stop no one; they are not released, and
/// they count and overlap again once the agent is resumed under its own id.
#[cfg(unix)]
#[test]
fn a_stopped_agent_s_claims_on_active_work_leave_the_scan_until_it_resumes() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_work(
        &connection,
        &project(),
        grace,
        "Build".into(),
        vec!["built/0.rs".into()],
        NOW,
    )
    .unwrap();
    add_claims(&connection, grace, grace_work, 2100);
    let start = |file: &str| {
        start_work(
            &connection,
            &project(),
            ada,
            format!("Edit {file}"),
            vec![file.into()],
            NOW,
        )
    };
    assert_eq!(code(start("src/a.rs")), "coord_claims_full");
    status_of(&connection, grace, "stopped");
    let (_, overlaps) = start("built/7.rs").unwrap();
    assert!(
        overlaps.is_empty(),
        "a stopped agent's active claims are left out: {overlaps:?}"
    );
    assert_eq!(held(&connection, grace, grace_work), 2101, "and kept");
    status_of(&connection, grace, "working");
    assert_eq!(
        code(start("src/b.rs")),
        "coord_claims_full",
        "a resumed agent's claims count again"
    );
}

/// OS-8 r2 M1: the owner hears once when a project's live claims pass what one overlap
/// check reads, not again while it stays full, and again only after it emptied and filled.
#[cfg(unix)]
#[test]
fn the_owner_hears_once_when_a_project_fills_the_claims_scan() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let (grace_work, _) = start_work(
        &connection,
        &project(),
        grace,
        "Build".into(),
        vec!["built/0.rs".into()],
        NOW,
    )
    .unwrap();
    assert!(!claims_full_notice(&connection, &project(), NOW).unwrap());
    add_claims(&connection, grace, grace_work, 2100);
    assert!(claims_full_notice(&connection, &project(), NOW).unwrap());
    assert!(
        !claims_full_notice(&connection, &project(), NOW).unwrap(),
        "told once while it stays full"
    );
    release_claims(&connection, grace, grace_work, NOW).unwrap();
    assert!(!claims_full_notice(&connection, &project(), NOW).unwrap());
    let (again, _) = start_work(
        &connection,
        &project(),
        grace,
        "Build again".into(),
        vec!["built/0.rs".into()],
        NOW,
    )
    .unwrap();
    add_claims(&connection, grace, again, 2100);
    assert!(
        claims_full_notice(&connection, &project(), NOW).unwrap(),
        "a project that filled again is told again"
    );
}

/// Sets the project's Overlapping edits option.
#[cfg(unix)]
fn overlaps_set(connection: &Connection, overlaps: Overlaps) {
    settings::write(
        connection,
        &project(),
        &CoordSettings {
            overlaps,
            ..CoordSettings::default()
        },
    )
    .unwrap();
}

/// The engine's prompts to one agent, oldest first.
#[cfg(unix)]
fn prompts(connection: &Connection, agent: AgentId) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT body FROM coord_messages WHERE from_kind='engine' AND to_agent=?1
             AND kind='prompt' ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([agent], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// How many units of work one agent owns.
#[cfg(unix)]
fn works_owned(connection: &Connection, agent: AgentId) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM coord_work WHERE owner_agent=?1",
            [agent],
            |row| row.get(0),
        )
        .unwrap()
}

/// One agent's start on one file.
#[cfg(unix)]
fn start_on(
    connection: &Connection,
    who: AgentId,
    file: &str,
) -> Result<(WorkId, Vec<Overlap>), crate::engine::EngineError> {
    start_work(
        connection,
        &project(),
        who,
        format!("Edit {file}"),
        vec![file.into()],
        NOW,
    )
}

/// The prompt a waiting agent gets once its file is free (OS-8, plan wording).
#[cfg(unix)]
const LEXER_FREE: &str = "src/lexer.rs is free now: start your work again.";

/// OS-8 Wait: an agent starting on a file another agent's active work claims is refused
/// with `coord_claim_waits` and starts nothing; review-ready work still holds the file, and
/// once the change set holding it lands the waiting agent gets one prompt and starts.
#[cfg(unix)]
#[test]
fn under_wait_an_overlapping_start_waits_and_its_agent_is_told_when_the_file_lands() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    assert_eq!(
        works_owned(&connection, ada),
        0,
        "a waiting start starts nothing"
    );
    assert!(prompts(&connection, ada).is_empty());
    finish(
        &connection,
        grace_work,
        1,
        Finish::ReviewReady,
        "Green.".into(),
        grace,
        NOW,
    )
    .unwrap();
    assert!(
        prompts(&connection, ada).is_empty(),
        "review-ready work still holds the file"
    );
    assert_eq!(hold_for_set(&connection, grace, SETS[0]).unwrap(), 1);
    assert_eq!(release_set(&connection, SETS[0], NOW).unwrap(), 1);
    assert_eq!(prompts(&connection, ada), vec![LEXER_FREE.to_owned()]);
    let (_, overlaps) = start_on(&connection, ada, "src/lexer.rs").unwrap();
    assert!(overlaps.is_empty(), "{overlaps:?}");
    assert_eq!(prompts(&connection, ada).len(), 1, "one prompt per wait");
}

/// OS-8 Wait: an owner releasing the held work wakes the agent waiting for its file.
#[cfg(unix)]
#[test]
fn under_wait_a_released_claim_wakes_the_agent_waiting_for_its_file() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    transfer(
        &connection,
        grace_work,
        1,
        Transfer::Release,
        Party::Agent(grace),
        NOW,
    )
    .unwrap();
    assert_eq!(prompts(&connection, ada), vec![LEXER_FREE.to_owned()]);
}

/// OS-8 Wait never waits forever: once the holder stops, its active claim no longer holds
/// the file, so the engine's sweep prompts the waiting agent once and keeps no wait.
#[cfg(unix)]
#[test]
fn under_wait_a_holder_that_stops_frees_its_waiter_at_the_next_sweep() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    assert_eq!(
        wake_waiters(&connection, NOW).unwrap(),
        0,
        "Grace still works"
    );
    connection
        .execute(
            "UPDATE team_agents SET status='stopped' WHERE id=?1",
            [grace],
        )
        .unwrap();
    assert_eq!(wake_waiters(&connection, NOW).unwrap(), 1);
    assert_eq!(prompts(&connection, ada), vec![LEXER_FREE.to_owned()]);
    assert_eq!(wake_waiters(&connection, NOW).unwrap(), 0);
    assert_eq!(prompts(&connection, ada).len(), 1);
}

/// OS-8 Wait: a holder already away does not hold the file, so the start goes ahead and
/// reports the overlap as stale, as under Warn.
#[cfg(unix)]
#[test]
fn under_wait_an_away_holder_never_makes_a_start_wait() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    start_on(&connection, grace, "src/lexer.rs").unwrap();
    connection
        .execute(
            "UPDATE coord_activity SET last_seen=?2 WHERE agent=?1",
            params![grace, NOW - 31 * MINUTE],
        )
        .unwrap();
    let (_, overlaps) = start_on(&connection, ada, "src/lexer.rs").unwrap();
    assert_eq!(overlaps.len(), 1);
    assert!(overlaps[0].theirs.stale);
}

/// OS-8: Warn, Block and Off keep starting the overlapping work, reporting the overlap, and
/// prompt no one.
#[cfg(unix)]
#[test]
fn warn_block_and_off_start_an_overlapping_work_and_prompt_no_one() {
    for overlaps_option in [Overlaps::Warn, Overlaps::Block, Overlaps::Off] {
        let connection = db();
        overlaps_set(&connection, overlaps_option);
        let grace = agent(&connection, "sub", None);
        let ada = agent(&connection, "sub", None);
        let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
        let (_, overlaps) = start_on(&connection, ada, "src/lexer.rs").unwrap();
        assert_eq!(overlaps.len(), 1, "{overlaps_option:?}");
        transfer(
            &connection,
            grace_work,
            1,
            Transfer::Release,
            Party::Agent(grace),
            NOW,
        )
        .unwrap();
        assert!(prompts(&connection, ada).is_empty(), "{overlaps_option:?}");
    }
}

/// OS-8 Wait: a waiting agent that stops is forgotten: the file's release prompts no one.
#[cfg(unix)]
#[test]
fn under_wait_a_waiter_that_stops_is_forgotten() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    connection
        .execute("UPDATE team_agents SET status='stopped' WHERE id=?1", [ada])
        .unwrap();
    transfer(
        &connection,
        grace_work,
        1,
        Transfer::Release,
        Party::Agent(grace),
        NOW,
    )
    .unwrap();
    assert!(prompts(&connection, ada).is_empty());
    let left: i64 = connection
        .query_row("SELECT COUNT(*) FROM coord_waits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// OS-8 Wait: one agent keeps at most 8 waits; a ninth refused start drops its oldest.
#[cfg(unix)]
#[test]
fn under_wait_an_agent_keeps_at_most_eight_waits() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let ada = agent(&connection, "sub", None);
    for index in 0..9 {
        let holder = agent(&connection, "sub", None);
        let file = format!("src/f{index}.rs");
        start_on(&connection, holder, &file).unwrap();
        assert_eq!(code(start_on(&connection, ada, &file)), "coord_claim_waits");
    }
    let files: Vec<String> = {
        let mut statement = connection
            .prepare("SELECT file FROM coord_waits WHERE agent=?1 ORDER BY file")
            .unwrap();
        statement
            .query_map([ada], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(files.len(), 8, "{files:?}");
    assert!(!files.contains(&"src/f0.rs".to_owned()), "{files:?}");
}

/// OS-8 Wait never waits forever: a holder that goes away after the start waited no longer
/// holds the file, as a start would see it, so the next sweep prompts the waiting agent once;
/// a holder only idle still holds it.
#[cfg(unix)]
#[test]
fn under_wait_a_holder_that_goes_away_frees_its_waiter_at_the_next_sweep() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    let quiet = |minutes: i64| {
        connection
            .execute(
                "UPDATE coord_activity SET last_seen=?2 WHERE agent=?1",
                params![grace, NOW - minutes * MINUTE],
            )
            .unwrap();
    };
    quiet(29);
    assert_eq!(
        wake_waiters(&connection, NOW).unwrap(),
        0,
        "an idle holder still holds the file"
    );
    quiet(31);
    assert_eq!(wake_waiters(&connection, NOW).unwrap(), 1);
    assert_eq!(prompts(&connection, ada), vec![LEXER_FREE.to_owned()]);
    assert_eq!(wake_waiters(&connection, NOW).unwrap(), 0);
    let (_, overlaps) = start_on(&connection, ada, "src/lexer.rs").unwrap();
    assert!(overlaps[0].theirs.stale, "{overlaps:?}");
}

/// OS-8 Wait, DEC-117 Q7: finished work holds its files until its change set lands, so an
/// away holder's review-ready work keeps its waiter waiting.
#[cfg(unix)]
#[test]
fn under_wait_finished_work_of_an_away_holder_still_holds_the_file() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    finish(
        &connection,
        grace_work,
        1,
        Finish::ReviewReady,
        "Green.".into(),
        grace,
        NOW,
    )
    .unwrap();
    connection
        .execute(
            "UPDATE coord_activity SET last_seen=?2 WHERE agent=?1",
            params![grace, NOW - 31 * MINUTE],
        )
        .unwrap();
    assert_eq!(wake_waiters(&connection, NOW).unwrap(), 0);
    assert!(prompts(&connection, ada).is_empty());
}

/// OS-8 Wait never closes a cycle: Ada waits for Grace's file, so Grace's start on Ada's
/// file goes ahead as under Warn, reporting the overlap, and the owner hears of the pair
/// once, however often it happens again.
#[cfg(unix)]
#[test]
fn under_wait_two_agents_never_wait_on_each_other_and_the_owner_hears_once() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    start_on(&connection, grace, "src/lexer.rs").unwrap();
    start_on(&connection, ada, "src/parser.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    let (_, overlaps) = start_on(&connection, grace, "src/parser.rs")
        .expect("waiting on Ada, who waits on Grace, would close a cycle");
    assert_eq!(overlaps.len(), 1, "{overlaps:?}");
    assert_eq!(works_owned(&connection, grace), 2);
    assert_eq!(
        wait_cycle_notices(&connection, &project(), NOW).unwrap(),
        vec![(grace, ada, "src/parser.rs".to_owned())]
    );
    assert!(
        wait_cycle_notices(&connection, &project(), NOW)
            .unwrap()
            .is_empty(),
        "the owner hears once"
    );
    start_on(&connection, grace, "src/parser.rs").unwrap();
    assert!(
        wait_cycle_notices(&connection, &project(), NOW)
            .unwrap()
            .is_empty(),
        "the same pair is not told again"
    );
}

/// OS-8 Wait: a ring of three waits never closes either. Ada waits for Grace and Grace for
/// Lin, so Lin's start on Ada's file goes ahead and the owner hears of Lin and Ada.
#[cfg(unix)]
#[test]
fn under_wait_a_ring_of_three_waits_never_closes() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let lin = agent(&connection, "sub", None);
    start_on(&connection, grace, "src/lexer.rs").unwrap();
    start_on(&connection, ada, "src/parser.rs").unwrap();
    start_on(&connection, lin, "src/main.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    assert_eq!(
        code(start_on(&connection, grace, "src/main.rs")),
        "coord_claim_waits",
        "a chain that does not reach the starter still waits"
    );
    start_on(&connection, lin, "src/parser.rs").expect("the third wait would close the ring");
    assert_eq!(
        wait_cycle_notices(&connection, &project(), NOW).unwrap(),
        vec![(lin, ada, "src/parser.rs".to_owned())]
    );
}

/// OS-8 Wait: a chain of waits longer than one walk reads counts as a cycle, so the start
/// at its far end goes ahead as under Warn rather than wait on a chain never read to its end.
#[cfg(unix)]
#[test]
fn under_wait_a_chain_past_the_walk_bound_goes_ahead_as_under_warn() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let agents: Vec<AgentId> = (0..super::claims::WAIT_WALK_MAX + 2)
        .map(|_| agent(&connection, "sub", None))
        .collect();
    for (index, who) in agents.iter().enumerate() {
        start_on(&connection, *who, &format!("src/f{index}.rs")).unwrap();
    }
    let mut went_ahead = Vec::new();
    for index in (0..agents.len() - 1).rev() {
        match start_on(
            &connection,
            agents[index],
            &format!("src/f{}.rs", index + 1),
        ) {
            Ok(_) => went_ahead.push(index),
            Err(problem) => assert_eq!(problem.0, "coord_claim_waits", "{index}"),
        }
    }
    assert_eq!(went_ahead, vec![0]);
}

/// OS-8 Wait, T9-FU: a start whose wait would close a cycle goes ahead without the file
/// another agent's live claim holds, so that holder keeps editing it and the starter's
/// edits of it stay refused; the start's file in another folder is claimed, its overlap says
/// the file was left out, and the owner hears who started and who holds the file.
#[cfg(unix)]
#[test]
fn under_wait_a_cycle_fallback_leaves_the_held_file_to_its_holder() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/parser.rs").unwrap();
    start_on(&connection, ada, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, grace, "src/lexer.rs")),
        "coord_claim_waits"
    );
    let (work, overlaps) = start_work(
        &connection,
        &project(),
        ada,
        "Fix both".into(),
        vec!["src/parser.rs".into(), "docs/notes.md".into()],
        NOW,
    )
    .expect("waiting on Grace, who waits on Ada, would close a cycle");
    assert_eq!(
        overlaps
            .iter()
            .map(|overlap| (
                overlap.theirs.work,
                overlap.mine.pattern.as_str(),
                overlap.left_out
            ))
            .collect::<Vec<_>>(),
        vec![(grace_work, "src/parser.rs", true)]
    );
    let claimed: Vec<String> = connection
        .prepare("SELECT pattern FROM coord_claims WHERE work=?1 ORDER BY pattern")
        .unwrap()
        .query_map([work], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(claimed, vec!["docs/notes.md".to_owned()]);
    let live = |who: AgentId| -> Vec<AgentId> {
        overlapping(&connection, &project(), who, "src/parser.rs")
            .unwrap()
            .into_iter()
            .filter(|claim| !claim.stale)
            .map(|claim| claim.agent)
            .collect()
    };
    assert_eq!(
        live(grace),
        Vec::<AgentId>::new(),
        "Grace keeps editing her own file"
    );
    assert_eq!(live(ada), vec![grace], "Ada's edits of it stay refused");
    assert_eq!(
        wait_cycle_notices(&connection, &project(), NOW).unwrap(),
        vec![(ada, grace, "src/parser.rs".to_owned())]
    );
}

/// OS-8 Wait is at least as strong as Block: a start on a file another agent's folder or
/// glob claim covers waits for that work, and its release prompts the waiting agent.
#[cfg(unix)]
#[test]
fn under_wait_a_start_on_a_file_a_held_folder_or_glob_covers_waits() {
    for held in ["src/", "src/*.rs"] {
        let connection = db();
        overlaps_set(&connection, Overlaps::Wait);
        let grace = agent(&connection, "sub", None);
        let ada = agent(&connection, "sub", None);
        let (grace_work, _) = start_on(&connection, grace, held).unwrap();
        assert_eq!(
            code(start_on(&connection, ada, "src/lexer.rs")),
            "coord_claim_waits",
            "{held}"
        );
        assert_eq!(
            latest_wait(&connection, ada).unwrap(),
            Some((grace, grace_work, "src/lexer.rs".to_owned())),
            "{held}"
        );
        transfer(
            &connection,
            grace_work,
            1,
            Transfer::Release,
            Party::Agent(grace),
            NOW,
        )
        .unwrap();
        assert_eq!(
            prompts(&connection, ada),
            vec![LEXER_FREE.to_owned()],
            "{held}"
        );
    }
}

/// OS-8 Wait is visible: a waiting agent's coordination state names the file it waits for
/// until the wait settles.
#[cfg(unix)]
#[test]
fn a_waiting_agent_s_state_names_the_file_it_waits_for() {
    let connection = db();
    overlaps_set(&connection, Overlaps::Wait);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let (grace_work, _) = start_on(&connection, grace, "src/lexer.rs").unwrap();
    assert_eq!(
        code(start_on(&connection, ada, "src/lexer.rs")),
        "coord_claim_waits"
    );
    let waits = |connection: &Connection| -> Vec<(AgentId, Option<String>)> {
        snapshot(connection, &project(), NOW)
            .unwrap()
            .agents
            .into_iter()
            .map(|state| (state.agent, state.waits_for))
            .collect()
    };
    assert_eq!(
        waits(&connection),
        vec![(grace, None), (ada, Some("src/lexer.rs".to_owned()))]
    );
    transfer(
        &connection,
        grace_work,
        1,
        Transfer::Release,
        Party::Agent(grace),
        NOW,
    )
    .unwrap();
    assert_eq!(waits(&connection), vec![(grace, None), (ada, None)]);
}

#[cfg(unix)]
#[test]
fn finishing_work_needs_evidence_and_the_lead_completes_review_ready_work() {
    let connection = db();
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    let other = agent(&connection, "sub", None);
    let (work_id, _) = start_work(
        &connection,
        &project(),
        sub,
        "Tests".into(),
        vec!["tests/".into()],
        NOW,
    )
    .unwrap();
    let end = |version, outcome, evidence: &str, by| {
        finish(
            &connection,
            work_id,
            version,
            outcome,
            evidence.into(),
            by,
            NOW,
        )
    };
    assert_eq!(
        code(end(1, Finish::ReviewReady, " ", sub)),
        "coord_text_invalid"
    );
    assert_eq!(
        code(end(1, Finish::ReviewReady, "Green.", lead)),
        "coord_transfer_refused"
    );
    assert_eq!(
        code(end(2, Finish::ReviewReady, "Green.", sub)),
        "coord_work_stale"
    );
    assert_eq!(end(1, Finish::ReviewReady, "Green.", sub).unwrap(), 2);
    assert_eq!(
        held(&connection, sub, work_id),
        1,
        "review-ready work keeps its claims until it lands (OS-8)"
    );
    assert_eq!(
        work(&connection, work_id).unwrap().state,
        WorkState::ReviewReady
    );
    assert_eq!(
        code(end(2, Finish::ReviewReady, "Again.", sub)),
        "coord_transfer_refused"
    );
    assert_eq!(
        code(end(2, Finish::Complete, "Mine.", other)),
        "coord_transfer_refused"
    );
    assert_eq!(end(2, Finish::Complete, "Reviewed.", lead).unwrap(), 3);
    assert_eq!(
        held(&connection, sub, work_id),
        1,
        "a reviewed sub-agent's claims hold until its change set closes (OS-8)"
    );
    let evidence: String = connection
        .query_row(
            "SELECT evidence FROM coord_work WHERE id=?1",
            [work_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "Reviewed.");
    assert_eq!(
        code(end(3, Finish::Closed, "Late.", lead)),
        "coord_transfer_invalid"
    );
    assert!(works(&connection, &project(), 10).unwrap().is_empty());
    let (open, _) = start_work(&connection, &project(), other, "Open".into(), vec![], NOW).unwrap();
    transfer(
        &connection,
        open,
        1,
        Transfer::Release,
        Party::Agent(other),
        NOW,
    )
    .unwrap();
    assert_eq!(
        code(finish(
            &connection,
            open,
            2,
            Finish::Closed,
            "No.".into(),
            other,
            NOW
        )),
        "coord_transfer_invalid"
    );
    assert_eq!(
        works(&connection, &project(), 10)
            .unwrap()
            .iter()
            .map(|found| found.id)
            .collect::<Vec<_>>(),
        vec![open]
    );
}

#[test]
fn a_claim_is_normalized_to_one_relative_spelling() {
    let normalize = super::claims::normalize;
    for (raw, stored) in [
        ("./a", "a"),
        ("a//b", "a/b"),
        ("a/./b", "a/b"),
        (" src/ ", "src/"),
        ("src//ui/", "src/ui/"),
        (".", "**"),
        ("./", "**"),
        ("**", "**"),
        ("module:  Auth Core ", "module:Auth Core"),
        ("domain:billing", "domain:billing"),
    ] {
        assert_eq!(normalize(raw).unwrap(), stored, "{raw:?}");
    }
    for raw in [
        "",
        "  ",
        "../a",
        "a/../b",
        "/etc/passwd",
        "a\\b",
        "a\nb",
        "module: - ",
        "domain:",
    ] {
        assert_eq!(code(normalize(raw)), "coord_claim_invalid", "{raw:?}");
    }
    assert_eq!(
        code(normalize(&"a".repeat(PATTERN_BYTES_MAX + 1))),
        "coord_claim_invalid"
    );
    let many: Vec<String> = (0..=CLAIMS_PER_WORK_MAX).map(|n| format!("f{n}")).collect();
    assert_eq!(
        code(super::claims::normalize_all(&many)),
        "coord_claims_too_many"
    );
    assert_eq!(
        super::claims::normalize_all(&["a".into(), "./a".into(), "b/".into()]).unwrap(),
        vec!["a".to_owned(), "b/".to_owned()]
    );
}

#[test]
fn a_glob_star_stays_in_its_folder_and_a_double_star_crosses_folders() {
    for (claim, path, covered) in [
        ("src/*.rs", "src/a.rs", true),
        ("src/*.rs", "src/ui/a.rs", false),
        ("src/**", "src/ui/deep/a.rs", true),
        ("src/**/a.rs", "src/a.rs", true),
        ("src/**/a.rs", "src/x/y/a.rs", true),
        ("src/**/a.rs", "src/x/b.rs", false),
        ("src/?.rs", "src/a.rs", true),
        ("src/?.rs", "src/ab.rs", false),
        ("src/?/a.rs", "src//a.rs", false),
        ("src/", "src/ui/a.rs", true),
        ("src/", "srcx/a.rs", false),
        ("**", "anything/at/all.rs", true),
        ("src/a.rs", "src/a.rs", true),
        ("src/a.rs", "src/a.rsx", false),
        ("module:auth", "auth", false),
    ] {
        assert_eq!(covers(claim, path), covered, "{claim} over {path}");
    }
    assert_eq!(
        path_scopes(&[
            "src/".into(),
            "lib/**/*.rs".into(),
            "a.rs".into(),
            "**".into(),
            "module:x".into()
        ])
        .unwrap(),
        vec![
            ("src/".to_owned(), "src/".to_owned()),
            ("lib/**/*.rs".to_owned(), "lib/".to_owned())
        ]
    );
}

#[cfg(unix)]
fn overlap(mine: &str, theirs: &str) -> Vec<String> {
    let connection = db();
    let first = agent(&connection, "sub", None);
    let second = agent(&connection, "sub", None);
    start_work(
        &connection,
        &project(),
        first,
        "Theirs".into(),
        vec![theirs.into()],
        NOW,
    )
    .unwrap();
    let (_, found) = start_work(
        &connection,
        &project(),
        second,
        "Mine".into(),
        vec![mine.into()],
        NOW,
    )
    .unwrap();
    found
        .into_iter()
        .flat_map(|overlap| overlap.reasons)
        .collect()
}

#[cfg(unix)]
#[test]
fn an_overlap_names_why_two_claims_meet() {
    for (mine, theirs, reason) in [
        ("src/a.rs", "src/a.rs", Some("same file src/a.rs")),
        ("src/a.rs", "src/b.rs", Some("same folder src")),
        ("a.rs", "b.rs", None),
        ("src/ui/a.rs", "src/", Some("src/ui/a.rs is inside src/")),
        (
            "src/ui/a.rs",
            "**",
            Some("src/ui/a.rs is inside the whole repository"),
        ),
        ("src/a.rs", "src/*.rs", Some("src/a.rs matches src/*.rs")),
        ("src/ui/a.rs", "src/*.rs", None),
        ("src/", "src/ui/", Some("src/ and src/ui/ nest")),
        ("src/", "lib/", None),
        (
            "src/",
            "src/**/*.rs",
            Some("src/ and src/**/*.rs may overlap"),
        ),
        (
            "src/ui/*.ts",
            "src/**",
            Some("src/ui/*.ts and src/** may overlap"),
        ),
        (
            "module:Auth Core",
            "module:auth-api",
            Some("shared module words: auth"),
        ),
        ("module:auth", "domain:auth", None),
        ("domain:billing", "domain:payments", None),
    ] {
        let expected: Vec<String> = reason.into_iter().map(str::to_owned).collect();
        assert_eq!(overlap(mine, theirs), expected, "{mine} vs {theirs}");
    }
}

/// DEC-117 Q7: a claim on review-ready work never goes stale, since it holds until the work
/// lands even after its agent's process ends, so the edit-time check still names it; a
/// quiet agent's claim on work it is still on goes stale as before.
#[cfg(unix)]
#[test]
fn a_quiet_agent_s_review_ready_claim_never_goes_stale_and_an_active_one_does() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let linus = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    let claim = |who, title: &str| {
        start_work(
            &connection,
            &project(),
            who,
            title.into(),
            vec!["pyproject.toml".into()],
            NOW,
        )
        .unwrap()
        .0
    };
    let grace_work = claim(grace, "Dependencies");
    claim(linus, "Lockfile");
    finish(
        &connection,
        grace_work,
        1,
        Finish::ReviewReady,
        "Green.".into(),
        grace,
        NOW,
    )
    .unwrap();
    let now = crate::team::now_ms();
    for quiet in [grace, linus] {
        connection
            .execute(
                "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,?2)
                 ON CONFLICT(agent) DO UPDATE SET last_seen=?2",
                params![quiet, now - 31 * MINUTE],
            )
            .unwrap();
    }
    let mut by: Vec<(AgentId, bool)> = overlapping(&connection, &project(), ada, "pyproject.toml")
        .unwrap()
        .iter()
        .map(|claim| (claim.agent, claim.stale))
        .collect();
    by.sort();
    assert_eq!(by, vec![(grace, false), (linus, true)]);
}

/// OS-8 M1: the master's review that completes or closes a sub-agent's review-ready work
/// keeps that work's claims, so the next agent starting on the same file still hears the
/// overlap while the sub-agent's change set waits for the owner.
#[cfg(unix)]
#[test]
fn the_master_completing_or_closing_review_ready_work_keeps_the_sub_agent_s_claims() {
    let connection = db();
    let master = agent(&connection, "master", None);
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    for (outcome, file) in [
        (Finish::Complete, "pyproject.toml"),
        (Finish::Closed, "uv.lock"),
    ] {
        let start = |who, title: &str| {
            start_work(
                &connection,
                &project(),
                who,
                title.into(),
                vec![file.into()],
                NOW,
            )
            .unwrap()
        };
        let (work, _) = start(grace, "Dependencies");
        finish(
            &connection,
            work,
            1,
            Finish::ReviewReady,
            "Green.".into(),
            grace,
            NOW,
        )
        .unwrap();
        finish(
            &connection,
            work,
            2,
            outcome,
            "Reviewed.".into(),
            master,
            NOW,
        )
        .unwrap();
        assert_eq!(held(&connection, grace, work), 1, "{outcome:?}");
        let (_, overlaps) = start(ada, "CLI");
        assert_eq!(
            overlaps.len(),
            1,
            "{outcome:?}: Grace's change set has not closed: {overlaps:?}"
        );
        assert_eq!(overlaps[0].theirs.work, work);
    }
}

/// OS-8: the master's own work has no sub-agent change set to wait for, so finishing it
/// releases its claims at every end, review-ready included, and nothing holds them forever.
#[cfg(unix)]
#[test]
fn a_master_s_own_finished_work_releases_its_claims_at_every_end() {
    let connection = db();
    let master = agent(&connection, "master", None);
    for (index, outcome) in [Finish::ReviewReady, Finish::Complete, Finish::Closed]
        .into_iter()
        .enumerate()
    {
        let (work, _) = start_work(
            &connection,
            &project(),
            master,
            "Plan".into(),
            vec![format!("docs/plan-{index}.md")],
            NOW,
        )
        .unwrap();
        finish(&connection, work, 1, outcome, "Done.".into(), master, NOW).unwrap();
        assert_eq!(held(&connection, master, work), 0, "{outcome:?}");
    }
}

/// Seeds `count` claims on one unit of work an agent finished as review-ready, as many
/// waiting change sets would hold them, each on its own file under `held/`. Takes the
/// database, the agent and the count. Returns the work.
#[cfg(unix)]
fn held_claims(connection: &Connection, who: AgentId, count: usize) -> WorkId {
    let (work, _) = start_work(
        connection,
        &project(),
        who,
        "Backlog".into(),
        vec!["held/0.rs".into()],
        NOW,
    )
    .unwrap();
    finish(
        connection,
        work,
        1,
        Finish::ReviewReady,
        "Green.".into(),
        who,
        NOW,
    )
    .unwrap();
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?4)
             INSERT INTO coord_claims(project,agent,work,pattern,created)
             SELECT ?1,?2,?3,'held/'||i||'.rs',?5 FROM n",
            params![PROJECT, who, work, count as i64 - 1, NOW],
        )
        .unwrap();
    assert_eq!(held(connection, who, work), count);
    work
}

/// F3: claims held for waiting change sets never crowd a newer claim out of the overlap
/// check, so a start on a file another agent claimed after 600 held claims still hears it,
/// and the edit-time check still names it.
#[cfg(unix)]
#[test]
fn a_newer_claim_still_overlaps_behind_more_than_512_held_ones() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let linus = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    held_claims(&connection, grace, 600);
    let (linus_work, _) = start_work(
        &connection,
        &project(),
        linus,
        "Lexer".into(),
        vec!["src/lexer.rs".into()],
        NOW,
    )
    .unwrap();
    let (_, overlaps) = start_work(
        &connection,
        &project(),
        ada,
        "Parser".into(),
        vec!["src/lexer.rs".into()],
        NOW,
    )
    .unwrap();
    assert_eq!(overlaps.len(), 1, "{overlaps:?}");
    assert_eq!(overlaps[0].theirs.work, linus_work);
    let holders: Vec<AgentId> = overlapping(&connection, &project(), ada, "src/lexer.rs")
        .unwrap()
        .iter()
        .map(|claim| claim.agent)
        .collect();
    assert_eq!(holders, vec![linus]);
}

/// F3: past the claims one overlap check reads, the check fails closed: a new start is
/// refused with a plain code and the file check says so, never a silent miss.
#[cfg(unix)]
#[test]
fn a_project_past_the_claims_one_check_reads_refuses_new_work_and_the_file_check() {
    let connection = db();
    let grace = agent(&connection, "sub", None);
    let ada = agent(&connection, "sub", None);
    held_claims(&connection, grace, 9_000);
    assert_eq!(
        code(start_work(
            &connection,
            &project(),
            ada,
            "Parser".into(),
            vec!["src/parser.rs".into()],
            NOW,
        )),
        "coord_claims_full"
    );
    assert_eq!(
        code(overlapping(&connection, &project(), ada, "src/parser.rs")),
        "coord_claims_full"
    );
    let works: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM coord_work WHERE owner_agent=?1",
            [ada],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(works, 0, "the refused start stored nothing");
}

#[cfg(unix)]
#[test]
fn a_lead_and_its_own_sub_agent_never_overlap_and_a_quiet_claim_goes_stale() {
    let connection = db();
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    let other = agent(&connection, "sub", None);
    start_work(
        &connection,
        &project(),
        lead,
        "Lead".into(),
        vec!["src/a.rs".into()],
        NOW,
    )
    .unwrap();
    let (_, found) = start_work(
        &connection,
        &project(),
        sub,
        "Sub".into(),
        vec!["src/a.rs".into()],
        NOW,
    )
    .unwrap();
    assert!(found.is_empty(), "{found:?}");
    let now = crate::team::now_ms();
    connection
        .execute(
            "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,?2)
             ON CONFLICT(agent) DO UPDATE SET last_seen=?2",
            params![lead, now - 31 * MINUTE],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,?2)
             ON CONFLICT(agent) DO UPDATE SET last_seen=?2",
            params![sub, now],
        )
        .unwrap();
    let covering = overlapping(&connection, &project(), other, "./src/a.rs").unwrap();
    let mut by: Vec<(AgentId, bool)> = covering
        .iter()
        .map(|claim| (claim.agent, claim.stale))
        .collect();
    by.sort();
    assert_eq!(by, vec![(lead, true), (sub, false)]);
    assert!(
        overlapping(&connection, &project(), sub, "src/a.rs")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        code(overlapping(&connection, &project(), other, "src/")),
        "coord_claim_invalid"
    );
    start_work(
        &connection,
        &project(),
        other,
        "Other".into(),
        vec!["src/a.rs".into()],
        now,
    )
    .unwrap();
    let pairs = |agent: AgentId| -> Vec<(AgentId, AgentId, Vec<String>)> {
        overlaps_of(&connection, &project(), agent, now)
            .unwrap()
            .into_iter()
            .map(|overlap| (overlap.mine.agent, overlap.theirs.agent, overlap.reasons))
            .collect()
    };
    let same = || vec!["same file src/a.rs".to_owned()];
    assert_eq!(
        pairs(other),
        vec![(other, lead, same()), (other, sub, same())]
    );
    assert_eq!(
        pairs(lead),
        vec![(lead, other, same())],
        "a lead's sweep never lists its own sub-agent"
    );
    assert_eq!(pairs(sub), vec![(sub, other, same())]);
}

#[test]
fn a_broad_claim_says_why_it_should_be_split() {
    let owned = |claims: &[&str]| {
        claims
            .iter()
            .map(|claim| (*claim).to_owned())
            .collect::<Vec<_>>()
    };
    assert!(
        breadth(&owned(&["src/a.rs", "module:auth"]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        breadth(&owned(&["."])).unwrap(),
        vec!["a claim covers the whole repository".to_owned()]
    );
    assert_eq!(
        breadth(&owned(&["*"])).unwrap(),
        vec!["a claim covers the whole repository".to_owned()]
    );
    assert_eq!(
        breadth(&owned(&["a", "b", "c", "d", "e", "f"])).unwrap(),
        vec!["6 files or folders (split above 5)".to_owned()]
    );
    assert_eq!(
        breadth(&owned(&[
            "module:a", "module:b", "module:c", "module:d", "domain:x", "domain:y", "domain:z"
        ]))
        .unwrap(),
        vec![
            "4 modules (split above 3)".to_owned(),
            "3 domains (split above 2)".to_owned()
        ]
    );
}

#[cfg(unix)]
#[test]
fn presence_follows_the_last_activity_and_a_wait_keeps_its_start() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let stopped = agent_in(&connection, PROJECT, "sub", None, "stopped");
    let current = |now| super::presence::current(&connection, sub, now).unwrap();
    assert_eq!(current(NOW + 4 * MINUTE), Presence::Live, "from its start");
    touch(&connection, sub, NOW).unwrap();
    for (quiet, presence) in [
        (0, Presence::Live),
        (5 * MINUTE - 1, Presence::Live),
        (5 * MINUTE, Presence::Idle),
        (30 * MINUTE, Presence::Away),
        (60 * MINUTE, Presence::Stopped),
    ] {
        assert_eq!(current(NOW + quiet), presence, "{quiet}");
    }
    touch(&connection, sub, NOW - MINUTE).unwrap();
    assert_eq!(
        current(NOW + 5 * MINUTE),
        Presence::Idle,
        "last_seen never goes back"
    );
    assert_eq!(
        super::presence::current(&connection, stopped, NOW).unwrap(),
        Presence::Stopped
    );
    assert_eq!(
        super::presence::current(&connection, 999, NOW).unwrap(),
        Presence::Stopped
    );
    assert_eq!(code(touch(&connection, 999, NOW)), "coord_agent_unknown");

    let waited = || -> (Option<String>, Option<i64>) {
        connection
            .query_row(
                "SELECT waiting,waiting_since FROM coord_activity WHERE agent=?1",
                [sub],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    set_waiting(&connection, sub, Some(Waiting::Question), NOW + 1).unwrap();
    set_waiting(&connection, sub, Some(Waiting::Question), NOW + 2).unwrap();
    assert_eq!(waited(), (Some("question".into()), Some(NOW + 1)));
    set_waiting(&connection, sub, Some(Waiting::Permission), NOW + 3).unwrap();
    assert_eq!(waited(), (Some("permission".into()), Some(NOW + 3)));
    set_waiting(&connection, sub, None, NOW + 4).unwrap();
    assert_eq!(waited(), (None, None));

    record_tool(&connection, sub, "graphify", NOW).unwrap();
    record_tool(&connection, sub, "graphify", NOW + 1).unwrap();
    let used: (i64, i64) = connection
        .query_row(
            "SELECT count,last FROM coord_tool_use WHERE agent=?1 AND tool='graphify'",
            [sub],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(used, (2, NOW + 1));
    for tool in [
        "",
        "has space",
        "tab\t",
        "t".repeat(TOOL_BYTES_MAX + 1).as_str(),
    ] {
        assert_eq!(
            code(record_tool(&connection, sub, tool, NOW)),
            "coord_tool_invalid",
            "{tool:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_snapshot_counts_chosen_tools_orders_owner_questions_and_sums_unread_edges() {
    let connection = db();
    let lead = agent(&connection, "sub", None);
    let sub = agent(&connection, "sub", Some(lead));
    for tool in [
        "ctx_search",
        "mcp__plugin_context-mode_context-mode__ctx_execute",
        "graphify",
        "Bash",
        "mcp__crazyagent__start_work",
    ] {
        record_tool(&connection, sub, tool, NOW).unwrap();
    }
    for (severity, body) in [
        (Severity::Low, "Low?"),
        (Severity::High, "High?"),
        (Severity::Normal, "Normal?"),
    ] {
        send(
            &connection,
            question(sub, QuestionFor::Me, severity, body),
            NOW,
        )
        .unwrap();
    }
    let first = send(
        &connection,
        note(Party::Owner, Party::Agent(sub), MessageKind::Note, "One."),
        NOW,
    )
    .unwrap();
    let second = send(
        &connection,
        note(Party::Owner, Party::Agent(sub), MessageKind::Note, "Two."),
        NOW,
    )
    .unwrap();
    mark(&connection, first, Receipt::Delivered, NOW).unwrap();
    mark(&connection, second, Receipt::Typed, NOW).unwrap();
    let snapshot = snapshot(&connection, &project(), NOW + 1).unwrap();
    let state = snapshot
        .agents
        .iter()
        .find(|state| state.agent == sub)
        .unwrap();
    let mut counted: Vec<(&str, _)> = state
        .tool_counts
        .iter()
        .map(|(tool, count)| (tool.as_str(), *count))
        .collect();
    counted.sort_unstable();
    assert_eq!(
        counted,
        [
            ("ctx_search", 1),
            ("graphify", 1),
            ("mcp__crazyagent__start_work", 1),
            ("mcp__plugin_context-mode_context-mode__ctx_execute", 1)
        ]
    );
    assert_eq!(state.unread, 2);
    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|state| state.agent)
            .collect::<Vec<_>>(),
        vec![lead, sub]
    );
    let bodies: Vec<&str> = snapshot
        .questions_for_owner
        .iter()
        .map(|message| message.body.as_str())
        .collect();
    assert_eq!(bodies, ["High?", "Normal?", "Low?"]);
    assert_eq!(
        snapshot.unread_edges.len(),
        2,
        "{:?}",
        snapshot.unread_edges
    );
    assert!(
        snapshot
            .unread_edges
            .contains(&(Party::Owner, Party::Agent(sub), 2, Receipt::Typed))
    );
    assert!(
        snapshot
            .unread_edges
            .contains(&(Party::Agent(sub), Party::Owner, 3, Receipt::Sent))
    );
}

#[cfg(unix)]
#[test]
fn coordination_settings_default_validate_and_keep_each_harness_column() {
    let connection = db();
    let project = project();
    let unknown = ProjectId::new("coord-unknown").unwrap();
    assert_eq!(
        settings::read(&connection, &project).unwrap(),
        CoordSettings::default()
    );
    let defaults = CoordSettings::default();
    assert_eq!(
        (defaults.questions_to, defaults.overlaps, defaults.nudges),
        (QuestionsTo::Lead, Overlaps::Warn, Nudges::Off)
    );
    for counted in [
        (0..=settings::COUNTED_TOOLS_MAX)
            .map(|n| format!("t{n}"))
            .collect::<Vec<_>>(),
        vec!["t".repeat(settings::COUNTED_TOOL_BYTES_MAX + 1)],
        vec!["has space".into()],
        vec!["a.b".into()],
        vec![String::new()],
        vec!["ctx_*".into(), "ctx_*".into()],
    ] {
        let invalid = CoordSettings {
            counted_tools: counted,
            ..CoordSettings::default()
        };
        assert_eq!(code(invalid.validate()), "coord_settings_invalid");
        assert_eq!(
            code(settings::write(&connection, &project, &invalid)),
            "coord_settings_invalid"
        );
    }
    let chosen = CoordSettings {
        overlaps: Overlaps::Block,
        counted_tools: vec!["t".repeat(settings::COUNTED_TOOL_BYTES_MAX), "Skill".into()],
        ..CoordSettings::default()
    };
    settings::write(&connection, &project, &chosen).unwrap();
    assert_eq!(settings::read(&connection, &project).unwrap(), chosen);
    assert_eq!(
        code(settings::write(&connection, &unknown, &chosen)),
        "coord_project_unknown"
    );
    connection
        .execute(
            "UPDATE coord_settings SET json=?2 WHERE project=?1",
            params![PROJECT, r#"{"questions_to":"lead","overlaps":"warn","nudges":true,"counted_tools":[],"extra":1}"#],
        )
        .unwrap();
    assert_eq!(
        code(settings::read(&connection, &project)),
        "coord_settings_invalid"
    );

    for harness in HarnessKind::ALL {
        assert_eq!(
            settings::mode(&connection, &project, harness).unwrap(),
            SubagentMode::AppSpawned
        );
        assert_eq!(
            settings::delivery(&connection, &project, harness).unwrap(),
            Delivery::Auto
        );
    }
    for harness in [HarnessKind::Codex, HarnessKind::Rcli] {
        assert_eq!(
            code(settings::set_mode(
                &connection,
                &project,
                harness,
                SubagentMode::Native
            )),
            "coord_native_unsupported"
        );
    }
    assert_eq!(
        code(settings::set_mode(
            &connection,
            &unknown,
            HarnessKind::ClaudeCode,
            SubagentMode::Native
        )),
        "coord_project_unknown"
    );
    settings::set_mode(
        &connection,
        &project,
        HarnessKind::ClaudeCode,
        SubagentMode::Native,
    )
    .unwrap();
    settings::set_delivery(
        &connection,
        &project,
        HarnessKind::ClaudeCode,
        Delivery::TypingOnly,
    )
    .unwrap();
    assert_eq!(
        settings::mode(&connection, &project, HarnessKind::ClaudeCode).unwrap(),
        SubagentMode::Native
    );
    assert_eq!(
        settings::delivery(&connection, &project, HarnessKind::ClaudeCode).unwrap(),
        Delivery::TypingOnly
    );
    assert_eq!(
        settings::mode(&connection, &project, HarnessKind::Codex).unwrap(),
        SubagentMode::AppSpawned
    );
    assert_eq!(
        HarnessKind::from_harness("claude"),
        Some(HarnessKind::ClaudeCode)
    );
    assert_eq!(HarnessKind::from_harness("gemini"), None);
}

/// T3-15: the Task-start reminder is On for a new project and for a row saved before the
/// setting existed, Off saves and reads back, and each option keeps its stored name and the
/// words settings show.
#[test]
fn the_task_start_reminder_is_on_by_default_and_for_older_rows_and_off_round_trips() {
    let connection = db();
    let project = project();
    assert_eq!(CoordSettings::default().task_start, TaskStart::On);
    assert_eq!(
        settings::read(&connection, &project).unwrap().task_start,
        TaskStart::On
    );
    let off = CoordSettings {
        task_start: TaskStart::Off,
        ..CoordSettings::default()
    };
    settings::write(&connection, &project, &off).unwrap();
    assert_eq!(settings::read(&connection, &project).unwrap(), off);
    connection
        .execute(
            "UPDATE coord_settings SET json=?2 WHERE project=?1",
            params![
                PROJECT,
                r#"{"questions_to":"lead","overlaps":"warn","nudges":"off","counted_tools":[]}"#
            ],
        )
        .unwrap();
    assert_eq!(
        settings::read(&connection, &project).unwrap().task_start,
        TaskStart::On,
        "a row saved before the setting existed reads On"
    );
    assert_eq!(
        TaskStart::ALL.map(|option| (option.as_str(), option.words())),
        [("on", "On"), ("off", "Off")]
    );
    for option in TaskStart::ALL {
        assert_eq!(TaskStart::parse(option.as_str()), Some(option));
    }
}

/// DEC-111 R3: each team role's skill choice validates with the skill name rule, keeps its
/// order through a save, and a row saved before the choice existed reads as All approved
/// skills for every role.
#[cfg(unix)]
#[test]
fn role_skill_choices_validate_save_and_default_for_rows_saved_before_them() {
    let connection = db();
    let project = project();
    let worker = |names: Vec<String>| CoordSettings {
        role_skills: RoleSkills {
            worker: Some(names),
            ..RoleSkills::default()
        },
        ..CoordSettings::default()
    };
    for names in [
        (0..=settings::ROLE_SKILLS_MAX)
            .map(|n| format!("skill-{n}"))
            .collect::<Vec<_>>(),
        vec!["Upper".into()],
        vec!["-lead".into()],
        vec!["tail-".into()],
        vec!["s".repeat(crate::plugins::skill_folder::NAME_BYTES_MAX + 1)],
        vec![String::new()],
        vec!["meal-tips".into(), "meal-tips".into()],
    ] {
        let invalid = worker(names);
        assert_eq!(code(invalid.validate()), "coord_settings_invalid");
        assert_eq!(
            code(settings::write(&connection, &project, &invalid)),
            "coord_settings_invalid"
        );
    }
    let chosen = CoordSettings {
        role_skills: RoleSkills {
            master: Some(vec!["meal-tips".into()]),
            lead: Some(Vec::new()),
            worker: Some(vec![
                "style-guide".into(),
                "s".repeat(crate::plugins::skill_folder::NAME_BYTES_MAX),
            ]),
        },
        ..CoordSettings::default()
    };
    settings::write(&connection, &project, &chosen).unwrap();
    let read = settings::read(&connection, &project).unwrap();
    assert_eq!(read, chosen);
    assert_eq!(
        SkillRole::ALL.map(|role| read.role_skills.of(role).map(<[String]>::len)),
        [Some(1), Some(0), Some(2)]
    );
    connection
        .execute(
            "UPDATE coord_settings SET json=?2 WHERE project=?1",
            params![PROJECT, r#"{"questions_to":"me","overlaps":"warn","nudges":false,"counted_tools":["Skill"]}"#],
        )
        .unwrap();
    let older = settings::read(&connection, &project).unwrap();
    assert_eq!(older.role_skills, RoleSkills::default());
    assert_eq!(
        (older.questions_to, older.counted_tools),
        (QuestionsTo::Me, vec!["Skill".to_owned()])
    );
}

/// Settings saved before the nudge options (DEC-111 R4) held `nudges` as a bool: a stored
/// `true` reads as TenMinutes and `false` as Off, each option is stored by its own name and
/// reads back, and a name this build does not know is refused.
#[cfg(unix)]
#[test]
fn an_old_true_nudge_setting_reads_as_ten_minutes() {
    let connection = db();
    let project = project();
    settings::write(&connection, &project, &CoordSettings::default()).unwrap();
    let stored = |json: &str| {
        connection
            .execute(
                "UPDATE coord_settings SET json=?2 WHERE project=?1",
                params![PROJECT, json],
            )
            .unwrap();
        settings::read(&connection, &project)
    };
    let old = |nudges: &str| {
        format!(
            r#"{{"questions_to":"lead","overlaps":"warn","nudges":{nudges},"counted_tools":["graphify"]}}"#
        )
    };
    assert_eq!(
        stored(&old("true")).map(|read| read.nudges),
        Ok(Nudges::TenMinutes)
    );
    assert_eq!(
        stored(&old("false")).map(|read| read.nudges),
        Ok(Nudges::Off)
    );
    for nudges in Nudges::ALL {
        let chosen = CoordSettings {
            nudges,
            ..CoordSettings::default()
        };
        settings::write(&connection, &project, &chosen).unwrap();
        let json: String = connection
            .query_row(
                "SELECT json FROM coord_settings WHERE project=?1",
                [PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            json.contains(&format!(r#""nudges":"{}""#, nudges.as_str())),
            "{json}"
        );
        assert_eq!(settings::read(&connection, &project).unwrap(), chosen);
    }
    assert_eq!(code(stored(&old(r#""hourly""#))), "coord_settings_invalid");
}

#[cfg(unix)]
#[test]
fn a_project_tool_switch_overrides_the_global_one_and_restores_by_scope() {
    let connection = db();
    let project = project();
    let other = ProjectId::new(OTHER).unwrap();
    let server = tool_switches::CRAZYAGENT_SERVER;
    let get = |scope: Option<&ProjectId>, tool: &str| {
        tool_switches::get(&connection, scope, server, tool).unwrap()
    };
    assert_eq!(get(Some(&project), "spawn"), Switch::On);
    tool_switches::set(&connection, None, server, "spawn", Switch::Off).unwrap();
    tool_switches::set(&connection, None, server, "stop", Switch::Off).unwrap();
    assert_eq!(get(Some(&project), "spawn"), Switch::Off);
    tool_switches::set(&connection, Some(&project), server, "spawn", Switch::On).unwrap();
    tool_switches::set(&connection, Some(&project), server, "merge", Switch::Hidden).unwrap();
    assert_eq!(get(Some(&project), "spawn"), Switch::On);
    assert_eq!(get(None, "spawn"), Switch::Off);
    assert_eq!(get(Some(&other), "spawn"), Switch::Off);
    assert_eq!(
        tool_switches::overrides(&connection, Some(&project), server).unwrap(),
        vec![
            ("merge".to_owned(), Switch::Hidden),
            ("spawn".to_owned(), Switch::On)
        ]
    );
    assert_eq!(
        tool_switches::off_tools(&connection, &project, server).unwrap(),
        vec!["merge".to_owned(), "stop".to_owned()]
    );
    assert_eq!(
        tool_switches::off_tools(&connection, &other, server).unwrap(),
        vec!["spawn".to_owned(), "stop".to_owned()]
    );
    tool_switches::restore_defaults(&connection, Some(&project), server).unwrap();
    assert!(
        tool_switches::overrides(&connection, Some(&project), server)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        tool_switches::overrides(&connection, None, server)
            .unwrap()
            .len(),
        2
    );
    for (scope_server, tool) in [("bad server", "spawn"), (server, "bad tool"), (server, "")] {
        assert_eq!(
            code(tool_switches::get(&connection, None, scope_server, tool)),
            "coord_tool_invalid"
        );
        assert_eq!(
            code(tool_switches::set(
                &connection,
                None,
                scope_server,
                tool,
                Switch::Off
            )),
            "coord_tool_invalid"
        );
    }
    assert_eq!(
        code(tool_switches::set(
            &connection,
            Some(&ProjectId::new("coord-unknown").unwrap()),
            server,
            "spawn",
            Switch::Off
        )),
        "coord_project_unknown"
    );
}

#[cfg(unix)]
#[test]
fn a_coord_write_inside_a_caller_s_transaction_rolls_back_with_it() {
    let connection = db();
    let sub = agent(&connection, "sub", None);
    let count = |connection: &Connection| -> i64 {
        connection
            .query_row("SELECT COUNT(*) FROM coord_messages", [], |row| row.get(0))
            .unwrap()
    };
    let outer = connection.unchecked_transaction().unwrap();
    send(
        &outer,
        note(Party::Owner, Party::Agent(sub), MessageKind::Note, "Kept?"),
        NOW,
    )
    .unwrap();
    assert_eq!(count(&outer), 1);
    outer.rollback().unwrap();
    assert_eq!(count(&connection), 0);

    let outer = connection.unchecked_transaction().unwrap();
    send(
        &outer,
        note(Party::Owner, Party::Agent(sub), MessageKind::Note, "Kept."),
        NOW,
    )
    .unwrap();
    assert_eq!(
        code(send(
            &outer,
            note(
                Party::Agent(sub),
                Party::Agent(sub),
                MessageKind::Note,
                "Self."
            ),
            NOW
        )),
        "coord_message_self"
    );
    {
        let change = Change::begin(&outer).unwrap();
        send(
            change.db(),
            note(
                Party::Owner,
                Party::Agent(sub),
                MessageKind::Note,
                "Dropped.",
            ),
            NOW,
        )
        .unwrap();
    }
    outer.commit().unwrap();
    let bodies: Vec<String> = connection
        .prepare("SELECT body FROM coord_messages ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(bodies, ["Kept."]);
    assert!(connection.is_autocommit());
}

#[cfg(unix)]
#[test]
fn removing_a_project_forgets_its_coordination_and_leaves_other_projects() {
    let connection = db();
    let project = project();
    let other = ProjectId::new(OTHER).unwrap();
    let seed = |project: &ProjectId| {
        let live = agent_in(&connection, project.as_str(), "sub", None, "working");
        let ended = agent_in(&connection, project.as_str(), "sub", None, "stopped");
        let (work_id, _) = start_work(
            &connection,
            project,
            live,
            "W".into(),
            vec!["a.rs".into()],
            NOW,
        )
        .unwrap();
        transfer(
            &connection,
            work_id,
            1,
            Transfer::Release,
            Party::Agent(live),
            NOW,
        )
        .unwrap();
        send(
            &connection,
            NewMessage {
                project: project.clone(),
                ..note(Party::Owner, Party::Agent(live), MessageKind::Note, "Hi.")
            },
            NOW,
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,?2)",
                params![ended, NOW],
            )
            .unwrap();
        record_tool(&connection, live, "graphify", NOW).unwrap();
        connection
            .execute(
                "INSERT INTO coord_tool_use(agent,tool,count,last) VALUES (?1,'graphify',1,?2)",
                params![ended, NOW],
            )
            .unwrap();
        settings::write(&connection, project, &CoordSettings::default()).unwrap();
        settings::set_mode(
            &connection,
            project,
            HarnessKind::ClaudeCode,
            SubagentMode::Native,
        )
        .unwrap();
        tool_switches::set(
            &connection,
            Some(project),
            tool_switches::CRAZYAGENT_SERVER,
            "spawn",
            Switch::Off,
        )
        .unwrap();
        (live, ended)
    };
    let (live, ended) = seed(&project);
    let (other_live, other_ended) = seed(&other);
    tool_switches::set(
        &connection,
        None,
        tool_switches::CRAZYAGENT_SERVER,
        "stop",
        Switch::Off,
    )
    .unwrap();
    let rows = |table: &str, project: &str| -> i64 {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
                [project],
                |row| row.get(0),
            )
            .unwrap()
    };
    let tables = [
        "coord_work",
        "coord_messages",
        "coord_claims",
        "coord_audit",
        "coord_settings",
        "coord_harness_settings",
    ];
    for table in tables {
        assert!(
            rows(table, PROJECT) > 0 && rows(table, OTHER) > 0,
            "{table}"
        );
    }
    let before: Vec<i64> = tables.iter().map(|table| rows(table, OTHER)).collect();
    forget_project(&connection, &project).unwrap();
    for table in tables {
        assert_eq!(rows(table, PROJECT), 0, "{table}");
    }
    assert_eq!(
        tables
            .iter()
            .map(|table| rows(table, OTHER))
            .collect::<Vec<_>>(),
        before
    );
    let kept = |table: &str, agent: AgentId| -> bool {
        connection
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE agent=?1)"),
                [agent],
                |row| row.get(0),
            )
            .unwrap()
    };
    for table in ["coord_activity", "coord_tool_use"] {
        assert!(!kept(table, ended), "{table}");
        assert!(kept(table, live), "a live agent keeps its {table}");
        assert!(
            kept(table, other_live) && kept(table, other_ended),
            "{table}"
        );
    }
    let switches = |scope: &str| -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM tool_switches WHERE scope_project=?1",
                [scope],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        (switches(PROJECT), switches(OTHER), switches("")),
        (0, 1, 1)
    );
}

/// b19 P2 r2: a coordination database made before `coord_messages.turn` gets the column
/// when the engine opens it, every message kept as it was, and a second open changes
/// nothing.
#[cfg(unix)]
#[test]
fn a_database_made_before_the_turn_column_gets_it_on_open_and_keeps_its_messages() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE coord_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                from_kind TEXT NOT NULL,
                from_agent INTEGER,
                to_kind TEXT NOT NULL,
                to_agent INTEGER,
                kind TEXT NOT NULL,
                q_for TEXT,
                q_severity TEXT,
                work INTEGER,
                reply_to INTEGER,
                body TEXT NOT NULL,
                idem_key TEXT,
                receipt TEXT NOT NULL,
                created INTEGER NOT NULL,
                receipt_at INTEGER NOT NULL,
                UNIQUE(from_agent, idem_key)
            );
            INSERT INTO coord_messages(project,from_kind,to_kind,to_agent,kind,body,receipt,created,receipt_at)
            VALUES ('coord-units','engine','agent',7,'note','kept','delivered',1,2);",
        )
        .unwrap();
    let row = |connection: &Connection| -> rusqlite::Result<(String, i64, Option<String>)> {
        connection.query_row(
            "SELECT receipt, receipt_at, turn FROM coord_messages WHERE body='kept'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    };
    for _ in 0..2 {
        ensure(&connection).unwrap();
        assert_eq!(
            row(&connection).ok(),
            Some(("delivered".to_owned(), 2, None)),
            "the open added the turn column and kept the message"
        );
    }
}
