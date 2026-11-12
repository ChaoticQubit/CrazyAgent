use super::*;
use crate::notifications::{NoticeAction, NoticeCounts, NoticePage};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::tui::client::OwnerEvent;
use crate::tui::ctx::Load;
use crate::tui::keys::Key;
use crate::tui::render_tests::{ready, render, shell_at};
use crate::tui::screens::{Place, ProjectTab, SettingsSection};
use crate::tui::seam::{Arrival, ProjectCard, Request};
use crate::tui::shell::Shell;

const NOW: i64 = 1_793_000_000_000;

fn project() -> ProjectId {
    ProjectId::new("tidepool").unwrap()
}

fn account(name: &str) -> CredentialRef {
    CredentialRef::new(name).unwrap()
}

fn notice(id: i64, kind: NoticeKind, text: &str, target: NoticeTarget) -> Notice {
    Notice {
        id,
        kind,
        class: kind.class(),
        project_id: target.project().cloned(),
        text: text.into(),
        target,
        created_ms: NOW - id * 60_000,
        read_ms: None,
        resolved_ms: None,
        folded: 1,
        unread: 1,
        pending: 1,
        ids: vec![id],
        banner: true,
    }
}

fn question(id: i64, text: &str) -> Notice {
    notice(
        id,
        NoticeKind::Question,
        text,
        NoticeTarget::Questions {
            project_id: project(),
            question: Some(id),
            agent: None,
        },
    )
}

fn sign_in(id: i64, name: &str) -> Notice {
    notice(
        id,
        NoticeKind::AccountSignIn,
        &format!("{name} needs you to sign in again."),
        NoticeTarget::AccountSignIn {
            account: account(name),
        },
    )
}

fn page(items: Vec<Notice>) -> Result<NoticesReply, String> {
    Ok(NoticesReply::Page(NoticePage {
        newest: items.iter().map(|item| item.id).max().unwrap_or(0),
        counts: NoticeCounts {
            unread: 0,
            pending: 0,
        },
        more: false,
        items,
    }))
}

fn event(query: NoticesQuery, result: Result<NoticesReply, String>) -> NoticesEvent {
    NoticesEvent { query, result }
}

fn both(needs: &mut NeedsYou, now_ms: i64) -> (NoticesQuery, NoticesQuery) {
    let due = needs.due(now_ms);
    assert_eq!(
        due,
        vec![NeedsYou::own_query(&project()), NeedsYou::sign_ins_query()]
    );
    (due[0].clone(), due[1].clone())
}

#[test]
fn the_reads_are_the_projects_pending_items_folded_and_every_pending_sign_in() {
    let NoticesQuery::Page(own) = NeedsYou::own_query(&project()) else {
        panic!("the project's read is a page");
    };
    assert_eq!(
        (own.filter, own.project_id, own.kind, own.fold),
        (NoticeFilter::Pending, Some(project()), None, true)
    );
    let NoticesQuery::Page(signs) = NeedsYou::sign_ins_query() else {
        panic!("the sign-ins read is a page");
    };
    assert_eq!(
        (signs.filter, signs.project_id, signs.kind, signs.fold),
        (
            NoticeFilter::Pending,
            None,
            Some(NoticeKind::AccountSignIn),
            false
        ),
        "a sign-in carries no project, so each account is its own item"
    );

    let mut needs = NeedsYou::new(project());
    let (own, signs) = both(&mut needs, NOW);
    assert_eq!(
        needs.due(NOW + POLL_MS),
        Vec::new(),
        "one read of each at a time"
    );
    needs.arrive(&event(own, page(Vec::new())));
    needs.arrive(&event(signs, page(Vec::new())));
    assert_eq!(needs.due(NOW + 1), Vec::new(), "read again after POLL_MS");
    let (own, signs) = both(&mut needs, NOW + POLL_MS);
    needs.arrive(&event(own, page(Vec::new())));
    needs.arrive(&event(signs, page(Vec::new())));
    needs.refresh();
    both(&mut needs, NOW + POLL_MS + 1);
}

#[test]
fn rows_join_the_projects_items_with_the_sign_ins_of_its_pool_accounts_newest_first() {
    let mut needs = NeedsYou::new(project());
    needs.set_accounts(vec![account("acct-1")]);
    let (own, signs) = both(&mut needs, NOW);
    assert!(needs.rows().is_empty(), "nothing while it loads");
    needs.arrive(&event(
        signs,
        page(vec![sign_in(9, "acct-9"), sign_in(7, "acct-1")]),
    ));
    assert!(
        needs.rows().is_empty(),
        "sign-ins wait for the project's own read"
    );
    needs.arrive(&event(
        own,
        page(vec![
            question(8, "Which page ships first?"),
            question(2, "Which colour should the shop use?"),
        ]),
    ));
    let ids: Vec<i64> = needs.rows().iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![8, 7, 2], "another pool's sign-in is left out");
}

#[test]
fn a_failed_read_says_so_until_one_lands_and_a_failed_sign_in_read_keeps_the_projects_items() {
    let mut needs = NeedsYou::new(project());
    let (own, signs) = both(&mut needs, NOW);
    needs.arrive(&event(own, Err("owner_busy".into())));
    needs.arrive(&event(signs, Err("owner_busy".into())));
    assert_eq!(needs.problem(), Some("owner_busy"));
    assert!(needs.rows().is_empty());

    let (own, signs) = both(&mut needs, NOW + POLL_MS);
    needs.arrive(&event(own, page(vec![question(3, "Which page?")])));
    needs.arrive(&event(signs, Err("owner_busy".into())));
    assert_eq!(needs.problem(), None);
    assert_eq!(needs.rows().len(), 1);

    let (own, signs) = both(&mut needs, NOW + 2 * POLL_MS);
    needs.arrive(&event(own, Err("owner_busy".into())));
    needs.arrive(&event(signs, page(Vec::new())));
    assert_eq!(needs.problem(), None, "a later failure keeps what was read");
    assert_eq!(needs.rows().len(), 1);
}

#[test]
fn a_reply_this_view_did_not_ask_for_is_left_alone() {
    let mut needs = NeedsYou::new(project());
    let other = NeedsYou::own_query(&ProjectId::new("plant-shop").unwrap());
    needs.arrive(&event(other, page(vec![question(4, "Elsewhere?")])));
    assert!(needs.rows().is_empty());
    both(&mut needs, NOW);
}

fn overview() -> Shell {
    let mut ctx = ready(120, 36);
    let pool = account("pool-1");
    ctx.store.projects = Load::Ready(vec![ProjectCard {
        id: project(),
        name: "Tidepool".into(),
        about: String::new(),
        platforms: Vec::new(),
        pool: None,
        pool_id: Some(pool.clone()),
        research: Default::default(),
        created_ms: None,
        activity: Default::default(),
    }]);
    ctx.store.pools = Load::Ready(vec![PoolSummary {
        id: pool,
        name: "Main".into(),
        summary: String::new(),
        members: vec![PoolMember {
            id: account("acct-1"),
            kind: MemberKind::Claude,
            provider: "Claude".into(),
            status: MemberStatus::NotSignedIn,
            status_label: "Not signed in".into(),
        }],
        project_count: 1,
        provider_quota_policy: Default::default(),
    }]);
    shell_at(
        ctx,
        Place::Project {
            id: project(),
            name: "Tidepool".into(),
            tab: ProjectTab::Overview,
        },
    )
}

fn notice_requests(shell: &mut Shell) -> Vec<NoticesQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Notices(query) => Some(*query),
            _ => None,
        })
        .collect()
}

fn arrive(shell: &mut Shell, query: NoticesQuery, result: Result<NoticesReply, String>) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Notices(Box::new(
        event(query, result),
    )))));
}

fn listed() -> Shell {
    let mut shell = overview();
    let asked = notice_requests(&mut shell);
    let own = NeedsYou::own_query(&project());
    let signs = NeedsYou::sign_ins_query();
    assert!(
        asked.contains(&own) && asked.contains(&signs),
        "the Overview reads what needs you: {asked:?}"
    );
    arrive(
        &mut shell,
        signs,
        page(vec![sign_in(9, "acct-9"), sign_in(7, "acct-1")]),
    );
    arrive(
        &mut shell,
        own,
        page(vec![question(8, "Which page ships first?")]),
    );
    shell
}

fn read_changes(shell: &mut Shell) -> Vec<NoticeAction> {
    notice_requests(shell)
        .into_iter()
        .filter_map(|query| match query {
            NoticesQuery::Act { action, .. } => Some(action),
            _ => None,
        })
        .collect()
}

#[test]
fn the_overview_lists_what_needs_you_from_the_store_and_enter_opens_the_picked_item() {
    let mut shell = listed();
    let shown = render(&mut shell, 120, 36).join("\n");
    assert!(shown.contains("Needs you  2"), "{shown}");
    assert!(shown.contains("Which page ships first?"), "{shown}");
    assert!(
        shown.contains("acct-1 needs you to sign in again."),
        "{shown}"
    );
    assert!(!shown.contains("acct-9"), "{shown}");
    assert!(
        !shown.contains("Save to memory?"),
        "the inline memory card is gone (DEC-136a decision 4): {shown}"
    );
    let line = shown
        .lines()
        .find(|line| line.contains("Which page ships first?"))
        .unwrap();
    assert!(
        line.contains("> "),
        "the newest item is picked first: {line}"
    );

    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: project(),
            name: "Tidepool".into(),
            tab: ProjectTab::Questions,
        })
    );
    assert_eq!(
        read_changes(&mut shell),
        vec![NoticeAction::Read { ids: vec![8] }]
    );

    let mut shell = listed();
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Settings(SettingsSection::Accounts))
    );
    assert_eq!(
        read_changes(&mut shell),
        vec![NoticeAction::Read { ids: vec![7] }]
    );
}

#[test]
fn n_on_the_overview_opens_the_notifications_list_since_nothing_there_asks() {
    let mut shell = listed();
    shell.key(Key::Char('n'));
    assert_eq!(shell.place(), Some(Place::Notifications));
}
