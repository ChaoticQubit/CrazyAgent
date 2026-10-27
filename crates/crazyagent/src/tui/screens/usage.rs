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
use crate::usage::report::{UsageReport, UsageRow};
use crate::usage::{AccountLimits, LimitSource, LimitWindow};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const ROWS_SHOWN: usize = 5;
const UNITS: [(f64, &str); 3] = [(1e3, "K"), (1e6, "M"), (1e9, "B")];

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
        other => format!("Couldn't read this account's limits ({other})."),
    }
}

fn refusal(code: &str) -> &'static str {
    match code {
        "usage_unavailable" => "Usage isn't available in this engine yet.",
        "usage_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Press r to read again."
        }
        other => plain(other),
    }
}

fn tokens(row: &crate::usage::capture::Tokens) -> u64 {
    row.input + row.output + row.cache_read + row.cache_write
}

/// Names shown in place of ids: projects, pools and accounts by id.
#[derive(Default)]
pub struct Names {
    pub projects: HashMap<String, String>,
    pub pools: HashMap<String, String>,
    pub accounts: HashMap<String, String>,
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
            for account in accounts {
                names
                    .accounts
                    .insert(account.id.as_str().into(), account.name.clone());
            }
        }
        names
    }
}

fn shown<'a>(names: &'a HashMap<String, String>, key: &'a str) -> &'a str {
    names.get(key).map_or(key, String::as_str)
}

fn window_line(label: &str, window: Option<LimitWindow>, own: bool, now_ms: i64) -> String {
    match window {
        None => format!("{label:<8}Not reported"),
        Some(window) => format!(
            "{label:<8}{:>3}% used  {}{}",
            window.used_percent.round() as u32,
            reset_text(window.resets_at_ms, now_ms),
            if own { " (your time)" } else { "" }
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
    lines.push(Line::raw(format!(
        "  {}",
        window_line(
            "5-hour",
            limits.five_hour_at(now_ms),
            limits.five_hour_reset_override_ms.is_some(),
            now_ms
        )
    )));
    lines.push(Line::raw(format!(
        "  {}",
        window_line(
            "Weekly",
            limits.weekly_at(now_ms),
            limits.weekly_reset_override_ms.is_some(),
            now_ms
        )
    )));
    if let Some(code) = &limits.failure {
        for part in wrap(&poll_failure(code), limit.saturating_sub(2).max(8)) {
            lines.push(styled(format!("  {part}"), Role::Warning, theme));
        }
    }
    lines
}

fn group(
    title: &str,
    rows: &[UsageRow],
    name: impl Fn(&str) -> String,
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
        lines.push(Line::raw(format!(
            "  {label:<width$}  {:>6} tokens  {} {runs}",
            compact(tokens(&row.tokens)),
            row.runs
        )));
    }
    if rows.len() > ROWS_SHOWN {
        lines.push(styled(
            format!("  and {} more", rows.len() - ROWS_SHOWN),
            Role::Muted,
            theme,
        ));
    }
    lines
}

/// The Usage summary's lines: totals, each account's limits with reset countdowns, and
/// tokens by project, pool and account. Inputs: the report, its range, names by id, now,
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
    let cache = match (total.cache_read * 100 + prompt / 2).checked_div(prompt) {
        Some(share) => format!("{share}%"),
        None => "none".into(),
    };
    let (added, removed) = report.lines.iter().fold((0, 0), |(added, removed), row| {
        (added + row.added, removed + row.removed)
    });
    let mut lines = vec![
        row(
            limit,
            styled("Usage", Role::Accent, theme),
            styled(range.label(), Role::Muted, theme),
            ctx.glyphs,
        ),
        Line::raw(format!(
            "Tokens {} ({} in, {} out)  Cache {cache}  Runs {}  Lines +{} -{}",
            compact(tokens(total)),
            compact(total.input),
            compact(total.output),
            report.runs,
            compact(added),
            compact(removed)
        )),
        Line::default(),
        styled("Account limits", Role::Heading, theme),
    ];
    if report.limits.is_empty() {
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
    }
    if tokens(total) == 0 && report.lines.is_empty() {
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
        theme,
    ));
    lines.extend(group(
        "By pool",
        &report.pools,
        |key| shown(&names.pools, key).to_owned(),
        theme,
    ));
    lines.extend(group(
        "By account",
        &report.accounts,
        |key| shown(&names.accounts, key).to_owned(),
        theme,
    ));
    lines
}

/// Usage: a summary of the tokens agents used in a range and each account's limits.
/// Input: keys and the engine's usage report. Output: the summary page.
pub struct UsagePage {
    range: UsageRange,
    report: Option<Box<UsageReport>>,
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
            load: Load::Idle,
            waiting: false,
        }
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
        send(
            ctx,
            SettingsJob::Usage {
                since_ms: self.range.start(now_ms(), offset),
                offset_minutes: i32::try_from(offset / 60).unwrap_or(0),
            },
        );
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
            (_, Some(report)) => summary(
                report,
                self.range,
                &Names::of(ctx),
                now_ms,
                offset,
                limit,
                ctx,
            ),
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

    /// Applies the engine's answer to a usage read. Inputs: the answer and the context.
    pub fn apply(&mut self, result: &Result<Box<UsageReport>, String>, ctx: &mut Ctx) {
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(report) => {
                self.report = Some(report.clone());
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
        vec![
            hint("t", "Range"),
            hint("r", "Read again"),
            hint("c", "Check limits now"),
            hint("Esc", "Back"),
        ]
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
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::Usage(result)) => self.apply(result, ctx),
            Some(SettingsEvent::UsageRefreshed(Ok(()))) => ctx.say(Message::success(
                "Checking limits now.",
                "Press r in a minute to read them.",
            )),
            Some(SettingsEvent::UsageRefreshed(Err(code))) => {
                ctx.say(Message::error("Could not check limits.", refusal(code)))
            }
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

#[cfg(all(test, unix))]
mod tests;
