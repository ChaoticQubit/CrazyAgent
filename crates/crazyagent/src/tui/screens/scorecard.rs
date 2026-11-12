use super::new_project_view::printable;
use super::settings_accounts::{plain, send};
use crate::engine::owner::ScorecardSaved;
use crate::scorecard::{
    self, Baseline, DAY_MS, DailyQuestion, Flaw, Kind, Scorecard, Settings, Span as Days, Tally,
    Window,
};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::ago;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::text::{styled, wrap};
use crate::tui::theme::{Role, Theme};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

const ROWS_SHOWN: usize = 5;
const TITLE: &str = "Go/no-go scorecard";

/// The settings one key changes: `w` switches the window between the last 14 days and 14
/// days from today, `-` and `+` move a start day by one day, `b` and `d` step to the next
/// baseline and daily question option. Inputs: the settings, the key and today,
/// `YYYY-MM-DD`. Output: the settings to save, or None when the key changes nothing.
pub fn changed(settings: &Settings, key: char, today: &str) -> Option<Settings> {
    let mut next = settings.clone();
    match key {
        'w' => {
            next.window = match settings.window {
                Window::Rolling => Window::FromDay(today.to_owned()),
                Window::FromDay(_) => Window::Rolling,
            }
        }
        '-' | '+' => {
            let Window::FromDay(day) = &settings.window else {
                return None;
            };
            let step = if key == '-' { -1 } else { 1 };
            let moved = scorecard::day_number(day)? + step;
            next.window = Window::FromDay(scorecard::day_label(moved * DAY_MS, 0));
        }
        'b' => next.baseline = following(&Baseline::ALL, settings.baseline),
        'd' => next.daily_question = following(&DailyQuestion::ALL, settings.daily_question),
        _ => return None,
    }
    assert!(next != *settings);
    Some(next)
}

fn following<T: Copy + PartialEq>(all: &[T], current: T) -> T {
    let at = all
        .iter()
        .position(|option| *option == current)
        .expect("every stored option is listed");
    all[(at + 1) % all.len()]
}

fn refusal(code: &str) -> &'static str {
    match code {
        "scorecard_unavailable" => "The scorecard isn't available in this engine yet.",
        "scorecard_settings_invalid" => {
            "That start day isn't a calendar day. Pick a day like 2026-11-03."
        }
        "scorecard_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Press r to read again."
        }
        other => plain(other),
    }
}

fn per_task(tally: &Tally) -> Option<f64> {
    (tally.tasks > 0).then(|| f64::from(tally.acts()) / f64::from(tally.tasks))
}

/// An average per task in words: whole, one decimal, or no tasks yet. Input: the average.
/// Output: the text.
pub fn per_task_text(value: Option<f64>) -> String {
    match value {
        None => "no tasks yet".into(),
        Some(value) if value.fract() == 0.0 => format!("{value:.0}"),
        Some(value) => format!("{value:.1}"),
    }
}

/// How much less coordination per task the measured span took than the baseline, in whole
/// percent, negative when it took more. Inputs: both tallies. Output: the percent, or None
/// when either span closed no real task or the baseline counted no act.
pub fn cut(baseline: &Tally, measured: &Tally) -> Option<i64> {
    let (baseline, measured) = (per_task(baseline)?, per_task(measured)?);
    if baseline == 0.0 {
        return None;
    }
    let percent = ((1.0 - measured / baseline) * 100.0).round();
    assert!(percent.is_finite());
    assert!(percent <= 100.0);
    Some(percent as i64)
}

fn cut_text(cut: i64) -> String {
    match cut {
        0 => "the same".into(),
        less if less > 0 => format!("{less}% less"),
        more => format!("{}% more", -more),
    }
}

fn days_text(span: Days, offset_minutes: i32) -> String {
    assert!(span.end_ms > span.start_ms);
    let first = scorecard::day_label(span.start_ms, offset_minutes);
    let last = scorecard::day_label(span.end_ms - DAY_MS, offset_minutes);
    format!("{first} to {last}")
}

fn flaw_text(flaw: Flaw) -> &'static str {
    match flaw {
        Flaw::Discarded => "A change set was discarded",
        Flaw::OwnerEdited => "You edited it before accepting",
        Flaw::FirstBatchNotPassed => "Its first check batch didn't pass",
    }
}

fn act_text(kind: Kind) -> &'static str {
    match kind {
        Kind::OwnerMessage => "Message",
        Kind::Answer => "Answer",
        Kind::OwnerEdit => "Edit",
        Kind::OwnerStop => "Stop",
        Kind::OwnerInterrupt => "Interrupt",
        Kind::OwnerRestart => "Restart",
        Kind::Discard => "Discard",
        Kind::Accept => "Accept",
        Kind::Daily => "Daily answer",
    }
}

/// One of the four numbers as the desktop's tile reads it: its label and value, its target,
/// and whether the target is met. Inputs: the label with its value, the target, whether it
/// is met and the theme. Output: the line.
fn score(label: String, target: String, met: bool, theme: &Theme) -> Line<'static> {
    let (mark, role) = if met {
        ("Target met", Role::Success)
    } else {
        ("Not met yet", Role::Warning)
    };
    Line::from(vec![
        Span::raw(format!("{label}  {target}  ")),
        Span::styled(mark, theme.style(role)),
    ])
}

fn scores(card: &Scorecard, theme: &Theme) -> Vec<Line<'static>> {
    let (targets, met, coordination) = (&card.targets, &card.met, &card.coordination);
    let measured = per_task_text(per_task(&coordination.measured));
    let baseline = per_task_text(per_task(&coordination.baseline));
    let against = cut(&coordination.baseline, &coordination.measured)
        .map_or(String::new(), |cut| format!(", {}", cut_text(cut)));
    let asked = card.settings.daily_question == DailyQuestion::On || card.answered_days > 0;
    let chosen = if asked {
        format!("Target {} of 10 answered days", targets.chosen_of_ten)
    } else {
        "The daily question is off".into()
    };
    let lines = vec![
        score(
            format!("Real tasks {}", card.real),
            format!("Target {} done tickets you accepted", targets.tasks),
            met.tasks,
            theme,
        ),
        score(
            format!("Clean {} of {}", card.clean, card.real),
            format!(
                "Target {} of 10, from {} tasks",
                targets.clean_of_ten, targets.tasks
            ),
            met.clean,
            theme,
        ),
        score(
            format!("Coordination per task {measured}"),
            format!(
                "Baseline {baseline}{against}. Target {}% less",
                targets.coordination_cut_percent
            ),
            met.coordination,
            theme,
        ),
        score(
            format!(
                "Chose CrazyAgent {} of {} days",
                card.chosen_days, card.answered_days
            ),
            chosen,
            met.chosen,
            theme,
        ),
    ];
    assert_eq!(lines.len(), 4);
    lines
}

fn settings_lines(settings: &Settings, theme: &Theme) -> Vec<Line<'static>> {
    let window = match &settings.window {
        Window::Rolling => "Last 14 days (w)".to_owned(),
        Window::FromDay(day) => format!("14 days from a start day, {day} (w, - and + move it)"),
    };
    let baseline = match settings.baseline {
        Baseline::FirstDays => "Its first 3 days (b)",
        Baseline::PreviousWindow => "The 14 days before (b)",
    };
    let daily = match settings.daily_question {
        DailyQuestion::Off => "Off (d)",
        DailyQuestion::On => "On (d)",
    };
    vec![
        styled("Settings", Role::Heading, theme),
        Line::raw(format!("  Window: {window}")),
        Line::raw(format!("  Baseline: {baseline}")),
        Line::raw(format!(
            "  Ask me once a day whether I chose CrazyAgent for my work: {daily}"
        )),
    ]
}

fn task_lines(
    card: &Scorecard,
    limit: usize,
    offset_minutes: i32,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![styled(
        "Real tasks behind the numbers",
        Role::Heading,
        theme,
    )];
    if card.tasks.is_empty() {
        lines.extend(
            wrap(
                "No real tasks in this window yet. A ticket counts once it is done and you accepted its change set.",
                limit.saturating_sub(2).max(1),
            )
            .into_iter()
            .map(|part| styled(format!("  {part}"), Role::Muted, theme)),
        );
    }
    for task in card.tasks.iter().take(ROWS_SHOWN) {
        let clean = if task.clean {
            "Yes".to_owned()
        } else {
            task.flaws
                .iter()
                .map(|flaw| flaw_text(*flaw))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let closed = scorecard::day_label(task.closed_ms, offset_minutes);
        let sets = task.change_sets.join(", ");
        lines.push(Line::raw(printable(format!(
            "  {} {}  closed {closed}  {sets}  {clean}",
            task.ticket, task.title
        ))));
    }
    let listed = card.tasks.len().min(ROWS_SHOWN);
    if usize::try_from(card.real).is_ok_and(|real| real > listed) {
        lines.push(styled(
            format!("  Showing the newest {listed} of {}.", card.real),
            Role::Muted,
            theme,
        ));
    }
    lines
}

fn coordination_lines(card: &Scorecard, offset_minutes: i32, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![styled("Coordination", Role::Heading, theme)];
    let spans = [
        ("Baseline", card.baseline, &card.coordination.baseline),
        ("Measured", card.measured, &card.coordination.measured),
    ];
    for (name, span, tally) in spans {
        lines.push(Line::raw(format!(
            "  {name} {}: {} messages, {} answers, {} interventions, {} real tasks, {} per task",
            days_text(span, offset_minutes),
            tally.messages,
            tally.answers,
            tally.interventions,
            tally.tasks,
            per_task_text(per_task(tally))
        )));
    }
    lines
}

fn record_lines(
    card: &Scorecard,
    projects: &HashMap<String, String>,
    now_ms: i64,
    offset: i64,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !card.acts.is_empty() {
        lines.push(styled("Your newest acts", Role::Heading, theme));
    }
    for act in card.acts.iter().take(ROWS_SHOWN) {
        let project = projects.get(&act.project).unwrap_or(&act.project);
        lines.push(Line::raw(printable(format!(
            "  {}  {project}  {}  {}",
            act_text(act.kind),
            act.subject,
            ago(act.at_ms, now_ms, offset)
        ))));
    }
    if !card.days.is_empty() {
        lines.push(styled("Daily answers", Role::Heading, theme));
    }
    for day in card.days.iter().take(ROWS_SHOWN) {
        let answer = if day.yes { "Yes" } else { "No" };
        lines.push(Line::raw(format!("  {} {answer}", day.day)));
    }
    lines
}

/// One scorecard's lines: the window, the four numbers with their targets, the settings and
/// the newest tasks, coordination, acts and daily answers behind them. Inputs: the scorecard,
/// the width, project names, now, the local offset in seconds and the theme. Output: the
/// lines.
pub fn card_lines(
    card: &Scorecard,
    limit: usize,
    projects: &HashMap<String, String>,
    now_ms: i64,
    offset: i64,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let offset_minutes = minutes(offset);
    let window = days_text(card.window, offset_minutes);
    let mut lines = vec![styled(TITLE, Role::Accent, theme)];
    for text in wrap(
        &format!(
            "Whether CrazyAgent earns its place over {window}: real tasks, clean ones, how much you coordinated per task and the days you chose it."
        ),
        limit,
    ) {
        lines.push(styled(text, Role::Muted, theme));
    }
    lines.extend(scores(card, theme));
    lines.push(Line::default());
    lines.extend(settings_lines(&card.settings, theme));
    lines.push(Line::default());
    lines.extend(task_lines(card, limit, offset_minutes, theme));
    lines.extend(coordination_lines(card, offset_minutes, theme));
    lines.extend(record_lines(card, projects, now_ms, offset, theme));
    lines
}

fn minutes(offset_seconds: i64) -> i32 {
    i32::try_from(offset_seconds / 60).unwrap_or(0)
}

/// The go/no-go scorecard inside Usage: reads it in local days, saves one setting per key by
/// version, and reads again after every save, a stale one included.
/// Input: keys and the engine's answers. Output: the scorecard's lines.
pub struct ScorecardPanel {
    card: Option<Box<Scorecard>>,
    load: Load<()>,
    waiting: bool,
    saving: bool,
}

impl Default for ScorecardPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ScorecardPanel {
    /// The panel with nothing read yet; `reload` asks the engine.
    pub fn new() -> Self {
        Self {
            card: None,
            load: Load::Idle,
            waiting: false,
            saving: false,
        }
    }

    /// Asks the engine for the scorecard unless a read is already awaited. Inputs: the
    /// local offset in seconds and the context.
    pub fn reload(&mut self, offset: i64, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if matches!(self.load, Load::Idle | Load::Failed(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        let offset_minutes = minutes(offset);
        send(ctx, SettingsJob::Scorecard { offset_minutes });
    }

    /// Handles one key: `r` reads again, a settings key saves the changed settings. Inputs:
    /// the key, today `YYYY-MM-DD`, the local offset in seconds and the context. Output:
    /// whether the key was the panel's.
    pub fn key(&mut self, key: &Key, today: &str, offset: i64, ctx: &mut Ctx) -> bool {
        let Key::Char(pressed) = key else {
            return false;
        };
        if *pressed == 'r' {
            self.reload(offset, ctx);
            return true;
        }
        if !matches!(pressed, 'w' | '-' | '+' | 'b' | 'd') {
            return false;
        }
        let Some(card) = &self.card else {
            return true;
        };
        if self.saving {
            ctx.say(Message::error(
                "Still saving.",
                "Wait for the last change to land.",
            ));
            return true;
        }
        if let Some(settings) = changed(&card.settings, *pressed, today) {
            self.saving = true;
            let version = card.version;
            send(ctx, SettingsJob::ScorecardSet { settings, version });
        }
        true
    }

    /// Applies one engine answer for the scorecard. Inputs: the event, the local offset in
    /// seconds and the context.
    pub fn arrive(&mut self, event: &SettingsEvent, offset: i64, ctx: &mut Ctx) {
        match event {
            SettingsEvent::Scorecard(result) => self.apply(result, ctx),
            SettingsEvent::ScorecardSaved(result) => {
                self.saving = false;
                match result {
                    Ok(ScorecardSaved::Saved(_)) => ctx.say(Message::success(
                        "Scorecard settings saved.",
                        "The scorecard reads them now.",
                    )),
                    Ok(ScorecardSaved::Stale(_)) => ctx.say(Message::error(
                        "These settings changed elsewhere first.",
                        "The saved ones show now; choose again.",
                    )),
                    Err(code) => ctx.say(Message::error(
                        "Could not save the scorecard settings.",
                        refusal(code),
                    )),
                }
                self.reload(offset, ctx);
            }
            _ => {}
        }
    }

    /// Applies the engine's answer to a scorecard read. Inputs: the answer and the context.
    pub fn apply(&mut self, result: &Result<Box<Scorecard>, String>, ctx: &mut Ctx) {
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(card) => {
                self.card = Some(card.clone());
                self.load = Load::Ready(());
            }
            Err(code) if code == "scorecard_unavailable" => self.load = Load::NotReady,
            Err(code) if self.card.is_none() => self.load = Load::Failed(code.clone()),
            Err(code) if asked => ctx.say(Message::error(
                "Could not read the scorecard.",
                refusal(code),
            )),
            Err(_) => {}
        }
    }

    /// The panel's lines. Inputs: the width, project names, now, the local offset in
    /// seconds and the context. Output: the lines.
    pub fn lines(
        &self,
        limit: usize,
        projects: &HashMap<String, String>,
        now_ms: i64,
        offset: i64,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let title = styled(TITLE, Role::Accent, theme);
        match (&self.load, &self.card) {
            (Load::NotReady, _) => vec![
                title,
                Line::default(),
                Line::raw("The scorecard isn't ready yet in this engine."),
            ],
            (Load::Failed(code), None) => vec![
                title,
                Line::default(),
                styled(
                    format!("Could not read the scorecard. {}", refusal(code)),
                    Role::Error,
                    theme,
                ),
                Line::raw("Press r to try again."),
            ],
            (_, Some(card)) => card_lines(card, limit, projects, now_ms, offset, theme),
            _ => vec![
                title,
                Line::default(),
                Line::from(vec![
                    Span::styled(ctx.spinner(), theme.running),
                    Span::raw(" Reading the scorecard"),
                ]),
            ],
        }
    }

    /// The panel's key hints. Output: the hints.
    pub fn hints() -> Vec<Hint> {
        vec![
            hint("w", "Window"),
            hint("-/+", "Start day"),
            hint("b", "Baseline"),
            hint("d", "Daily question"),
            hint("r", "Read again"),
            hint("g", "Usage"),
            hint("Esc", "Back"),
        ]
    }

    /// Whether a read or a save is awaited. Output: true while one is.
    pub fn busy(&self) -> bool {
        self.waiting || self.saving
    }
}

#[cfg(all(test, unix))]
#[path = "scorecard_tests.rs"]
mod tests;
