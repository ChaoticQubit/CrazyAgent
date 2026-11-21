use crate::protocol::{Command, Outcome};
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::settings_log::tests::{moved, store};
use rusqlite::Connection;

/// The quit settings the rows save: stop, never, 12 hours.
fn stop_never_12() -> QuitSettings {
    QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    }
}

/// Every logged event's aggregate, kind, schema and body, in seq order. Input: the store.
/// Output: the rows.
fn logged(db: &Connection) -> Vec<(String, String, i64, Vec<u8>)> {
    db.prepare("SELECT aggregate, kind, schema, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn the_logged_quit_event_keeps_its_kind_schema_and_body() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let command = Command::OwnerAgentQuit {
        settings: stop_never_12(),
    };
    assert_eq!(
        crate::settings_log::owner(&tx, &command, revision).unwrap(),
        Outcome::Applied
    );
    assert_eq!(moved(&tx).0, before.0 + 1);
    assert_eq!(
        logged(&tx),
        vec![(
            "settings".to_owned(),
            "AgentQuitSaved".to_owned(),
            1,
            br#"{"settings":{"on_quit":"stop","left_running":"never","hours":12}}"#.to_vec(),
        )]
    );
}
