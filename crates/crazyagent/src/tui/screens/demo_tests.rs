use super::*;
use crate::accounts::AccountStatus;
use crate::demo::{DemoPool, DemoSteps, DemoView, NAME, PROJECT_ID};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::OwnerEvent;
use crate::tui::client::demo::{DemoEvent, DemoQuery, DemoReply};
use crate::tui::ctx::Load;
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::{AccountRow, Arrival, PoolChange, Request};
use crate::tui::shell::Shell;

const GOAL: &str = "Add a dark mode switch to the page and a test for it.";

fn pool_ref(id: &str) -> CredentialRef {
    CredentialRef::new(id).unwrap()
}

fn view(project: Option<&str>, pools: &[(&str, Option<&str>)]) -> DemoView {
    DemoView {
        project_id: project.map(|id| ProjectId::new(id).unwrap()),
        name: NAME.into(),
        created_ms: project.map(|_| 1),
        steps: DemoSteps {
            created: project.is_some(),
            ..DemoSteps::default()
        },
        goal: GOAL.into(),
        pools: pools
            .iter()
            .map(|(id, unavailable)| DemoPool {
                pool: pool_ref(id),
                unavailable: unavailable.map(str::to_owned),
            })
            .collect(),
    }
}

fn summary(id: &str, name: &str, members: &[&str]) -> PoolSummary {
    PoolSummary {
        id: pool_ref(id),
        name: name.into(),
        summary: String::new(),
        members: members
            .iter()
            .map(|member| PoolMember {
                id: pool_ref(member),
                kind: MemberKind::Claude,
                provider: "Claude".into(),
                status: MemberStatus::SignedIn,
                status_label: String::new(),
            })
            .collect(),
        project_count: 0,
        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
    }
}

fn account(id: &str, name: &str, status: AccountStatus) -> AccountRow {
    AccountRow {
        id: pool_ref(id),
        name: name.into(),
        provider: "Claude".into(),
        status: status.label().into(),
        no_window: None,
    }
}

fn demo(query: DemoQuery, result: Result<DemoReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Demo(Box::new(DemoEvent {
        query,
        result,
    }))))
}

fn read(view: DemoView) -> Arrival {
    demo(DemoQuery::Read, Ok(DemoReply::View(Box::new(view))))
}

/// The demo page on an engine with the given demo read, pools and accounts, every request
/// it sent while opening already taken. Inputs: the read, pools and accounts. Output: the shell.
fn page(view: DemoView, pools: Vec<PoolSummary>, accounts: Vec<AccountRow>) -> Shell {
    let mut ctx = ctx(100, 40);
    ctx.store.pools = Load::Ready(pools);
    ctx.store.accounts = Load::Ready(accounts);
    let mut shell = Shell::with(ctx, Box::new(DemoPage::new()));
    shell.arrive(read(view));
    while shell.ctx.next_request().is_some() {}
    shell
}

fn one_ready_pool() -> Shell {
    page(
        view(None, &[("pool-a", None)]),
        vec![summary("pool-a", "Main", &["acc-1"])],
        vec![account("acc-1", "Work Claude", AccountStatus::SignedIn)],
    )
}

fn creates(shell: &mut Shell) -> Vec<CredentialRef> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Demo(query) => match *query {
                DemoQuery::Create { pool, .. } => Some(pool),
                DemoQuery::Read => None,
            },
            _ => None,
        })
        .collect()
}

fn sent(shell: &mut Shell) -> (usize, Vec<(Option<CredentialRef>, ProjectId)>) {
    let mut reads = 0;
    let mut assigns = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        match request {
            Request::Demo(query) if matches!(*query, DemoQuery::Read) => reads += 1,
            Request::EditPool {
                change: PoolChange::Assign { pool, project },
            } => assigns.push((pool, project)),
            _ => {}
        }
    }
    (reads, assigns)
}

fn text(rows: &[String]) -> String {
    rows.join("\n")
}

#[test]
fn the_demo_page_shows_the_cost_note_before_any_pool() {
    let mut shell = one_ready_pool();
    let rows = render(&mut shell, 100, 40);
    let note = rows
        .iter()
        .position(|row| row.contains("The demo is a real run."))
        .expect("cost note shown");
    let first_pool = rows
        .iter()
        .position(|row| row.contains("( ) Main"))
        .expect("pool shown");
    assert!(note < first_pool, "{rows:#?}");
    assert!(text(&rows).contains("may be used"), "{rows:#?}");
}

#[test]
fn no_pool_is_picked_for_the_owner_and_enter_on_the_highlighted_row_creates_nothing() {
    let mut shell = one_ready_pool();
    let rows = render(&mut shell, 100, 40);
    assert!(!text(&rows).contains("(*)"), "{rows:#?}");
    shell.key(Key::Enter);
    assert!(creates(&mut shell).is_empty(), "the cursor is not a pick");
    assert!(text(&render(&mut shell, 100, 40)).contains(PICK_FIRST));
    shell.key(Key::Char(' '));
    assert!(text(&render(&mut shell, 100, 40)).contains("(*) Main"));
    shell.key(Key::Enter);
    assert_eq!(creates(&mut shell), vec![pool_ref("pool-a")]);
}

#[test]
fn a_pool_the_engine_marks_unavailable_shows_its_reason_and_cannot_be_picked() {
    let mut shell = page(
        view(
            None,
            &[
                ("pool-r", Some("demo_harness_unavailable")),
                ("pool-n", Some("demo_pool_not_ready")),
                ("pool-a", None),
            ],
        ),
        vec![
            summary("pool-r", "Local models", &["acc-r"]),
            summary("pool-n", "Signed out", &["acc-n"]),
            summary("pool-a", "Main", &["acc-1"]),
        ],
        vec![
            account("acc-r", "Ollama", AccountStatus::KeySaved),
            account("acc-n", "Old", AccountStatus::NotSignedIn),
            account("acc-1", "Work Claude", AccountStatus::SignedIn),
        ],
    );
    let rows = text(&render(&mut shell, 100, 40));
    assert!(rows.contains("rcli"), "{rows}");
    assert!(rows.contains(pool_reason("demo_pool_not_ready").as_str()), "{rows}");
    shell.key(Key::Char(' '));
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    assert!(creates(&mut shell).is_empty());
    assert!(!text(&render(&mut shell, 100, 40)).contains("(*)"));
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    assert_eq!(creates(&mut shell), vec![pool_ref("pool-a")]);
}

#[test]
fn pool_rows_name_ready_accounts_by_their_accounts_words_and_keep_the_engine_order() {
    let rows = pool_rows(
        &view(None, &[("pool-b", Some("demo_new_code")), ("pool-a", None)]),
        &[summary("pool-a", "Main", &["acc-1", "acc-2", "acc-3", "acc-gone"])],
        &[
            account("acc-1", "Work", AccountStatus::SignedIn),
            account("acc-2", "Work", AccountStatus::KeySaved),
            account("acc-3", "Spare", AccountStatus::NotSignedIn),
        ],
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "pool-b");
    assert_eq!(rows[0].reason.as_deref(), Some(format!("{POOL_UNKNOWN} (demo_new_code)").as_str()));
    assert_eq!(rows[1].name, "Main");
    assert_eq!(rows[1].ready, vec!["Work (acc-1)".to_owned(), "Work (acc-2)".to_owned()]);
    assert_eq!(rows[1].reason, None);
}

#[test]
fn with_no_pool_the_page_offers_create_a_pool_and_no_create() {
    let mut shell = page(view(None, &[]), Vec::new(), Vec::new());
    let rows = text(&render(&mut shell, 100, 40));
    assert!(rows.contains(NO_POOL), "{rows}");
    assert!(!rows.contains("( )"), "{rows}");
    shell.key(Key::Enter);
    assert!(creates(&mut shell).is_empty());
    shell.key(Key::Char('p'));
    assert_eq!(shell.place(), Some(Place::NewPool));
}

#[test]
fn an_existing_demo_opens_instead_of_making_another() {
    let mut shell = page(
        view(Some(PROJECT_ID), &[("pool-a", None)]),
        vec![summary("pool-a", "Main", &["acc-1"])],
        vec![account("acc-1", "Work Claude", AccountStatus::SignedIn)],
    );
    let rows = text(&render(&mut shell, 100, 40));
    assert!(rows.contains(EXISTS), "{rows}");
    assert!(!rows.contains("( )"), "{rows}");
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    assert!(creates(&mut shell).is_empty());
    assert!(
        matches!(shell.place(), Some(Place::Project { ref id, .. }) if id.as_str() == PROJECT_ID),
        "{:?}",
        shell.place()
    );
}

#[test]
fn an_applied_create_gives_the_picked_pool_to_the_demo_then_opens_it() {
    let mut shell = one_ready_pool();
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let pool = creates(&mut shell).pop().unwrap();
    shell.arrive(demo(
        DemoQuery::Create {
            pool: pool.clone(),
            generation: 1,
        },
        Ok(DemoReply::Created),
    ));
    let (_, assigns) = sent(&mut shell);
    assert_eq!(assigns, vec![(Some(pool), ProjectId::new(PROJECT_ID).unwrap())]);
    assert_eq!(shell.place(), Some(Place::Demo), "the demo opens only once its pool is given");
    shell.arrive(Arrival::PoolEdited(Ok(())));
    assert!(
        matches!(shell.place(), Some(Place::Project { ref id, tab: ProjectTab::Conversation, .. }) if id.as_str() == PROJECT_ID),
        "{:?}",
        shell.place()
    );
}

#[test]
fn a_pool_that_could_not_be_given_still_opens_the_demo_with_the_line_to_choose_it_again() {
    let mut shell = one_ready_pool();
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let pool = creates(&mut shell).pop().unwrap();
    shell.arrive(demo(
        DemoQuery::Create {
            pool,
            generation: 1,
        },
        Ok(DemoReply::Created),
    ));
    let _ = sent(&mut shell);
    shell.arrive(Arrival::PoolEdited(Err("settings_read_only".into())));
    let said = shell.ctx.message().expect("a line is said");
    let line = format!("{} {}", said.lead, said.rest);
    assert!(line.contains(POOL_UNASSIGNED), "{line}");
    assert!(matches!(shell.place(), Some(Place::Project { .. })));
}

#[test]
fn a_refused_create_gives_no_pool_and_says_why_in_the_demo_s_words() {
    let mut shell = one_ready_pool();
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let pool = creates(&mut shell).pop().unwrap();
    shell.arrive(demo(
        DemoQuery::Create {
            pool,
            generation: 1,
        },
        Err("demo_name_taken".into()),
    ));
    let (_, assigns) = sent(&mut shell);
    assert!(assigns.is_empty());
    assert_eq!(shell.place(), Some(Place::Demo));
    assert!(text(&render(&mut shell, 100, 40)).contains("already holds a project or folder named CrazyAgent demo"));
    assert!(refusal("demo_brand_new").contains("demo_brand_new"));
    for (code, sentence) in REFUSALS {
        assert_eq!(refusal(code), sentence, "{code}");
    }
}

#[test]
fn an_unknown_create_outcome_reads_the_demo_again_and_goes_on_only_when_it_exists() {
    let mut shell = one_ready_pool();
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let pool = creates(&mut shell).pop().unwrap();
    shell.arrive(demo(
        DemoQuery::Create {
            pool: pool.clone(),
            generation: 1,
        },
        Err(OUTCOME_UNKNOWN.into()),
    ));
    let (reads, assigns) = sent(&mut shell);
    assert_eq!((reads, assigns.len()), (1, 0));
    shell.arrive(read(view(Some(PROJECT_ID), &[("pool-a", None)])));
    let (_, assigns) = sent(&mut shell);
    assert_eq!(assigns, vec![(Some(pool), ProjectId::new(PROJECT_ID).unwrap())]);

    let mut lost = one_ready_pool();
    lost.key(Key::Char(' '));
    lost.key(Key::Enter);
    let pool = creates(&mut lost).pop().unwrap();
    lost.arrive(demo(
        DemoQuery::Create {
            pool,
            generation: 1,
        },
        Err(OUTCOME_UNKNOWN.into()),
    ));
    let _ = sent(&mut lost);
    lost.arrive(read(view(None, &[("pool-a", None)])));
    let (_, assigns) = sent(&mut lost);
    assert!(assigns.is_empty());
    assert!(text(&render(&mut lost, 100, 40)).contains(UNCONFIRMED));
}

#[test]
fn the_guide_ticks_each_step_from_the_demo_read_and_shows_only_on_the_demo_project() {
    let demo_id = ProjectId::new(PROJECT_ID).unwrap();
    assert!(guide(&view(None, &[]), &demo_id).is_none());
    assert!(guide(&view(Some(PROJECT_ID), &[]), &ProjectId::new("another").unwrap()).is_none());
    let mut read = view(Some(PROJECT_ID), &[]);
    read.steps.goal_sent = true;
    read.steps.agent_started = true;
    let shown = guide(&read, &demo_id).unwrap();
    assert_eq!(
        shown.steps.iter().map(|step| (step.id, step.done)).collect::<Vec<_>>(),
        vec![("goal", true), ("agents", true), ("question", false), ("changes", false), ("delete", false)]
    );
    assert_eq!(shown.current, 2);
    read.steps.question_answered = true;
    read.steps.change_set_accepted = true;
    assert_eq!(guide(&read, &demo_id).unwrap().current, 4, "delete never ticks");
}

#[test]
fn the_guide_lines_show_the_goal_every_step_and_the_current_step_s_place() {
    let ctx = ctx(100, 40);
    let mut read = view(Some(PROJECT_ID), &[]);
    read.steps.goal_sent = true;
    let lines = guide_lines(&read, &ProjectId::new(PROJECT_ID).unwrap(), 96, &ctx);
    let shown: Vec<String> = lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    let all = shown.join("\n");
    assert!(all.contains(GUIDE_TITLE), "{all}");
    assert!(all.contains(GOAL), "{all}");
    for (_, title, _) in STEPS {
        assert!(all.contains(title), "{title}: {all}");
    }
    assert!(all.contains("Step 2 of 5"), "{all}");
    assert!(guide_lines(&read, &ProjectId::new("another").unwrap(), 96, &ctx).is_empty());
}

#[test]
fn the_home_entry_tries_the_demo_until_one_exists_then_opens_it() {
    assert_eq!(entry(&view(None, &[])), (ENTRY_START, Place::Demo));
    let (label, place) = entry(&view(Some(PROJECT_ID), &[]));
    assert_eq!(label, ENTRY_OPEN);
    assert!(
        matches!(place, Place::Project { ref id, ref name, tab: ProjectTab::Conversation } if id.as_str() == PROJECT_ID && name == NAME),
        "{place:?}"
    );
}
