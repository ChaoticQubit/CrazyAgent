use super::super::tests::{NOW, frame, press, project, team};
use super::*;

const LABEL: &str = "Uncommitted edits reminder";

#[test]
fn the_uncommitted_edits_reminder_shows_on_by_default_last_and_space_turns_it_off() {
    let folder = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let coordination = crate::team::coordination(&connection, &project()).unwrap();
    let mut view = TeamView::new(project());
    let query = view.next_query(NOW).unwrap();
    let mut stored = team();
    stored.coordination = Some(coordination.clone());
    view.apply(&query, Ok(SetupView::Team(Box::new(stored))));
    assert_eq!(view.key(press(KeyCode::Char('c'))), TeamReply::Handled);
    let shown = frame(&view, 120, 60);
    let row = shown
        .iter()
        .find(|row| row.contains(LABEL))
        .unwrap_or_else(|| panic!("no {LABEL} row:\n{}", shown.join("\n")));
    assert!(row.contains("On"), "{row}");
    for _ in 0..COORD_ROWS.len() {
        view.key(press(KeyCode::Down));
    }
    let mut value = serde_json::to_value(&coordination.settings).unwrap();
    value["uncommitted_reminder"] = "off".into();
    let off: crate::coord::settings::CoordSettings = serde_json::from_value(value).unwrap();
    assert_eq!(
        view.key(press(KeyCode::Char(' '))),
        TeamReply::Change(TeamAction::CoordSettings {
            project_id: project(),
            settings: off,
        })
    );
}
