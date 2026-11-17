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
