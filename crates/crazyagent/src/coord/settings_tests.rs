use super::*;

const PROJECT: &str = "coord-reminder";

/// An engine database holding one project. Output: the connection and the project.
fn db() -> (Connection, ProjectId) {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let row = crate::protocol::StoredProject {
        project_id: project.clone(),
        spec: crate::protocol::ProjectSpec {
            name: PROJECT.into(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![PROJECT, serde_json::to_string(&row).unwrap()],
        )
        .unwrap();
    (connection, project)
}

/// The stored name of a settings' Uncommitted edits reminder. Input: the settings. Output:
/// the name as JSON, null when the settings have none.
fn reminder(settings: &CoordSettings) -> serde_json::Value {
    serde_json::to_value(settings).unwrap()["uncommitted_reminder"].clone()
}

#[test]
fn the_uncommitted_edits_reminder_is_on_by_default_and_for_older_rows_and_off_round_trips() {
    let (connection, project) = db();
    assert_eq!(reminder(&CoordSettings::default()), "on");
    assert_eq!(reminder(&read(&connection, &project).unwrap()), "on");
    let mut value = serde_json::to_value(CoordSettings::default()).unwrap();
    value["uncommitted_reminder"] = "off".into();
    let off: CoordSettings = serde_json::from_value(value).unwrap();
    off.validate().unwrap();
    write(&connection, &project, &off).unwrap();
    assert_eq!(read(&connection, &project).unwrap(), off);
    assert_eq!(reminder(&off), "off");
    connection
        .execute(
            "UPDATE coord_settings SET json=?2 WHERE project=?1",
            params![
                PROJECT,
                r#"{"questions_to":"lead","overlaps":"warn","nudges":"off","counted_tools":[]}"#
            ],
        )
        .unwrap();
    assert_eq!(
        reminder(&read(&connection, &project).unwrap()),
        "on",
        "a row saved before the setting existed reads On"
    );
}
