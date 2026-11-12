use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_keeps_team_agents_running_and_ends_them_a_day_after_it_quit() {
    let (_folder, connection) = engine();
    let settings = load(&connection).unwrap();
    assert_eq!(
        settings,
        QuitSettings {
            on_quit: OnQuit::KeepRunning,
            left_running: LeftRunning::AfterHours,
            hours: 24,
        }
    );
    assert!(settings.keeps_team_agents());
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    assert_eq!(
        settings.deadline(now),
        now + Duration::from_secs(24 * 60 * 60)
    );
}

#[test]
fn each_option_is_saved_and_read_back_and_its_hours_stay_while_never_is_picked() {
    let (folder, connection) = engine();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let options = [
        (OnQuit::Stop, LeftRunning::AfterHours, 1, Some(60 * 60)),
        (OnQuit::KeepRunning, LeftRunning::Never, 1, None),
        (OnQuit::Stop, LeftRunning::Never, 168, None),
        (
            OnQuit::KeepRunning,
            LeftRunning::AfterHours,
            168,
            Some(168 * 60 * 60),
        ),
    ];
    for (on_quit, left_running, hours, ahead) in options {
        let settings = QuitSettings {
            on_quit,
            left_running,
            hours,
        };
        assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), settings);
        assert_eq!(settings.keeps_team_agents(), on_quit == OnQuit::KeepRunning);
        let deadline = settings.deadline(now).duration_since(now).unwrap();
        match ahead {
            Some(seconds) => assert_eq!(deadline, Duration::from_secs(seconds)),
            None => assert_eq!(deadline, NEVER_AHEAD),
        }
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), settings);
    }
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM agent_quit_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn hours_outside_one_to_168_are_refused_and_the_stored_setting_stays() {
    let (_folder, connection) = engine();
    let kept = QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::AfterHours,
        hours: 3,
    };
    save(&connection, &kept).unwrap();
    for hours in [0, 169, u16::MAX] {
        let refused = save(
            &connection,
            &QuitSettings {
                on_quit: OnQuit::KeepRunning,
                left_running: LeftRunning::Never,
                hours,
            },
        );
        assert_eq!(
            refused.unwrap(),
            Outcome::Rejected {
                code: "quit_settings_invalid".into()
            },
            "{hours}"
        );
        assert_eq!(load(&connection).unwrap(), kept);
    }
}

#[test]
fn a_stored_value_this_engine_does_not_know_fails_closed() {
    let (_folder, connection) = engine();
    assert!(
        connection
            .execute(
                "INSERT INTO agent_quit_settings(singleton,on_quit,left_running,hours) VALUES(1,'pause','never',1)",
                [],
            )
            .is_err(),
        "the table refuses a quit choice outside the two options"
    );
    for (on_quit, left_running, hours) in [
        ("pause", "never", 1),
        ("stop", "weekly", 1),
        ("stop", "after_hours", 0),
        ("keep_running", "after_hours", 200),
    ] {
        connection
            .execute_batch(
                "DROP TABLE agent_quit_settings; CREATE TABLE agent_quit_settings (singleton INTEGER PRIMARY KEY, on_quit TEXT NOT NULL, left_running TEXT NOT NULL, hours INTEGER NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO agent_quit_settings VALUES(1,?1,?2,?3)",
                rusqlite::params![on_quit, left_running, hours],
            )
            .unwrap();
        assert_eq!(
            load(&connection).unwrap_err().0,
            "quit_settings_invalid",
            "{on_quit} {left_running} {hours}"
        );
    }
}
