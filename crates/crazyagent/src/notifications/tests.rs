use super::reconcile::{Condition, sync};
use super::*;
use rusqlite::{Connection, params};

const NOW_MS: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn store() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

fn question(project_id: &str, subject: &str, text: &str) -> Condition {
    Condition {
        kind: NoticeKind::Question,
        project_id: Some(project(project_id)),
        subject: subject.into(),
        text: text.into(),
        target: NoticeTarget::Questions {
            project_id: project(project_id),
            question: None,
            agent: None,
        },
    }
}

fn memories(project_id: &str, text: &str) -> Condition {
    Condition {
        kind: NoticeKind::MemoryReview,
        project_id: Some(project(project_id)),
        subject: format!("memories:{project_id}"),
        text: text.into(),
        target: NoticeTarget::Memories {
            project_id: project(project_id),
        },
    }
}

fn synced(connection: &mut Connection, kinds: &[NoticeKind], live: &[Condition], now: i64) {
    let tx = connection.transaction().unwrap();
    sync(&tx, kinds, &[], live, now).unwrap();
    tx.commit().unwrap();
}

fn synced_in_part(
    connection: &mut Connection,
    kinds: &[NoticeKind],
    held: Option<&str>,
    live: &[Condition],
    now: i64,
) {
    let partial: Vec<(NoticeKind, Option<ProjectId>)> = kinds
        .iter()
        .map(|kind| (*kind, held.map(project)))
        .collect();
    let tx = connection.transaction().unwrap();
    sync(&tx, kinds, &partial, live, now).unwrap();
    tx.commit().unwrap();
}

fn finished(connection: &mut Connection, project_id: &str, subject: &str, now: i64) -> Option<i64> {
    let tx = connection.transaction().unwrap();
    let id = record(
        &tx,
        &InfoNotice {
            kind: NoticeKind::AgentFinished,
            project_id: Some(&project(project_id)),
            subject,
            text: "Sub-agent 2 finished Add the login form",
            target: NoticeTarget::Agents {
                project_id: project(project_id),
            },
        },
        now,
    )
    .unwrap();
    tx.commit().unwrap();
    id
}

fn act(connection: &mut Connection, action: NoticeAction, now: i64) -> Outcome {
    let tx = connection.transaction().unwrap();
    let outcome = apply(&tx, &action, now).unwrap();
    tx.commit().unwrap();
    outcome
}

fn query(filter: NoticeFilter) -> NoticeQuery {
    NoticeQuery {
        filter,
        project_id: None,
        kind: None,
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    }
}

fn listed(connection: &Connection, query: &NoticeQuery) -> NoticePage {
    page(connection, query).unwrap()
}

fn texts(page: &NoticePage) -> Vec<&str> {
    page.items.iter().map(|item| item.text.as_str()).collect()
}

fn rows(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM owner_notices", [], |row| row.get(0))
        .unwrap()
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

#[test]
fn a_new_action_condition_opens_one_pending_unread_item() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question(
            "p1",
            "question:c7",
            "Master asks: which database?",
        )],
        NOW_MS,
    );
    let page = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(item.kind, NoticeKind::Question);
    assert_eq!(item.class, NoticeClass::Action);
    assert_eq!(item.project_id, Some(project("p1")));
    assert_eq!(item.text, "Master asks: which database?");
    assert_eq!(item.created_ms, NOW_MS);
    assert_eq!(item.read_ms, None);
    assert_eq!(item.resolved_ms, None);
    assert_eq!((item.unread, item.pending, item.folded), (1, 1, 1));
    assert_eq!(item.ids, vec![item.id]);
    assert_eq!(
        page.counts,
        NoticeCounts {
            unread: 1,
            pending: 1
        }
    );
    assert_eq!(page.newest, item.id);
}

#[test]
fn a_condition_seen_again_while_open_keeps_its_one_item_and_takes_the_new_text() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::MemoryReview];
    synced(
        &mut connection,
        &kinds,
        &[memories("p1", "2 suggested memories to review.")],
        NOW_MS,
    );
    let first = listed(&connection, &query(NoticeFilter::All)).items[0].id;
    synced(
        &mut connection,
        &kinds,
        &[memories("p1", "3 suggested memories to review.")],
        NOW_MS + 1,
    );
    let page = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(texts(&page), ["3 suggested memories to review."]);
    assert_eq!(page.items[0].id, first);
    assert_eq!(page.items[0].created_ms, NOW_MS);
    assert_eq!(rows(&connection), 1);
}

#[test]
fn a_cleared_condition_resolves_its_item_marks_it_read_and_lists_it_under_done() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    synced(
        &mut connection,
        &kinds,
        &[question("p1", "question:q1", "Which port?")],
        NOW_MS,
    );
    synced(&mut connection, &kinds, &[], NOW_MS + 5);
    let done = listed(&connection, &query(NoticeFilter::Done));
    assert_eq!(texts(&done), ["Which port?"]);
    assert_eq!(done.items[0].resolved_ms, Some(NOW_MS + 5));
    assert_eq!(done.items[0].read_ms, Some(NOW_MS + 5));
    assert_eq!(done.items[0].pending, 0);
    assert!(
        listed(&connection, &query(NoticeFilter::Pending))
            .items
            .is_empty()
    );
    assert_eq!(
        done.counts,
        NoticeCounts {
            unread: 0,
            pending: 0
        }
    );
}

#[test]
fn a_kind_whose_source_could_not_be_read_resolves_none_of_its_items() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Which port?")],
        NOW_MS,
    );
    synced(
        &mut connection,
        &[NoticeKind::MemoryReview],
        &[],
        NOW_MS + 5,
    );
    let pending = listed(&connection, &query(NoticeFilter::Pending));
    assert_eq!(texts(&pending), ["Which port?"]);
    assert_eq!(pending.counts.pending, 1);
}

#[test]
fn a_kind_its_source_read_only_in_part_opens_new_items_and_resolves_none() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Which port?")],
        NOW_MS,
    );
    synced_in_part(
        &mut connection,
        &[NoticeKind::Question],
        None,
        &[question("p1", "question:q2", "Which host?")],
        NOW_MS + 5,
    );
    let pending = listed(&connection, &query(NoticeFilter::Pending));
    assert_eq!(texts(&pending), ["Which host?", "Which port?"]);
    assert_eq!(pending.counts.pending, 2);
    assert!(texts(&listed(&connection, &query(NoticeFilter::Done))).is_empty());
}

#[test]
fn a_kind_read_only_in_part_for_one_project_still_resolves_the_other_projects_items() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "Which port?"),
            question("p2", "question:q2", "Which host?"),
        ],
        NOW_MS,
    );
    synced_in_part(
        &mut connection,
        &[NoticeKind::Question],
        Some("p1"),
        &[],
        NOW_MS + 5,
    );
    let pending = listed(&connection, &query(NoticeFilter::Pending));
    assert_eq!(texts(&pending), ["Which port?"]);
    let done = listed(&connection, &query(NoticeFilter::Done));
    assert_eq!(texts(&done), ["Which host?"]);
}

#[test]
fn a_condition_that_breaks_again_after_it_cleared_opens_a_new_item() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    let live = [question("p1", "question:q1", "Which port?")];
    synced(&mut connection, &kinds, &live, NOW_MS);
    synced(&mut connection, &kinds, &[], NOW_MS + 1);
    synced(&mut connection, &kinds, &live, NOW_MS + 2);
    let all = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(all.items.len(), 2);
    assert!(all.items[0].id > all.items[1].id);
    assert_eq!(all.items[0].resolved_ms, None);
    assert_eq!(all.items[1].resolved_ms, Some(NOW_MS + 1));
    assert_eq!(
        all.counts,
        NoticeCounts {
            unread: 1,
            pending: 1
        }
    );
}

#[test]
fn deleting_a_pending_action_hides_it_until_its_state_clears_and_breaks_again() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    let live = [question("p1", "question:q1", "Which port?")];
    synced(&mut connection, &kinds, &live, NOW_MS);
    let id = listed(&connection, &query(NoticeFilter::All)).items[0].id;
    assert_eq!(
        act(
            &mut connection,
            NoticeAction::Delete { ids: vec![id] },
            NOW_MS + 1
        ),
        Outcome::Applied
    );
    synced(&mut connection, &kinds, &live, NOW_MS + 2);
    let hidden = listed(&connection, &query(NoticeFilter::All));
    assert!(hidden.items.is_empty());
    assert_eq!(
        hidden.counts,
        NoticeCounts {
            unread: 0,
            pending: 0
        }
    );
    assert_eq!(rows(&connection), 1);
    synced(&mut connection, &kinds, &[], NOW_MS + 3);
    assert_eq!(rows(&connection), 0);
    assert!(
        listed(&connection, &query(NoticeFilter::Done))
            .items
            .is_empty()
    );
    synced(&mut connection, &kinds, &live, NOW_MS + 4);
    let again = listed(&connection, &query(NoticeFilter::Pending));
    assert_eq!(texts(&again), ["Which port?"]);
    assert!(again.items[0].id > id);
}

#[test]
fn deleting_an_info_item_or_a_resolved_action_removes_its_row() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    synced(
        &mut connection,
        &kinds,
        &[question("p1", "question:q1", "Which port?")],
        NOW_MS,
    );
    synced(&mut connection, &kinds, &[], NOW_MS + 1);
    let info = finished(&mut connection, "p1", "activity:4:1", NOW_MS + 2).unwrap();
    let resolved = listed(&connection, &query(NoticeFilter::Done)).items[0].id;
    let outcome = act(
        &mut connection,
        NoticeAction::Delete {
            ids: vec![info, resolved],
        },
        NOW_MS + 3,
    );
    assert_eq!(outcome, Outcome::Applied);
    assert_eq!(rows(&connection), 0);
}

#[test]
fn resolved_actions_and_read_info_past_the_retention_go_while_unread_and_pending_items_stay() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    synced(
        &mut connection,
        &kinds,
        &[
            question("p1", "question:q1", "Old"),
            question("p1", "question:q2", "Open"),
        ],
        NOW_MS,
    );
    synced(
        &mut connection,
        &kinds,
        &[question("p1", "question:q2", "Open")],
        NOW_MS + 1,
    );
    let read = finished(&mut connection, "p1", "activity:1:1", NOW_MS).unwrap();
    finished(&mut connection, "p1", "activity:2:1", NOW_MS).unwrap();
    act(
        &mut connection,
        NoticeAction::Read { ids: vec![read] },
        NOW_MS + 2,
    );
    let tx = connection.transaction().unwrap();
    prune(&tx, &NoticeSettings::default(), NOW_MS + 30 * DAY_MS).unwrap();
    tx.commit().unwrap();
    assert_eq!(rows(&connection), 4);
    let tx = connection.transaction().unwrap();
    prune(&tx, &NoticeSettings::default(), NOW_MS + 31 * DAY_MS).unwrap();
    tx.commit().unwrap();
    let all = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(
        texts(&all),
        ["Sub-agent 2 finished Add the login form", "Open"]
    );
    assert_eq!(
        all.counts,
        NoticeCounts {
            unread: 2,
            pending: 1
        }
    );
}

#[test]
fn a_shorter_retention_setting_removes_done_items_sooner() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    synced(
        &mut connection,
        &kinds,
        &[question("p1", "question:q1", "Old")],
        NOW_MS,
    );
    synced(&mut connection, &kinds, &[], NOW_MS);
    let settings = NoticeSettings {
        retention_days: 1,
        ..NoticeSettings::default()
    };
    let tx = connection.transaction().unwrap();
    prune(&tx, &settings, NOW_MS + 2 * DAY_MS).unwrap();
    tx.commit().unwrap();
    assert_eq!(rows(&connection), 0);
}

#[test]
fn the_row_cap_drops_the_oldest_finished_rows_and_never_a_pending_action() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Open")],
        NOW_MS,
    );
    let tx = connection.transaction().unwrap();
    for at in 0..ROWS_MAX {
        let subject = format!("activity:{at}:1");
        let info = InfoNotice {
            kind: NoticeKind::AgentFinished,
            project_id: Some(&project("p1")),
            subject: &subject,
            text: "Sub-agent 2 finished Add the login form",
            target: NoticeTarget::Agents {
                project_id: project("p1"),
            },
        };
        assert!(record(&tx, &info, NOW_MS + at).unwrap().is_some());
    }
    tx.commit().unwrap();
    assert_eq!(rows(&connection), ROWS_MAX);
    let tx = connection.transaction().unwrap();
    prune(&tx, &NoticeSettings::default(), NOW_MS + ROWS_MAX).unwrap();
    tx.commit().unwrap();
    assert_eq!(rows(&connection), ROWS_MAX);
    finished(
        &mut connection,
        "p1",
        "activity:last:1",
        NOW_MS + ROWS_MAX + 1,
    );
    assert_eq!(rows(&connection), ROWS_MAX);
    let pending = listed(&connection, &query(NoticeFilter::Pending));
    assert_eq!(texts(&pending), ["Open"]);
    let oldest: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM owner_notices WHERE subject='activity:0:1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(oldest, 0);
}

#[test]
fn each_filter_lists_its_items_and_the_counts_never_depend_on_the_page() {
    let (_folder, mut connection) = store();
    let kinds = [NoticeKind::Question];
    synced(
        &mut connection,
        &kinds,
        &[
            question("p1", "question:q1", "Done one"),
            question("p1", "question:q2", "Open one"),
        ],
        NOW_MS,
    );
    synced(
        &mut connection,
        &kinds,
        &[question("p1", "question:q2", "Open one")],
        NOW_MS + 1,
    );
    let read = finished(&mut connection, "p1", "activity:1:1", NOW_MS + 2).unwrap();
    act(
        &mut connection,
        NoticeAction::Read { ids: vec![read] },
        NOW_MS + 3,
    );
    connection
        .execute(
            "UPDATE owner_notices SET text='Read info' WHERE id=?1",
            [read],
        )
        .unwrap();
    finished(&mut connection, "p1", "activity:2:1", NOW_MS + 4);
    connection
        .execute(
            "UPDATE owner_notices SET text='Unread info' WHERE subject='activity:2:1'",
            [],
        )
        .unwrap();
    assert_eq!(
        texts(&listed(&connection, &query(NoticeFilter::All))),
        ["Unread info", "Read info", "Open one", "Done one"]
    );
    assert_eq!(
        texts(&listed(&connection, &query(NoticeFilter::Unread))),
        ["Unread info", "Open one"]
    );
    assert_eq!(
        texts(&listed(&connection, &query(NoticeFilter::Pending))),
        ["Open one"]
    );
    assert_eq!(
        texts(&listed(&connection, &query(NoticeFilter::Done))),
        ["Done one"]
    );
    let one = NoticeQuery {
        limit: 1,
        ..query(NoticeFilter::All)
    };
    let first = listed(&connection, &one);
    assert_eq!(texts(&first), ["Unread info"]);
    assert!(first.more);
    assert_eq!(
        first.counts,
        NoticeCounts {
            unread: 2,
            pending: 1
        }
    );
    let next = listed(
        &connection,
        &NoticeQuery {
            before: Some(first.items[0].id),
            ..one
        },
    );
    assert_eq!(texts(&next), ["Read info"]);
}

#[test]
fn a_project_scope_lists_and_counts_only_that_projects_items() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "In one"),
            question("p2", "question:q2", "In two"),
        ],
        NOW_MS,
    );
    let scoped = listed(
        &connection,
        &NoticeQuery {
            project_id: Some(project("p2")),
            ..query(NoticeFilter::Pending)
        },
    );
    assert_eq!(texts(&scoped), ["In two"]);
    assert_eq!(
        scoped.counts,
        NoticeCounts {
            unread: 1,
            pending: 1
        }
    );
    assert_eq!(
        listed(&connection, &query(NoticeFilter::Pending))
            .counts
            .pending,
        2
    );
}

#[test]
fn same_kind_items_in_one_project_fold_into_one_row_with_their_counts_and_ids() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "First"),
            question("p1", "question:q2", "Second"),
            question("p2", "question:q3", "Other project"),
        ],
        NOW_MS,
    );
    let first = finished(&mut connection, "p1", "activity:1:1", NOW_MS + 1).unwrap();
    let second = finished(&mut connection, "p1", "activity:2:1", NOW_MS + 2).unwrap();
    act(
        &mut connection,
        NoticeAction::Read { ids: vec![first] },
        NOW_MS + 3,
    );
    let folded = listed(
        &connection,
        &NoticeQuery {
            fold: true,
            ..query(NoticeFilter::All)
        },
    );
    assert_eq!(folded.items.len(), 3);
    let agents = &folded.items[0];
    assert_eq!(agents.id, second);
    assert_eq!((agents.folded, agents.unread, agents.pending), (2, 1, 0));
    assert_eq!(agents.ids, vec![second, first]);
    assert_eq!(folded.items[1].text, "Other project");
    assert_eq!(folded.items[1].folded, 1);
    let questions = &folded.items[2];
    assert_eq!(questions.text, "Second");
    assert_eq!(
        (questions.folded, questions.unread, questions.pending),
        (2, 2, 2)
    );
    assert_eq!(
        folded.counts,
        NoticeCounts {
            unread: 4,
            pending: 3
        }
    );
}

#[test]
fn a_kind_and_project_query_without_folding_opens_one_fold() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "First"),
            question("p1", "question:q2", "Second"),
        ],
        NOW_MS,
    );
    finished(&mut connection, "p1", "activity:1:1", NOW_MS + 1);
    let opened = listed(
        &connection,
        &NoticeQuery {
            project_id: Some(project("p1")),
            kind: Some(NoticeKind::Question),
            ..query(NoticeFilter::All)
        },
    );
    assert_eq!(texts(&opened), ["Second", "First"]);
    assert!(opened.items.iter().all(|item| item.folded == 1));
}

#[test]
fn a_fold_lists_at_most_its_id_bound() {
    let (_folder, mut connection) = store();
    for at in 0..(FOLD_IDS_MAX as i64 + 3) {
        finished(
            &mut connection,
            "p1",
            &format!("activity:{at}:1"),
            NOW_MS + at,
        );
    }
    let folded = listed(
        &connection,
        &NoticeQuery {
            fold: true,
            ..query(NoticeFilter::All)
        },
    );
    assert_eq!(folded.items.len(), 1);
    assert_eq!(folded.items[0].folded, FOLD_IDS_MAX as u32 + 3);
    assert_eq!(folded.items[0].ids.len(), FOLD_IDS_MAX);
    assert_eq!(folded.items[0].ids[0], folded.items[0].id);
}

#[test]
fn reading_an_action_item_keeps_it_pending_and_unread_puts_it_back() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Which port?")],
        NOW_MS,
    );
    let id = listed(&connection, &query(NoticeFilter::All)).items[0].id;
    assert_eq!(
        act(
            &mut connection,
            NoticeAction::Read { ids: vec![id] },
            NOW_MS + 1
        ),
        Outcome::Applied
    );
    let read = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(read.items[0].read_ms, Some(NOW_MS + 1));
    assert_eq!(
        read.counts,
        NoticeCounts {
            unread: 0,
            pending: 1
        }
    );
    assert_eq!(
        act(
            &mut connection,
            NoticeAction::Unread { ids: vec![id] },
            NOW_MS + 2
        ),
        Outcome::Applied
    );
    let unread = listed(&connection, &query(NoticeFilter::Unread));
    assert_eq!(unread.items[0].read_ms, None);
    assert_eq!(
        unread.counts,
        NoticeCounts {
            unread: 1,
            pending: 1
        }
    );
}

#[test]
fn read_all_marks_one_projects_items_or_every_item_read() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "One"),
            question("p2", "question:q2", "Two"),
        ],
        NOW_MS,
    );
    let one = NoticeAction::ReadAll {
        project_id: Some(project("p1")),
    };
    assert_eq!(act(&mut connection, one, NOW_MS + 1), Outcome::Applied);
    assert_eq!(
        texts(&listed(&connection, &query(NoticeFilter::Unread))),
        ["Two"]
    );
    let every = NoticeAction::ReadAll { project_id: None };
    assert_eq!(act(&mut connection, every, NOW_MS + 2), Outcome::Applied);
    assert!(
        listed(&connection, &query(NoticeFilter::Unread))
            .items
            .is_empty()
    );
    assert_eq!(
        listed(&connection, &query(NoticeFilter::All))
            .counts
            .pending,
        2
    );
}

#[test]
fn an_empty_or_too_long_id_list_is_refused_and_nothing_changes() {
    let (_folder, mut connection) = store();
    let id = finished(&mut connection, "p1", "activity:1:1", NOW_MS).unwrap();
    let empty = act(
        &mut connection,
        NoticeAction::Read { ids: vec![] },
        NOW_MS + 1,
    );
    assert_eq!(empty, rejected("notification_ids_invalid"));
    let mut ids = vec![id; ACTION_IDS_MAX];
    ids.push(id);
    let long = act(&mut connection, NoticeAction::Delete { ids }, NOW_MS + 1);
    assert_eq!(long, rejected("notification_ids_invalid"));
    assert_eq!(rows(&connection), 1);
    assert_eq!(
        listed(&connection, &query(NoticeFilter::Unread))
            .items
            .len(),
        1
    );
}

#[test]
fn a_fresh_engine_has_every_kind_on_thirty_days_and_all_banners() {
    let (_folder, connection) = store();
    let settings = settings(&connection).unwrap();
    assert_eq!(settings, NoticeSettings::default());
    assert_eq!(settings.retention_days, 30);
    assert_eq!(settings.banners, Banners::All);
    let kinds: Vec<NoticeKind> = settings.kinds.iter().map(|item| item.kind).collect();
    assert_eq!(kinds, NoticeKind::ALL);
    assert!(
        settings
            .kinds
            .iter()
            .all(|item| item.delivery == Delivery::On)
    );
}

#[test]
fn each_delivery_option_of_an_info_kind_is_saved_and_read_back() {
    let (_folder, mut connection) = store();
    for delivery in [Delivery::BellOnly, Delivery::Off, Delivery::On] {
        let mut chosen = NoticeSettings::default();
        chosen.kinds[NoticeKind::ALL
            .iter()
            .position(|kind| *kind == NoticeKind::AgentFailed)
            .unwrap()]
        .delivery = delivery;
        let saved = act(
            &mut connection,
            NoticeAction::SaveSettings {
                settings: chosen.clone(),
            },
            NOW_MS,
        );
        assert_eq!(saved, Outcome::Applied);
        assert_eq!(settings(&connection).unwrap(), chosen);
        assert_eq!(
            settings(&connection)
                .unwrap()
                .delivery(NoticeKind::AgentFailed),
            delivery
        );
    }
}

#[test]
fn an_action_kind_can_be_bell_only_but_never_off() {
    let (_folder, mut connection) = store();
    let mut silent = NoticeSettings::default();
    silent.kinds[0].delivery = Delivery::BellOnly;
    assert_eq!(silent.kinds[0].kind, NoticeKind::Question);
    let saved = act(
        &mut connection,
        NoticeAction::SaveSettings {
            settings: silent.clone(),
        },
        NOW_MS,
    );
    assert_eq!(saved, Outcome::Applied);
    let mut off = silent.clone();
    off.kinds[0].delivery = Delivery::Off;
    let refused = act(
        &mut connection,
        NoticeAction::SaveSettings { settings: off },
        NOW_MS,
    );
    assert_eq!(refused, rejected("notification_settings_invalid"));
    assert_eq!(settings(&connection).unwrap(), silent);
}

#[test]
fn settings_with_a_missing_or_repeated_kind_or_retention_out_of_bounds_are_refused() {
    let (_folder, mut connection) = store();
    let mut missing = NoticeSettings::default();
    missing.kinds.pop();
    let mut repeated = NoticeSettings::default();
    repeated.kinds[1] = repeated.kinds[0].clone();
    let short = NoticeSettings {
        retention_days: RETENTION_DAYS_MIN - 1,
        ..NoticeSettings::default()
    };
    let long = NoticeSettings {
        retention_days: RETENTION_DAYS_MAX + 1,
        ..NoticeSettings::default()
    };
    for settings in [missing, repeated, short, long] {
        let outcome = act(
            &mut connection,
            NoticeAction::SaveSettings { settings },
            NOW_MS,
        );
        assert_eq!(outcome, rejected("notification_settings_invalid"));
    }
    assert_eq!(settings(&connection).unwrap(), NoticeSettings::default());
}

#[test]
fn each_retention_bound_is_saved_and_read_back() {
    let (_folder, mut connection) = store();
    for days in [
        RETENTION_DAYS_MIN,
        RETENTION_DAYS_MAX,
        RETENTION_DAYS_DEFAULT,
    ] {
        let chosen = NoticeSettings {
            retention_days: days,
            ..NoticeSettings::default()
        };
        let saved = act(
            &mut connection,
            NoticeAction::SaveSettings {
                settings: chosen.clone(),
            },
            NOW_MS,
        );
        assert_eq!(saved, Outcome::Applied);
        assert_eq!(settings(&connection).unwrap().retention_days, days);
    }
}

fn banner_of(connection: &mut Connection, banners: Banners, delivery: Delivery) -> (bool, bool) {
    let mut chosen = NoticeSettings {
        banners,
        ..NoticeSettings::default()
    };
    for item in &mut chosen.kinds {
        item.delivery = delivery;
    }
    let saved = act(
        connection,
        NoticeAction::SaveSettings { settings: chosen },
        NOW_MS,
    );
    assert_eq!(saved, Outcome::Applied);
    let page = listed(connection, &query(NoticeFilter::All));
    let action = page
        .items
        .iter()
        .find(|item| item.class == NoticeClass::Action)
        .unwrap();
    let info = page
        .items
        .iter()
        .find(|item| item.class == NoticeClass::Info)
        .unwrap();
    (action.banner, info.banner)
}

#[test]
fn banners_all_shows_action_and_info_kinds_that_are_on() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Q")],
        NOW_MS,
    );
    finished(&mut connection, "p1", "activity:1:1", NOW_MS);
    assert_eq!(
        banner_of(&mut connection, Banners::All, Delivery::On),
        (true, true)
    );
}

#[test]
fn banners_action_only_shows_only_action_kinds() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Q")],
        NOW_MS,
    );
    finished(&mut connection, "p1", "activity:1:1", NOW_MS);
    assert_eq!(
        banner_of(&mut connection, Banners::ActionOnly, Delivery::On),
        (true, false)
    );
}

#[test]
fn banners_off_shows_none() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Q")],
        NOW_MS,
    );
    finished(&mut connection, "p1", "activity:1:1", NOW_MS);
    assert_eq!(
        banner_of(&mut connection, Banners::Off, Delivery::On),
        (false, false)
    );
}

#[test]
fn a_bell_only_kind_shows_in_the_bell_with_no_banner() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", "Q")],
        NOW_MS,
    );
    finished(&mut connection, "p1", "activity:1:1", NOW_MS);
    assert_eq!(
        banner_of(&mut connection, Banners::All, Delivery::BellOnly),
        (false, false)
    );
}

#[test]
fn an_info_kind_set_off_is_not_stored_and_one_set_bell_only_is() {
    let (_folder, mut connection) = store();
    let mut chosen = NoticeSettings::default();
    let at = NoticeKind::ALL
        .iter()
        .position(|kind| *kind == NoticeKind::AgentFinished)
        .unwrap();
    chosen.kinds[at].delivery = Delivery::Off;
    act(
        &mut connection,
        NoticeAction::SaveSettings {
            settings: chosen.clone(),
        },
        NOW_MS,
    );
    assert_eq!(
        finished(&mut connection, "p1", "activity:1:1", NOW_MS),
        None
    );
    assert_eq!(rows(&connection), 0);
    chosen.kinds[at].delivery = Delivery::BellOnly;
    act(
        &mut connection,
        NoticeAction::SaveSettings { settings: chosen },
        NOW_MS,
    );
    assert!(finished(&mut connection, "p1", "activity:1:1", NOW_MS).is_some());
    assert!(!listed(&connection, &query(NoticeFilter::All)).items[0].banner);
}

#[test]
fn the_same_info_event_is_stored_once() {
    let (_folder, mut connection) = store();
    assert!(finished(&mut connection, "p1", "activity:1:1", NOW_MS).is_some());
    assert_eq!(
        finished(&mut connection, "p1", "activity:1:1", NOW_MS + 1),
        None
    );
    assert_eq!(rows(&connection), 1);
}

#[test]
fn an_app_update_is_reported_once_per_version_and_the_engine_lost_once_a_minute() {
    let (_folder, mut connection) = store();
    let update = |version: &str| NoticeAction::Report {
        report: NoticeReport::AppUpdateReady {
            version: version.into(),
        },
    };
    assert_eq!(
        act(&mut connection, update("2.4.1"), NOW_MS),
        Outcome::Applied
    );
    assert_eq!(
        act(&mut connection, update("2.4.1"), NOW_MS + 1),
        Outcome::Applied
    );
    assert_eq!(
        act(&mut connection, update("2.5.0-dev.3"), NOW_MS + 2),
        Outcome::Applied
    );
    let lost = || NoticeAction::Report {
        report: NoticeReport::EngineLost,
    };
    assert_eq!(act(&mut connection, lost(), NOW_MS), Outcome::Applied);
    assert_eq!(
        act(&mut connection, lost(), NOW_MS + 59_000),
        Outcome::Applied
    );
    assert_eq!(
        act(&mut connection, lost(), NOW_MS + 61_000),
        Outcome::Applied
    );
    let page = listed(&connection, &query(NoticeFilter::All));
    let kinds: Vec<NoticeKind> = page.items.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            NoticeKind::EngineLost,
            NoticeKind::EngineLost,
            NoticeKind::AppUpdateReady,
            NoticeKind::AppUpdateReady,
        ]
    );
    assert!(page.items.iter().all(|item| item.project_id.is_none()));
    assert_eq!(page.items[2].target, NoticeTarget::Updates);
    assert_eq!(page.items[0].target, NoticeTarget::Home);
}

#[test]
fn a_reported_version_that_is_not_a_plain_version_is_refused() {
    let (_folder, mut connection) = store();
    for version in [
        "",
        "2.4.1 now",
        "2.4.1\n",
        &"9".repeat(VERSION_BYTES_MAX + 1),
    ] {
        let outcome = act(
            &mut connection,
            NoticeAction::Report {
                report: NoticeReport::AppUpdateReady {
                    version: version.into(),
                },
            },
            NOW_MS,
        );
        assert_eq!(outcome, rejected("notification_report_invalid"));
    }
    assert_eq!(rows(&connection), 0);
}

#[test]
fn item_text_is_one_line_cut_to_its_bound_with_hidden_characters_shown() {
    let (_folder, mut connection) = store();
    let long = format!("Asks\nabout \u{202e}this {}", "x".repeat(TEXT_BYTES_MAX));
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question("p1", "question:q1", &long)],
        NOW_MS,
    );
    let text = &listed(&connection, &query(NoticeFilter::All)).items[0].text;
    assert!(text.len() <= TEXT_BYTES_MAX);
    assert!(text.starts_with("Asks about \\u202e"));
    assert!(!text.contains('\n'));
    assert!(!text.contains('\u{202e}'));
}

#[test]
fn a_full_page_of_the_longest_items_stays_under_its_byte_bound() {
    let (_folder, mut connection) = store();
    let live: Vec<Condition> = (0..i64::from(PAGE_MAX) + 5)
        .map(|at| {
            question(
                &format!("p{at}"),
                &format!("question:q{at}"),
                &"w ".repeat(TEXT_BYTES_MAX),
            )
        })
        .collect();
    synced(&mut connection, &[NoticeKind::Question], &live, NOW_MS);
    let page = listed(&connection, &query(NoticeFilter::All));
    assert!(!page.items.is_empty());
    assert!(page.more);
    assert!(serde_json::to_vec(&page).unwrap().len() <= LISTED_BYTES_MAX);
}

#[test]
fn after_lists_only_items_newer_than_a_seen_id() {
    let (_folder, mut connection) = store();
    let seen = finished(&mut connection, "p1", "activity:1:1", NOW_MS).unwrap();
    let newer = finished(&mut connection, "p1", "activity:2:1", NOW_MS + 1).unwrap();
    let page = listed(
        &connection,
        &NoticeQuery {
            after: Some(seen),
            ..query(NoticeFilter::All)
        },
    );
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        [newer]
    );
    assert_eq!(page.newest, newer);
}

#[test]
fn a_page_limit_outside_one_to_its_bound_is_refused() {
    let (_folder, connection) = store();
    for limit in [0, PAGE_MAX + 1] {
        let wrong = NoticeQuery {
            limit,
            ..query(NoticeFilter::All)
        };
        assert_eq!(
            page(&connection, &wrong).unwrap_err().0,
            "notification_query_invalid"
        );
    }
}

#[test]
fn every_kind_has_one_wire_name_and_its_class() {
    let actions = [
        NoticeKind::Question,
        NoticeKind::AccountSignIn,
        NoticeKind::ResearchPool,
        NoticeKind::MemoryReview,
        NoticeKind::ChangeSetReview,
    ];
    for kind in NoticeKind::ALL {
        assert_eq!(NoticeKind::parse(kind.name()), Some(kind));
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.name());
        let class = if actions.contains(&kind) {
            NoticeClass::Action
        } else {
            NoticeClass::Info
        };
        assert_eq!(kind.class(), class);
    }
    assert_eq!(NoticeKind::parse("wake_paused"), None);
}

#[test]
fn every_kind_has_its_own_short_label_that_every_client_shows() {
    let mut seen = std::collections::BTreeSet::new();
    for kind in NoticeKind::ALL {
        let label = kind.label();
        assert!(!label.is_empty() && label.len() <= 32, "{label}");
        assert!(
            label.chars().next().unwrap().is_ascii_uppercase(),
            "{label}"
        );
        assert!(seen.insert(label), "{label} is shared");
    }
    assert_eq!(NoticeKind::Question.label(), "Question");
    assert_eq!(NoticeKind::ChangeSetLanded.label(), "Change set landed");
    assert_eq!(NoticeKind::TeamAlert.label(), "Team alert");
}

fn fold_of(item: &Notice, filter: NoticeFilter) -> NoticeFold {
    NoticeFold {
        kind: item.kind,
        project_id: item.project_id.clone(),
        filter,
        newest: item.id,
    }
}

fn folds(fold: NoticeFold, change: FoldChange) -> NoticeAction {
    NoticeAction::Folds {
        folds: vec![fold],
        change,
    }
}

#[test]
fn reading_and_deleting_a_fold_reaches_every_item_it_holds_past_its_listed_ids() {
    let (_folder, mut connection) = store();
    for n in 0..40 {
        finished(
            &mut connection,
            "p1",
            &format!("activity:{n}:1"),
            NOW_MS + n,
        )
        .unwrap();
    }
    let folded = listed(
        &connection,
        &NoticeQuery {
            fold: true,
            ..query(NoticeFilter::All)
        },
    );
    let row = fold_of(&folded.items[0], NoticeFilter::All);
    assert_eq!(
        (folded.items[0].folded, folded.items[0].ids.len()),
        (40, FOLD_IDS_MAX)
    );
    let later = finished(&mut connection, "p1", "activity:40:1", NOW_MS + 40).unwrap();
    let read = act(
        &mut connection,
        folds(row.clone(), FoldChange::Read),
        NOW_MS + 50,
    );
    assert_eq!(read, Outcome::Applied);
    let unread = listed(&connection, &query(NoticeFilter::Unread));
    assert_eq!(unread.counts.unread, 1, "every item of the fold is read");
    assert_eq!(texts(&unread).len(), 1);
    assert_eq!(
        unread.items[0].id, later,
        "an item newer than the fold is left"
    );
    act(
        &mut connection,
        folds(row.clone(), FoldChange::Unread),
        NOW_MS + 51,
    );
    assert_eq!(
        listed(&connection, &query(NoticeFilter::Unread))
            .counts
            .unread,
        41
    );
    act(&mut connection, folds(row, FoldChange::Delete), NOW_MS + 52);
    assert_eq!(rows(&connection), 1, "the fold's 40 items are gone");
    assert_eq!(
        listed(&connection, &query(NoticeFilter::All)).items[0].id,
        later
    );
}

#[test]
fn deleting_a_fold_of_pending_actions_hides_them_until_their_state_clears() {
    let (_folder, mut connection) = store();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[
            question("p1", "question:q1", "First"),
            question("p1", "question:q2", "Second"),
        ],
        NOW_MS,
    );
    let folded = listed(
        &connection,
        &NoticeQuery {
            fold: true,
            ..query(NoticeFilter::Pending)
        },
    );
    let row = fold_of(&folded.items[0], NoticeFilter::Pending);
    act(&mut connection, folds(row, FoldChange::Delete), NOW_MS + 1);
    let pending = listed(&connection, &query(NoticeFilter::Pending));
    assert!(pending.items.is_empty());
    assert_eq!(pending.counts.pending, 0);
    assert_eq!(
        rows(&connection),
        2,
        "a pending Action stays as a hidden row"
    );
}

#[test]
fn a_fold_change_with_no_folds_too_many_or_a_negative_bound_is_refused() {
    let (_folder, mut connection) = store();
    let id = finished(&mut connection, "p1", "activity:1:1", NOW_MS).unwrap();
    let fold = NoticeFold {
        kind: NoticeKind::AgentFinished,
        project_id: Some(project("p1")),
        filter: NoticeFilter::All,
        newest: id,
    };
    let empty = NoticeAction::Folds {
        folds: Vec::new(),
        change: FoldChange::Read,
    };
    let many = NoticeAction::Folds {
        folds: vec![fold.clone(); usize::from(PAGE_MAX) + 1],
        change: FoldChange::Read,
    };
    let negative = folds(NoticeFold { newest: -1, ..fold }, FoldChange::Delete);
    for action in [empty, many, negative] {
        assert_eq!(
            act(&mut connection, action, NOW_MS + 1),
            rejected("notification_folds_invalid")
        );
    }
    assert_eq!(
        listed(&connection, &query(NoticeFilter::Unread))
            .counts
            .unread,
        1
    );
    assert_eq!(rows(&connection), 1);
}

fn token() -> String {
    ["gh", "p_", &"a1B2c3D4".repeat(5)].concat()
}

fn stored_texts(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT text FROM owner_notices ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn an_agent_item_with_a_token_is_stored_and_served_without_it() {
    let (_folder, mut connection) = store();
    let secret = token();
    let text = format!("Sub-agent 2 finished: pushed with {secret} to main");
    let tx = connection.transaction().unwrap();
    let id = record(
        &tx,
        &InfoNotice {
            kind: NoticeKind::AgentFinished,
            project_id: Some(&project("p1")),
            subject: "agent:2",
            text: &text,
            target: NoticeTarget::Agents {
                project_id: project("p1"),
            },
        },
        NOW_MS,
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(id.is_some());
    let page = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(
        texts(&page),
        vec!["Sub-agent 2 finished: pushed with [hidden] to main"]
    );
    assert!(stored_texts(&connection).iter().all(|text| !text.contains(&secret)));
}

#[test]
fn a_question_with_a_credential_line_never_reaches_the_store_or_a_client() {
    let (_folder, mut connection) = store();
    let secret = token();
    synced(
        &mut connection,
        &[NoticeKind::Question],
        &[question(
            "p1",
            "question:c9",
            &format!("Master asks: deploy now?\nANTHROPIC_API_KEY={secret}"),
        )],
        NOW_MS,
    );
    let page = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(texts(&page), vec!["[hidden: this line may contain a secret]"]);
    let stored = stored_texts(&connection);
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].contains(&secret));
    assert!(!stored[0].contains("ANTHROPIC_API_KEY"));
}

#[test]
fn a_row_stored_with_a_token_before_redaction_is_served_without_it() {
    let (_folder, connection) = store();
    let secret = token();
    let target = target_text(&NoticeTarget::Agents {
        project_id: project("p1"),
    })
    .unwrap();
    connection
        .execute(
            "INSERT INTO owner_notices(kind,action,project,subject,text,target,created_ms)
             VALUES ('agent_finished',0,'p1','agent:3',?1,?2,?3)",
            params![format!("Sub-agent 3 used {secret}"), target, NOW_MS],
        )
        .unwrap();
    let page = listed(&connection, &query(NoticeFilter::All));
    assert_eq!(texts(&page), vec!["Sub-agent 3 used [hidden]"]);
    let mut folded = query(NoticeFilter::All);
    folded.fold = true;
    assert_eq!(texts(&listed(&connection, &folded)), vec!["Sub-agent 3 used [hidden]"]);
}

#[test]
fn item_text_shown_again_is_unchanged() {
    let secret = token();
    for text in [
        "Master asks: which database?".to_owned(),
        format!("pushed with {secret} to main"),
        format!("first line\npassword: {secret}"),
        format!("-----BEGIN PRIVATE KEY-----\n{secret}\n-----END PRIVATE KEY-----"),
        format!("token: {secret} and /Users/me/work/app/notes.md"),
        "x".repeat(400),
    ] {
        let once = shown(&text);
        assert!(!once.contains(&secret), "{once}");
        assert_eq!(shown(&once), once, "shown twice differs for {text:?}");
    }
}
