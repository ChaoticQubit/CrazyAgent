mod e2e_support;

use crazyagent::harnesses::manifest::{DriverKind, RecordKind};
use crazyagent::harnesses::registry::Registry;
use crazyagent::team::Delegation;
use crazyagent::team::tool::{DROP_FILE, SERVER_COMMAND, SERVER_NAME, take};
use e2e_support::harness_standin::{self, KINDS, Standin};
use std::fs;

const INIT: &str = r#"{"type":"init","session":"s-1"}"#;
const ONE: &str = r#"{"type":"text","text":"one"}"#;
const TWO_ERROR: &str = r#"{"type":"error","text":"two"}"#;
const DONE: &str = r#"{"type":"done"}"#;
const READY: &str = r#"{"type":"ready"}"#;
const WORKING: &str = r#"{"type":"text","text":"working"}"#;
const NEVER: &str = r#"{"type":"text","text":"never"}"#;
const ABORTED: &str = r#"{"type":"aborted"}"#;
const PROMPT_ONE: &str = r#"{"type":"prompt","message":"one"}"#;
const PROMPT_TWO: &str = r#"{"type":"prompt","message":"two"}"#;
const ABORT: &str = r#"{"type":"abort"}"#;
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"authMethods":[]}}"#;
const NEW: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#;
const NEW_REPLY: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"sess_standin"}}"#;
const PROMPT: &str = r#"{"jsonrpc":"2.0","id":"p-2","method":"session/prompt","params":{"sessionId":"sess_standin","prompt":[{"type":"text","text":"say \"id\":99 hi"}]}}"#;
const PROMPT_REPLY: &str = r#"{"jsonrpc":"2.0","id":"p-2","result":{"stopReason":"end_turn"}}"#;
const CHUNK: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#;
const TOOL: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"Read","status":"pending"}}}"#;
const USAGE: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"usage_update","used":120,"size":200000}}}"#;
const PERMISSION: &str = r#"{"jsonrpc":"2.0","id":"perm-1","method":"session/request_permission","params":{"sessionId":"sess_standin","toolCall":{"toolCallId":"call_1"},"options":[{"optionId":"allow-once","name":"Allow","kind":"allow_once"}]}}"#;
const ALLOWED: &str = r#"{"jsonrpc":"2.0","id":"perm-1","result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}"#;
const TOOL_DONE: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed"}}}"#;
const TOOL_PROMPT: &str = r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"sess_standin","prompt":[{"type":"text","text":"tool"}]}}"#;
const TOOL_REPLY: &str = r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#;
const LONG_PROMPT: &str = r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"sess_standin","prompt":[{"type":"text","text":"cancel me"}]}}"#;
const CANCEL: &str =
    r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"sess_standin"}}"#;
const CANCELLED: &str = r#"{"jsonrpc":"2.0","id":4,"result":{"stopReason":"cancelled"}}"#;
const HISTORY: &str = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_standin","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"old"}}}}"#;
const LOAD: &str = r#"{"jsonrpc":"2.0","id":5,"method":"session/load","params":{"sessionId":"sess_standin","cwd":"/tmp","mcpServers":[]}}"#;
const LOADED: &str = r#"{"jsonrpc":"2.0","id":5,"result":{}}"#;
const AUTH_REQUIRED: &str = r#"{"code":-32000,"message":"Authentication required"}"#;
const NEW_REFUSED: &str =
    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Authentication required"}}"#;
const NOTICE: &str = r#"{"jsonrpc":"2.0","method":"x/notice","params":{}}"#;
const UNKNOWN: &str = r#"{"jsonrpc":"2.0","id":7,"method":"x/unknown","params":{}}"#;
const UNKNOWN_REPLY: &str =
    r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found"}}"#;
const ESCAPE: &str = r#"{"jsonrpc":"2.0","id":8,"method":"../../escape","params":{}}"#;
const DYING_PROMPT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"sess_standin","prompt":[]}}"#;
const SESSION_REFUSAL: &str =
    "standin: the session id is not 1 to 64 letters, digits, dots, dashes or underscores.\n";
const KIND_REFUSAL: &str = "standin: the program name or --kind names no stand-in kind.\n";
const HOME_REFUSAL: &str = "standin: --home or STANDIN_HOME must name an absolute folder.\n";
const MCP_INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"standin","version":"1"}}}"#;
const MCP_DELEGATE: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delegate","arguments":{"harness":"codex","task":"Write page.md"}}}"#;

/// Joins lines, each with its newline. Input: the lines. Output: the text.
fn lines(items: &[&str]) -> String {
    items.iter().map(|line| format!("{line}\n")).collect()
}

#[test]
fn every_stand_in_manifest_loads_together_each_with_its_own_program() {
    let texts: Vec<(String, String)> = KINDS
        .iter()
        .map(|kind| {
            (
                format!("standin-{kind}"),
                harness_standin::manifest_text(kind),
            )
        })
        .collect();
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(file, text)| (file.as_str(), text.as_str()))
        .collect();
    let registry = Registry::build(&files).unwrap();
    assert_eq!(registry.all().len(), KINDS.len());
    let drivers = [
        ("acp", DriverKind::Acp),
        ("jsonl-turn", DriverKind::JsonlTurn),
        ("jsonl-rpc", DriverKind::JsonlRpc),
        ("text-turn", DriverKind::TextTurn),
    ];
    for (kind, driver) in drivers {
        let harness = registry.get(&format!("standin-{kind}")).unwrap();
        assert_eq!(harness.piped(), Some(driver));
        assert_eq!(harness.record(), RecordKind::Unavailable);
        assert_eq!(harness.manifest().binaries, [format!("standin-{kind}")]);
    }
    let readers = [
        ("jsonl-append", RecordKind::JsonlAppend),
        ("json-rewrite", RecordKind::JsonRewrite),
        ("sqlite-poll", RecordKind::SqlitePoll),
        ("markdown-history", RecordKind::MarkdownHistory),
        ("screen", RecordKind::Screen),
    ];
    for (kind, reader) in readers {
        let harness = registry.get(&format!("standin-{kind}")).unwrap();
        assert_eq!(harness.piped(), None);
        assert_eq!(harness.record(), reader);
        assert_eq!(harness.manifest().binaries, [format!("standin-{kind}")]);
    }
}

#[test]
fn a_filled_piped_argv_from_the_manifest_runs_the_stand_in_as_written() {
    let standin = Standin::new("jsonl-turn");
    let home = standin.home().to_str().unwrap().to_owned();
    let manifest = harness_standin::manifest("jsonl-turn");
    let values = [
        ("home", home.as_str()),
        ("model", "m-1"),
        ("prompt", "hi there"),
        ("session", "s-2"),
    ];
    let mut argv = harness_standin::filled_argv(&manifest, &["piped", "argv"], &values);
    argv.extend(harness_standin::filled_argv(
        &manifest,
        &["piped", "resume"],
        &values,
    ));
    assert_eq!(
        argv,
        [
            "--home",
            home.as_str(),
            "--model",
            "m-1",
            "--prompt=hi there",
            "--resume",
            "s-2",
        ]
    );
    standin.put("turn.out", lines(&[DONE]));
    let ended = standin.run(&argv, b"");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(ended.stdout, lines(&[DONE]).as_bytes());
    assert_eq!(standin.argv(1), argv);
    assert_eq!(standin.log("prompt-1"), "hi there");
    assert_eq!(standin.log("session-1"), "s-2\n");
}

#[test]
fn a_filled_terminal_argv_writes_its_record_where_the_manifest_record_path_says() {
    let standin = Standin::new("jsonl-append");
    let home = standin.home().to_str().unwrap().to_owned();
    let manifest = harness_standin::manifest("jsonl-append");
    let values = [("home", home.as_str()), ("session", "s-filled")];
    let argv = harness_standin::filled_argv(&manifest, &["terminal", "argv"], &values);
    let path = ["terminal", "record", "path"];
    let record = harness_standin::filled_text(&manifest, &path, &values);
    let turn = r#"{"role":"user","text":"hello"}"#;
    standin.put("turn-1.jsonl", lines(&[turn]));
    let ended = standin.run(&argv, b"hello\n");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(record).unwrap_or_default(),
        lines(&[turn])
    );
    assert_eq!(standin.log("stdin-1"), "hello\n");
}

#[test]
fn jsonl_turn_prints_each_turn_ends_with_its_code_and_logs_what_it_was_given() {
    let standin = Standin::new("jsonl-turn");
    standin.put("turn-1.out", lines(&[INIT, ONE]));
    standin.put("turn-2.out", lines(&[TWO_ERROR]));
    standin.put("turn-2.err", "rate limited\n");
    standin.put("turn-2.exit", "3\n");
    let first = standin.run(&standin.at_home(&["--prompt", "first prompt"]), b"");
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(first.stdout, lines(&[INIT, ONE]).as_bytes());
    let again = standin.at_home(&["--resume", "s-1", "--prompt", "second"]);
    let second = standin.run(&again, b"");
    assert_eq!(second.status.code(), Some(3));
    assert_eq!(second.stdout, lines(&[TWO_ERROR]).as_bytes());
    assert_eq!(second.stderr, b"rate limited\n");
    assert_eq!(standin.argv(2), again);
    assert_eq!(standin.log("prompt-2"), "second");
    assert_eq!(standin.log("session-2"), "s-1\n");
    assert_eq!(standin.log("turns"), "2\n");
    let home = format!("HOME={}\n", standin.home().display());
    assert!(standin.log("env-1").contains(&home));
}

#[test]
fn text_turn_keeps_its_bytes_exactly_and_reads_its_stdin_whole() {
    let standin = Standin::new("text-turn");
    standin.put(
        "turn.out",
        "before\n@standin sleep 0\n@mention stays\nlast line without newline",
    );
    let ended = standin.run(&standin.at_home(&["--stdin"]), b"typed body");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        ended.stdout,
        b"before\n@mention stays\nlast line without newline"
    );
    assert_eq!(standin.log("stdin-1"), "typed body");
}

#[test]
fn jsonl_rpc_answers_each_prompt_line_and_an_interrupt_line_stops_the_turn() {
    let standin = Standin::new("jsonl-rpc");
    standin.put("ready.out", lines(&[READY]));
    standin.put("turn-1.out", lines(&[ONE, DONE]));
    standin.put("turn-2.out", lines(&[WORKING, "@standin wait", NEVER]));
    standin.put("interrupt.match", "abort");
    standin.put("interrupt.out", lines(&[ABORTED]));
    let mut live = standin.start(&standin.at_home(&[]));
    assert_eq!(live.line().as_deref(), Some(READY));
    live.send(PROMPT_ONE);
    assert_eq!(live.line().as_deref(), Some(ONE));
    assert_eq!(live.line().as_deref(), Some(DONE));
    live.send(PROMPT_TWO);
    assert_eq!(live.line().as_deref(), Some(WORKING));
    live.send(ABORT);
    assert_eq!(live.line().as_deref(), Some(ABORTED));
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(!ended.killed);
    assert!(!String::from_utf8_lossy(&ended.stdout).contains("never"));
    assert_eq!(
        standin.log("stdin-1"),
        lines(&[PROMPT_ONE, PROMPT_TWO, ABORT])
    );
    assert_eq!(standin.log("turns"), "2\n");
}

#[test]
fn acp_answers_its_setup_and_a_prompt_echoing_each_request_id() {
    let standin = Standin::new("acp");
    standin.put("session-id", "sess_standin");
    standin.put("turn-1.out", lines(&[CHUNK, TOOL, USAGE]));
    let mut live = standin.start(&standin.at_home(&[]));
    live.send(INITIALIZE);
    assert_eq!(live.line().as_deref(), Some(INITIALIZED));
    live.send(NEW);
    assert_eq!(live.line().as_deref(), Some(NEW_REPLY));
    live.send(PROMPT);
    assert_eq!(live.line().as_deref(), Some(CHUNK));
    assert_eq!(live.line().as_deref(), Some(TOOL));
    assert_eq!(live.line().as_deref(), Some(USAGE));
    assert_eq!(live.line().as_deref(), Some(PROMPT_REPLY));
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(standin.log("stdin-1"), lines(&[INITIALIZE, NEW, PROMPT]));
}

#[test]
fn acp_asks_permission_mid_turn_and_a_cancelled_turn_ends_as_cancelled() {
    let standin = Standin::new("acp");
    standin.put(
        "turn-1.out",
        lines(&[TOOL, PERMISSION, "@standin wait", TOOL_DONE]),
    );
    standin.put("turn-2.out", lines(&[CHUNK, "@standin wait", NEVER]));
    let mut live = standin.start(&standin.at_home(&[]));
    live.send(TOOL_PROMPT);
    assert_eq!(live.line().as_deref(), Some(TOOL));
    assert_eq!(live.line().as_deref(), Some(PERMISSION));
    live.send(ALLOWED);
    assert_eq!(live.line().as_deref(), Some(TOOL_DONE));
    assert_eq!(live.line().as_deref(), Some(TOOL_REPLY));
    live.send(LONG_PROMPT);
    assert_eq!(live.line().as_deref(), Some(CHUNK));
    live.send(CANCEL);
    assert_eq!(live.line().as_deref(), Some(CANCELLED));
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&ended.stdout).contains("never"));
    assert_eq!(
        standin.log("stdin-1"),
        lines(&[TOOL_PROMPT, ALLOWED, LONG_PROMPT, CANCEL])
    );
}

#[test]
fn acp_replays_a_load_answers_an_error_and_refuses_an_unknown_or_unsafe_method() {
    let standin = Standin::new("acp");
    standin.put("acp-session-load.out", lines(&[HISTORY]));
    standin.put("acp-session-new.error", AUTH_REQUIRED);
    let mut live = standin.start(&standin.at_home(&[]));
    live.send(NEW);
    assert_eq!(live.line().as_deref(), Some(NEW_REFUSED));
    live.send(NOTICE);
    live.send(LOAD);
    assert_eq!(live.line().as_deref(), Some(HISTORY));
    assert_eq!(live.line().as_deref(), Some(LOADED));
    live.send(ESCAPE);
    live.send(UNKNOWN);
    assert_eq!(live.line().as_deref(), Some(UNKNOWN_REPLY));
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(ended.stdout.is_empty());
    assert!(!standin.home().join("escape").exists());
    assert!(!standin.state().join("acp-..-..-escape.out").exists());
}

#[test]
fn acp_dies_mid_turn_with_the_exit_code_its_fixture_names() {
    let standin = Standin::new("acp");
    standin.put("turn.out", lines(&[CHUNK, "@standin exit 9", NEVER]));
    let mut live = standin.start(&standin.at_home(&[]));
    live.send(DYING_PROMPT);
    assert_eq!(live.line().as_deref(), Some(CHUNK));
    assert_eq!(live.line(), None);
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(9));
    assert!(!ended.killed);
}

#[test]
fn jsonl_append_appends_each_turn_and_keeps_a_half_written_last_line() {
    let standin = Standin::new("jsonl-append");
    let hello = r#"{"role":"user","text":"hello"}"#;
    let reply = r#"{"role":"assistant","text":"reply one"}"#;
    let half = r#"{"role":"user","text":"again"}"#.to_owned() + "\n{\"role\":\"assis";
    standin.put("turn-1.out", "reply one\n");
    standin.put("turn-1.jsonl", lines(&[hello, reply]));
    standin.put("turn-2.jsonl", &half);
    let ended = standin.run(&standin.at_home(&["--session", "s-7"]), b"hello\nagain\n");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(ended.stdout, b"> reply one\n> > ");
    let record = standin.state().join("records").join("s-7.jsonl");
    let written = fs::read_to_string(record).unwrap_or_default();
    assert_eq!(written, lines(&[hello, reply]) + &half);
}

#[test]
fn json_rewrite_replaces_the_whole_record_each_turn_and_leaves_no_temporary_file() {
    use std::os::unix::fs::MetadataExt;
    let standin = Standin::new("json-rewrite");
    standin.put("turn-1.json", "[{\"n\":1}]\n");
    standin.put("turn-2.json", "[{\"n\":1},{\"n\":2}]\n");
    let record = standin.state().join("records").join("s-8.json");
    let first = standin.run(&standin.at_home(&["--session", "s-8"]), b"one\n");
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&record).unwrap_or_default(),
        "[{\"n\":1}]\n"
    );
    let before = fs::metadata(&record).map(|found| found.ino()).unwrap_or(0);
    let second = standin.run(&standin.at_home(&["--resume", "s-8"]), b"two\n");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&record).unwrap_or_default(),
        "[{\"n\":1},{\"n\":2}]\n"
    );
    let after = fs::metadata(record).map(|found| found.ino()).unwrap_or(0);
    assert_ne!(
        before, after,
        "the record was written in place, not replaced"
    );
    let names: Vec<String> = fs::read_dir(standin.state().join("records"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["s-8.json"]);
}

#[test]
fn sqlite_poll_writes_rows_a_read_only_reader_sees_in_order() {
    let standin = Standin::new("sqlite-poll");
    standin.put(
        "turn-1.sql",
        "CREATE TABLE message(id INTEGER PRIMARY KEY, role TEXT, text TEXT);\n\
         INSERT INTO message(role, text) VALUES ('user', 'hello');\n",
    );
    standin.put(
        "turn-2.sql",
        "INSERT INTO message(role, text) VALUES ('assistant', 'reply');\n",
    );
    let ended = standin.run(&standin.at_home(&["--session", "s-9"]), b"hello\nagain\n");
    assert_eq!(ended.status.code(), Some(0));
    let database = standin.state().join("records").join("s-9.db");
    assert!(database.is_file(), "the sqlite-poll record was not written");
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = connection
        .prepare("SELECT role, text FROM message ORDER BY id")
        .unwrap();
    let rows: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            ("user".to_owned(), "hello".to_owned()),
            ("assistant".to_owned(), "reply".to_owned()),
        ]
    );
}

#[test]
fn markdown_history_appends_each_turn_to_the_record_it_is_told() {
    let standin = Standin::new("markdown-history");
    standin.put("turn-1.md", "#### hello\n\nreply one\n\n");
    standin.put("turn-2.md", "#### again\n\nreply two\n\n");
    let record = standin.home().join("project").join(".history.md");
    let path = record.to_str().unwrap();
    let ended = standin.run(&standin.at_home(&["--record", path]), b"hello\nagain\n");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(record).unwrap_or_default(),
        "#### hello\n\nreply one\n\n#### again\n\nreply two\n\n"
    );
    assert!(!standin.state().join("records").exists());
}

#[test]
fn screen_prints_its_first_screen_each_turn_and_its_prompt_mark_and_writes_no_record() {
    let standin = Standin::new("screen");
    standin.put("prompt-mark", "standin> ");
    standin.put("first-screen.out", "Welcome to stand-in\n");
    standin.put("turn-1.out", "reply one\n");
    let ended = standin.run(&standin.at_home(&[]), b"hello\n");
    assert_eq!(ended.status.code(), Some(0));
    assert_eq!(
        ended.stdout,
        b"Welcome to stand-in\nstandin> reply one\nstandin> "
    );
    assert!(!standin.state().join("records").exists());
}

#[test]
fn a_terminal_interrupt_stops_the_turn_and_an_idle_one_leaves_the_next_turn_whole() {
    let standin = Standin::new("screen");
    standin.put("turn-1.out", "working\n@standin sleep 5\nnever\n");
    standin.put("turn-2.out", "second\n@standin sleep 0.2\nsecond done\n");
    standin.put("interrupt.out", "interrupted\n");
    let mut live = standin.start(&standin.at_home(&[]));
    live.send("go");
    assert_eq!(live.line().as_deref(), Some("> working"));
    assert!(live.signal("INT"));
    assert_eq!(live.line().as_deref(), Some("interrupted"));
    assert!(live.signal("INT"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    live.send("next");
    assert_eq!(live.line().as_deref(), Some("> second"));
    assert_eq!(live.line().as_deref(), Some("second done"));
    let ended = live.finish();
    assert_eq!(ended.status.code(), Some(0));
    assert!(!ended.killed);
    assert_eq!(ended.stdout, b"> ");
    assert_eq!(standin.log("stdin-1"), "go\nnext\n");
}

#[test]
fn a_bad_session_id_kind_or_home_is_refused_before_anything_is_written() {
    let standin = Standin::new("jsonl-turn");
    let escape = standin.run(&standin.at_home(&["--session", "../escape"]), b"");
    assert_eq!(escape.status.code(), Some(2));
    assert_eq!(escape.stderr, SESSION_REFUSAL.as_bytes());
    let nope = standin.run(&standin.at_home(&["--kind", "nope"]), b"");
    assert_eq!(nope.status.code(), Some(2));
    assert_eq!(nope.stderr, KIND_REFUSAL.as_bytes());
    let relative = standin.run(&["--home".to_owned(), "relative".to_owned()], b"");
    assert_eq!(relative.status.code(), Some(2));
    assert_eq!(relative.stderr, HOME_REFUSAL.as_bytes());
    assert_eq!(fs::read_dir(standin.state()).unwrap().count(), 0);
    standin.put("session-id", "../file");
    let filed = standin.run(&standin.at_home(&[]), b"");
    assert_eq!(filed.status.code(), Some(2));
    assert_eq!(filed.stderr, SESSION_REFUSAL.as_bytes());
    assert!(!standin.home().join("file").exists());
}

/// T19b 19b-3: on a turn whose `turn-<n>.mcp` fixture holds MCP lines, the ACP stand-in runs
/// the MCP server its `session/new` named with those lines on its input and keeps the answer
/// in `mcp-<n>.out`; the engine's own `team-mcp` server then takes the `delegate` call into
/// its drop file, as a stand-in lead's call reaches the runtime.
#[test]
fn acp_runs_the_mcp_server_its_session_names_and_keeps_its_answer() {
    let standin = Standin::new("acp");
    standin.put("session-id", "sess_standin");
    standin.put("turn-1.mcp", lines(&[MCP_INITIALIZE, MCP_DELEGATE]));
    standin.put("turn-1.out", lines(&[CHUNK]));
    let drop = standin.home().canonicalize().unwrap().join(DROP_FILE);
    let new = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/new",
        "params": {"cwd": "/tmp", "mcpServers": [{
            "name": SERVER_NAME,
            "command": env!("CARGO_BIN_EXE_crazyagent"),
            "args": [SERVER_COMMAND, drop.to_str().unwrap()],
            "env": []
        }]}
    })
    .to_string();
    let mut live = standin.start(&standin.at_home(&[]));
    live.send(INITIALIZE);
    assert_eq!(live.line().as_deref(), Some(INITIALIZED));
    live.send(&new);
    assert_eq!(live.line().as_deref(), Some(NEW_REPLY));
    live.send(PROMPT);
    assert_eq!(live.line().as_deref(), Some(CHUNK));
    assert_eq!(live.line().as_deref(), Some(PROMPT_REPLY));
    assert_eq!(live.finish().status.code(), Some(0));
    let kept = standin.log("mcp-1.out");
    let answers: Vec<serde_json::Value> = kept
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        answers
            .iter()
            .map(|answer| answer["id"].clone())
            .collect::<Vec<_>>(),
        [serde_json::json!(1), serde_json::json!(2)],
        "{kept}"
    );
    assert_eq!(answers[1]["result"]["isError"], false, "{kept}");
    assert_eq!(
        take(&drop),
        [Delegation {
            harness: Some("codex".into()),
            prompt: "Write page.md".into(),
            role: None,
        }]
    );
}
