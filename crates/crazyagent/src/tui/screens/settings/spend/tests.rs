use super::*;
use crate::pools::{MemberStatus, PoolMember, PoolSummary};
use crate::tui::screens::settings_accounts::tests::ctx;
use crate::tui::seam::{AccountRow, Request};
use crate::usage::spend::Origin;

const NOW: i64 = 1_793_000_000_000;

fn r(text: &str) -> CredentialRef {
    CredentialRef::new(text).unwrap()
}

fn settings(confirm: SpendConfirm, all_limited: AllLimited) -> SpendSettings {
    SpendSettings {
        confirm,
        all_limited,
    }
}

fn view(settings: SpendSettings, attempts: Vec<SpendAttempt>) -> SpendView {
    SpendView {
        settings,
        attempts,
        limited_pools: if settings.all_limited == AllLimited::Notice {
            vec![LimitedPool {
                pool: r("pool-a"),
                name: Some("Daily work".into()),
                since_ms: NOW,
                with_resets: vec![r("codex-b")],
            }]
        } else {
            Vec::new()
        },
    }
}

fn attempt(state: AttemptState, outcome: Option<&str>) -> SpendAttempt {
    SpendAttempt {
        account: r("codex-b"),
        origin: Origin::Owner,
        state,
        outcome: outcome.map(str::to_owned),
        requested_ms: NOW,
        settled_ms: None,
    }
}

fn member(id: &str) -> PoolMember {
    PoolMember {
        id: r(id),
        kind: MemberKind::Chatgpt,
        provider: "ChatGPT".into(),
        status: MemberStatus::SignedIn,
        status_label: "Signed in".into(),
    }
}

fn ready(spend_view: SpendView) -> (Spend, Ctx) {
    let mut ctx = ctx(120, 40);
    ctx.store.pools = Load::Ready(vec![PoolSummary {
        id: r("pool-a"),
        name: "Daily work".into(),
        summary: String::new(),
        members: vec![member("codex-a"), member("codex-b")],
        project_count: 1,
        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
    }]);
    ctx.store.accounts = Load::Ready(
        [("codex-a", "Alpha"), ("codex-b", "Bravo")]
            .into_iter()
            .map(|(id, name)| AccountRow {
                id: r(id),
                name: name.into(),
                provider: "ChatGPT".into(),
                status: "signed_in".into(),
                no_window: None,
            })
            .collect(),
    );
    while ctx.next_request().is_some() {}
    let mut spend = Spend::new();
    spend.view = Some(spend_view);
    (spend, ctx)
}

fn shown(spend: &Spend, ctx: &Ctx) -> String {
    spend
        .lines(ctx, 400)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn keys(spend: &Spend, ctx: &Ctx) -> Vec<String> {
    spend.hints(ctx).into_iter().map(|hint| hint.key).collect()
}

fn job(ctx: &mut Ctx) -> Option<SettingsJob> {
    match ctx.next_request() {
        Some(Request::Settings(job)) => Some(*job),
        None => None,
        Some(_) => panic!("only settings jobs"),
    }
}

#[test]
fn option_spend_confirmation_confirm_each_asks_first_then_sends_the_spend() {
    let (mut spend, mut ctx) = ready(view(
        settings(SpendConfirm::ConfirmEach, AllLimited::Notice),
        Vec::new(),
    ));
    assert!(keys(&spend, &ctx).contains(&"u".to_owned()));
    assert!(matches!(
        spend.key(&Key::Char('u'), &mut ctx),
        Flow::Handled
    ));
    assert!(
        job(&mut ctx).is_none(),
        "nothing is sent before the confirm"
    );
    let (modal, _) = spend.modal(&ctx).expect("the confirm is open");
    assert_eq!(modal.body[0].to_string(), "Use a free reset on Bravo?");
    spend.key(&Key::Enter, &mut ctx);
    assert!(job(&mut ctx).is_none(), "Cancel has the focus first");
    assert!(spend.modal(&ctx).is_none());
    spend.key(&Key::Char('u'), &mut ctx);
    spend.key(&Key::Right, &mut ctx);
    spend.key(&Key::Enter, &mut ctx);
    assert!(
        matches!(job(&mut ctx), Some(SettingsJob::Spend { account, confirmed: true }) if account == r("codex-b"))
    );
    assert!(!spend.asking());
}

#[test]
fn option_spend_confirmation_one_click_sends_the_spend_at_once() {
    let (mut spend, mut ctx) = ready(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        Vec::new(),
    ));
    spend.key(&Key::Char('u'), &mut ctx);
    assert!(spend.modal(&ctx).is_none(), "no confirm on one click");
    assert!(
        matches!(job(&mut ctx), Some(SettingsJob::Spend { account, confirmed: false }) if account == r("codex-b"))
    );
}

#[test]
fn option_spend_confirmation_off_offers_no_spend_and_says_so() {
    let (mut spend, mut ctx) = ready(view(
        settings(SpendConfirm::Off, AllLimited::Notice),
        Vec::new(),
    ));
    assert!(!keys(&spend, &ctx).contains(&"u".to_owned()));
    assert!(matches!(
        spend.key(&Key::Char('u'), &mut ctx),
        Flow::Ignored
    ));
    assert!(job(&mut ctx).is_none());
    let text = shown(&spend, &ctx);
    assert!(text.contains(
        "Spend confirmation (c): Off. CrazyAgent never uses a free reset. Use them in the provider's own app."
    ), "{text}");
    assert!(!text.contains("Press u"), "{text}");
}

#[test]
fn c_steps_spend_confirmation_and_stores_it() {
    for (from, to) in [
        (SpendConfirm::ConfirmEach, SpendConfirm::OneClick),
        (SpendConfirm::OneClick, SpendConfirm::Off),
        (SpendConfirm::Off, SpendConfirm::ConfirmEach),
    ] {
        let (mut spend, mut ctx) = ready(view(settings(from, AllLimited::NoHint), Vec::new()));
        spend.key(&Key::Char('c'), &mut ctx);
        assert!(
            matches!(job(&mut ctx), Some(SettingsJob::SetSpend(sent)) if sent == settings(to, AllLimited::NoHint))
        );
    }
}

#[test]
fn option_all_limited_notice_names_the_accounts_with_free_resets() {
    let (spend, ctx) = ready(view(
        settings(SpendConfirm::ConfirmEach, AllLimited::Notice),
        Vec::new(),
    ));
    let text = shown(&spend, &ctx);
    assert!(text.contains("When every pool account is limited (l): Notice. When every account in a pool is at its limit, Pools names the accounts that hold a free reset."), "{text}");
    assert!(
        text.contains("Every account in Daily work is at its limit. Free resets: Bravo."),
        "{text}"
    );
    assert!(
        text.contains("Press u to use a free reset on Bravo."),
        "{text}"
    );
}

#[test]
fn option_all_limited_no_hint_shows_no_pool_notice() {
    let (spend, ctx) = ready(view(
        settings(SpendConfirm::ConfirmEach, AllLimited::NoHint),
        Vec::new(),
    ));
    let text = shown(&spend, &ctx);
    assert!(text.contains("When every pool account is limited (l): No hint. Nothing is shown when every account in a pool is at its limit."), "{text}");
    assert!(!text.contains("Every account in"), "{text}");
    assert!(!keys(&spend, &ctx).contains(&"u".to_owned()));
}

#[test]
fn option_all_limited_auto_spend_says_what_it_does_and_that_it_waits_while_spending_is_off() {
    let (spend, ctx) = ready(view(
        settings(SpendConfirm::OneClick, AllLimited::AutoSpend),
        Vec::new(),
    ));
    assert!(shown(&spend, &ctx).contains("Spend on its own. When every account in a pool is at its limit, CrazyAgent uses one free reset on its own"));
    let (spend, ctx) = ready(view(
        settings(SpendConfirm::Off, AllLimited::AutoSpend),
        Vec::new(),
    ));
    assert!(shown(&spend, &ctx).contains(
        "Spend on its own. Spending is off, so CrazyAgent never uses a free reset on its own."
    ));
}

#[test]
fn l_steps_the_all_limited_setting_and_stores_it() {
    for (from, to) in [
        (AllLimited::Notice, AllLimited::NoHint),
        (AllLimited::NoHint, AllLimited::AutoSpend),
        (AllLimited::AutoSpend, AllLimited::Notice),
    ] {
        let (mut spend, mut ctx) = ready(view(settings(SpendConfirm::OneClick, from), Vec::new()));
        spend.key(&Key::Char('l'), &mut ctx);
        assert!(
            matches!(job(&mut ctx), Some(SettingsJob::SetSpend(sent)) if sent == settings(SpendConfirm::OneClick, to))
        );
    }
}

#[test]
fn a_spend_still_out_offers_no_second_one_and_shows_where_it_stands() {
    for (state, words) in [
        (AttemptState::Queued, "Bravo: Using a free reset now."),
        (
            AttemptState::Sent,
            "Bravo: Sent. Waiting for the provider's answer.",
        ),
    ] {
        let (mut spend, mut ctx) = ready(view(
            settings(SpendConfirm::OneClick, AllLimited::Notice),
            vec![attempt(state, None)],
        ));
        assert!(matches!(
            spend.key(&Key::Char('u'), &mut ctx),
            Flow::Ignored
        ));
        assert!(job(&mut ctx).is_none());
        assert!(shown(&spend, &ctx).contains(words));
    }
    let (spend, ctx) = ready(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        vec![attempt(
            AttemptState::Settled,
            Some("usage_spend_not_signed_in"),
        )],
    ));
    let text = shown(&spend, &ctx);
    assert!(
        text.contains("Bravo: This account wasn't signed in or was busy."),
        "{text}"
    );
    assert!(
        text.contains("Press u to use a free reset on Bravo."),
        "{text}"
    );
}

#[test]
fn a_claude_or_key_account_is_never_offered_a_spend_here() {
    let (mut spend, mut ctx) = ready(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        Vec::new(),
    ));
    if let Load::Ready(pools) = &mut ctx.store.pools {
        pools[0].members[1].kind = MemberKind::Claude;
    }
    assert!(spend.target(&ctx).is_none());
    assert!(matches!(
        spend.key(&Key::Char('u'), &mut ctx),
        Flow::Ignored
    ));
}

#[test]
fn no_spend_lines_or_keys_before_the_spend_view_is_read() {
    let (_, ctx) = ready(view(SpendSettings::default(), Vec::new()));
    let spend = Spend::new();
    assert!(spend.lines(&ctx, 120).is_empty());
    assert!(spend.hints(&ctx).is_empty());
}

fn desktop_table(file: &str, table: &str) -> Vec<(String, String)> {
    let path = format!(
        "{}/../../apps/desktop/src/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let source = std::fs::read_to_string(&path).unwrap();
    let start = source
        .find(table)
        .unwrap_or_else(|| panic!("{table} in {file}"));
    let body = &source[start..];
    let end = body.find("\n};").unwrap();
    body[..end]
        .lines()
        .skip(1)
        .map(|line| {
            let (code, words) = line.trim().split_once(": \"").unwrap();
            (code.to_owned(), words.trim_end_matches("\",").to_owned())
        })
        .collect()
}

#[test]
fn the_terminal_says_every_spend_outcome_and_help_line_in_the_desktop_words() {
    let outcomes = desktop_table("api-usage.ts", "const SPEND_OUTCOMES");
    assert!(outcomes.len() >= 17, "{outcomes:?}");
    for (code, words) in outcomes {
        let mut settled = attempt(AttemptState::Settled, Some(code.as_str()));
        settled.settled_ms = Some(NOW);
        assert_eq!(outcome_words(&settled), words, "{code}");
    }
    for (code, words) in desktop_table("screens/settings/spend-settings.tsx", "const CONFIRM_HELP")
    {
        let value: SpendConfirm = serde_json::from_value(serde_json::json!(code)).unwrap();
        assert_eq!(confirm_help(value), words, "{code}");
    }
    for (code, words) in desktop_table(
        "screens/settings/spend-settings.tsx",
        "const ALL_LIMITED_HELP",
    ) {
        let value: AllLimited = serde_json::from_value(serde_json::json!(code)).unwrap();
        assert_eq!(
            all_limited_help(settings(SpendConfirm::ConfirmEach, value)),
            words,
            "{code}"
        );
    }
}

#[test]
fn the_spend_view_is_read_once_a_minute_or_every_5_s_while_a_spend_is_out() {
    let usage_reads = |ctx: &mut Ctx| {
        let mut reads = 0;
        while let Some(request) = ctx.next_request() {
            if let Request::Settings(job) = request
                && matches!(*job, SettingsJob::Usage { .. })
            {
                reads += 1;
            }
        }
        reads
    };
    let (mut spend, mut ctx) = ready(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        Vec::new(),
    ));
    spend.view = None;
    spend.shown(&mut ctx, NOW);
    assert_eq!(usage_reads(&mut ctx), 1);
    spend.view = Some(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        Vec::new(),
    ));
    spend.shown(&mut ctx, NOW + OPEN_READ_EVERY_MS);
    assert_eq!(
        usage_reads(&mut ctx),
        0,
        "the shell's 5 s show reads nothing new"
    );
    spend.shown(&mut ctx, NOW + READ_EVERY_MS);
    assert_eq!(usage_reads(&mut ctx), 1, "a minute on");
    spend.view = Some(view(
        settings(SpendConfirm::OneClick, AllLimited::Notice),
        vec![attempt(AttemptState::Queued, None)],
    ));
    spend.shown(&mut ctx, NOW + READ_EVERY_MS + OPEN_READ_EVERY_MS);
    assert_eq!(usage_reads(&mut ctx), 1, "every 5 s while a spend is out");
    spend.shown(&mut ctx, NOW);
    assert_eq!(
        usage_reads(&mut ctx),
        1,
        "a clock that went back reads again"
    );
}
