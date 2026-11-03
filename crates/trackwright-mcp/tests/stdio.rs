use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use trackwright_core::catalog::{catalog, op_schema};
use trackwright_core::model::{Actor, OpKind, ProjectKey};
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, Store};
use trackwright_mcp::{
    BATCH_REQUESTS_MAX, LINE_BYTES_MAX, PROTOCOL_VERSIONS, SERVER_NAME, Session, serve,
};

const AGENT: &str = "agent:7";

/// A real standalone tracker database in a private temporary folder, opened and migrated by
/// the core's store and holding one project `CA` created by the actor `setup`.
struct Tracker {
    _folder: TempDir,
    path: PathBuf,
}

impl Tracker {
    fn new() -> Self {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let path = folder.path().join("tracker.db");
        let mut store = Store::open(&path).expect("the standalone store opens and migrates");
        let tx = store.transaction().expect("a write transaction");
        let setup = Actor::new("setup").expect("a valid actor");
        let key = ProjectKey::new("CA").expect("a valid key");
        store::create_project(&tx, &setup, &key, "CrazyAgent", 0).expect("the project creates");
        tx.commit().expect("the setup commits");
        Self {
            _folder: folder,
            path,
        }
    }

    fn connection(&self) -> Connection {
        Store::open(&self.path)
            .expect("the standalone store reopens")
            .into_connection()
    }

    fn session(&self, actor: &str) -> Session<Standalone> {
        let actor = Actor::new(actor).expect("a valid actor");
        Session::new(self.connection(), Standalone, actor)
    }

    fn impatient_session(&self) -> Session<Standalone> {
        let connection = self.connection();
        connection
            .busy_timeout(Duration::ZERO)
            .expect("the busy timeout sets");
        Session::new(
            connection,
            Standalone,
            Actor::new(AGENT).expect("a valid actor"),
        )
    }

    fn tickets(&self) -> Vec<(String, String)> {
        let connection = Connection::open(&self.path).expect("a reader connection opens");
        let mut statement = connection
            .prepare("SELECT title, created_by FROM tk_tickets ORDER BY number")
            .expect("the tickets query prepares");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("the tickets query runs")
            .collect::<Result<_, _>>()
            .expect("every ticket row reads")
    }

    fn event_actors(&self) -> Vec<String> {
        let connection = Connection::open(&self.path).expect("a reader connection opens");
        let mut statement = connection
            .prepare("SELECT DISTINCT actor FROM tk_events WHERE ticket IS NOT NULL ORDER BY actor")
            .expect("the events query prepares");
        statement
            .query_map([], |row| row.get(0))
            .expect("the events query runs")
            .collect::<Result<_, _>>()
            .expect("every event row reads")
    }
}

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn call(id: u64, tool: &str, arguments: Value) -> String {
    rpc(
        id,
        "tools/call",
        json!({"name": tool, "arguments": arguments}),
    )
}

fn exchange(session: &Session<Standalone>, lines: &[String]) -> Vec<Value> {
    let mut input = lines.join("\n");
    input.push('\n');
    let mut output = Vec::new();
    serve(Cursor::new(input.into_bytes()), &mut output, session)
        .expect("serve ends cleanly when its input closes");
    let text = String::from_utf8(output).expect("the server writes UTF-8");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("each output line is one JSON-RPC message"))
        .collect()
}

fn envelope(reply: &Value) -> (bool, Value) {
    let result = &reply["result"];
    let content = result["content"]
        .as_array()
        .expect("a tool result has content");
    assert_eq!(content.len(), 1, "one text block per tool result");
    assert_eq!(content[0]["type"], "text");
    let text = content[0]["text"].as_str().expect("the block holds text");
    let envelope: Value = serde_json::from_str(text).expect("the text is the envelope's JSON");
    assert_eq!(envelope["api"], 1);
    let is_error = result["isError"].as_bool().expect("isError is a bool");
    assert_eq!(
        envelope["ok"].as_bool(),
        Some(!is_error),
        "isError mirrors the envelope's ok"
    );
    (is_error, envelope)
}

fn error_code(reply: &Value) -> i64 {
    assert!(
        reply.get("result").is_none(),
        "an error reply has no result: {reply}"
    );
    reply["error"]["code"]
        .as_i64()
        .expect("a JSON-RPC error code")
}

fn padded_ping(id: u64, length: usize) -> String {
    let empty = rpc(id, "ping", json!({"pad": ""}));
    assert!(length >= empty.len(), "the target length fits the ping");
    let line = rpc(id, "ping", json!({"pad": "x".repeat(length - empty.len())}));
    assert_eq!(line.len(), length);
    line
}

#[test]
fn serve_answers_initialize_lists_the_catalog_and_runs_ready() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            rpc(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "stdio-test", "version": "0"},
                }),
            ),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            rpc(2, "tools/list", json!({})),
            call(
                3,
                "create",
                json!({"project": "CA", "title": "Wire the tracker"}),
            ),
            call(4, "ready", json!({"project": "CA"})),
        ],
    );
    assert_eq!(replies.len(), 4, "the notification gets no reply");
    for (reply, id) in replies.iter().zip(1..) {
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], id, "replies come in request order");
    }

    let initialized = &replies[0]["result"];
    assert_eq!(initialized["protocolVersion"], "2025-06-18");
    assert!(initialized["capabilities"]["tools"].is_object());
    assert_eq!(initialized["serverInfo"]["name"], SERVER_NAME);
    assert_eq!(
        initialized["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );

    let tools = replies[1]["result"]["tools"]
        .as_array()
        .expect("a tool list");
    assert_eq!(tools.len(), catalog().len());

    let (created_failed, created) = envelope(&replies[2]);
    assert!(!created_failed, "create succeeds: {created}");
    assert!(!created["data"].is_null());
    assert_eq!(
        tracker.tickets(),
        vec![("Wire the tracker".to_owned(), AGENT.to_owned())]
    );

    let (ready_failed, ready) = envelope(&replies[3]);
    assert!(!ready_failed, "ready succeeds: {ready}");
    assert!(!ready["data"].is_null());
}

#[test]
fn initialize_echoes_a_supported_version_and_offers_the_latest_otherwise() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let latest = PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1];
    let mut lines: Vec<String> = PROTOCOL_VERSIONS
        .iter()
        .zip(1..)
        .map(|(version, id)| rpc(id, "initialize", json!({"protocolVersion": version})))
        .collect();
    lines.push(rpc(
        10,
        "initialize",
        json!({"protocolVersion": "1999-01-01"}),
    ));
    lines.push(rpc(11, "initialize", json!({})));
    let replies = exchange(&session, &lines);
    assert_eq!(replies.len(), PROTOCOL_VERSIONS.len() + 2);
    for (reply, version) in replies.iter().zip(PROTOCOL_VERSIONS) {
        assert_eq!(reply["result"]["protocolVersion"], version);
    }
    for reply in &replies[PROTOCOL_VERSIONS.len()..] {
        assert_eq!(reply["result"]["protocolVersion"], latest);
    }
}

#[test]
fn tools_list_matches_the_catalog() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(&session, &[rpc(1, "tools/list", json!({}))]);
    assert_eq!(replies.len(), 1);
    let tools = replies[0]["result"]["tools"]
        .as_array()
        .expect("a tool list");
    assert_eq!(tools.len(), catalog().len());
    assert_eq!(
        catalog().len(),
        OpKind::ALL.len(),
        "the catalog covers every op"
    );
    for (tool, spec) in tools.iter().zip(catalog()) {
        let keys: Vec<&str> = tool
            .as_object()
            .expect("a tool is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys.len(),
            3,
            "a tool has a name, a description and a schema only"
        );
        assert_eq!(tool["name"], spec.kind.as_str());
        assert_eq!(tool["description"], spec.summary);
        assert_eq!(tool["inputSchema"], op_schema(spec)["args"]);
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    for op in OpKind::ALL {
        let listed = tools
            .iter()
            .filter(|tool| tool["name"] == op.as_str())
            .count();
        assert_eq!(listed, 1, "{op} is listed once");
    }
}

#[test]
fn a_call_cannot_set_its_own_actor() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            call(
                1,
                "create",
                json!({"project": "CA", "title": "Spoofed", "actor": "owner"}),
            ),
            rpc(
                2,
                "tools/call",
                json!({
                    "name": "create",
                    "arguments": {"project": "CA", "title": "Mine"},
                    "actor": "owner",
                    "_meta": {"actor": "owner"},
                }),
            ),
        ],
    );
    assert_eq!(replies.len(), 2);

    let (spoof_failed, spoof) = envelope(&replies[0]);
    assert!(spoof_failed, "an actor argument is refused: {spoof}");
    assert_eq!(spoof["ok"], false);

    let (mine_failed, mine) = envelope(&replies[1]);
    assert!(
        !mine_failed,
        "params outside the arguments are ignored: {mine}"
    );
    assert_eq!(
        tracker.tickets(),
        vec![("Mine".to_owned(), AGENT.to_owned())],
        "only the session's actor ever writes"
    );
    assert_eq!(tracker.event_actors(), vec![AGENT.to_owned()]);
}

#[test]
fn an_oversized_line_is_refused_and_the_server_keeps_serving() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            padded_ping(1, LINE_BYTES_MAX + 1),
            rpc(2, "ping", json!({})),
            padded_ping(3, LINE_BYTES_MAX),
            padded_ping(4, LINE_BYTES_MAX * 3),
            call(5, "list", json!({"project": "CA"})),
        ],
    );
    assert_eq!(replies.len(), 5);

    assert_eq!(error_code(&replies[0]), -32600);
    assert_eq!(
        replies[0]["id"], 1,
        "an oversized request is answered with its own id"
    );
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["result"], json!({}));
    assert_eq!(replies[2]["id"], 3, "a line of exactly the bound is served");
    assert_eq!(replies[2]["result"], json!({}));
    assert_eq!(error_code(&replies[3]), -32600);
    assert_eq!(replies[3]["id"], 4);
    assert_eq!(replies[4]["id"], 5);
    let (list_failed, list) = envelope(&replies[4]);
    assert!(!list_failed, "list still runs after refused lines: {list}");

    let mut output = Vec::new();
    let unterminated = padded_ping(6, LINE_BYTES_MAX + 1).into_bytes();
    serve(Cursor::new(unterminated), &mut output, &session)
        .expect("an oversized last line without a newline ends cleanly");
    let last: Value = serde_json::from_slice(output.trim_ascii()).expect("one reply");
    assert_eq!(error_code(&last), -32600);
    assert_eq!(last["id"], 6);
}

fn oversized(before: &str, after: &str) -> String {
    let line = format!("{before}{}{after}", "x".repeat(LINE_BYTES_MAX * 2));
    assert!(line.len() > LINE_BYTES_MAX);
    line
}

#[test]
fn an_oversized_line_is_answered_with_its_top_level_id_wherever_it_stands() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            oversized(
                concat!(
                    r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"create","#,
                    r#""arguments":{"project":"CA","title":""#,
                ),
                r#""}},"id":"late-7"}"#,
            ),
            oversized(
                r#"{"jsonrpc":"2.0","method":"ping","params":{"note":""#,
                r#""},"id":"a\"}b"}"#,
            ),
            oversized(
                r#"{"jsonrpc":"2.0","method":"ping","params":{"id":9,"pad":""#,
                r#""}}"#,
            ),
            oversized(
                r#"{"jsonrpc":"2.0","method":"ping","id":{"x":1},"params":{"pad":""#,
                r#""}}"#,
            ),
            oversized(
                r#"[{"jsonrpc":"2.0","id":5,"method":"ping","params":{"pad":""#,
                r#""}}]"#,
            ),
            rpc(6, "ping", json!({})),
        ],
    );
    assert_eq!(replies.len(), 6);
    for reply in &replies[..5] {
        assert_eq!(error_code(reply), -32600);
    }
    assert_eq!(
        replies[0]["id"], "late-7",
        "an id after the long params is found"
    );
    assert_eq!(
        replies[1]["id"], "a\"}b",
        "quotes and braces inside strings are skipped"
    );
    assert_eq!(
        replies[2]["id"],
        Value::Null,
        "an id inside params is not the request's"
    );
    assert_eq!(replies[3]["id"], Value::Null, "an object is not an id");
    assert_eq!(
        replies[4]["id"],
        Value::Null,
        "an oversized batch has no one id"
    );
    assert_eq!(replies[5]["id"], 6);
    assert_eq!(replies[5]["result"], json!({}));
    assert!(
        tracker.tickets().is_empty(),
        "nothing in an oversized line runs"
    );
}

#[test]
fn a_batch_gets_one_reply_per_request_in_one_array() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "ping"},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "create", "arguments": {"project": "CA", "title": "Batched"}},
        },
        {"jsonrpc": "2.0", "id": 3, "method": "resources/list"},
        5,
    ]);
    let notifications = json!([{"jsonrpc": "2.0", "method": "notifications/initialized"}]);
    let full: Vec<Value> = (0..BATCH_REQUESTS_MAX)
        .map(|n| json!({"jsonrpc": "2.0", "id": 100 + n, "method": "ping"}))
        .collect();
    let mut over: Vec<Value> = vec![json!({
        "jsonrpc": "2.0",
        "id": 200,
        "method": "tools/call",
        "params": {"name": "create", "arguments": {"project": "CA", "title": "Too many"}},
    })];
    over.extend(full.iter().cloned());
    assert_eq!(over.len(), BATCH_REQUESTS_MAX + 1);
    let replies = exchange(
        &session,
        &[
            batch.to_string(),
            notifications.to_string(),
            Value::Array(full).to_string(),
            Value::Array(over).to_string(),
            rpc(9, "ping", json!({})),
        ],
    );
    assert_eq!(
        replies.len(),
        4,
        "a batch of notifications gets no reply at all"
    );

    let answered = replies[0]
        .as_array()
        .expect("a batch is answered with an array");
    assert_eq!(answered.len(), 4, "the notification gets no element");
    assert_eq!(answered[0]["id"], 1);
    assert_eq!(answered[0]["result"], json!({}));
    assert_eq!(answered[1]["id"], 2);
    let (created_failed, created) = envelope(&answered[1]);
    assert!(!created_failed, "a call inside a batch runs: {created}");
    assert_eq!(answered[2]["id"], 3);
    assert_eq!(error_code(&answered[2]), -32601);
    assert_eq!(answered[3]["id"], Value::Null);
    assert_eq!(
        error_code(&answered[3]),
        -32600,
        "a batch element is an object"
    );

    let full = replies[1]
        .as_array()
        .expect("a full batch is answered with an array");
    assert_eq!(full.len(), BATCH_REQUESTS_MAX);
    for (reply, id) in full.iter().zip(100..) {
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"], json!({}));
    }

    assert!(
        replies[2].is_object(),
        "an oversized batch gets one error, not an array"
    );
    assert_eq!(error_code(&replies[2]), -32600);
    assert_eq!(replies[2]["id"], Value::Null);
    assert_eq!(replies[3]["id"], 9);
    assert_eq!(
        tracker.tickets(),
        vec![("Batched".to_owned(), AGENT.to_owned())],
        "nothing in a refused batch runs"
    );
}

#[test]
fn an_unknown_method_replies_method_not_found() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            rpc(1, "resources/list", json!({})),
            json!({"jsonrpc": "2.0", "method": "notifications/unknown"}).to_string(),
            call(2, "drop_tables", json!({})),
            rpc(3, "ping", json!({})),
        ],
    );
    assert_eq!(replies.len(), 3, "a notification is never answered");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(error_code(&replies[0]), -32601);
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(
        error_code(&replies[1]),
        -32602,
        "an unknown tool is invalid params"
    );
    assert_eq!(replies[2]["id"], 3);
    assert_eq!(replies[2]["result"], json!({}));
    assert!(tracker.tickets().is_empty());
}

#[test]
fn malformed_messages_get_json_rpc_errors_and_change_nothing() {
    let tracker = Tracker::new();
    let session = tracker.session(AGENT);
    let replies = exchange(
        &session,
        &[
            "{not json".to_owned(),
            "[]".to_owned(),
            String::new(),
            json!({"id": 1, "method": "ping"}).to_string(),
            json!({"jsonrpc": "2.0", "id": {"nested": 1}, "method": "ping"}).to_string(),
            rpc(
                2,
                "tools/call",
                json!({"name": "create", "arguments": "CA"}),
            ),
            rpc(3, "tools/call", json!({"arguments": {"project": "CA"}})),
            rpc(4, "tools/call", json!("create")),
            rpc(5, "ping", json!({})),
        ],
    );
    assert_eq!(replies.len(), 8, "the empty line is skipped");
    assert_eq!(error_code(&replies[0]), -32700);
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(error_code(&replies[1]), -32600);
    assert_eq!(replies[1]["id"], Value::Null);
    assert_eq!(error_code(&replies[2]), -32600, "jsonrpc 2.0 is required");
    assert_eq!(replies[2]["id"], 1);
    assert_eq!(
        error_code(&replies[3]),
        -32600,
        "an id is a string or a number"
    );
    assert_eq!(replies[3]["id"], Value::Null);
    for (reply, id) in replies[4..7].iter().zip(2..) {
        assert_eq!(reply["id"], id);
        assert_eq!(error_code(reply), -32602);
    }
    assert_eq!(replies[7]["id"], 5);
    assert_eq!(replies[7]["result"], json!({}));
    assert!(tracker.tickets().is_empty());
}

#[test]
fn a_locked_database_refuses_the_write_and_keeps_serving() {
    let tracker = Tracker::new();
    let session = tracker.impatient_session();
    let holder = Connection::open(&tracker.path).expect("a second writer opens");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("the second writer takes the write lock");

    let locked = exchange(
        &session,
        &[
            call(1, "create", json!({"project": "CA", "title": "Blocked"})),
            rpc(2, "ping", json!({})),
        ],
    );
    assert_eq!(locked.len(), 2);
    let (locked_failed, refused) = envelope(&locked[0]);
    assert!(
        locked_failed,
        "a write cannot start while another writer holds the lock"
    );
    assert_eq!(refused["code"], "store_failed");
    assert_eq!(locked[1]["result"], json!({}), "the server keeps serving");

    holder
        .execute_batch("ROLLBACK")
        .expect("the second writer lets go");
    let freed = exchange(
        &session,
        &[call(
            3,
            "create",
            json!({"project": "CA", "title": "Unblocked"}),
        )],
    );
    let (freed_failed, created) = envelope(&freed[0]);
    assert!(
        !freed_failed,
        "the same session writes once the lock is free: {created}"
    );
    assert_eq!(
        tracker.tickets(),
        vec![("Unblocked".to_owned(), AGENT.to_owned())]
    );
}
