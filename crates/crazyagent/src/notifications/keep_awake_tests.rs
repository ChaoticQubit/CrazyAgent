use super::keep_awake::{
    APPROVAL_TEXT, LID_JOURNAL_TEXT, LID_SETTING_TEXT, apps_not_paused, apps_not_paused_text,
    ended_text, hot_text, left_over, lid_closed_left, lid_journal,
};
use super::*;
use crate::config::ModelAccess;
use crate::keep_awake::{EndReason, HeatEnd, Leftover, LidSupport, Sensors};
use rusqlite::{Connection, params};

const NOW_MS: i64 = 1_800_000_000_000;
const NOT_PUT_BACK: &str = "Keep awake could not put back a sleep setting it changed. It tries \
                            again when the engine starts.";
const NOT_READ: &str = "Keep awake could not read its list of sleep settings to put back. It \
                        tries again when the engine starts.";
const KEEP_AWAKE_KINDS: [(NoticeKind, &str, &str, NoticeClass); 5] = [
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
    (
        NoticeKind::KeepAwakeAppsNotPaused,
        "keep_awake_apps_not_paused",
        "Apps not paused",
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
fn the_keep_awake_kinds_have_their_names_labels_and_classes() {
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
        [
            NoticeKind::KeepAwakeEnded,
            NoticeKind::KeepAwakeHeat,
            NoticeKind::KeepAwakeAppsNotPaused
        ]
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

/// QUESTION-03:02 A: a lid close that paused nothing because the lane could not check it
/// tells the owner why, once per refusal, as an Info item that opens Keep awake.
#[test]
fn a_refused_freeze_tells_the_owner_why_once_per_refusal() {
    let (_folder, connection) = store();
    let agents = "Keep awake did not pause the apps you chose while the lid was closed: it \
                  could not tell which processes belong to agents. They kept running.";
    let unread = "Keep awake did not pause the apps you chose while the lid was closed: it \
                  could not read which apps are running. They kept running.";
    assert_eq!(apps_not_paused_text(true), agents);
    assert_eq!(apps_not_paused_text(false), unread);

    apps_not_paused(&connection, true, NOW_MS);
    apps_not_paused(&connection, true, NOW_MS);
    apps_not_paused(&connection, false, NOW_MS + 1);

    let query = NoticeQuery {
        filter: NoticeFilter::All,
        project_id: None,
        kind: Some(NoticeKind::KeepAwakeAppsNotPaused),
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    };
    let items = page(&connection, &query).unwrap().items;
    let mut texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    texts.sort();
    assert_eq!(texts, vec![unread.to_owned(), agents.to_owned()]);
    assert!(
        items
            .iter()
            .all(|item| item.target == NoticeTarget::KeepAwake)
    );
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

/// An engine database with every table, Keep awake's included.
fn engine_store() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    crate::landing::number_change_sets(&connection).unwrap();
    (folder, connection)
}

fn approvals(connection: &Connection, filter: NoticeFilter) -> Vec<Notice> {
    let query = NoticeQuery {
        filter,
        project_id: None,
        kind: Some(NoticeKind::KeepAwakeApproval),
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    };
    page(connection, &query).unwrap().items
}

#[cfg(not(windows))]
#[test]
fn the_lid_helper_journal_item_shows_the_fix_and_outlives_the_engine_start_undo() {
    let (_folder, mut connection) = store();
    lid_journal(&connection, true).unwrap();
    left_over(&connection, &kinds(&["sleep_disabled"])).unwrap();
    reconciled(&mut connection, NOW_MS);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 2, "the undo's item and the lid helper's");
    let lid = pending
        .iter()
        .find(|item| item.text == LID_JOURNAL_TEXT)
        .expect("the lid helper item");
    assert!(lid.text.contains("sudo pmset -a disablesleep 0"));
    let sleep_back = lid.text.find("disablesleep").unwrap();
    let journal = lid.text.find("journal.json").unwrap();
    let removed = lid.text.find("Remove lid helper").unwrap();
    assert!(
        sleep_back < journal && journal < removed,
        "sleep goes back before the journal and the helper go (threat model section 8)"
    );
    assert!(LID_JOURNAL_TEXT.len() <= TEXT_BYTES_MAX);

    left_over(&connection, &kinds(&[])).unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 1, "the undo clears only its own rows");
    assert_eq!(pending[0].text, LID_JOURNAL_TEXT);

    lid_journal(&connection, false).unwrap();
    lid_journal(&connection, false).unwrap();
    reconciled(&mut connection, NOW_MS + 2);
    assert!(restores(&connection, NoticeFilter::Pending).is_empty());
}

/// DEC-167 addendum 32: on Windows the lid row names the Windows fix, and the lid line
/// clearing it also clears the engine start's row for a lid setting not put back, so one
/// put back that works resolves both.
#[cfg(windows)]
#[test]
fn on_windows_the_lid_item_names_put_back_now_and_clears_with_the_engine_start_one() {
    let (_folder, mut connection) = store();
    let left = kinds(&["windows_lid_action", "sleep_disabled"]);
    left_over(&connection, &left).unwrap();
    lid_journal(&connection, true).unwrap();
    reconciled(&mut connection, NOW_MS);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 3, "two kinds left and the lid line's row");
    let lid = pending
        .iter()
        .find(|item| item.text == LID_SETTING_TEXT)
        .expect("the lid setting item");
    assert!(lid.text.contains("Put the lid setting back now"));
    assert!(lid.text.contains("Power Options"));
    assert!(!lid.text.contains("pmset"));
    assert!(LID_SETTING_TEXT.len() <= TEXT_BYTES_MAX);

    lid_journal(&connection, false).unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    let pending = restores(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 1, "only the other kind's item stays");
    assert_eq!(pending[0].text, NOT_PUT_BACK);
}

#[test]
fn the_approval_notice_shows_while_the_lid_helper_waits_for_approval() {
    let (_folder, mut connection) = engine_store();
    let waiting = Sensors {
        lid_support: LidSupport::NeedsApproval,
        ..Sensors::unread()
    };
    crate::keep_awake::evaluate(&connection, NOW_MS, Some(&waiting)).unwrap();
    reconciled(&mut connection, NOW_MS);
    let pending = approvals(&connection, NoticeFilter::Pending);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, APPROVAL_TEXT);
    assert_eq!(pending[0].target, NoticeTarget::KeepAwake);
    assert_eq!(pending[0].class, NoticeClass::Action);
    assert_eq!(
        APPROVAL_TEXT,
        "Allow the lid helper in System Settings to hold with the lid closed."
    );

    let approved = Sensors {
        lid_support: LidSupport::Held,
        ..Sensors::unread()
    };
    crate::keep_awake::evaluate(&connection, NOW_MS + 1, Some(&approved)).unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    assert!(approvals(&connection, NoticeFilter::Pending).is_empty());
    assert_eq!(approvals(&connection, NoticeFilter::Done).len(), 1);
}

/// rv-secreview and rv-code T17 r1: an app or the sound the lid-closed lane could not put
/// back after three tries gets its own item while the engine runs. The lane's items change
/// only their own rows: the engine start's undo item and the lid helper item stay, and the
/// lane's items clear once it put everything back.
#[test]
fn the_lid_closed_lane_items_change_only_their_own_rows() {
    let (_folder, mut connection) = store();
    let freeze = crate::keep_awake::freeze::KIND;
    let mute = crate::keep_awake::mute::KIND;
    let lid = if cfg!(windows) {
        LID_SETTING_TEXT
    } else {
        LID_JOURNAL_TEXT
    };
    lid_journal(&connection, true).unwrap();
    left_over(&connection, &kinds(&["sleep_disabled"])).unwrap();

    lid_closed_left(&connection, &[freeze, mute]).unwrap();
    lid_closed_left(&connection, &[freeze, mute]).unwrap();

    reconciled(&mut connection, NOW_MS);
    let texts: Vec<String> = restores(&connection, NoticeFilter::Pending)
        .into_iter()
        .map(|item| item.text)
        .collect();
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|text| text.contains("could not start an app again"))
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("could not put the sound back"))
    );
    assert!(texts.iter().any(|text| text == NOT_PUT_BACK));
    assert!(texts.iter().any(|text| text == lid));

    lid_closed_left(&connection, &[mute]).unwrap();
    reconciled(&mut connection, NOW_MS + 1);
    assert_eq!(restores(&connection, NoticeFilter::Pending).len(), 3);

    lid_closed_left(&connection, &[]).unwrap();
    reconciled(&mut connection, NOW_MS + 2);
    let texts: Vec<String> = restores(&connection, NoticeFilter::Pending)
        .into_iter()
        .map(|item| item.text)
        .collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.iter().any(|text| text == NOT_PUT_BACK));
    assert!(texts.iter().any(|text| text == lid));
}
