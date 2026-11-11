use super::*;
use crate::protocol::ProjectId;
use crate::questions::{Asker, Note};
use rusqlite::Connection;
use serde_json::json;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};

const PROJECT: &str = "alpha";
const AGENT: i64 = 7;

/// A private engine root with the engine database and one set-up project, as a team launch
/// leaves them.
struct Case {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    db: Connection,
    project: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join("project");
        let tmp_folder = base.join("tmp");
        for path in [&root, &folder, &tmp_folder] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        let db = crate::questions::open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        Self {
            _tmp: tmp,
            base,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn drop_file(&self) -> PathBuf {
        self.base.join("tmp").join(super::super::DROP_FILE)
    }
}

fn lines(requests: &[Value]) -> Cursor<Vec<u8>> {
    let mut input = Vec::new();
    for request in requests {
        input.extend(serde_json::to_vec(request).unwrap());
        input.push(b'\n');
    }
    Cursor::new(input)
}

fn answers(output: &[u8]) -> Vec<Value> {
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn ask_line(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

fn text(answer: &Value) -> (&str, bool) {
    let result = &answer["result"];
    (
        result["content"][0]["text"].as_str().unwrap(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

fn shipping() -> Value {
    json!({
        "question": "Ship the release on Friday?",
        "options": ["Friday", "Monday"],
        "recommended": "Friday",
        "context": "Every check passes today."
    })
}

fn shipped() -> Call {
    Call::Ask {
        question: "Ship the release on Friday?".into(),
        options: vec!["Friday".into(), "Monday".into()],
        recommended: Some("Friday".into()),
        context: Some("Every check passes today.".into()),
    }
}

#[test]
fn the_drop_file_server_records_checked_questions_that_the_engine_takes_once() {
    let case = Case::new();
    let drop = case.drop_file();
    let mut output = Vec::new();
    super::super::serve(
        lines(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            ask_line(2, ASK_TOOL, shipping()),
            ask_line(
                3,
                ASK_TOOL,
                json!({"question": "Which day?", "options": ["Friday"], "recommended": "Sunday"}),
            ),
            ask_line(4, ASK_TOOL, json!({"question": "Which day?", "id": 3})),
            ask_line(
                5,
                RECORD_TOOL,
                json!({"question": 3, "version": 0, "owner_turn": 41}),
            ),
            ask_line(6, RECORD_TOOL, json!({"question": 3, "answer": "Yes"})),
        ]),
        &mut output,
        &drop,
        None,
        &[],
    )
    .unwrap();
    let answered = answers(&output);
    let names: Vec<&str> = answered[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [super::super::TOOL_NAME, ASK_TOOL, RECORD_TOOL]);
    assert_eq!(answered[0]["result"]["tools"][1], super::super::ask_tool());
    assert_eq!(
        answered[0]["result"]["tools"][2],
        super::super::record_tool()
    );
    assert_eq!(text(&answered[1]), (ASK_LOGGED, false));
    let (refused, error) = text(&answered[2]);
    assert!(error && refused.starts_with("Not asked"), "{refused}");
    let (refused, error) = text(&answered[3]);
    assert!(error && refused.starts_with("Not asked"), "{refused}");
    assert_eq!(text(&answered[4]), (RECORD_TAKEN, false));
    let (refused, error) = text(&answered[5]);
    assert!(error && refused.starts_with("Not recorded"), "{refused}");

    assert_eq!(
        take_asks(&drop),
        [
            shipped(),
            Call::RecordAnswer {
                question: 3,
                version: 0,
                owner_turn: 41
            }
        ]
    );
    assert!(!ask_file(&drop).exists(), "taking clears the record");
    assert!(take_asks(&drop).is_empty(), "a question is taken once");
    assert!(
        super::super::take(&drop).is_empty(),
        "a question is never read as a delegation"
    );
}

#[test]
fn one_reply_records_at_most_eight_question_calls() {
    let case = Case::new();
    let drop = case.drop_file();
    for _ in 0..ASKS_PER_REPLY_MAX {
        let params = json!({"name": ASK_TOOL, "arguments": shipping()});
        assert_eq!(
            drop_call(Some(&params), &drop),
            Some((ASK_LOGGED.to_owned(), false))
        );
    }
    let params = json!({"name": ASK_TOOL, "arguments": shipping()});
    assert_eq!(
        drop_call(Some(&params), &drop),
        Some((ASKS_FULL.to_owned(), true))
    );
    let delegate = json!({"name": super::super::TOOL_NAME, "arguments": {"task": "x"}});
    assert_eq!(drop_call(Some(&delegate), &drop), None);
    assert_eq!(take_asks(&drop).len(), ASKS_PER_REPLY_MAX);
}

#[test]
fn the_engine_keeps_only_checked_questions_and_never_follows_a_linked_record() {
    let case = Case::new();
    let drop = case.drop_file();
    let mut planted = serde_json::to_vec(&Call::InspectWork).unwrap();
    planted.extend(b"\nnot json\n");
    planted.extend(serde_json::to_vec(&shipped()).unwrap());
    planted.push(b'\n');
    let unchecked = Call::Ask {
        question: "Which day?".into(),
        options: vec!["Friday".into()],
        recommended: Some("Sunday".into()),
        context: None,
    };
    planted.extend(serde_json::to_vec(&unchecked).unwrap());
    planted.push(b'\n');
    std::fs::write(ask_file(&drop), &planted).unwrap();
    assert_eq!(take_asks(&drop), [shipped()]);

    let target = case.base.join("elsewhere.jsonl");
    std::fs::write(&target, serde_json::to_vec(&shipped()).unwrap()).unwrap();
    symlink(&target, ask_file(&drop)).unwrap();
    let params = json!({"name": ASK_TOOL, "arguments": shipping()});
    assert_eq!(
        drop_call(Some(&params), &drop),
        Some((ASK_UNWRITABLE.to_owned(), true))
    );
    assert!(take_asks(&drop).is_empty());
    assert!(target.is_file(), "the link's target is left alone");
    assert!(!ask_file(&drop).exists());

    std::fs::write(ask_file(&drop), serde_json::to_vec(&shipped()).unwrap()).unwrap();
    clear_asks(&drop);
    assert!(!ask_file(&drop).exists());
    clear_asks(&drop);
}

#[test]
fn a_sub_agent_server_lists_ask_but_not_record_answer_and_logs_its_question_as_the_agent() {
    let sub_agent = [RECORD_TOOL.to_owned()];
    let listed = super::super::team_tools(&sub_agent);
    let tools = listed["tools"].as_array().unwrap();
    assert_eq!(tools.last(), Some(&super::super::ask_tool()));
    assert!(tools.iter().all(|tool| tool["name"] != RECORD_TOOL));
    assert!(super::super::known_tools(&sub_agent));

    let case = Case::new();
    assert_eq!(
        super::super::live_call(ASK_TOOL, Some(&json!({"question": ""}))),
        Some(Err(super::super::ASK_INVALID))
    );
    let (logged, error) = agent_ask(&case.db, &case.project, AGENT, shipped());
    assert!(!error, "{logged}");
    assert!(logged.starts_with("Question 1 is logged"), "{logged}");
    assert!(logged.contains("types the owner's answer or dismissal into your terminal"));
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].asker, Asker::Agent(AGENT).name());
    assert_eq!(pending[0].question, "Ship the release on Friday?");
    assert_eq!(pending[0].recommended.as_deref(), Some("Friday"));
}

#[test]
fn a_sub_agent_hears_why_its_question_was_not_logged() {
    let case = Case::new();
    let asker = Asker::Agent(AGENT);
    let ask = crate::questions::Ask {
        question: "Ship the release on Friday?".into(),
        options: Vec::new(),
        recommended: None,
        context: None,
    };
    for _ in 0..crate::questions::PENDING_PER_ASKER_MAX {
        crate::questions::ask(&case.db, &case.project, &asker, &ask).unwrap();
    }
    let (refused, error) = agent_ask(&case.db, &case.project, AGENT, shipped());
    assert!(error);
    assert!(
        refused.starts_with("Not asked: 8 of your questions are still open"),
        "{refused}"
    );
    let record = Call::RecordAnswer {
        question: 1,
        version: 0,
        owner_turn: 1,
    };
    let (refused, error) = agent_ask(&case.db, &case.project, AGENT + 1, record);
    assert!(error);
    assert_eq!(refused, AGENT_ASKS_ONLY);
    assert_eq!(
        crate::questions::pending(&case.db, &case.project)
            .unwrap()
            .len(),
        crate::questions::PENDING_PER_ASKER_MAX as usize,
        "a refused question and a sub-agent's record_answer log nothing"
    );
}

#[test]
fn note_lines_carry_the_oldest_notes_that_fit_and_name_them() {
    assert_eq!(note_lines(&[]), (String::new(), Vec::new()));
    let short = [
        Note {
            question: 3,
            text: "The owner answered your question 3 (\"Ship?\"):\nFriday".into(),
        },
        Note {
            question: 5,
            text: "The owner dismissed your question 5 (\"Rename?\"). No answer comes.".into(),
        },
    ];
    let (lines, carried) = note_lines(&short);
    assert_eq!(
        lines,
        "Engine: The owner answered your question 3 (\"Ship?\"):\nFriday\n\nEngine: The owner dismissed your question 5 (\"Rename?\"). No answer comes.\n\n"
    );
    assert_eq!(carried, [3, 5]);

    let half = NOTES_BYTES_MAX / 2 - NOTE_PREFIX.len() - 2;
    let long: Vec<Note> = (1..=3)
        .map(|question| Note {
            question,
            text: "a".repeat(half),
        })
        .collect();
    let (lines, carried) = note_lines(&long);
    assert_eq!(carried, [1, 2], "the third waits for a later turn");
    assert_eq!(
        lines.len(),
        NOTES_BYTES_MAX,
        "two notes fill the budget exactly"
    );
    assert_eq!(lines.matches("Engine: ").count(), 2);
}

/// Logs a plain question for `asker` straight into the store and returns its id.
fn asked(case: &Case, asker: &Asker, question: &str) -> u64 {
    let ask = crate::questions::Ask {
        question: question.into(),
        options: Vec::new(),
        recommended: None,
        context: None,
    };
    crate::questions::ask(&case.db, &case.project, asker, &ask).unwrap()
}

/// Writes one question call into the drop file as the master's tool server does.
fn called(drop: &Path, name: &str, arguments: Value) {
    let params = json!({"name": name, "arguments": arguments});
    let (said, refused) = drop_call(Some(&params), drop).unwrap();
    assert!(!refused, "{said}");
}

fn open_ids(case: &Case) -> Vec<u64> {
    crate::questions::pending(&case.db, &case.project)
        .unwrap()
        .iter()
        .map(|question| question.id)
        .collect()
}

#[test]
fn a_drop_file_master_logs_its_questions_and_records_a_chat_answer_from_the_owner() {
    let case = Case::new();
    let drop = case.drop_file();
    let master = Asker::Master(41);
    let renamed = asked(&case, &master, "Rename the app?");
    let tx = case.db.unchecked_transaction().unwrap();
    let typed = crate::team::TeamAction::Message {
        project_id: case.project.clone(),
        text: "Yes, rename it.".into(),
    };
    assert_eq!(
        crate::team::execute(&tx, &crate::config::ModelAccess::default(), &typed).unwrap(),
        crate::protocol::Outcome::Applied
    );
    let owner: i64 = tx
        .query_row(
            "SELECT MAX(id) FROM team_messages WHERE project=?1",
            [case.project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    tx.commit().unwrap();

    called(&drop, ASK_TOOL, shipping());
    called(
        &drop,
        RECORD_TOOL,
        json!({"question": renamed, "version": 0, "owner_turn": owner}),
    );
    called(
        &drop,
        RECORD_TOOL,
        json!({"question": 999, "version": 0, "owner_turn": owner}),
    );
    assert_eq!(
        apply_master_calls(&case.db, &case.project, 41, &drop),
        ["Your record of question 999 was not taken: you asked no such question in this project."]
    );
    assert!(take_asks(&drop).is_empty(), "the calls are taken once");
    let open = crate::questions::pending(&case.db, &case.project).unwrap();
    assert_eq!(open.len(), 1, "the renamed question is answered");
    assert_eq!(open[0].question, "Ship the release on Friday?");
    assert_eq!(open[0].asker, master.name());

    for n in 1..crate::questions::PENDING_PER_ASKER_MAX {
        asked(&case, &master, &format!("Question {n}?"));
    }
    called(&drop, ASK_TOOL, json!({"question": "One more?"}));
    assert_eq!(
        apply_master_calls(&case.db, &case.project, 41, &drop),
        [
            "Your question \"One more?\" was not logged for the owner: 8 of your questions are still open."
        ]
    );
    assert!(apply_master_calls(&case.db, &case.project, 41, &drop).is_empty());
}

#[test]
fn a_conversation_master_records_only_from_its_turns_owner_message_and_before_new_questions() {
    let case = Case::new();
    let drop = case.drop_file();
    let session = SessionId::new("session-1").unwrap();
    let older = asked(&case, &Asker::Session(session.clone()), "Rename the app?");
    let fresh = older + 1;

    called(&drop, ASK_TOOL, shipping());
    called(
        &drop,
        RECORD_TOOL,
        json!({"question": fresh, "version": 0, "owner_turn": 5}),
    );
    called(
        &drop,
        RECORD_TOOL,
        json!({"question": older, "version": 0, "owner_turn": 4}),
    );
    called(
        &drop,
        RECORD_TOOL,
        json!({"question": older, "version": 0, "owner_turn": 5}),
    );
    assert_eq!(
        apply_session_calls(
            &case.db,
            &case.project,
            &session,
            Some((5, "Yes, rename it.")),
            &drop
        ),
        [
            format!(
                "Your record of question {fresh} was not taken: you asked no such question in this project."
            ),
            format!(
                "Your record of question {older} was not taken: message 4 is not the owner's message of that turn."
            ),
        ],
        "a question asked in the same reply is logged only after the records"
    );
    assert_eq!(open_ids(&case), [fresh]);
    assert!(
        crate::questions::session_notes(&case.db, &case.project, &session)
            .unwrap()
            .is_empty(),
        "a recorded chat answer is not handed back as a note"
    );

    called(
        &drop,
        RECORD_TOOL,
        json!({"question": fresh, "version": 0, "owner_turn": 6}),
    );
    assert_eq!(
        apply_session_calls(&case.db, &case.project, &session, None, &drop),
        [format!(
            "Your record of question {fresh} was not taken: message 6 is not the owner's message of that turn."
        )],
        "an engine turn carries no owner message"
    );
    assert_eq!(open_ids(&case), [fresh]);
}

#[test]
fn a_conversation_masters_turn_message_answers_only_one_of_its_questions() {
    let case = Case::new();
    let drop = case.drop_file();
    let session = SessionId::new("session-1").unwrap();
    let asker = Asker::Session(session.clone());
    let first = asked(&case, &asker, "Ship on Friday?");
    let second = asked(&case, &asker, "Rename the app?");
    for question in [first, second] {
        called(
            &drop,
            RECORD_TOOL,
            json!({"question": question, "version": 0, "owner_turn": 5}),
        );
    }
    assert_eq!(
        apply_session_calls(&case.db, &case.project, &session, Some((5, "Yes.")), &drop),
        [format!(
            "Your record of question {second} was not taken: message 5 already answered another question."
        )]
    );
    assert_eq!(open_ids(&case), [second]);
}

#[test]
fn the_open_line_names_only_the_askers_open_questions_and_the_message_to_record_from() {
    let case = Case::new();
    let master = Asker::Master(41);
    assert_eq!(open_line(&[], &master, Some(5)), "");
    let first = asked(&case, &master, "Ship on\nFriday?");
    asked(&case, &Asker::Master(42), "Rename the app?");
    let second = asked(&case, &master, "Which \"database\"?");
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    assert_eq!(open_line(&pending, &Asker::Master(43), Some(5)), "");
    let listed = format!(
        "Engine: Your questions still open for the owner: {first} \"Ship on Friday?\"; {second} \"Which \\\"database\\\"?\"."
    );
    assert_eq!(open_line(&pending, &master, None), format!("{listed}\n\n"));
    assert_eq!(
        open_line(&pending, &master, Some(5)),
        format!(
            "{listed} The owner's message below is message 5. If it answers one of them, record it with record_answer: the question's number, version 0 and owner_turn 5.\n\n"
        )
    );

    let crowded = Asker::Master(44);
    for _ in 0..crate::questions::PENDING_PER_ASKER_MAX {
        asked(
            &case,
            &crowded,
            &"\"".repeat(crate::questions::QUESTION_BYTES_MAX),
        );
    }
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    let line = open_line(&pending, &crowded, Some(u64::MAX));
    assert!(line.len() <= OPEN_LINE_BYTES_MAX, "{}", line.len());
    assert_eq!(line.matches("; ").count(), 7);
}
