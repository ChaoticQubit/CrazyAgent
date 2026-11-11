use super::*;
use crate::protocol::ProjectId;
use crate::roadmap_status::Refresh;
use rusqlite::Connection;

const FEATURES: &str = ".crazyagent/overview/features.md";

fn case() -> (Connection, ProjectId, tempfile::TempDir) {
    let db = Connection::open_in_memory().unwrap();
    crate::roadmap_status::initialize(&db).unwrap();
    initialize(&db).unwrap();
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(folder.path().join(".crazyagent")).unwrap();
    (db, ProjectId::new("alpha").unwrap(), folder)
}

fn tab_of(overview: &Overview, tab: Tab) -> &TabState {
    overview.tabs.iter().find(|state| state.tab == tab).unwrap()
}

fn features(db: &Connection, project: &ProjectId, folder: &std::path::Path) -> TabState {
    tab_of(&read(db, project, folder, 1).unwrap(), Tab::Features).clone()
}

#[test]
fn the_first_offer_writes_the_tab_file_and_it_reads_back_unedited() {
    let (db, project, folder) = case();
    assert_eq!(
        offer(
            &db,
            &project,
            folder.path(),
            Tab::Features,
            "- Search papers\n"
        )
        .unwrap(),
        Offered::Written
    );
    let found = features(&db, &project, folder.path());
    assert_eq!(found.text.as_deref(), Some("- Search papers\n"));
    assert!(!found.edited);
    assert!(found.proposal.is_none());
    assert!(folder.path().join(FEATURES).is_file());
}

#[test]
fn a_later_offer_is_a_proposal_and_never_touches_the_file() {
    let (db, project, folder) = case();
    offer(
        &db,
        &project,
        folder.path(),
        Tab::Features,
        "- Search papers\n",
    )
    .unwrap();
    assert_eq!(
        offer(
            &db,
            &project,
            folder.path(),
            Tab::Features,
            "- Search papers\n- Share lists\n"
        )
        .unwrap(),
        Offered::Proposed
    );
    let found = features(&db, &project, folder.path());
    assert_eq!(found.text.as_deref(), Some("- Search papers\n"));
    assert_eq!(
        found.proposal.as_deref(),
        Some("- Search papers\n- Share lists\n")
    );
}

#[test]
fn an_owner_edit_survives_a_refresh_and_shows_as_edited() {
    let (db, project, folder) = case();
    offer(
        &db,
        &project,
        folder.path(),
        Tab::Features,
        "- Search papers\n",
    )
    .unwrap();
    let version = features(&db, &project, folder.path()).version;
    save(
        &db,
        &project,
        folder.path(),
        Tab::Features,
        "- Search papers fast\n",
        &version,
    )
    .unwrap();
    offer(&db, &project, folder.path(), Tab::Features, "- Search\n").unwrap();
    let found = features(&db, &project, folder.path());
    assert_eq!(found.text.as_deref(), Some("- Search papers fast\n"));
    assert!(found.edited);
    assert_eq!(found.proposal.as_deref(), Some("- Search\n"));
}

#[test]
fn accept_writes_the_proposal_only_at_the_version_the_owner_saw() {
    let (db, project, folder) = case();
    offer(&db, &project, folder.path(), Tab::Features, "- A\n").unwrap();
    offer(&db, &project, folder.path(), Tab::Features, "- B\n").unwrap();
    let seen = features(&db, &project, folder.path()).version;
    std::fs::write(folder.path().join(FEATURES), "- A edited elsewhere\n").unwrap();
    assert_eq!(
        accept(&db, &project, folder.path(), Tab::Features, &seen)
            .unwrap_err()
            .0,
        TAB_CHANGED
    );
    let now = features(&db, &project, folder.path());
    assert_eq!(now.text.as_deref(), Some("- A edited elsewhere\n"));
    assert_eq!(now.proposal.as_deref(), Some("- B\n"));
    accept(&db, &project, folder.path(), Tab::Features, &now.version).unwrap();
    let done = features(&db, &project, folder.path());
    assert_eq!(done.text.as_deref(), Some("- B\n"));
    assert!(done.proposal.is_none());
    assert!(!done.edited);
    assert_eq!(
        accept(&db, &project, folder.path(), Tab::Features, &done.version)
            .unwrap_err()
            .0,
        NO_PROPOSAL
    );
}

#[test]
fn reject_drops_the_proposal_and_an_equal_offer_is_dropped_too() {
    let (db, project, folder) = case();
    offer(&db, &project, folder.path(), Tab::Features, "- A\n").unwrap();
    offer(&db, &project, folder.path(), Tab::Features, "- B\n").unwrap();
    reject(&db, &project, Tab::Features).unwrap();
    assert!(features(&db, &project, folder.path()).proposal.is_none());
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, "- A\n").unwrap(),
        Offered::Same
    );
}

#[test]
fn apply_when_unedited_writes_an_unedited_tab_and_still_asks_for_an_edited_one() {
    let (db, project, folder) = case();
    set_apply(&db, &project, Apply::WhenUnedited).unwrap();
    offer(&db, &project, folder.path(), Tab::Features, "- A\n").unwrap();
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, "- B\n").unwrap(),
        Offered::Written
    );
    std::fs::write(folder.path().join(FEATURES), "- mine\n").unwrap();
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, "- C\n").unwrap(),
        Offered::Proposed
    );
    assert_eq!(
        std::fs::read_to_string(folder.path().join(FEATURES)).unwrap(),
        "- mine\n"
    );
}

#[cfg(unix)]
#[test]
fn a_tab_file_that_is_a_link_is_neither_read_nor_replaced() {
    let (db, project, folder) = case();
    let outside = folder.path().join("outside.md");
    std::fs::write(&outside, "secret\n").unwrap();
    std::fs::create_dir_all(folder.path().join(".crazyagent/overview")).unwrap();
    std::os::unix::fs::symlink(&outside, folder.path().join(FEATURES)).unwrap();
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, "- A\n")
            .unwrap_err()
            .0,
        TAB_BLOCKED
    );
    assert_eq!(
        save(&db, &project, folder.path(), Tab::Features, "- A\n", "")
            .unwrap_err()
            .0,
        TAB_BLOCKED
    );
    assert!(features(&db, &project, folder.path()).text.is_none());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret\n");
    assert!(
        std::fs::symlink_metadata(folder.path().join(FEATURES))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn a_tabs_folder_that_is_a_link_is_neither_read_nor_written_through() {
    let (db, project, folder) = case();
    let outside = folder.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("features.md"), "- theirs\n").unwrap();
    std::os::unix::fs::symlink(&outside, folder.path().join(".crazyagent/overview")).unwrap();
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, "- A\n")
            .unwrap_err()
            .0,
        TAB_BLOCKED
    );
    assert!(features(&db, &project, folder.path()).text.is_none());
    assert_eq!(
        std::fs::read_to_string(outside.join("features.md")).unwrap(),
        "- theirs\n"
    );
}

#[test]
fn a_text_over_the_bound_is_refused_not_cut() {
    let (db, project, folder) = case();
    let long = "x".repeat(TAB_BYTES_MAX + 1);
    assert_eq!(
        save(&db, &project, folder.path(), Tab::Features, &long, "")
            .unwrap_err()
            .0,
        TAB_TOO_LARGE
    );
    assert_eq!(
        offer(&db, &project, folder.path(), Tab::Features, &long)
            .unwrap_err()
            .0,
        TAB_TOO_LARGE
    );
    assert_eq!(
        save(&db, &project, folder.path(), Tab::Features, " \n", "")
            .unwrap_err()
            .0,
        TAB_EMPTY
    );
    let full = "x".repeat(TAB_BYTES_MAX);
    save(&db, &project, folder.path(), Tab::Features, &full, "").unwrap();
    assert_eq!(
        features(&db, &project, folder.path()).text.as_deref(),
        Some(full.as_str())
    );
}

#[test]
fn a_file_that_is_too_long_or_not_text_is_left_alone_and_never_cut() {
    let (db, project, folder) = case();
    std::fs::create_dir_all(folder.path().join(".crazyagent/overview")).unwrap();
    let long = "y".repeat(TAB_BYTES_MAX + 1);
    for body in [long.as_bytes(), &[0xff, 0xfe, 0x00][..], &b"  \n"[..]] {
        std::fs::write(folder.path().join(FEATURES), body).unwrap();
        assert!(features(&db, &project, folder.path()).text.is_none());
        assert_eq!(
            offer(&db, &project, folder.path(), Tab::Features, "- A\n")
                .unwrap_err()
                .0,
            TAB_BLOCKED
        );
        assert_eq!(std::fs::read(folder.path().join(FEATURES)).unwrap(), body);
    }
}

#[test]
fn a_landed_set_marks_the_overview_stale_only_when_refresh_follows_landing() {
    let (mut db, project, _folder) = case();
    set_refresh(&db, &project, Refresh::RefreshOnly).unwrap();
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &project, 1_000).unwrap();
    tx.commit().unwrap();
    assert!(!state(&db, &project).unwrap().stale);
    set_refresh(&db, &project, Refresh::AfterLanding).unwrap();
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &project, 2_000).unwrap();
    tx.commit().unwrap();
    assert!(state(&db, &project).unwrap().stale);
    let due: i64 = db
        .query_row(
            "SELECT due_ms FROM project_overview WHERE project='alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(due, 2_000 + crate::roadmap_status::QUIET_MS);
}

#[test]
fn the_first_read_asks_for_the_first_writing_run_and_lists_the_four_tabs_in_order() {
    let (db, project, folder) = case();
    assert!(!state(&db, &project).unwrap().stale);
    let overview = read(&db, &project, folder.path(), 1).unwrap();
    assert!(overview.stale);
    assert!(!overview.running);
    assert_eq!(overview.refresh, Refresh::AfterLanding);
    assert_eq!(overview.apply, Apply::AlwaysAsk);
    assert!(overview.readme.is_none());
    assert_eq!(overview.competitors, Competitors::Missing);
    assert_eq!(
        overview.tabs.iter().map(|tab| tab.tab).collect::<Vec<_>>(),
        Tab::ALL
    );
    assert!(
        overview
            .tabs
            .iter()
            .all(|tab| tab.text.is_none() && tab.version.is_empty() && !tab.edited)
    );
    db.execute("UPDATE project_overview SET stale=0", [])
        .unwrap();
    assert!(!read(&db, &project, folder.path(), 2).unwrap().stale);
    refresh(&db, &project).unwrap();
    assert!(state(&db, &project).unwrap().stale);
    set_refresh(&db, &project, Refresh::Off).unwrap();
    assert_eq!(refresh(&db, &project).unwrap_err().0, OFF);
}

#[test]
fn an_overview_goes_over_the_wire_in_the_shape_the_desktop_reads() {
    let (db, project, folder) = case();
    offer(&db, &project, folder.path(), Tab::InstallAndUse, "npm i\n").unwrap();
    let overview = read(&db, &project, folder.path(), 1).unwrap();
    let wire = serde_json::to_value(&overview).unwrap();
    assert_eq!(wire["readme"], serde_json::Value::Null);
    assert_eq!(
        wire["competitors"],
        serde_json::json!({ "kind": "missing" })
    );
    assert_eq!(wire["refresh"], "after_landing");
    assert_eq!(wire["apply"], "always_ask");
    assert_eq!(wire["tabs"][3]["tab"], "install_and_use");
    assert_eq!(wire["tabs"][3]["text"], "npm i\n");
    assert_eq!(wire["tabs"][3]["edited"], false);
    assert_eq!(wire["tabs"][0]["tab"], "quick_info");
    assert_eq!(wire["tabs"][0]["version"], "");
    assert_eq!(
        serde_json::to_value(Competitors::Linked {
            paths: vec!["docs/research-competitors.md".to_owned()]
        })
        .unwrap(),
        serde_json::json!({ "kind": "linked", "paths": ["docs/research-competitors.md"] })
    );
    assert_eq!(serde_json::from_value::<Overview>(wire).unwrap(), overview);
    assert!(
        folder
            .path()
            .join(".crazyagent/overview/install-and-use.md")
            .is_file()
    );
}
