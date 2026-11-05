use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use trackwright_core::catalog::{self, FieldType, Shape};
use trackwright_core::host::{
    Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links, Paths, TicketView,
};
use trackwright_core::model::{
    ACTOR_BYTES_MAX, Actor, CRITERIA_PER_TICKET_MAX, HOST_CODE_BYTES_MAX,
    OPEN_TICKETS_CREATED_PER_ACTOR_MAX, OpKind, PolicyKey, Priority, ProjectKey, REQUEST_BYTES_MAX,
    Refusal, ShortId, TicketKind, TicketStatus, WRITES_PER_ACTOR_MINUTE_MAX, codes,
};
use trackwright_core::ops;
use trackwright_core::policy;
use trackwright_core::reply::{
    LISTED_TITLE_BYTES_MAX, NEXT_MAX, PAGE_ROWS_MAX, REPLY_BYTES_DEFAULT, REPLY_BYTES_MAX, Reply,
};
use trackwright_core::requests::Request;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, NewTicket, Store, TicketChange};
use trackwright_core::text::{BODY_BYTES_MAX, CRITERION_BYTES_MAX, TITLE_BYTES_MAX};

const HOST_TABLES: &str = "
CREATE TABLE host_grants(actor TEXT NOT NULL, op TEXT NOT NULL, PRIMARY KEY (actor, op));
CREATE TABLE host_targets(kind TEXT NOT NULL, target TEXT NOT NULL, state TEXT NOT NULL,
    PRIMARY KEY (kind, target));
CREATE TABLE host_results(kind TEXT NOT NULL, ref TEXT NOT NULL, result TEXT NOT NULL,
    PRIMARY KEY (kind, ref));";

const KINDS: &[LinkKind] = &[
    LinkKind {
        name: "branch",
        label: "Branch",
    },
    LinkKind {
        name: "note",
        label: "Note",
    },
];

/// A host whose links, evidence and grants live in its own tables of the same database, read
/// in the core's transaction: `host_targets`, `host_results` and `host_grants` (`*` grants
/// every op). File paths follow the standalone host.
struct TableHost;

impl Links for TableHost {
    fn kinds(&self) -> &[LinkKind] {
        KINDS
    }

    fn resolve(
        &self,
        tx: &Transaction<'_>,
        _project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM host_targets WHERE kind = ?1 AND target = ?2",
                params![kind, target],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        match state {
            Some(state) => Ok(LinkState {
                live: true,
                label: format!("{kind} {target}"),
                state,
            }),
            None => Err(Refusal::host("host_target_unknown", "no such target")),
        }
    }
}

impl Evidence for TableHost {
    fn check(
        &self,
        tx: &Transaction<'_>,
        _project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        let result: Option<String> = tx
            .query_row(
                "SELECT result FROM host_results WHERE kind = ?1 AND ref = ?2",
                params![check.kind.as_str(), check.reference],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        Ok(EvidenceState {
            satisfied: result.as_deref() == Some("pass"),
            detail: result.unwrap_or_else(|| "no result".to_owned()),
        })
    }
}

impl Authority for TableHost {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        _project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        _ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        let granted: i64 = tx
            .query_row(
                "SELECT count(*) FROM host_grants WHERE actor = ?1 AND op IN (?2, '*')",
                params![actor.as_str(), op.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        if granted > 0 {
            Ok(())
        } else {
            Err(Refusal::host(
                "host_not_allowed",
                "this actor may not run it",
            ))
        }
    }

    fn actors_verified(&self) -> bool {
        true
    }

    fn evidence_may(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _actor: &Actor,
        _check: &CheckView,
    ) -> Result<(), Refusal> {
        Ok(())
    }
}

impl Paths for TableHost {
    fn resolve_path(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal> {
        Standalone.resolve_path(tx, project, actor, op, requested)
    }
}

fn db_path(dir: &Path) -> PathBuf {
    dir.join("tracker").join("tracker.db")
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn id(text: &str) -> ShortId {
    ShortId::parse(text).unwrap()
}

fn grant(tx: &Transaction<'_>, who: &str, ops: &[&str]) {
    for op in ops {
        tx.execute(
            "INSERT INTO host_grants(actor, op) VALUES (?1, ?2)",
            params![who, op],
        )
        .unwrap();
    }
}

/// A fresh database with the host tables, the owner granted every op and project CA.
fn setup(dir: &Path) -> Store {
    let mut store = Store::open(&db_path(dir)).unwrap();
    let tx = store.transaction().unwrap();
    tx.execute_batch(HOST_TABLES).unwrap();
    grant(&tx, "owner", &["*"]);
    store::create_project(&tx, &actor("owner"), &key(), "CrazyAgent", 1_000).unwrap();
    tx.commit().unwrap();
    store
}

fn raw(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Reply {
    raw_bounded(tx, who, op, args, REPLY_BYTES_DEFAULT)
}

fn raw_bounded(tx: &Transaction<'_>, who: &str, op: &str, args: Value, bound: usize) -> Reply {
    let bytes = serde_json::to_vec(&args).unwrap();
    let request = Request::parse(op, &bytes)
        .unwrap_or_else(|refusal| panic!("{op} {args} parses: {refusal}"))
        .with_reply_bytes_max(bound);
    let reply = ops::dispatch(tx, &TableHost, &actor(who), request);
    assert!(
        reply.json().len() <= bound,
        "{op} replied {} bytes over its bound {bound}",
        reply.json().len()
    );
    reply
}

fn call(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let reply = raw(tx, who, op, args);
    let value: Value = serde_json::from_str(reply.json()).unwrap();
    assert_eq!(value["api"], 1);
    assert_eq!(value["ok"].as_bool(), Some(reply.is_ok()));
    value
}

fn ok(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let value = call(tx, who, op, args);
    assert_eq!(value["ok"], true, "{op} succeeds: {value}");
    value
}

fn refused(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let value = call(tx, who, op, args);
    assert_eq!(value["ok"], false, "{op} is refused: {value}");
    assert!(value["code"].as_str().is_some_and(|code| !code.is_empty()));
    assert!(
        value["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    value
}

fn count(tx: &Transaction<'_>, sql: &str) -> i64 {
    tx.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn events_of(tx: &Transaction<'_>, op: &str) -> i64 {
    tx.query_row(
        "SELECT count(*) FROM tk_events WHERE op = ?1",
        [op],
        |row| row.get(0),
    )
    .unwrap()
}

fn next_ops(reply: &Value) -> Vec<String> {
    reply["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|next| next["op"].as_str().unwrap().to_owned())
        .collect()
}

fn new_ticket(title: String, body: String) -> NewTicket {
    NewTicket {
        title,
        body,
        kind: TicketKind::Feature,
        status: TicketStatus::Todo,
        priority: Priority::new(2).unwrap(),
    }
}

#[test]
fn a_stale_write_replies_stale_version_with_next_show_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let created = ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "First"}),
    );
    assert_eq!(created["data"]["id"], "CA-1");
    assert_eq!(created["data"]["version"], 1);
    let edited = ok(
        &tx,
        "owner",
        "edit",
        json!({"id": "CA-1", "version": 1, "title": "Second"}),
    );
    assert_eq!(edited["data"]["version"], 2);
    let events_before = count(&tx, "SELECT count(*) FROM tk_events");

    for (op, args) in [
        (
            "edit",
            json!({"id": "CA-1", "version": 1, "title": "Third"}),
        ),
        (
            "move",
            json!({"id": "CA-1", "version": 1, "status": "in_progress"}),
        ),
    ] {
        let stale = refused(&tx, "owner", op, args);
        assert_eq!(stale["code"], codes::STALE_VERSION, "{op}");
        assert_eq!(stale["data"]["id"], "CA-1");
        assert_eq!(
            stale["data"]["version"], 2,
            "the reply names the current version"
        );
        let next = stale["next"].as_array().unwrap();
        assert_eq!(next[0]["op"], "show");
        assert_eq!(next[0]["id"], "CA-1");
    }

    let stored = store::ticket(&tx, &id("CA-1")).unwrap();
    assert_eq!(stored.title, "Second");
    assert_eq!(json!(stored.status.as_str()), created["data"]["status"]);
    assert_eq!(stored.version, 2);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events_before);
}

#[test]
fn a_repeated_request_id_returns_the_first_reply_and_writes_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let first = {
        let tx = store.transaction().unwrap();
        let reply = raw(
            &tx,
            "owner",
            "create",
            json!({"project": "CA", "title": "Once", "priority": 1, "request_id": "req-1"}),
        );
        tx.commit().unwrap();
        reply
    };
    assert!(first.is_ok());
    let tx = store.transaction().unwrap();
    let again = raw(
        &tx,
        "owner",
        "create",
        json!({"request_id": "req-1", "priority": 1, "title": "Once", "project": "CA"}),
    );
    assert_eq!(
        again.json(),
        first.json(),
        "the repeat returns the first reply byte for byte"
    );
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), 1);
    assert_eq!(events_of(&tx, "ticket.create"), 1);

    let edit = json!({"id": "CA-1", "version": 1, "title": "Twice", "request_id": "req-2"});
    let edited = raw(&tx, "owner", "edit", edit.clone());
    let replayed = raw(&tx, "owner", "edit", edit);
    assert!(edited.is_ok());
    assert_eq!(
        replayed.json(),
        edited.json(),
        "a replayed versioned write is not stale: it is the same write"
    );
    assert_eq!(store::ticket(&tx, &id("CA-1")).unwrap().version, 2);
    assert_eq!(events_of(&tx, "ticket.update"), 1);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_requests"), 2);
}

#[test]
fn a_reused_request_id_with_another_payload_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    grant(&tx, "agent:w2", &["*"]);
    ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "A", "request_id": "req-1"}),
    );
    let events_before = count(&tx, "SELECT count(*) FROM tk_events");

    let reused = refused(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "B", "request_id": "req-1"}),
    );
    assert_eq!(reused["code"], codes::REQUEST_ID_REUSED);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), 1);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events_before);

    let other = ok(
        &tx,
        "agent:w2",
        "create",
        json!({"project": "CA", "title": "B", "request_id": "req-1"}),
    );
    assert_eq!(
        other["data"]["id"], "CA-2",
        "request ids belong to one actor"
    );

    let stale = refused(
        &tx,
        "owner",
        "edit",
        json!({"id": "CA-1", "version": 7, "title": "C", "request_id": "req-3"}),
    );
    assert_eq!(stale["code"], codes::STALE_VERSION);
    ok(
        &tx,
        "owner",
        "edit",
        json!({"id": "CA-1", "version": 1, "title": "C", "request_id": "req-3"}),
    );
    assert_eq!(store::ticket(&tx, &id("CA-1")).unwrap().title, "C");
}

#[test]
fn the_generated_schema_matches_schema_v1_json() {
    let file: Value = serde_json::from_str(include_str!("../schema/v1.json")).unwrap();
    let generated = catalog::schema_v1();
    assert_eq!(
        generated, file,
        "schema/v1.json is regenerated, never hand-edited"
    );
    assert_eq!(generated["api"], 1);

    let listed: Vec<OpKind> = catalog::catalog().iter().map(|spec| spec.kind).collect();
    assert_eq!(
        listed,
        OpKind::ALL,
        "the catalog holds every op once, in order"
    );
    let ops = generated["ops"].as_object().unwrap();
    assert_eq!(ops.len(), OpKind::ALL.len());
    for kind in OpKind::ALL {
        let op = &ops[kind.as_str()];
        assert_eq!(op["write"], kind.is_write(), "{kind}");
        assert_eq!(op["args"]["additionalProperties"], false, "{kind}");
        let has_request_id = op["args"]["properties"].get("request_id").is_some();
        assert_eq!(
            has_request_id,
            kind.is_write(),
            "{kind}: request_id on writes only"
        );
    }
}

#[test]
fn codes_all_lists_every_core_code_once() {
    let model = include_str!("../src/model.rs");
    let start = model.find("pub mod codes {").unwrap();
    let block = &model[start..start + model[start..].find("\n}").unwrap()];
    let declared: BTreeSet<&str> = block
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub const "))
        .filter_map(|line| line.split_once(": &str = \""))
        .map(|(_, value)| value.trim_end_matches("\";"))
        .collect();
    let listed: BTreeSet<&str> = codes::ALL.iter().copied().collect();
    assert_eq!(listed.len(), codes::ALL.len(), "codes::ALL has no repeats");
    assert_eq!(listed, declared, "codes::ALL lists every declared code");
}

#[test]
fn schema_lists_every_op_across_pages_for_any_actor() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let mut listed = Vec::new();
    let mut args = json!({});
    for _ in 0..OpKind::ALL.len() {
        let page = ok(&tx, "agent:nobody", "schema", args.clone());
        assert_eq!(page["data"]["api"], 1);
        for entry in page["data"]["ops"].as_array().unwrap() {
            let kind = OpKind::parse(entry["op"].as_str().unwrap()).unwrap();
            assert_eq!(entry["write"], kind.is_write(), "{kind}");
            assert!(
                entry["summary"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            listed.push(kind);
        }
        match page["cursor"].as_str() {
            Some(cursor) => args = json!({"cursor": cursor}),
            None => break,
        }
    }
    assert_eq!(listed, OpKind::ALL, "every op once, in order");

    let create = ok(&tx, "agent:nobody", "schema", json!({"op": "create"}));
    assert_eq!(create["data"]["op"]["op"], "create");
    assert_eq!(
        create["data"]["op"]["args"],
        catalog::schema_v1()["ops"]["create"]["args"]
    );
    for cursor in ["ops:999", "tickets:1", "ops:x"] {
        let wrong = refused(&tx, "agent:nobody", "schema", json!({"cursor": cursor}));
        assert_eq!(wrong["code"], codes::CURSOR_INVALID, "{cursor}");
    }
}

fn sample(ty: FieldType) -> Value {
    match ty {
        FieldType::Project => json!("CA"),
        FieldType::Ticket => json!("CA-1"),
        FieldType::Test(object) => json!(format!("CA-{}1", object.letter())),
        FieldType::Line { .. } => json!("x"),
        FieldType::Shape(shape) => sample_shape(shape),
        FieldType::Actor => json!("agent:w1"),
        FieldType::Text(_) => json!("x"),
        FieldType::Version => json!(1),
        FieldType::Integer { min, .. } => json!(min),
        FieldType::Choice(named) => json!(named.names()[0]),
        FieldType::Day => json!("2026-11-02"),
        FieldType::Flag => json!(true),
        FieldType::Cursor => json!("a1"),
        FieldType::RequestId => json!("req-1"),
        FieldType::Word => json!("evidence"),
        FieldType::Path => json!("roadmap.md"),
        FieldType::Keys => json!(["item-1"]),
    }
}

fn sample_shape(shape: Shape) -> Value {
    match shape {
        Shape::Steps => json!(["x"]),
        Shape::Results => json!([{"name": "t", "outcome": "passed"}]),
        Shape::Source => json!({"kind": "declared", "ref": "local"}),
        Shape::Policy => json!({
            "profile": "d".repeat(64),
            "profile_approved_ms": 0,
            "check": "unit",
            "kind": "unit",
            "argv": [],
            "toolchain": {"family": "none", "version": ""},
            "sandbox": "e".repeat(64),
            "network": "off",
            "timeout_s": 0,
            "environment": "f".repeat(64),
        }),
        Shape::Integrity => json!({
            "executed": 0,
            "inventory": {"baseline_run": null, "removed": [], "removed_total": 0},
            "new_skips": [],
            "protected_touched": [],
            "holdouts_ran": false,
            "build_ok": true,
        }),
    }
}

fn parse_code(op: &str, args: &[u8]) -> &'static str {
    match Request::parse(op, args) {
        Ok(_) => panic!("{op} accepted {}", String::from_utf8_lossy(args)),
        Err(refusal) => refusal.code,
    }
}

#[test]
fn every_op_refuses_an_unknown_field_and_an_oversized_request() {
    assert_eq!(parse_code("teleport", b"{}"), codes::UNKNOWN_OP);
    assert_eq!(parse_code("show", b"[]"), codes::ARGS_INVALID);
    assert_eq!(parse_code("show", b"{\"id\":"), codes::ARGS_INVALID);
    for spec in catalog::catalog() {
        let op = spec.kind.as_str();
        let mut full = serde_json::Map::new();
        for field in spec.fields {
            full.insert(field.name.to_owned(), sample(field.ty));
        }
        let bytes = serde_json::to_vec(&full).unwrap();
        if let Err(refusal) = Request::parse(op, &bytes) {
            panic!("{op} accepts every catalog field: {refusal}");
        }

        let mut unknown = full.clone();
        unknown.insert("zz_unknown".to_owned(), json!(true));
        assert_eq!(
            parse_code(op, &serde_json::to_vec(&unknown).unwrap()),
            codes::UNKNOWN_FIELD,
            "{op}"
        );

        for field in spec.fields.iter().filter(|field| field.required) {
            let mut missing = full.clone();
            missing.remove(field.name);
            assert_eq!(
                parse_code(op, &serde_json::to_vec(&missing).unwrap()),
                codes::MISSING_FIELD,
                "{op} without {}",
                field.name
            );
        }

        let mut padded = bytes.clone();
        padded.resize(REQUEST_BYTES_MAX, b' ');
        assert!(
            Request::parse(op, &padded).is_ok(),
            "{op} accepts a request of exactly {REQUEST_BYTES_MAX} bytes"
        );
        padded.push(b' ');
        assert_eq!(parse_code(op, &padded), codes::REQUEST_TOO_LARGE, "{op}");
    }

    let long_title = "t".repeat(TITLE_BYTES_MAX + 1);
    let args = serde_json::to_vec(&json!({"project": "CA", "title": long_title})).unwrap();
    assert_eq!(parse_code("create", &args), codes::TEXT_TOO_LONG);
    let hidden = serde_json::to_vec(&json!({"project": "CA", "title": "a\u{202e}b"})).unwrap();
    assert_eq!(parse_code("create", &hidden), codes::TEXT_HIDDEN_CHARACTERS);
    let typed = serde_json::to_vec(&json!({"id": "CA-1", "version": "1"})).unwrap();
    assert_eq!(parse_code("edit", &typed), codes::FIELD_INVALID);
    let id_text = serde_json::to_vec(&json!({"id": "ca-1"})).unwrap();
    assert_eq!(parse_code("show", &id_text), codes::FIELD_INVALID);
}

#[test]
fn next_lists_only_ops_the_actor_may_run_and_at_most_five() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    grant(&tx, "agent:w1", &["show", "edit", "link"]);
    let created = ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "Plan", "status": "todo"}),
    );
    let owner_next = next_ops(&created);
    assert!(owner_next.contains(&"claim".to_owned()), "{owner_next:?}");
    assert!(
        owner_next.contains(&"criterion_add".to_owned()),
        "{owner_next:?}"
    );
    assert!(owner_next.len() <= NEXT_MAX);

    let shown = ok(&tx, "agent:w1", "show", json!({"id": "CA-1"}));
    let agent_next = next_ops(&shown);
    assert!(agent_next.contains(&"edit".to_owned()), "{agent_next:?}");
    for op in &agent_next {
        assert!(
            ["show", "edit", "link"].contains(&op.as_str()),
            "the agent may not run {op}"
        );
    }
    for next in shown["next"].as_array().unwrap() {
        assert_eq!(next["id"], "CA-1");
        assert!(next["why"].as_str().is_some_and(|why| !why.is_empty()));
    }

    store::add_criterion(&tx, &actor("owner"), &id("CA-1"), "plans in tickets", 2_000).unwrap();
    let busy = ok(&tx, "owner", "show", json!({"id": "CA-1"}));
    assert_eq!(
        next_ops(&busy).len(),
        NEXT_MAX,
        "an unclaimed todo with an open criterion has more than five candidates"
    );
    let ops: BTreeSet<String> = next_ops(&busy).into_iter().collect();
    assert_eq!(ops.len(), NEXT_MAX, "no op is listed twice");

    let stranger = refused(&tx, "agent:w9", "show", json!({"id": "CA-1"}));
    assert_eq!(stranger["code"], "host_not_allowed");
    assert!(stranger["next"].as_array().unwrap().is_empty());
}

#[test]
fn replies_stay_under_the_bound_and_page_with_a_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let owner = actor("owner");
    let title = "\u{e9}".repeat(TITLE_BYTES_MAX / 2);
    let assignee = actor(&format!("agent:{}", "a".repeat(ACTOR_BYTES_MAX - 6)));
    for n in 0..60 {
        let created = store::create_ticket(
            &tx,
            &owner,
            &key(),
            &new_ticket(title.clone(), String::new()),
            n,
        )
        .unwrap();
        let change = TicketChange {
            assignee: Some(Some(assignee.clone())),
            ..TicketChange::default()
        };
        store::update_ticket(&tx, &owner, &created.short_id, created.version, &change, n).unwrap();
    }

    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let mut args = json!({"project": "CA"});
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let page = ok(&tx, "owner", "list", args);
        let rows = page["data"]["tickets"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() <= PAGE_ROWS_MAX);
        for row in rows {
            let listed = row["title"].as_str().unwrap();
            assert!(listed.len() <= LISTED_TITLE_BYTES_MAX);
            assert!(title.starts_with(listed));
            assert_eq!(row["elided"], true);
            assert_eq!(row["status"], "todo");
            assert_eq!(row["priority"], 2);
            assert_eq!(row["assignee"], assignee.as_str());
            seen.push(row["id"].as_str().unwrap().to_owned());
        }
        pages += 1;
        cursor = page["cursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(pages < 60, "paging ends");
    }
    let expected: Vec<String> = (1..=60).map(|n| format!("CA-{n}")).collect();
    assert_eq!(seen, expected, "every ticket once, in order");
    assert!(
        pages >= 3,
        "8 KiB holds fewer than 30 rows with long assignees"
    );

    let wide = raw_bounded(
        &tx,
        "owner",
        "list",
        json!({"project": "CA"}),
        REPLY_BYTES_MAX,
    );
    let wide: Value = serde_json::from_str(wide.json()).unwrap();
    assert_eq!(
        wide["data"]["tickets"].as_array().unwrap().len(),
        PAGE_ROWS_MAX
    );
    assert!(!wide["cursor"].is_null());

    let body: String = "\u{20ac}\n".repeat(BODY_BYTES_MAX / 4);
    assert!(body.len() <= BODY_BYTES_MAX);
    let big = store::create_ticket(
        &tx,
        &owner,
        &key(),
        &new_ticket("Big".into(), body.clone()),
        99,
    )
    .unwrap();
    let criterion = "c".repeat(CRITERION_BYTES_MAX);
    for _ in 0..CRITERIA_PER_TICKET_MAX {
        store::add_criterion(&tx, &owner, &big.short_id, &criterion, 100).unwrap();
    }
    for n in 0..40 {
        let target = format!("feat/branch-{n:02}-{}", "b".repeat(200));
        tx.execute(
            "INSERT INTO host_targets(kind, target, state) VALUES ('branch', ?1, 'open')",
            [&target],
        )
        .unwrap();
        store::add_link(&tx, &owner, &big.short_id, "branch", &target, 101).unwrap();
    }

    let mut assembled = String::new();
    let mut positions = Vec::new();
    let mut targets = BTreeSet::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let mut args = json!({"id": big.short_id.to_string()});
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let page = ok(&tx, "owner", "show", args);
        let data = &page["data"];
        assert_eq!(data["ticket"]["id"], big.short_id.to_string());
        assert_eq!(data["ticket"]["body_bytes"], body.len());
        if let Some(part) = data["body"].as_str() {
            assert_eq!(
                data["body_at"],
                assembled.len(),
                "body parts arrive in order"
            );
            assembled.push_str(part);
        }
        for criterion in data["criteria"].as_array().into_iter().flatten() {
            positions.push(criterion["position"].as_u64().unwrap());
        }
        for link in data["links"].as_array().into_iter().flatten() {
            assert_eq!(link["live"], true);
            assert_eq!(link["state"], "open");
            assert!(targets.insert(link["target"].as_str().unwrap().to_owned()));
        }
        pages += 1;
        cursor = page["cursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(pages < 100, "paging ends");
    }
    assert_eq!(
        assembled, body,
        "the body reassembles exactly, split on characters"
    );
    let expected: Vec<u64> = (1..=CRITERIA_PER_TICKET_MAX as u64).collect();
    assert_eq!(positions, expected);
    assert_eq!(targets.len(), 40);
}

#[test]
fn two_connections_editing_one_version_leave_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = setup(dir.path());
        let tx = store.transaction().unwrap();
        ok(
            &tx,
            "owner",
            "create",
            json!({"project": "CA", "title": "Race"}),
        );
        tx.commit().unwrap();
    }
    let path = db_path(dir.path());
    let barrier = Arc::new(Barrier::new(2));
    let racers: Vec<_> = (0..2)
        .map(|n| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                let tx = store.transaction().unwrap();
                let reply = call(
                    &tx,
                    "owner",
                    "edit",
                    json!({"id": "CA-1", "version": 1, "title": format!("winner {n}")}),
                );
                tx.commit().unwrap();
                reply
            })
        })
        .collect();
    let replies: Vec<Value> = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect();

    let winners: Vec<&Value> = replies.iter().filter(|reply| reply["ok"] == true).collect();
    let losers: Vec<&Value> = replies
        .iter()
        .filter(|reply| reply["ok"] == false)
        .collect();
    assert_eq!(winners.len(), 1, "{replies:?}");
    assert_eq!(losers.len(), 1, "{replies:?}");
    assert_eq!(losers[0]["code"], codes::STALE_VERSION);
    assert_eq!(losers[0]["data"]["version"], 2);

    let mut store = Store::open(&path).unwrap();
    let tx = store.transaction().unwrap();
    let stored = store::ticket(&tx, &id("CA-1")).unwrap();
    assert_eq!(stored.version, 2);
    assert_eq!(json!(stored.title), winners[0]["data"]["title"]);
    assert_eq!(events_of(&tx, "ticket.update"), 1);
}

#[test]
fn an_actor_past_its_write_rate_or_open_ticket_bound_is_refused_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    grant(&tx, "agent:fast", &["*"]);
    grant(&tx, "agent:many", &["*"]);
    for n in 0..WRITES_PER_ACTOR_MINUTE_MAX {
        ok(
            &tx,
            "agent:fast",
            "create",
            json!({"project": "CA", "title": format!("fast {n}")}),
        );
    }
    let tickets = count(&tx, "SELECT count(*) FROM tk_tickets");
    let limited = refused(
        &tx,
        "agent:fast",
        "create",
        json!({"project": "CA", "title": "one too many"}),
    );
    assert_eq!(limited["code"], codes::RATE_LIMITED);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), tickets);
    ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "owner unaffected"}),
    );

    let many = actor("agent:many");
    let mut last = None;
    for n in 0..OPEN_TICKETS_CREATED_PER_ACTOR_MAX {
        let title = format!("open {n}");
        last = Some(
            store::create_ticket(&tx, &many, &key(), &new_ticket(title, String::new()), 5).unwrap(),
        );
    }
    let full = refused(
        &tx,
        "agent:many",
        "create",
        json!({"project": "CA", "title": "51st"}),
    );
    assert_eq!(full["code"], codes::OPEN_TICKETS_FULL);
    let last = last.unwrap();
    store::update_ticket(
        &tx,
        &many,
        &last.short_id,
        last.version,
        &TicketChange {
            status: Some(TicketStatus::Done),
            ..TicketChange::default()
        },
        6,
    )
    .unwrap();
    ok(
        &tx,
        "agent:many",
        "create",
        json!({"project": "CA", "title": "after a close"}),
    );
}

#[test]
fn move_keeps_to_open_statuses_and_names_close_for_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "Move me"}),
    );
    let moved = ok(
        &tx,
        "owner",
        "move",
        json!({"id": "CA-1", "version": 1, "status": "in_progress"}),
    );
    assert_eq!(moved["data"]["status"], "in_progress");
    assert_eq!(moved["data"]["version"], 2);
    for status in ["done", "cancelled"] {
        let closing = refused(
            &tx,
            "owner",
            "move",
            json!({"id": "CA-1", "version": 2, "status": status}),
        );
        assert_eq!(closing["code"], codes::USE_CLOSE);
        assert_eq!(next_ops(&closing), ["close"]);
    }
    assert_eq!(
        store::ticket(&tx, &id("CA-1")).unwrap().status,
        TicketStatus::InProgress
    );

    let closed = store::update_ticket(
        &tx,
        &actor("owner"),
        &id("CA-1"),
        2,
        &TicketChange {
            status: Some(TicketStatus::Done),
            ..TicketChange::default()
        },
        5_000,
    )
    .unwrap();
    assert_eq!(closed.version, 3);
    let events = count(&tx, "SELECT count(*) FROM tk_events");
    for status in ["in_progress", "todo", "done"] {
        let reopening = refused(
            &tx,
            "owner",
            "move",
            json!({"id": "CA-1", "version": 3, "status": status}),
        );
        assert_eq!(reopening["code"], codes::TICKET_CLOSED, "{status}");
        assert_eq!(next_ops(&reopening), ["reopen"], "{status}");
    }
    let stored = store::ticket(&tx, &id("CA-1")).unwrap();
    assert_eq!(stored.status, TicketStatus::Done);
    assert_eq!(stored.closed_by, Some(actor("owner")));
    assert_eq!(stored.closed_ms, Some(5_000));
    assert_eq!(stored.version, 3);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events);
}

#[test]
fn create_starts_where_the_create_policy_says() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    grant(&tx, "agent:w1", &["create"]);
    let status_of = |who: &str, args: Value| -> Value {
        ok(&tx, who, "create", args)["data"]["status"].clone()
    };
    let todo = json!({"project": "CA", "title": "Asked for todo", "status": "todo"});
    let plain = json!({"project": "CA", "title": "No status"});

    assert_eq!(status_of("agent:w1", todo.clone()), "triage");
    assert_eq!(status_of("agent:w1", plain.clone()), "triage");
    assert_eq!(status_of("owner", todo.clone()), "todo");
    assert_eq!(status_of("owner", plain.clone()), "backlog");

    policy::set(
        &tx,
        &TableHost,
        &actor("owner"),
        &key(),
        PolicyKey::Create,
        "direct",
        0,
        3_000,
    )
    .unwrap();
    assert_eq!(status_of("agent:w1", todo.clone()), "todo");
    assert_eq!(status_of("agent:w1", plain), "backlog");
    assert_eq!(status_of("owner", todo), "todo");

    let tickets = count(&tx, "SELECT count(*) FROM tk_tickets");
    for status in ["done", "cancelled"] {
        let closed = refused(
            &tx,
            "agent:w1",
            "create",
            json!({"project": "CA", "title": "Closed at birth", "status": status}),
        );
        assert_eq!(closed["code"], codes::STATUS_CLOSED_AT_CREATE, "{status}");
    }
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), tickets);
}

#[test]
fn a_repeat_whose_first_reply_is_over_its_bound_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let args = json!({"project": "CA", "title": "Big reply", "request_id": "big-1"});
    let first = raw_bounded(&tx, "owner", "create", args.clone(), REPLY_BYTES_MAX);
    assert!(first.is_ok());

    let padded = format!(
        "{{\"api\":1,\"ok\":true,\"data\":{{\"pad\":\"{}\"}},\"next\":[],\"cursor\":null}}",
        "x".repeat(REPLY_BYTES_DEFAULT + 1_000)
    );
    assert!(padded.len() > REPLY_BYTES_DEFAULT);
    assert!(padded.len() < REPLY_BYTES_MAX);
    let stored = tx
        .execute(
            "UPDATE tk_requests SET reply_json = ?1 WHERE actor = 'owner' AND request_id = 'big-1'",
            [&padded],
        )
        .unwrap();
    assert_eq!(stored, 1);

    let small = raw(&tx, "owner", "create", args.clone());
    let small: Value = serde_json::from_str(small.json()).unwrap();
    assert_eq!(small["ok"], false);
    assert_eq!(small["code"], codes::REQUEST_ID_REUSED);
    assert!(small["message"].as_str().unwrap().contains("reply bound"));

    let large = raw_bounded(&tx, "owner", "create", args, REPLY_BYTES_MAX);
    assert!(large.is_ok());
    assert_eq!(large.json(), padded, "the first reply, byte for byte");
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), 1);
}

#[test]
fn link_and_unlink_go_through_the_host_and_a_refused_link_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    ok(
        &tx,
        "owner",
        "create",
        json!({"project": "CA", "title": "Linked"}),
    );
    tx.execute(
        "INSERT INTO host_targets(kind, target, state) VALUES ('branch', 'feat/x', 'open')",
        [],
    )
    .unwrap();
    let branch = json!({"id": "CA-1", "kind": "branch", "target": "feat/x"});

    let linked = ok(&tx, "owner", "link", branch.clone());
    assert_eq!(linked["data"]["id"], "CA-1");
    assert_eq!(linked["data"]["kind"], "branch");
    assert_eq!(linked["data"]["target"], "feat/x");
    assert_eq!(linked["data"]["live"], true);
    assert_eq!(linked["data"]["state"], "open");
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_links"), 1);

    let events = count(&tx, "SELECT count(*) FROM tk_events");
    let unknown_kind = refused(
        &tx,
        "owner",
        "link",
        json!({"id": "CA-1", "kind": "wiki", "target": "feat/x"}),
    );
    assert_eq!(unknown_kind["code"], codes::LINK_KIND_UNKNOWN);
    let unknown_target = refused(
        &tx,
        "owner",
        "link",
        json!({"id": "CA-1", "kind": "branch", "target": "feat/none"}),
    );
    assert_eq!(unknown_target["code"], "host_target_unknown");
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_links"), 1);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events);

    let removed = ok(&tx, "owner", "unlink", branch.clone());
    assert_eq!(removed["data"]["id"], "CA-1");
    assert_eq!(removed["data"]["removed"], true);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_links"), 0);
    let again = refused(&tx, "owner", "unlink", branch);
    assert_eq!(again["code"], codes::LINK_UNKNOWN);
}

#[test]
fn a_host_code_is_kept_apart_from_the_core_codes() {
    let refusal = Refusal::host("host_target_unknown", "no such target");
    assert_eq!(refusal.code, "host_target_unknown");
    assert_eq!(refusal.message, "no such target");
    assert_eq!(
        Refusal::new(codes::STALE_VERSION, "x").code,
        codes::STALE_VERSION
    );
    let at_bound: &'static str = "h".repeat(HOST_CODE_BYTES_MAX).leak();
    assert_eq!(Refusal::host(at_bound, "x").code.len(), HOST_CODE_BYTES_MAX);

    let over: &'static str = "h".repeat(HOST_CODE_BYTES_MAX + 1).leak();
    for code in [
        codes::STALE_VERSION,
        "",
        "Host_x",
        "host-x",
        "host x",
        "h\u{f4}st",
        over,
    ] {
        let built = std::panic::catch_unwind(|| Refusal::host(code, "x"));
        assert!(built.is_err(), "{code:?} is not a host code");
    }
    for code in ["host_target_unknown", "Stale_version"] {
        let built = std::panic::catch_unwind(|| Refusal::new(code, "x"));
        assert!(built.is_err(), "{code:?} is not a core code");
    }
}
