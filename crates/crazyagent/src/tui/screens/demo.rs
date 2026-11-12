use super::project::plain_problem;
use super::settings_accounts::load_accounts;
use super::{Flow, Place, ProjectTab, Screen, open};
use crate::accounts::AccountStatus;
use crate::demo::{DemoView, NAME};
use crate::pools::PoolSummary;
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::OwnerEvent;
use crate::tui::client::demo::{DemoEvent, DemoQuery, DemoReply, OUTCOME_UNKNOWN};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{AccountRow, Arrival, PoolChange, Request, account_labels};
use crate::tui::text::{join, list_row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{buttons, checkbox, radio};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

/// The demo page's title and the Home entry's two actions: Try before a demo exists and
/// Open once it does. Every demo word here is the same on the desktop (DEC-153), which keeps
/// it in apps/desktop/src/screens/demo/words.ts; demo-words.test.mjs holds the two equal.
pub(in crate::tui) const ENTRY_TITLE: &str = "Try the demo";
pub(in crate::tui) const ENTRY_START: &str = "Try the demo";
pub(in crate::tui) const ENTRY_OPEN: &str = "Open the demo";

/// The cost note, shown first on the demo page, before any pool.
const COST_NOTE: &str = "The demo is a real run. Its master and agents spend the limits of the pool you pick, like any project, and any account in that pool may be used, since a pool moves to its next account when one runs out.";

/// What the demo makes and what it never does.
const ABOUT: &str = "CrazyAgent makes a project named CrazyAgent demo in your projects folder. Its agents work Sandboxed with no remote: nothing is pushed and nothing is downloaded.";

/// The pool list's lead line and the create button.
const PICK: &str = "Pick the pool the demo runs on. Nothing is picked for you.";
const CREATE: &str = "Create the demo";

/// The line shown when the owner has no pool yet, and its action.
const NO_POOL: &str =
    "You have no pool yet. Create one with an account that is ready for agents, then come back.";
const NO_POOL_ACTION: &str = "Create a pool";

/// The line shown in place of the pool list once the demo exists.
const EXISTS: &str = "Your demo project is ready. Open it to follow the guide.";

/// Why a pool cannot run the demo, by the engine's code, shown under that pool's row.
const POOL_REASONS: [(&str, &str); 3] = [
    (
        "demo_harness_unavailable",
        "Its accounts run only on rcli, which has no live terminal or team agents yet.",
    ),
    (
        "demo_pool_not_ready",
        "No account in this pool is ready for agents.",
    ),
    ("demo_pool_missing", "This pool is gone."),
];

/// A pool's reason for a code this app has no sentence for; the code follows it.
const POOL_UNKNOWN: &str = "The demo can't run on this pool.";

/// What a refused Create the demo says, by the engine's code.
const REFUSALS: [(&str, &str); 8] = [
    (
        "demo_exists",
        "Your demo project already exists. Open it, or delete it before making a new one.",
    ),
    (
        "demo_name_taken",
        "Your projects folder already holds a project or folder named CrazyAgent demo. Rename or move it, then try again.",
    ),
    (
        "demo_project_id_invalid",
        "This app sent a demo request the engine does not take. Update CrazyAgent, then try again.",
    ),
    ("demo_pool_missing", "That pool is gone. Pick another pool."),
    (
        "demo_pool_not_ready",
        "No account in that pool is ready for agents. Sign one in, or pick another pool.",
    ),
    (
        "demo_harness_unavailable",
        "That pool's accounts run only on rcli, which has no live terminal or team agents yet. Pick a pool with an account that runs Claude Code or Codex.",
    ),
    (
        "demo_platform_unqualified",
        "The demo runs only on macOS in this version.",
    ),
    (
        "demo_seed_failed",
        "CrazyAgent couldn't write the demo's files into your projects folder. Check that it can be written to, then try again.",
    ),
];

/// What Create the demo says when the engine's answer could not be confirmed.
const UNCONFIRMED: &str =
    "We couldn't confirm the demo was made. Check your projects before trying again.";

/// The line said when the demo was made but the picked pool could not be given to it, so its
/// master has no account until the owner chooses the pool again.
const POOL_UNASSIGNED: &str = "The demo was made, but the pool you picked wasn't given to it. Choose the pool again in the demo's Settings, Pool, before you send the goal.";

/// The guide's title and its goal block label.
const GUIDE_TITLE: &str = "Demo guide";
const GOAL_LABEL: &str = "The goal to send";

/// The guide's steps in order: an id, a title and two sentences. The first four tick from the
/// engine's demo read; the last opens Delete everything and never ticks.
const STEPS: [(&str, &str, &str); 5] = [
    (
        "goal",
        "Give the master its goal",
        "Open the master on Conversation and send it the goal below. You talk to the master, and it plans the work.",
    ),
    (
        "agents",
        "Watch the agents work",
        "The master hands tasks to team agents. Agents shows who works on what.",
    ),
    (
        "question",
        "Answer the question",
        "An agent asked you something and waits for your answer. Answer it on Questions.",
    ),
    (
        "changes",
        "Accept the change set",
        "The agents' work lands as one change set. Review it on Changes and accept it.",
    ),
    (
        "delete",
        "Delete the demo",
        "When you are done, open Remove and pick Delete everything. The demo's folder goes to the Trash.",
    ),
];

/// The TUI's own line for Enter before a pool is picked, since the cursor is not a pick.
const PICK_FIRST: &str = "Pick a pool first: press Space on a pool, then Enter.";

const POOLS_MAX: usize = 64;
const TEXT_WIDTH: usize = 96;
const INDENT: &str = "      ";

/// One pool as the demo page offers it: its id, its name, the words of its accounts that are
/// ready for agents, and why the demo cannot run on it, if it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct PoolRow {
    pub id: CredentialRef,
    pub name: String,
    pub ready: Vec<String>,
    pub reason: Option<String>,
}

/// One guide step: its id, title and body, and whether the engine's demo read ticks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct GuideStep {
    pub id: &'static str,
    pub title: &'static str,
    pub body: &'static str,
    pub done: bool,
}

/// The demo guide: every step in order and the index of the first step not done yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Guide {
    pub steps: Vec<GuideStep>,
    pub current: usize,
}

/// Says why a pool cannot run the demo in the demo's words. Input: the engine's code.
/// Output: the sentence, or the unknown-pool sentence with the plain code after it.
pub(in crate::tui) fn pool_reason(code: &str) -> String {
    POOL_REASONS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, sentence)| (*sentence).to_owned())
        .unwrap_or_else(|| format!("{POOL_UNKNOWN} ({})", crate::protocol::plain_code(code)))
}

/// Says why Create the demo was refused in the demo's words. Input: the engine's code.
/// Output: the sentence, or the project page's plain problem for any other code.
pub(in crate::tui) fn refusal(code: &str) -> String {
    REFUSALS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, sentence)| (*sentence).to_owned())
        .unwrap_or_else(|| plain_problem(code))
}

fn ready_for_agents(account: &AccountRow) -> bool {
    account.status == AccountStatus::SignedIn.label()
        || account.status == AccountStatus::KeySaved.label()
}

/// The demo page's pool rows, in the engine's order and at most 64: each pool's name (its id
/// when it has none), its accounts that are ready for agents by their account words, and the
/// engine's reason it cannot run the demo. Inputs: the demo read, the pools and the accounts.
/// Output: the rows.
pub(in crate::tui) fn pool_rows(
    view: &DemoView,
    pools: &[PoolSummary],
    accounts: &[AccountRow],
) -> Vec<PoolRow> {
    let labels = account_labels(accounts);
    let rows: Vec<PoolRow> = view
        .pools
        .iter()
        .take(POOLS_MAX)
        .map(|offer| {
            let summary = pools.iter().find(|pool| pool.id == offer.pool);
            let name = summary
                .map(|pool| pool.name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or(offer.pool.as_str())
                .to_owned();
            let ready = summary
                .map(|pool| {
                    pool.members
                        .iter()
                        .filter(|member| {
                            accounts
                                .iter()
                                .any(|account| account.id == member.id && ready_for_agents(account))
                        })
                        .filter_map(|member| labels.get(member.id.as_str()).cloned())
                        .collect()
                })
                .unwrap_or_default();
            PoolRow {
                id: offer.pool.clone(),
                name,
                ready,
                reason: offer.unavailable.as_deref().map(pool_reason),
            }
        })
        .collect();
    assert!(rows.len() <= POOLS_MAX);
    rows
}

/// The demo guide for one project: none unless the project is the demo. The first four steps
/// tick from the engine's read; Delete the demo never ticks. Inputs: the demo read and the
/// project. Output: the guide.
pub(in crate::tui) fn guide(view: &DemoView, project: &ProjectId) -> Option<Guide> {
    if view.project_id.as_ref() != Some(project) {
        return None;
    }
    let ticks = [
        view.steps.goal_sent,
        view.steps.agent_started,
        view.steps.question_answered,
        view.steps.change_set_accepted,
        false,
    ];
    let steps: Vec<GuideStep> = STEPS
        .iter()
        .zip(ticks)
        .map(|(&(id, title, body), done)| GuideStep {
            id,
            title,
            body,
            done,
        })
        .collect();
    let current = steps
        .iter()
        .position(|step| !step.done)
        .expect("Delete the demo never ticks");
    Some(Guide { steps, current })
}

/// The guide's lines for the demo project: the title with the step it stands at, every step
/// as a checkbox, the goal to send under the first step and the current step's sentences.
/// Inputs: the demo read, the project, the width and the context. Output: the lines, none
/// when the project is not the demo.
pub(in crate::tui) fn guide_lines(
    view: &DemoView,
    project: &ProjectId,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let Some(guide) = guide(view, project) else {
        return Vec::new();
    };
    let theme = &ctx.theme;
    let inner = limit.saturating_sub(INDENT.len()).max(12);
    let indented = |text: String, role: Option<Role>| {
        let line = match role {
            Some(role) => styled(text, role, theme),
            None => Line::raw(text),
        };
        join([Line::raw(INDENT), line])
    };
    let mut lines = vec![join([
        styled(GUIDE_TITLE, Role::Heading, theme),
        styled(
            format!("  Step {} of {}", guide.current + 1, guide.steps.len()),
            Role::Muted,
            theme,
        ),
    ])];
    for (index, step) in guide.steps.iter().enumerate() {
        let current = index == guide.current;
        let marker = if current { "> " } else { "  " };
        let text = format!("{marker}{}", checkbox(step.done, step.title));
        lines.push(match (current, step.done) {
            (true, _) => styled(text, Role::Accent, theme),
            (false, true) => styled(text, Role::Muted, theme),
            (false, false) => Line::raw(text),
        });
        if current {
            lines.extend(
                wrap(step.body, inner)
                    .into_iter()
                    .map(|text| indented(text, None)),
            );
        }
        if step.id == "goal" {
            lines.push(indented(format!("{GOAL_LABEL}:"), Some(Role::Muted)));
            lines.extend(
                wrap(&view.goal, inner)
                    .into_iter()
                    .map(|text| indented(text, None)),
            );
        }
    }
    lines
}

/// Where the Home entry goes: the demo page until a demo exists, then the demo project on
/// Conversation. Input: the demo read. Output: the entry's action words and its place.
pub(in crate::tui) fn entry(view: &DemoView) -> (&'static str, Place) {
    match &view.project_id {
        Some(id) => (
            ENTRY_OPEN,
            Place::Project {
                id: id.clone(),
                name: view.name.clone(),
                tab: ProjectTab::Conversation,
            },
        ),
        None => (ENTRY_START, Place::Demo),
    }
}

fn demo_project() -> ProjectId {
    crate::demo::demo_id().expect("the demo's fixed id is a valid project id")
}

enum Phase {
    Idle,
    Creating(CredentialRef),
    Checking(CredentialRef),
    Giving,
}

/// The demo page (DEC-153): the cost note first, then every pool with none picked; Space
/// picks an available pool and Enter makes the demo on it. Once the engine applies it, the
/// page gives the picked pool to the demo and opens it on Conversation. An answer that could
/// not be read is settled by reading the demo again. Once a demo exists, Enter opens it.
pub(in crate::tui) struct DemoPage {
    view: Option<Result<DemoView, String>>,
    cursor: usize,
    picked: Option<CredentialRef>,
    phase: Phase,
    problem: Option<String>,
}

impl DemoPage {
    /// A demo page with nothing read and nothing picked. Input: none. Output: the screen;
    /// it reads the demo, the pools and the accounts when shown.
    pub(in crate::tui) fn new() -> Self {
        Self {
            view: None,
            cursor: 0,
            picked: None,
            phase: Phase::Idle,
            problem: None,
        }
    }

    fn rows(&self, ctx: &Ctx) -> Vec<PoolRow> {
        let Some(Ok(view)) = &self.view else {
            return Vec::new();
        };
        let pools = ctx.store.pools.ready().map_or(&[][..], Vec::as_slice);
        let accounts = ctx.store.accounts.ready().map_or(&[][..], Vec::as_slice);
        pool_rows(view, pools, accounts)
    }

    fn read(ctx: &mut Ctx) {
        ctx.request(Request::Demo(Box::new(DemoQuery::Read)));
    }

    fn create(&mut self, rows: &[PoolRow], ctx: &mut Ctx) {
        let available =
            |id: &CredentialRef| rows.iter().any(|row| &row.id == id && row.reason.is_none());
        let Some(pool) = self.picked.clone().filter(available) else {
            if !rows.is_empty() {
                self.problem = Some(PICK_FIRST.into());
            }
            return;
        };
        self.problem = None;
        let generation = ctx.next_generation();
        self.phase = Phase::Creating(pool.clone());
        ctx.request(Request::Demo(Box::new(DemoQuery::Create {
            pool,
            generation,
        })));
    }

    fn give(&mut self, pool: CredentialRef, ctx: &mut Ctx) {
        self.phase = Phase::Giving;
        ctx.request(Request::Projects);
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool: Some(pool),
                project: demo_project(),
            },
        });
    }

    fn given(&mut self, result: &Result<(), String>, ctx: &mut Ctx) -> Flow {
        self.phase = Phase::Idle;
        match result {
            Ok(()) => {
                ctx.request(Request::PoolProjects);
                ctx.say(Message::success(&format!("{NAME} is ready."), ""));
            }
            Err(code) => ctx.say(Message::warning(POOL_UNASSIGNED, &plain_problem(code))),
        }
        let place = Place::Project {
            id: demo_project(),
            name: NAME.into(),
            tab: ProjectTab::Conversation,
        };
        Flow::Replace(open(&place, ctx))
    }

    fn keep_pick(&mut self) {
        let Some(Ok(view)) = &self.view else {
            return;
        };
        let still = |picked: &CredentialRef| {
            view.pools
                .iter()
                .any(|offer| &offer.pool == picked && offer.unavailable.is_none())
        };
        if !self.picked.as_ref().is_some_and(still) {
            self.picked = None;
        }
        self.cursor = self.cursor.min(view.pools.len().clamp(1, POOLS_MAX) - 1);
    }

    fn answered(&mut self, event: &DemoEvent, ctx: &mut Ctx) {
        match (&event.query, &event.result) {
            (DemoQuery::Read, Ok(DemoReply::View(view))) => {
                let made = view.project_id.is_some();
                self.view = Some(Ok((**view).clone()));
                self.keep_pick();
                match std::mem::replace(&mut self.phase, Phase::Idle) {
                    Phase::Checking(pool) if made => self.give(pool, ctx),
                    Phase::Checking(_) => self.problem = Some(UNCONFIRMED.into()),
                    other => self.phase = other,
                }
            }
            (DemoQuery::Read, Ok(DemoReply::Created)) => {}
            (DemoQuery::Read, Err(code)) => {
                if matches!(self.phase, Phase::Checking(_)) {
                    self.phase = Phase::Idle;
                    self.problem = Some(UNCONFIRMED.into());
                } else if !matches!(self.view, Some(Ok(_))) {
                    self.view = Some(Err(code.clone()));
                }
            }
            (DemoQuery::Create { pool, .. }, result) => {
                if !matches!(&self.phase, Phase::Creating(sent) if sent == pool) {
                    return;
                }
                match result {
                    Ok(DemoReply::Created) => self.give(pool.clone(), ctx),
                    Ok(DemoReply::View(_)) => {
                        self.phase = Phase::Checking(pool.clone());
                        Self::read(ctx);
                    }
                    Err(code) if code == OUTCOME_UNKNOWN => {
                        self.phase = Phase::Checking(pool.clone());
                        Self::read(ctx);
                    }
                    Err(code) => {
                        self.phase = Phase::Idle;
                        self.problem = Some(refusal(code));
                        Self::read(ctx);
                    }
                }
            }
        }
    }

    fn pool_lines(&self, rows: &[PoolRow], limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, usize) {
        let theme = &ctx.theme;
        let inner = limit.saturating_sub(INDENT.len()).max(12);
        let mut lines = vec![Line::raw(PICK), Line::default()];
        let mut focus = 0;
        for (index, row) in rows.iter().enumerate() {
            let selected = index == self.cursor;
            if selected {
                focus = lines.len();
            }
            let on = row.reason.is_none() && self.picked.as_ref() == Some(&row.id);
            let content = match row.reason {
                Some(_) => styled(radio(false, &row.name), Role::Muted, theme),
                None => Line::raw(radio(on, &row.name)),
            };
            lines.push(list_row(content, limit, selected, true, theme, ctx.glyphs));
            let detail = match &row.reason {
                Some(reason) => Some((reason.clone(), Role::Warning)),
                None if row.ready.is_empty() => None,
                None => Some((format!("Ready: {}", row.ready.join(", ")), Role::Muted)),
            };
            if let Some((text, role)) = detail {
                lines.extend(
                    wrap(&text, inner)
                        .into_iter()
                        .map(|text| join([Line::raw(INDENT), styled(text, role, theme)])),
                );
            }
        }
        (lines, focus)
    }
}

impl Screen for DemoPage {
    fn crumbs(&self) -> Vec<String> {
        vec![ENTRY_TITLE.into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if !matches!(self.phase, Phase::Idle) {
            return Vec::new();
        }
        match &self.view {
            Some(Ok(view)) if view.project_id.is_some() => {
                vec![hint("Enter", ENTRY_OPEN), hint("Esc", "Back")]
            }
            Some(Ok(_)) if !self.rows(ctx).is_empty() => vec![
                hint("Space", "Pick"),
                hint("Enter", CREATE),
                hint("p", NO_POOL_ACTION),
                hint("Esc", "Back"),
            ],
            Some(Ok(_)) => vec![hint("p", NO_POOL_ACTION), hint("Esc", "Back")],
            _ => vec![hint("Esc", "Back")],
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if !matches!(self.phase, Phase::Idle) {
            return Flow::Handled;
        }
        let Some(Ok(view)) = &self.view else {
            return Flow::Ignored;
        };
        if view.project_id.is_some() {
            return match key {
                Key::Enter => Flow::Replace(open(&entry(view).1, ctx)),
                _ => Flow::Ignored,
            };
        }
        let rows = self.rows(ctx);
        match key {
            Key::Char('p') => Flow::Open(Place::NewPool),
            Key::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Flow::Handled
            }
            Key::Down => {
                if self.cursor + 1 < rows.len() {
                    self.cursor += 1;
                }
                Flow::Handled
            }
            Key::Char(' ') => {
                if let Some(row) = rows.get(self.cursor)
                    && row.reason.is_none()
                {
                    self.picked = Some(row.id.clone());
                    self.problem = None;
                }
                Flow::Handled
            }
            Key::Enter => {
                self.create(&rows, ctx);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => {
                if let OwnerEvent::Demo(event) = event.as_ref() {
                    self.answered(event, ctx);
                }
                Flow::Handled
            }
            Arrival::PoolEdited(result) if matches!(self.phase, Phase::Giving) => {
                self.given(result, ctx)
            }
            _ => Flow::Handled,
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        Self::read(ctx);
        ctx.request(Request::Pools);
        load_accounts(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize)
            .saturating_sub(4)
            .clamp(24, TEXT_WIDTH);
        let mut lines = vec![styled(ENTRY_TITLE, Role::Heading, theme), Line::default()];
        lines.extend(
            wrap(COST_NOTE, limit)
                .into_iter()
                .map(|text| styled(text, Role::Warning, theme)),
        );
        lines.push(Line::default());
        lines.extend(wrap(ABOUT, limit).into_iter().map(Line::raw));
        lines.push(Line::default());
        let mut focus = 0;
        match &self.view {
            None => lines.push(ctx.loading("Reading the demo")),
            Some(Err(code)) => lines.extend(
                wrap(&plain_problem(code), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            ),
            Some(Ok(view)) => match &view.project_id {
                Some(id) => {
                    lines.extend(wrap(EXISTS, limit).into_iter().map(Line::raw));
                    lines.push(Line::default());
                    lines.extend(guide_lines(view, id, limit, ctx));
                    lines.push(Line::default());
                    lines.push(buttons(&[ENTRY_OPEN], Some(0), theme));
                }
                None => {
                    let rows = self.rows(ctx);
                    if rows.is_empty() {
                        lines.extend(wrap(NO_POOL, limit).into_iter().map(Line::raw));
                    } else {
                        let (pools, at) = self.pool_lines(&rows, limit, ctx);
                        focus = lines.len() + at;
                        lines.extend(pools);
                    }
                    lines.push(Line::default());
                    if let Some(problem) = &self.problem {
                        lines.extend(
                            wrap(problem, limit)
                                .into_iter()
                                .map(|text| styled(text, Role::Error, theme)),
                        );
                        lines.push(Line::default());
                    }
                    match self.phase {
                        Phase::Idle if rows.is_empty() => {}
                        Phase::Idle => lines.push(buttons(&[CREATE], Some(0), theme)),
                        Phase::Creating(_) => lines.push(ctx.loading("Creating the demo")),
                        Phase::Checking(_) => {
                            lines.push(ctx.loading("Checking whether the demo was made"))
                        }
                        Phase::Giving => lines.push(ctx.loading("Giving the demo its pool")),
                    }
                }
            },
        }
        let top = (focus + 2).saturating_sub(area.height as usize);
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .skip(top)
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.view.is_none() || !matches!(self.phase, Phase::Idle)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Demo)
    }
}

#[cfg(all(test, unix))]
#[path = "demo_tests.rs"]
mod tests;
