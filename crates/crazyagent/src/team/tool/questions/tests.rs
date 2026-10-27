use super::*;
use crate::protocol::ProjectId;
use crate::questions::{Asker, Note};
use rusqlite::Connection;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};

const PROJECT: &str = "alpha";
const AGENT: i64 = 7;

/// A private engine root with the engine database, one set-up project and an agent's
/// mailbox folder, as a team launch leaves them.
struct Case {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    mailbox: PathBuf,
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
        let mailbox = base.join("tmp").join(super::super::MAILBOX_FOLDER);
        for path in [&root, &folder, &mailbox] {
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
            mailbox,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn drop_file(&self) -> PathBuf {
        self.base.join("tmp").join(super::super::DROP_FILE)
    }

    /// Runs the sub-agent's ask server on the given requests while the engine side answers
    /// its mailbox, logging each question for agent `AGENT`. Returns the server's answers.
    fn served(&self, requests: &[Value]) -> Vec<Value> {
        let input = lines(requests);
        let mailbox = self.mailbox.clone();
        let server = std::thread::spawn(move || {
            let mut output = Vec::new();
            serve_agent(input, &mut output, &mailbox).map(|()| output)
        });
        let asker = Asker::Agent(AGENT);
        while !server.is_finished() {
            serve_agent_calls(&self.mailbox, |ask| {
                crate::questions::ask(&self.db, &self.project, &asker, ask)
            });
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        answers(&server.join().unwrap().unwrap())
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
        ]),
        &mut output,
        &drop,
    )
    .unwrap();
    let answered = answers(&output);
    let names: Vec<&str> = answered[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [super::super::TOOL_NAME, ASK_TOOL]);
    assert_eq!(answered[0]["result"]["tools"][1], super::super::ask_tool());
    assert_eq!(text(&answered[1]), (ASK_LOGGED, false));
    let (refused, error) = text(&answered[2]);
    assert!(error && refused.starts_with("Not asked"), "{refused}");
    let (refused, error) = text(&answered[3]);
    assert!(error && refused.starts_with("Not asked"), "{refused}");

    assert_eq!(take_asks(&drop), [shipped()]);
    assert!(!ask_file(&drop).exists(), "taking clears the record");
    assert!(take_asks(&drop).is_empty(), "a question is taken once");
    assert!(
        super::super::take(&drop).is_empty(),
        "a question is never read as a delegation"
    );
}

#[test]
fn one_reply_records_at_most_four_questions() {
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
    let mut planted = serde_json::to_vec(&Call::Status).unwrap();
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
fn a_sub_agent_server_lists_only_ask_and_logs_its_question_as_the_agent() {
    let case = Case::new();
    let answered = case.served(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ask_line(3, super::super::TOOL_NAME, json!({"task": "fix it"})),
        ask_line(4, ASK_TOOL, json!({"question": ""})),
        ask_line(5, ASK_TOOL, shipping()),
    ]);
    assert_eq!(answered.len(), 5);
    assert!(answered[0]["result"].is_object());
    assert_eq!(
        answered[1]["result"]["tools"],
        json!([super::super::ask_tool()])
    );
    assert_eq!(answered[2]["error"]["code"], -32602);
    let (refused, error) = text(&answered[3]);
    assert!(error && refused.starts_with("Not asked"), "{refused}");
    let (logged, error) = text(&answered[4]);
    assert!(!error, "{logged}");
    assert!(logged.starts_with("Question 1 is logged"), "{logged}");
    assert!(logged.contains("through your master"));

    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].asker, Asker::Agent(AGENT).name());
    assert_eq!(pending[0].question, "Ship the release on Friday?");
    assert_eq!(pending[0].recommended.as_deref(), Some("Friday"));
    assert_eq!(
        std::fs::read_dir(&case.mailbox).unwrap().count(),
        0,
        "every call was answered and cleared"
    );
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
    let answered = case.served(&[ask_line(1, ASK_TOOL, shipping())]);
    let (refused, error) = text(&answered[0]);
    assert!(error);
    assert!(
        refused.starts_with("Not asked: 8 of your questions are still open"),
        "{refused}"
    );
    assert_eq!(
        crate::questions::pending(&case.db, &case.project)
            .unwrap()
            .len(),
        crate::questions::PENDING_PER_ASKER_MAX as usize
    );

    std::fs::write(
        case.mailbox
            .join(format!("{}{}", "d".repeat(32), super::super::CALL_SUFFIX)),
        serde_json::to_vec(&Call::Status).unwrap(),
    )
    .unwrap();
    let other = Asker::Agent(AGENT + 1);
    assert_eq!(
        serve_agent_calls(&case.mailbox, |ask| {
            crate::questions::ask(&case.db, &case.project, &other, ask)
        }),
        1
    );
    assert_eq!(
        crate::questions::pending(&case.db, &case.project)
            .unwrap()
            .len(),
        crate::questions::PENDING_PER_ASKER_MAX as usize,
        "a call other than ask is answered and never logged"
    );
}

#[test]
fn the_sub_agent_server_needs_an_absolute_plain_mailbox() {
    let case = Case::new();
    let list = lines(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    assert!(serve_agent(list.clone(), Vec::new(), Path::new("relative")).is_err());
    let linked = case.base.join("linked");
    symlink(&case.mailbox, &linked).unwrap();
    assert!(serve_agent(list, Vec::new(), &linked).is_err());
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

    let long: Vec<Note> = (1..=3)
        .map(|question| Note {
            question,
            text: "a".repeat(crate::questions::ANSWER_BYTES_MAX),
        })
        .collect();
    let (lines, carried) = note_lines(&long);
    assert_eq!(carried, [1, 2], "the third waits for a later turn");
    assert!(lines.len() <= NOTES_BYTES_MAX);
    assert_eq!(lines.matches("Engine: ").count(), 2);
}
