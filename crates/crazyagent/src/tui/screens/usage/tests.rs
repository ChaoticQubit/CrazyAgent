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
        not_reported_by: None,
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
    }
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
        "Tokens 6.3K (1.2K in, 300 out)  Cache 67%  Runs 3  Lines +90 -57"
    );
    let all = lines.join("\n");
    for wanted in [
        "  Work Claude  max  read from the provider 3 min ago",
        "    5-hour   34% used  Resets in 2 h 0 min",
        "    Weekly    0% used  Next reset not known yet",
        "    The provider asked CrazyAgent to wait before reading limits again.",
        "By project\n  Harbor Notes    5.5K tokens  2 runs",
        "By pool\n  Main pool    5.5K tokens  2 runs",
        "By account\n  acct-0    5.5K tokens  2 runs",
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
        "By project\n  Harbor Notes    5.5K tokens  2 runs\n  Unattributed    5.5K tokens  2 runs",
        "By account\n  Unattributed    5.5K tokens  2 runs",
    ] {
        assert!(all.contains(wanted), "{wanted:?} in\n{all}");
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
    assert!(all.contains("Cache none"), "{all}");
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
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    page.apply(&Ok(Box::new(report())), &mut ctx);
    let shown = text(&page.body(96, NOW, 0, &ctx)).join("\n");
    assert!(shown.contains("Last 30 days"), "{shown}");
    assert!(shown.contains("Tokens 6.3K"), "{shown}");

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
        "Tokens 6.3K (1.2K in, 300 out)  Cache 67%  Runs 3  Lines +90 -57"
    );
    assert_eq!(
        lines[2],
        "Not in these totals: usage not reported by Gemini CLI, OpenCode."
    );
    let all = lines.join("\n");
    for wanted in [
        "  Harbor Notes    5.5K tokens  2 runs  + usage not reported by Gemini CLI",
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
        lines[2],
        "Checking account limits hit an internal error once since the engine started, last just now. CrazyAgent keeps retrying; limits shown may be out of date."
    );
    assert_eq!(
        lines[3],
        "Counting agent tokens hit an internal error 3 times since the engine started, last 4 min ago. CrazyAgent keeps retrying; agent and research run token totals may be low."
    );
    assert_eq!(
        lines[4],
        "Counting research run tokens hit an internal error 2 times since the engine started, last 4 min ago. It worked again 1 min ago."
    );
    assert_eq!(lines[6], "Account limits");
}
