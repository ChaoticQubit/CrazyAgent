use super::*;
use crate::questions::{Ask, State};
use serde_json::json;

const PROJECT: &str = "alpha";

/// An engine root whose database holds the team tables and one set-up project with its
/// `.crazyagent/` folder. Output: the temporary folder, the engine root and the project.
fn engine() -> (tempfile::TempDir, PathBuf, ProjectId) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-questions-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::create_dir(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    let db = questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::team::initialize(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    (tmp, root, ProjectId::new(PROJECT).unwrap())
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body. Inputs: the engine root and the body. Output: the reply, or the
/// refusal.
fn send(root: &Path, body: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Questions));
    run(root, parse(&bytes)?)
}

fn listed(root: &Path, pending_only: bool, before: Option<i64>) -> Vec<Question> {
    let body = json!({"questions": {"kind": "list", "project": PROJECT, "pending_only": pending_only, "before": before}});
    match send(root, body).unwrap() {
        Reply::Listed { questions } => questions,
        other => panic!("expected a list, got {other:?}"),
    }
}

fn asked(question: &str) -> Ask {
    Ask {
        question: question.into(),
        options: vec![],
        recommended: None,
        context: None,
    }
}

#[test]
fn the_owner_lists_answers_and_dismisses_each_question_once_by_its_version() {
    let (_tmp, root, project) = engine();
    let db = questions::open(&root).unwrap();
    let first = questions::ask(&db, &project, 41, &asked("Ship on Friday?"), 1)
        .unwrap()
        .unwrap();
    let second = questions::ask(&db, &project, 41, &asked("Rename the app?"), 2)
        .unwrap()
        .unwrap();

    let pending = listed(&root, true, None);
    assert_eq!(
        pending
            .iter()
            .map(|q| (q.id, q.version))
            .collect::<Vec<_>>(),
        [(second, 0), (first, 0)]
    );
    let answer = |id: i64, version: u64, text: &str| {
        send(
            &root,
            json!({"questions": {"kind": "answer", "project": PROJECT, "id": id, "version": version, "answer": text}}),
        )
    };
    let dismiss = |id: i64, version: u64| {
        send(
            &root,
            json!({"questions": {"kind": "dismiss", "project": PROJECT, "id": id, "version": version}}),
        )
    };
    assert!(matches!(answer(first, 0, "Yes").unwrap(), Reply::Answered));
    match answer(first, 0, "No").unwrap() {
        Reply::Stale { question } => assert_eq!(
            (question.state, question.answer.as_deref(), question.version),
            (State::Answered, Some("Yes"), 1)
        ),
        other => panic!("expected stale, got {other:?}"),
    }
    match dismiss(second, 1).unwrap() {
        Reply::Stale { question } => assert_eq!(
            (question.id, question.state, question.version),
            (second, State::Pending, 0)
        ),
        other => panic!("expected stale, got {other:?}"),
    }
    assert!(matches!(dismiss(second, 0).unwrap(), Reply::Dismissed));

    assert!(listed(&root, true, None).is_empty());
    assert_eq!(listed(&root, false, None).len(), 2);
    assert_eq!(
        listed(&root, false, Some(second))
            .iter()
            .map(|q| q.id)
            .collect::<Vec<_>>(),
        [first]
    );
    let refused = [
        answer(second + 1, 0, "Yes").unwrap_err().0,
        answer(first, 1, " ").unwrap_err().0,
    ];
    assert_eq!(refused, ["question_missing", "question_invalid"]);
    let messages: i64 = db
        .query_row("SELECT COUNT(*) FROM team_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 2);
    for code in &refused {
        assert!(QUESTIONS_CODES.contains(&code.as_str()), "{code}");
    }
}

#[test]
fn a_full_list_of_the_largest_questions_fits_one_owner_frame() {
    let (_tmp, root, project) = engine();
    let db = questions::open(&root).unwrap();
    for n in 0..questions::OPEN_PER_PROJECT_MAX {
        let largest = Ask {
            question: format!("{n} {}", "\\".repeat(questions::QUESTION_BYTES_MAX - 4)),
            options: ["a", "b", "c", "d"]
                .iter()
                .map(|letter| letter.repeat(questions::OPTION_BYTES_MAX))
                .collect(),
            recommended: Some("\"".repeat(questions::RECOMMENDED_BYTES_MAX)),
            context: Some("c\n".repeat(questions::CONTEXT_BYTES_MAX / 2)),
        };
        questions::ask(&db, &project, 41, &largest, 1)
            .unwrap()
            .unwrap();
    }
    let questions = listed(&root, false, None);
    assert!(!questions.is_empty());
    assert!(encode_frame(&Reply::Listed { questions }).is_ok());
}

#[test]
fn malformed_or_oversized_questions_requests_are_refused_before_the_log() {
    for body in [
        r#"{"questions":{"kind":"list","project":"alpha"}}"#,
        r#"{"questions":{"kind":"answer","project":"alpha","id":1,"answer":"x"}}"#,
        r#"{"questions":{"kind":"answer","project":"a b","id":1,"version":0,"answer":"x"}}"#,
        r#"{"questions":{"kind":"dismiss","project":"alpha","id":1,"version":-1}}"#,
        r#"{"questions":{"kind":"dismiss","project":"alpha","id":1,"version":0,"force":true}}"#,
        r#"{"questions":{"kind":"ask","project":"alpha","question":"x"}}"#,
        r#"{"questions":{"kind":"answer","project":"alpha","id":"1","version":0,"answer":"x"}}"#,
    ] {
        assert_eq!(
            parse(body.as_bytes()).unwrap_err().0,
            "questions_request_invalid",
            "{body}"
        );
    }
    let long = format!(
        r#"{{"questions":{{"kind":"list","project":"alpha","pending_only":true,"before":null}},"pad":"{}"}}"#,
        "x".repeat(REQUEST_BYTES_MAX)
    );
    assert_eq!(
        parse(long.as_bytes()).unwrap_err().0,
        "questions_request_bounds"
    );
}

#[test]
fn questions_codes_cover_every_refusal_of_the_protocol_and_log_and_stay_sorted() {
    let sources = [
        include_str!("questions.rs"),
        include_str!("../../../questions.rs"),
        include_str!("../../../questions/page.rs"),
    ];
    let found: Vec<&str> = sources
        .iter()
        .flat_map(|source| {
            ["failure(\"", "code: \"", "Refusal::Code(\""]
                .into_iter()
                .flat_map(move |opening| source.split(opening).skip(1))
        })
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(found.len() >= 12, "{found:?}");
    for code in found {
        assert!(
            QUESTIONS_CODES.contains(&code),
            "{code} is not in QUESTIONS_CODES"
        );
    }
    let mut sorted = QUESTIONS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, QUESTIONS_CODES);
}
