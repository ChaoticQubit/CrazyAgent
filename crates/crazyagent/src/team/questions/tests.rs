#![cfg(unix)]

use super::*;
use crate::coord::NewMessage;

const NOW: i64 = 1_800_000_000_000;

/// One real engine database with the team and coordination tables, holding one project
/// whose agents put questions to the owner.
struct Engine {
    _dir: tempfile::TempDir,
    connection: Connection,
    project: ProjectId,
}

impl Engine {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let connection = Connection::open(dir.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        super::super::initialize(&connection).unwrap();
        crate::coord::ensure(&connection).unwrap();
        Self {
            _dir: dir,
            connection,
            project: ProjectId::new("questions-1").unwrap(),
        }
    }

    /// Adds one team agent of a project with a status. Inputs: the project, the role and
    /// the status. Output: its id.
    fn agent_in(&self, project: &ProjectId, role: &str, status: &str) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
                 VALUES (?1, ?2, 'claude-code', 1, ?3, 1, 1)",
                params![project.as_str(), role, status],
            )
            .unwrap();
        let id = self.connection.last_insert_rowid();
        crate::coord::touch(&self.connection, id, NOW).unwrap();
        id
    }

    fn agent(&self, status: &str) -> i64 {
        self.agent_in(&self.project.clone(), "sub", status)
    }

    /// Sends one question from an agent to the owner through the real coordination store.
    /// Inputs: the asker, the severity and the body. Output: the message id.
    fn ask(&self, agent: i64, severity: Severity, body: &str) -> i64 {
        crate::coord::send(
            &self.connection,
            NewMessage {
                project: self.project.clone(),
                from: Party::Agent(agent),
                to: Party::Owner,
                kind: MessageKind::Question,
                question: Some((QuestionFor::Me, severity)),
                work: None,
                reply_to: None,
                body: body.into(),
                idempotency_key: None,
            },
            NOW,
        )
        .unwrap()
    }

    fn tick(&mut self, now: i64) {
        tick(&mut self.connection, now).unwrap();
    }

    fn answer(&mut self, message: i64, text: &str, now: i64) -> Outcome {
        let project = self.project.clone();
        let tx = self.connection.transaction().unwrap();
        let outcome = answer(&tx, &project, message, text, now).unwrap();
        tx.commit().unwrap();
        outcome
    }

    fn inbox(&self) -> Inbox {
        inbox(&self.connection, &self.project).unwrap()
    }

    /// The owner's notifications of the project, oldest first: kind and when seen.
    fn notifications(&self) -> Vec<(String, Option<i64>)> {
        self.connection
            .prepare("SELECT kind, seen_ms FROM team_notifications WHERE project=?1 ORDER BY id")
            .unwrap()
            .query_map([self.project.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn waiting(&self, agent: i64) -> Option<(Waiting, i64)> {
        waiting_now(&self.connection, agent).unwrap()
    }
}

fn kinds(notifications: &[(String, Option<i64>)]) -> Vec<&str> {
    notifications
        .iter()
        .map(|(kind, _)| kind.as_str())
        .collect()
}

#[test]
fn the_inbox_orders_by_severity_and_bounds_its_reply() {
    let engine = Engine::new();
    let asker = engine.agent("working");
    let low = engine.ask(asker, Severity::Low, "Low first?");
    let normal = engine.ask(asker, Severity::Normal, "Normal older?");
    let high = engine.ask(asker, Severity::High, "High?");
    let newer = engine.ask(asker, Severity::Normal, "Normal newer?");
    let master = engine.agent_in(&engine.project.clone(), "master", "working");
    let routed = crate::coord::send(
        &engine.connection,
        NewMessage {
            project: engine.project.clone(),
            from: Party::Agent(asker),
            to: Party::Agent(master),
            kind: MessageKind::Question,
            question: Some((QuestionFor::Master, Severity::High)),
            work: None,
            reply_to: None,
            body: "Not for the owner.".into(),
            idempotency_key: None,
        },
        NOW,
    )
    .unwrap();
    let to_agent: Option<i64> = engine
        .connection
        .query_row(
            "SELECT to_agent FROM coord_messages WHERE id=?1",
            [routed],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(to_agent, Some(master));
    let inbox = engine.inbox();
    assert_eq!(
        inbox
            .questions
            .iter()
            .map(|question| question.id)
            .collect::<Vec<_>>(),
        [high, normal, newer, low]
    );
    assert_eq!(inbox.more, 0);
    let first = &inbox.questions[0];
    assert_eq!((first.agent, first.text.as_str()), (asker, "High?"));
    assert_eq!(first.terminal, AgentId::team(asker).ok());
    assert!(!first.label.is_empty());

    let wide = Engine::new();
    let asker = wide.agent("working");
    let body = "q".repeat(3900);
    for _ in 0..40 {
        wide.ask(asker, Severity::Normal, &body);
    }
    let inbox = wide.inbox();
    assert!(inbox.questions.len() <= QUESTIONS_SHOWN_MAX);
    assert_eq!(inbox.questions.len() as u32 + inbox.more, 40);
    assert!(encoded_len(&inbox).unwrap() <= STATUS_REPLY_BYTES_MAX);
    assert!(
        inbox
            .questions
            .iter()
            .all(|question| question.text.len() < body.len())
    );
}

#[test]
fn a_high_question_notifies_once_and_pauses_the_live_asker() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    let stopped = engine.agent("stopped");
    engine.ask(asker, Severity::High, "May I force-push main?");
    engine.ask(stopped, Severity::High, "Asked before stopping?");
    engine.tick(NOW + 1);
    assert_eq!(
        kinds(&engine.notifications()),
        ["urgent_question", "urgent_question"]
    );
    assert_eq!(engine.waiting(asker), Some((Waiting::Question, NOW + 1)));
    assert_eq!(
        engine.waiting(stopped),
        None,
        "a stopped asker is not paused"
    );
    assert!(
        engine.inbox().waiting.is_empty(),
        "the pause the question set is not a terminal wait"
    );
    engine.tick(NOW + 2);
    assert_eq!(engine.notifications().len(), 2, "one alert per question");
    assert_eq!(engine.waiting(asker), Some((Waiting::Question, NOW + 1)));
}

#[test]
fn a_normal_question_only_notifies() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    engine.ask(asker, Severity::Normal, "Which word, Hello or Hi?");
    engine.tick(NOW + 1);
    assert_eq!(kinds(&engine.notifications()), ["question"]);
    assert_eq!(engine.waiting(asker), None);
    engine.tick(NOW + 2);
    assert_eq!(engine.notifications().len(), 1);
}

#[test]
fn a_low_question_never_notifies_and_shows_its_assumption() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    let id = engine.ask(
        asker,
        Severity::Low,
        "Which port?\nAssumption: 8080 until you say.",
    );
    engine.tick(NOW + 1);
    assert!(engine.notifications().is_empty());
    assert_eq!(engine.waiting(asker), None);
    let inbox = engine.inbox();
    assert_eq!(inbox.questions.len(), 1);
    let question = &inbox.questions[0];
    assert_eq!(question.id, id);
    assert_eq!(question.severity, Severity::Low);
    assert_eq!(question.text, "Which port?");
    assert_eq!(question.assumption.as_deref(), Some("8080 until you say."));
    assert_eq!(
        split_assumption("Assumption: only this."),
        ("Assumption: only this.".to_owned(), None)
    );
}

#[test]
fn answering_a_high_question_lifts_the_pause_it_set_but_not_a_later_native_wait() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    let first = engine.ask(asker, Severity::High, "First?");
    engine.tick(NOW + 1);
    assert_eq!(engine.waiting(asker), Some((Waiting::Question, NOW + 1)));
    assert_eq!(engine.answer(first, "Yes.", NOW + 2), Outcome::Applied);
    assert_eq!(engine.waiting(asker), None);
    assert_eq!(
        engine.notifications(),
        [("urgent_question".to_owned(), Some(NOW + 2))]
    );
    assert!(engine.inbox().questions.is_empty());
    let replies: i64 = engine
        .connection
        .query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE kind='reply' AND reply_to=?1 AND from_kind='owner' AND body='Yes.'",
            [first],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replies, 1);

    let second = engine.ask(asker, Severity::High, "Second?");
    engine.tick(NOW + 3);
    assert_eq!(engine.waiting(asker), Some((Waiting::Question, NOW + 3)));
    crate::coord::set_waiting(
        &engine.connection,
        asker,
        Some(Waiting::Permission),
        NOW + 4,
    )
    .unwrap();
    assert_eq!(engine.answer(second, "No.", NOW + 5), Outcome::Applied);
    assert_eq!(
        engine.waiting(asker),
        Some((Waiting::Permission, NOW + 4)),
        "the answer never lifts a wait the question did not set"
    );
}

#[test]
fn a_terminal_wait_alerts_once_and_its_notification_is_seen_when_it_ends() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    crate::coord::set_waiting(&engine.connection, asker, Some(Waiting::Permission), NOW).unwrap();
    engine.tick(NOW + 1);
    assert_eq!(
        engine.notifications(),
        [("waiting_on_you".to_owned(), None)]
    );
    let inbox = engine.inbox();
    assert_eq!(
        inbox
            .waiting
            .iter()
            .map(|waiting| (waiting.agent, waiting.waiting, waiting.since_ms))
            .collect::<Vec<_>>(),
        [(asker, Waiting::Permission, NOW)]
    );
    engine.tick(NOW + 2);
    assert_eq!(engine.notifications().len(), 1, "one alert per wait");
    crate::coord::set_waiting(&engine.connection, asker, None, NOW + 3).unwrap();
    engine.tick(NOW + 4);
    assert_eq!(
        engine.notifications(),
        [("waiting_on_you".to_owned(), Some(NOW + 4))]
    );
    assert!(engine.inbox().waiting.is_empty());
    let rows: i64 = engine
        .connection
        .query_row("SELECT COUNT(*) FROM team_owner_alerts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn answering_a_closed_or_other_project_question_is_refused() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    let id = engine.ask(asker, Severity::Normal, "Once?");
    assert_eq!(engine.answer(id, "Yes.", NOW + 1), Outcome::Applied);
    assert_eq!(
        engine.answer(id, "Again.", NOW + 2),
        rejected("coord_question_closed")
    );
    assert_eq!(
        engine.answer(id + 1000, "Nobody.", NOW + 3),
        rejected("coord_question_missing")
    );
    let elsewhere = ProjectId::new("questions-2").unwrap();
    let foreign = engine.agent_in(&elsewhere, "sub", "working");
    let theirs = crate::coord::send(
        &engine.connection,
        NewMessage {
            project: elsewhere.clone(),
            from: Party::Agent(foreign),
            to: Party::Owner,
            kind: MessageKind::Question,
            question: Some((QuestionFor::Me, Severity::Normal)),
            work: None,
            reply_to: None,
            body: "Theirs?".into(),
            idempotency_key: None,
        },
        NOW,
    )
    .unwrap();
    assert_eq!(
        engine.answer(theirs, "Not mine.", NOW + 4),
        rejected("coord_question_missing")
    );
    for text in ["   ", &"a".repeat(crate::coord::BODY_BYTES_MAX + 1)] {
        let action = super::super::TeamAction::AnswerQuestion {
            project_id: engine.project.clone(),
            message: id,
            text: text.to_owned(),
        };
        assert_eq!(action.validate().unwrap_err().0, "team_answer_invalid");
    }
}

/// Every inbox question names whom its asker meant it for (OP-50): Me for a question to
/// the owner, Master for one the master asks for itself, and the asker's lead for a lead
/// question while the project sends lead questions to the owner.
#[test]
fn each_inbox_question_names_whom_its_asker_meant_it_for() {
    use crate::coord::settings::{CoordSettings, QuestionsTo};
    let engine = Engine::new();
    let project = engine.project.clone();
    let master = engine.agent_in(&project, "master", "working");
    let lead = engine.agent("working");
    let worker = engine.agent("working");
    engine
        .connection
        .execute(
            "UPDATE team_agents SET parent=?1 WHERE id=?2",
            params![lead, worker],
        )
        .unwrap();
    let row = crate::protocol::StoredProject {
        project_id: project.clone(),
        spec: crate::protocol::ProjectSpec {
            name: "Questions".into(),
            repositories: vec![],
        },
    };
    engine
        .connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![project.as_str(), serde_json::to_string(&row).unwrap()],
        )
        .unwrap();
    crate::coord::settings::write(
        &engine.connection,
        &project,
        &CoordSettings {
            questions_to: QuestionsTo::Me,
            ..CoordSettings::default()
        },
    )
    .unwrap();
    let ask = |from: i64, target: QuestionFor, body: &str| {
        crate::coord::send(
            &engine.connection,
            NewMessage {
                project: project.clone(),
                from: Party::Agent(from),
                to: Party::Owner,
                kind: MessageKind::Question,
                question: Some((target, Severity::Normal)),
                work: None,
                reply_to: None,
                body: body.into(),
                idempotency_key: None,
            },
            NOW,
        )
        .unwrap()
    };
    let mine = ask(worker, QuestionFor::Me, "For me?");
    let masters = ask(master, QuestionFor::Master, "For the master?");
    let leads = ask(worker, QuestionFor::Lead, "For the lead?");
    let lead_label = asker(&engine.connection, &project, lead).unwrap().label;
    assert_ne!(lead_label, AgentRole::Master.agent_label(master));
    let inbox = engine.inbox();
    assert_eq!(
        inbox
            .questions
            .iter()
            .map(|question| (question.id, question.asked_for, question.for_label.as_str()))
            .collect::<Vec<_>>(),
        [
            (mine, QuestionFor::Me, OWNER_LABEL),
            (masters, QuestionFor::Master, "Master"),
            (leads, QuestionFor::Lead, lead_label.as_str()),
        ]
    );
}

impl Engine {
    /// Registers the project the way onboarding does, so its owner questions can be logged
    /// in the questions log. Output: nothing.
    fn register(&self) {
        let folder = self._dir.path().join("project");
        std::fs::create_dir_all(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        crate::ide::workspace::tests::add_project(&self.connection, self.project.as_str(), &folder);
    }

    /// The project's logged questions linked to a team message: id, asker, question,
    /// context, status and closer, oldest first.
    fn linked(&self) -> Vec<crate::questions::Question> {
        let page = crate::questions::page(&self.connection, &self.project, None).unwrap();
        let ids: Vec<i64> = self
            .connection
            .prepare("SELECT id FROM questions_log WHERE coord_message IS NOT NULL ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut found: Vec<_> = page
            .items
            .into_iter()
            .filter(|question| ids.contains(&(question.id as i64)))
            .collect();
        found.sort_by_key(|question| question.id);
        found
    }

    fn receipt(&self, message: i64) -> String {
        self.connection
            .query_row(
                "SELECT receipt FROM coord_messages WHERE id=?1",
                [message],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The owner's team replies to one message, oldest first.
    fn replies(&self, message: i64) -> Vec<String> {
        self.connection
            .prepare("SELECT body FROM coord_messages WHERE reply_to=?1 AND kind='reply' AND from_kind='owner' ORDER BY id")
            .unwrap()
            .query_map([message], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn alert_settled(&self, message: i64) -> bool {
        self.connection
            .query_row(
                "SELECT settled IS NOT NULL FROM team_owner_alerts WHERE source='question' AND ref=?1",
                [message],
                |row| row.get(0),
            )
            .unwrap()
    }
}

/// F12: a question an agent put to the owner through the team is logged once in the
/// questions log under its asker, with its assumption as context; the team inbox stops
/// listing it, and its only notification is the team alert's.
#[test]
fn a_team_question_to_the_owner_is_logged_once_where_every_question_waits() {
    let mut engine = Engine::new();
    engine.register();
    let asker = engine.agent("working");
    let message = engine.ask(
        asker,
        Severity::High,
        "Ship on Friday?\nAssumption: we ship Monday.",
    );
    engine.tick(NOW);
    engine.tick(NOW + 1);
    let linked = engine.linked();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].asker, format!("agent:{asker}"));
    assert_eq!(linked[0].question, "Ship on Friday?");
    assert_eq!(linked[0].kind, crate::questions::QuestionKind::Text);
    assert!(
        linked[0]
            .context
            .as_deref()
            .is_some_and(|context| context.contains("we ship Monday.")),
        "{:?}",
        linked[0].context
    );
    assert!(
        engine.inbox().questions.is_empty(),
        "the owner sees it once"
    );
    assert_eq!(kinds(&engine.notifications()), ["urgent_question"]);
    assert_eq!(engine.receipt(message), "sent");
    let master = engine.agent_in(&engine.project.clone(), "master", "working");
    let own = crate::coord::send(
        &engine.connection,
        NewMessage {
            project: engine.project.clone(),
            from: Party::Agent(master),
            to: Party::Owner,
            kind: MessageKind::Question,
            question: Some((QuestionFor::Me, Severity::Normal)),
            work: None,
            reply_to: None,
            body: "Which region?".into(),
            idempotency_key: None,
        },
        NOW,
    )
    .unwrap();
    engine.tick(NOW + 2);
    let linked = engine.linked();
    assert_eq!(linked.len(), 2);
    assert_eq!(
        linked[1].asker,
        format!("master:{master}"),
        "the master's own, by role"
    );
    assert_eq!(engine.receipt(own), "sent");
}

/// F12, CM#1143: a team question that was already open and alerted before this engine
/// (no project row then) is logged on a later tick.
#[test]
fn an_open_team_question_from_before_is_logged_on_a_later_tick() {
    let mut engine = Engine::new();
    let asker = engine.agent("working");
    engine.ask(asker, Severity::High, "Old question?");
    engine.tick(NOW);
    assert!(engine.linked().is_empty());
    engine.register();
    engine.tick(NOW + 1);
    assert_eq!(engine.linked().len(), 1);
}

/// F12: the owner's answer in the Questions view reaches the asker once, as one team reply
/// from the owner, and the next tick lifts the pause and settles the alert; no chat message
/// and no typed note repeat it.
#[test]
fn an_answer_in_the_questions_view_replies_once_through_the_team() {
    let mut engine = Engine::new();
    engine.register();
    let asker = engine.agent("working");
    let message = engine.ask(asker, Severity::High, "Ship on Friday?");
    engine.tick(NOW);
    assert_eq!(
        engine.waiting(asker).map(|(kind, _)| kind),
        Some(Waiting::Question)
    );
    let id = engine.linked()[0].id;
    let answer = crate::questions::Answer {
        picks: Vec::new(),
        comment: Some("Friday is fine.".into()),
    };
    assert_eq!(
        crate::questions::answer(&engine.connection, id, 0, &answer).unwrap(),
        crate::questions::Settlement::Settled
    );
    assert_eq!(engine.replies(message), ["Friday is fine."]);
    assert_eq!(engine.receipt(message), "replied");
    engine.tick(NOW + 1);
    assert!(engine.alert_settled(message));
    assert_eq!(engine.waiting(asker), None);
    assert!(
        crate::questions::agent_notes(&engine.connection, &engine.project, asker)
            .unwrap()
            .is_empty()
    );
    let chat: i64 = engine
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_messages WHERE project=?1",
            [engine.project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(chat, 0);
    assert_eq!(engine.replies(message).len(), 1);
}

/// F12: an answer given in the team inbox settles the linked question on the next tick with
/// the owner's words and tells nobody again.
#[test]
fn an_answer_in_the_team_inbox_settles_the_linked_question() {
    let mut engine = Engine::new();
    engine.register();
    let asker = engine.agent("working");
    let message = engine.ask(asker, Severity::Normal, "Which region?");
    engine.tick(NOW);
    assert_eq!(engine.answer(message, "EU", NOW + 1), Outcome::Applied);
    engine.tick(NOW + 2);
    let linked = engine.linked();
    assert_eq!(linked[0].status, crate::questions::QuestionStatus::Answered);
    assert_eq!(linked[0].answer.as_deref(), Some("EU"));
    assert_eq!(engine.replies(message), ["EU"]);
    assert!(
        crate::questions::agent_notes(&engine.connection, &engine.project, asker)
            .unwrap()
            .is_empty()
    );
}

/// F12: dismissing a linked question tells its asker once through the team, and a master
/// withdrawing one ends its team message from any open receipt, Read included, so the
/// alert settles and the asker hears it once as a typed note.
#[test]
fn dismissed_and_withdrawn_team_questions_close_both_sides() {
    let mut engine = Engine::new();
    engine.register();
    let asker = engine.agent("working");
    let master = engine.agent_in(&engine.project.clone(), "master", "working");
    let dismissed = engine.ask(asker, Severity::Normal, "Rename the repo?");
    let withdrawn = engine.ask(asker, Severity::Normal, "Which region?");
    engine.tick(NOW);
    crate::coord::mark(&engine.connection, withdrawn, Receipt::Read, NOW + 1).unwrap();
    let linked = engine.linked();
    assert_eq!(
        crate::questions::dismiss(&engine.connection, linked[0].id, 0).unwrap(),
        crate::questions::Settlement::Settled
    );
    assert_eq!(
        crate::questions::withdraw(
            &engine.connection,
            &engine.project,
            &crate::questions::Asker::Master(master),
            linked[1].id,
            0
        )
        .unwrap(),
        crate::questions::Settlement::Settled
    );
    assert_eq!(engine.replies(dismissed).len(), 1);
    assert_eq!(engine.receipt(dismissed), "replied");
    assert!(engine.replies(withdrawn).is_empty());
    assert_eq!(engine.receipt(withdrawn), "expired");
    engine.tick(NOW + 2);
    assert!(engine.alert_settled(dismissed));
    assert!(engine.alert_settled(withdrawn));
    let notes = crate::questions::agent_notes(&engine.connection, &engine.project, asker).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].text.starts_with("The master withdrew"),
        "{}",
        notes[0].text
    );
    let closed = engine.linked();
    assert_eq!(
        closed[1].closed_by,
        Some(crate::questions::ClosedBy::Master)
    );
}

/// F12: an owner answer to a linked team question whose team message ended before the tick
/// noticed is not lost: it reaches the asker once the way an unlinked answer does.
#[test]
fn an_answer_after_the_team_message_ended_still_reaches_the_asker_once() {
    let mut engine = Engine::new();
    engine.register();
    let asker = engine.agent("working");
    let asked = engine.ask(asker, Severity::Normal, "Rename the repo?");
    engine.tick(NOW);
    let linked = engine.linked();
    assert_eq!(linked.len(), 1);
    assert!(crate::coord::withdraw(&engine.connection, asked, NOW + 1).unwrap());
    assert_eq!(
        crate::questions::dismiss(&engine.connection, linked[0].id, 0).unwrap(),
        crate::questions::Settlement::Settled
    );
    assert!(engine.replies(asked).is_empty());
    let notes = crate::questions::agent_notes(&engine.connection, &engine.project, asker).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].text.starts_with("The owner dismissed"),
        "{}",
        notes[0].text
    );
}
