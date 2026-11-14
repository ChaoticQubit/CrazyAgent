use super::*;
use crate::protocol::CredentialRef;
use crate::tui::screens::settings_accounts::tests::ctx;
use crate::tui::seam::Request;
use crate::usage::capture::Tokens;
use crate::usage::report::LinesRow;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const NOW: i64 = 1_793_091_600_000;

fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

fn usage_row(key: &str) -> UsageRow {
    UsageRow {
        key: key.into(),
        tokens: Tokens {
            input: 1200,
            output: 300,
            cache_read: 4000,
            cache_write: 0,
        },
        runs: 2,
        last_ms: NOW - 5 * MINUTE,
        cost_nanos: 0,
        not_reported_by: None,
        project: None,
    }
}

fn limits(failure: Option<&str>) -> AccountLimits {
    AccountLimits {
        account: CredentialRef::new("acct-claude").unwrap(),
        plan: Some("max".into()),
        five_hour: Some(LimitWindow {
            used_percent: 34.4,
            resets_at_ms: Some(NOW + 2 * HOUR),
        }),
        weekly: Some(LimitWindow {
            used_percent: 90.0,
            resets_at_ms: Some(NOW - MINUTE),
        }),
        polled_five_hour: Some(LimitWindow {
            used_percent: 34.4,
            resets_at_ms: Some(NOW + 2 * HOUR),
        }),
        polled_weekly: Some(LimitWindow {
            used_percent: 90.0,
            resets_at_ms: Some(NOW - MINUTE),
        }),
        five_hour_reset_override_ms: None,
        weekly_reset_override_ms: None,
        source: Some(LimitSource::Poll),
        observed_ms: Some(NOW - 3 * MINUTE),
        checked_ms: Some(NOW - 3 * MINUTE),
        failure: failure.map(Into::into),
        five_hour_left_out: None,
        weekly_left_out: None,
        five_hour_left_out_label: None,
        weekly_left_out_label: None,
    }
}

fn report() -> UsageReport {
    UsageReport {
        since_ms: None,
        total: Tokens {
            input: 1200,
            output: 300,
            cache_read: 4000,
            cache_write: 800,
        },
        runs: 3,
        projects: vec![usage_row("project-harbor")],
        pools: vec![usage_row("pool-main")],
        accounts: (0..7).map(|n| usage_row(&format!("acct-{n}"))).collect(),
        agents: vec![usage_row("team-7")],
        days: Vec::new(),
        lines: vec![LinesRow {
            project: "project-harbor".into(),
            change_sets: 3,
            added: 90,
            removed: 57,
            commits: 2,
            committed_added: 80,
            committed_removed: 55,
        }],
        agent_lines: Vec::new(),
        limits: vec![limits(Some("usage_rate_limited"))],
        not_reported_by: Vec::new(),
        health: Vec::new(),
        partial_homes: 0,
        money: Default::default(),
        recount: None,
        counting_notice: None,
        activity: Vec::new(),
        models: Vec::new(),
        spend: Default::default(),
    }
}

#[test]
fn a_reading_with_a_home_cut_at_the_scan_bound_says_it_is_partial() {
    let ctx = ctx(100, 40);
    let shown = |homes: u64| {
        text(&summary(
            &UsageReport {
                partial_homes: homes,
                ..report()
            },
            UsageRange::Today,
            &names(),
            NOW,
            0,
            96,
            &ctx,
        ))
        .join("\n")
    };
    assert!(!shown(0).contains("may be missing"), "{}", shown(0));
    assert!(
        shown(1).contains("Some usage may be missing: 1 agent home holds more records than one scan reads, so its least recently changed records are not counted."),
        "{}",
        shown(1)
    );
    assert!(
        shown(3).contains("3 agent homes hold more records than one scan reads, so their least"),
        "{}",
        shown(3)
    );
}

fn names() -> Names {
    let mut names = Names::default();
    names
        .projects
        .insert("project-harbor".into(), "Harbor Notes".into());
    names.pools.insert("pool-main".into(), "Main pool".into());
    names
        .accounts
        .insert("acct-claude".into(), "Work Claude".into());
    names
}

#[test]
fn ranges_start_at_local_midnight_and_hold_whole_local_days() {
    let offset = 2 * 3600;
    let midnight = NOW - 11 * HOUR;
    assert_eq!(UsageRange::Today.start(NOW, offset), Some(midnight));
    assert_eq!(
        UsageRange::Week.start(NOW, offset),
        Some(midnight - 6 * DAY_MS)
    );
    assert_eq!(
        UsageRange::Month.start(NOW, offset),
        Some(midnight - 29 * DAY_MS)
    );
    assert_eq!(UsageRange::All.start(NOW, offset), None);
    assert_eq!(
        UsageRange::Today.start(NOW, -10 * 3600),
        Some(NOW - 23 * HOUR)
    );
    let mut range = UsageRange::Today;
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(range.label());
        range = range.next();
    }
    assert_eq!(range, UsageRange::Today);
    assert_eq!(seen, ["Today", "Last 7 days", "Last 30 days", "All time"]);
}

#[test]
fn counts_countdowns_and_failed_checks_read_as_plain_words() {
    assert_eq!(compact(950), "950");
    assert_eq!(compact(1000), "1K");
    assert_eq!(compact(1234), "1.2K");
    assert_eq!(compact(999_950), "1M");
    assert_eq!(compact(3_400_000), "3.4M");
    assert_eq!(compact(2_500_000_000), "2.5B");
    assert_eq!(reset_text(None, NOW), "Next reset not known yet");
    assert_eq!(
        reset_text(Some(NOW + 30_000), NOW),
        "Resets in under a minute"
    );
    assert_eq!(
        reset_text(Some(NOW - HOUR), NOW),
        "Resets in under a minute"
    );
    assert_eq!(
        reset_text(Some(NOW + 14 * MINUTE + 59_000), NOW),
        "Resets in 14 min"
    );
    assert_eq!(
        reset_text(Some(NOW + 2 * HOUR + 14 * MINUTE), NOW),
        "Resets in 2 h 14 min"
    );
    assert_eq!(
        reset_text(Some(NOW + 3 * DAY_MS + 4 * HOUR), NOW),
        "Resets in 3 d 4 h"
    );
    assert_eq!(
        poll_failure("usage_sign_in_lapsed"),
        "The provider no longer accepts this sign-in. Sign in again in Accounts."
    );
    assert_eq!(
        poll_failure("native_profile_torn"),
        "Couldn't read this account's limits (native_profile_torn)."
    );
}

#[test]
fn a_sign_in_the_limits_check_cannot_use_says_what_it_is_and_what_to_do() {
    for (code, sentence) in [
        (
            "native_profile_codex_api_key",
            "An API-key account has no 5-hour or weekly window.",
        ),
        (
            "native_profile_codex_token_unreadable",
            "CrazyAgent can't read this Codex sign-in. Sign in to Codex again in Accounts.",
        ),
        (
            "native_profile_codex_sign_in_expired",
            "This Codex sign-in expired and CrazyAgent couldn't renew it. Sign in to Codex again in Accounts.",
        ),
        (
            "native_profile_credential_unavailable",
            "CrazyAgent can't read this account's sign-in. Sign in again in Accounts.",
        ),
        (
            "native_profile_renewal_failed",
            "CrazyAgent couldn't renew this account's sign-in. It tries again in 10 minutes.",
        ),
        (
            "native_profile_renewal_busy",
            "Another check of this account's sign-in is running. CrazyAgent tries again in 10 minutes.",
        ),
        (
            "native_profile_sign_in_revoked",
            "The provider ended this sign-in. Sign in again in Accounts.",
        ),
    ] {
        assert_eq!(poll_failure(code), sentence, "{code}");
    }
}

#[test]
fn the_summary_shows_totals_limits_with_countdowns_and_named_groups() {
    let ctx = ctx(100, 40);
    let lines = text(&summary(
        &report(),
        UsageRange::Week,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    assert!(lines[0].starts_with("Usage") && lines[0].ends_with("Last 7 days"));
    assert_eq!(
        lines[1],
        "Tokens 2.3K (1.2K input, 800 cache write, 300 output)"
    );
    assert_eq!(
        lines[2],
        "Re-read from cache 4K: 67% of everything read, not in Tokens"
    );
    assert_eq!(lines[3], "Runs 3  Lines +90 -57");
    let all = lines.join("\n");
    for wanted in [
        "  Work Claude  max  read from the provider 3 min ago",
        "    5-hour   34% used  Resets in 2 h 0 min",
        "    Weekly    0% used  Next reset not known yet",
        "    The provider asked CrazyAgent to wait before reading limits again.",
        "By project\n  Harbor Notes    1.5K tokens  2 runs",
        "By pool\n  Main pool    1.5K tokens  2 runs",
        "By account\n  acct-0    1.5K tokens  2 runs",
        "  and 2 more",
    ] {
        assert!(all.contains(wanted), "{wanted:?} in\n{all}");
    }
    assert!(!all.contains("project-harbor") && !all.contains("pool-main"));
}

#[test]
fn runs_tied_to_no_project_or_account_read_as_unattributed() {
    let ctx = ctx(100, 40);
    let report = UsageReport {
        projects: vec![usage_row("project-harbor"), usage_row("")],
        accounts: vec![usage_row("")],
        ..report()
    };
    let all = text(&summary(
        &report,
        UsageRange::Week,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    for wanted in [
        "By project\n  Harbor Notes    1.5K tokens  2 runs\n  Unattributed    1.5K tokens  2 runs",
        "By account\n  Unattributed    1.5K tokens  2 runs",
    ] {
        assert!(all.contains(wanted), "{wanted:?} in\n{all}");
    }
}

/// At 80 columns the page draws every number of the summary: the headline with its three
/// parts, the cache re-reads with their share, the runs and the lines changed (T19-L2).
#[test]
fn at_80_columns_the_summary_draws_every_number() {
    let mut ctx = ctx(80, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut answer = report();
    answer.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(answer)), &mut ctx);
    let mut screen = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40)).unwrap();
    screen
        .draw(|frame| {
            page.draw(frame, frame.area(), &ctx);
        })
        .unwrap();
    let buffer = screen.backend().buffer();
    let all = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for wanted in [
        "Tokens 2.3K (1.2K input, 800 cache write, 300 output)",
        "Re-read from cache 4K: 67% of everything read, not in Tokens",
        "Runs 3  Lines +90 -57",
    ] {
        assert!(all.contains(wanted), "{wanted:?} at 80 columns:\n{all}");
    }
}

#[test]
fn a_manual_reset_is_marked_and_an_empty_range_says_so() {
    let ctx = ctx(100, 40);
    let mut own = limits(None);
    own.five_hour_reset_override_ms = Some(NOW + 45 * MINUTE);
    own.weekly = None;
    own.plan = None;
    let empty = UsageReport {
        total: Tokens::default(),
        runs: 0,
        projects: Vec::new(),
        pools: Vec::new(),
        accounts: Vec::new(),
        agents: Vec::new(),
        lines: Vec::new(),
        limits: vec![own],
        ..report()
    };
    let all = text(&summary(
        &empty,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    assert!(
        all.contains("5-hour   34% used  Resets in 45 min (your time)"),
        "{all}"
    );
    assert!(all.contains("Weekly  Not reported"), "{all}");
    assert!(all.contains("Re-read from cache 0, not in Tokens"), "{all}");
    assert!(all.contains("No usage in this range."), "{all}");
    assert!(!all.contains("By project"), "{all}");
    let none = UsageReport {
        limits: Vec::new(),
        ..empty
    };
    let all = text(&summary(
        &none,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    assert!(all.contains("No limits read yet."), "{all}");

    let mut keyed = names();
    keyed
        .accounts
        .insert("acct-key".into(), "Team OpenAI key".into());
    keyed
        .accounts
        .insert("acct-local".into(), "Ollama box".into());
    keyed.no_window = vec![
        ("acct-key".into(), crate::usage::NO_WINDOW_KEY),
        ("acct-local".into(), crate::usage::NO_WINDOW_LOCAL),
    ];
    let all = text(&summary(
        &none,
        UsageRange::Today,
        &keyed,
        NOW,
        0,
        200,
        &ctx,
    ))
    .join("\n");
    for shown in [
        "Team OpenAI key",
        crate::usage::NO_WINDOW_KEY,
        "Ollama box",
        crate::usage::NO_WINDOW_LOCAL,
    ] {
        assert!(
            all.contains(shown),
            "DEC-141 PA-16: a key or local account says what it has: {all}"
        );
    }
    assert!(!all.contains("No limits read yet."), "{all}");
}

/// PROTO-2 F1 TUI sibling: a reset time the owner set that has passed is not shown, so the
/// window's line names no time of the owner's.
#[test]
fn a_passed_manual_reset_is_not_marked_as_the_owners_time() {
    let ctx = ctx(100, 40);
    let mut passed = limits(None);
    passed.five_hour_reset_override_ms = Some(NOW - MINUTE);
    let shown = UsageReport {
        limits: vec![passed],
        ..report()
    };
    let all = text(&summary(
        &shown,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    let five_hour = all
        .lines()
        .find(|line| line.contains("5-hour"))
        .expect("the 5-hour line shows");
    assert!(five_hour.contains("Next reset not known yet"), "{all}");
    assert!(!five_hour.contains("(your time)"), "{all}");
}

/// R4 N-D16 follow-up: an account whose read failed before any window was read shows its
/// cause first and each window as not read, pointing at that cause, never as a window the
/// provider left out.
#[test]
fn a_failed_read_with_no_window_shows_each_window_as_not_read_after_its_cause() {
    let ctx = ctx(100, 40);
    let unread = AccountLimits {
        plan: None,
        five_hour: None,
        weekly: None,
        polled_five_hour: None,
        polled_weekly: None,
        source: None,
        observed_ms: None,
        ..limits(Some("native_profile_credential_unavailable"))
    };
    let lines = text(&summary(
        &UsageReport {
            limits: vec![unread],
            ..report()
        },
        UsageRange::Week,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    let at = |wanted: &str| lines.iter().position(|line| line == wanted);
    let cause = at("    CrazyAgent can't read this account's sign-in. Sign in again in Accounts.");
    let five_hour = at("    5-hour  Not read. The last read failed, for the reason above.");
    let weekly = at("    Weekly  Not read. The last read failed, for the reason above.");
    assert!(
        cause.is_some() && five_hour.is_some() && weekly.is_some(),
        "{lines:#?}"
    );
    assert!(cause < five_hour && five_hour < weekly, "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("Not reported")),
        "{lines:#?}"
    );
}

/// F1 / ND16F-M1: an API-key account has no 5-hour or weekly window by design, so its
/// windows keep the provider-omitted wording under its note, never a failed read.
#[test]
fn an_api_key_account_s_windows_keep_the_provider_omitted_wording() {
    let ctx = ctx(100, 40);
    let keyed = AccountLimits {
        plan: None,
        five_hour: None,
        weekly: None,
        polled_five_hour: None,
        polled_weekly: None,
        source: None,
        observed_ms: None,
        ..limits(Some("native_profile_codex_api_key"))
    };
    let lines = text(&summary(
        &UsageReport {
            limits: vec![keyed],
            ..report()
        },
        UsageRange::Week,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    for wanted in [
        "    An API-key account has no 5-hour or weekly window.",
        "    5-hour  Not reported",
        "    Weekly  Not reported",
    ] {
        assert!(
            lines.iter().any(|line| line == wanted),
            "{wanted:?} in {lines:#?}"
        );
    }
    assert!(
        !lines.iter().any(|line| line.contains("Not read.")),
        "{lines:#?}"
    );
}

/// N-D16 run 6: a window a good poll left out names the provider that left it out and what
/// that means, Codex for the plan and Claude for now, while a failed read keeps its own wording.
#[test]
fn a_window_a_good_poll_left_out_names_its_provider_and_a_failed_read_keeps_its_wording() {
    use crate::usage::poll::Provider;
    let ctx = ctx(100, 40);
    let shown = |limits: AccountLimits| {
        text(&summary(
            &UsageReport {
                limits: vec![limits],
                ..report()
            },
            UsageRange::Week,
            &names(),
            NOW,
            0,
            96,
            &ctx,
        ))
    };
    let five_hour_out = |provider: Provider, label: &str, failure: Option<&str>| AccountLimits {
        five_hour: None,
        polled_five_hour: None,
        five_hour_left_out: Some(provider),
        five_hour_left_out_label: Some(label.into()),
        ..limits(failure)
    };
    let codex = shown(five_hour_out(
        Provider::Codex,
        "Codex reports no 5-hour limit for this plan.",
        None,
    ));
    let engine = shown(five_hour_out(
        Provider::Claude,
        "A sentence only the engine wrote.",
        None,
    ));
    let weekly = shown(AccountLimits {
        weekly: None,
        polled_weekly: None,
        weekly_left_out: Some(Provider::Codex),
        weekly_left_out_label: Some("A weekly sentence only the engine wrote.".into()),
        ..limits(None)
    });
    let failed = shown(five_hour_out(
        Provider::Codex,
        "Codex reports no 5-hour limit for this plan.",
        Some("usage_unreachable"),
    ));
    for (lines, wanted) in [
        (
            &codex,
            "    5-hour  Codex reports no 5-hour limit for this plan.",
        ),
        (&engine, "    5-hour  A sentence only the engine wrote."),
        (
            &weekly,
            "    Weekly  A weekly sentence only the engine wrote.",
        ),
        (
            &failed,
            "    5-hour  Not read. The last read failed, for the reason above.",
        ),
    ] {
        assert!(
            lines.iter().any(|line| line == wanted),
            "{wanted:?} in {lines:#?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("Not reported")),
            "{lines:#?}"
        );
    }
}

/// Takes the next usage read the page sent, past any stored extras read an earlier read left
/// queued, and the extras read every usage read sends with it. Input: the context. Output:
/// the range start the usage read asked for.
fn asked_since(ctx: &mut Ctx) -> Option<i64> {
    loop {
        match ctx.next_request() {
            Some(Request::Settings(job)) => match *job {
                SettingsJob::Extras { detail } => {
                    assert!(!detail, "the usage screen never asks for the detail rows");
                }
                SettingsJob::Usage { since_ms, .. } => {
                    let next = ctx.next_request();
                    assert!(
                        matches!(&next, Some(Request::Settings(job)) if matches!(**job, SettingsJob::Extras { detail: false })),
                        "each usage read also reads the stored extras"
                    );
                    return since_ms;
                }
                other => panic!("a usage read expected, got {other:?}"),
            },
            other => panic!("a usage read expected, got {other:?}"),
        }
    }
}

#[test]
fn a_reply_for_an_earlier_range_never_shows_under_the_new_label_and_the_newer_error_is_said() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let week = asked_since(&mut ctx);
    assert!(week.is_some());
    page.key(&Key::Char('t'), &mut ctx);
    let month = asked_since(&mut ctx);
    page.key(&Key::Char('t'), &mut ctx);
    assert_eq!(asked_since(&mut ctx), None, "the third range is all time");
    assert!(month.is_some() && month < week);

    let mut late = report();
    late.since_ms = week;
    page.apply(&Ok(Box::new(late)), &mut ctx);
    let shown = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(
        !shown.contains("Tokens 2.3K"),
        "the Last 7 days totals never show under All time: {shown}"
    );
    assert!(page.busy(&ctx), "the All time read is still awaited");

    page.apply(&Err("usage_outcome_unknown".into()), &mut ctx);
    let failed = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(failed.contains("Could not read usage."), "{failed}");
    assert!(!failed.contains("Tokens 2.3K"), "{failed}");
    assert!(!page.busy(&ctx));

    page.key(&Key::Char('r'), &mut ctx);
    let all = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(report())), &mut ctx);
    assert_eq!(all, None);
    let shown = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(shown.contains("All time"), "{shown}");
    assert!(shown.contains("Tokens 2.3K"), "{shown}");

    page.key(&Key::Char('t'), &mut ctx);
    asked_since(&mut ctx);
    let waiting = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(
        !waiting.contains("Today"),
        "until Today's reply arrives the shown totals keep their own label: {waiting}"
    );
    assert!(waiting.contains("All time"), "{waiting}");
}

#[test]
fn the_page_asks_for_its_range_and_shows_answers_failures_and_not_ready() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let Some(Request::Settings(job)) = ctx.next_request() else {
        panic!("the page asks for usage");
    };
    assert_eq!(format!("{job:?}"), "Usage");
    let Some(Request::Settings(job)) = ctx.next_request() else {
        panic!("the page reads the stored extras with it");
    };
    assert_eq!(format!("{job:?}"), "Extras");
    assert!(page.busy(&ctx));
    assert!(
        text(&page.body(96, NOW, 0, &ctx))
            .join("\n")
            .contains("Reading usage")
    );

    page.apply(&Err("usage_outcome_unknown".into()), &mut ctx);
    let failed = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(
        failed.contains("Could not read usage. CrazyAgent couldn't confirm what happened."),
        "{failed}"
    );
    assert!(!page.busy(&ctx));

    assert!(matches!(page.key(&Key::Char('t'), &mut ctx), Flow::Handled));
    let mut month = report();
    month.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(month)), &mut ctx);
    let shown = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(shown.contains("Last 30 days"), "{shown}");
    assert!(shown.contains("Tokens 2.3K"), "{shown}");

    assert!(matches!(page.key(&Key::Char('c'), &mut ctx), Flow::Handled));
    let Some(Request::Settings(job)) = ctx.next_request() else {
        panic!("Check limits now asks the engine");
    };
    assert_eq!(format!("{job:?}"), "RefreshUsage");

    page.apply(&Err("usage_unavailable".into()), &mut ctx);
    assert!(
        text(&page.body(96, NOW, 0, &ctx))
            .join("\n")
            .contains("Usage isn't ready yet in this engine.")
    );
    assert_eq!(page.place(), Some(Place::Usage));
    assert!(matches!(page.key(&Key::Char('x'), &mut ctx), Flow::Ignored));
}

#[test]
fn a_harness_without_a_reader_reads_as_not_reported_never_as_zero() {
    let ctx = ctx(100, 40);
    let unreported = |key: &str, harness: &str| UsageRow {
        tokens: Tokens::default(),
        runs: 0,
        not_reported_by: Some(harness.into()),
        ..usage_row(key)
    };
    let only = UsageReport {
        total: Tokens::default(),
        runs: 0,
        projects: vec![unreported("project-harbor", "RCLI")],
        pools: Vec::new(),
        accounts: Vec::new(),
        agents: vec![unreported("master-project-harbor", "RCLI")],
        lines: Vec::new(),
        not_reported_by: vec!["RCLI".into()],
        ..report()
    };
    let lines = text(&summary(
        &only,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    assert_eq!(lines[1], "Usage not reported by RCLI  Lines +0 -0");
    let all = lines.join("\n");
    assert!(
        all.contains("By project\n  Harbor Notes  usage not reported by RCLI"),
        "{all}"
    );
    assert!(!all.contains("No usage in this range"), "{all}");
    assert!(
        !all.contains("Tokens 0") && !all.contains(" 0 tokens"),
        "{all}"
    );
    assert!(!all.contains("Re-read from cache"), "{all}");

    let mixed = UsageReport {
        projects: vec![
            UsageRow {
                not_reported_by: Some("Gemini CLI".into()),
                ..usage_row("project-harbor")
            },
            unreported("project-docs", "OpenCode"),
        ],
        not_reported_by: vec!["Gemini CLI".into(), "OpenCode".into()],
        ..report()
    };
    let lines = text(&summary(
        &mixed,
        UsageRange::Week,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    assert_eq!(
        lines[1],
        "Tokens 2.3K (1.2K input, 800 cache write, 300 output)"
    );
    assert_eq!(
        lines[2],
        "Re-read from cache 4K: 67% of everything read, not in Tokens"
    );
    assert_eq!(lines[3], "Runs 3  Lines +90 -57");
    assert_eq!(
        lines[4],
        "Not in these totals: usage not reported by Gemini CLI, OpenCode."
    );
    let all = lines.join("\n");
    for wanted in [
        "  Harbor Notes    1.5K tokens  2 runs  + usage not reported by Gemini CLI",
        "  project-docs  usage not reported by OpenCode",
    ] {
        assert!(all.contains(wanted), "{wanted:?} in\n{all}");
    }
}

#[test]
fn a_usage_step_that_panicked_shows_one_plain_line_with_its_count_and_last_time() {
    let ctx = ctx(100, 40);
    let healthy = text(&summary(
        &report(),
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    assert!(!healthy.contains("internal error"), "{healthy}");
    let degraded = UsageReport {
        health: vec![
            StepHealth {
                step: UsageStep::Poll,
                panics: 1,
                last_ms: NOW - 30_000,
                recovered_ms: None,
            },
            StepHealth {
                step: UsageStep::Team,
                panics: 3,
                last_ms: NOW - 4 * MINUTE,
                recovered_ms: None,
            },
            StepHealth {
                step: UsageStep::Runs,
                panics: 2,
                last_ms: NOW - 4 * MINUTE,
                recovered_ms: Some(NOW - MINUTE),
            },
        ],
        ..report()
    };
    let lines = text(&summary(
        &degraded,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    assert_eq!(
        lines[4],
        "Checking account limits hit an internal error once since the engine started, last just now. CrazyAgent keeps retrying; limits shown may be out of date."
    );
    assert_eq!(
        lines[5],
        "Counting agent tokens hit an internal error 3 times since the engine started, last 4 min ago. CrazyAgent keeps retrying; agent and research run token totals may be low."
    );
    assert_eq!(
        lines[6],
        "Counting research run tokens hit an internal error 2 times since the engine started, last 4 min ago. It worked again 1 min ago."
    );
    assert_eq!(lines[8], "Account limits");
}

fn money(shown: crate::usage::money::MainNumber) -> MoneyView {
    MoneyView {
        shown,
        as_of: Some("2026-11-08".into()),
        cost_nanos: 5_000_000_000,
        ..MoneyView::default()
    }
}

#[test]
fn every_tui_dollar_carries_the_label() {
    assert_eq!(
        money_text(5_000_000_000),
        "$5.00 (Estimated cost at API prices)"
    );
    assert_eq!(
        money_text(4_000_000),
        "<$0.01 (Estimated cost at API prices)"
    );
    assert_eq!(
        money_text(5_000_000),
        "$0.01 (Estimated cost at API prices)"
    );
    assert_eq!(money_text(0), "$0.00 (Estimated cost at API prices)");
    assert_eq!(
        money_text(i64::MAX),
        "$9223372036.85 (Estimated cost at API prices)"
    );
}

#[test]
fn a_row_leads_with_the_chosen_number_and_shows_only_tokens_without_prices() {
    use crate::usage::money::MainNumber;
    assert_eq!(
        figures_text("1.2K tokens", 5_000_000_000, &money(MainNumber::Dollars)),
        "$5.00 (Estimated cost at API prices)  1.2K tokens"
    );
    assert_eq!(
        figures_text("1.2K tokens", 5_000_000_000, &money(MainNumber::Tokens)),
        "1.2K tokens  $5.00 (Estimated cost at API prices)"
    );
    assert_eq!(
        figures_text("1.2K tokens", 5_000_000_000, &MoneyView::default()),
        "1.2K tokens"
    );
}

#[test]
fn the_summary_shows_each_rows_dollars_and_the_total_with_prices_on() {
    use crate::usage::money::MainNumber;
    let ctx = ctx(100, 40);
    let mut priced = report();
    priced.money = money(MainNumber::Dollars);
    priced.projects = vec![UsageRow {
        cost_nanos: 2_000_000_000,
        ..usage_row("project-harbor")
    }];
    let shown = text(&summary(
        &priced,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ))
    .join("\n");
    assert!(
        shown.contains("$2.00 (Estimated cost at API prices)"),
        "{shown}"
    );
    assert!(
        shown
            .lines()
            .any(|line| line == "$5.00 (Estimated cost at API prices)"),
        "{shown}"
    );
}

#[test]
fn the_money_keys_flip_one_setting_and_send_it() {
    use crate::usage::money::{MainNumber, MoneySettings, PriceSource};
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut answer = report();
    answer.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(answer)), &mut ctx);
    let labels: Vec<(String, String)> = page
        .hints(&ctx)
        .into_iter()
        .map(|hint| (hint.key, hint.label))
        .collect();
    assert!(
        labels.contains(&("m".to_owned(), "Dollars or tokens".to_owned())),
        "{labels:?}"
    );
    assert!(
        labels.contains(&("p".to_owned(), "Prices on or off".to_owned())),
        "{labels:?}"
    );
    let sent = |key: char, page: &mut UsagePage, ctx: &mut Ctx| -> MoneySettings {
        assert!(matches!(page.key(&Key::Char(key), ctx), Flow::Handled));
        let Some(Request::Settings(job)) = ctx.next_request() else {
            panic!("the money key asks the engine");
        };
        let SettingsJob::SetMoney(settings) = *job else {
            panic!("the money key sets the money choice");
        };
        settings
    };
    assert_eq!(
        sent('m', &mut page, &mut ctx),
        MoneySettings {
            main_number: MainNumber::Tokens,
            prices: PriceSource::Bundled,
            ..MoneySettings::default()
        }
    );
    assert_eq!(
        sent('p', &mut page, &mut ctx),
        MoneySettings {
            main_number: MainNumber::Dollars,
            prices: PriceSource::Off,
            ..MoneySettings::default()
        }
    );
}

#[test]
fn a_priced_row_at_80_columns_wraps_so_no_figure_or_label_is_cut() {
    use crate::usage::money::MainNumber;
    let limit = 76;
    let ctx = ctx(80, 40);
    let mut priced = report();
    priced.money = money(MainNumber::Tokens);
    priced.projects = vec![UsageRow {
        cost_nanos: 2_000_000_000,
        ..usage_row(&"p".repeat(32))
    }];
    let lines = text(&summary(
        &priced,
        UsageRange::Today,
        &names(),
        NOW,
        0,
        limit,
        &ctx,
    ));
    assert!(
        lines.iter().all(|line| line.chars().count() <= limit),
        "{lines:#?}"
    );
    let joined = lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("$2.00 (Estimated cost at API prices)"),
        "{joined}"
    );
    assert!(joined.contains("2 runs"), "{joined}");
}

#[test]
fn a_recount_fault_reads_in_plain_words() {
    let line = health_line(
        &StepHealth {
            step: UsageStep::Recount,
            panics: 1,
            last_ms: NOW - 30_000,
            recovered_ms: None,
        },
        NOW,
        0,
    );
    assert!(
        line.starts_with("Recounting history hit an internal error once"),
        "{line}"
    );
    assert!(line.contains("the comparison may arrive later"), "{line}");
}

#[test]
fn with_prices_off_the_main_number_key_is_not_offered_and_does_nothing() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut answer = report();
    answer.money.settings.prices = PriceSource::Off;
    answer.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(answer)), &mut ctx);
    let keys: Vec<String> = page.hints(&ctx).into_iter().map(|hint| hint.key).collect();
    assert!(!keys.contains(&"m".to_owned()), "{keys:?}");
    assert!(keys.contains(&"p".to_owned()), "{keys:?}");
    assert!(matches!(page.key(&Key::Char('m'), &mut ctx), Flow::Ignored));
    assert!(ctx.next_screen_request().is_none());
}

#[test]
fn the_recount_offer_shows_both_totals_and_how_to_choose() {
    let status = RecountStatus {
        old: Tokens {
            input: 4000,
            ..Tokens::default()
        },
        new: Tokens {
            input: 1000,
            ..Tokens::default()
        },
        new_cost_nanos: 5_000_000_000,
        sessions_recounted: 2,
        sessions_kept_old: 1,
        homes_partial: 0,
    };
    let text: Vec<String> = recount_lines(&status, true)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(text[1].contains("Not estimated"));
    assert!(text[2].contains("$5.00 (Estimated cost at API prices)"));
    assert!(text[3].starts_with("2 sessions recounted, 1 keep"));
    assert!(text[4].contains("the account their agent last used"));
    assert!(text[5].contains("New dollars cover recounted sessions only"));
    assert!(text[6].contains("Press n"));
    let off: Vec<String> = recount_lines(&status, false)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(off.iter().all(|line| !line.contains('$')
        && !line.contains("Estimated cost")
        && !line.contains("Not estimated")));
    assert!(off.last().unwrap().contains("Press n"));
}

fn offer() -> RecountStatus {
    RecountStatus {
        old: Tokens {
            input: 4000,
            ..Tokens::default()
        },
        new: Tokens {
            input: 1000,
            ..Tokens::default()
        },
        new_cost_nanos: 5_000_000_000,
        sessions_recounted: 2,
        sessions_kept_old: 1,
        homes_partial: 0,
    }
}

#[test]
fn the_recount_offer_wraps_to_the_screen_under_its_notice() {
    let mut view = report();
    view.counting_notice = Some("These numbers still use the old counting.".to_owned());
    view.recount = Some(offer());
    let lines = text(&recount_head(&view, 60));
    assert_eq!(lines[0], "These numbers still use the old counting.");
    assert!(
        lines.iter().all(|line| line.chars().count() <= 60),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("Press n")),
        "{lines:?}"
    );
}

#[test]
fn the_recount_keys_work_only_while_it_is_offered() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut answer = report();
    answer.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(answer.clone())), &mut ctx);
    assert!(matches!(page.key(&Key::Char('n'), &mut ctx), Flow::Ignored));
    answer.recount = Some(offer());
    page.apply(&Ok(Box::new(answer)), &mut ctx);
    for (key, expected) in [('n', true), ('o', false)] {
        assert!(matches!(page.key(&Key::Char(key), &mut ctx), Flow::Handled));
        let Some(Request::Settings(job)) = ctx.next_request() else {
            panic!("the recount key asks the engine");
        };
        let SettingsJob::DecideRecount(use_new) = *job else {
            panic!("the recount key sends the choice");
        };
        assert_eq!(use_new, expected);
    }
}

fn activity_row(activity: &str, turns: u64, edit_turns: u64, first_try_turns: u64) -> ActivityRow {
    ActivityRow {
        activity: activity.to_owned(),
        turns,
        edit_turns,
        first_try_turns,
    }
}

fn model_row(model: &str, input: u64, cost_nanos: i64) -> ModelRow {
    ModelRow {
        model: model.to_owned(),
        tokens: Tokens {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        },
        cost_nanos,
    }
}

#[test]
fn the_activity_lines_end_with_the_first_try_rate() {
    let ctx = ctx(100, 40);
    let rows = [activity_row("coding", 4, 4, 3)];
    assert_eq!(
        text(&activity_lines(&rows, 96, &ctx.theme)),
        [
            "  Coding         4 turns",
            "75% of 4 editing turns worked first try"
        ]
    );
}

#[test]
fn the_first_try_rate_rounds_to_the_nearest_percent_and_says_one_turn() {
    let ctx = ctx(100, 40);
    let rate = |rows: &[ActivityRow]| text(&activity_lines(rows, 96, &ctx.theme)).pop();
    assert_eq!(
        rate(&[activity_row("coding", 3, 3, 2)]).as_deref(),
        Some("67% of 3 editing turns worked first try")
    );
    assert_eq!(
        rate(&[activity_row("coding", 1, 1, 1)]).as_deref(),
        Some("100% of 1 editing turn worked first try")
    );
}

#[test]
fn no_first_try_rate_shows_without_an_editing_turn() {
    let ctx = ctx(100, 40);
    assert_eq!(
        text(&activity_lines(
            &[activity_row("git", 1, 0, 0)],
            96,
            &ctx.theme
        )),
        ["  Git            1 turn"]
    );
}

#[test]
fn activities_past_the_cap_are_counted_and_stay_in_the_rate() {
    let ctx = ctx(100, 40);
    let rows: Vec<ActivityRow> = [
        "build_deploy",
        "coding",
        "debugging",
        "feature",
        "testing",
        "refactoring",
        "exploration",
    ]
    .iter()
    .map(|name| activity_row(name, 2, 2, 1))
    .collect();
    let shown = text(&activity_lines(&rows, 96, &ctx.theme));
    assert_eq!(shown[0], "  Build/deploy   2 turns");
    assert_eq!(shown.len(), 7, "{shown:?}");
    assert_eq!(shown[5], "  and 2 more");
    assert_eq!(shown[6], "50% of 14 editing turns worked first try");
}

#[test]
fn model_lines_name_the_blank_model_show_dollars_and_stop_at_the_cap() {
    let ctx = ctx(100, 40);
    let mut rows = vec![model_row("gpt-5", 1200, 0), model_row("", 900, 0)];
    rows.extend((0..5).map(|n| model_row(&format!("m{n}"), 10, 0)));
    let shown = text(&model_lines(&rows, &MoneyView::default(), 96, &ctx.theme));
    assert_eq!(shown[0], "  gpt-5  1.2K tokens");
    assert_eq!(shown[1], "  Model not recorded  900 tokens");
    assert_eq!(shown.len(), 6, "{shown:?}");
    assert_eq!(shown[5], "  and 2 more");
    let money = MoneyView {
        shown: MainNumber::Dollars,
        as_of: Some("2026-11-08".to_owned()),
        ..MoneyView::default()
    };
    let priced = text(&model_lines(
        &[model_row("gpt-5", 1200, 1_000_000_000)],
        &money,
        96,
        &ctx.theme,
    ));
    assert_eq!(
        priced,
        ["  gpt-5  $1.00 (Estimated cost at API prices)  1.2K tokens"]
    );
}

#[test]
fn the_summary_lists_models_and_activities_after_the_accounts_only_when_there_are_any() {
    let ctx = ctx(100, 40);
    let bare = text(&summary(
        &report(),
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    assert!(
        !bare
            .iter()
            .any(|line| line == "By model" || line == "By activity"),
        "{bare:?}"
    );
    let shown = text(&summary(
        &UsageReport {
            models: vec![model_row("gpt-5", 1200, 0)],
            activity: vec![activity_row("coding", 4, 4, 3)],
            ..report()
        },
        UsageRange::Today,
        &names(),
        NOW,
        0,
        96,
        &ctx,
    ));
    let place = |wanted: &str| {
        shown
            .iter()
            .position(|line| line == wanted)
            .unwrap_or_else(|| panic!("{wanted}: {shown:?}"))
    };
    assert!(place("By account") < place("By model"));
    assert!(place("By model") < place("By activity"));
    assert_eq!(place("  gpt-5  1.2K tokens"), place("By model") + 1);
    assert_eq!(place("  Coding         4 turns"), place("By activity") + 1);
    assert!(place("75% of 4 editing turns worked first try") > place("By activity"));
}

#[test]
fn model_and_activity_lines_fit_a_narrow_screen() {
    let ctx = ctx(30, 40);
    let limit = 24;
    let activity = text(&activity_lines(
        &[activity_row("build_deploy", 2, 2, 1)],
        limit,
        &ctx.theme,
    ));
    let models = text(&model_lines(
        &[model_row(
            "a-very-long-model-name-that-will-not-fit",
            1200,
            0,
        )],
        &MoneyView::default(),
        limit,
        &ctx.theme,
    ));
    assert!(activity.len() > 1, "{activity:?}");
    assert!(models.len() > 1, "{models:?}");
    for line in activity.iter().chain(&models) {
        assert!(line.chars().count() <= limit, "{line}");
    }
}

#[test]
fn a_second_recount_choice_says_it_was_already_decided() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    let refused = Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Settings(
        Box::new(SettingsEvent::RecountDecided(Err(
            "usage_recount_not_ready".into(),
        ))),
    )));
    page.arrive(&refused, &mut ctx);
    let said = ctx.message().expect("the refusal is said");
    assert_eq!(
        (said.lead.as_str(), said.rest.as_str()),
        (
            "Could not apply that choice.",
            "This recount was already decided. Press r to read again."
        )
    );
}

fn extras_of(
    account: &str,
    extras: crate::usage::extras::LimitExtras,
) -> crate::engine::owner::AccountExtras {
    crate::engine::owner::AccountExtras {
        account: CredentialRef::new(account).unwrap(),
        extras,
    }
}

fn not_reported(harness: &str) -> crate::usage::extras::LimitExtras {
    crate::usage::extras::LimitExtras {
        harness: harness.into(),
        adapter: crate::usage::extras::Adapter::NotReported,
        may_break: false,
        resets: None,
        credits: None,
        spend: crate::usage::extras::Spend::NotReported,
        not_reported: format!("not reported by {harness}"),
        resets_checked_ms: None,
        credits_checked_ms: None,
        resets_gained_ms: None,
        resets_unread: None,
    }
}

fn extras_arrival(rows: Vec<crate::engine::owner::AccountExtras>) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Settings(
        Box::new(SettingsEvent::Extras(Ok(rows))),
    )))
}

/// DEC-154: the TUI usage screen reads what the engine stored beside each read of usage,
/// never the detail rows, and shows each account's free resets and credits line under its
/// limits, a key, URL or local account too, word for word the desktop's.
#[test]
fn each_account_s_limits_end_with_its_free_resets_and_credits_line() {
    let mut ctx = ctx(120, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut asked = Vec::new();
    while let Some(request) = ctx.next_request() {
        if let Request::Settings(job) = request {
            asked.push(format!("{job:?}"));
            if let SettingsJob::Extras { detail } = *job {
                assert!(!detail, "the usage screen never asks for the detail rows");
            }
        }
    }
    assert!(asked.iter().any(|job| job == "Extras"), "{asked:?}");

    let mut report = report();
    report.since_ms = page.asked.and_then(|(_, since)| since);
    report.limits = vec![limits(None)];
    page.apply(&Ok(Box::new(report)), &mut ctx);
    let mut claude = not_reported("Claude Code");
    claude.adapter = crate::usage::extras::Adapter::Claude;
    claude.may_break = true;
    claude.resets = Some(crate::usage::extras::Resets {
        count: 2,
        kinds: vec![crate::usage::extras::ResetKind::FiveHour],
        next_expiry_ms: None,
    });
    let mut key = not_reported("OpenRouter");
    key.credits = Some("$3.10 left".into());
    page.arrive(
        &extras_arrival(vec![
            extras_of("acct-claude", claude),
            extras_of("acct-key", key),
        ]),
        &mut ctx,
    );
    ctx.store.accounts = Load::Ready(Vec::new());
    let shown = text(&page.body(116, NOW, 0, &ctx)).join("\n");
    assert!(
        shown.contains(
            "2 free resets (5-hour) (may break) \u{b7} Credits: not reported by Claude Code"
        ),
        "{shown}"
    );
    let at = shown.find("Weekly").unwrap();
    assert!(
        shown.find("2 free resets").unwrap() > at,
        "the line comes under the bars: {shown}"
    );
}

#[test]
fn a_key_or_local_card_also_ends_with_its_line_and_no_extras_read_adds_nothing() {
    let ctx = ctx(120, 40);
    let mut keyed = names();
    keyed.accounts.insert("acct-key".into(), "Team key".into());
    keyed.no_window = vec![("acct-key".into(), crate::usage::NO_WINDOW_KEY)];
    let none = UsageReport {
        limits: Vec::new(),
        ..report()
    };
    let bare = text(&summary(
        &none,
        UsageRange::Today,
        &keyed,
        NOW,
        0,
        200,
        &ctx,
    ))
    .join("\n");
    assert!(
        !bare.contains("free reset") && !bare.contains("Credits:"),
        "{bare}"
    );

    let mut key = not_reported("OpenRouter");
    key.credits = Some("$3.10 left".into());
    key.spend = crate::usage::extras::Spend::Elsewhere {
        place: "openrouter.ai".into(),
    };
    keyed.extras.insert("acct-key".into(), key);
    let shown = text(&summary(
        &none,
        UsageRange::Today,
        &keyed,
        NOW,
        0,
        200,
        &ctx,
    ))
    .join("\n");
    assert!(
        shown.contains("    Free resets: not reported by OpenRouter \u{b7} Credits: $3.10 left \u{b7} Spend at openrouter.ai"),
        "{shown}"
    );
}

#[test]
fn money_one_each_key_sends_only_its_own_choice() {
    let mut ctx = ctx(100, 40);
    let mut page = UsagePage::new();
    page.shown(&mut ctx);
    let mut answer = report();
    answer.since_ms = asked_since(&mut ctx);
    page.apply(&Ok(Box::new(answer)), &mut ctx);
    for key in ['m', 'p'] {
        assert!(
            matches!(page.key(&Key::Char(key), &mut ctx), Flow::Handled),
            "{key}"
        );
        let Some(Request::Settings(job)) = ctx.next_request() else {
            panic!("the {key} key asks the engine");
        };
        assert_eq!(format!("{job:?}"), "SetMoneyOne", "{key}");
    }
}
