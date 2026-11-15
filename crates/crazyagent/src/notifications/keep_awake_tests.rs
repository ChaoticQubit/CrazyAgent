use super::keep_awake::{ended_text, hot_text, left_over};
use super::*;
use crate::config::ModelAccess;
use crate::keep_awake::{EndReason, HeatEnd, Leftover};
use rusqlite::{Connection, params};

const NOW_MS: i64 = 1_800_000_000_000;
const NOT_PUT_BACK: &str = "Keep awake could not put back a sleep setting it changed. It tries \
                            again when the engine starts.";
const NOT_READ: &str = "Keep awake could not read its list of sleep settings to put back. It \
                        tries again when the engine starts.";
const KEEP_AWAKE_KINDS: [(NoticeKind, &str, &str, NoticeClass); 4] = [
    (
        NoticeKind::KeepAwakeRestore,
        "keep_awake_restore",
        "Sleep not restored",
        NoticeClass::Action,
    ),
    (
        NoticeKind::KeepAwakeApproval,
        "keep_awake_approval",
        "Lid hold approval",
        NoticeClass::Action,
    ),
    (
        NoticeKind::KeepAwakeEnded,
        "keep_awake_ended",
        "Keep awake ended",
        NoticeClass::Info,
    ),
    (
        NoticeKind::KeepAwakeHeat,
        "keep_awake_heat",
        "Hot during Keep awake",
        NoticeClass::Info,
    ),
];

fn store() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

fn restores(connection: &Connection, filter: NoticeFilter) -> Vec<Notice> {
    let query = NoticeQuery {
        filter,
        project_id: None,
        kind: Some(NoticeKind::KeepAwakeRestore),
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    };
    page(connection, &query).unwrap().items
}

fn reconciled(connection: &mut Connection, now: i64) {
    reconcile::reconcile(connection, &ModelAccess::default(), now).unwrap();
}

fn kinds(names: &[&str]) -> Leftover {
    Leftover::Kinds(names.iter().map(|name| (*name).to_owned()).collect())
}

#[test]
fn the_four_keep_awake_kinds_have_their_names_labels_and_classes() {
    for (kind, name, label, class) in KEEP_AWAKE_KINDS {
        assert_eq!(kind.name(), name);
        assert_eq!(kind.label(), label);
        assert_eq!(kind.class(), class);
        assert_eq!(NoticeKind::parse(name), Some(kind));
        assert!(label.chars().count() <= 22, "{label} fits the label column");
    }
    let actions: Vec<NoticeKind> = NoticeKind::ALL
        .into_iter()
        .take_while(|kind| kind.class() == NoticeClass::Action)
        .collect();
    assert_eq!(actions.len(), 7, "every Action kind comes first");
    assert_eq!(
        &actions[5..],
        [NoticeKind::KeepAwakeRestore, NoticeKind::KeepAwakeApproval]
    );
    assert_eq!(
        NoticeKind::ALL[22..],
        [NoticeKind::KeepAwakeEnded, NoticeKind::KeepAwakeHeat]
    );
    assert_eq!(NoticeTarget::KeepAwake.project(), None);
}

#[test]
fn settings_saved_before_the_keep_awake_kinds_read_them_as_on() {
    let (_folder, connection) = store();
    connection
        .execute(
            "INSERT INTO owner_notice_settings(singleton,retention_days,banners) VALUES(1,7,'all')",
            [],
        )
        .unwrap();
    for kind in &NoticeKind::ALL[..5] {
        connection
            .execute(
                "INSERT INTO owner_notice_kinds(kind,delivery) VALUES(?1,'bell_only')",
                params![kind.name()],
            )
            .unwrap();
    }
    let read = settings(&connection).unwrap();
    assert_eq!(read.retention_days, 7);
    assert_eq!(read.delivery(NoticeKind::Question), Delivery::BellOnly);
    for (kind, _, _, _) in KEEP_AWAKE_KINDS {
        assert_eq!(read.delivery(kind), Delivery::On, "{kind:?}");
    }
    let mut refused = read.clone();
    for item in &mut refused.kinds {
        if item.kind == NoticeKind::KeepAwakeRestore {
            item.delivery = Delivery::Off;
        }
    }
    assert!(!refused.valid(), "an Action kind is never Off");
}

#[test]
fn an_ended_hold_and_heat_get_the_control_words() {
    let cases = [
        (
            EndReason::Battery { pct: 9 },
            Some("Keep awake ended: the battery reached 9%."),
        ),
        (
            EndReason::BatteryUnread,
            Some("Keep awake ended: the battery charge could not be read."),
        ),
        (
            EndReason::Heat,
            Some("Keep awake ended: this computer got too hot."),
        ),
        (
            EndReason::Cap { hours: 8 },
            Some("Keep awake ended after its 8 h limit."),
        ),
        (
            EndReason::HoldLost,
            Some("Keep awake ended: the system did not keep this computer awake."),
        ),
        (EndReason::Manual, None),
        (EndReason::Off, None),
    ];
    for (ended, words) in cases {
        assert_eq!(ended_text(ended).as_deref(), words, "{ended:?}");
    }
    assert_eq!(
        ended_text(EndReason::Battery { pct: 250 }).as_deref(),
        Some("Keep awake ended: the battery reached 100%.")
    );
    assert_eq!(
        hot_text(HeatEnd::Critical),
        "This computer is running hot. Keep awake goes on and ends if the heat gets critical."
    );
    assert_eq!(
        hot_text(HeatEnd::Never),
        "This computer is running hot. Keep awake goes on; it is set never to end for heat."
    );
    assert!(!hot_text(HeatEnd::Serious).is_empty());
}

#[test]
fn the_restore_source_opens_one_pending_item_per_kind_left_and_resolves_when_it_is_gone() {
    let (_folder, mut connection) = store();
    let left = kinds(&["sleep_disabled", "lid_action", "sleep_disabled"]);
    left_over(&connection, &left).unwrap();
    reconciled(&mut connection, NOW_MS);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 2, "one item per kind left");
    for item in &pending {
        assert_eq!(item.text, NOT_PUT_BACK);
        assert_eq!(item.target, NoticeTarget::KeepAwake);
        assert_eq!(item.class, NoticeClass::Action);
        assert!(!item.text.contains("sleep_disabled"));
        assert!(!item.text.contains("lid_action"));
    }

    left_over(&connection, &kinds(&["lid_action"])).unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    assert_eq!(restores(&connection, NoticeFilter::Pending).len(), 1);
    assert_eq!(restores(&connection, NoticeFilter::Done).len(), 1);

    left_over(&connection, &Leftover::Unreadable).unwrap();
    reconciled(&mut connection, NOW_MS + 2);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, NOT_READ);

    left_over(&connection, &kinds(&[])).unwrap();
    reconciled(&mut connection, NOW_MS + 3);
    assert!(restores(&connection, NoticeFilter::Pending).is_empty());
    assert_eq!(restores(&connection, NoticeFilter::Done).len(), 3);
}

#[test]
fn a_missing_restore_state_resolves_nothing() {
    let (_folder, mut connection) = store();
    left_over(&connection, &kinds(&["sleep_disabled"])).unwrap();
    reconciled(&mut connection, NOW_MS);
    assert_eq!(restores(&connection, NoticeFilter::Pending).len(), 1);
    connection
        .execute_batch("DROP TABLE keep_awake_restore")
        .unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    assert_eq!(
        restores(&connection, NoticeFilter::Pending).len(),
        1,
        "a source that cannot be read resolves nothing"
    );
}
