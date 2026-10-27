use super::*;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

const PROJECT: &str = "alpha";
const MASTER: i64 = 41;
const NOW: i64 = 1_800_000_000_000;

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

    fn ask(&self, question: &str) -> i64 {
        ask(&self.db, &self.project, MASTER, &plain_ask(question), NOW)
            .unwrap()
            .unwrap()
    }

    fn version(&self, id: i64) -> u64 {
        let tx = self.db.unchecked_transaction().unwrap();
        versioned::current(&tx, &item(&self.project, id).unwrap())
            .unwrap()
            .version
    }
}

fn plain_ask(question: &str) -> Ask {
    Ask {
        question: question.into(),
        options: vec![],
        recommended: None,
        context: None,
    }
}

fn friday() -> Ask {
    Ask {
        question: "Ship V2 on Friday?".into(),
        options: vec!["Yes".into(), "No, wait a week".into()],
        recommended: Some("Yes".into()),
        context: Some("All slices merged.\nOne flaky test left.".into()),
    }
}

fn stale(outcome: Result<(), Refusal>) -> Question {
    match outcome {
        Err(Refusal::Stale(current)) => *current,
        other => panic!("expected a stale refusal, got {other:?}"),
    }
}

/// The heading lines of a page outside its fences, as a Markdown reader sees them.
fn headings(page: &str) -> Vec<&str> {
    let mut fence: Option<&str> = None;
    let mut found = Vec::new();
    for line in page.lines() {
        match fence {
            Some(open) if line == open => fence = None,
            Some(_) => {}
            None if line.starts_with("~~~") => {
                fence = Some(line.trim_end_matches("text"));
            }
            None if line.starts_with('#') => found.push(line),
            None => {}
        }
    }
    assert!(fence.is_none(), "every fence closes");
    found
}

#[test]
fn a_master_question_is_logged_pending_notified_and_written_to_its_read_only_page() {
    let case = Case::new();
    let id = ask(&case.db, &case.project, MASTER, &friday(), NOW)
        .unwrap()
        .unwrap();

    let listed = list(&case.db, &case.project, true, None).unwrap();
    assert_eq!(listed.len(), 1);
    let question = &listed[0];
    assert_eq!(
        (
            question.id,
            question.source,
            question.agent,
            question.state,
            question.version
        ),
        (id, Source::Master, Some(MASTER), State::Pending, 0)
    );
    assert_eq!(question.options, ["Yes", "No, wait a week"]);
    assert_eq!(question.recommended.as_deref(), Some("Yes"));
    assert_eq!(question.asked_ms, NOW);

    let notices: Vec<(String, String)> = case
        .db
        .prepare("SELECT kind,text FROM team_notifications WHERE project=?1")
        .unwrap()
        .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        notices,
        [(
            "escalation".to_owned(),
            format!("Question {id} from the master: Ship V2 on Friday?")
        )]
    );

    let page = case.page();
    assert!(page.contains("## Waiting for the owner (1)"), "{page}");
    assert!(
        page.contains(&format!(
            "### Question {id} from the master\n\n~~~text\nShip V2 on Friday?\n~~~\n"
        )),
        "{page}"
    );
    assert!(
        page.contains("~~~text\n1. Yes (recommended)\n2. No, wait a week\n~~~\n"),
        "{page}"
    );
    assert!(
        page.contains("Context:\n\n~~~text\nAll slices merged.\nOne flaky test left.\n~~~\n"),
        "{page}"
    );
    assert!(!page.contains("Recommended:"), "{page}");
    let metadata = std::fs::symlink_metadata(case.page_path()).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o444);
    assert!(!case.page_path().with_extension("md.partial").exists());
    assert!(case.messages().is_empty());
}

#[test]
fn an_owner_answer_reaches_the_master_once_and_a_second_answer_is_stale() {
    let case = Case::new();
    let id = ask(&case.db, &case.project, MASTER, &friday(), NOW)
        .unwrap()
        .unwrap();

    answer(&case.db, &case.project, id, 0, "  Yes, ship it.  ", NOW + 1)
        .unwrap()
        .unwrap();

    let delivered = [(
        "owner".to_owned(),
        format!("The owner answered your question {id} (\"Ship V2 on Friday?\"):\nYes, ship it."),
        "pending".to_owned(),
    )];
    assert_eq!(case.messages(), delivered);
    let current = stale(answer(&case.db, &case.project, id, 0, "No.", NOW + 2).unwrap());
    assert_eq!(
        (
            current.state,
            current.answer.as_deref(),
            current.answered_by,
            current.version,
            current.resolved_ms
        ),
        (
            State::Answered,
            Some("Yes, ship it."),
            Some(AnsweredBy::Owner),
            1,
            Some(NOW + 1)
        )
    );
    stale(answer(&case.db, &case.project, id, 1, "No.", NOW + 2).unwrap());
    stale(dismiss(&case.db, &case.project, id, 1, NOW + 2).unwrap());
    stale(record(&case.db, &case.project, MASTER, id, "No.", NOW + 2).unwrap());

    let reopened = open(&case.root).unwrap();
    assert!(
        list(&reopened, &case.project, true, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        list(&reopened, &case.project, false, None).unwrap().len(),
        1
    );
    assert_eq!(case.messages(), delivered);
    let linked: Option<i64> = case
        .db
        .query_row(
            "SELECT message FROM owner_questions WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let message: i64 = case
        .db
        .query_row(
            "SELECT id FROM team_messages WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, Some(message));
    let page = case.page();
    assert!(page.contains("## Waiting for the owner (0)"), "{page}");
    assert!(
        page.contains("Answered by the owner:\n\n~~~text\nYes, ship it.\n~~~\n"),
        "{page}"
    );
}

#[test]
fn a_dismissed_question_tells_the_master_once_that_no_answer_comes() {
    let case = Case::new();
    let id = case.ask("May Ada rename the crate?");

    dismiss(&case.db, &case.project, id, 0, NOW + 1)
        .unwrap()
        .unwrap();

    assert_eq!(
        case.messages(),
        [(
            "owner".to_owned(),
            format!(
                "The owner dismissed your question {id} (\"May Ada rename the crate?\") without an answer. Do not wait for it."
            ),
            "pending".to_owned(),
        )]
    );
    let current = stale(dismiss(&case.db, &case.project, id, 1, NOW + 2).unwrap());
    assert_eq!((current.state, current.answer), (State::Dismissed, None));
    assert_eq!(case.messages().len(), 1);
    assert!(case.page().contains("Dismissed by the owner."));
}

#[test]
fn a_full_master_conversation_keeps_the_question_pending_until_it_has_room() {
    let case = Case::new();
    let id = case.ask("Pick the release name?");
    {
        let tx = case.db.unchecked_transaction().unwrap();
        for n in 0..crate::team::MESSAGES_WAITING_MAX {
            append_message(
                &tx,
                &case.project,
                MessageAuthor::Owner,
                &format!("Earlier message {n}"),
                MessageState::Pending,
                NOW,
            )
            .unwrap()
            .unwrap();
        }
        tx.commit().unwrap();
    }

    assert_eq!(
        answer(&case.db, &case.project, id, 0, "Comet", NOW + 1).unwrap(),
        Err(Refusal::Code("question_delivery_full"))
    );

    let pending = list(&case.db, &case.project, true, None).unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|q| (q.id, q.version))
            .collect::<Vec<_>>(),
        [(id, 0)]
    );
    assert_eq!(case.version(id), 0);
    assert!(case.page().contains("## Waiting for the owner (1)"));
    case.db
        .execute("UPDATE team_messages SET state='answered'", [])
        .unwrap();
    answer(&case.db, &case.project, id, 0, "Comet", NOW + 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        case.count("SELECT COUNT(*) FROM team_messages WHERE state='pending'"),
        1
    );
}

#[test]
fn a_chat_answer_the_master_records_is_never_delivered_back_to_it() {
    let case = Case::new();
    let id = case.ask("Use Postgres or SQLite?");

    assert_eq!(
        record(&case.db, &case.project, 0, id, "SQLite", NOW).unwrap(),
        Err(Refusal::Code("question_invalid"))
    );
    record(&case.db, &case.project, MASTER, id, "SQLite", NOW + 1)
        .unwrap()
        .unwrap();

    assert!(case.messages().is_empty());
    let current = stale(answer(&case.db, &case.project, id, 0, "Postgres", NOW + 2).unwrap());
    assert_eq!(
        (current.answered_by, current.answer.as_deref()),
        (Some(AnsweredBy::Master), Some("SQLite"))
    );
    let tx = case.db.unchecked_transaction().unwrap();
    let writer = versioned::current(&tx, &item(&case.project, id).unwrap())
        .unwrap()
        .writer;
    assert_eq!(
        writer,
        versioned::LastWriter::Agent(AgentId::team(MASTER).unwrap())
    );
    drop(tx);
    assert!(
        case.page()
            .contains("Answered in chat, as the master recorded it:\n\n~~~text\nSQLite\n~~~\n")
    );
}

#[test]
fn an_engine_step_takes_each_outcome_exactly_once() {
    let case = Case::new();
    let raised = |step: Step, question: &str| {
        let tx = case.db.unchecked_transaction().unwrap();
        let id = raise(&tx, &case.project, step, &plain_ask(question), NOW)
            .unwrap()
            .unwrap();
        tx.commit().unwrap();
        id
    };
    let taken = |step: Step, commit: bool| {
        let tx = case.db.unchecked_transaction().unwrap();
        let taken = take(&tx, &case.project, step).unwrap();
        if commit {
            tx.commit().unwrap();
        }
        taken
    };
    let market = raised(Step::Research, "Which market first?");
    let scope = raised(Step::Mapping, "Is the admin page in scope?");
    assert!(taken(Step::Research, true).is_empty());
    let page = case.page();
    assert!(
        page.contains(&format!("### Question {market} from research")),
        "{page}"
    );
    assert!(
        page.contains(&format!("### Question {scope} from project mapping")),
        "{page}"
    );

    answer(&case.db, &case.project, market, 0, "Europe", NOW + 1)
        .unwrap()
        .unwrap();
    dismiss(&case.db, &case.project, scope, 0, NOW + 1)
        .unwrap()
        .unwrap();
    assert!(case.messages().is_empty());

    let europe = [Taken {
        id: market,
        answer: Some("Europe".into()),
    }];
    assert_eq!(taken(Step::Research, false), europe);
    assert_eq!(taken(Step::Research, true), europe);
    assert!(taken(Step::Research, true).is_empty());
    assert!(taken(Step::Landing, true).is_empty());
    assert_eq!(
        taken(Step::Mapping, true),
        [Taken {
            id: scope,
            answer: None
        }]
    );
    assert!(taken(Step::Mapping, true).is_empty());
}

#[test]
fn open_questions_stop_at_the_bound_and_wait_until_settled_and_taken() {
    let case = Case::new();
    let research = {
        let tx = case.db.unchecked_transaction().unwrap();
        let id = raise(
            &tx,
            &case.project,
            Step::Research,
            &plain_ask("Which region?"),
            NOW,
        )
        .unwrap()
        .unwrap();
        tx.commit().unwrap();
        id
    };
    let mut asked = Vec::new();
    for n in 1..OPEN_PER_PROJECT_MAX {
        asked.push(case.ask(&format!("Question number {n}?")));
    }
    assert_eq!(
        ask(
            &case.db,
            &case.project,
            MASTER,
            &plain_ask("One more?"),
            NOW
        )
        .unwrap(),
        Err("questions_full")
    );

    answer(&case.db, &case.project, research, 0, "EU", NOW)
        .unwrap()
        .unwrap();
    assert_eq!(
        ask(
            &case.db,
            &case.project,
            MASTER,
            &plain_ask("One more?"),
            NOW
        )
        .unwrap(),
        Err("questions_full")
    );
    let tx = case.db.unchecked_transaction().unwrap();
    assert_eq!(take(&tx, &case.project, Step::Research).unwrap().len(), 1);
    tx.commit().unwrap();
    let more = case.ask("One more?");
    assert_eq!(
        ask(
            &case.db,
            &case.project,
            MASTER,
            &plain_ask("And another?"),
            NOW
        )
        .unwrap(),
        Err("questions_full")
    );
    dismiss(&case.db, &case.project, asked[0], 0, NOW)
        .unwrap()
        .unwrap();
    case.ask("And another?");
    assert_eq!(
        case.count("SELECT COUNT(*) FROM owner_questions WHERE state='pending'"),
        OPEN_PER_PROJECT_MAX
    );
    assert!(asked.iter().chain([&more]).all(|id| *id > research));
}

#[test]
fn asks_outside_the_bounds_are_refused_before_anything_is_written() {
    let case = Case::new();
    let long = |n: usize| "x".repeat(n + 1);
    let with = |change: &dyn Fn(&mut Ask)| {
        let mut ask = friday();
        change(&mut ask);
        ask
    };
    let refused = [
        with(&|ask: &mut Ask| ask.question = "   ".into()),
        with(&|ask: &mut Ask| ask.question = long(QUESTION_BYTES_MAX)),
        with(&|ask: &mut Ask| ask.question = "ring \u{7}".into()),
        with(&|ask: &mut Ask| {
            ask.options = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]
        }),
        with(&|ask: &mut Ask| ask.options = vec!["two\nlines".into()]),
        with(&|ask: &mut Ask| ask.options = vec!["Yes".into(), " Yes ".into()]),
        with(&|ask: &mut Ask| ask.options = vec![long(OPTION_BYTES_MAX)]),
        with(&|ask: &mut Ask| ask.options = vec![String::new()]),
        with(&|ask: &mut Ask| ask.recommended = Some(" ".into())),
        with(&|ask: &mut Ask| ask.recommended = Some(long(RECOMMENDED_BYTES_MAX))),
        with(&|ask: &mut Ask| ask.context = Some(long(CONTEXT_BYTES_MAX))),
        with(&|ask: &mut Ask| ask.context = Some("escape \u{1b}[2J".into())),
    ];
    for ask_case in &refused {
        assert_eq!(
            ask(&case.db, &case.project, MASTER, ask_case, NOW).unwrap(),
            Err("question_invalid"),
            "{ask_case:?}"
        );
    }
    let ghost = ProjectId::new("ghost").unwrap();
    assert_eq!(
        ask(&case.db, &ghost, MASTER, &friday(), NOW).unwrap_err().0,
        "question_project_unknown"
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM owner_questions"), 0);
    assert_eq!(case.count("SELECT COUNT(*) FROM team_notifications"), 0);
    assert!(!case.page_path().exists());

    let trimmed = with(&|ask: &mut Ask| {
        ask.question = "  Why?  ".into();
        ask.options = vec![" A ".into(), "B".into()];
        ask.recommended = None;
    })
    .checked()
    .unwrap();
    assert_eq!(
        (trimmed.question.as_str(), trimmed.options),
        ("Why?", vec!["A".to_owned(), "B".to_owned()])
    );
    let id = case.ask("Name it?");
    for text in ["", "  ", long(ANSWER_BYTES_MAX).as_str(), "bell \u{7}"] {
        assert_eq!(
            answer(&case.db, &case.project, id, 0, text, NOW).unwrap(),
            Err(Refusal::Code("question_invalid"))
        );
    }
    assert_eq!(
        answer(&case.db, &case.project, id + 1, 0, "Yes", NOW).unwrap(),
        Err(Refusal::Code("question_missing"))
    );
    assert_eq!(case.version(id), 0);
}

#[test]
fn terminal_questions_are_logged_answered_on_screen_and_never_delivered() {
    let case = Case::new();
    let line = "Allow Bash(git push origin main)? 1. Yes 2. No";
    let id = asked_in_terminal(&case.db, &case.project, MASTER, line, NOW)
        .unwrap()
        .unwrap();
    let other = asked_in_terminal(&case.db, &case.project, MASTER + 1, "Allow Edit?", NOW)
        .unwrap()
        .unwrap();
    for text in [
        "two\nlines",
        "",
        "x".repeat(TERMINAL_BYTES_MAX + 1).as_str(),
    ] {
        assert_eq!(
            asked_in_terminal(&case.db, &case.project, MASTER, text, NOW).unwrap(),
            Err("question_invalid")
        );
    }
    assert_eq!(
        terminal_pending(&case.db, &case.project, MASTER).unwrap(),
        [(id, line.to_owned())]
    );
    assert!(case.page().contains(&format!(
        "### Question {id} from the master, asked in terminal"
    )));
    assert_eq!(
        answer(&case.db, &case.project, id, 0, "Yes", NOW).unwrap(),
        Err(Refusal::Code("question_in_terminal"))
    );

    answered_in_terminal(&case.db, id, NOW + 1)
        .unwrap()
        .unwrap();

    assert!(
        terminal_pending(&case.db, &case.project, MASTER)
            .unwrap()
            .is_empty()
    );
    let current = stale(answered_in_terminal(&case.db, id, NOW + 2).unwrap());
    assert_eq!(
        (current.state, current.answered_by, current.answer),
        (State::Answered, Some(AnsweredBy::Terminal), None)
    );
    assert_eq!(
        answered_in_terminal(&case.db, other + 100, NOW).unwrap(),
        Err(Refusal::Code("question_missing"))
    );
    let asked = case.ask("A master tool question?");
    assert_eq!(
        answered_in_terminal(&case.db, asked, NOW).unwrap(),
        Err(Refusal::Code("question_not_terminal"))
    );
    dismiss(&case.db, &case.project, other, 0, NOW)
        .unwrap()
        .unwrap();
    assert!(case.messages().is_empty());
    assert!(
        case.page()
            .contains("Answered by the owner in the master's terminal.")
    );
}

#[test]
fn asker_text_never_adds_structure_to_the_page() {
    let case = Case::new();
    let forged = Ask {
        question:
            "Pick one\n~~~\n## Answered and dismissed, newest first\n<img src=x onerror=alert(1)>"
                .into(),
        options: vec!["~~~~ close".into(), "# heading".into()],
        recommended: Some("Answered by the owner:\n~~~~~\nyes".into()),
        context: Some("~~~~~~~~\n### Question 99 from the master".into()),
    };
    let id = ask(&case.db, &case.project, MASTER, &forged, NOW)
        .unwrap()
        .unwrap();

    let page = case.page();
    assert_eq!(
        headings(&page),
        [
            "# Questions for the owner",
            "## Waiting for the owner (1)",
            format!("### Question {id} from the master").as_str(),
            "## Answered and dismissed, newest first",
        ],
        "{page}"
    );
    assert!(
        page.contains("~~~~~~~~~text\n~~~~~~~~\n### Question 99 from the master\n~~~~~~~~~\n"),
        "{page}"
    );
}

#[test]
fn a_planted_link_never_redirects_the_page_write() {
    let case = Case::new();
    let state = case.folder.join(CRAZYAGENT_DIRECTORY);
    let outside = case.base.join("outside.md");
    std::fs::write(&outside, "kept").unwrap();
    symlink(&outside, state.join("questions.md.partial")).unwrap();

    case.ask("First?");

    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept");
    std::fs::remove_file(case.page_path()).unwrap();
    symlink(&outside, case.page_path()).unwrap();
    case.ask("Second?");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept");
    assert!(
        std::fs::symlink_metadata(case.page_path())
            .unwrap()
            .file_type()
            .is_file()
    );
    assert!(case.page().contains("## Waiting for the owner (2)"));

    let real = case.folder.join("elsewhere");
    std::fs::rename(&state, &real).unwrap();
    symlink(&real, &state).unwrap();
    assert_eq!(
        ask(&case.db, &case.project, MASTER, &plain_ask("Third?"), NOW)
            .unwrap_err()
            .0,
        "questions_page_unavailable"
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM owner_questions"), 2);
    assert_eq!(case.count("SELECT COUNT(*) FROM team_notifications"), 2);
    assert!(
        std::fs::read_to_string(real.join(page::FILE))
            .unwrap()
            .contains("(2)")
    );
}

#[test]
fn lists_page_newest_first_within_one_owner_frame() {
    let case = Case::new();
    let mut asked = Vec::new();
    for n in 0..OPEN_PER_PROJECT_MAX - 1 {
        let full = Ask {
            question: format!("{n} {}", "q".repeat(QUESTION_BYTES_MAX - 4)),
            options: ["a", "b", "c", "d"]
                .iter()
                .map(|letter| letter.repeat(OPTION_BYTES_MAX))
                .collect(),
            recommended: Some("r".repeat(RECOMMENDED_BYTES_MAX)),
            context: Some("\"".repeat(CONTEXT_BYTES_MAX)),
        };
        asked.push(
            ask(&case.db, &case.project, MASTER, &full, NOW)
                .unwrap()
                .unwrap(),
        );
    }

    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let listed = list(&case.db, &case.project, false, before).unwrap();
        if listed.is_empty() {
            break;
        }
        assert!(listed.len() < asked.len());
        assert!(serde_json::to_vec(&listed).unwrap().len() <= LISTED_BYTES_MAX + 64);
        before = listed.last().map(|question| question.id);
        seen.extend(listed.into_iter().map(|question| question.id));
    }
    asked.reverse();
    assert_eq!(seen, asked);
}

#[test]
fn settled_history_keeps_the_newest_and_never_drops_an_untaken_outcome() {
    let case = Case::new();
    let research = {
        let tx = case.db.unchecked_transaction().unwrap();
        let id = raise(
            &tx,
            &case.project,
            Step::Research,
            &plain_ask("Which region?"),
            NOW,
        )
        .unwrap()
        .unwrap();
        tx.commit().unwrap();
        id
    };
    answer(&case.db, &case.project, research, 0, "EU", NOW)
        .unwrap()
        .unwrap();
    let mut settled = Vec::new();
    for n in 0..KEPT_PER_PROJECT_MAX + 3 {
        let id = asked_in_terminal(
            &case.db,
            &case.project,
            MASTER,
            &format!("Allow step {n}?"),
            NOW,
        )
        .unwrap()
        .unwrap();
        answered_in_terminal(&case.db, id, NOW).unwrap().unwrap();
        settled.push(id);
    }

    assert_eq!(
        case.count("SELECT COUNT(*) FROM owner_questions WHERE source='terminal'"),
        KEPT_PER_PROJECT_MAX
    );
    let kept: i64 = case
        .db
        .query_row(
            "SELECT MIN(id) FROM owner_questions WHERE source='terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, settled[3]);
    assert_eq!(case.version(settled[0]), 0);
    assert_eq!(case.version(settled[3]), 1);
    let tx = case.db.unchecked_transaction().unwrap();
    assert_eq!(
        take(&tx, &case.project, Step::Research).unwrap(),
        [Taken {
            id: research,
            answer: Some("EU".into())
        }]
    );
    tx.commit().unwrap();
    assert_eq!(
        case.count("SELECT COUNT(*) FROM owner_questions"),
        KEPT_PER_PROJECT_MAX
    );
    assert!(case.page().len() <= page::PAGE_BYTES_MAX);
}

#[test]
#[should_panic(expected = "a pending question always holds version 0")]
fn a_pending_question_whose_item_was_written_trips_the_invariant() {
    let case = Case::new();
    let id = case.ask("Still pending?");
    let tx = case.db.unchecked_transaction().unwrap();
    versioned::write_if(
        &tx,
        &item(&case.project, id).unwrap(),
        0,
        &Writer::Owner {
            force_after_conflict: false,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let _ = list(&case.db, &case.project, true, None);
}

#[test]
fn the_page_renders_the_newest_settled_questions_that_fit_its_bound() {
    let question = |id: i64, state: State| Question {
        id,
        project_id: ProjectId::new(PROJECT).unwrap(),
        source: Source::Master,
        agent: Some(MASTER),
        question: "q".repeat(QUESTION_BYTES_MAX),
        options: vec![],
        recommended: None,
        context: Some("c".repeat(CONTEXT_BYTES_MAX)),
        state,
        answer: Some("a".repeat(ANSWER_BYTES_MAX)),
        answered_by: Some(AnsweredBy::Owner),
        asked_ms: NOW,
        resolved_ms: Some(NOW),
        version: 1,
    };
    let settled: Vec<Question> = (1..=400)
        .rev()
        .map(|id| question(id, State::Answered))
        .collect();
    let page = page::render(&[], &settled);
    assert!(page.len() <= page::PAGE_BYTES_MAX);
    assert!(page.contains("### Question 400 from the master"));
    assert!(!page.contains("### Question 1 from the master\n"));
    let shown = page.matches("### Question ").count();
    assert!(shown > 100 && shown < 400, "{shown}");
    assert!(page.ends_with(&format!(
        "\n{} older questions are only in the Questions view.\n",
        400 - shown
    )));
}
