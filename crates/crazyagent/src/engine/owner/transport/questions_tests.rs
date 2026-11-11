use super::*;
#[cfg(unix)]
use crate::questions::{Ask, Asker, Page, QuestionStatus, Settlement};
use serde_json::json;

#[cfg(unix)]
const PROJECT: &str = "alpha";

/// An engine root whose database holds the team tables and one set-up project with its
/// `.crazyagent/` folder. Output: the temporary folder, the engine root and the project.
#[cfg(unix)]
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
#[cfg(unix)]
fn send(root: &Path, body: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Questions));
    run(root, parse(&bytes)?)
}

#[cfg(unix)]
fn listed(root: &Path, after: Option<u64>) -> Page {
    let body = json!({"questions": {"kind": "list", "project": PROJECT, "after": after}});
    match send(root, body).unwrap() {
        Reply::Page { page } => page,
        other => panic!("expected a page, got {other:?}"),
    }
}

#[cfg(unix)]
fn settled(reply: Result<Reply, EngineError>) -> Settlement {
    match reply.unwrap() {
        Reply::Settled { settlement } => settlement,
        other => panic!("expected a settlement, got {other:?}"),
    }
}

#[cfg(unix)]
fn asked(question: &str) -> Ask {
    Ask {
        question: question.into(),
        kind: crate::questions::QuestionKind::Single,
        options: vec!["Yes".into(), "No".into()],
        recommended: Some("Yes".into()),
        context: None,
    }
}

#[cfg(unix)]
#[test]
fn the_owner_lists_answers_and_dismisses_each_question_once_by_its_version() {
    let (_tmp, root, project) = engine();
    let db = questions::open(&root).unwrap();
    let master = Asker::Master(41);
    let first = questions::ask(&db, &project, &master, &asked("Ship on Friday?")).unwrap();
    let second = questions::ask(&db, &project, &master, &asked("Rename the app?")).unwrap();

    let page = listed(&root, None);
    assert_eq!(
        page.items
            .iter()
            .map(|q| (q.id, q.version, q.status))
            .collect::<Vec<_>>(),
        [
            (first, 0, QuestionStatus::Pending),
            (second, 0, QuestionStatus::Pending)
        ]
    );
    assert_eq!(page.next, None);
    match send(&root, json!({"questions": {"kind": "counts"}})).unwrap() {
        Reply::Counts { counts } => assert_eq!(counts, [(project.clone(), 2)]),
        other => panic!("expected counts, got {other:?}"),
    }
    let answer = |id: u64, version: u64, choice: serde_json::Value| {
        let mut body = json!({"kind": "answer", "id": id, "version": version});
        body.as_object_mut()
            .unwrap()
            .extend(choice.as_object().unwrap().clone());
        send(&root, json!({ "questions": body }))
    };
    let dismiss = |id: u64, version: u64| {
        send(
            &root,
            json!({"questions": {"kind": "dismiss", "id": id, "version": version}}),
        )
    };
    assert_eq!(
        settled(answer(first, 0, json!({"picks": [0]}))),
        Settlement::Settled
    );
    assert_eq!(
        settled(answer(first, 0, json!({"comment": "No"}))),
        Settlement::Stale
    );
    assert_eq!(settled(dismiss(second, 1)), Settlement::Stale);
    assert_eq!(settled(dismiss(second, 0)), Settlement::Settled);

    let page = listed(&root, None);
    assert_eq!(
        page.items
            .iter()
            .map(|q| (q.id, q.status, q.answer.as_deref(), q.version))
            .collect::<Vec<_>>(),
        [
            (second, QuestionStatus::Dismissed, None, 1),
            (first, QuestionStatus::Answered, Some("Yes"), 1)
        ]
    );
    match send(&root, json!({"questions": {"kind": "counts"}})).unwrap() {
        Reply::Counts { counts } => assert!(counts.is_empty()),
        other => panic!("expected counts, got {other:?}"),
    }
    let refused = [
        answer(second + 1, 0, json!({"comment": "Yes"}))
            .unwrap_err()
            .0,
        answer(first, 1, json!({"picks": [], "comment": null}))
            .unwrap_err()
            .0,
        answer(first, 1, json!({})).unwrap_err().0,
    ];
    assert_eq!(
        refused,
        [
            "questions_not_found",
            "questions_answer_invalid",
            "questions_answer_invalid"
        ]
    );
    let third = questions::ask(&db, &project, &master, &asked("Use Postgres?")).unwrap();
    let refused = [
        answer(third, 0, json!({"picks": [2]})).unwrap_err().0,
        answer(third, 0, json!({"picks": [0, 1]})).unwrap_err().0,
        answer(third, 0, json!({"comment": "   "})).unwrap_err().0,
        answer(third, 0, json!({"picks": [0], "comment": "yes\u{202e}no"}))
            .unwrap_err()
            .0,
    ];
    assert_eq!(
        refused,
        [
            "questions_option_invalid",
            "questions_option_invalid",
            "questions_answer_invalid",
            "questions_text_invalid"
        ]
    );
    let messages: Vec<(String, String)> = {
        let mut statement = db
            .prepare("SELECT author,text FROM team_messages ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        messages,
        [
            (
                "owner".to_owned(),
                format!("The owner answered your question {first} (\"Ship on Friday?\"):\nYes")
            ),
            (
                "owner".to_owned(),
                format!(
                    "The owner dismissed your question {second} (\"Rename the app?\"). No answer comes."
                )
            ),
        ]
    );
    assert_eq!(
        settled(answer(
            third,
            0,
            json!({"picks": [1], "comment": "Not yet."})
        )),
        Settlement::Settled
    );
    assert_eq!(
        listed(&root, None).items[0].answer.as_deref(),
        Some("No\nComment: Not yet.")
    );
    for code in &refused {
        assert!(QUESTIONS_CODES.contains(&code.as_str()), "{code}");
    }
}

#[cfg(unix)]
#[test]
fn a_full_page_of_the_largest_questions_fits_one_owner_frame() {
    let (_tmp, root, project) = engine();
    let db = questions::open(&root).unwrap();
    let mut ids = Vec::new();
    for n in 0..questions::PENDING_PER_PROJECT_MAX {
        let largest = Ask {
            question: format!("{n} {}", "\\".repeat(questions::QUESTION_BYTES_MAX - 4)),
            kind: crate::questions::QuestionKind::Single,
            options: ["a", "b", "c", "d", "e", "f"]
                .iter()
                .map(|letter| letter.repeat(questions::OPTION_BYTES_MAX))
                .collect(),
            recommended: Some("a".repeat(questions::OPTION_BYTES_MAX)),
            context: Some("c\n".repeat(questions::CONTEXT_BYTES_MAX / 2)),
        };
        let asker = Asker::Agent(1 + n / questions::PENDING_PER_ASKER_MAX);
        ids.push(questions::ask(&db, &project, &asker, &largest).unwrap());
    }
    let mut seen = Vec::new();
    let mut after = None;
    for _ in 0..ids.len() {
        let page = listed(&root, after);
        assert!(!page.items.is_empty());
        seen.extend(page.items.iter().map(|q| q.id));
        assert!(encode_frame(&Reply::Page { page: page.clone() }).is_ok());
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, ids);
}

#[test]
fn malformed_or_oversized_questions_requests_are_refused_before_the_log() {
    for body in [
        r#"{"questions":{"kind":"list"}}"#,
        r#"{"questions":{"kind":"list","project":"alpha","pending_only":true}}"#,
        r#"{"questions":{"kind":"answer","id":1,"comment":"x"}}"#,
        r#"{"questions":{"kind":"list","project":"a b","after":null}}"#,
        r#"{"questions":{"kind":"dismiss","id":1,"version":-1}}"#,
        r#"{"questions":{"kind":"dismiss","id":1,"version":0,"force":true}}"#,
        r#"{"questions":{"kind":"answer","id":1,"version":0,"picks":[256]}}"#,
        r#"{"questions":{"kind":"answer","id":1,"version":0,"picks":0}}"#,
        r#"{"questions":{"kind":"answer","id":1,"version":0,"option":1}}"#,
        r#"{"questions":{"kind":"answer","id":1,"version":0,"text":"x"}}"#,
        r#"{"questions":{"kind":"ask","project":"alpha","question":"x"}}"#,
        r#"{"questions":{"kind":"answer","id":"1","version":0,"comment":"x"}}"#,
        r#"{"questions":{"kind":"counts","project":"alpha"}}"#,
    ] {
        assert_eq!(
            parse(body.as_bytes()).unwrap_err().0,
            "questions_request_invalid",
            "{body}"
        );
    }
    let long = format!(
        r#"{{"questions":{{"kind":"list","project":"alpha","after":null}},"pad":"{}"}}"#,
        "x".repeat(REQUEST_BYTES_MAX)
    );
    assert_eq!(
        parse(long.as_bytes()).unwrap_err().0,
        "questions_request_bounds"
    );
    let largest = json!({"questions": {"kind": "answer", "id": u64::MAX, "version": u64::MAX, "picks": [0, 1, 2, 3, 4, 5], "comment": "\\".repeat(questions::ANSWER_BYTES_MAX)}});
    let bytes = serde_json::to_vec(&largest).unwrap();
    assert!(bytes.len() <= REQUEST_BYTES_MAX, "{}", bytes.len());
    assert!(parse(&bytes).is_ok());
}

#[test]
fn questions_codes_cover_every_refusal() {
    let sources = [
        include_str!("questions.rs"),
        include_str!("../../../questions.rs"),
        include_str!("../../../questions/page.rs"),
        include_str!("../../../storage/versioned.rs"),
    ];
    let found: Vec<&str> = sources
        .iter()
        .flat_map(|source| {
            ["failure(\"", "code: \""]
                .into_iter()
                .flat_map(move |opening| source.split(opening).skip(1))
        })
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(found.len() >= 20, "{found:?}");
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
