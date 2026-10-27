use super::*;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

const PROJECT: &str = "alpha";
const MASTER: Asker = Asker::Master(41);

/// One engine root with a set-up project whose folder holds `.crazyagent/`, as onboarding
/// leaves it, and the engine database with the team and setup tables.
struct Case {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    folder: PathBuf,
    db: Connection,
    project: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-questions-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join("project");
        for path in [&root, &folder] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::create_dir(folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
        let db = open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        Self {
            _tmp: tmp,
            base,
            root,
            folder,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn page_path(&self) -> PathBuf {
        self.folder.join(CRAZYAGENT_DIRECTORY).join(page::FILE)
    }

    fn page(&self) -> String {
        std::fs::read_to_string(self.page_path()).unwrap()
    }

    /// The project's team messages oldest first: author, text and state.
    fn messages(&self) -> Vec<(String, String, String)> {
        let mut statement = self
            .db
            .prepare("SELECT author,text,state FROM team_messages WHERE project=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn ask(&self, asker: &Asker, question: &str) -> u64 {
        ask(&self.db, &self.project, asker, &friday(question)).unwrap()
    }

    fn get(&self, id: u64) -> Question {
        let tx = self.db.unchecked_transaction().unwrap();
        found(&tx, id).unwrap()
    }

    /// Appends one finished chat message of the project, as the runtime keeps the chat.
    fn chat(&self, project: &str, author: MessageAuthor, text: &str) -> i64 {
        let tx = self.db.unchecked_transaction().unwrap();
        let id = append_message(
            &tx,
            &ProjectId::new(project).unwrap(),
            author,
            text,
            MessageState::Answered,
            now_ms(),
        )
        .unwrap()
        .unwrap();
        tx.commit().unwrap();
        id
    }

    /// Fills the master's conversation with waiting engine messages until it refuses more.
    fn fill(&self) {
        let tx = self.db.unchecked_transaction().unwrap();
        let mut full = false;
        for n in 0..256 {
            let text = format!("waiting {n}");
            let appended = append_message(
                &tx,
                &self.project,
                MessageAuthor::Engine,
                &text,
                MessageState::Pending,
                now_ms(),
            )
            .unwrap();
            if appended.is_err() {
                full = true;
                break;
            }
        }
        assert!(full);
        tx.commit().unwrap();
    }
}

fn friday(question: &str) -> Ask {
    Ask {
        question: question.into(),
        options: vec!["Yes".into(), "No".into()],
        recommended: Some("Yes".into()),
        context: Some("CI is green.".into()),
    }
}

fn bare(question: &str) -> Ask {
    Ask {
        question: question.into(),
        options: vec![],
        recommended: None,
        context: None,
    }
}

fn sample(id: u64, status: QuestionStatus, question: &str) -> Question {
    Question {
        id,
        project: ProjectId::new(PROJECT).unwrap(),
        asker: MASTER.name(),
        question: question.into(),
        options: vec![],
        recommended: None,
        context: None,
        status,
        version: u64::from(status != QuestionStatus::Pending),
        answer: None,
        answered_by: None,
        created_ms: 1,
        settled_ms: None,
    }
}

fn code(result: Result<impl std::fmt::Debug, EngineError>) -> String {
    result.unwrap_err().0
}

/// The page's headings, skipping every line inside a fence, so a heading an asker wrote
/// inside its text is never counted.
fn headings(page: &str) -> Vec<&str> {
    let mut fence: Option<&str> = None;
    let mut found = Vec::new();
    for line in page.lines() {
        match fence {
            Some(open) if line == open => fence = None,
            Some(_) => {}
            None if line.starts_with("~~~") => {
                fence = Some(&line[..line.len() - line.trim_start_matches('~').len()]);
            }
            None if line.starts_with('#') => found.push(line),
            None => {}
        }
    }
    assert!(fence.is_none(), "an unclosed fence:\n{page}");
    found
}

#[test]
fn an_agent_ask_is_pending_rendered_and_notified() {
    let case = Case::new();
    let id = ask(
        &case.db,
        &case.project,
        &Asker::Agent(7),
        &friday("  Ship on Friday?\nThe release notes are ready.  "),
    )
    .unwrap();
    assert_eq!(id, 1);

    let waiting = pending(&case.db, &case.project).unwrap();
    assert_eq!(waiting.len(), 1);
    let question = &waiting[0];
    assert_eq!(
        (
            question.id,
            question.asker.as_str(),
            question.question.as_str(),
            question.status,
            question.version
        ),
        (
            1,
            "agent:7",
            "Ship on Friday?\nThe release notes are ready.",
            QuestionStatus::Pending,
            0
        )
    );
    assert_eq!(question.options, ["Yes", "No"]);
    assert_eq!(
        (
            question.recommended.as_deref(),
            question.context.as_deref(),
            question.answer.as_deref(),
            question.answered_by,
            question.settled_ms
        ),
        (Some("Yes"), Some("CI is green."), None, None, None)
    );
    assert!(question.created_ms > 0);

    let notes = events::notifications(&case.db, Some(&case.project), 10).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(
        (notes[0].kind, notes[0].text.as_str()),
        (NotificationKind::Question, "Agent 7: Ship on Friday?")
    );
    assert_eq!(
        case.messages(),
        [(
            "engine".to_owned(),
            "Agent 7 asked the owner question 1 (\"Ship on Friday? The release notes are ready.\"). The owner's answer comes to you; pass it on to agent 7.".to_owned(),
            "pending".to_owned()
        )]
    );
    assert_eq!(
        count_pending(&case.db).unwrap(),
        [(case.project.clone(), 1)]
    );

    let page = case.page();
    assert_eq!(
        headings(&page),
        [
            "# Questions for the owner",
            "## Waiting for the owner (1)",
            "### Question 1 from agent 7",
            "## Answered and dismissed, newest first"
        ]
    );
    assert!(page.contains("1. Yes (recommended)\n2. No\n"), "{page}");
    assert!(page.contains("Waiting for your answer.\n"), "{page}");
    let mode = std::fs::metadata(case.page_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o444);
}

#[test]
fn an_answer_reaches_the_asker_once_and_a_restart_never_repeats_it() {
    let case = Case::new();
    let id = case.ask(&MASTER, "Ship on Friday?");
    assert!(case.messages().is_empty());
    assert_eq!(
        answer(&case.db, id, 0, &Answer::Option(0)).unwrap(),
        Settlement::Settled
    );
    let delivered = [(
        "owner".to_owned(),
        "The owner answered your question 1 (\"Ship on Friday?\"):\nYes".to_owned(),
        "pending".to_owned(),
    )];
    assert_eq!(case.messages(), delivered);

    let restarted = open(&case.root).unwrap();
    for settled in [
        answer(&restarted, id, 0, &Answer::Text("No".into())),
        answer(&restarted, id, 1, &Answer::Text("No".into())),
        dismiss(&restarted, id, 1),
    ] {
        assert_eq!(settled.unwrap(), Settlement::Stale);
    }
    assert_eq!(case.messages(), delivered);
    let question = case.get(id);
    assert_eq!(
        (
            question.status,
            question.answer.as_deref(),
            question.answered_by,
            question.version
        ),
        (
            QuestionStatus::Answered,
            Some("Yes"),
            Some(AnsweredBy::Owner),
            1
        )
    );
    assert!(question.settled_ms.is_some());
    assert!(count_pending(&case.db).unwrap().is_empty());
    let page = case.page();
    assert!(page.contains("## Waiting for the owner (0)"), "{page}");
    assert!(
        page.contains("Answered by the owner:\n\n~~~text\nYes\n~~~\n"),
        "{page}"
    );
}

#[test]
fn a_second_answer_to_the_same_version_is_stale() {
    let case = Case::new();
    let id = case.ask(&MASTER, "Which database?");
    let other = case.ask(&MASTER, "Which region?");
    let second = open(&case.root).unwrap();
    let read = |db: &Connection| {
        let tx = db.unchecked_transaction().unwrap();
        found(&tx, id).unwrap().version
    };
    assert_eq!((read(&case.db), read(&second)), (0, 0));
    assert_eq!(
        answer(&case.db, id, 0, &Answer::Text("Postgres".into())).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        answer(&second, id, 0, &Answer::Option(1)).unwrap(),
        Settlement::Stale
    );
    assert_eq!(case.get(id).answer.as_deref(), Some("Postgres"));

    assert_eq!(
        answer(&case.db, other, 3, &Answer::Option(0)).unwrap(),
        Settlement::Stale
    );
    let untouched = case.get(other);
    assert_eq!(
        (untouched.status, untouched.version),
        (QuestionStatus::Pending, 0)
    );
    assert_eq!(case.messages().len(), 1);
}

#[test]
fn dismiss_tells_the_asker_once() {
    let case = Case::new();
    let id = case.ask(&MASTER, "Rename the app?");
    assert_eq!(dismiss(&case.db, id, 0).unwrap(), Settlement::Settled);
    assert_eq!(dismiss(&case.db, id, 0).unwrap(), Settlement::Stale);
    assert_eq!(
        answer(&case.db, id, 1, &Answer::Option(0)).unwrap(),
        Settlement::Stale
    );
    assert_eq!(
        case.messages(),
        [(
            "owner".to_owned(),
            "The owner dismissed your question 1 (\"Rename the app?\"). No answer comes."
                .to_owned(),
            "pending".to_owned()
        )]
    );
    let question = case.get(id);
    assert_eq!(
        (question.status, question.answer, question.version),
        (QuestionStatus::Dismissed, None, 1)
    );
    assert!(case.page().contains("Dismissed by the owner.\n"));
}

#[test]
fn a_sub_agent_question_reaches_it_through_its_master() {
    let case = Case::new();
    let id = case.ask(&Asker::Agent(7), "Which database?");
    let dismissed = case.ask(&Asker::Agent(7), "Which region?");
    assert_eq!(
        answer(
            &case.db,
            id,
            0,
            &Answer::Text("  Use Postgres 16.  ".into())
        )
        .unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        dismiss(&case.db, dismissed, 0).unwrap(),
        Settlement::Settled
    );
    let messages = case.messages();
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[2..],
        [
            (
                "owner".to_owned(),
                "The owner answered agent 7's question 1 (\"Which database?\"):\nUse Postgres 16.\nPass this on to agent 7."
                    .to_owned(),
                "pending".to_owned()
            ),
            (
                "owner".to_owned(),
                "The owner dismissed agent 7's question 2 (\"Which region?\"). No answer comes.\nPass this on to agent 7."
                    .to_owned(),
                "pending".to_owned()
            ),
        ]
    );
}

#[test]
fn asks_past_the_per_asker_and_project_bounds_are_refused_plainly() {
    let case = Case::new();
    for n in 0..PENDING_PER_ASKER_MAX {
        case.ask(&MASTER, &format!("Master question {n}?"));
    }
    assert_eq!(
        code(ask(&case.db, &case.project, &MASTER, &friday("One more?"))),
        "questions_asker_full"
    );
    for agent in 1..=(PENDING_PER_PROJECT_MAX / PENDING_PER_ASKER_MAX - 1) {
        for n in 0..PENDING_PER_ASKER_MAX {
            case.ask(
                &Asker::Agent(agent),
                &format!("Agent {agent} question {n}?"),
            );
        }
    }
    let all = "SELECT COUNT(*) FROM owner_questions";
    assert_eq!(case.count(all), PENDING_PER_PROJECT_MAX);
    assert_eq!(
        code(ask(
            &case.db,
            &case.project,
            &Asker::Agent(99),
            &bare("Room?")
        )),
        "questions_full"
    );
    assert_eq!(
        code(raise(
            &case.db,
            &case.project,
            &EngineSource::Mapping,
            &bare("Room?")
        )),
        "questions_full"
    );
    let first = pending(&case.db, &case.project).unwrap()[0].id;
    assert_eq!(dismiss(&case.db, first, 0).unwrap(), Settlement::Settled);
    case.ask(&Asker::Agent(99), "Room now?");
    assert_eq!(case.count(all), PENDING_PER_PROJECT_MAX + 1);
}

#[test]
fn asks_outside_their_bounds_are_refused_before_anything_is_written() {
    let case = Case::new();
    let mut invalid = vec![bare("   "), bare(&"q".repeat(QUESTION_BYTES_MAX + 1))];
    let mut options = friday("Pick one?");
    options.options = (0..=OPTIONS_MAX).map(|n| format!("option {n}")).collect();
    invalid.push(options);
    for (options, recommended) in [
        (vec!["Yes", "Yes"], None),
        (vec!["two\nlines"], None),
        (vec![""], None),
        (vec!["Yes", "No"], Some("Maybe")),
    ] {
        let mut ask = friday("Pick one?");
        ask.options = options.into_iter().map(str::to_owned).collect();
        ask.recommended = recommended.map(str::to_owned);
        invalid.push(ask);
    }
    let mut ask = bare("Pick one?");
    ask.options = vec!["o".repeat(OPTION_BYTES_MAX + 1)];
    invalid.push(ask);
    let mut ask = bare("Pick one?");
    ask.recommended = Some("r".repeat(RECOMMENDED_BYTES_MAX + 1));
    invalid.push(ask);
    let mut ask = bare("Pick one?");
    ask.context = Some("c".repeat(CONTEXT_BYTES_MAX + 1));
    invalid.push(ask);
    for ask in &invalid {
        assert_eq!(
            code(super::ask(&case.db, &case.project, &MASTER, ask)),
            "questions_ask_invalid",
            "{ask:?}"
        );
    }
    let mut hidden = Vec::new();
    for text in [
        "Pick \u{202e}one?",
        "zero\u{200b}width",
        "line\u{2028}break",
        "bell \u{7} ring",
    ] {
        hidden.push(bare(text));
        let mut ask = friday("Pick one?");
        ask.options[1] = text.into();
        hidden.push(ask);
        let mut ask = bare("Pick one?");
        ask.context = Some(text.into());
        hidden.push(ask);
    }
    for ask in &hidden {
        assert_eq!(
            code(super::ask(&case.db, &case.project, &MASTER, ask)),
            "questions_text_invalid",
            "{ask:?}"
        );
    }
    for asker in [Asker::Master(0), Asker::Agent(-1), Asker::Terminal(0)] {
        assert_eq!(
            code(super::ask(&case.db, &case.project, &asker, &bare("Pick?"))),
            "questions_ask_invalid"
        );
    }
    for source in [
        EngineSource::Research("two words".into()),
        EngineSource::Landing(String::new()),
        EngineSource::Research("k".repeat(NAME_BYTES_MAX + 1)),
    ] {
        assert_eq!(
            code(raise(&case.db, &case.project, &source, &bare("Pick?"))),
            "questions_ask_invalid"
        );
    }
    let ghost = ProjectId::new("ghost").unwrap();
    assert_eq!(
        code(super::ask(&case.db, &ghost, &MASTER, &bare("Pick?"))),
        "questions_project_missing"
    );
    assert!(
        page(&case.db, &case.project, None)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM team_notifications"), 0);
    assert!(!case.page_path().exists());

    let mut widest = bare(&"q".repeat(QUESTION_BYTES_MAX));
    widest.options = (0..OPTIONS_MAX)
        .map(|n| format!("{n}{}", "o".repeat(OPTION_BYTES_MAX - 1)))
        .collect();
    widest.recommended = Some(widest.options[5].clone());
    widest.context = Some("c".repeat(CONTEXT_BYTES_MAX));
    let id = super::ask(&case.db, &case.project, &MASTER, &widest).unwrap();
    assert_eq!(
        answer(
            &case.db,
            id,
            0,
            &Answer::Text("a".repeat(ANSWER_BYTES_MAX + 1))
        )
        .unwrap_err()
        .0,
        "questions_answer_invalid"
    );
    assert_eq!(
        answer(&case.db, id, 0, &Answer::Option(6)).unwrap_err().0,
        "questions_option_invalid"
    );
    assert_eq!(
        answer(&case.db, id, 0, &Answer::Text("a".repeat(ANSWER_BYTES_MAX))).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        answer(&case.db, 999, 0, &Answer::Option(0)).unwrap_err().0,
        "questions_not_found"
    );
}

#[test]
fn record_from_chat_takes_only_a_newer_owner_message_for_the_masters_own_question() {
    let case = Case::new();
    let beta = case.base.join("beta");
    std::fs::create_dir_all(beta.join(CRAZYAGENT_DIRECTORY)).unwrap();
    crate::ide::workspace::tests::add_project(&case.db, "beta", &beta);

    let early = case.chat(PROJECT, MessageAuthor::Owner, "Yes, ship it.");
    let id = case.ask(&MASTER, "Ship on Friday?");
    let other = case.ask(&Asker::Master(42), "Rename the app?");
    let agents = case.ask(&Asker::Agent(7), "Which database?");
    let own = case.chat(PROJECT, MessageAuthor::Master, "The owner said yes.");
    let elsewhere = case.chat("beta", MessageAuthor::Owner, "Yes, Friday.");
    let owner = case.chat(PROJECT, MessageAuthor::Owner, "  Yes, Friday.  ");
    let before = case.messages().len();

    for turn in [early, own, elsewhere, owner + 100] {
        assert_eq!(
            code(record_from_chat(&case.db, &case.project, 41, id, 0, turn)),
            "questions_owner_turn_invalid"
        );
    }
    let beta_project = ProjectId::new("beta").unwrap();
    for (project, master, question) in [
        (&case.project, 41, other),
        (&case.project, 41, agents),
        (&case.project, 0, id),
        (&case.project, 41, 999),
        (&beta_project, 41, id),
    ] {
        assert_eq!(
            code(record_from_chat(
                &case.db, project, master, question, 0, owner
            )),
            "questions_not_found"
        );
    }
    assert_eq!(case.get(id).status, QuestionStatus::Pending);

    assert_eq!(
        record_from_chat(&case.db, &case.project, 41, id, 0, owner).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        record_from_chat(&case.db, &case.project, 41, id, 0, owner).unwrap(),
        Settlement::Stale
    );
    let question = case.get(id);
    assert_eq!(
        (
            question.status,
            question.answer.as_deref(),
            question.answered_by
        ),
        (
            QuestionStatus::Answered,
            Some("Yes, Friday."),
            Some(AnsweredBy::Chat)
        )
    );
    assert_eq!(case.messages().len(), before);
    assert!(
        case.page()
            .contains("Answered by the owner in the master chat:")
    );
}

#[test]
fn an_engine_question_is_raised_once_and_its_answer_is_noted_without_blocking() {
    let case = Case::new();
    let mapping = EngineSource::Mapping;
    let id = raise(&case.db, &case.project, &mapping, &friday("Which auth?")).unwrap();
    assert_eq!(
        raise(&case.db, &case.project, &mapping, &friday("Which auth?")).unwrap(),
        id
    );
    let landing = EngineSource::Landing("cs-12".into());
    let set = raise(&case.db, &case.project, &landing, &bare("Squash cs-12?")).unwrap();
    assert_ne!(set, id);
    assert_eq!(case.get(set).asker, "engine:landing:cs-12");
    assert_eq!(case.count("SELECT COUNT(*) FROM owner_questions"), 2);
    assert!(case.messages().is_empty());

    assert_eq!(
        answer(&case.db, id, 0, &Answer::Option(1)).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        case.messages(),
        [(
            "engine".to_owned(),
            "The owner answered the project mapping question 1 (\"Which auth?\"):\nNo".to_owned(),
            "pending".to_owned()
        )]
    );
    let again = raise(&case.db, &case.project, &mapping, &friday("Which auth?")).unwrap();
    assert_ne!(again, id);

    let master = case.ask(&MASTER, "Ship on Friday?");
    case.fill();
    let waiting = case.messages().len();
    assert_eq!(
        code(answer(&case.db, master, 0, &Answer::Option(0))),
        "questions_delivery_full"
    );
    assert_eq!(
        code(dismiss(&case.db, master, 0)),
        "questions_delivery_full"
    );
    let kept = case.get(master);
    assert_eq!((kept.status, kept.version), (QuestionStatus::Pending, 0));
    assert_eq!(dismiss(&case.db, set, 0).unwrap(), Settlement::Settled);
    assert_eq!(case.messages().len(), waiting);

    case.db
        .execute(
            "UPDATE team_messages SET state='answered' WHERE id=(SELECT MIN(id) FROM team_messages WHERE state='pending')",
            [],
        )
        .unwrap();
    assert_eq!(
        answer(&case.db, master, 0, &Answer::Option(0)).unwrap(),
        Settlement::Settled
    );
    assert_eq!(case.messages().len(), waiting + 1);
}

#[test]
fn a_conversation_master_gets_each_note_once() {
    let case = Case::new();
    let session = SessionId::new("session-1").unwrap();
    let asker = Asker::Session(session.clone());
    let answered = case.ask(&asker, "Ship on Friday?");
    let dismissed = case.ask(&asker, "Rename the app?");
    assert_eq!(case.get(answered).asker, "master:session:session-1");
    assert!(
        session_notes(&case.db, &case.project, &session)
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        answer(&case.db, answered, 0, &Answer::Option(0)).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        dismiss(&case.db, dismissed, 0).unwrap(),
        Settlement::Settled
    );
    assert!(case.messages().is_empty());
    let notes = session_notes(&case.db, &case.project, &session).unwrap();
    assert_eq!(
        notes,
        [
            Note {
                question: answered,
                text: "The owner answered your question 1 (\"Ship on Friday?\"):\nYes".into(),
            },
            Note {
                question: dismissed,
                text: "The owner dismissed your question 2 (\"Rename the app?\"). No answer comes."
                    .into(),
            },
        ]
    );
    let stranger = SessionId::new("session-2").unwrap();
    assert!(
        session_notes(&case.db, &case.project, &stranger)
            .unwrap()
            .is_empty()
    );
    notes_delivered(&case.db, &case.project, &stranger, &[answered, dismissed]).unwrap();
    assert_eq!(
        session_notes(&case.db, &case.project, &session)
            .unwrap()
            .len(),
        2
    );

    notes_delivered(&case.db, &case.project, &session, &[answered]).unwrap();
    let left = session_notes(&case.db, &case.project, &session).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].question, dismissed);
    notes_delivered(&case.db, &case.project, &session, &[answered, dismissed]).unwrap();
    assert!(
        session_notes(&case.db, &case.project, &session)
            .unwrap()
            .is_empty()
    );
    let many = vec![1; NOTES_MAX + 1];
    assert_eq!(
        code(notes_delivered(&case.db, &case.project, &session, &many)),
        "questions_notes_invalid"
    );
}

#[test]
fn terminal_questions_are_answered_on_screen_and_never_delivered() {
    let case = Case::new();
    let id = case.ask(&Asker::Terminal(7), "Allow this command?");
    assert!(
        case.page()
            .contains("Waiting for your answer in that agent's terminal.\n")
    );
    assert_eq!(
        code(answer(&case.db, id, 0, &Answer::Option(0))),
        "questions_in_terminal"
    );
    assert_eq!(terminal_settled(&case.db, id).unwrap(), Settlement::Settled);
    assert_eq!(terminal_settled(&case.db, id).unwrap(), Settlement::Stale);
    let question = case.get(id);
    assert_eq!(
        (question.status, question.version, question.answer),
        (QuestionStatus::AnsweredInTerminal, 1, None)
    );
    assert!(
        case.page()
            .contains("Answered by the owner in that agent's terminal.\n")
    );

    let dismissed = case.ask(&Asker::Terminal(7), "Allow this other command?");
    assert_eq!(
        dismiss(&case.db, dismissed, 0).unwrap(),
        Settlement::Settled
    );
    let master = case.ask(&MASTER, "Ship on Friday?");
    assert_eq!(
        code(terminal_settled(&case.db, master)),
        "questions_not_terminal"
    );
    assert_eq!(code(terminal_settled(&case.db, 999)), "questions_not_found");
    assert!(case.messages().is_empty());
}

#[test]
fn pages_list_pending_first_then_settled_newest_first() {
    let case = Case::new();
    let mut ids = Vec::new();
    for session in 1..=7 {
        let asker = Asker::Session(SessionId::new(format!("s{session}")).unwrap());
        for n in 0..PENDING_PER_ASKER_MAX {
            ids.push(case.ask(&asker, &format!("Session {session} question {n}?")));
        }
    }
    let first = page(&case.db, &case.project, None).unwrap();
    assert_eq!(first.items.len(), LISTED_MAX);
    assert_eq!(
        first.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        ids[..LISTED_MAX]
    );
    assert_eq!(first.next, Some(ids[LISTED_MAX - 1] << 1));
    let rest = page(&case.db, &case.project, first.next).unwrap();
    assert_eq!(
        rest.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        ids[LISTED_MAX..]
    );
    assert_eq!(rest.next, None);

    for id in ids.iter().filter(|id| *id % 2 == 0) {
        assert_eq!(dismiss(&case.db, *id, 0).unwrap(), Settlement::Settled);
    }
    let odd: Vec<u64> = ids.iter().copied().filter(|id| id % 2 == 1).collect();
    let even: Vec<u64> = ids.iter().rev().copied().filter(|id| id % 2 == 0).collect();
    let first = page(&case.db, &case.project, None).unwrap();
    let shown: Vec<u64> = first.items.iter().map(|q| q.id).collect();
    assert_eq!(shown[..odd.len()], odd);
    assert_eq!(shown[odd.len()..], even[..LISTED_MAX - odd.len()]);
    let last = *shown.last().unwrap();
    assert_eq!(first.next, Some((last << 1) | 1));

    assert_eq!(dismiss(&case.db, odd[0], 0).unwrap(), Settlement::Settled);
    let rest = page(&case.db, &case.project, first.next).unwrap();
    let settled: Vec<u64> = rest.items.iter().map(|q| q.id).collect();
    let mut expected = even[LISTED_MAX - odd.len()..].to_vec();
    expected.push(odd[0]);
    expected.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(settled, expected);
    assert!(
        rest.items
            .iter()
            .all(|q| q.status == QuestionStatus::Dismissed)
    );
    assert_eq!(rest.next, None);
    assert_eq!(
        count_pending(&case.db).unwrap(),
        [(case.project.clone(), (odd.len() - 1) as u32)]
    );
}

#[test]
fn questions_md_is_rewritten_and_never_follows_a_link() {
    let case = Case::new();
    case.ask(&MASTER, "Ship on Friday?");
    let state = case.folder.join(CRAZYAGENT_DIRECTORY);
    let outside = case.base.join("outside.md");
    std::fs::write(&outside, "keep").unwrap();
    std::fs::remove_file(case.page_path()).unwrap();
    symlink(&outside, case.page_path()).unwrap();
    let partial_target = case.base.join("partial-target.md");
    std::fs::write(&partial_target, "keep").unwrap();
    symlink(&partial_target, state.join("questions.md.partial")).unwrap();

    case.ask(&MASTER, "Rename the app?");
    let metadata = std::fs::symlink_metadata(case.page_path()).unwrap();
    assert!(metadata.is_file() && !metadata.file_type().is_symlink());
    assert!(case.page().contains("## Waiting for the owner (2)"));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert_eq!(std::fs::read_to_string(&partial_target).unwrap(), "keep");
    assert!(!state.join("questions.md.partial").exists());

    let elsewhere = case.base.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::rename(&state, case.base.join("moved-state")).unwrap();
    symlink(&elsewhere, &state).unwrap();
    let id = case.ask(&MASTER, "Use Postgres?");
    assert_eq!(dismiss(&case.db, id, 0).unwrap(), Settlement::Settled);
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn a_project_without_its_state_folder_still_asks_answers_and_dismisses() {
    let case = Case::new();
    std::fs::remove_dir(case.folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
    let id = case.ask(&MASTER, "Ship on Friday?");
    let other = case.ask(&MASTER, "Rename the app?");
    assert_eq!(
        answer(&case.db, id, 0, &Answer::Option(0)).unwrap(),
        Settlement::Settled
    );
    std::fs::remove_dir(&case.folder).unwrap();
    assert_eq!(dismiss(&case.db, other, 0).unwrap(), Settlement::Settled);
    assert_eq!(case.messages().len(), 2);
    assert!(!case.page_path().exists());
    assert!(!case.folder.exists());
}

#[test]
fn hidden_characters_never_reach_the_page_unescaped() {
    let mut question = sample(3, QuestionStatus::Answered, "Ship \u{202e}yadirf no?");
    question.options = vec!["Yes".into(), "No\u{200b}".into()];
    question.context = Some("~~~\n# Planted heading\n~~~~".into());
    question.answer = Some("Yes\u{2066}".into());
    question.answered_by = Some(AnsweredBy::Owner);
    let rendered = page::render(&[], &[question]);
    assert!(!hidden_text(&rendered), "{rendered}");
    for escaped in ["\\u202e", "\\u200b", "\\u2066"] {
        assert!(rendered.contains(escaped), "{escaped} in {rendered}");
    }
    assert_eq!(
        headings(&rendered),
        [
            "# Questions for the owner",
            "## Waiting for the owner (0)",
            "## Answered and dismissed, newest first",
            "### Question 3 from the master"
        ]
    );
    assert!(rendered.contains("~~~~~text\n~~~\n# Planted heading\n~~~~\n~~~~~\n"));
}

#[test]
fn the_page_keeps_every_pending_question_and_cuts_settled_ones_at_its_bound() {
    let large = "x".repeat(QUESTION_BYTES_MAX);
    let pending: Vec<Question> = (1..=PENDING_PER_PROJECT_MAX as u64)
        .map(|id| {
            let mut question = sample(id, QuestionStatus::Pending, &large);
            question.context = Some("c".repeat(CONTEXT_BYTES_MAX));
            question
        })
        .collect();
    let settled: Vec<Question> = (100..100 + page::SETTLED_SHOWN_MAX as u64)
        .rev()
        .map(|id| {
            let mut question = sample(id, QuestionStatus::Answered, &large);
            question.answer = Some("a".repeat(ANSWER_BYTES_MAX));
            question.context = Some("c".repeat(CONTEXT_BYTES_MAX));
            question
        })
        .collect();
    let rendered = page::render(&pending, &settled);
    assert!(rendered.len() <= page::PAGE_BYTES_MAX);
    let shown = headings(&rendered)
        .iter()
        .filter(|line| line.starts_with("### "))
        .count();
    assert!(shown > pending.len() && shown < pending.len() + settled.len());
    let left = pending.len() + settled.len() - shown;
    assert!(rendered.ends_with(&format!(
        "\n{left} older questions are only in the Questions view.\n"
    )));
    assert!(rendered.contains(&format!("### Question {} from the master", pending.len())));
    assert!(!rendered.contains("more waiting questions"));
}

#[test]
fn settled_history_keeps_the_newest_and_never_drops_a_pending_question() {
    let case = Case::new();
    std::fs::remove_dir(case.folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
    let kept = case.ask(&MASTER, "Still waiting?");
    let total = KEPT_PER_PROJECT_MAX + 10;
    let mut last = 0;
    for n in 0..total {
        let source = EngineSource::Research(format!("k{n}"));
        last = raise(&case.db, &case.project, &source, &bare("Which one?")).unwrap();
        assert_eq!(dismiss(&case.db, last, 0).unwrap(), Settlement::Settled);
    }
    assert_eq!(
        case.count("SELECT COUNT(*) FROM owner_questions"),
        KEPT_PER_PROJECT_MAX
    );
    assert_eq!(case.get(kept).status, QuestionStatus::Pending);
    assert_eq!(case.get(last).status, QuestionStatus::Dismissed);
    let oldest: i64 = case.count("SELECT MIN(id) FROM owner_questions WHERE status!='pending'");
    assert_eq!(
        oldest,
        i64::try_from(last).unwrap() - (KEPT_PER_PROJECT_MAX - 2)
    );
    assert_eq!(
        case.count("SELECT COUNT(*) FROM versioned_items WHERE item LIKE 'question:alpha:%'"),
        KEPT_PER_PROJECT_MAX - 1
    );
}

#[test]
#[should_panic(expected = "a pending question always holds version 0")]
fn a_pending_question_whose_item_was_written_trips_the_invariant() {
    let case = Case::new();
    let id = case.ask(&MASTER, "Ship on Friday?");
    let tx = case.db.unchecked_transaction().unwrap();
    versioned::write_if(&tx, &item(&case.project, id).unwrap(), 0, &owner()).unwrap();
    tx.commit().unwrap();
    let _ = pending(&case.db, &case.project);
}

#[test]
fn askers_are_named_and_labelled_for_the_owner() {
    let session = SessionId::new("s-1").unwrap();
    assert_eq!(
        [
            MASTER.name(),
            Asker::Session(session).name(),
            Asker::Agent(7).name(),
            Asker::Terminal(7).name()
        ],
        ["master:41", "master:session:s-1", "agent:7", "terminal:7"]
    );
    assert_eq!(
        [
            EngineSource::Mapping.name(),
            EngineSource::Planning.name(),
            EngineSource::Research("security_review".into()).name(),
            EngineSource::Landing("cs-12".into()).name(),
            EngineSource::Research("a/b".into()).name()
        ],
        [
            Some("engine:mapping".to_owned()),
            Some("engine:planning".to_owned()),
            Some("engine:research:security_review".to_owned()),
            Some("engine:landing:cs-12".to_owned()),
            None
        ]
    );
    for (asker, label) in [
        ("master:41", "the master"),
        ("master:session:s-1", "the master"),
        ("agent:7", "agent 7"),
        ("terminal:7", "the terminal of agent 7"),
        ("engine:mapping", "project mapping"),
        ("engine:planning", "roadmap planning"),
        (
            "engine:research:security_review",
            "research (security review)",
        ),
        ("engine:landing:cs-12", "landing change set cs-12"),
    ] {
        assert_eq!(asker_label(asker), label);
    }
}
