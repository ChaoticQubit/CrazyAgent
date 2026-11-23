use super::*;
use crate::settings_log::{Event, Saved, apply};

#[test]
fn log_compaction_defaults_to_off_and_round_trips() {
    let db = Connection::open_in_memory().unwrap();
    assert_eq!(load(&db).unwrap(), LogCompaction::Off, "no table reads Off");
    initialize(&db).unwrap();
    assert_eq!(load(&db).unwrap(), LogCompaction::Off, "no row reads Off");

    for choice in [LogCompaction::On, LogCompaction::Off, LogCompaction::On] {
        apply(&db, &Event::Compaction(Saved { settings: choice })).unwrap();
        assert_eq!(load(&db).unwrap(), choice);
    }
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM log_compaction_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
    assert!(
        db.execute(
            "UPDATE log_compaction_settings SET choice='sometimes' WHERE singleton=1",
            []
        )
        .is_err(),
        "the table refuses a word it does not list"
    );
}

#[test]
fn dev_build_reads_the_dev_suffix() {
    assert!(dev_build("2.5.0-dev.202611211700"));
    assert!(!dev_build("2.5.0"));
    assert!(!dev_build("2.4.4"));
    assert!(!dev_build("2.5.0-rc.1"));
}

#[test]
fn plugins_compaction_stays_off_with_the_switch_on() {
    assert_eq!(
        compaction_for(Aggregate::Plugins, true, LogCompaction::On),
        Compaction::Off
    );
    assert_eq!(
        compaction_for(Aggregate::Settings, true, LogCompaction::On),
        Compaction::On
    );
    assert_eq!(
        compaction_for(Aggregate::Settings, false, LogCompaction::On),
        Compaction::Off,
        "a release build never compacts, whatever a dev build saved"
    );
    assert_eq!(
        compaction_for(Aggregate::Worktrees, true, LogCompaction::Off),
        Compaction::Off
    );
}
