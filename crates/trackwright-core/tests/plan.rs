use rusqlite::Transaction;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};
use trackwright_core::model::{
    Actor, PolicyKey, Priority, ProjectKey, ShortId, TicketKind, TicketStatus, codes,
};
use trackwright_core::ops;
use trackwright_core::policy;
use trackwright_core::reply::{PAGE_ROWS_MAX, REPLY_BYTES_DEFAULT};
use trackwright_core::requests::Request;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, NewTicket, Store};

const OWNER: &str = "owner";
const DAY_MS: i64 = 86_400_000;

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

/// A fresh database holding project CA, created by the owner.
fn setup(dir: &Path) -> Store {
    let mut store = Store::open(&db_path(dir)).unwrap();
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &actor(OWNER), &key(), "CrazyAgent", 1_000).unwrap();
    tx.commit().unwrap();
    store
}

fn call(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let bytes = serde_json::to_vec(&args).unwrap();
    let request = Request::parse(op, &bytes)
        .unwrap_or_else(|refusal| panic!("{op} {args} parses: {refusal}"));
    let reply = ops::dispatch(tx, &Standalone, &actor(who), request);
    assert!(reply.json().len() <= REPLY_BYTES_DEFAULT);
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

fn refused(tx: &Transaction<'_>, who: &str, op: &str, args: Value, code: &str) -> Value {
    let value = call(tx, who, op, args);
    assert_eq!(value["ok"], false, "{op} is refused: {value}");
    assert_eq!(value["code"], code, "{op}: {value}");
    value
}

fn count(tx: &Transaction<'_>, sql: &str) -> i64 {
    tx.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// Seeds a ticket straight through the store at a time outside the write-rate window, so
/// setup rows never count against the actor that runs the operation under test.
fn seed(
    tx: &Transaction<'_>,
    title: &str,
    status: TicketStatus,
    priority: u8,
    at_ms: i64,
) -> String {
    let new = NewTicket {
        title: title.to_owned(),
        body: String::new(),
        kind: TicketKind::Feature,
        status,
        priority: Priority::new(priority).unwrap(),
    };
    store::create_ticket(tx, &actor(OWNER), &key(), &new, at_ms)
        .unwrap()
        .short_id
        .to_string()
}

fn version_of(tx: &Transaction<'_>, text: &str) -> i64 {
    store::ticket(tx, &id(text)).unwrap().version
}

fn close(tx: &Transaction<'_>, text: &str, status: TicketStatus) {
    let version = version_of(tx, text);
    policy::close(
        tx,
        &Standalone,
        &actor(OWNER),
        &id(text),
        version,
        status,
        None,
        5_000,
    )
    .unwrap();
}

fn shown(tx: &Transaction<'_>, text: &str) -> Value {
    ok(tx, OWNER, "show", json!({"id": text}))["data"]["ticket"].clone()
}

fn place(tx: &Transaction<'_>, text: &str, fields: Value) -> Value {
    let mut args = json!({"id": text, "version": version_of(tx, text)});
    for (name, value) in fields.as_object().unwrap() {
        args[name] = value.clone();
    }
    ok(tx, OWNER, "edit", args)
}

fn ready_ids(tx: &Transaction<'_>, who: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut args = json!({"project": "CA"});
    for _ in 0..100 {
        let page = ok(tx, who, "ready", args.clone());
        let rows = page["data"]["tickets"].as_array().unwrap();
        assert!(rows.len() <= PAGE_ROWS_MAX);
        ids.extend(
            rows.iter()
                .map(|row| row["id"].as_str().unwrap().to_owned()),
        );
        match page["cursor"].as_str() {
            Some(cursor) => args = json!({"project": "CA", "cursor": cursor}),
            None => return ids,
        }
    }
    panic!("ready pages end");
}

fn next_of(reply: &Value) -> Vec<(String, Option<String>)> {
    reply["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|next| {
            (
                next["op"].as_str().unwrap().to_owned(),
                next["id"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// The civil date of a day count since 1970-01-01, by Howard Hinnant's algorithm, written
/// here on its own so the test does not lean on the core's date code.
fn day_text(days: i64) -> String {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn today() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(now.as_millis()).unwrap().div_euclid(DAY_MS)
}

#[test]
fn ready_excludes_a_ticket_until_its_blocker_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let blocker = seed(&tx, "Blocker", TicketStatus::Todo, 2, 2_000);
    let waiting = seed(&tx, "Waiting", TicketStatus::Todo, 2, 2_001);
    assert_eq!(
        ready_ids(&tx, "agent:a"),
        [blocker.as_str(), waiting.as_str()]
    );

    let depended = ok(&tx, OWNER, "depend", json!({"id": waiting, "on": blocker}));
    assert_eq!(depended["data"]["id"], waiting.as_str());
    assert_eq!(depended["data"]["on"], blocker.as_str());
    assert_eq!(depended["data"]["blocked"], true);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_deps"), 1);
    assert_eq!(ready_ids(&tx, "agent:a"), [blocker.as_str()]);
    assert_eq!(shown(&tx, &waiting)["blocked_by"], json!([blocker]));

    close(&tx, &blocker, TicketStatus::Done);
    assert_eq!(ready_ids(&tx, "agent:a"), [waiting.as_str()]);
    assert_eq!(shown(&tx, &waiting)["blocked_by"], json!([]));

    let reopened = seed(&tx, "Second blocker", TicketStatus::Todo, 2, 2_002);
    ok(&tx, OWNER, "depend", json!({"id": waiting, "on": reopened}));
    assert_eq!(ready_ids(&tx, "agent:a"), [reopened.as_str()]);
    let undepended = ok(
        &tx,
        OWNER,
        "undepend",
        json!({"id": waiting, "on": reopened}),
    );
    assert_eq!(undepended["data"]["removed"], true);
    assert_eq!(
        ready_ids(&tx, "agent:a"),
        [waiting.as_str(), reopened.as_str()]
    );
    refused(
        &tx,
        OWNER,
        "undepend",
        json!({"id": waiting, "on": reopened}),
        codes::DEPENDENCY_UNKNOWN,
    );
    let history = ok(&tx, OWNER, "history", json!({"id": waiting}));
    let ops: Vec<&str> = history["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops.iter().filter(|op| **op == "dep.add").count(), 2);
    assert_eq!(ops.iter().filter(|op| **op == "dep.remove").count(), 1);
}

#[test]
fn ready_orders_by_priority_then_the_active_cycle_then_age() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
    let old = seed(&tx, "Old, priority 2", TicketStatus::Todo, 2, 2_000);
    let planned = seed(
        &tx,
        "New, priority 2, this cycle",
        TicketStatus::Todo,
        2,
        2_001,
    );
    let urgent = seed(&tx, "Priority 1", TicketStatus::Todo, 1, 2_002);
    let triage = seed(&tx, "Triage", TicketStatus::Triage, 0, 2_003);
    let backlog = seed(&tx, "Backlog", TicketStatus::Backlog, 0, 2_004);
    let theirs = seed(&tx, "Claimed by b", TicketStatus::InProgress, 0, 2_005);
    let mine = seed(&tx, "Claimed by a", TicketStatus::Todo, 2, 2_006);
    let parent = seed(&tx, "Parent", TicketStatus::Todo, 0, 2_007);
    let child = seed(&tx, "Child", TicketStatus::Todo, 3, 2_008);
    place(&tx, &planned, json!({"cycle": 1}));
    place(&tx, &child, json!({"parent": parent}));
    ok(&tx, "agent:b", "claim", json!({"id": theirs, "version": 1}));
    ok(&tx, "agent:a", "claim", json!({"id": mine, "version": 1}));

    assert_eq!(
        ready_ids(&tx, "agent:a"),
        [
            urgent.as_str(),
            planned.as_str(),
            old.as_str(),
            mine.as_str(),
            child.as_str()
        ]
    );
    let for_b = ready_ids(&tx, "agent:b");
    assert_eq!(
        for_b[0], theirs,
        "b's own in-progress work at priority 0 leads"
    );
    assert!(!for_b.contains(&mine));
    for excluded in [&triage, &backlog, &parent] {
        assert!(!for_b.contains(excluded), "{excluded} is not ready");
    }

    let page = ok(&tx, "agent:a", "ready", json!({"project": "CA"}));
    let rows = page["data"]["tickets"].as_array().unwrap();
    assert_eq!(rows[1]["id"], planned.as_str());
    assert_eq!(rows[1]["cycle"], 1);
    assert_eq!(rows[1]["priority"], 2);
    assert_eq!(rows[1]["status"], "todo");
    assert_eq!(rows[3]["assignee"], "agent:a");
    assert_eq!(rows[0]["cycle"], Value::Null);
    assert!(next_of(&page).contains(&("claim".to_owned(), Some(urgent.clone()))));

    for at in 0..60 {
        seed(
            &tx,
            &format!("Filler {at}"),
            TicketStatus::Todo,
            4,
            3_000 + at,
        );
    }
    let all = ready_ids(&tx, "agent:a");
    assert_eq!(all.len(), 65);
    assert_eq!(
        all[..5],
        [
            urgent.as_str(),
            planned.as_str(),
            old.as_str(),
            mine.as_str(),
            child.as_str()
        ]
    );
    assert_eq!(all[5], "CA-10");
    assert_eq!(all[64], "CA-69");
    refused(
        &tx,
        "agent:a",
        "ready",
        json!({"project": "CA", "cursor": "ready:99999"}),
        codes::CURSOR_INVALID,
    );
}

#[test]
fn a_dependency_cycle_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let first = seed(&tx, "First", TicketStatus::Todo, 2, 2_000);
    let second = seed(&tx, "Second", TicketStatus::Todo, 2, 2_001);
    let third = seed(&tx, "Third", TicketStatus::Todo, 2, 2_002);
    ok(&tx, OWNER, "depend", json!({"id": second, "on": first}));
    ok(&tx, OWNER, "depend", json!({"id": third, "on": second}));
    let deps = count(&tx, "SELECT count(*) FROM tk_deps");
    let events = count(&tx, "SELECT count(*) FROM tk_events");

    let cycle = refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": first, "on": third}),
        codes::DEPENDENCY_CYCLE,
    );
    assert_eq!(cycle["data"]["id"], first.as_str());
    assert_eq!(cycle["data"]["on"], third.as_str());
    refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": first, "on": second}),
        codes::DEPENDENCY_CYCLE,
    );
    refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": first, "on": first}),
        codes::DEPENDENCY_INVALID,
    );
    refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": second, "on": first}),
        codes::DEPENDENCY_EXISTS,
    );
    let other = ProjectKey::new("XY").unwrap();
    store::create_project(&tx, &actor(OWNER), &other, "Other", 1_000).unwrap();
    let new = NewTicket {
        title: "Elsewhere".to_owned(),
        body: String::new(),
        kind: TicketKind::Chore,
        status: TicketStatus::Todo,
        priority: Priority::new(2).unwrap(),
    };
    store::create_ticket(&tx, &actor(OWNER), &other, &new, 2_003).unwrap();
    refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": first, "on": "XY-1"}),
        codes::DEPENDENCY_INVALID,
    );
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_deps"), deps);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events);

    place(&tx, &second, json!({"parent": first}));
    place(&tx, &third, json!({"parent": second}));
    for parent in [&third, &second, &first] {
        refused(
            &tx,
            OWNER,
            "edit",
            json!({"id": first, "version": version_of(&tx, &first), "parent": parent}),
            codes::PARENT_INVALID,
        );
    }
    assert_eq!(shown(&tx, &first)["parent"], Value::Null);

    let many = seed(&tx, "Waits for many", TicketStatus::Todo, 2, 2_100);
    let waits = id(&many).number();
    for at in 0..64 {
        let blocker = id(&seed(
            &tx,
            &format!("Blocker {at}"),
            TicketStatus::Todo,
            2,
            2_200 + at,
        ));
        tx.execute(
            "INSERT INTO tk_deps(ticket, blocks) SELECT b.id, w.id FROM tk_tickets b \
             JOIN tk_tickets w ON w.project = b.project JOIN tk_projects p ON p.id = b.project \
             WHERE p.key = 'CA' AND b.number = ?1 AND w.number = ?2",
            [blocker.number(), waits],
        )
        .unwrap();
    }
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_deps"), deps + 64);
    refused(
        &tx,
        OWNER,
        "depend",
        json!({"id": many, "on": first}),
        codes::DEPENDENCIES_FULL,
    );
}

#[test]
fn two_claims_race_and_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let ticket = seed(&tx, "Contended", TicketStatus::Todo, 2, 2_000);
    tx.commit().unwrap();
    drop(store);

    let path = db_path(dir.path());
    let barrier = Arc::new(Barrier::new(2));
    let racers: Vec<_> = ["agent:a", "agent:b"]
        .into_iter()
        .map(|who| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let ticket = ticket.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                let tx = store.transaction().unwrap();
                let reply = call(&tx, who, "claim", json!({"id": ticket, "version": 1}));
                tx.commit().unwrap();
                (who, reply)
            })
        })
        .collect();
    let replies: Vec<(&str, Value)> = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect();

    let winners: Vec<&(&str, Value)> = replies
        .iter()
        .filter(|(_, reply)| reply["ok"] == true)
        .collect();
    assert_eq!(winners.len(), 1, "exactly one claim wins: {replies:?}");
    let (winner, won) = winners[0];
    assert_eq!(won["data"]["assignee"], *winner);
    assert_eq!(won["data"]["version"], 2);
    let (loser, lost) = replies.iter().find(|(who, _)| who != winner).unwrap();
    assert_eq!(lost["code"], codes::ALREADY_CLAIMED);
    assert_eq!(lost["data"]["assignee"], *winner);
    assert_eq!(next_of(lost), [("ready".to_owned(), None)]);

    let mut store = Store::open(&path).unwrap();
    let tx = store.transaction().unwrap();
    let stored = store::ticket(&tx, &id(&ticket)).unwrap();
    assert_eq!(stored.assignee.as_ref().map(Actor::as_str), Some(*winner));
    assert_eq!(stored.version, 2);
    assert_eq!(
        count(
            &tx,
            "SELECT count(*) FROM tk_events WHERE op = 'ticket.update'"
        ),
        1
    );

    let again = ok(&tx, winner, "claim", json!({"id": ticket, "version": 1}));
    assert_eq!(
        again["data"]["version"], 2,
        "claiming your own claim changes nothing"
    );
    refused(
        &tx,
        loser,
        "release",
        json!({"id": ticket, "version": 2}),
        codes::NOT_CLAIMANT,
    );
    let released = ok(&tx, winner, "release", json!({"id": ticket, "version": 2}));
    assert_eq!(released["data"]["assignee"], Value::Null);
    refused(
        &tx,
        winner,
        "release",
        json!({"id": ticket, "version": 3}),
        codes::NOT_CLAIMANT,
    );
    let taken = ok(&tx, loser, "claim", json!({"id": ticket, "version": 3}));
    assert_eq!(taken["data"]["assignee"], *loser);
    let freed = ok(&tx, OWNER, "release", json!({"id": ticket, "version": 4}));
    assert_eq!(
        freed["data"]["assignee"],
        Value::Null,
        "the owner frees a stuck claim"
    );
    ok(&tx, loser, "claim", json!({"id": ticket, "version": 5}));
    let unstuck = ok(
        &tx,
        "engine:supervisor",
        "release",
        json!({"id": ticket, "version": 6}),
    );
    assert_eq!(
        unstuck["data"]["assignee"],
        Value::Null,
        "an engine the host lets edit the ticket frees a stuck claim"
    );

    close(&tx, &ticket, TicketStatus::Done);
    refused(
        &tx,
        loser,
        "claim",
        json!({"id": ticket, "version": version_of(&tx, &ticket)}),
        codes::TICKET_CLOSED,
    );
}

#[test]
fn closing_a_cycle_rolls_unfinished_work_per_policy() {
    for rollover in ["auto", "ask", "off"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = setup(dir.path());
        let tx = store.transaction().unwrap();
        let owner = actor(OWNER);
        policy::set(
            &tx,
            &Standalone,
            &owner,
            &key(),
            PolicyKey::Rollover,
            rollover,
            0,
            3_000,
        )
        .unwrap();
        let started = ok(
            &tx,
            OWNER,
            "cycle_start",
            json!({"project": "CA", "starts_day": "2099-11-02", "days": 14}),
        );
        assert_eq!(started["data"]["cycle"]["number"], 1);
        assert_eq!(started["data"]["cycle"]["status"], "active");
        assert_eq!(started["data"]["cycle"]["ends_day"], "2099-11-15");
        let unfinished = seed(&tx, "Unfinished", TicketStatus::Todo, 2, 2_000);
        let finished = seed(&tx, "Finished", TicketStatus::Todo, 2, 2_001);
        let outside = seed(&tx, "In no cycle", TicketStatus::Todo, 2, 2_002);
        place(&tx, &unfinished, json!({"cycle": 1}));
        place(&tx, &finished, json!({"cycle": 1}));
        close(&tx, &finished, TicketStatus::Done);

        let closing = ok(
            &tx,
            OWNER,
            "cycle_close",
            json!({"project": "CA", "number": 1, "version": 1}),
        );
        let data = &closing["data"];
        assert_eq!(data["closed"]["number"], 1, "{rollover}");
        assert_eq!(data["closed"]["status"], "closed", "{rollover}");
        assert_eq!(data["rollover"], rollover);
        assert_eq!(data["unfinished"], 1, "{rollover}");
        assert_eq!(
            shown(&tx, &finished)["cycle"],
            1,
            "{rollover}: closed work stays"
        );
        assert_eq!(shown(&tx, &outside)["cycle"], Value::Null, "{rollover}");
        let edits: Vec<_> = next_of(&closing)
            .into_iter()
            .filter(|(op, _)| op == "edit")
            .collect();
        match rollover {
            "auto" => {
                assert_eq!(data["moved_to"], 2);
                assert_eq!(shown(&tx, &unfinished)["cycle"], 2);
                assert!(edits.is_empty());
                let roadmap = ok(&tx, OWNER, "roadmap", json!({"project": "CA"}));
                let cycles = roadmap["data"]["cycles"].as_array().unwrap();
                assert_eq!(cycles[1]["number"], 2);
                assert_eq!(cycles[1]["status"], "active");
                assert_eq!(cycles[1]["starts_day"], "2099-11-16");
                assert_eq!(cycles[1]["ends_day"], "2099-11-29", "the policy's 14 days");

                let third = ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
                assert_eq!(third["data"]["cycle"]["number"], 3);
                assert_eq!(third["data"]["cycle"]["status"], "upcoming");
                let second = ok(
                    &tx,
                    OWNER,
                    "cycle_close",
                    json!({"project": "CA", "number": 2, "version": 1}),
                );
                assert_eq!(second["data"]["moved_to"], 3);
                assert_eq!(shown(&tx, &unfinished)["cycle"], 3);
                let roadmap = ok(&tx, OWNER, "roadmap", json!({"project": "CA"}));
                assert_eq!(roadmap["data"]["cycles"][2]["status"], "active");
            }
            "ask" => {
                assert_eq!(data["moved_to"], Value::Null);
                assert_eq!(data["tickets"], json!([unfinished]));
                assert_eq!(shown(&tx, &unfinished)["cycle"], 1);
                assert_eq!(edits, [("edit".to_owned(), Some(unfinished.clone()))]);
            }
            _ => {
                assert_eq!(data["moved_to"], Value::Null);
                assert_eq!(shown(&tx, &unfinished)["cycle"], 1);
                assert!(edits.is_empty());
            }
        }
        refused(
            &tx,
            OWNER,
            "cycle_close",
            json!({"project": "CA", "number": 1, "version": 2}),
            codes::CYCLE_CLOSED,
        );
        refused(
            &tx,
            OWNER,
            "cycle_close",
            json!({"project": "CA", "number": 9, "version": 1}),
            codes::CYCLE_UNKNOWN,
        );
    }
}

#[test]
fn roadmap_progress_counts_closed_out_of_total_per_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let first = ok(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v1", "target_day": "2026-12-01"}),
    );
    assert_eq!(first["data"]["version"]["name"], "v1");
    assert_eq!(first["data"]["version"]["position"], 0);
    assert_eq!(first["data"]["version"]["status"], "planned");
    assert_eq!(first["data"]["version"]["version"], 1);
    let second = ok(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v2"}),
    );
    assert_eq!(second["data"]["version"]["position"], 1);

    let tickets: Vec<String> = (0..5)
        .map(|at| {
            seed(
                &tx,
                &format!("Ticket {at}"),
                TicketStatus::Todo,
                2,
                2_000 + at,
            )
        })
        .collect();
    for ticket in &tickets[..3] {
        place(&tx, ticket, json!({"release": "v1"}));
    }
    place(&tx, &tickets[3], json!({"release": "v2"}));
    close(&tx, &tickets[0], TicketStatus::Done);
    close(&tx, &tickets[1], TicketStatus::Cancelled);
    assert_eq!(shown(&tx, &tickets[0])["release"], "v1");

    let roadmap = ok(&tx, OWNER, "roadmap", json!({"project": "CA"}));
    let versions = roadmap["data"]["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["name"], "v1");
    assert_eq!(versions[0]["closed"], 2);
    assert_eq!(versions[0]["total"], 3);
    assert_eq!(versions[0]["target_day"], "2026-12-01");
    assert_eq!(versions[1]["name"], "v2");
    assert_eq!(versions[1]["closed"], 0);
    assert_eq!(versions[1]["total"], 1);
    assert_eq!(
        roadmap["data"]["unplanned"],
        json!({"closed": 0, "total": 1})
    );

    let stale = refused(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v1", "status": "active"}),
        codes::STALE_VERSION,
    );
    assert_eq!(stale["data"]["version"], 1);
    refused(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v9", "version": 1}),
        codes::VERSION_UNKNOWN,
    );
    let moved = ok(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v1", "version": 1, "position": 5, "status": "active"}),
    );
    assert_eq!(moved["data"]["version"]["version"], 2);
    assert_eq!(moved["data"]["version"]["status"], "active");
    assert_eq!(moved["data"]["version"]["target_day"], "2026-12-01");

    ok(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "release": "v1"}),
    );
    place(&tx, &tickets[2], json!({"cycle": 1}));
    let roadmap = ok(&tx, OWNER, "roadmap", json!({"project": "CA"}));
    let versions = roadmap["data"]["versions"].as_array().unwrap();
    assert_eq!(versions[0]["name"], "v2", "versions follow their position");
    assert_eq!(versions[1]["name"], "v1");
    assert_eq!(versions[1]["closed"], 2);
    assert_eq!(versions[1]["total"], 3);
    let cycles = roadmap["data"]["cycles"].as_array().unwrap();
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0]["release"], "v1");
    assert_eq!(cycles[0]["closed"], 0);
    assert_eq!(cycles[0]["total"], 1);
    assert_eq!(roadmap["cursor"], Value::Null);

    for at in 2..64 {
        let planner = format!("engine:planner{}", at % 3);
        let name = format!("v{at}.0");
        ok(
            &tx,
            &planner,
            "version_upsert",
            json!({"project": "CA", "name": name}),
        );
    }
    refused(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v99"}),
        codes::VERSIONS_FULL,
    );
}

#[test]
fn a_cycle_takes_its_length_from_policy_and_never_overlaps() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let owner = actor(OWNER);
    policy::set(
        &tx,
        &Standalone,
        &owner,
        &key(),
        PolicyKey::CycleDays,
        "7",
        0,
        3_000,
    )
    .unwrap();

    let before = today();
    let first = ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
    let after = today();
    let starts = &first["data"]["cycle"]["starts_day"];
    let start = if *starts == day_text(before) {
        before
    } else {
        after
    };
    assert_eq!(*starts, day_text(start));
    assert_eq!(first["data"]["cycle"]["ends_day"], day_text(start + 6));
    assert_eq!(first["data"]["cycle"]["status"], "active");

    let second = ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
    assert_eq!(second["data"]["cycle"]["number"], 2);
    assert_eq!(second["data"]["cycle"]["starts_day"], day_text(start + 7));
    assert_eq!(second["data"]["cycle"]["ends_day"], day_text(start + 13));
    assert_eq!(second["data"]["cycle"]["status"], "upcoming");

    let overlap = refused(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "starts_day": day_text(start + 13)}),
        codes::CYCLE_OVERLAPS,
    );
    assert_eq!(overlap["data"]["after"], day_text(start + 13));
    let leap = ok(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "starts_day": "2028-02-25", "days": 7}),
    );
    assert_eq!(leap["data"]["cycle"]["ends_day"], "2028-03-02");
    refused(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "starts_day": "9999-12-30", "days": 7}),
        codes::FIELD_INVALID,
    );
    refused(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "release": "v1"}),
        codes::VERSION_UNKNOWN,
    );
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_cycles"), 3);
    let bad_day = Request::parse(
        "cycle_start",
        br#"{"project":"CA","starts_day":"2027-02-29"}"#,
    );
    assert_eq!(bad_day.map(|_| ()).unwrap_err().code, codes::FIELD_INVALID);
}

#[test]
fn planning_fields_place_a_ticket_and_clear_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    ok(
        &tx,
        OWNER,
        "version_upsert",
        json!({"project": "CA", "name": "v1"}),
    );
    ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
    let parent = seed(&tx, "Parent", TicketStatus::Todo, 2, 2_000);

    let created = ok(
        &tx,
        OWNER,
        "create",
        json!({"project": "CA", "title": "Child", "status": "todo",
               "parent": parent, "release": "v1", "cycle": 1}),
    );
    let child = created["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["data"]["parent"], parent.as_str());
    assert_eq!(created["data"]["release"], "v1");
    assert_eq!(created["data"]["cycle"], 1);
    assert_eq!(created["data"]["version"], 1, "created already placed");

    let cleared = ok(
        &tx,
        OWNER,
        "edit",
        json!({"id": child, "version": 1, "cycle_clear": true, "title": "Child, renamed"}),
    );
    assert_eq!(cleared["data"]["cycle"], Value::Null);
    assert_eq!(cleared["data"]["release"], "v1");
    assert_eq!(cleared["data"]["title"], "Child, renamed");
    assert_eq!(cleared["data"]["version"], 2, "one edit, one version");

    let history = ok(&tx, OWNER, "history", json!({"id": child}));
    let events = history["data"]["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        2,
        "one event for the create, one for the edit"
    );
    assert_eq!(events[0]["op"], "ticket.create");
    assert_eq!(events[1]["op"], "ticket.update");
    assert_eq!(events[1]["fields"], json!(["cycle", "title"]));

    let refusals = [
        (
            json!({"cycle": 1, "cycle_clear": true}),
            codes::FIELD_INVALID,
        ),
        (json!({"parent": child}), codes::PARENT_INVALID),
        (json!({"release": "v9"}), codes::VERSION_UNKNOWN),
        (json!({"cycle": 7}), codes::CYCLE_UNKNOWN),
    ];
    for (fields, code) in refusals {
        let mut args = json!({"id": child, "version": 2});
        for (name, value) in fields.as_object().unwrap() {
            args[name] = value.clone();
        }
        refused(&tx, OWNER, "edit", args, code);
    }
    let other = ProjectKey::new("XY").unwrap();
    store::create_project(&tx, &actor(OWNER), &other, "Other", 1_000).unwrap();
    let new = NewTicket {
        title: "Elsewhere".to_owned(),
        body: String::new(),
        kind: TicketKind::Chore,
        status: TicketStatus::Todo,
        priority: Priority::new(2).unwrap(),
    };
    store::create_ticket(&tx, &actor(OWNER), &other, &new, 2_001).unwrap();
    refused(
        &tx,
        OWNER,
        "edit",
        json!({"id": child, "version": 2, "parent": "XY-1"}),
        codes::PARENT_INVALID,
    );
    close(&tx, &parent, TicketStatus::Done);
    let orphan = seed(&tx, "Orphan", TicketStatus::Todo, 2, 2_002);
    refused(
        &tx,
        OWNER,
        "edit",
        json!({"id": orphan, "version": 1, "parent": parent}),
        codes::PARENT_INVALID,
    );
    ok(
        &tx,
        OWNER,
        "cycle_close",
        json!({"project": "CA", "number": 1, "version": 1}),
    );
    refused(
        &tx,
        OWNER,
        "edit",
        json!({"id": orphan, "version": 1, "cycle": 1}),
        codes::CYCLE_CLOSED,
    );
    assert_eq!(
        version_of(&tx, &child),
        2,
        "every refusal left the ticket as it was"
    );
    assert_eq!(shown(&tx, &child)["parent"], parent.as_str());
}

#[test]
fn a_parent_needs_authority_to_edit_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let owners = seed(&tx, "Owner's work", TicketStatus::Todo, 2, 2_000);
    let events = count(&tx, "SELECT count(*) FROM tk_events");
    refused(
        &tx,
        "agent:x",
        "create",
        json!({"project": "CA", "title": "Mine", "parent": owners}),
        codes::AGENT_EDIT_REFUSED,
    );
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_tickets"), 1);
    assert_eq!(count(&tx, "SELECT count(*) FROM tk_events"), events);
    let mine = ok(
        &tx,
        "agent:x",
        "create",
        json!({"project": "CA", "title": "Mine"}),
    )["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    refused(
        &tx,
        "agent:x",
        "edit",
        json!({"id": mine, "version": 1, "parent": owners}),
        codes::AGENT_EDIT_REFUSED,
    );
    assert_eq!(shown(&tx, &mine)["parent"], Value::Null);
    assert_eq!(
        ready_ids(&tx, "agent:y"),
        [owners.as_str()],
        "no agent took the owner's ticket out of ready"
    );

    policy::set(
        &tx,
        &Standalone,
        &actor(OWNER),
        &key(),
        PolicyKey::AgentEdit,
        "any",
        0,
        3_000,
    )
    .unwrap();
    let placed = ok(
        &tx,
        "agent:x",
        "edit",
        json!({"id": mine, "version": 1, "parent": owners}),
    );
    assert_eq!(placed["data"]["parent"], owners.as_str());
    assert!(
        ready_ids(&tx, "agent:y").is_empty(),
        "a parent with open work waits"
    );
}

#[test]
fn a_default_cycle_start_is_never_in_the_past() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = setup(dir.path());
    let tx = store.transaction().unwrap();
    let old = ok(
        &tx,
        OWNER,
        "cycle_start",
        json!({"project": "CA", "starts_day": "2020-01-01", "days": 7}),
    );
    assert_eq!(old["data"]["cycle"]["ends_day"], "2020-01-07");
    let before = today();
    let next = ok(&tx, OWNER, "cycle_start", json!({"project": "CA"}));
    let after = today();
    let starts = next["data"]["cycle"]["starts_day"].as_str().unwrap();
    assert!(
        starts == day_text(before) || starts == day_text(after),
        "a cycle after a pause starts today (UTC), not on 2020-01-08: {starts}"
    );
    assert_eq!(next["data"]["cycle"]["status"], "upcoming");
}
