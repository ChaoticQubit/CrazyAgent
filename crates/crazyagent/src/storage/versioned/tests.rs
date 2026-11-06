use super::*;
use rusqlite::{Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

fn database() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("engine.db");
    (directory, path)
}

fn connect(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
}

fn agent(id: i64) -> Writer {
    Writer::Agent(AgentId::team(id).unwrap())
}

fn owner() -> Writer {
    Writer::Owner {
        force_after_conflict: false,
    }
}

fn keep_mine() -> Writer {
    Writer::Owner {
        force_after_conflict: true,
    }
}

fn write(
    connection: &mut Connection,
    item: &Item,
    expected: u64,
    writer: &Writer,
) -> Result<u64, Refusal> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let written = write_if(&tx, item, expected, writer);
    tx.commit().unwrap();
    written
}

fn stale(written: Result<u64, Refusal>) -> Stale {
    match written {
        Err(Refusal::Stale(stale)) => stale,
        other => panic!("expected a stale refusal, got {other:?}"),
    }
}

fn read(connection: &mut Connection, item: &Item) -> Current {
    let tx = connection.transaction().unwrap();
    let now = current(&tx, item).unwrap();
    drop(tx);
    now
}

fn forced(connection: &mut Connection, item: &Item) -> Vec<Force> {
    let tx = connection.transaction().unwrap();
    let records = forces(&tx, item).unwrap();
    drop(tx);
    records
}

#[test]
fn stale_agent_write_is_refused_with_the_current_version() {
    let (_directory, path) = database();
    let mut connection = connect(&path);
    let item = Item::new("process:project:tidepool").unwrap();
    assert_eq!(
        read(&mut connection, &item),
        Current {
            version: 0,
            writer: LastWriter::Nobody,
        },
        "an item nobody wrote is at version 0"
    );

    assert_eq!(write(&mut connection, &item, 0, &owner()), Ok(1));
    assert_eq!(write(&mut connection, &item, 1, &agent(3)), Ok(2));
    let refused = stale(write(&mut connection, &item, 1, &agent(4)));
    assert_eq!(
        refused,
        Stale {
            version: 1,
            current: 2,
            writer: LastWriter::Agent(AgentId::team(3).unwrap()),
        },
        "the stale writer learns the version it must re-read and who wrote it"
    );
    assert_eq!(
        read(&mut connection, &item),
        Current {
            version: 2,
            writer: LastWriter::Agent(AgentId::team(3).unwrap()),
        },
        "a refused write changes nothing"
    );

    let ahead = stale(write(&mut connection, &item, 7, &agent(4)));
    assert_eq!(
        (ahead.version, ahead.current),
        (7, 2),
        "a version from the future is stale too"
    );
    let fresh = Item::new("memory:tidepool:entry-12").unwrap();
    assert_eq!(
        stale(write(&mut connection, &fresh, 1, &agent(4))),
        Stale {
            version: 1,
            current: 0,
            writer: LastWriter::Nobody,
        }
    );
    assert_eq!(
        write(&mut connection, &item, 2, &agent(4)),
        Ok(3),
        "after a re-read the redo lands"
    );

    for invalid in [
        "",
        "has space",
        "tab\there",
        "naïve",
        &"x".repeat(ITEM_BYTES_MAX + 1),
    ] {
        assert_eq!(
            Item::new(invalid).unwrap_err(),
            EngineError("versioned_item_invalid".into()),
            "{invalid:?}"
        );
    }
    assert!(Item::new(&"x".repeat(ITEM_BYTES_MAX)).is_ok());
}

#[test]
fn owner_force_after_conflict_is_recorded_and_agents_cannot_force() {
    let (_directory, path) = database();
    let mut connection = connect(&path);
    let item = Item::new("agent-instructions:tidepool:team-3").unwrap();
    assert_eq!(write(&mut connection, &item, 0, &agent(3)), Ok(1));

    let conflict = stale(write(&mut connection, &item, 0, &owner()));
    assert_eq!(conflict.current, 1);
    assert!(
        forced(&mut connection, &item).is_empty(),
        "a refused save records nothing"
    );

    assert_eq!(
        write(&mut connection, &item, conflict.current, &keep_mine()),
        Ok(2),
        "Keep mine writes over the version the owner was shown"
    );
    let records = forced(&mut connection, &item);
    assert_eq!(
        records,
        vec![Force {
            replaced_version: 1,
            replaced_writer: LastWriter::Agent(AgentId::team(3).unwrap()),
            version: 2,
            at_ms: records[0].at_ms,
        }]
    );
    assert!(records[0].at_ms > 0);
    assert_eq!(
        read(&mut connection, &item).writer,
        LastWriter::Owner,
        "the owner's edit wins"
    );

    assert_eq!(write(&mut connection, &item, 2, &agent(5)), Ok(3));
    let unseen = stale(write(&mut connection, &item, 2, &keep_mine()));
    assert_eq!(
        (unseen.current, unseen.writer),
        (3, LastWriter::Agent(AgentId::team(5).unwrap())),
        "Keep mine never overwrites a version the owner was not shown"
    );
    assert_eq!(forced(&mut connection, &item).len(), 1);

    assert_eq!(
        stale(write(&mut connection, &item, 2, &agent(3))).current,
        3,
        "an agent has no way to write past a version it did not read"
    );
    assert_eq!(write(&mut connection, &item, 3, &owner()), Ok(4));
    assert_eq!(
        forced(&mut connection, &item).len(),
        1,
        "only a Keep mine after a conflict is recorded"
    );

    for round in 0..FORCES_PER_ITEM_MAX + 5 {
        let now = read(&mut connection, &item).version;
        assert_eq!(
            write(&mut connection, &item, now, &keep_mine()),
            Ok(now + 1),
            "round {round}"
        );
    }
    let records = forced(&mut connection, &item);
    assert_eq!(
        records.len(),
        FORCES_PER_ITEM_MAX,
        "the record of one item is bounded"
    );
    assert_eq!(
        records.last().unwrap().version,
        read(&mut connection, &item).version,
        "the newest records are kept, oldest first"
    );
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].version < pair[1].version)
    );
}

#[test]
fn two_connections_never_both_win_one_version() {
    let (_directory, path) = database();
    let item = Item::new("team-rules:tidepool").unwrap();
    let mut first = connect(&path);
    let mut second = Connection::open(&path).unwrap();
    second.busy_timeout(Duration::ZERO).unwrap();

    let holding = first
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(write_if(&holding, &item, 0, &agent(1)), Ok(1));
    let racing = second.transaction().unwrap();
    let lost = write_if(&racing, &item, 0, &agent(2));
    assert!(
        matches!(lost, Err(Refusal::Failed(_)) | Err(Refusal::Stale(_))),
        "a write while another holds the item never wins: {lost:?}"
    );
    drop(racing);
    holding.commit().unwrap();
    assert_eq!(
        stale(write(&mut second, &item, 0, &agent(2))),
        Stale {
            version: 0,
            current: 1,
            writer: LastWriter::Agent(AgentId::team(1).unwrap()),
        }
    );

    let racers = 8;
    let start = Arc::new(Barrier::new(racers));
    let shared = Item::new("pack:tidepool:graphify").unwrap();
    let results: Vec<Result<u64, Refusal>> = (0..racers)
        .map(|index| {
            let (path, start, shared) = (path.clone(), Arc::clone(&start), shared.clone());
            std::thread::spawn(move || {
                let mut connection = connect(&path);
                start.wait();
                write(
                    &mut connection,
                    &shared,
                    0,
                    &agent(10 + i64::try_from(index).unwrap()),
                )
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect();
    let won = results.iter().filter(|result| **result == Ok(1)).count();
    assert_eq!(won, 1, "exactly one writer wins version 1: {results:?}");
    assert!(
        results.iter().all(|result| matches!(
            result,
            Ok(1) | Err(Refusal::Stale(Stale { current: 1, .. }))
        )),
        "every other writer is told to re-read version 1: {results:?}"
    );
    assert_eq!(read(&mut first, &shared).version, 1);
}

/// A retired item moves one version past every version handed out, written or not, so every
/// holder of an earlier version is refused and re-reads, and other items keep theirs (V2V-1).
#[test]
fn a_retired_item_refuses_every_version_read_before() {
    let (_directory, path) = database();
    let mut connection = connect(&path);
    let item = agent_policy_item(&ProjectId::new("tidepool").unwrap());
    let never = instruction_settings_item(&ProjectId::new("tidepool").unwrap());
    let other = agent_policy_item(&ProjectId::new("harbor").unwrap());
    assert_eq!(write(&mut connection, &item, 0, &owner()), Ok(1));
    assert_eq!(write(&mut connection, &item, 1, &owner()), Ok(2));
    assert_eq!(write(&mut connection, &other, 0, &owner()), Ok(1));

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(retire(&tx, &item), Ok(3));
    assert_eq!(
        retire(&tx, &never),
        Ok(1),
        "an item never written still moves on"
    );
    tx.commit().unwrap();
    for held in [0, 1, 2] {
        assert_eq!(
            stale(write(&mut connection, &item, held, &owner())).current,
            3,
            "a screen holding version {held} re-reads"
        );
    }
    assert_eq!(
        stale(write(&mut connection, &never, 0, &owner())).current,
        1
    );
    assert_eq!(
        read(&mut connection, &other).version,
        1,
        "other items keep their version"
    );
    assert_eq!(write(&mut connection, &item, 3, &owner()), Ok(4));
    assert_eq!(write(&mut connection, &never, 1, &owner()), Ok(2));
}

#[test]
fn a_forgotten_item_starts_again_at_version_0_without_its_records() {
    let (_directory, path) = database();
    let mut connection = connect(&path);
    let item = Item::new("agent-instructions:tidepool:team-3").unwrap();
    let other = Item::new("agent-instructions:tidepool:team-4").unwrap();
    assert_eq!(write(&mut connection, &item, 0, &agent(3)), Ok(1));
    assert_eq!(write(&mut connection, &item, 1, &keep_mine()), Ok(2));
    assert_eq!(write(&mut connection, &other, 0, &owner()), Ok(1));

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(forget(&tx, &item), Ok(true));
    assert_eq!(
        forget(&tx, &item),
        Ok(false),
        "forgetting twice finds nothing"
    );
    tx.commit().unwrap();
    assert_eq!(
        read(&mut connection, &item),
        Current {
            version: 0,
            writer: LastWriter::Nobody,
        }
    );
    assert!(
        forced(&mut connection, &item).is_empty(),
        "its Keep mine records go with it"
    );
    assert_eq!(
        read(&mut connection, &other).version,
        1,
        "other items keep their version"
    );
    assert_eq!(
        stale(write(&mut connection, &item, 2, &owner())).current,
        0,
        "a holder of the old version re-reads"
    );
    assert_eq!(write(&mut connection, &item, 0, &owner()), Ok(1));

    let (_untouched, fresh) = database();
    let mut empty = connect(&fresh);
    let tx = empty.transaction().unwrap();
    assert_eq!(
        forget(&tx, &item),
        Ok(false),
        "a store never written has nothing to forget"
    );
    drop(tx);

    assert_eq!(
        open(Path::new("relative")).unwrap_err(),
        EngineError("versioned_unavailable".into())
    );
    let missing = tempfile::tempdir().unwrap();
    let missing = missing.path().canonicalize().unwrap();
    assert!(open(&missing).is_err(), "open never creates engine.db");
    assert!(!missing.join("engine.db").exists());
    let root = path.parent().unwrap().canonicalize().unwrap();
    let mut opened = open(&root).unwrap();
    assert_eq!(read(&mut opened, &item).version, 1);
}
