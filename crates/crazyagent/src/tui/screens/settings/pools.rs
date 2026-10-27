use super::account_choice::ChoiceEditor;
use super::{draw_page, step};
use crate::config::{POOL_MEMBERS_MAX, plain_problem};
use crate::pools::{MemberStatus, PoolSummary};
use crate::protocol::CredentialRef;
use crate::scheduling::{PoolRanking, Schedule};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::settings_event;
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{Arrival, PoolChange, Request};
use crate::tui::text::{cells, join, list_row, restyle, row, styled, thumb, with_bar, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::buttons;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const ROWS_START: usize = 10;
const TABS_HEIGHT: usize = 2;
const HEAD_HEIGHT: usize = 4;
const MODAL_TEXT: usize = 50;

/// Settings, Pools: every pool with its size, readiness and the projects using
/// it.
/// Input: keys and pool arrivals. Output: the pool table or the empty state.
pub struct Pools {
    selected: usize,
    offset: usize,
    rows: usize,
    confirm: Option<Confirm>,
    deleting: Option<bool>,
    schedule: Option<Schedule>,
    ranking: Option<Result<Vec<PoolRanking>, String>>,
    choice: Option<ChoiceEditor>,
}

struct Confirm {
    id: CredentialRef,
    name: String,
    users: Option<String>,
    delete_focused: bool,
}

impl Default for Pools {
    fn default() -> Self {
        Self::new()
    }
}

impl Pools {
    /// The pool list with the first pool focused.
    /// Input: none. Output: the screen.
    pub fn new() -> Self {
        Self {
            selected: 0,
            offset: 0,
            rows: ROWS_START,
            confirm: None,
            deleting: None,
            schedule: None,
            ranking: None,
            choice: None,
        }
    }

    fn chosen_pool<'a>(&self, ctx: &'a Ctx) -> Option<&'a PoolSummary> {
        let editor = self.choice.as_ref()?;
        ctx.store
            .pools
            .ready()?
            .iter()
            .find(|pool| pool.id == editor.pool)
    }

    fn choice_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let (Some(schedule), Some(pool)) = (self.schedule.clone(), self.chosen_pool(ctx).cloned())
        else {
            self.choice = None;
            return Flow::Handled;
        };
        let Some(editor) = self.choice.as_mut() else {
            return Flow::Ignored;
        };
        if !editor.key(key, &schedule, &pool, ctx) {
            self.choice = None;
        }
        Flow::Handled
    }

    fn open_choice(&mut self, ctx: &mut Ctx) {
        let Some(pool) = ctx
            .store
            .pools
            .ready()
            .and_then(|pools| pools.get(self.selected))
        else {
            return;
        };
        if self.schedule.is_none() {
            ctx.request(Request::Settings(Box::new(SettingsJob::Schedule)));
            ctx.say(Message::error(
                "Account choice is still loading.",
                "Try again in a moment.",
            ));
            return;
        }
        self.choice = Some(ChoiceEditor::new(pool.id.clone()));
    }

    fn page(&self) -> usize {
        self.rows.saturating_sub(1).max(1)
    }

    fn follow(&mut self, count: usize) {
        self.selected = self.selected.min(count.saturating_sub(1));
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + self.rows {
            self.offset = self.selected + 1 - self.rows;
        }
        self.offset = self.offset.min(count.saturating_sub(self.rows));
        assert!(
            count == 0 || (self.offset <= self.selected && self.selected < self.offset + self.rows)
        );
    }

    fn confirm_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(confirm) = &mut self.confirm else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.confirm = None,
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                confirm.delete_focused = !confirm.delete_focused;
            }
            Key::Enter if confirm.delete_focused => {
                let id = confirm.id.clone();
                self.deleting = Some(confirm.users.is_some());
                self.confirm = None;
                ctx.request(Request::EditPool {
                    change: PoolChange::Remove { id },
                });
            }
            Key::Enter => self.confirm = None,
            _ => {}
        }
        Flow::Handled
    }

    fn ask_delete(&mut self, ctx: &mut Ctx) {
        let Some(pool) = ctx
            .store
            .pools
            .ready()
            .and_then(|pools| pools.get(self.selected))
        else {
            return;
        };
        let users = used_by(pool, true, ctx);
        self.confirm = Some(Confirm {
            id: pool.id.clone(),
            name: pool.name.clone(),
            users: (users != "Not used").then_some(users),
            delete_focused: false,
        });
    }

    fn widths(wide: bool) -> [usize; 4] {
        if wide {
            [18, 10, 32, 40]
        } else {
            [13, 9, 30, 14]
        }
    }
}

/// The readiness cell of one pool: "6 ready, 1 not signed in".
/// Inputs: the pool and context. Output: styled counts, or "No accounts".
pub fn readiness(pool: &PoolSummary, ctx: &Ctx) -> Line<'static> {
    let ready = pool
        .members
        .iter()
        .filter(|member| matches!(member.status, MemberStatus::SignedIn | MemberStatus::SetUp))
        .count();
    let waiting = pool.members.len() - ready;
    if pool.members.is_empty() {
        return styled("No accounts", Role::Muted, &ctx.theme);
    }
    let mut spans = vec![Span::styled(format!("{ready} ready"), ctx.theme.success)];
    if waiting > 0 {
        spans.push(Span::raw(", "));
        spans.push(Span::styled(
            format!("{waiting} not signed in"),
            ctx.theme.warning,
        ));
    }
    Line::from(spans)
}

/// Who uses a pool: project names when they fit, else a count.
/// Inputs: the pool, whether names are wanted and the context. Output: the
/// plain text, "Not used" when no project uses it.
pub fn used_by(pool: &PoolSummary, names: bool, ctx: &Ctx) -> String {
    let projects: Vec<String> = ctx
        .store
        .projects
        .ready()
        .map(|cards| {
            cards
                .iter()
                .filter(|card| card.pool.as_deref() == Some(pool.name.as_str()))
                .map(|card| card.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let count = pool.project_count.max(projects.len());
    match count {
        0 => "Not used".into(),
        _ if names && projects.len() == count => projects.join(", "),
        1 => "1 project".into(),
        _ => format!("{count} projects"),
    }
}

impl Screen for Pools {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Pools".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match ctx.store.pools.ready() {
            Some(pools) if pools.is_empty() => {
                vec![
                    hint("Enter", "New pool"),
                    sections_hint(ctx.glyphs),
                    hint("Esc", "Home"),
                ]
            }
            _ => vec![
                hint("a", "New pool"),
                hint("Enter", "Edit"),
                hint("o", "Order"),
                hint("d", "Delete"),
                sections_hint(ctx.glyphs),
                hint("Esc", "Home"),
            ],
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.confirm.is_some() {
            return self.confirm_key(key, ctx);
        }
        if self.choice.is_some() {
            return self.choice_key(key, ctx);
        }
        let count = ctx.store.pools.ready().map_or(0, Vec::len);
        match key {
            Key::Char('a') => Flow::Open(Place::NewPool),
            Key::Char('o') if count > 0 => {
                self.open_choice(ctx);
                Flow::Handled
            }
            Key::Char('d') if count > 0 && self.deleting.is_none() => {
                self.ask_delete(ctx);
                Flow::Handled
            }
            Key::Enter => match ctx
                .store
                .pools
                .ready()
                .and_then(|pools| pools.get(self.selected))
            {
                Some(pool) => Flow::Open(Place::EditPool(pool.id.clone())),
                None => Flow::Open(Place::NewPool),
            },
            Key::Char('r') if matches!(ctx.store.pools, Load::Failed(_)) => {
                ctx.request(Request::Pools);
                Flow::Handled
            }
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.follow(count);
                Flow::Handled
            }
            Key::Down if count > 0 => {
                self.selected = (self.selected + 1).min(count - 1);
                self.follow(count);
                Flow::Handled
            }
            Key::PageUp => {
                self.selected = self.selected.saturating_sub(self.page());
                self.follow(count);
                Flow::Handled
            }
            Key::PageDown if count > 0 => {
                self.selected = (self.selected + self.page()).min(count - 1);
                self.follow(count);
                Flow::Handled
            }
            Key::Home => {
                self.selected = 0;
                self.follow(count);
                Flow::Handled
            }
            Key::End if count > 0 => {
                self.selected = count - 1;
                self.follow(count);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Pools, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::Schedule(Ok(schedule))) => self.schedule = Some(schedule.clone()),
            Some(SettingsEvent::Schedule(Err(code))) if self.choice.is_some() => {
                ctx.say(Message::error(
                    plain_problem(code).unwrap_or("Could not read the account choice."),
                    "",
                ));
            }
            Some(SettingsEvent::ScheduleSet(result)) => {
                if let Err(code) = result {
                    ctx.say(Message::error(
                        plain_problem(code).unwrap_or("Could not save the account choice."),
                        "",
                    ));
                }
                ctx.request(Request::Settings(Box::new(SettingsJob::Schedule)));
                ctx.request(Request::Settings(Box::new(SettingsJob::Ranking)));
            }
            Some(SettingsEvent::Ranking(result)) => self.ranking = Some(result.clone()),
            _ => {}
        }
        match arrival {
            Arrival::Pools(Ok(pools)) => self.follow(pools.len()),
            Arrival::PoolEdited(result) if self.deleting.is_some() => {
                let used = self.deleting.take() == Some(true);
                match result {
                    Ok(()) if used => ctx.say(Message::success(
                        "Pool deleted.",
                        "Its projects have no pool now. Pick a pool to start in each project's Settings.",
                    )),
                    Ok(()) => ctx.say(Message::success("Pool deleted.", "")),
                    Err(code) => ctx.say(Message::error(
                        plain_problem(code).unwrap_or("Could not delete the pool."),
                        "",
                    )),
                }
                ctx.request(Request::Pools);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let (Some(editor), Some(schedule), Some(pool)) =
            (&self.choice, &self.schedule, self.chosen_pool(ctx))
        {
            return Some(editor.modal(schedule, pool, self.ranking.as_ref(), ctx));
        }
        let confirm = self.confirm.as_ref()?;
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let mut body = vec![
            Line::raw(format!("Delete {}?", confirm.name)),
            Line::default(),
        ];
        let action = match &confirm.users {
            Some(users) => {
                body.extend(
                    wrap(
                        &format!(
                            "Used by {users}. They will have no pool until you pick one. Its accounts stay saved."
                        ),
                        MODAL_TEXT,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                "Unassign and delete"
            }
            None => {
                body.push(Line::raw("No project uses it. Its accounts stay saved."));
                "Delete"
            }
        };
        body.push(Line::default());
        body.push(buttons(
            &["Cancel", action],
            Some(usize::from(confirm.delete_focused)),
            &ctx.theme,
        ));
        Some((
            Modal {
                width: 54,
                title: "Delete pool".into(),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Confirm"),
                hint("Esc", "Cancel"),
            ],
        ))
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::Projects);
        ctx.request(Request::Settings(Box::new(SettingsJob::Schedule)));
        ctx.request(Request::Settings(Box::new(SettingsJob::Ranking)));
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = area.width as usize - 4;
        let content = match &ctx.store.pools {
            Load::Ready(pools) if pools.is_empty() => {
                let mut content = vec![
                    styled("Pools", Role::Accent, theme),
                    Line::default(),
                    Line::raw("No pools yet."),
                ];
                content.extend(
                    wrap(
                        "A pool groups up to 15 accounts for your projects to use. Each project uses one pool, and its agents share those accounts.",
                        limit,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                content.push(Line::default());
                content.push(buttons(&["New pool"], Some(0), theme));
                content
            }
            Load::Ready(pools) => {
                let wide = ctx.wide();
                let widths = Self::widths(wide);
                self.rows = (area.height as usize)
                    .saturating_sub(TABS_HEIGHT + HEAD_HEIGHT)
                    .max(1);
                self.follow(pools.len());
                let windowed = pools.len() > self.rows;
                let width = if windowed { limit - 1 } else { limit };
                let mut content = vec![
                    row(
                        limit,
                        styled("Pools", Role::Accent, theme),
                        styled(
                            format!(
                                "{} pool{}",
                                pools.len(),
                                if pools.len() == 1 { "" } else { "s" }
                            ),
                            Role::Muted,
                            theme,
                        ),
                        ctx.glyphs,
                    ),
                    Line::raw("A pool is a group of accounts. Each project uses one pool."),
                    Line::default(),
                    join([
                        Line::raw("  "),
                        restyle(
                            cells(
                                &widths,
                                ["Name", "Accounts", "Ready", "Used by"]
                                    .map(Line::raw)
                                    .to_vec(),
                                2,
                                ctx.glyphs,
                            ),
                            theme.muted,
                        ),
                    ]),
                ];
                let mut shown = Vec::new();
                for (index, pool) in pools.iter().enumerate().skip(self.offset).take(self.rows) {
                    shown.push(list_row(
                        cells(
                            &widths,
                            vec![
                                Line::raw(pool.name.clone()),
                                Line::raw(format!("{} of {POOL_MEMBERS_MAX}", pool.members.len())),
                                readiness(pool, ctx),
                                Line::raw(used_by(pool, wide, ctx)),
                            ],
                            2,
                            ctx.glyphs,
                        ),
                        width,
                        index == self.selected,
                        true,
                        theme,
                        ctx.glyphs,
                    ));
                }
                assert!(shown.len() <= self.rows);
                if windowed {
                    let (top, size) = thumb(shown.len(), self.offset, pools.len());
                    shown = with_bar(shown, limit, top, size, theme, ctx.glyphs);
                }
                assert_eq!(content.len(), HEAD_HEIGHT);
                content.extend(shown);
                content
            }
            Load::Failed(_) => vec![
                styled("Pools", Role::Accent, theme),
                Line::default(),
                crate::tui::text::status("Failed to load pools.", theme, ctx.glyphs),
                Line::raw("Press r to try again."),
            ],
            _ => vec![
                styled("Pools", Role::Accent, theme),
                Line::default(),
                ctx.loading("Loading pools"),
            ],
        };
        draw_page(frame, area, SettingsSection::Pools, content, None, ctx)
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        self.deleting.is_some() || matches!(ctx.store.pools, Load::Loading | Load::Idle)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Pools))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pools::{MemberKind, PoolMember};
    use crate::tui::glyphs::UNICODE;
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn pool(name: &str, projects: usize) -> PoolSummary {
        PoolSummary {
            id: CredentialRef::new(format!("pool-{name}")).unwrap(),
            name: name.into(),
            summary: String::new(),
            members: vec![PoolMember {
                id: CredentialRef::new("member-1").unwrap(),
                kind: MemberKind::Chatgpt,
                provider: "ChatGPT".into(),
                status: MemberStatus::SignedIn,
                status_label: "Signed in".into(),
            }],
            project_count: projects,
            provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
        }
    }

    fn ctx() -> Ctx {
        let mut ctx = Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        );
        ctx.store.pools = Load::Ready(vec![pool("main", 2), pool("spare", 0)]);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx
    }

    fn card(name: &str, pool: &str) -> crate::tui::seam::ProjectCard {
        crate::tui::seam::ProjectCard {
            id: crate::protocol::ProjectId::new(name.to_lowercase()).unwrap(),
            name: name.into(),
            about: String::new(),
            platforms: Vec::new(),
            pool: Some(pool.into()),
            research: Default::default(),
            created_ms: None,
            activity: Default::default(),
        }
    }

    fn body(modal: &Modal) -> String {
        modal
            .body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_used_pool_names_its_projects_and_unassigns_them_when_deleted() {
        let mut ctx = ctx();
        ctx.store.projects = Load::Ready(vec![card("Alpha", "main"), card("Beta", "main")]);
        let mut screen = Pools::new();
        screen.key(&Key::Char('d'), &mut ctx);
        let (modal, _) = screen.modal(&ctx).expect("confirmation shows");
        let shown = body(&modal).replace('\n', " ");
        assert!(shown.contains("Used by Alpha, Beta."), "{shown}");
        assert!(
            shown.contains("They will have no pool until you pick one."),
            "{shown}"
        );
        assert!(shown.contains("Unassign and delete"), "{shown}");
        screen.key(&Key::Right, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        match ctx.next_request() {
            Some(Request::EditPool {
                change: PoolChange::Remove { id },
            }) => assert_eq!(id.as_str(), "pool-main"),
            other => panic!("unexpected request {other:?}"),
        }
        screen.arrive(&Arrival::PoolEdited(Ok(())), &mut ctx);
        let message = ctx.message().unwrap();
        assert_eq!(message.lead, "Pool deleted.");
        assert!(
            message.rest.contains("Pick a pool to start"),
            "{}",
            message.rest
        );
        assert!(screen.deleting.is_none());
    }

    #[test]
    fn deleting_an_unused_pool_asks_then_sends_remove() {
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Char('d'), &mut ctx);
        let (modal, _) = screen.modal(&ctx).expect("confirmation shows");
        assert_eq!(modal.title, "Delete pool");
        screen.key(&Key::Enter, &mut ctx);
        assert!(screen.modal(&ctx).is_none(), "Cancel is focused first");
        assert_eq!(ctx.queued(), 0);

        screen.key(&Key::Char('d'), &mut ctx);
        screen.key(&Key::Right, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        match ctx.next_request() {
            Some(Request::EditPool {
                change: PoolChange::Remove { id },
            }) => assert_eq!(id.as_str(), "pool-spare"),
            other => panic!("unexpected request {other:?}"),
        }
        assert!(screen.busy(&ctx));
        screen.key(&Key::Char('d'), &mut ctx);
        assert!(
            screen.modal(&ctx).is_none(),
            "no second delete while one runs"
        );

        screen.arrive(&Arrival::PoolEdited(Err("pool_not_found".into())), &mut ctx);
        assert_eq!(
            ctx.message().unwrap().lead,
            "This pool no longer exists. Refresh and try again."
        );
        assert!(matches!(ctx.next_request(), Some(Request::Pools)));
        assert!(screen.deleting.is_none());
    }

    fn settings(event: SettingsEvent) -> Arrival {
        Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Settings(
            Box::new(event),
        )))
    }

    fn change(ctx: &mut Ctx) -> crate::scheduling::ScheduleChange {
        match ctx.next_request() {
            Some(Request::Settings(job)) => match *job {
                SettingsJob::SetSchedule { change, generation } => {
                    assert!(generation > 0);
                    change
                }
                other => panic!("unexpected settings job {other:?}"),
            },
            other => panic!("unexpected request {other:?}"),
        }
    }

    #[test]
    fn account_choice_sets_the_app_and_pool_strategy_and_each_members_rank_reserve_and_pin() {
        use crate::scheduling::{AccountPlan, Pin, ScheduleChange, Strategy};
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.key(&Key::Char('o'), &mut ctx);
        assert!(
            screen.modal(&ctx).is_none(),
            "no editor before the schedule arrives"
        );
        assert!(matches!(
            ctx.next_request(),
            Some(Request::Settings(job)) if matches!(*job, SettingsJob::Schedule)
        ));
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(Schedule::default()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        let (modal, _) = screen.modal(&ctx).expect("the editor shows");
        assert_eq!(modal.title, "Account choice: main");
        let shown = body(&modal);
        assert!(shown.contains("> Every pool"), "{shown}");
        assert!(shown.contains("Default (Use it or lose it)"), "{shown}");
        assert!(shown.contains("ChatGPT member-1"), "{shown}");
        assert!(shown.contains("No rank, used to 100%"), "{shown}");
        screen.key(&Key::Right, &mut ctx);
        assert_eq!(
            change(&mut ctx),
            ScheduleChange::AppStrategy {
                strategy: Strategy::ManualRank
            }
        );
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Left, &mut ctx);
        assert_eq!(
            change(&mut ctx),
            ScheduleChange::PoolStrategy {
                pool: CredentialRef::new("pool-main").unwrap(),
                strategy: Some(Strategy::Balanced),
            }
        );
        screen.key(&Key::Down, &mut ctx);
        let member = CredentialRef::new("member-1").unwrap();
        for (key, plan) in [
            (
                Key::Right,
                AccountPlan {
                    rank: Some(1),
                    ..AccountPlan::default()
                },
            ),
            (
                Key::Char('>'),
                AccountPlan {
                    reserve_percent: 5,
                    ..AccountPlan::default()
                },
            ),
            (
                Key::Char('p'),
                AccountPlan {
                    pin: Some(Pin::UseLast),
                    ..AccountPlan::default()
                },
            ),
        ] {
            screen.key(&key, &mut ctx);
            assert_eq!(
                change(&mut ctx),
                ScheduleChange::Account {
                    connection: member.clone(),
                    plan
                }
            );
        }
        screen.key(&Key::Char('<'), &mut ctx);
        assert_eq!(ctx.queued(), 0, "a reserve already at 0 sends nothing");
        screen.arrive(
            &settings(SettingsEvent::ScheduleSet(Err(
                "account_reserve_invalid".into()
            ))),
            &mut ctx,
        );
        assert!(ctx.message().is_some());
        assert!(matches!(
            ctx.next_request(),
            Some(Request::Settings(job)) if matches!(*job, SettingsJob::Schedule)
        ));
        screen.key(&Key::Esc, &mut ctx);
        assert!(screen.modal(&ctx).is_none());
    }

    #[test]
    fn account_choice_steps_stay_in_range() {
        use super::super::account_choice::{cycle, next_pin, plan_line, rank_step, reserve_step};
        use crate::scheduling::{AccountPlan, Pin, Strategy};
        assert_eq!(rank_step(None, false), None);
        assert_eq!(rank_step(Some(1), false), None);
        assert_eq!(rank_step(Some(999), true), Some(999));
        assert_eq!(reserve_step(95, true), 99);
        assert_eq!(reserve_step(99, true), 99);
        assert_eq!(reserve_step(3, false), 0);
        assert_eq!(next_pin(Some(Pin::NeverUse)), None);
        assert_eq!(
            cycle(Some(Strategy::Balanced), true, false),
            Some(Strategy::UseItOrLoseIt)
        );
        assert_eq!(cycle(Some(Strategy::Balanced), true, true), None);
        assert_eq!(
            plan_line(&AccountPlan {
                rank: Some(2),
                reserve_percent: 10,
                pin: Some(Pin::UseLast)
            }),
            "Rank 2, keeps 10% of its week, used last"
        );
        assert_eq!(
            plan_line(&AccountPlan {
                pin: Some(Pin::NeverUse),
                ..AccountPlan::default()
            }),
            "Never used"
        );
    }

    #[test]
    fn account_choice_lists_the_next_accounts_in_order_with_why_each_waits() {
        use crate::scheduling::{PoolRanking, RankedAccount, Standing, Strategy};
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.shown(&mut ctx);
        let mut asked = Vec::new();
        while let Some(request) = ctx.next_request() {
            if let Request::Settings(job) = request {
                asked.push(format!("{job:?}"));
            }
        }
        assert!(asked.iter().any(|job| job.contains("Ranking")), "{asked:?}");
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(Schedule::default()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        let shown = body(&screen.modal(&ctx).expect("the editor shows").0);
        assert!(shown.contains("Next accounts\n  Loading..."), "{shown}");
        screen.arrive(
            &settings(SettingsEvent::Ranking(Err("owner_read_failed".into()))),
            &mut ctx,
        );
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(
            shown.contains("Could not read the next accounts."),
            "{shown}"
        );
        let ranked = |id: &str, standing| RankedAccount {
            connection: CredentialRef::new(id).unwrap(),
            standing,
        };
        screen.arrive(
            &settings(SettingsEvent::Ranking(Ok(vec![PoolRanking {
                pool: CredentialRef::new("pool-main").unwrap(),
                strategy: Strategy::UseItOrLoseIt,
                accounts: vec![
                    ranked(
                        "member-1",
                        Standing::Next {
                            weekly_resets_ms: None,
                            weekly_left_percent: Some(40),
                            use_last: false,
                        },
                    ),
                    ranked("member-9", Standing::NeverUse),
                ],
            }]))),
            &mut ctx,
        );
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(shown.contains(" 1. ChatGPT member-1"), "{shown}");
        assert!(shown.contains("40% left"), "{shown}");
        assert!(shown.contains("    member-9"), "{shown}");
        assert!(shown.contains("never use"), "{shown}");
        screen.arrive(&settings(SettingsEvent::ScheduleSet(Ok(()))), &mut ctx);
        let mut asked = Vec::new();
        while let Some(request) = ctx.next_request() {
            if let Request::Settings(job) = request {
                asked.push(format!("{job:?}"));
            }
        }
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].contains("Ranking"), "{asked:?}");
    }

    #[test]
    fn a_standing_reads_as_one_short_reason_in_local_time() {
        use super::super::account_choice::{at_text, standing_line};
        use crate::scheduling::Standing;
        const HOUR: i64 = 3_600_000;
        assert_eq!(at_text(14 * HOUR + 1_800_000, 0, 0), "14:30");
        assert_eq!(at_text(4 * 24 * HOUR + 9 * HOUR, 0, 0), "Mon 09:00");
        assert_eq!(at_text(23 * HOUR + 1_800_000, 0, 3600), "Fri 00:30");
        let line = |standing| standing_line(&standing, 0, 0);
        assert_eq!(
            line(Standing::Next {
                weekly_resets_ms: Some(4 * 24 * HOUR + 9 * HOUR),
                weekly_left_percent: Some(40),
                use_last: false,
            }),
            "resets Mon 09:00, 40% left"
        );
        assert_eq!(
            line(Standing::Next {
                weekly_resets_ms: None,
                weekly_left_percent: None,
                use_last: true,
            }),
            "no usage read yet, used last"
        );
        assert_eq!(
            line(Standing::ReserveReached {
                reserve_percent: 20,
                until_ms: Some(HOUR),
            }),
            "reserve reached (20%)"
        );
        assert_eq!(
            line(Standing::ReserveReached {
                reserve_percent: 0,
                until_ms: Some(14 * HOUR + 1_800_000),
            }),
            "weekly limit used up until 14:30"
        );
        assert_eq!(
            line(Standing::FiveHourCap {
                until_ms: Some(14 * HOUR + 1_800_000),
            }),
            "5-hour cap until 14:30"
        );
        assert_eq!(
            line(Standing::FiveHourCap { until_ms: None }),
            "5-hour cap reached"
        );
        assert_eq!(line(Standing::NeverUse), "never use");
        assert_eq!(
            line(Standing::Paused { until_ms: 2 * HOUR }),
            "paused until 02:00"
        );
    }
}
