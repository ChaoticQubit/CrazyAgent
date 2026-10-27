use super::super::settings_add_account::{AddAccount, choices, key_problem, provider_tiles};
use super::*;
use crate::accounts::{AccountStatus, AccountView, Provider, SignInMethod, catalog};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::tui::client::accounts::AccountsReply;
use crate::tui::client::{OwnerEvent, settings::SettingsEvent};
use crate::tui::ctx::{Link, Load};
use crate::tui::glyphs::UNICODE;
use crate::tui::shell::Shell;
use crate::tui::theme::{Appearance, Colors, Depth};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};

pub(crate) fn ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    );
    ctx.width = width;
    ctx.height = height;
    ctx.tick = 4;
    ctx.link = Link::Ready;
    ctx
}

pub(crate) fn render(shell: &mut Shell, width: u16, height: u16) -> Vec<String> {
    let away = Position::new(u16::MAX, u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.backend_mut().set_cursor_position(away).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().ok();
    let shown = cursor != Some(away);
    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            if shown && cursor == Some(Position::new(x, y)) {
                line.push('█');
            } else {
                line.push_str(symbol);
            }
            skip = crate::tui::text::width(symbol).saturating_sub(1);
        }
        rows.push(line);
    }
    rows
}

pub(crate) fn fixture(name: &str, swaps: &[(&str, &str)]) -> Vec<String> {
    let path = format!("{}/src/tui/frames/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {path}"));
    text.lines()
        .map(|row| {
            let mut row = row.to_owned();
            for (from, to) in swaps {
                let from_width = crate::tui::text::width(from);
                let to_width = crate::tui::text::width(to);
                if row.contains(from) && to_width <= from_width {
                    row = row.replacen(
                        from,
                        &format!("{to}{}", " ".repeat(from_width - to_width)),
                        1,
                    );
                }
            }
            row
        })
        .collect()
}

/// Compares rendered rows with a design frame; `rows` limits the check to the
/// rows this build draws the same as the design.
pub(crate) fn check(name: &str, actual: &[String], swaps: &[(&str, &str)], rows: Option<&[usize]>) {
    let expected = fixture(name, swaps);
    assert_eq!(expected.len(), actual.len(), "{name}: row count");
    let all: Vec<usize> = (0..expected.len()).collect();
    let wrong: Vec<String> = rows
        .unwrap_or(&all)
        .iter()
        .filter(|row| expected[**row] != actual[**row])
        .map(|row| {
            format!(
                "row {row}\n want |{}|\n got  |{}|",
                expected[*row], actual[*row]
            )
        })
        .collect();
    assert!(wrong.is_empty(), "{name} differs:\n{}", wrong.join("\n"));
}

pub(crate) const STRIP: (&str, &str) = (
    "@ Accounts 240   ⊞ Pools 3   ❯ Harnesses 2 of 9   ✦ Plugins and skills   § Process   ⁂ Memory 41   ◉ Profile",
    "@ Accounts 240   ⊞ Pools 3   ❯ Harnesses   ✦ Plugins and skills   § Process   ⁂ Memory 41   ◉ Profile",
);

fn id(text: &str) -> CredentialRef {
    CredentialRef::new(text).unwrap()
}

fn account(
    index: usize,
    name: &str,
    provider: Provider,
    method: SignInMethod,
    status: AccountStatus,
) -> AccountView {
    AccountView {
        id: id(&format!("acct-{index:03}")),
        name: name.into(),
        provider,
        provider_name: crate::accounts::provider_name(provider).into(),
        method,
        method_label: String::new(),
        url: None,
        model: None,
        status,
        status_label: status.label().into(),
    }
}

const NAMED: [(&str, Provider, SignInMethod, AccountStatus, &[&str]); 24] = [
    (
        "Claude Pro - personal",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::NotSignedIn,
        &["Research"],
    ),
    (
        "ChatGPT Plus - spare 4",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::NotSignedIn,
        &[],
    ),
    (
        "LM Studio on mini",
        Provider::Local,
        SignInMethod::Url,
        AccountStatus::Unreachable,
        &["Local only"],
    ),
    (
        "Anthropic API - team",
        Provider::AnthropicApi,
        SignInMethod::ApiKey,
        AccountStatus::KeySaved,
        &["Main"],
    ),
    (
        "ChatGPT Plus - spare 1",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main", "Research"],
    ),
    (
        "ChatGPT Plus - spare 2",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "ChatGPT Plus - spare 3",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Research"],
    ),
    (
        "ChatGPT Pro - main",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "ChatGPT Team - seat 01",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "ChatGPT Team - seat 02",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "ChatGPT Team - seat 03",
        Provider::Chatgpt,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Max - work",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "Claude Max - work 2",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Research"],
    ),
    (
        "Claude Team - seat 01",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "Claude Team - seat 02",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "Claude Team - seat 03",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &["Main"],
    ),
    (
        "Claude Team - seat 04",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 05",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 06",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 07",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 08",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 09",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 10",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
    (
        "Claude Team - seat 11",
        Provider::Claude,
        SignInMethod::Subscription,
        AccountStatus::SignedIn,
        &[],
    ),
];

pub(crate) fn dataset(extra: Option<&str>) -> (Vec<AccountView>, Vec<PoolSummary>) {
    let mut accounts: Vec<AccountView> = NAMED
        .iter()
        .enumerate()
        .map(|(index, (name, provider, method, status, _))| {
            account(index, name, *provider, *method, *status)
        })
        .collect();
    if let Some(name) = extra {
        accounts.push(account(
            24,
            name,
            Provider::Claude,
            SignInMethod::Subscription,
            AccountStatus::SignedIn,
        ));
    }
    let start = accounts.len();
    for index in start..240 {
        accounts.push(account(
            index,
            &format!("Zz spare {index:03}"),
            Provider::OpenaiApi,
            SignInMethod::ApiKey,
            AccountStatus::KeySaved,
        ));
    }
    let pools = ["Main", "Research", "Local only"]
        .iter()
        .map(|pool| PoolSummary {
            id: id(&format!("pool-{}", pool.replace(' ', "-").to_lowercase())),
            name: (*pool).into(),
            summary: String::new(),
            members: NAMED
                .iter()
                .enumerate()
                .filter(|(_, named)| named.4.contains(pool))
                .map(|(index, _)| PoolMember {
                    id: id(&format!("acct-{index:03}")),
                    kind: MemberKind::Chatgpt,
                    provider: "ChatGPT".into(),
                    status: MemberStatus::SignedIn,
                    status_label: "Signed in".into(),
                })
                .collect(),
            project_count: 0,
            provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
        })
        .collect();
    (accounts, pools)
}

pub(crate) fn list_ctx(width: u16, height: u16, pools: Vec<PoolSummary>) -> Ctx {
    let mut ctx = ctx(width, height);
    ctx.store.accounts = Load::Ready(dataset(None).0.iter().map(account_row).collect());
    ctx.store.pools = Load::Ready(pools);
    ctx.store.projects = Load::Ready(Vec::new());
    ctx.store.memory = Load::Ready(41);
    ctx
}

#[test]
fn accounts_needing_attention_come_first_then_names() {
    let (accounts, pools) = dataset(None);
    let ctx = list_ctx(120, 36, pools);
    let order: Vec<&str> = sorted(&accounts, &ctx)
        .iter()
        .take(6)
        .map(|account| account.name.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "Claude Pro - personal",
            "ChatGPT Plus - spare 4",
            "LM Studio on mini",
            "Anthropic API - team",
            "ChatGPT Plus - spare 1",
            "ChatGPT Plus - spare 2"
        ]
    );
}

#[test]
fn account_list_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let (accounts, pools) = dataset(None);
        let ctx = list_ctx(width, height, pools);
        let mut shell = Shell::with(ctx, Box::new(Accounts::with(accounts)));
        check(
            &format!("accounts-{width}"),
            &render(&mut shell, width, height),
            &[STRIP],
            None,
        );
    }
}

#[test]
fn filtering_keeps_matching_rows_and_counts_them() {
    let (accounts, pools) = dataset(Some("Claude Max - work 3"));
    let ctx = list_ctx(80, 24, pools);
    let mut shell = Shell::with(ctx, Box::new(Accounts::with(accounts)));
    shell.key(Key::Char('/'));
    for character in "claude max".chars() {
        shell.key(Key::Char(character));
    }
    check(
        "accounts-filter-80",
        &render(&mut shell, 80, 24),
        &[("Claude Max - review", "Claude Max - work 3")],
        None,
    );
    shell.key(Key::Esc);
    let rows = render(&mut shell, 80, 24);
    assert!(
        rows[21].starts_with("  Showing 1-12 of 240."),
        "{}",
        rows[21]
    );
}

#[test]
fn no_accounts_shows_the_empty_state() {
    let ctx = list_ctx(80, 24, Vec::new());
    let mut shell = Shell::with(ctx, Box::new(Accounts::with(Vec::new())));
    check("accounts-empty-80", &render(&mut shell, 80, 24), &[], None);
}

#[test]
fn list_keys_open_add_account_details_and_selection() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut screen = Accounts::with(accounts);
    assert!(matches!(
        screen.key(&Key::Char('a'), &mut ctx),
        Flow::Open(Place::AddAccount)
    ));
    screen.key(&Key::Char(' '), &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "1 selected.");
    screen.key(&Key::Char('f'), &mut ctx);
    assert_eq!(screen.visible(&ctx).len(), 3);
    match screen.key(&Key::Enter, &mut ctx) {
        Flow::Replace(details) => assert_eq!(
            details.crumbs(),
            ["Settings", "Accounts", "Claude Pro - personal"]
        ),
        other => panic!("details expected, got {other:?}"),
    }
}

#[test]
fn p_opens_a_new_pool_with_the_selected_accounts_ticked() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut screen = Accounts::with(accounts);
    assert!(!screen.hints(&ctx).iter().any(|item| item.key == "p"));
    assert!(matches!(
        screen.key(&Key::Char('p'), &mut ctx),
        Flow::Handled
    ));
    assert_eq!(
        ctx.message().unwrap().lead,
        "Select accounts with Space first."
    );

    screen.key(&Key::Char(' '), &mut ctx);
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Char(' '), &mut ctx);
    let mut picked: Vec<CredentialRef> = screen
        .visible(&ctx)
        .iter()
        .take(2)
        .map(|account| account.id.clone())
        .collect();
    picked.sort();
    assert!(
        screen
            .hints(&ctx)
            .iter()
            .any(|item| item.key == "p" && item.label == "New pool from selected")
    );
    match screen.key(&Key::Char('p'), &mut ctx) {
        Flow::Open(Place::NewPoolFrom(ids)) => assert_eq!(ids, picked),
        other => panic!("new pool expected, got {other:?}"),
    }
}

#[test]
fn p_with_more_than_fifteen_selected_says_the_pool_limit() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut screen = Accounts::with(accounts);
    for _ in 0..16 {
        screen.key(&Key::Char(' '), &mut ctx);
        screen.key(&Key::Down, &mut ctx);
    }
    assert_eq!(ctx.message().unwrap().lead, "16 selected.");
    assert!(matches!(
        screen.key(&Key::Char('p'), &mut ctx),
        Flow::Handled
    ));
    let message = ctx.message().unwrap();
    assert_eq!(message.lead, "A pool holds at most 15 accounts.");
    assert_eq!(message.rest, "Unselect some, then press p again.");
}

#[test]
fn a_full_load_fills_the_list_and_the_shared_rows() {
    let (accounts, pools) = dataset(None);
    let mut shell = Shell::with(list_ctx(80, 24, pools), Box::new(Accounts::new()));
    shell.ctx.store.accounts = Load::NotReady;
    let mut screen = Accounts::new();
    screen.shown(&mut shell.ctx);
    assert_eq!(shell.ctx.store.accounts, Load::Loading);
    assert!(screen.busy(&shell.ctx));
    let loaded = |result: Result<Vec<AccountView>, String>| {
        Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
            SettingsEvent::AllAccounts(result),
        ))))
    };
    shell.arrive(loaded(Ok(accounts.clone())));
    screen.arrive(&loaded(Ok(accounts.clone())), &mut shell.ctx);
    assert_eq!(screen.accounts.len(), 240);
    assert!(!screen.busy(&shell.ctx));
    let rows = shell.ctx.store.accounts.ready().unwrap();
    assert_eq!(rows.len(), 240);
    assert_eq!(rows[2].provider, "Local model");
    assert_eq!(rows[2].status, "Can't reach this URL");
    screen.shown(&mut shell.ctx);
    screen.arrive(
        &loaded(Err("controller_pending_intent".into())),
        &mut shell.ctx,
    );
    shell.arrive(loaded(Err("controller_pending_intent".into())));
    assert_eq!(
        screen.accounts.len(),
        240,
        "a failed refresh keeps the rows"
    );
    assert_eq!(shell.ctx.store.accounts.ready().map(Vec::len), Some(240));
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "Could not refresh accounts."
    );
}

fn modal_text(modal: &Modal) -> String {
    modal
        .body
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn removing_an_account_in_a_pool_names_the_pool_and_takes_it_out() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut details = AccountDetails::new(accounts[0].clone(), Accounts::with(accounts.clone()));
    details.key(&Key::Right, &mut ctx);
    details.key(&Key::Right, &mut ctx);
    details.key(&Key::Enter, &mut ctx);
    let (modal, _) = details.modal(&ctx).expect("a confirm before removing");
    assert_eq!(modal.title, "Remove account");
    let text = modal_text(&modal);
    assert!(text.contains("It leaves pool Research."), "{text}");
    assert!(text.contains("Remove from pools and delete"), "{text}");
    details.key(&Key::Right, &mut ctx);
    details.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 1, "the confirmed remove goes to the engine");
    assert!(ctx.next_request().is_some());
    let mut lonely = AccountDetails::new(accounts[1].clone(), Accounts::with(accounts));
    lonely.key(&Key::Left, &mut ctx);
    lonely.key(&Key::Enter, &mut ctx);
    assert_eq!(lonely.modal(&ctx).unwrap().0.title, "Remove account");
    lonely.key(&Key::Enter, &mut ctx);
    assert!(lonely.modal(&ctx).is_none(), "Cancel is focused first");
    assert_eq!(ctx.queued(), 0);
}

#[test]
fn delete_selected_asks_first_and_names_the_pools_its_accounts_leave() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut screen = Accounts::with(accounts.clone());
    assert!(!screen.hints(&ctx).iter().any(|hint| hint.key == "d"));
    screen.key(&Key::Char('d'), &mut ctx);
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(
        ctx.message().unwrap().lead,
        "Select accounts with Space first."
    );
    screen.marked.insert(accounts[0].id.clone());
    screen.key(&Key::Char('d'), &mut ctx);
    let (modal, _) = screen
        .modal(&ctx)
        .expect("a pooled account is confirmed, not refused");
    let text = modal_text(&modal);
    assert!(text.contains("Delete 1 account?"), "{text}");
    assert!(text.contains("It leaves pool Research."), "{text}");
    assert!(text.contains("Remove from pools and delete"), "{text}");
    screen.key(&Key::Esc, &mut ctx);
    screen.marked.insert(accounts[1].id.clone());
    screen.marked.insert(accounts[200].id.clone());
    assert!(
        screen
            .hints(&ctx)
            .iter()
            .any(|hint| hint.key == "d" && hint.label == "Delete selected")
    );
    screen.key(&Key::Char('d'), &mut ctx);
    let (modal, _) = screen.modal(&ctx).expect("a confirm before deleting");
    assert_eq!(modal.title, "Delete accounts");
    let text = modal_text(&modal);
    assert!(text.contains("Delete 3 accounts?"), "{text}");
    assert!(text.contains("Saved keys are deleted too."), "{text}");
    assert!(text.contains("They leave pool Research."), "{text}");
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "Cancel is focused first");
    assert_eq!(ctx.queued(), 0);
    assert!(!screen.busy(&ctx));
    screen.key(&Key::Char('d'), &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(ctx.queued(), 0);
}

fn add_shell(width: u16, height: u16) -> Shell {
    let ctx = list_ctx(width, height, dataset(None).1);
    let mut shell = Shell::with(ctx, Box::new(AddAccount::new()));
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Accounts(Ok(AccountsReply::Catalog(catalog()))),
    )))));
    shell
}

#[test]
fn add_account_provider_and_method_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut shell = add_shell(width, height);
        check(
            &format!("add-provider-{width}"),
            &render(&mut shell, width, height),
            &[STRIP],
            None,
        );
        shell.key(Key::Enter);
        check(
            &format!("add-method-chatgpt-{width}"),
            &render(&mut shell, width, height),
            &[STRIP],
            None,
        );
    }
    let mut shell = add_shell(80, 24);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    check(
        "add-method-claude-80",
        &render(&mut shell, 80, 24),
        &[],
        None,
    );
}

#[test]
fn api_key_form_masks_the_key_and_counts_it() {
    let mut shell = add_shell(80, 24);
    shell.key(Key::Enter);
    for _ in 0..2 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    for character in " - billing".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Paste("k".repeat(51)));
    let rows = render(&mut shell, 80, 24);
    check(
        "add-key-80",
        &rows,
        &[(
            "Stored in your macOS Keychain. Never shown again.",
            "Kept by the CrazyAgent engine. Never shown again.",
        )],
        Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 23]),
    );
    assert!(
        rows.iter().all(|row| !row.contains("kkk")),
        "the key never shows"
    );
}

#[test]
fn chatgpt_offers_three_ways_and_keys_are_checked_locally() {
    let entry = catalog()
        .into_iter()
        .find(|entry| entry.provider == Provider::Chatgpt)
        .unwrap();
    let titles: Vec<&str> = choices(&entry).iter().map(|choice| choice.title).collect();
    assert_eq!(
        titles,
        [
            "Sign in with ChatGPT",
            "Use the Codex sign-in on this Mac",
            "Use an API key"
        ]
    );
    assert_eq!(
        key_problem(Provider::OpenaiApi, 9, true).as_deref(),
        Some("This is too short for an OpenAI key. Paste the whole key.")
    );
    assert_eq!(key_problem(Provider::OpenaiApi, 51, true), None);
    assert_eq!(key_problem(Provider::OpenaiCompatible, 0, false), None);
}

#[test]
fn without_a_separate_api_provider_a_company_keeps_its_own_key_choice() {
    let older: Vec<_> = catalog()
        .into_iter()
        .filter(|entry| !matches!(entry.provider, Provider::OpenaiApi | Provider::AnthropicApi))
        .collect();
    let tiles = provider_tiles(&older);
    let anthropic = tiles.iter().find(|tile| tile.name == "Anthropic").unwrap();
    assert_eq!(
        anthropic
            .choices
            .iter()
            .map(|choice| (choice.provider, choice.method))
            .collect::<Vec<_>>(),
        [
            (Provider::Claude, SignInMethod::Subscription),
            (Provider::Claude, SignInMethod::ApiKey)
        ]
    );
    assert_eq!(tiles.len(), 5);
}

#[test]
#[should_panic(expected = "every Add account entry offers a way to add an account")]
fn a_catalog_provider_with_no_way_to_add_an_account_trips_the_entry_check() {
    provider_tiles(&[crate::accounts::ProviderEntry {
        provider: Provider::Openrouter,
        name: "OpenRouter".into(),
        methods: Vec::new(),
    }]);
}

#[cfg(unix)]
mod lost_engine {
    use super::*;
    use crate::engine::Client;
    use crate::engine::bootstrap::EngineService;
    use crate::tui::client::Connection;
    use crate::tui::client::accounts::AccountsRequest;
    use crate::tui::client::settings::SettingsJob;
    use crate::tui::screens::open;
    use crate::tui::seam::Request;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    pub(super) async fn until(
        shell: &mut Shell,
        connection: &mut Connection,
        done: impl Fn(&OwnerEvent) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(event) = connection.poll_owner().unwrap() {
                let last = done(&event);
                shell.arrive(crate::tui::run::owner_arrival(event));
                if last {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owner event timed out"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub(super) fn ready(event: &OwnerEvent) -> bool {
        matches!(event, OwnerEvent::Access { ready: true, .. })
    }

    pub(super) fn settled(event: &OwnerEvent) -> bool {
        matches!(event, OwnerEvent::Settings(_))
    }

    pub(super) async fn send(
        shell: &mut Shell,
        connection: &mut Connection,
        wanted: fn(&SettingsJob) -> bool,
    ) {
        let job = std::iter::from_fn(|| shell.ctx.next_request())
            .find_map(|request| match request {
                Request::Settings(job) if wanted(&job) => Some(job),
                _ => None,
            })
            .expect("the screen asked the engine");
        connection.settings(*job).unwrap();
        until(shell, connection, settled).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_check_against_a_stopped_engine_shows_the_lost_connection_and_recovers() {
        let root = tempfile::Builder::new()
            .prefix("ca-tacc-lost-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = crate::tui::test_home::engine_config();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let mut connection = Connection::connect_paired(
            Client::new(
                root.path().join("e.sock"),
                crate::tui::test_home::ENGINE_WAIT,
            )
            .unwrap(),
        )
        .unwrap();
        let mut start = ctx(80, 24);
        let home = open(&Place::Home, &mut start);
        let mut shell = Shell::with(start, home);
        until(&mut shell, &mut connection, ready).await;
        assert_eq!(shell.ctx.link, Link::Ready);
        connection
            .settings(SettingsJob::Accounts(AccountsRequest::Add {
                draft: crate::accounts::AccountDraft {
                    provider: Provider::Local,
                    method: SignInMethod::Url,
                    name: "Laptop".into(),
                    url: None,
                    model: Some("qwen2.5:0.5b".into()),
                },
                key: None,
            }))
            .unwrap();
        until(&mut shell, &mut connection, settled).await;
        while shell.ctx.next_request().is_some() {}

        shell.key(Key::Ctrl('k'));
        for character in "set accounts".chars() {
            shell.key(Key::Char(character));
        }
        shell.key(Key::Enter);
        assert_eq!(
            shell.place(),
            Some(Place::Settings(SettingsSection::Accounts))
        );
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::AllAccounts)
        })
        .await;
        shell.key(Key::Enter);
        assert_eq!(shell.depth(), 2);
        assert_eq!(shell.place(), None, "the account details are open");

        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        shell.key(Key::Enter);
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::Accounts(AccountsRequest::Check { .. }))
        })
        .await;
        assert_eq!(shell.ctx.link, Link::Reconnecting);
        let message = shell.ctx.message().unwrap();
        assert_eq!(message.lead, "Lost the connection to the engine.");
        assert_eq!(message.rest, "Retrying in 3 s. What you typed is kept.");
        let before = shell.ctx.queued();
        shell.key(Key::Enter);
        assert_eq!(
            shell.ctx.queued(),
            before + 1,
            "the details still take a check"
        );

        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        connection.reconnect().unwrap();
        until(&mut shell, &mut connection, ready).await;
        assert_eq!(shell.ctx.link, Link::Ready);
        assert_eq!(
            shell.ctx.message().unwrap().lead,
            "Connected to the engine again."
        );
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::Accounts(AccountsRequest::Check { .. }))
        })
        .await;
        let checked = shell.ctx.message().unwrap();
        assert!(checked.lead.starts_with("Connected. "), "{}", checked.lead);
        drop(connection);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(unix)]
mod replace_key {
    use super::lost_engine::{ready, send, settled, until};
    use super::*;
    use crate::engine::Client;
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::AccountSecret;
    use crate::tui::client::Connection;
    use crate::tui::client::accounts::AccountsRequest;
    use crate::tui::client::settings::SettingsJob;
    use crate::tui::screens::open;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn holders(root: &Path, secret: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                if metadata.is_dir() {
                    folders.push(path);
                } else if metadata.is_file()
                    && std::fs::read(&path).is_ok_and(|bytes| {
                        bytes
                            .windows(secret.len())
                            .any(|window| window == secret.as_bytes())
                    })
                {
                    found.push(path);
                }
            }
        }
        found
    }

    async fn accounts(connection: &mut Connection) -> Vec<AccountView> {
        connection
            .settings(SettingsJob::Accounts(AccountsRequest::List {
                after: None,
                limit: 50,
            }))
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(OwnerEvent::Settings(event)) = connection.poll_owner().unwrap()
                && let SettingsEvent::Accounts(Ok(AccountsReply::Page(page))) = *event
            {
                return page.accounts;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "account page timed out"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_key_rotates_the_same_account_and_deletes_the_old_key() {
        let root = tempfile::Builder::new()
            .prefix("ca-tacc-key-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = crate::tui::test_home::engine_config();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let mut connection = Connection::connect_paired(
            Client::new(
                root.path().join("e.sock"),
                crate::tui::test_home::ENGINE_WAIT,
            )
            .unwrap(),
        )
        .unwrap();
        let mut start = ctx(80, 24);
        let home = open(&Place::Home, &mut start);
        let mut shell = Shell::with(start, home);
        until(&mut shell, &mut connection, ready).await;
        let old = format!("sk-old-{}-leaked-key", std::process::id());
        let new = format!("sk-new-{}-rotated-key", std::process::id());
        connection
            .settings(SettingsJob::Accounts(AccountsRequest::Add {
                draft: crate::accounts::AccountDraft {
                    provider: Provider::OpenaiApi,
                    method: SignInMethod::ApiKey,
                    name: "Work key".into(),
                    url: None,
                    model: Some("gpt-5".into()),
                },
                key: Some(AccountSecret::new(old.as_bytes().to_vec()).unwrap()),
            }))
            .unwrap();
        until(&mut shell, &mut connection, settled).await;
        while shell.ctx.next_request().is_some() {}
        let before = accounts(&mut connection).await;
        assert_eq!(before.len(), 1);
        assert_eq!(holders(root.path(), &old).len(), 1);

        shell.key(Key::Ctrl('k'));
        for character in "set accounts".chars() {
            shell.key(Key::Char(character));
        }
        shell.key(Key::Enter);
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::AllAccounts)
        })
        .await;
        shell.key(Key::Enter);
        assert_eq!(shell.depth(), 2);
        let page = render(&mut shell, 80, 24).join("\n");
        assert!(page.contains("[> Replace key ]"), "{page}");

        shell.key(Key::Enter);
        let page = render(&mut shell, 80, 24).join("\n");
        assert!(page.contains("New API key"), "a masked field opens: {page}");
        assert_eq!(shell.depth(), 2, "the details page stays open");
        for character in new.chars() {
            shell.key(Key::Char(character));
        }
        let page = render(&mut shell, 80, 24).join("\n");
        assert!(!page.contains(&new), "the typed key stays hidden: {page}");
        shell.key(Key::Enter);
        send(&mut shell, &mut connection, |job| {
            matches!(
                job,
                SettingsJob::Accounts(AccountsRequest::ReplaceKey { .. })
            )
        })
        .await;
        assert_eq!(shell.ctx.message().unwrap().lead, "Key replaced.");
        let page = render(&mut shell, 80, 24).join("\n");
        assert!(page.contains("Key saved"), "{page}");

        let after = accounts(&mut connection).await;
        assert_eq!(after.len(), 1, "no second account: {after:?}");
        assert_eq!(after[0].id, before[0].id);
        assert_eq!(after[0].name, "Work key");
        assert!(
            holders(root.path(), &old).is_empty(),
            "the old key is deleted"
        );
        let held = holders(root.path(), &new);
        assert_eq!(
            held.len(),
            1,
            "only the key file holds the new key: {held:?}"
        );
        assert!(held[0].parent().unwrap().ends_with("account-keys"));
        drop(connection);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(unix)]
mod sign_in_refused {
    use super::lost_engine::{ready, send, until};
    use super::*;
    use crate::engine::Client;
    use crate::engine::bootstrap::EngineService;
    use crate::tui::client::Connection;
    use crate::tui::client::accounts::AccountsRequest;
    use crate::tui::client::settings::SettingsJob;
    use crate::tui::test_home::ENGINE_WAIT;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    const CLAUDE: &str = r#"
[model_access]
accounts = ["account-claude"]
[[model_access.connections]]
id = "acct-000"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "model-claude", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sign_in_the_engine_refuses_says_why_on_the_add_account_page() {
        let root = tempfile::Builder::new()
            .prefix("ca-tacc-ro-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = crate::config::resolve_config_bytes(
            &format!("timeout_ms = {}\n{CLAUDE}", ENGINE_WAIT.as_millis()),
            "",
        )
        .unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let mut connection = Connection::connect_paired(
            Client::new(root.path().join("e.sock"), ENGINE_WAIT).unwrap(),
        )
        .unwrap();
        let claude = account(
            0,
            "Work Claude",
            Provider::Claude,
            SignInMethod::Subscription,
            AccountStatus::NotSignedIn,
        );
        let mut shell = Shell::with(ctx(100, 30), Box::new(AddAccount::again(claude)));
        until(&mut shell, &mut connection, ready).await;
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::Accounts(AccountsRequest::Catalog))
        })
        .await;
        shell.refresh();
        send(&mut shell, &mut connection, |job| {
            matches!(job, SettingsJob::SignIn(_))
        })
        .await;

        let page = render(&mut shell, 100, 30).join("\n");
        assert!(
            page.contains("Settings are read-only in this engine."),
            "{page}"
        );
        assert!(page.contains("Press n to try again."), "{page}");
        assert!(!page.contains("model_access_read_only"), "{page}");
        assert!(!page.contains("read_original_operation"), "{page}");
        drop(connection);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
