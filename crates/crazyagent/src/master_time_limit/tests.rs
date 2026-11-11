use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_gives_the_master_no_time_limit() {
    let (_folder, connection) = engine();
    let settings = load(&connection).unwrap();
    assert_eq!(
        settings,
        MasterTimeSettings {
            limit: TimeLimit::NoLimit,
        }
    );
    assert_eq!(settings.limit.minutes(), None);
    assert_eq!(settings.limit.run_time(), None);
    assert_eq!(settings.limit.cause(), None);
}

#[test]
fn each_option_is_saved_and_read_back_with_its_clock_and_its_cause() {
    let (folder, connection) = engine();
    let options = [
        (TimeLimit::Minutes10, "minutes10", Some(10)),
        (TimeLimit::Minutes30, "minutes30", Some(30)),
        (TimeLimit::Minutes60, "minutes60", Some(60)),
        (TimeLimit::NoLimit, "no_limit", None),
    ];
    assert_eq!(options.map(|option| option.0).len(), TimeLimit::ALL.len());
    for (limit, name, minutes) in options {
        let settings = MasterTimeSettings { limit };
        assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), settings);
        assert_eq!(serde_json::to_value(limit).unwrap(), name);
        assert_eq!(limit.minutes(), minutes);
        match minutes {
            Some(minutes) => {
                let clock = Duration::from_secs(u64::from(minutes) * 60);
                assert_eq!(limit.run_time(), Some(clock));
                let cause = limit.cause().unwrap();
                assert_eq!(cause, format!("master_time_limit_{minutes}"));
                assert_eq!(minutes_of_cause(&cause), Some(minutes));
            }
            None => {
                assert_eq!(limit.run_time(), None);
                assert_eq!(limit.cause(), None);
            }
        }
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), settings);
    }
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM configured_master_time", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn the_longest_limit_is_the_longest_clock_a_host_takes() {
    let longest = TimeLimit::ALL
        .into_iter()
        .filter_map(TimeLimit::run_time)
        .max();
    assert_eq!(longest, Some(RUN_TIME_MAX));
    assert_eq!(TimeLimit::default(), TimeLimit::NoLimit);
    assert_eq!(TimeLimit::ALL[0], TimeLimit::NoLimit);
}

#[test]
fn minutes_the_owner_cannot_pick_name_no_cause() {
    for text in [
        "", "0", "1", "9", "010", "10 ", "11", "20", "61", "600", "-10", "no_limit",
    ] {
        assert_eq!(
            minutes_of_cause(&format!("master_time_limit_{text}")),
            None,
            "{text:?}"
        );
    }
    assert_eq!(minutes_of_cause("claude_host_deadline"), None);
    assert_eq!(minutes_of_cause("master_time_limit"), None);
    assert_eq!(minutes_of_cause("master_step_limit_50"), None);
}

#[test]
fn a_stored_value_this_engine_does_not_know_fails_closed() {
    let (_folder, connection) = engine();
    assert!(
        connection
            .execute(
                "INSERT INTO configured_master_time(singleton,time_limit) VALUES(1,'minutes5')",
                [],
            )
            .is_err(),
        "the table refuses a limit outside the four options"
    );
    connection
        .execute_batch(
            "DROP TABLE configured_master_time; CREATE TABLE configured_master_time (singleton INTEGER PRIMARY KEY, time_limit TEXT NOT NULL); INSERT INTO configured_master_time VALUES(1,'minutes5');",
        )
        .unwrap();
    assert_eq!(
        load(&connection).unwrap_err().0,
        "master_time_settings_invalid"
    );
}
