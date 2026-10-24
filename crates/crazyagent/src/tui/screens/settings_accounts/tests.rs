use super::super::settings_add_account::{AddAccount, choices, key_problem};
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
    "@ Accounts 240   ⊞ Pools 3   ❯ Harnesses 2 of 9   ✦ Plugins and skills   ⁂ Memory 41   ◉ Profile",
    "@ Accounts 240   ⊞ Pools 3   ❯ Harnesses   ✦ Plugins and skills   ⁂ Memory 41   ◉ Profile",
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

#[test]
fn removing_an_account_in_a_pool_is_refused_with_the_reason() {
    let (accounts, pools) = dataset(None);
    let mut ctx = list_ctx(80, 24, pools);
    let mut details = AccountDetails::new(accounts[0].clone(), Accounts::with(accounts.clone()));
    details.key(&Key::Right, &mut ctx);
    details.key(&Key::Right, &mut ctx);
    details.key(&Key::Enter, &mut ctx);
    assert!(details.modal(&ctx).is_none());
    assert_eq!(
        ctx.message().unwrap().lead,
        "Claude Pro - personal is in pool Research. Remove it from the pool first."
    );
    let mut lonely = AccountDetails::new(accounts[1].clone(), Accounts::with(accounts));
    lonely.key(&Key::Left, &mut ctx);
    lonely.key(&Key::Enter, &mut ctx);
    assert_eq!(lonely.modal(&ctx).unwrap().0.title, "Remove account");
    lonely.key(&Key::Enter, &mut ctx);
    assert!(lonely.modal(&ctx).is_none(), "Cancel is focused first");
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
    for _ in 0..3 {
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
            "Use an API key",
            "Use the Codex sign-in on this Mac"
        ]
    );
    assert_eq!(
        key_problem(Provider::OpenaiApi, 9, true).as_deref(),
        Some("This is too short for an OpenAI key. Paste the whole key.")
    );
    assert_eq!(key_problem(Provider::OpenaiApi, 51, true), None);
    assert_eq!(key_problem(Provider::OpenaiCompatible, 0, false), None);
}
