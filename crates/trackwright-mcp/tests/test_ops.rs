use serde_json::{Value, json};
use std::io::Cursor;
use tempfile::TempDir;
use trackwright_core::catalog::{catalog, op_schema, schema_v1};
use trackwright_core::model::{Actor, OpKind, PolicyKey, ProjectKey};
use trackwright_core::policy;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, Store};
use trackwright_mcp::{Session, serve};

const OWNER: &str = "owner";
const AGENT: &str = "agent:7";
const ORACLE: &str = "expected 42, got 41";

const TEST_OPS: [&str; 22] = [
    "case_list",
    "case_show",
    "set_show",
    "plan_list",
    "plan_show",
    "run_list",
    "run_show",
    "coverage",
    "case_create",
    "case_edit",
    "case_link",
    "case_unlink",
    "set_upsert",
    "set_add",
    "set_remove",
    "plan_upsert",
    "plan_add",
    "plan_remove",
    "run_start",
    "run_record",
    "run_finish",
    "run_cancel",
];

const V3A_OPS: [&str; 30] = [
    "ready",
    "list",
    "show",
    "history",
    "roadmap",
    "policy_get",
    "export",
    "schema",
    "create",
    "edit",
    "move",
    "claim",
    "release",
    "depend",
    "undepend",
    "criterion_add",
    "criterion_edit",
    "check_add",
    "evidence_record",
    "criterion_set",
    "link",
    "unlink",
    "close",
    "reopen",
    "version_upsert",
    "cycle_start",
    "cycle_close",
    "import_preview",
    "import_apply",
    "policy_set",
];

fn session(folder: &TempDir) -> Session<Standalone> {
    let path = folder.path().join("tracker.db");
    let mut store = Store::open(&path).expect("the standalone store opens and migrates");
    let tx = store.transaction().expect("a write transaction");
    let owner = Actor::new(OWNER).expect("a valid actor");
    let key = ProjectKey::new("CA").expect("a valid key");
    store::create_project(&tx, &owner, &key, "CrazyAgent", 0).expect("the project creates");
    tx.commit().expect("the setup commits");
    Session::new(store.into_connection(), Standalone, owner)
}

fn call(id: u64, tool: &str, arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    })
    .to_string()
}

fn exchange(session: &Session<Standalone>, lines: &[String]) -> Vec<Value> {
    let mut input = lines.join("\n");
    input.push('\n');
    let mut output = Vec::new();
    serve(Cursor::new(input.into_bytes()), &mut output, session)
        .expect("serve ends cleanly when its input closes");
    let text = String::from_utf8(output).expect("the server writes UTF-8");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("each line is one JSON-RPC message"))
        .collect()
}

fn envelope(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("a tool result holds one text block");
    let envelope: Value = serde_json::from_str(text).expect("the text is the envelope");
    assert_eq!(envelope["api"], 1);
    envelope
}

fn ok(session: &Session<Standalone>, tool: &str, arguments: Value) -> Value {
    let replies = exchange(session, &[call(1, tool, arguments.clone())]);
    assert_eq!(replies.len(), 1);
    let envelope = envelope(&replies[0]);
    assert_eq!(
        envelope["ok"], true,
        "{tool} {arguments} replied {envelope}"
    );
    envelope["data"].clone()
}

fn replay_v3a_requests(session: &Session<Standalone>) {
    let created = ok(
        session,
        "create",
        json!({"project": "CA", "title": "First ticket", "request_id": "v3a-1"}),
    );
    assert_eq!(created["id"], "CA-1");
    assert_eq!(created["version"], 1);
    let again = ok(
        session,
        "create",
        json!({"project": "CA", "title": "First ticket", "request_id": "v3a-1"}),
    );
    assert_eq!(
        again, created,
        "a repeated request id replays the first reply"
    );
    let edited = ok(
        session,
        "edit",
        json!({"id": "CA-1", "version": 1, "body": "Why it matters."}),
    );
    assert_eq!(edited["version"], 2);
    let moved = ok(
        session,
        "move",
        json!({"id": "CA-1", "version": 2, "status": "todo"}),
    );
    assert_eq!(moved["status"], "todo");
    let listed = ok(session, "list", json!({"project": "CA"}));
    assert_eq!(listed["tickets"][0]["id"], "CA-1");
    let shown = ok(session, "show", json!({"id": "CA-1"}));
    assert_eq!(shown["ticket"]["title"], "First ticket");
    let schema = ok(session, "schema", json!({"op": "create"}));
    assert_eq!(schema["op"]["op"], "create");
}

#[test]
fn tools_list_includes_the_test_ops_and_schema_v1_stays_compatible() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let session = session(&folder);
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let replies = exchange(&session, &[list.to_string()]);
    let tools = replies[0]["result"]["tools"]
        .as_array()
        .expect("a tool list");
    assert_eq!(tools.len(), catalog().len());
    assert_eq!(tools.len(), V3A_OPS.len() + TEST_OPS.len());
    for (tool, spec) in tools.iter().zip(catalog()) {
        assert_eq!(tool["name"], spec.kind.as_str());
        assert_eq!(tool["inputSchema"], op_schema(spec)["args"]);
    }
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool name"))
        .collect();
    assert_eq!(&names[..V3A_OPS.len()], V3A_OPS, "V3a ops keep their order");
    assert_eq!(
        &names[V3A_OPS.len()..],
        TEST_OPS,
        "the test ops follow them"
    );

    let schema = schema_v1();
    assert_eq!(schema["api"], 1, "the test ops add to API version 1");
    for op in TEST_OPS {
        let kind = OpKind::parse(op).expect("every test op is an OpKind");
        let args = &schema["ops"][op]["args"];
        assert_eq!(args["additionalProperties"], false, "{op}");
        assert_eq!(
            args["properties"].get("request_id").is_some(),
            kind.is_write(),
            "{op}: request_id on writes only"
        );
    }
    replay_v3a_requests(&session);

    let case = ok(
        &session,
        "case_create",
        json!({"project": "CA", "title": "Adds two numbers", "kind": "unit"}),
    );
    assert_eq!(case["case"]["id"], "CA-T1");
    let shown = ok(&session, "case_show", json!({"id": "CA-T1"}));
    assert_eq!(shown["case"]["title"], "Adds two numbers");
    let listed = ok(&session, "case_list", json!({"project": "CA"}));
    assert_eq!(listed["cases"][0]["id"], "CA-T1");
}

fn session_as(folder: &TempDir, actor: &str) -> Session<Standalone> {
    let path = folder.path().join("tracker.db");
    let store = Store::open(&path).expect("the standalone store reopens");
    let actor = Actor::new(actor).expect("a valid actor");
    Session::new(store.into_connection(), Standalone, actor)
}

fn show_protected_cases_fully(folder: &TempDir) {
    let mut store = Store::open(&folder.path().join("tracker.db")).expect("the store reopens");
    let tx = store.transaction().expect("a write transaction");
    let owner = Actor::new(OWNER).expect("a valid actor");
    let key = ProjectKey::new("CA").expect("a valid key");
    let setting = PolicyKey::CaseProtectedVisibility;
    policy::set(&tx, &Standalone, &owner, &key, setting, "full", 0, 1).expect("the owner sets it");
    tx.commit().expect("the policy commits");
}

fn digest(digit: char) -> String {
    std::iter::repeat_n(digit, 64).collect()
}

fn seed_protected_run(owner: &Session<Standalone>) {
    let case = ok(
        owner,
        "case_create",
        json!({
            "project": "CA",
            "title": "Holdout sum",
            "kind": "unit",
            "stable_name": "oracle::sum",
            "protected": true,
        }),
    );
    assert_eq!(case["case"]["id"], "CA-T1");
    let policy = json!({
        "profile": digest('d'),
        "profile_approved_ms": 0,
        "check": "holdout",
        "kind": "unit",
        "argv": [],
        "toolchain": {"family": "none", "version": ""},
        "sandbox": digest('e'),
        "network": "off",
        "timeout_s": 0,
        "environment": digest('f'),
    });
    let started = ok(
        owner,
        "run_start",
        json!({
            "project": "CA",
            "title": "holdouts",
            "source": {"kind": "declared", "ref": "ci"},
            "policy": policy,
        }),
    );
    let result = json!({
        "name": "oracle::sum",
        "outcome": "failed",
        "message": ORACLE,
        "artifact": "logs/oracle-sum.txt",
    });
    ok(
        owner,
        "run_record",
        json!({"id": "CA-R1", "version": started["run"]["version"], "results": [result]}),
    );
}

fn first_result(session: &Session<Standalone>) -> Value {
    let shown = ok(session, "run_show", json!({"id": "CA-R1"}));
    assert_eq!(
        shown["results"].as_array().map(Vec::len),
        Some(1),
        "{shown}"
    );
    shown["results"][0].clone()
}

#[test]
fn a_protected_case_result_message_is_withheld_from_an_agent_in_run_show() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let owner = session(&folder);
    seed_protected_run(&owner);

    let seen = first_result(&owner);
    assert_eq!(
        seen["case"], "CA-T1",
        "the result maps to the protected case"
    );
    assert_eq!(seen["message"], ORACLE, "the owner reads the message");
    assert_eq!(seen["artifact"], "logs/oracle-sum.txt");
    assert!(seen.get("withheld").is_none(), "{seen}");

    let agent = session_as(&folder, AGENT);
    let narrowed = first_result(&agent);
    assert_eq!(narrowed["name"], "oracle::sum");
    assert_eq!(narrowed["outcome"], "failed");
    assert_eq!(narrowed["case"], "CA-T1");
    assert_eq!(
        narrowed["message"],
        Value::Null,
        "hidden withholds the message"
    );
    assert_eq!(
        narrowed["artifact"],
        Value::Null,
        "hidden withholds the artifact"
    );
    assert_eq!(narrowed["withheld"], true);

    show_protected_cases_fully(&folder);
    let full = first_result(&session_as(&folder, AGENT));
    assert_eq!(
        full["message"], ORACLE,
        "full shows protected results to agents"
    );
    assert!(full.get("withheld").is_none(), "{full}");
}
