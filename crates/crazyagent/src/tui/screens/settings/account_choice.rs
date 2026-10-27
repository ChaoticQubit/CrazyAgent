use crate::pools::PoolSummary;
use crate::protocol::CredentialRef;
use crate::scheduling::{
    AccountPlan, Pin, RANK_MAX, RESERVE_PERCENT_MAX, Schedule, ScheduleChange, Strategy,
};
use crate::tui::client::settings::SettingsJob;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Modal, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Request;
use ratatui::text::Line;

const RESERVE_STEP: u8 = 5;
const NAME_WIDTH: usize = 28;

/// The account choice editor over one pool, shown as a modal on Settings, Pools: row 0 is
/// the app strategy, row 1 the pool's own strategy and each later row one member's rank,
/// reserve and pin. Each change goes to the engine at once.
pub struct ChoiceEditor {
    pub pool: CredentialRef,
    pub row: usize,
}

/// The next strategy in the picker. Takes the current one, the direction and whether the
/// picker offers "Default" (none). Returns the strategy to save.
pub fn cycle(current: Option<Strategy>, forward: bool, inherit: bool) -> Option<Strategy> {
    let mut choices: Vec<Option<Strategy>> = Strategy::ALL.into_iter().map(Some).collect();
    if inherit {
        choices.insert(0, None);
    }
    let count = choices.len();
    let at = choices
        .iter()
        .position(|choice| *choice == current)
        .unwrap_or(0);
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    assert!(next < count);
    choices[next]
}

/// Moves a rank one step: up from none is 1, down from 1 is none, capped at the maximum.
/// Takes the rank and the direction. Returns the new rank.
pub fn rank_step(rank: Option<u16>, up: bool) -> Option<u16> {
    match (rank, up) {
        (None, true) => Some(1),
        (None, false) | (Some(1), false) => None,
        (Some(rank), true) => Some(rank.saturating_add(1).min(RANK_MAX)),
        (Some(rank), false) => Some(rank - 1),
    }
}

/// Moves a weekly reserve by five percent within 0 to the maximum.
/// Takes the reserve and the direction. Returns the new reserve.
pub fn reserve_step(reserve: u8, up: bool) -> u8 {
    let next = if up {
        reserve
            .saturating_add(RESERVE_STEP)
            .min(RESERVE_PERCENT_MAX)
    } else {
        reserve.saturating_sub(RESERVE_STEP)
    };
    assert!(next <= RESERVE_PERCENT_MAX);
    next
}

/// The next pin: none, then use last, then never use, then none again.
pub fn next_pin(pin: Option<Pin>) -> Option<Pin> {
    match pin {
        None => Some(Pin::UseLast),
        Some(Pin::UseLast) => Some(Pin::NeverUse),
        Some(Pin::NeverUse) => None,
    }
}

/// Describes an account's plan in one line, such as "Rank 2, keeps 10% of its week".
/// Takes the plan. Returns the line.
pub fn plan_line(plan: &AccountPlan) -> String {
    if plan.pin == Some(Pin::NeverUse) {
        return "Never used".into();
    }
    let mut parts = vec![
        plan.rank
            .map_or_else(|| "No rank".to_owned(), |rank| format!("Rank {rank}")),
        if plan.reserve_percent == 0 {
            "used to 100%".to_owned()
        } else {
            format!("keeps {}% of its week", plan.reserve_percent)
        },
    ];
    if plan.pin == Some(Pin::UseLast) {
        parts.push("used last".into());
    }
    parts.join(", ")
}

fn pool_strategy(schedule: &Schedule, pool: &CredentialRef) -> Option<Strategy> {
    schedule
        .pools
        .iter()
        .find(|entry| &entry.id == pool)
        .map(|entry| entry.strategy)
}

fn clipped(text: &str, width: usize) -> String {
    let mut shown: String = text.chars().take(width).collect();
    while shown.chars().count() < width {
        shown.push(' ');
    }
    shown
}

impl ChoiceEditor {
    /// Opens the editor on a pool with the app strategy focused.
    pub fn new(pool: CredentialRef) -> Self {
        Self { pool, row: 0 }
    }

    /// Handles one key: arrows move and change, `<` and `>` move a reserve, `p` cycles a
    /// pin and Esc closes. Takes the key, the stored schedule, the pool and the context.
    /// Returns false when the editor closes; a change is queued for the engine.
    pub fn key(
        &mut self,
        key: &Key,
        schedule: &Schedule,
        pool: &PoolSummary,
        ctx: &mut Ctx,
    ) -> bool {
        let rows = 2 + pool.members.len();
        self.row = self.row.min(rows - 1);
        let change = match key {
            Key::Esc => return false,
            Key::Up => {
                self.row = self.row.saturating_sub(1);
                None
            }
            Key::Down => {
                self.row = (self.row + 1).min(rows - 1);
                None
            }
            Key::Left | Key::Right => self.shift(schedule, pool, matches!(key, Key::Right)),
            Key::Char('<') | Key::Char('>') => self.member(schedule, pool, |plan| AccountPlan {
                reserve_percent: reserve_step(plan.reserve_percent, matches!(key, Key::Char('>'))),
                ..plan
            }),
            Key::Char('p') => self.member(schedule, pool, |plan| AccountPlan {
                pin: next_pin(plan.pin),
                ..plan
            }),
            _ => None,
        };
        if let Some(change) = change {
            let generation = ctx.next_generation();
            ctx.request(Request::Settings(Box::new(SettingsJob::SetSchedule {
                change,
                generation,
            })));
        }
        true
    }

    fn shift(
        &self,
        schedule: &Schedule,
        pool: &PoolSummary,
        forward: bool,
    ) -> Option<ScheduleChange> {
        match self.row {
            0 => Some(ScheduleChange::AppStrategy {
                strategy: cycle(Some(schedule.strategy), forward, false)?,
            }),
            1 => Some(ScheduleChange::PoolStrategy {
                pool: pool.id.clone(),
                strategy: cycle(pool_strategy(schedule, &pool.id), forward, true),
            }),
            _ => self.member(schedule, pool, |plan| AccountPlan {
                rank: rank_step(plan.rank, forward),
                ..plan
            }),
        }
    }

    fn member(
        &self,
        schedule: &Schedule,
        pool: &PoolSummary,
        change: impl Fn(AccountPlan) -> AccountPlan,
    ) -> Option<ScheduleChange> {
        let member = pool.members.get(self.row.checked_sub(2)?)?;
        let plan = schedule.plan(&member.id);
        let next = change(plan);
        (next != plan).then(|| ScheduleChange::Account {
            connection: member.id.clone(),
            plan: next,
        })
    }

    /// The editor as a modal: one line per row with the focused row marked.
    /// Takes the schedule, the pool and the context. Returns the modal and its hints.
    pub fn modal(&self, schedule: &Schedule, pool: &PoolSummary, ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let mark = |row: usize| if row == self.row { "> " } else { "  " };
        let app = schedule.strategy;
        let own = pool_strategy(schedule, &pool.id);
        let mut body = vec![
            Line::raw(format!(
                "{}{}{}",
                mark(0),
                clipped("Every pool", NAME_WIDTH),
                app.label()
            )),
            Line::raw(format!(
                "{}{}{}",
                mark(1),
                clipped("This pool", NAME_WIDTH),
                own.map_or_else(
                    || format!("Default ({})", app.label()),
                    |own| own.label().to_owned()
                )
            )),
            Line::default(),
        ];
        for (index, member) in pool.members.iter().enumerate() {
            body.push(Line::raw(format!(
                "{}{}{}",
                mark(index + 2),
                clipped(
                    &format!("{} {}", member.provider, member.id.as_str()),
                    NAME_WIDTH
                ),
                plan_line(&schedule.plan(&member.id))
            )));
        }
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        (
            Modal {
                width: 76,
                title: format!("Account choice: {}", pool.name),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Order or rank"),
                hint("< >", "Reserve"),
                hint("p", "Pin"),
                hint("Esc", "Close"),
            ],
        )
    }
}
