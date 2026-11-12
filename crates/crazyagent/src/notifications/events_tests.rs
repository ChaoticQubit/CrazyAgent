use super::events::{
    engine_started, engine_stopped, harness_update, master_moved, master_off,
};
use super::*;
use crate::protocol::CredentialRef;
use crate::usage::poll::Provider;
use crate::usage::{LimitSource, LimitWindow, Observation};
use rusqlite::Connection;

const NOW_MS: i64 = 1_800_000_000_000;
const RESETS_MS: i64 = NOW_MS + 3 * 60 * 60 * 1000;
const NEXT_RESETS_MS: i64 = RESETS_MS + 5 * 60 * 60 * 1000;

fn store() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    crate::usage::initialize(&connection).unwrap();
    (folder, connection)
}

fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

fn account(id: &str) -> CredentialRef {
    CredentialRef::new(id).unwrap()
}

fn items(connection: &Connection) -> Vec<Notice> {
    page(
        connection,
        &NoticeQuery {
            filter: NoticeFilter::All,
            project_id: None,
            kind: None,
            fold: false,
            before: None,
            after: None,
            limit: PAGE_MAX,
        },
    )
    .unwrap()
    .items
}

fn texts(connection: &Connection) -> Vec<(NoticeKind, String)> {
    items(connection)
        .into_iter()
        .map(|item| (item.kind, item.text))
        .collect()
}

fn polled(connection: &Connection, five_hour: f64, weekly: f64, resets_ms: i64, now: i64) {
    let observation = Observation {
        plan: None,
        five_hour: Some(LimitWindow {
            used_percent: five_hour,
            resets_at_ms: Some(resets_ms),
        }),
        weekly: Some(LimitWindow {
            used_percent: weekly,
            resets_at_ms: Some(resets_ms + 6 * 24 * 60 * 60 * 1000),
        }),
        provider: Some(Provider::Claude),
    };
    crate::usage::record_observation(
        connection,
        &account("work"),
        &observation,
        LimitSource::Poll,
        now,
    )
    .unwrap();
}

#[test]
fn the_first_engine_start_records_nothing() {
    let (_folder, connection) = store();
    engine_started(&connection, NOW_MS);
    assert!(items(&connection).is_empty());
}

#[test]
fn an_engine_start_after_an_engine_that_never_stopped_records_one_restart_item() {
    let (_folder, connection) = store();
    engine_started(&connection, NOW_MS);
    engine_started(&connection, NOW_MS + 1_000);
    let found = items(&connection);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, NoticeKind::EngineRestarted);
    assert_eq!(found[0].class, NoticeClass::Info);
    assert_eq!(
        found[0].text,
        "The CrazyAgent engine stopped without closing and started again."
    );
    assert_eq!(found[0].target, NoticeTarget::Home);
}

#[test]
fn an_engine_start_after_a_clean_stop_records_nothing() {
    let (_folder, mut connection) = store();
    engine_started(&connection, NOW_MS);
    let tx = connection.transaction().unwrap();
    engine_stopped(&tx).unwrap();
    tx.commit().unwrap();
    engine_started(&connection, NOW_MS + 1_000);
    assert!(items(&connection).is_empty());
}

#[test]
fn a_live_master_the_engine_turned_off_records_its_line_on_the_conversation_page() {
    let (_folder, connection) = store();
    master_off(
        &connection,
        &project("p1"),
        "The live master was turned off: it was idle for 30 minutes.",
        NOW_MS,
    );
    let found = items(&connection);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, NoticeKind::MasterOff);
    assert_eq!(found[0].project_id, Some(project("p1")));
    assert_eq!(
        found[0].text,
        "The live master was turned off: it was idle for 30 minutes."
    );
    assert_eq!(
        found[0].target,
        NoticeTarget::Conversation {
            project_id: project("p1")
        }
    );
}

#[test]
fn a_live_master_moved_to_another_pool_account_records_a_failover_naming_both() {
    let (_folder, connection) = store();
    master_moved(&connection, &project("p1"), "Work Claude", "Home Claude", NOW_MS);
    let found = items(&connection);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, NoticeKind::PoolFailover);
    assert_eq!(
        found[0].text,
        "The live master moved from Work Claude to Home Claude in this project's pool."
    );
    assert_eq!(
        found[0].target,
        NoticeTarget::Conversation {
            project_id: project("p1")
        }
    );
}

#[test]
fn a_poll_that_crosses_ninety_percent_records_one_near_limit_item() {
    let (_folder, connection) = store();
    polled(&connection, 40.0, 10.0, RESETS_MS, NOW_MS);
    polled(&connection, 92.0, 10.0, RESETS_MS, NOW_MS + 1);
    polled(&connection, 95.0, 10.0, RESETS_MS, NOW_MS + 2);
    assert_eq!(
        texts(&connection),
        [(
            NoticeKind::AccountNearLimit,
            "An account is near its 5-hour limit: 92% used.".to_owned()
        )]
    );
    assert_eq!(items(&connection)[0].target, NoticeTarget::Usage);
}

#[test]
fn a_poll_that_reaches_the_limit_records_an_at_limit_item_and_no_near_item() {
    let (_folder, connection) = store();
    polled(&connection, 40.0, 10.0, RESETS_MS, NOW_MS);
    polled(&connection, 40.0, 100.0, RESETS_MS, NOW_MS + 1);
    assert_eq!(
        texts(&connection),
        [(
            NoticeKind::AccountAtLimit,
            "An account reached its weekly limit.".to_owned()
        )]
    );
}

#[test]
fn the_first_poll_of_an_account_already_near_its_limit_records_it() {
    let (_folder, connection) = store();
    polled(&connection, 91.0, 10.0, RESETS_MS, NOW_MS);
    assert_eq!(
        texts(&connection),
        [(
            NoticeKind::AccountNearLimit,
            "An account is near its 5-hour limit: 91% used.".to_owned()
        )]
    );
}

#[test]
fn a_new_window_that_crosses_again_records_a_new_item() {
    let (_folder, connection) = store();
    polled(&connection, 92.0, 10.0, RESETS_MS, NOW_MS);
    polled(&connection, 5.0, 10.0, NEXT_RESETS_MS, NOW_MS + 1);
    polled(&connection, 93.0, 10.0, NEXT_RESETS_MS, NOW_MS + 2);
    assert_eq!(
        texts(&connection),
        [
            (
                NoticeKind::AccountNearLimit,
                "An account is near its 5-hour limit: 93% used.".to_owned()
            ),
            (
                NoticeKind::AccountNearLimit,
                "An account is near its 5-hour limit: 92% used.".to_owned()
            ),
        ]
    );
}

#[test]
fn a_newer_harness_version_is_recorded_once_per_version() {
    let (_folder, connection) = store();
    harness_update(&connection, "claude", "2.2.0", NOW_MS);
    harness_update(&connection, "claude", "2.2.0", NOW_MS + 1);
    harness_update(&connection, "claude", "2.3.0", NOW_MS + 2);
    let found = items(&connection);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, NoticeKind::HarnessUpdate);
    assert_eq!(found[0].text, "Claude Code 2.3.0 is available.");
    assert_eq!(found[0].target, NoticeTarget::Harnesses);
    assert_eq!(found[1].text, "Claude Code 2.2.0 is available.");
}

#[test]
fn an_unknown_harness_or_a_version_that_is_not_plain_records_nothing() {
    let (_folder, connection) = store();
    harness_update(&connection, "not-a-harness", "1.0.0", NOW_MS);
    harness_update(&connection, "claude", "1.0.0; rm", NOW_MS);
    assert!(items(&connection).is_empty());
}
