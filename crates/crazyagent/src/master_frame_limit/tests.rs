use super::*;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

#[test]
fn a_fresh_engine_keeps_the_master_frame_at_the_engine_wire_size() {
    let (_folder, connection) = engine();
    let settings = load(&connection).unwrap();
    assert_eq!(
        settings,
        MasterFrameSettings {
            limit: FrameSize::Kib64,
        }
    );
    assert_eq!(settings.limit.bytes(), crate::protocol::FRAME_BYTES_MAX);
}

#[test]
fn each_frame_option_is_saved_and_read_back_with_its_bytes() {
    let (folder, connection) = engine();
    let options = [
        (FrameSize::Kib256, "kib256", 262_144),
        (FrameSize::Mib1, "mib1", 1_048_576),
        (FrameSize::Kib64, "kib64", 65_536),
    ];
    assert_eq!(options.map(|option| option.0).len(), FrameSize::ALL.len());
    for (limit, name, bytes) in options {
        let settings = MasterFrameSettings { limit };
        assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
        assert_eq!(load(&connection).unwrap(), settings);
        assert_eq!(serde_json::to_value(limit).unwrap(), name);
        assert_eq!(limit.bytes(), bytes);
        assert!(limit.bytes() <= BYTES_MAX);
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(load(&reader).unwrap(), settings);
    }
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM configured_master_frames", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn the_options_grow_from_the_engine_wire_size_to_the_ceiling() {
    let sizes = FrameSize::ALL.map(FrameSize::bytes);
    assert_eq!(sizes.first(), Some(&crate::protocol::FRAME_BYTES_MAX));
    assert_eq!(sizes.last(), Some(&BYTES_MAX));
    assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_stored_frame_size_this_engine_does_not_know_fails_closed() {
    let (_folder, connection) = engine();
    assert!(
        connection
            .execute(
                "INSERT INTO configured_master_frames(singleton,frame_limit) VALUES(1,'mib8')",
                [],
            )
            .is_err(),
        "the table refuses a size outside the three options"
    );
    connection
        .execute_batch(
            "DROP TABLE configured_master_frames; CREATE TABLE configured_master_frames (singleton INTEGER PRIMARY KEY, frame_limit TEXT NOT NULL); INSERT INTO configured_master_frames VALUES(1,'mib8');",
        )
        .unwrap();
    assert_eq!(
        load(&connection).unwrap_err().0,
        "master_frame_settings_invalid"
    );
}
