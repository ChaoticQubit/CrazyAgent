use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_gives_the_master_no_step_limit() {
    let (_folder, connection) = engine();
    let settings = load(&connection).unwrap();
    assert_eq!(
        settings,
        MasterStepSettings {
            limit: StepLimit::NoLimit,
        }
    );
    assert_eq!(settings.limit.steps(), None);
    assert_eq!(settings.limit.cause(), None);
}

#[test]
fn each_option_is_saved_and_read_back_with_its_steps_and_its_cause() {
    let (folder, connection) = engine();
    let options = [
        (StepLimit::Steps50, "steps50", Some(50), "50"),
        (StepLimit::Steps100, "steps100", Some(100), "100"),
        (StepLimit::Steps200, "steps200", Some(200), "200"),
        (StepLimit::NoLimit, "no_limit", None, ""),
    ];
    assert_eq!(options.map(|option| option.0).len(), StepLimit::ALL.len());
    for (limit, name, steps, argument) in options {
        let settings = MasterStepSettings { limit };
        assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), settings);
        assert_eq!(serde_json::to_value(limit).unwrap(), name);
        assert_eq!(limit.steps(), steps);
        match steps {
            Some(steps) => {
                assert_eq!(StepLimit::of_steps(argument), Some(limit));
                let cause = limit.cause().unwrap();
                assert_eq!(cause, format!("master_step_limit_{steps}"));
                assert_eq!(steps_of_cause(&cause), Some(steps));
            }
            None => assert_eq!(limit.cause(), None),
        }
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), settings);
    }
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM configured_master_steps", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn a_step_count_the_owner_cannot_pick_names_no_limit_and_no_cause() {
    for text in [
        "", "0", "1", "24", "050", "50 ", "51", "99", "201", "-50", "no_limit",
    ] {
        assert_eq!(StepLimit::of_steps(text), None, "{text:?}");
        assert_eq!(
            steps_of_cause(&format!("master_step_limit_{text}")),
            None,
            "{text:?}"
        );
    }
    assert_eq!(steps_of_cause("master_capture_incomplete"), None);
    assert_eq!(steps_of_cause("master_step_limit"), None);
}

#[test]
fn a_stored_value_this_engine_does_not_know_fails_closed() {
    let (_folder, connection) = engine();
    assert!(
        connection
            .execute(
                "INSERT INTO configured_master_steps(singleton,step_limit) VALUES(1,'steps24')",
                [],
            )
            .is_err(),
        "the table refuses a limit outside the four options"
    );
    connection
        .execute_batch(
            "DROP TABLE configured_master_steps; CREATE TABLE configured_master_steps (singleton INTEGER PRIMARY KEY, step_limit TEXT NOT NULL); INSERT INTO configured_master_steps VALUES(1,'steps24');",
        )
        .unwrap();
    assert_eq!(
        load(&connection).unwrap_err().0,
        "master_step_settings_invalid"
    );
}
