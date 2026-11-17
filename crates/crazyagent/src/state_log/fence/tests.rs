use super::*;
use crate::state_log::replay::redact;
use crate::state_log::{Aggregate, Appender, Draft};
use rusqlite::Connection;

/// A seeded store: the `state` row at revision 0 and the owner tables holding the test host
/// active, that host started in this process. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    seed(&db);
    db
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// One worktrees event. Output: the draft.
fn draft() -> Draft<'static> {
    Draft {
        aggregate: Aggregate::Worktrees,
        kind: "Tick",
        schema: 1,
        at_ms: 1,
        body: b"{}",
    }
}

/// The store's `state` revision. Input: the connection. Output: the revision.
fn revision(db: &Connection) -> i64 {
    db.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn a_writer_whose_fence_the_store_does_not_hold_is_refused_for_each_class() {
    let mut db = store();
    let foreign = HostStamp {
        db_instance: "d".repeat(64),
        engine_epoch: "b".repeat(64),
    };
    host_started(&foreign);
    let tx = db.transaction().unwrap();
    for (fence, expected) in [
        (Fence::Engine(foreign.clone()), "owner_runtime_stale"),
        (
            Fence::Owner(Admission {
                db_instance: foreign.db_instance.clone(),
                ..test_owner()
            }),
            "owner_runtime_stale",
        ),
        (
            Fence::Owner(Admission {
                owner_epoch: 2,
                ..test_owner()
            }),
            "owner_grant_stale_or_revoked",
        ),
    ] {
        assert_eq!(
            code(Appender::own(&tx, fence.clone())),
            Some(expected.into())
        );
        assert_eq!(code(Appender::owner(&tx, 1, fence)), Some(expected.into()));
    }
    tx.execute("UPDATE owner_identities SET status='revoked'", [])
        .unwrap();
    assert_eq!(
        code(Appender::own(&tx, Fence::Owner(test_owner()))),
        Some("owner_grant_stale_or_revoked".into())
    );
    assert_eq!(revision(&tx), 0, "a refused writer moves no revision");
}

#[test]
fn a_stale_engine_epoch_or_an_offline_runtime_is_refused_for_each_writer_class() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let stale = Some("owner_runtime_stale".to_string());
    tx.execute("UPDATE owner_meta SET engine_epoch=?1", ["f".repeat(64)])
        .unwrap();
    for fence in [engine(), Fence::Owner(test_owner())] {
        assert_eq!(
            code(Appender::own(&tx, fence.clone())),
            stale,
            "second instance"
        );
        assert_eq!(
            code(Appender::owner(&tx, 1, fence)),
            stale,
            "second instance"
        );
    }
    tx.execute(
        "UPDATE owner_meta SET engine_epoch=?1, runtime_state='offline'",
        ["b".repeat(64)],
    )
    .unwrap();
    for fence in [engine(), Fence::Owner(test_owner())] {
        assert_eq!(code(Appender::own(&tx, fence.clone())), stale, "offline");
        assert_eq!(code(Appender::owner(&tx, 1, fence)), stale, "offline");
    }
    tx.execute("UPDATE owner_meta SET runtime_state='active'", [])
        .unwrap();
    assert!(Appender::own(&tx, engine()).is_ok());
}

#[test]
fn an_append_from_outside_the_engine_process_is_refused_until_its_host_starts_here() {
    let mut db = store();
    let outside = HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "e".repeat(64),
    };
    db.execute(
        "UPDATE owner_meta SET engine_epoch=?1",
        [&outside.engine_epoch],
    )
    .unwrap();
    let owner = Fence::Owner(Admission {
        engine_epoch: outside.engine_epoch.clone(),
        ..test_owner()
    });
    let refused = Some("state_log_fence_out_of_process".to_string());
    let tx = db.transaction().unwrap();
    for fence in [Fence::Engine(outside.clone()), owner.clone()] {
        assert_eq!(code(Appender::own(&tx, fence)), refused);
    }
    host_started(&outside);
    assert!(hosted(&outside));
    let mut log = Appender::own(&tx, Fence::Engine(outside.clone())).unwrap();
    assert_eq!(log.append(&draft()).unwrap(), 1);
    host_stopped(&outside);
    assert!(!hosted(&outside));
    assert_eq!(
        code(log.project(|_| Ok(()))),
        refused,
        "a stopped host's projection"
    );
    for fence in [Fence::Engine(outside.clone()), owner] {
        assert_eq!(code(Appender::own(&tx, fence)), refused);
    }
}

#[test]
fn a_projection_apply_reasserts_the_fence_and_does_not_run_on_a_stale_one() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, engine()).unwrap();
    log.append(&draft()).unwrap();
    let mut ran = 0;
    let read = log.project(|tx| {
        ran += 1;
        tx.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(error)
    });
    assert_eq!(read, Ok(1));
    tx.execute("UPDATE owner_meta SET runtime_state='offline'", [])
        .unwrap();
    let refused = log.project(|_| {
        ran += 1;
        Ok(())
    });
    assert_eq!(code(refused), Some("owner_runtime_stale".into()));
    assert_eq!(ran, 1, "the projection does not run on a stale fence");
}

#[test]
fn redact_is_fenced_like_an_append() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let seq = Appender::own(&tx, engine())
        .unwrap()
        .append(&draft())
        .unwrap();
    let outside = HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "7".repeat(64),
    };
    assert_eq!(
        code(redact(&tx, seq, &Fence::Engine(outside))),
        Some("state_log_fence_out_of_process".into())
    );
    let revoked = Fence::Owner(Admission {
        owner_epoch: 2,
        ..test_owner()
    });
    assert_eq!(
        code(redact(&tx, seq, &revoked)),
        Some("owner_grant_stale_or_revoked".into())
    );
    let body: Vec<u8> = tx
        .query_row(
            "SELECT body FROM state_log WHERE seq=?1",
            [seq as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, b"{}", "a refused redact leaves the body");
    redact(&tx, seq, &Fence::Owner(test_owner())).unwrap();
}

#[test]
fn the_host_set_keeps_at_most_its_bound_and_drops_the_oldest() {
    let stamp = |index: usize| HostStamp {
        db_instance: format!("{index:064x}"),
        engine_epoch: "b".repeat(64),
    };
    let held = |hosts: &Hosts, index: usize| {
        let host = stamp(index);
        hosts.holds(&host.db_instance, &host.engine_epoch)
    };
    let mut hosts = Hosts::new();
    for index in 0..=HOSTS_MAX {
        hosts.start(&stamp(index));
    }
    assert!(!held(&hosts, 0), "the oldest is dropped past the bound");
    assert!(held(&hosts, 1) && held(&hosts, HOSTS_MAX));
    hosts.start(&stamp(1));
    hosts.start(&stamp(HOSTS_MAX + 1));
    assert!(held(&hosts, 1), "starting a host again makes it the newest");
    assert!(!held(&hosts, 2));
    assert_eq!(
        (0..=HOSTS_MAX + 1)
            .filter(|index| held(&hosts, *index))
            .count(),
        HOSTS_MAX
    );
    hosts.stop(&stamp(1));
    assert!(!held(&hosts, 1));
}

/// A store opening with its own `owner_meta` naming `db_instance`, and the `state` row at
/// revision 0; no host is started for it here. Input: the instance. Output: the connection.
fn opening_store(db_instance: &str) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0);
        CREATE TABLE owner_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            schema_version INTEGER NOT NULL CHECK(schema_version = 1),
            db_instance TEXT NOT NULL CHECK(length(db_instance) = 64),
            engine_epoch TEXT NOT NULL CHECK(length(engine_epoch) = 64),
            runtime_state TEXT NOT NULL CHECK(runtime_state IN ('offline', 'active')));
        INSERT INTO owner_meta VALUES (1,1,'{db_instance}','{}','offline');",
        "f".repeat(64)
    ))
    .unwrap();
    db
}

/// v2.5 L6 (leader 02:34 Q1 A): a writer of the storage open appends while no host of its
/// store runs in this process, moving the revision once, as an own-connection writer.
#[test]
fn an_opening_writer_appends_while_no_host_of_its_store_runs() {
    let mut db = opening_store(&"e".repeat(64));
    let tx = db.transaction().unwrap();
    let mut appender = Appender::opening(&tx, Opening::for_test(7)).unwrap();
    assert_eq!(appender.revision(), 1);
    assert_eq!(appender.append(&draft()).unwrap(), 1);
    assert_eq!(appender.project(|_| Ok(3)).unwrap(), 3);
    tx.commit().unwrap();
    assert_eq!(revision(&db), 1);

    let mut bare = Connection::open_in_memory().unwrap();
    bare.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,4);",
    )
    .unwrap();
    let tx = bare.transaction().unwrap();
    let appender = Appender::opening(&tx, Opening::for_test(7)).unwrap();
    assert_eq!(
        appender.revision(),
        5,
        "a store with no owner tables yet has no host"
    );
}

/// v2.5 L6 (leader 02:34 Q1 A): once a host of the store runs in this process the opening
/// class is refused, at the start and again at each projection apply, and nothing moves;
/// a host of another store does not refuse it.
#[test]
fn an_opening_writer_is_refused_once_a_host_of_its_store_runs() {
    let instance = "9".repeat(64);
    let host = HostStamp {
        db_instance: instance.clone(),
        engine_epoch: "8".repeat(64),
    };
    let mut db = opening_store(&instance);
    let tx = db.transaction().unwrap();
    let appender = Appender::opening(&tx, Opening::for_test(7)).unwrap();
    host_started(&host);
    assert_eq!(
        code(appender.project(|_| Ok(()))),
        Some("state_log_opening_host_running".into()),
        "the projection's check is the pair of the start's"
    );
    drop(appender);
    assert_eq!(
        code(Appender::opening(&tx, Opening::for_test(7))),
        Some("state_log_opening_host_running".into())
    );
    tx.commit().unwrap();
    assert_eq!(
        revision(&db),
        1,
        "only the first opening moved the revision"
    );
    host_stopped(&host);

    let tx = db.transaction().unwrap();
    assert!(
        Appender::opening(&tx, Opening::for_test(7)).is_ok(),
        "a stopped host no longer refuses it"
    );
}
