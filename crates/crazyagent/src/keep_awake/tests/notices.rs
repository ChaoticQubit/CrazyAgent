use super::*;
use crate::config::ModelAccess;
use crate::notifications::{
    Notice, NoticeClass, NoticeFilter, NoticeKind, NoticeQuery, NoticeTarget, PAGE_MAX,
};

const DAY_MS: i64 = 24 * HOUR_MS;
const NOT_PUT_BACK: &str = "Keep awake could not put back a sleep setting it changed. It tries \
                            again when the engine starts.";
const HOT_UNTIL_CRITICAL: &str =
    "This computer is running hot. Keep awake goes on and ends if the heat gets critical.";
const HOT_NEVER_ENDS: &str =
    "This computer is running hot. Keep awake goes on; it is set never to end for heat.";
const LOST: &str = "Keep awake ended: the system did not keep this computer awake.";

/// The store's items of one kind under one filter, newest first, each checked to open the
/// Keep awake page and to belong to no project.
fn listed(connection: &Connection, kind: NoticeKind, filter: NoticeFilter) -> Vec<Notice> {
    let query = NoticeQuery {
        filter,
        project_id: None,
        kind: Some(kind),
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    };
    let page = crate::notifications::page(connection, &query).unwrap();
    for item in &page.items {
        assert_eq!(item.target, NoticeTarget::KeepAwake);
        assert_eq!(item.project_id, None);
        assert_eq!(item.class, kind.class());
    }
    page.items
}

fn texts(connection: &Connection, kind: NoticeKind) -> Vec<String> {
    listed(connection, kind, NoticeFilter::All)
        .into_iter()
        .map(|item| item.text)
        .collect()
}

fn ended(connection: &Connection) -> Vec<String> {
    texts(connection, NoticeKind::KeepAwakeEnded)
}

fn hot(connection: &Connection) -> Vec<String> {
    texts(connection, NoticeKind::KeepAwakeHeat)
}

fn on_battery(pct: u8) -> Sensors {
    sensors(PowerSource::Battery, Some(pct))
}

fn warm(heat: Heat) -> Sensors {
    Sensors {
        heat,
        ..sensors(PowerSource::Ac, Some(90))
    }
}

fn notices_reconciled(connection: &mut Connection, now_ms: i64) {
    crate::notifications::reconcile::reconcile(connection, &ModelAccess::default(), now_ms)
        .unwrap();
}

fn pending_restores(connection: &Connection) -> Vec<String> {
    listed(
        connection,
        NoticeKind::KeepAwakeRestore,
        NoticeFilter::Pending,
    )
    .into_iter()
    .map(|item| item.text)
    .collect()
}

/// A platform hold the system never gives.
#[derive(Debug)]
struct Refusing;

impl Hold for Refusing {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        if *want == Want::Nothing {
            return Ok(Held {
                idle: false,
                lid: false,
                display: false,
            });
        }
        Err(HoldError::Unavailable("refused in this row".into()))
    }

    fn release(&mut self) {}
}

/// Plan Task 7 smallest real check.
#[test]
fn a_battery_ending_raises_one_info_notice_and_a_failed_restore_stays_pending_until_fixed() {
    let (_folder, mut connection) = engine();
    agent(&connection, "working");
    let held = evaluate(&connection, NOW_MS, Some(&on_battery(50))).unwrap();
    assert_ne!(held.state.want, Want::Nothing);
    assert!(ended(&connection).is_empty());

    let low = evaluate(&connection, NOW_MS + SECOND_MS, Some(&on_battery(9))).unwrap();
    assert_eq!(low.state.ended, Some(EndReason::Battery { pct: 9 }));
    assert_eq!(
        ended(&connection),
        ["Keep awake ended: the battery reached 9%."]
    );
    let items = listed(&connection, NoticeKind::KeepAwakeEnded, NoticeFilter::All);
    assert_eq!(items[0].class, NoticeClass::Info);
    evaluate(&connection, NOW_MS + 2 * SECOND_MS, Some(&on_battery(9))).unwrap();
    evaluate(&connection, NOW_MS + 10 * SECOND_MS, None).unwrap();
    assert_eq!(ended(&connection).len(), 1, "one notice for the one hold");

    let left = Leftover::Kinds(vec!["sleep_disabled".into()]);
    let mut keeper = Keeper::default().with_leftover(left);
    keeper.tick(&connection, NOW_MS + MINUTE_MS).unwrap();
    notices_reconciled(&mut connection, NOW_MS + MINUTE_MS);
    assert_eq!(pending_restores(&connection), [NOT_PUT_BACK]);
    keeper.tick(&connection, NOW_MS + 2 * MINUTE_MS).unwrap();
    notices_reconciled(&mut connection, NOW_MS + 2 * MINUTE_MS);
    assert_eq!(
        pending_restores(&connection),
        [NOT_PUT_BACK],
        "it stays pending while the change is not put back"
    );

    let mut next = Keeper::default().with_leftover(Leftover::Kinds(Vec::new()));
    next.tick(&connection, NOW_MS + 3 * MINUTE_MS).unwrap();
    notices_reconciled(&mut connection, NOW_MS + 3 * MINUTE_MS);
    assert!(
        pending_restores(&connection).is_empty(),
        "a start whose undo worked resolves it"
    );
    let done = listed(
        &connection,
        NoticeKind::KeepAwakeRestore,
        NoticeFilter::Done,
    );
    assert_eq!(done.len(), 1);
}

#[test]
fn only_a_hold_in_place_that_ends_by_itself_raises_an_ended_notice() {
    let (_folder, connection) = engine();
    agent(&connection, "working");
    let ac = sensors(PowerSource::Ac, Some(90));
    assert_ne!(
        evaluate(&connection, NOW_MS, Some(&ac)).unwrap().state.want,
        Want::Nothing
    );
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS + SECOND_MS),
        Outcome::Applied
    );
    let stopped = evaluate(&connection, NOW_MS + 2 * SECOND_MS, None).unwrap();
    assert_eq!(stopped.state.ended, Some(EndReason::Manual));
    assert!(ended(&connection).is_empty(), "End is the owner's own");

    let (_folder, connection) = engine();
    let start = KeepAwakeAction::Start { minutes: 1 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    assert_ne!(
        evaluate(&connection, NOW_MS + SECOND_MS, Some(&ac))
            .unwrap()
            .state
            .want,
        Want::Nothing
    );
    let spent = evaluate(&connection, NOW_MS + 2 * MINUTE_MS, None).unwrap();
    assert_eq!(spent.state.want, Want::Nothing);
    assert!(
        ended(&connection).is_empty(),
        "a timer that ran out is the owner's own"
    );

    let (_folder, connection) = engine();
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    assert_ne!(
        evaluate(&connection, NOW_MS + SECOND_MS, Some(&ac))
            .unwrap()
            .state
            .want,
        Want::Nothing
    );
    let off = KeepAwakeSettings {
        mode: Mode::Off,
        ..KeepAwakeSettings::DEFAULT
    };
    set(&connection, off);
    assert_eq!(view(&connection).unwrap().state.ended, Some(EndReason::Off));
    assert!(ended(&connection).is_empty(), "mode Off is the owner's own");

    let (_folder, connection) = engine();
    evaluate(&connection, NOW_MS, Some(&on_battery(5))).unwrap();
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(
        act(&connection, start, NOW_MS + SECOND_MS),
        Outcome::Applied
    );
    let refused = view(&connection).unwrap();
    assert_eq!(refused.state.want, Want::Nothing);
    assert_eq!(refused.state.ended, Some(EndReason::Battery { pct: 5 }));
    assert!(
        ended(&connection).is_empty(),
        "nothing was held, so nothing ended"
    );
}

#[test]
fn a_hold_that_ends_by_heat_the_limit_or_an_unread_battery_says_why() {
    let (_folder, connection) = engine();
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&warm(Heat::Nominal))).unwrap();
    let burnt = evaluate(&connection, NOW_MS + SECOND_MS, Some(&warm(Heat::Critical))).unwrap();
    assert_eq!(burnt.state.ended, Some(EndReason::Heat));
    assert_eq!(
        ended(&connection),
        ["Keep awake ended: this computer got too hot."]
    );
    assert!(
        hot(&connection).is_empty(),
        "a hold that ends for heat only says it ended"
    );

    let (_folder, connection) = engine();
    set(
        &connection,
        KeepAwakeSettings {
            cap: Cap::H4,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    agent(&connection, "working");
    let ac = sensors(PowerSource::Ac, Some(90));
    evaluate(&connection, NOW_MS, Some(&ac)).unwrap();
    let capped = evaluate(&connection, NOW_MS + 4 * HOUR_MS, None).unwrap();
    assert_eq!(capped.state.ended, Some(EndReason::Cap { hours: 4 }));
    assert_eq!(
        ended(&connection),
        ["Keep awake ended after its 4 h limit."]
    );

    let (_folder, connection) = engine();
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&on_battery(60))).unwrap();
    let unread = Sensors {
        battery_pct: None,
        battery_unread_twice: true,
        ..on_battery(60)
    };
    let lost = evaluate(&connection, NOW_MS + SECOND_MS, Some(&unread)).unwrap();
    assert_eq!(lost.state.ended, Some(EndReason::BatteryUnread));
    assert_eq!(
        ended(&connection),
        ["Keep awake ended: the battery charge could not be read."]
    );
}

#[test]
fn each_hold_says_its_end_once_and_a_hold_the_system_does_not_keep_once_a_day() {
    let (_folder, connection) = engine();
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&on_battery(50))).unwrap();
    evaluate(&connection, NOW_MS + SECOND_MS, Some(&on_battery(9))).unwrap();
    let again = evaluate(&connection, NOW_MS + 2 * SECOND_MS, Some(&on_battery(50))).unwrap();
    assert_ne!(
        again.state.want,
        Want::Nothing,
        "a charged battery holds again"
    );
    evaluate(&connection, NOW_MS + 3 * SECOND_MS, Some(&on_battery(8))).unwrap();
    assert_eq!(
        ended(&connection),
        [
            "Keep awake ended: the battery reached 8%.",
            "Keep awake ended: the battery reached 9%."
        ],
        "two holds, two notices"
    );

    let (_folder, connection) = engine();
    let mut keeper = Keeper::with_hold(Box::new(Refusing));
    let worker = agent(&connection, "working");
    let failed = keeper.tick(&connection, NOW_MS).unwrap().unwrap();
    assert_eq!(failed.state.ended, Some(EndReason::HoldLost));
    assert_eq!(ended(&connection), [LOST]);
    let burst = |keeper: &mut Keeper, at: i64| {
        mark(&connection, worker, "idle");
        keeper.tick(&connection, at).unwrap();
        mark(&connection, worker, "working");
        let view = keeper.tick(&connection, at + TICK_MS).unwrap().unwrap();
        assert_eq!(view.state.ended, Some(EndReason::HoldLost));
    };
    burst(&mut keeper, NOW_MS + TICK_MS);
    assert_eq!(ended(&connection), [LOST], "the same day says it once");
    burst(&mut keeper, NOW_MS + DAY_MS);
    assert_eq!(
        ended(&connection),
        [LOST, LOST],
        "the next day says it again"
    );
}

#[test]
fn serious_heat_during_a_hold_says_so_once_per_hold_and_only_where_the_setting_asks() {
    let (_folder, connection) = engine();
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&warm(Heat::Nominal))).unwrap();
    assert!(hot(&connection).is_empty());
    let serious = evaluate(&connection, NOW_MS + SECOND_MS, Some(&warm(Heat::Serious))).unwrap();
    assert_ne!(
        serious.state.want,
        Want::Nothing,
        "serious heat does not end it by default"
    );
    assert_eq!(hot(&connection), [HOT_UNTIL_CRITICAL]);
    for (at, heat) in [(2, Heat::Serious), (3, Heat::Nominal), (4, Heat::Serious)] {
        evaluate(&connection, NOW_MS + at * SECOND_MS, Some(&warm(heat))).unwrap();
    }
    assert_eq!(
        hot(&connection).len(),
        1,
        "one heat notice for the one hold"
    );
    assert!(ended(&connection).is_empty());

    let (_folder, connection) = engine();
    set(
        &connection,
        KeepAwakeSettings {
            heat_end: HeatEnd::Never,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&warm(Heat::Nominal))).unwrap();
    let critical = evaluate(&connection, NOW_MS + SECOND_MS, Some(&warm(Heat::Critical))).unwrap();
    assert_ne!(critical.state.want, Want::Nothing);
    assert_eq!(hot(&connection), [HOT_NEVER_ENDS]);

    let (_folder, connection) = engine();
    set(
        &connection,
        KeepAwakeSettings {
            heat_notice: HeatNotice::Off,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&warm(Heat::Nominal))).unwrap();
    evaluate(&connection, NOW_MS + SECOND_MS, Some(&warm(Heat::Serious))).unwrap();
    assert!(
        hot(&connection).is_empty(),
        "the heat notice setting is Off"
    );
}

#[test]
fn a_keep_awake_kind_the_owner_turned_off_records_nothing() {
    let (_folder, connection) = engine();
    for (kind, delivery) in [
        ("keep_awake_ended", "off"),
        ("keep_awake_heat", "bell_only"),
    ] {
        connection
            .execute(
                "INSERT INTO owner_notice_kinds(kind,delivery) VALUES(?1,?2)",
                params![kind, delivery],
            )
            .unwrap();
    }
    agent(&connection, "working");
    evaluate(&connection, NOW_MS, Some(&warm(Heat::Nominal))).unwrap();
    evaluate(&connection, NOW_MS + SECOND_MS, Some(&warm(Heat::Serious))).unwrap();
    evaluate(
        &connection,
        NOW_MS + 2 * SECOND_MS,
        Some(&warm(Heat::Critical)),
    )
    .unwrap();
    assert!(
        ended(&connection).is_empty(),
        "Off keeps them out of the store"
    );
    assert_eq!(
        hot(&connection),
        [HOT_UNTIL_CRITICAL],
        "Bell only still records"
    );
}

#[test]
fn the_engine_start_undo_hands_on_what_it_could_not_undo() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(reconcile(folder.path()), Leftover::Kinds(Vec::new()));
    let mut found = journal::Journal::open(folder.path()).unwrap();
    for (kind, restore) in [("sleep_disabled", "0"), ("lid_action", "1")] {
        let entry = journal::Entry {
            kind: kind.into(),
            restore: restore.into(),
        };
        found.record(entry).unwrap();
    }
    assert_eq!(
        reconcile(folder.path()),
        Leftover::Kinds(vec!["sleep_disabled".into(), "lid_action".into()]),
        "no kind of change can be undone yet, so both stay"
    );
    std::fs::write(
        folder.path().join("keep-awake/journal.json"),
        b"not a journal",
    )
    .unwrap();
    assert_eq!(
        reconcile(folder.path()),
        Leftover::Unreadable,
        "a journal that cannot be read never counts as empty"
    );
}

#[test]
fn a_restore_state_that_cannot_be_written_is_tried_again_and_never_fails_the_step() {
    let (_folder, connection) = engine();
    let rows = || -> i64 {
        connection
            .query_row("SELECT COUNT(*) FROM keep_awake_restore", [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    connection
        .execute_batch("ALTER TABLE keep_awake_restore RENAME TO keep_awake_restore_away")
        .unwrap();
    let left = Leftover::Kinds(vec!["sleep_disabled".into()]);
    let mut keeper = Keeper::default().with_leftover(left);
    assert!(keeper.tick(&connection, NOW_MS).is_ok(), "the step goes on");
    connection
        .execute_batch("ALTER TABLE keep_awake_restore_away RENAME TO keep_awake_restore")
        .unwrap();
    assert!(keeper.tick(&connection, NOW_MS + SECOND_MS).is_ok());
    assert_eq!(rows(), 1, "the next step writes it");
    connection
        .execute("DELETE FROM keep_awake_restore", [])
        .unwrap();
    keeper.tick(&connection, NOW_MS + 2 * SECOND_MS).unwrap();
    assert_eq!(rows(), 0, "it is written once");
}
