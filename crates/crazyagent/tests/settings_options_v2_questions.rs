#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::protocol::ProjectId;
use crazyagent::questions::{self, Answer, Ask, EngineSource, QuestionStatus, Settlement};
use e2e_support::{Engine, Owner, scratch};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::fs;

const QUESTION: &str = "Ship the alarm page before the tide map?";
const ALARMS: &str = "Yes, the alarm page first";
const MAP: &str = "No, the tide map first";
const TYPED: &str = "Ship both, the map behind a flag.";

/// The running engine with one project set up through the owner protocol, and the roadmap
/// planner's open point logged as an owner question the way the planning step raises it.
struct Asked {
    engine: Engine,
    owner: Owner,
    project: ProjectId,
    id: u64,
    _disk: tempfile::TempDir,
}

/// What the engine stored for one question and the one message that tells its asker:
/// the status, the answer and the message's author and text, if any.
#[derive(Debug, PartialEq, Eq)]
struct Told {
    status: String,
    answer: Option<String>,
    message: Option<(String, String)>,
}

impl Asked {
    /// Starts the engine, creates the project and raises the question. Input: a scratch
    /// prefix and project id. Output: the case.
    async fn start(prefix: &str, id: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-q-home-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(id, "Buoy questions").await,
            OwnerResult::Applied
        );
        let project = ProjectId::new(id).unwrap();
        let db = questions::open(engine.path()).unwrap();
        let question = questions::raise(
            &db,
            &project,
            &EngineSource::Planning,
            &Ask {
                question: QUESTION.into(),
                options: vec![ALARMS.into(), MAP.into()],
                recommended: Some(ALARMS.into()),
                context: Some("An open point of the roadmap.".into()),
            },
        )
        .unwrap();
        Self {
            engine,
            owner,
            project,
            id: question,
            _disk: disk,
        }
    }

    /// The owner's Questions view of this question: its status and the version an answer
    /// names. Output: both, read through the owner protocol.
    async fn shown(&self) -> (QuestionStatus, u64, Option<String>) {
        let page = self
            .owner
            .controller
            .questions_list(self.project.clone(), None)
            .await
            .unwrap();
        let item = page
            .items
            .into_iter()
            .find(|item| item.id == self.id)
            .expect("the question is listed");
        assert_eq!(item.question, QUESTION);
        (item.status, item.version, item.answer)
    }

    /// Reads the stored outcome from the running engine's database without writing to it.
    fn told(&self) -> Told {
        let db = Connection::open_with_flags(
            self.engine.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (status, answer, message): (String, Option<String>, Option<i64>) = db
            .query_row(
                "SELECT status, answer, message FROM questions_log WHERE id=?1",
                params![i64::try_from(self.id).unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let message = message.and_then(|id| {
            db.query_row(
                "SELECT author, text FROM team_messages WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
        });
        Told {
            status,
            answer,
            message,
        }
    }

    /// Counts the messages that name this question, so a second settle can be shown to add
    /// none. Output: the count.
    fn messages_naming(&self) -> i64 {
        let db = Connection::open_with_flags(
            self.engine.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT count(*) FROM team_messages WHERE project=?1 AND text LIKE ?2",
            params![
                self.project.as_str(),
                format!("%roadmap planning question {} %", self.id)
            ],
            |row| row.get(0),
        )
        .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answered_by_option_settles_it_and_tells_the_planner_that_option() {
    let case = Asked::start("ca-v2opt-q-option-", "v2-q-option").await;
    let (status, version, _) = case.shown().await;
    assert_eq!((status, version), (QuestionStatus::Pending, 0));

    let settled = case
        .owner
        .controller
        .questions_answer(case.id, version, Answer::Option(1))
        .await
        .unwrap();
    assert_eq!(settled, Settlement::Settled);
    assert_eq!(
        case.shown().await,
        (QuestionStatus::Answered, 1, Some(MAP.into()))
    );
    let told = case.told();
    assert_eq!(told.status, "answered");
    assert_eq!(told.answer.as_deref(), Some(MAP));
    let (author, text) = told.message.expect("the answer is told once");
    assert_eq!(author, "engine");
    assert!(
        text.starts_with("The owner answered the roadmap planning question")
            && text.ends_with(&format!(":\n{MAP}")),
        "{text}"
    );
    assert_eq!(case.messages_naming(), 1);

    let again = case
        .owner
        .controller
        .questions_answer(case.id, version, Answer::Option(0))
        .await
        .unwrap();
    assert_eq!(
        again,
        Settlement::Stale,
        "a settled question takes no second answer"
    );
    assert_eq!(case.told().answer.as_deref(), Some(MAP));
    assert_eq!(case.messages_naming(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answered_by_typed_text_tells_the_planner_the_owner_words() {
    let mut case = Asked::start("ca-v2opt-q-text-", "v2-q-text").await;
    let (_, version, _) = case.shown().await;
    let settled = case
        .owner
        .controller
        .questions_answer(case.id, version, Answer::Text(TYPED.into()))
        .await
        .unwrap();
    assert_eq!(settled, Settlement::Settled);
    let told = case.told();
    assert_eq!(told.status, "answered");
    assert_eq!(told.answer.as_deref(), Some(TYPED));
    let (author, text) = told.message.expect("the typed answer is told once");
    assert_eq!(author, "engine");
    assert!(text.ends_with(&format!(":\n{TYPED}")), "{text}");
    assert_eq!(case.messages_naming(), 1);

    case.engine.restart();
    case.owner = Owner::attach(&case.engine).await;
    assert_eq!(
        case.shown().await,
        (QuestionStatus::Answered, 1, Some(TYPED.into())),
        "the typed answer survives a restart"
    );
    assert_eq!(case.messages_naming(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn question_dismissed_settles_it_with_no_answer_and_tells_the_planner_none_comes() {
    let case = Asked::start("ca-v2opt-q-dismiss-", "v2-q-dismiss").await;
    let (_, version, _) = case.shown().await;
    let settled = case
        .owner
        .controller
        .questions_dismiss(case.id, version)
        .await
        .unwrap();
    assert_eq!(settled, Settlement::Settled);
    assert_eq!(case.shown().await, (QuestionStatus::Dismissed, 1, None));
    let told = case.told();
    assert_eq!(told.status, "dismissed");
    assert_eq!(told.answer, None);
    let (author, text) = told.message.expect("the dismissal is told once");
    assert_eq!(author, "engine");
    assert!(
        text.starts_with("The owner dismissed the roadmap planning question")
            && text.ends_with("No answer comes."),
        "{text}"
    );
    let late = case
        .owner
        .controller
        .questions_answer(case.id, version, Answer::Option(0))
        .await
        .unwrap();
    assert_eq!(
        late,
        Settlement::Stale,
        "a dismissed question takes no answer"
    );
    assert_eq!(case.told().answer, None);
    assert_eq!(case.messages_naming(), 1);
}
