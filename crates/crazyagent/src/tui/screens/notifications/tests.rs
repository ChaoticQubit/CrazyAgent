use crate::notifications::{
    ACTION_IDS_MAX, FOLD_IDS_MAX, Notice, NoticeAction, NoticeClass, NoticeCounts, NoticeFilter,
    NoticeKind, NoticePage, NoticeQuery, NoticeTarget, PAGE_MAX,
};
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::OwnerEvent;
use crate::tui::client::notifications::{NoticesEvent, NoticesQuery, NoticesReply};
use crate::tui::ctx::Load;
use crate::tui::keys::Key;
use crate::tui::render_tests::{ready, render, shell_at};
use crate::tui::screens::{Place, ProjectTab, SettingsSection};
use crate::tui::seam::{Arrival, ProjectCard, Request};
use crate::tui::shell::Shell;

const NOW: i64 = 1_793_000_000_000;
const TIDEPOOL: &str = "tidepool";
const SHOP: &str = "plant-shop";

fn id(project: &str) -> ProjectId {
    ProjectId::new(project).unwrap()
}

fn card(project: &str, name: &str) -> ProjectCard {
    ProjectCard {
        id: id(project),
        name: name.into(),
        about: String::new(),
        platforms: Vec::new(),
        pool: None,
        pool_id: None,
        research: Default::default(),
        created_ms: None,
        activity: Default::default(),
    }
}

fn notice(number: i64, kind: NoticeKind, project: Option<&str>, target: NoticeTarget) -> Notice {
    Notice {
        id: number,
        kind,
        class: kind.class(),
        project_id: project.map(id),
        text: format!("{} number {number}", kind.label()),
        target,
        created_ms: NOW - number * 60_000,
        read_ms: None,
        resolved_ms: None,
        folded: 1,
        unread: 1,
        pending: u32::from(kind.class() == NoticeClass::Action),
        ids: vec![number],
        banner: true,
    }
}

fn question(number: i64) -> Notice {
    notice(
        number,
        NoticeKind::Question,
        Some(TIDEPOOL),
        NoticeTarget::Questions {
            project_id: id(TIDEPOOL),
            question: Some(number),
            agent: None,
        },
    )
}

fn list(filter: NoticeFilter, project: Option<&str>, before: Option<i64>) -> NoticesQuery {
    NoticesQuery::Page(NoticeQuery {
        filter,
        project_id: project.map(id),
        kind: None,
        fold: true,
        before,
        after: None,
        limit: PAGE_MAX,
    })
}

fn opened() -> Shell {
    let mut ctx = ready(120, 30);
    ctx.store.projects = Load::Ready(vec![card(TIDEPOOL, "Tidepool"), card(SHOP, "Plant Shop")]);
    shell_at(ctx, Place::Notifications)
}

fn sent(shell: &mut Shell) -> Vec<NoticesQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Notices(query) if *query != NoticesQuery::counts() => Some(*query),
            _ => None,
        })
        .collect()
}

fn changes(shell: &mut Shell) -> Vec<NoticeAction> {
    sent(shell)
        .into_iter()
        .filter_map(|query| match query {
            NoticesQuery::Act { action, .. } => Some(action),
            _ => None,
        })
        .collect()
}

fn reply(query: NoticesQuery, result: Result<NoticesReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Notices(Box::new(NoticesEvent {
        query,
        result,
    }))))
}

fn page(query: NoticesQuery, items: Vec<Notice>, more: bool) -> Arrival {
    let unread = u32::try_from(items.len()).unwrap();
    reply(
        query,
        Ok(NoticesReply::Page(NoticePage {
            newest: items.iter().map(|item| item.id).max().unwrap_or(0),
            items,
            counts: NoticeCounts { unread, pending: 0 },
            more,
        })),
    )
}

fn loaded(items: Vec<Notice>) -> Shell {
    let mut shell = opened();
    let read = sent(&mut shell);
    assert_eq!(read, vec![list(NoticeFilter::All, None, None)]);
    shell.arrive(page(read[0].clone(), items, false));
    shell
}

fn screen(shell: &mut Shell) -> String {
    render(shell, 120, 30).join("\n")
}

#[test]
fn the_list_reads_every_item_of_every_project_folded_when_shown() {
    let mut shell = opened();
    assert_eq!(sent(&mut shell), vec![list(NoticeFilter::All, None, None)]);
    let shown = screen(&mut shell);
    assert!(shown.contains("Notifications"), "{shown}");
}

#[test]
fn f_and_shift_arrows_walk_the_four_filters_and_read_each_one() {
    let mut shell = loaded(Vec::new());
    for (filter, label) in [
        (NoticeFilter::Pending, "Needs action"),
        (NoticeFilter::Unread, "Unread"),
        (NoticeFilter::Done, "Done"),
        (NoticeFilter::All, "All"),
    ] {
        shell.key(Key::Char('f'));
        assert_eq!(sent(&mut shell), vec![list(filter, None, None)], "{label}");
        let shown = screen(&mut shell);
        assert!(shown.contains(&format!("[{label}]")), "{label}: {shown}");
    }
    shell.key(Key::ShiftRight);
    assert_eq!(
        sent(&mut shell),
        vec![list(NoticeFilter::Pending, None, None)]
    );
    shell.key(Key::ShiftLeft);
    assert_eq!(sent(&mut shell), vec![list(NoticeFilter::All, None, None)]);
}

#[test]
fn p_walks_the_project_picker_and_reads_only_that_projects_items() {
    let mut shell = loaded(Vec::new());
    assert!(screen(&mut shell).contains("Project: All projects"));
    shell.key(Key::Char('p'));
    assert_eq!(
        sent(&mut shell),
        vec![list(NoticeFilter::All, Some(TIDEPOOL), None)]
    );
    assert!(screen(&mut shell).contains("Project: Tidepool"));
    shell.key(Key::Char('p'));
    assert_eq!(
        sent(&mut shell),
        vec![list(NoticeFilter::All, Some(SHOP), None)]
    );
    shell.key(Key::Char('P'));
    assert_eq!(
        sent(&mut shell),
        vec![list(NoticeFilter::All, Some(TIDEPOOL), None)]
    );
    shell.key(Key::Char('P'));
    assert_eq!(sent(&mut shell), vec![list(NoticeFilter::All, None, None)]);
    assert!(screen(&mut shell).contains("Project: All projects"));
}

#[test]
fn rows_show_the_unread_mark_kind_text_project_and_a_fold_count() {
    let mut folded = notice(
        2,
        NoticeKind::AgentFinished,
        Some(SHOP),
        NoticeTarget::Agents {
            project_id: id(SHOP),
        },
    );
    folded.folded = 3;
    folded.ids = vec![2, 3, 4];
    folded.unread = 0;
    folded.read_ms = Some(NOW);
    let mut shell = loaded(vec![question(1), folded]);
    let shown = screen(&mut shell);
    let first = shown
        .lines()
        .find(|line| line.contains("Question number 1"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(first.contains('\u{2022}'), "unread mark: {first}");
    assert!(first.contains(NoticeKind::Question.label()), "{first}");
    assert!(first.contains("Tidepool"), "{first}");
    assert!(first.contains("needs action"), "{first}");
    let second = shown
        .lines()
        .find(|line| line.contains("Agent finished number 2"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(!second.contains('\u{2022}'), "read: {second}");
    assert!(second.contains("Plant Shop"), "{second}");
    assert!(second.contains("3 items"), "{second}");
}

#[test]
fn each_filter_says_what_an_empty_list_means() {
    let mut shell = loaded(Vec::new());
    assert!(screen(&mut shell).contains("No notifications yet."));
    for (filter, empty) in [
        (NoticeFilter::Pending, "Nothing needs your action."),
        (NoticeFilter::Unread, "Everything is read."),
        (NoticeFilter::Done, "No resolved items yet."),
    ] {
        shell.key(Key::Char('f'));
        let read = sent(&mut shell);
        assert_eq!(read, vec![list(filter, None, None)]);
        shell.arrive(page(read[0].clone(), Vec::new(), false));
        let shown = screen(&mut shell);
        assert!(shown.contains(empty), "{filter:?}: {shown}");
    }
}

#[test]
fn enter_opens_each_items_page_and_marks_an_unread_item_read() {
    let project = |tab| Place::Project {
        id: id(TIDEPOOL),
        name: "Tidepool".into(),
        tab,
    };
    let cases = [
        (
            NoticeTarget::Questions {
                project_id: id(TIDEPOOL),
                question: Some(4),
                agent: None,
            },
            project(ProjectTab::Questions),
        ),
        (
            NoticeTarget::AccountSignIn {
                account: CredentialRef::new("account-1").unwrap(),
            },
            Place::Settings(SettingsSection::Accounts),
        ),
        (
            NoticeTarget::ProjectPool {
                project_id: id(TIDEPOOL),
            },
            project(ProjectTab::Settings),
        ),
        (
            NoticeTarget::Memories {
                project_id: id(TIDEPOOL),
            },
            Place::ProjectMemory {
                id: id(TIDEPOOL),
                name: "Tidepool".into(),
            },
        ),
        (
            NoticeTarget::ChangeSet {
                project_id: id(TIDEPOOL),
                change_set: None,
            },
            project(ProjectTab::Changes),
        ),
        (
            NoticeTarget::Agents {
                project_id: id(TIDEPOOL),
            },
            project(ProjectTab::Agents),
        ),
        (
            NoticeTarget::Conversation {
                project_id: id(TIDEPOOL),
            },
            project(ProjectTab::Conversation),
        ),
        (
            NoticeTarget::Accounts,
            Place::Settings(SettingsSection::Accounts),
        ),
        (NoticeTarget::Usage, Place::Usage),
        (
            NoticeTarget::Harnesses,
            Place::Settings(SettingsSection::Harnesses),
        ),
        (NoticeTarget::Home, Place::Home),
    ];
    for (target, place) in cases {
        let mut item = question(7);
        item.ids = vec![7, 8];
        item.target = target.clone();
        let mut shell = loaded(vec![item]);
        shell.key(Key::Enter);
        assert_eq!(shell.place(), Some(place), "{target:?}");
        assert_eq!(
            changes(&mut shell),
            vec![NoticeAction::Read { ids: vec![7, 8] }],
            "{target:?}"
        );
    }

    let mut item = question(7);
    item.unread = 0;
    item.read_ms = Some(NOW);
    item.target = NoticeTarget::Usage;
    let mut shell = loaded(vec![item]);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Usage));
    assert_eq!(changes(&mut shell), Vec::new(), "a read item stays read");
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Notifications), "Esc returns");
}

#[test]
fn enter_stays_and_says_why_for_an_update_or_a_project_no_longer_listed() {
    let mut update = question(7);
    update.target = NoticeTarget::Updates;
    let mut shell = loaded(vec![update]);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Notifications));
    assert!(screen(&mut shell).contains("Install updates from the desktop app."));
    assert_eq!(
        changes(&mut shell),
        vec![NoticeAction::Read { ids: vec![7] }]
    );

    let mut gone = question(7);
    gone.target = NoticeTarget::Agents {
        project_id: id("removed-project"),
    };
    let mut shell = loaded(vec![gone]);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Notifications));
    assert!(screen(&mut shell).contains("That project is no longer in your projects."));
}

#[test]
fn space_marks_rows_and_r_u_d_act_on_every_id_of_the_marked_items() {
    let mut folded = question(5);
    folded.ids = vec![5, 6, 7];
    folded.folded = 3;
    let fresh = || {
        let mut shell = loaded(vec![question(1), folded.clone(), question(9)]);
        shell.key(Key::Char(' '));
        shell.key(Key::Down);
        shell.key(Key::Char(' '));
        shell
    };
    for (key, action) in [
        (
            'r',
            NoticeAction::Read {
                ids: vec![1, 5, 6, 7],
            },
        ),
        (
            'u',
            NoticeAction::Unread {
                ids: vec![1, 5, 6, 7],
            },
        ),
        (
            'd',
            NoticeAction::Delete {
                ids: vec![1, 5, 6, 7],
            },
        ),
    ] {
        let mut shell = fresh();
        assert!(screen(&mut shell).contains("2 selected"));
        shell.key(Key::Char(key));
        assert_eq!(changes(&mut shell), vec![action], "{key}");
        assert!(
            !screen(&mut shell).contains("selected"),
            "{key} clears marks"
        );
    }

    let mut shell = loaded(vec![question(1), folded.clone(), question(9)]);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Char('u'));
    assert_eq!(
        changes(&mut shell),
        vec![NoticeAction::Unread { ids: vec![9] }],
        "no marks act on the row under the cursor"
    );
}

#[test]
fn a_marks_every_item_of_the_picked_project_read() {
    let mut shell = loaded(vec![question(1)]);
    shell.key(Key::Char('a'));
    assert_eq!(
        changes(&mut shell),
        vec![NoticeAction::ReadAll { project_id: None }]
    );
    shell.key(Key::Char('p'));
    let read = sent(&mut shell);
    shell.arrive(page(read[0].clone(), vec![question(1)], false));
    shell.key(Key::Char('a'));
    assert_eq!(
        changes(&mut shell),
        vec![NoticeAction::ReadAll {
            project_id: Some(id(TIDEPOOL))
        }]
    );
}

#[test]
fn a_change_of_more_ids_than_one_action_holds_goes_out_in_chunks() {
    let items: Vec<Notice> = (0..10)
        .map(|row| {
            let first = row * 100;
            let mut item = question(first + 1);
            item.ids = (first + 1..=first + i64::try_from(FOLD_IDS_MAX).unwrap()).collect();
            item
        })
        .collect();
    let every: Vec<i64> = items.iter().flat_map(|item| item.ids.clone()).collect();
    assert!(every.len() > ACTION_IDS_MAX);
    let mut shell = loaded(items);
    for _ in 0..10 {
        shell.key(Key::Char(' '));
        shell.key(Key::Down);
    }
    shell.key(Key::Char('r'));
    let sent = changes(&mut shell);
    assert_eq!(
        sent,
        every
            .chunks(ACTION_IDS_MAX)
            .map(|ids| NoticeAction::Read { ids: ids.to_vec() })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_change_that_took_effect_reads_the_list_again_and_a_refused_one_says_so() {
    let mut shell = loaded(vec![question(1)]);
    shell.key(Key::Char('r'));
    let act = sent(&mut shell).remove(0);
    assert!(matches!(act, NoticesQuery::Act { .. }));
    shell.arrive(reply(act.clone(), Ok(NoticesReply::Done)));
    assert_eq!(sent(&mut shell), vec![list(NoticeFilter::All, None, None)]);
    shell.arrive(reply(act, Err("notification_ids_invalid".into())));
    assert!(screen(&mut shell).contains("The notifications were not changed."));
}

#[test]
fn go_to_opens_the_list_and_esc_clears_marks_before_going_back() {
    let mut ctx = ready(120, 30);
    ctx.store.projects = Load::Ready(vec![card(TIDEPOOL, "Tidepool")]);
    let mut shell = shell_at(ctx, Place::Home);
    shell.key(Key::Ctrl('k'));
    for character in "notifications".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Notifications));
    let read = sent(&mut shell);
    assert_eq!(read, vec![list(NoticeFilter::All, None, None)]);
    shell.arrive(page(read[0].clone(), vec![question(1)], false));
    shell.key(Key::Char(' '));
    assert!(screen(&mut shell).contains("1 selected"));
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Notifications));
    assert!(!screen(&mut shell).contains("selected"));
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
}

#[test]
fn the_next_page_is_read_at_the_end_of_the_list_and_added_below() {
    let mut shell = opened();
    let first = sent(&mut shell).remove(0);
    shell.arrive(page(first, vec![question(3), question(2)], true));
    shell.key(Key::Down);
    let next = sent(&mut shell);
    assert_eq!(next, vec![list(NoticeFilter::All, None, Some(2))]);
    shell.arrive(page(next[0].clone(), vec![question(1)], false));
    shell.key(Key::Down);
    assert_eq!(sent(&mut shell), Vec::new(), "no more pages");
    let shown = screen(&mut shell);
    for number in [3, 2, 1] {
        assert!(
            shown.contains(&format!("Question number {number}")),
            "{shown}"
        );
    }
}
