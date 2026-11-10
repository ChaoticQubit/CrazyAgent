use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_removes_a_finished_agent_s_worktree_one_day_after_it_finished() {
    let (_folder, connection) = engine();
    let settings = load(&connection).unwrap();
    assert_eq!(
        settings,
        CleanupSettings {
            mode: CleanupMode::AfterDays,
            days: 1,
        }
    );
    assert_eq!(settings.wait_ms(), Some(24 * 60 * 60 * 1000));
}

#[test]
fn each_option_is_saved_and_read_back_and_its_days_stay_while_another_is_picked() {
    let (folder, connection) = engine();
    let options = [
        (CleanupMode::AfterDays, 7, Some(7 * 24 * 60 * 60 * 1000)),
        (CleanupMode::OnceLanded, 7, Some(0)),
        (CleanupMode::Manual, 7, None),
        (CleanupMode::AfterDays, 365, Some(365 * 24 * 60 * 60 * 1000)),
    ];
    for (mode, days, wait) in options {
        let settings = CleanupSettings { mode, days };
        assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), settings);
        assert_eq!(load(&connection).unwrap().wait_ms(), wait, "{mode:?}");
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), settings);
    }
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM agent_cleanup_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn days_outside_one_to_365_are_refused_and_the_stored_setting_stays() {
    let (_folder, connection) = engine();
    let kept = CleanupSettings {
        mode: CleanupMode::OnceLanded,
        days: 3,
    };
    save(&connection, &kept).unwrap();
    for days in [0, 366, u16::MAX] {
        let refused = save(
            &connection,
            &CleanupSettings {
                mode: CleanupMode::AfterDays,
                days,
            },
        );
        assert_eq!(refused.unwrap_err().0, "cleanup_settings_invalid", "{days}");
        assert_eq!(load(&connection).unwrap(), kept);
    }
}

#[test]
fn a_stored_value_this_engine_does_not_know_fails_closed() {
    let (_folder, connection) = engine();
    assert!(
        connection
            .execute(
                "INSERT INTO agent_cleanup_settings(singleton,mode,days) VALUES(1,'weekly',1)",
                [],
            )
            .is_err(),
        "the table refuses a mode outside the three options"
    );
    for (mode, days) in [("hourly", 1), ("after_days", 0), ("after_days", 400)] {
        connection
            .execute_batch(
                "DROP TABLE agent_cleanup_settings; CREATE TABLE agent_cleanup_settings (singleton INTEGER PRIMARY KEY, mode TEXT NOT NULL, days INTEGER NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO agent_cleanup_settings VALUES(1,?1,?2)",
                rusqlite::params![mode, days],
            )
            .unwrap();
        assert_eq!(
            load(&connection).unwrap_err().0,
            "cleanup_settings_invalid",
            "{mode} {days}"
        );
    }
}
