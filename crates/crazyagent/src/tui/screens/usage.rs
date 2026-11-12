use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::{ago, local_offset, now_ms};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{indent, row, styled, wrap};
use crate::tui::theme::{Role, Theme};
use crate::usage::RecountStatus;
use crate::usage::extras::LimitExtras;
use crate::usage::health::{StepHealth, UsageStep};
use crate::usage::metrics::{ActivityRow, ModelRow};
use crate::usage::money::{MainNumber, MoneyView, PriceSource};
use crate::usage::report::{UsageReport, UsageRow};
use crate::usage::{AccountLimits, LimitSource, LimitWindow};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const ROWS_SHOWN: usize = 5;
const UNITS: [(f64, &str); 3] = [(1e3, "K"), (1e6, "M"), (1e9, "B")];

/// One dollar figure with its label. Input: nanodollars. Output: the text every TUI dollar uses.
pub fn money_text(cost_nanos: i64) -> String {
    let cents = cost_nanos.saturating_add(5_000_000) / 10_000_000;
    let amount = if cost_nanos > 0 && cost_nanos < 5_000_000 {
        "<$0.01".to_owned()
    } else {
        format!("${}.{:02}", cents / 100, cents % 100)
    };
    format!("{amount} (Estimated cost at API prices)")
}

/// One row's figures in the order the owner chose. Inputs: the tokens text, the row's dollars
/// and the view. Output: the text. With no price date only the tokens are shown.
pub fn figures_text(tokens: &str, cost_nanos: i64, money: &MoneyView) -> String {
    if money.as_of.is_none() {
        return tokens.to_owned();
    }
    match money.shown {
        MainNumber::Dollars => format!("{}  {tokens}", money_text(cost_nanos)),
        MainNumber::Tokens => format!("{tokens}  {}", money_text(cost_nanos)),
    }
}

/// The range the Usage summary covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageRange {
    Today,
    Week,
    Month,
    All,
}

impl UsageRange {
    /// The plain label. Output: "Today", "Last 7 days", "Last 30 days" or "All time".
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
            Self::All => "All time",
        }
    }

    /// The next range, wrapping from All time back to Today.
    pub fn next(self) -> Self {
        match self {
            Self::Today => Self::Week,
            Self::Week => Self::Month,
            Self::Month => Self::All,
            Self::All => Self::Today,
        }
    }

    /// Where the range starts: local midnight today, six or twenty-nine days before it so
    /// the range holds whole local days, or None for all time. Inputs: now in epoch
    /// milliseconds and the local offset in seconds east of UTC. Output: the start.
    pub fn start(self, now_ms: i64, offset_seconds: i64) -> Option<i64> {
        let offset = offset_seconds * 1000;
        let midnight = (now_ms + offset).div_euclid(DAY_MS) * DAY_MS - offset;
        match self {
            Self::Today => Some(midnight),
            Self::Week => Some(midnight - 6 * DAY_MS),
            Self::Month => Some(midnight - 29 * DAY_MS),
            Self::All => None,
        }
    }
}

/// A count in a few characters: 950, 1.2K, 3.4M. Input: the count. Output: the text.
pub fn compact(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    let value = value as f64;
    for (index, (size, unit)) in UNITS.iter().enumerate() {
        let shown = value / size;
        if shown < 999.95 || index == UNITS.len() - 1 {
            let text = format!("{shown:.1}");
            return format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text));
        }
    }
    unreachable!("the last unit always returns")
}

/// How long until a window resets. Inputs: the reset time, None when unknown, and now.
/// Output: "Resets in 2 h 14 min", "Resets in 3 d 4 h" or "Next reset not known yet".
pub fn reset_text(at_ms: Option<i64>, now_ms: i64) -> String {
    let Some(at_ms) = at_ms else {
        return "Next reset not known yet".into();
    };
    let minutes = (at_ms - now_ms).div_euclid(60_000);
    if minutes < 1 {
        return "Resets in under a minute".into();
    }
    let hours = minutes / 60;
    if hours < 1 {
        return format!("Resets in {minutes} min");
    }
    if hours < 24 {
        return format!("Resets in {hours} h {} min", minutes % 60);
    }
    format!("Resets in {} d {} h", hours / 24, hours % 24)
}

/// One plain sentence for the last failed read of an account's limits. Input: the
/// stored code. Output: the sentence.
pub fn poll_failure(code: &str) -> String {
    match code {
        "usage_sign_in_lapsed" => {
            "The provider no longer accepts this sign-in. Sign in again in Accounts.".into()
        }
        "usage_sign_in_rejected" => {
            "This account's sign-in was rejected. Sign in again in Accounts.".into()
        }
        "usage_rate_limited" => {
            "The provider asked CrazyAgent to wait before reading limits again.".into()
        }
        "usage_poll_failed" => {
            "The provider didn't return limits. CrazyAgent tries again in 10 minutes.".into()
        }
        "usage_unreachable" => {
            "CrazyAgent couldn't reach the provider. It tries again in 10 minutes.".into()
        }
        "usage_reply_invalid" => "The provider's answer wasn't one CrazyAgent can read.".into(),
        "usage_request_invalid" => {
            "CrazyAgent couldn't ask the provider for this account's limits.".into()
        }
        "native_profile_codex_api_key" => {
            "An API-key account has no 5-hour or weekly window.".into()
        }
        "native_profile_codex_token_unreadable" => {
            "CrazyAgent can't read this Codex sign-in. Sign in to Codex again in Accounts.".into()
        }
        "native_profile_codex_sign_in_expired" => "This Codex sign-in expired and CrazyAgent \
            couldn't renew it. Sign in to Codex again in Accounts."
            .into(),
        "native_profile_credential_unavailable" => {
            "CrazyAgent can't read this account's sign-in. Sign in again in Accounts.".into()
        }
        "native_profile_renewal_failed" => {
            "CrazyAgent couldn't renew this account's sign-in. It tries again in 10 minutes.".into()
        }
        "native_profile_renewal_busy" => "Another check of this account's sign-in is running. \
            CrazyAgent tries again in 10 minutes."
            .into(),
        "native_profile_sign_in_revoked" => {
            "The provider ended this sign-in. Sign in again in Accounts.".into()
        }
        other => format!("Couldn't read this account's limits ({other})."),
    }
}

/// One plain sentence for a usage step kind that panicked since the engine started: what
/// may be missing while it keeps failing, or when it worked again. Inputs: the step's
/// health, now and the local offset in seconds. Output: the sentence.
pub fn health_line(health: &StepHealth, now_ms: i64, offset: i64) -> String {
    let (what, missing) = match health.step {
        UsageStep::Poll => ("Checking account limits", "limits shown may be out of date"),
        UsageStep::Team => (
            "Counting agent tokens",
            "agent and research run token totals may be low",
        ),
        UsageStep::Runs => (
            "Counting research run tokens",
            "research token totals may be low",
        ),
        UsageStep::Recount => ("Recounting history", "the comparison may arrive later"),
    };
    let times = match health.panics {
        1 => "once".to_owned(),
        count => format!("{count} times"),
    };
    let said = format!(
        "{what} hit an internal error {times} since the engine started, last {}.",
        ago(health.last_ms, now_ms, offset)
    );
    match health.recovered_ms {
        None => format!("{said} CrazyAgent keeps retrying; {missing}."),
        Some(recovered) => format!("{said} It worked again {}.", ago(recovered, now_ms, offset)),
    }
}

/// The recount offer, shown until the owner chooses. Inputs: the comparison and whether
/// dollars are shown. Output: its lines.
fn recount_lines(status: &RecountStatus, priced: bool) -> Vec<Line<'static>> {
    let money = |text: String| {
        if priced {
            format!("  {text}")
        } else {
            String::new()
        }
    };
    let mut lines = vec![
        Line::raw("We recounted your history."),
        Line::raw(format!(
            "Old  {} tokens  {} cache read{}",
            compact(status.old.headline()),
            compact(status.old.cache_read),
            money("Not estimated".to_owned())
        )),
        Line::raw(format!(
            "New  {} tokens  {} cache read{}",
            compact(status.new.headline()),
            compact(status.new.cache_read),
            money(money_text(status.new_cost_nanos))
        )),
        Line::raw(format!(
            "{} sessions recounted, {} keep their old numbers because their transcripts are gone or could not be read to the end.",
            status.sessions_recounted, status.sessions_kept_old
        )),
        Line::raw("Recounted sessions count under the account their agent last used."),
    ];
    if status.homes_partial > 0 {
        lines.push(Line::raw(format!(
            "{} agent folders held more than one pass reads, and part of their history keeps its old numbers.",
            status.homes_partial
        )));
    }
    if priced {
        lines.push(Line::raw("New dollars cover recounted sessions only. Sessions that keep their old numbers have no estimate."));
    }
    lines.push(Line::raw(
        "Press n to use the new numbers, o to keep the old ones.",
    ));
    lines
}

/// The lines above the usage rows: how the numbers were counted and, until the owner chooses,
/// the recount offer, each wrapped to the screen. Inputs: the report and the width. Output:
/// the lines.
fn recount_head(report: &UsageReport, limit: usize) -> Vec<Line<'static>> {
    let priced = report.money.settings.prices != PriceSource::Off;
    report
        .counting_notice
        .iter()
        .map(|notice| Line::raw(notice.clone()))
        .chain(
            report
                .recount
                .iter()
                .flat_map(|status| recount_lines(status, priced)),
        )
        .flat_map(|line| wrap(&line.to_string(), limit.max(8)))
        .map(Line::raw)
        .collect()
}

/// Plain words for a usage refusal or failure code, shared by the usage and Pools screens.
/// Input: the code. Output: the sentence.
pub(in crate::tui) fn refusal(code: &str) -> &'static str {
    match code {
        "usage_unavailable" => "Usage isn't available in this engine yet.",
        "usage_spend_off" => "Spending free resets is off. Turn it on in Settings, Pools.",
        "usage_spend_unconfirmed" => {
            "Spend confirmation now asks first. Use a free reset again and confirm it."
        }
        "usage_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Press r to read again."
        }
        "usage_recount_not_ready" => "This recount was already decided. Press r to read again.",
        other => plain(other),
    }
}

/// Names shown in place of ids: projects, pools and accounts by id, each key, URL or local
/// account by id with what it has in place of a 5-hour or weekly window, and each account's
/// free resets and credits as stored, by id (DEC-154).
#[derive(Default)]
pub struct Names {
    pub projects: HashMap<String, String>,
    pub pools: HashMap<String, String>,
    pub accounts: HashMap<String, String>,
    pub no_window: Vec<(String, &'static str)>,
    pub extras: HashMap<String, LimitExtras>,
}

impl Names {
    fn of(ctx: &Ctx) -> Self {
        let mut names = Self::default();
        if let Some(projects) = ctx.store.projects.ready() {
            for card in projects {
                names
                    .projects
                    .insert(card.id.as_str().into(), card.name.clone());
            }
        }
        if let Some(pools) = ctx.store.pools.ready() {
            for pool in pools {
                names
                    .pools
                    .insert(pool.id.as_str().into(), pool.name.clone());
            }
        }
        if let Some(accounts) = ctx.store.accounts.ready() {
            names.accounts = crate::tui::seam::account_labels(accounts);
            names.no_window = accounts
                .iter()
                .filter_map(|row| row.no_window.map(|note| (row.id.as_str().to_owned(), note)))
                .collect();
        }
        names
    }
}

fn shown<'a>(names: &'a HashMap<String, String>, key: &'a str) -> &'a str {
    if key.is_empty() {
        return "Unattributed";
    }
    if key == crate::usage::REMOVED_PROJECTS {
        return "Removed projects";
    }
    names.get(key).map_or(key, String::as_str)
}

fn window_line(
    label: &str,
    window: Option<LimitWindow>,
    own: Option<i64>,
    failed: bool,
    left_out: Option<&str>,
    now_ms: i64,
) -> String {
    match (window, left_out) {
        (None, _) if failed => {
            format!("{label:<8}Not read. The last read failed, for the reason above.")
        }
        (None, Some(sentence)) => format!("{label:<8}{sentence}"),
        (None, None) => format!("{label:<8}Not reported"),
        (Some(window), _) => format!(
            "{label:<8}{:>3}% used  {}{}",
            window.used_percent.round() as u32,
            reset_text(window.resets_at_ms, now_ms),
            if own.is_some() && window.resets_at_ms == own {
                " (your time)"
            } else {
                ""
            }
        ),
    }
}

fn limits_lines(
    limits: &AccountLimits,
    names: &Names,
    now_ms: i64,
    offset: i64,
    limit: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut head = vec![Span::styled(
        shown(&names.accounts, limits.account.as_str()).to_owned(),
        theme.style(Role::Heading),
    )];
    if let Some(plan) = &limits.plan {
        head.push(Span::raw(format!("  {plan}")));
    }
    let read = match (limits.source, limits.observed_ms) {
        (Some(LimitSource::Poll), Some(at)) => {
            format!("  read from the provider {}", ago(at, now_ms, offset))
        }
        (Some(LimitSource::Transcript), Some(at)) => {
            format!("  read from a run's own record {}", ago(at, now_ms, offset))
        }
        _ => "  not read yet".into(),
    };
    head.push(Span::styled(read, theme.style(Role::Muted)));
    let mut lines = vec![Line::from(head)];
    if let Some(code) = &limits.failure {
        for part in wrap(&poll_failure(code), limit.saturating_sub(2).max(8)) {
            lines.push(styled(format!("  {part}"), Role::Warning, theme));
        }
    }
    let failed = limits
        .failure
        .as_deref()
        .is_some_and(|code| code != "native_profile_codex_api_key");
    lines.push(Line::raw(format!(
        "  {}",
        window_line(
            "5-hour",
            limits.five_hour_at(now_ms),
            limits.five_hour_reset_override_ms,
            failed,
            limits.five_hour_left_out_label.as_deref(),
            now_ms
        )
    )));
    lines.push(Line::raw(format!(
        "  {}",
        window_line(
            "Weekly",
            limits.weekly_at(now_ms),
            limits.weekly_reset_override_ms,
            failed,
            limits.weekly_left_out_label.as_deref(),
            now_ms
        )
    )));
    lines
}

/// An account's free resets and credits line under its limits, wrapped to the width, when the
/// extras read has the account. Inputs: the names with the extras, the account id, now, the
/// local offset in seconds, the content width and the theme. Output: the lines, none unread.
fn extras_lines(
    names: &Names,
    account: &str,
    now_ms: i64,
    offset: i64,
    limit: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(extras) = names.extras.get(account) else {
        return Vec::new();
    };
    wrap(
        &extras::extras_line(extras, now_ms, offset),
        limit.saturating_sub(4).max(8),
    )
    .into_iter()
    .map(|part| styled(format!("    {part}"), Role::Muted, theme))
    .collect()
}

fn group(
    title: &str,
    rows: &[UsageRow],
    name: impl Fn(&str) -> String,
    money: &MoneyView,
    limit: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::default(), styled(title, Role::Heading, theme)];
    let width = rows
        .iter()
        .take(ROWS_SHOWN)
        .map(|row| name(&row.key).chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    for row in rows.iter().take(ROWS_SHOWN) {
        let label: String = name(&row.key).chars().take(width).collect();
        let runs = if row.runs == 1 { "run" } else { "runs" };
        let counted = format!(
            "  {label:<width$}  {}  {} {runs}",
            figures_text(
                &format!("{:>6} tokens", compact(row.tokens.headline())),
                row.cost_nanos,
                money
            ),
            row.runs
        );
        let shown = match &row.not_reported_by {
            None => counted,
            Some(harness) if row.tokens.headline() == 0 => {
                format!("  {label:<width$}  usage not reported by {harness}")
            }
            Some(harness) => format!("{counted}  + usage not reported by {harness}"),
        };
        lines.extend(row_lines(&shown, limit));
    }
    lines.extend(and_more(rows.len(), theme));
    lines
}

/// One row wrapped under its heading. Inputs: the text and the content width. Output: the
/// lines, two columns in.
fn row_lines(text: &str, limit: usize) -> impl Iterator<Item = Line<'static>> {
    wrap(text.trim_start(), limit.saturating_sub(2).max(8))
        .into_iter()
        .map(|part| Line::raw(format!("  {part}")))
}

/// Says how many rows did not fit. Inputs: the row count and the theme. Output: the line, none
/// when all fit.
fn and_more(rows: usize, theme: &Theme) -> Option<Line<'static>> {
    (rows > ROWS_SHOWN).then(|| {
        styled(
            format!("  and {} more", rows - ROWS_SHOWN),
            Role::Muted,
            theme,
        )
    })
}

/// An activity's name as this screen shows it. Input: the stored name. Output: the words.
fn activity_name(activity: &str) -> String {
    if activity == "build_deploy" {
        return "Build/deploy".to_owned();
    }
    let mut letters = activity.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// The activity lines: one per activity up to `ROWS_SHOWN`, then the first-try rate over every
/// activity. Inputs: the rows, the content width and the theme. Output: the lines, none for no
/// rows.
fn activity_lines(rows: &[ActivityRow], limit: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows.iter().take(ROWS_SHOWN) {
        let word = if row.turns == 1 { "turn" } else { "turns" };
        let name = activity_name(&row.activity);
        lines.extend(row_lines(
            &format!("{name:<14} {} {word}", row.turns),
            limit,
        ));
    }
    lines.extend(and_more(rows.len(), theme));
    let edited: u64 = rows.iter().map(|row| row.edit_turns).sum();
    let first: u64 = rows.iter().map(|row| row.first_try_turns).sum();
    if let Some(percent) = (first * 100 + edited / 2).checked_div(edited) {
        let word = if edited == 1 { "turn" } else { "turns" };
        let rate = format!("{percent}% of {edited} editing {word} worked first try");
        lines.extend(
            wrap(&rate, limit.saturating_sub(2).max(8))
                .into_iter()
                .map(Line::raw),
        );
    }
    lines
}

/// The model lines: one per model up to `ROWS_SHOWN`. Inputs: the rows, the money view, the
/// content width and the theme. Output: the lines, none for no rows.
fn model_lines(
    rows: &[ModelRow],
    money: &MoneyView,
    limit: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows.iter().take(ROWS_SHOWN) {
        let name = if row.model.is_empty() {
            "Model not recorded"
        } else {
            row.model.as_str()
        };
        let tokens = format!("{} tokens", compact(row.tokens.headline()));
        let figures = figures_text(&tokens, row.cost_nanos, money);
        lines.extend(row_lines(&format!("{name}  {figures}"), limit));
    }
    lines.extend(and_more(rows.len(), theme));
    lines
}

/// A titled block of lines. Inputs: the title, its lines and the theme. Output: a blank line,
/// the heading and the lines, or nothing when there are none.
fn section(title: &str, body: Vec<Line<'static>>, theme: &Theme) -> Vec<Line<'static>> {
    if body.is_empty() {
        return body;
    }
    let mut lines = vec![Line::default(), styled(title, Role::Heading, theme)];
    lines.extend(body);
    lines
}

/// The plain line a reading shows when a scan left records out. Input: how many agent homes
/// held more records than one scan reads. Output: the line, or None when every home was read
/// whole.
pub fn partial_line(homes: u64) -> Option<String> {
    match homes {
        0 => None,
        1 => Some(
            "Some usage may be missing: 1 agent home holds more records than one scan reads, so its least recently changed records are not counted."
                .into(),
        ),
        homes => Some(format!(
            "Some usage may be missing: {homes} agent homes hold more records than one scan reads, so their least recently changed records are not counted."
        )),
    }
}

/// The Usage summary's lines: totals, each account's limits with reset countdowns, tokens by
/// project, pool, account and model, and turns by activity. Inputs: the report, its range, names by id, now,
/// the local offset in seconds, the content width and the theme. Output: the lines.
pub fn summary(
    report: &UsageReport,
    range: UsageRange,
    names: &Names,
    now_ms: i64,
    offset: i64,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let theme = &ctx.theme;
    let total = &report.total;
    let prompt = total.input + total.cache_read + total.cache_write;
    let share = (total.cache_read * 100 + prompt / 2).checked_div(prompt);
    let (added, removed) = report.lines.iter().fold((0, 0), |(added, removed), row| {
        (added + row.added, removed + row.removed)
    });
    let unreported = report.not_reported_by.join(", ");
    let mut lines = vec![row(
        limit,
        styled("Usage", Role::Accent, theme),
        styled(range.label(), Role::Muted, theme),
        ctx.glyphs,
    )];
    if total.headline() == 0 && !unreported.is_empty() {
        lines.push(Line::raw(format!(
            "Usage not reported by {unreported}  Lines +{} -{}",
            compact(added),
            compact(removed)
        )));
    } else {
        if report.money.as_of.is_some() {
            lines.push(Line::raw(money_text(report.money.cost_nanos)));
        }
        lines.push(Line::raw(format!(
            "Tokens {} ({} input, {} cache write, {} output)",
            compact(total.headline()),
            compact(total.input),
            compact(total.cache_write),
            compact(total.output)
        )));
        let read = compact(total.cache_read);
        lines.push(Line::raw(match share {
            Some(share) => {
                format!("Re-read from cache {read}: {share}% of everything read, not in Tokens")
            }
            None => format!("Re-read from cache {read}, not in Tokens"),
        }));
        lines.push(Line::raw(format!(
            "Runs {}  Lines +{} -{}",
            report.runs,
            compact(added),
            compact(removed)
        )));
        if !unreported.is_empty() {
            lines.push(styled(
                format!("Not in these totals: usage not reported by {unreported}."),
                Role::Muted,
                theme,
            ));
        }
    }
    if let Some(partial) = partial_line(report.partial_homes) {
        lines.push(styled(partial, Role::Warning, theme));
    }
    for health in &report.health {
        let role = match health.recovered_ms {
            None => Role::Warning,
            Some(_) => Role::Muted,
        };
        lines.push(styled(health_line(health, now_ms, offset), role, theme));
    }
    lines.push(Line::default());
    lines.push(styled("Account limits", Role::Heading, theme));
    let plain: Vec<&(String, &'static str)> = names
        .no_window
        .iter()
        .filter(|(account, _)| {
            !report
                .limits
                .iter()
                .any(|row| row.account.as_str() == account)
        })
        .collect();
    if report.limits.is_empty() && plain.is_empty() {
        lines.push(styled(
            "  No limits read yet. Signed-in Claude and ChatGPT accounts show here after their first check.",
            Role::Muted,
            theme,
        ));
    }
    for limits in &report.limits {
        lines.extend(indent(
            limits_lines(limits, names, now_ms, offset, limit, theme),
            2,
        ));
        lines.extend(extras_lines(
            names,
            limits.account.as_str(),
            now_ms,
            offset,
            limit,
            theme,
        ));
    }
    for (account, note) in plain {
        lines.push(styled(
            format!("  {}", shown(&names.accounts, account)),
            Role::Heading,
            theme,
        ));
        lines.push(styled(format!("    {note}"), Role::Muted, theme));
        lines.extend(extras_lines(names, account, now_ms, offset, limit, theme));
    }
    if total.headline() == 0 && report.lines.is_empty() && unreported.is_empty() {
        lines.push(Line::default());
        lines.push(styled(
            "No usage in this range. Runs show up a few seconds after an agent works.",
            Role::Muted,
            theme,
        ));
        return lines;
    }
    lines.extend(group(
        "By project",
        &report.projects,
        |key| shown(&names.projects, key).to_owned(),
        &report.money,
        limit,
        theme,
    ));
    lines.extend(group(
        "By pool",
        &report.pools,
        |key| shown(&names.pools, key).to_owned(),
        &report.money,
        limit,
        theme,
    ));
    lines.extend(group(
        "By account",
        &report.accounts,
        |key| shown(&names.accounts, key).to_owned(),
        &report.money,
        limit,
        theme,
    ));
    lines.extend(section(
        "By model",
        model_lines(&report.models, &report.money, limit, theme),
        theme,
    ));
    lines.extend(section(
        "By activity",
        activity_lines(&report.activity, limit, theme),
        theme,
    ));
    lines
}

/// Usage: a summary of the tokens agents used in a range and each account's limits.
/// Input: keys and the engine's usage report. Output: the summary page.
pub struct UsagePage {
    range: UsageRange,
    report: Option<(UsageRange, Box<UsageReport>)>,
    extras: HashMap<String, LimitExtras>,
    asked: Option<(UsageRange, Option<i64>)>,
    load: Load<()>,
    waiting: bool,
}

impl Default for UsagePage {
    fn default() -> Self {
        Self::new()
    }
}

impl UsagePage {
    /// The summary with nothing read yet; `shown` asks the engine for the report.
    pub fn new() -> Self {
        Self {
            range: UsageRange::Week,
            report: None,
            extras: HashMap::new(),
            asked: None,
            load: Load::Idle,
            waiting: false,
        }
    }

    /// Whether the main number can be changed: a report is shown and prices are on, as on the
    /// page. Output: the answer.
    fn offers_main_number(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(|(_, report)| report.money.settings.prices != PriceSource::Off)
    }

    /// Whether the page is offering the recount. Output: the answer.
    fn offers_recount(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(|(_, report)| report.recount.is_some())
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if matches!(self.load, Load::Idle | Load::Failed(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        let offset = local_offset();
        let since_ms = self.range.start(now_ms(), offset);
        self.asked = Some((self.range, since_ms));
        send(
            ctx,
            SettingsJob::Usage {
                since_ms,
                offset_minutes: i32::try_from(offset / 60).unwrap_or(0),
            },
        );
        send(ctx, SettingsJob::Extras { detail: false });
    }

    /// The page's lines. Inputs: the content width, now, the local offset in seconds and
    /// the context. Output: the lines.
    pub fn body(&self, limit: usize, now_ms: i64, offset: i64, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let title = styled("Usage", Role::Accent, theme);
        match (&self.load, &self.report) {
            (Load::NotReady, _) => vec![
                title,
                Line::default(),
                Line::raw("Usage isn't ready yet in this engine."),
            ],
            (Load::Failed(code), None) => vec![
                title,
                Line::default(),
                styled(
                    format!("Could not read usage. {}", refusal(code)),
                    Role::Error,
                    theme,
                ),
                Line::raw("Press r to try again."),
            ],
            (_, Some((range, report))) => {
                let mut shown = recount_head(report, limit);
                let mut names = Names::of(ctx);
                names.extras = self.extras.clone();
                shown.extend(summary(report, *range, &names, now_ms, offset, limit, ctx));
                shown
            }
            _ => vec![
                title,
                Line::default(),
                Line::from(vec![
                    Span::styled(ctx.spinner(), theme.running),
                    Span::raw(" Reading usage"),
                ]),
            ],
        }
    }

    /// Applies the engine's answer to a usage read. A report for another range than the
    /// last one asked is an earlier read's and is dropped, so totals always show under the
    /// range they cover and the newer read's answer, a failure too, still lands.
    /// Inputs: the answer and the context.
    pub fn apply(&mut self, result: &Result<Box<UsageReport>, String>, ctx: &mut Ctx) {
        if let Ok(report) = result
            && self
                .asked
                .is_none_or(|(_, since_ms)| since_ms != report.since_ms)
        {
            return;
        }
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(report) => {
                let range = self.asked.map_or(self.range, |(range, _)| range);
                self.report = Some((range, report.clone()));
                self.load = Load::Ready(());
            }
            Err(code) if code == "usage_unavailable" => self.load = Load::NotReady,
            Err(code) if self.report.is_none() => self.load = Load::Failed(code.clone()),
            Err(code) if asked => ctx.say(Message::error("Could not read usage.", refusal(code))),
            Err(_) => {}
        }
    }
}

impl Screen for UsagePage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Usage".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        let mut hints = vec![
            hint("t", "Range"),
            hint("r", "Read again"),
            hint("c", "Check limits now"),
        ];
        if self.offers_main_number() {
            hints.push(hint("m", "Dollars or tokens"));
        }
        hints.push(hint("p", "Prices on or off"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Char('t') => {
                self.range = self.range.next();
                self.waiting = false;
                self.reload(ctx);
            }
            Key::Char('r') => self.reload(ctx),
            Key::Char('c') => send(ctx, SettingsJob::RefreshUsage),
            Key::Char('m') if self.offers_main_number() => {
                if let Some((_, report)) = &self.report {
                    let mut settings = report.money.settings;
                    settings.main_number = match settings.main_number {
                        MainNumber::Dollars => MainNumber::Tokens,
                        MainNumber::Tokens => MainNumber::Dollars,
                    };
                    send(ctx, SettingsJob::SetMoney(settings));
                }
            }
            Key::Char('p') => {
                if let Some((_, report)) = &self.report {
                    let mut settings = report.money.settings;
                    settings.prices = match settings.prices {
                        PriceSource::Bundled => PriceSource::Off,
                        PriceSource::Off => PriceSource::Bundled,
                    };
                    send(ctx, SettingsJob::SetMoney(settings));
                }
            }
            Key::Char('n') if self.offers_recount() => send(ctx, SettingsJob::DecideRecount(true)),
            Key::Char('o') if self.offers_recount() => send(ctx, SettingsJob::DecideRecount(false)),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::Usage(result)) => self.apply(result, ctx),
            Some(SettingsEvent::Extras(Ok(rows))) => {
                self.extras = rows
                    .iter()
                    .map(|row| (row.account.as_str().to_owned(), row.extras.clone()))
                    .collect();
            }
            Some(SettingsEvent::UsageRefreshed(Ok(()))) => ctx.say(Message::success(
                "Checking limits now.",
                "Press r in a minute to read them.",
            )),
            Some(SettingsEvent::UsageRefreshed(Err(code))) => {
                ctx.say(Message::error("Could not check limits.", refusal(code)))
            }
            Some(SettingsEvent::RecountDecided(Ok(()))) => self.reload(ctx),
            Some(SettingsEvent::RecountDecided(Err(code))) => ctx.say(Message::error(
                "Could not apply that choice.",
                refusal(code),
            )),
            Some(SettingsEvent::MoneySet(Ok(()))) => self.reload(ctx),
            Some(SettingsEvent::MoneySet(Err(code))) => ctx.say(Message::error(
                "Could not change the setting.",
                refusal(code),
            )),
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let lines = indent(self.body(limit, now_ms(), local_offset(), ctx), 2);
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Usage)
    }
}

pub(in crate::tui) mod extras;

#[cfg(all(test, unix))]
mod tests;
