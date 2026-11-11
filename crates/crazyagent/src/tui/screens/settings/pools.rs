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
    saving: Vec<u64>,
    ranking: Option<Result<Vec<PoolRanking>, String>>,
    choice: Option<ChoiceEditor>,
}

struct Confirm {
    id: CredentialRef,
    name: String,
    users: Option<String>,
    projects: usize,
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
            saving: Vec::new(),
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

    /// Handles a key in the account choice editor: Esc closes it, and a change the key makes
    /// is applied to the cached schedule at once and sent to the engine under a generation
    /// the screen keeps until its answer, so the next quick key builds on it instead of on
    /// the schedule last read. Takes the key and the context. Returns whether it was handled.
    fn choice_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let pool = self.chosen_pool(ctx).cloned();
        let (Some(schedule), Some(pool)) = (self.schedule.as_mut(), pool) else {
            self.choice = None;
            return Flow::Handled;
        };
        let Some(editor) = self.choice.as_mut() else {
            return Flow::Ignored;
        };
        if *key == Key::Esc {
            self.choice = None;
            return Flow::Handled;
        }
        if let Some(change) = editor.key(key, schedule, &pool) {
            let version = schedule.version;
            schedule.update(&change);
            let generation = ctx.next_generation();
            self.saving.push(generation);
            ctx.request(Request::Settings(Box::new(SettingsJob::SetSchedule {
                change,
                version,
                generation,
            })));
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
                let change = PoolChange::Remove {
                    id: confirm.id.clone(),
                    opened_projects: confirm.projects,
                };
                self.deleting = Some(confirm.users.is_some());
                self.confirm = None;
                ctx.request(Request::EditPool { change });
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
            projects: pool.project_count,
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
                .filter(|card| card.pool_id.as_ref() == Some(&pool.id))
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
            Some(SettingsEvent::Schedule(Ok(schedule))) if self.saving.is_empty() => {
                self.schedule = Some(schedule.clone());
            }
            Some(SettingsEvent::Schedule(Err(code))) if self.choice.is_some() => {
                ctx.say(Message::error(
                    plain_problem(code).unwrap_or("Could not read the account choice."),
                    "",
                ));
            }
            Some(SettingsEvent::ScheduleSet(generation, result)) => {
                if let Some(at) = self.saving.iter().position(|sent| sent == generation) {
                    self.saving.swap_remove(at);
                    if let Err(code) = result {
                        ctx.say(Message::error(
                            plain_problem(code).unwrap_or("Could not save the account choice."),
                            "",
                        ));
                    }
                }
                assert!(!self.saving.contains(generation));
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
            pool_id: Some(CredentialRef::new(format!("pool-{pool}")).unwrap()),
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
                change: PoolChange::Remove { id, .. },
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
                change: PoolChange::Remove { id, .. },
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

    fn sent(ctx: &mut Ctx) -> (crate::scheduling::ScheduleChange, u64) {
        match ctx.next_request() {
            Some(Request::Settings(job)) => match *job {
                SettingsJob::SetSchedule {
                    change, generation, ..
                } => {
                    assert!(generation > 0);
                    (change, generation)
                }
                other => panic!("unexpected settings job {other:?}"),
            },
            other => panic!("unexpected request {other:?}"),
        }
    }

    fn change(ctx: &mut Ctx) -> crate::scheduling::ScheduleChange {
        sent(ctx).0
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
        let mut last = 0;
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
                    rank: Some(1),
                    reserve_percent: 5,
                    pin: None,
                },
            ),
            (
                Key::Char('p'),
                AccountPlan {
                    rank: Some(1),
                    reserve_percent: 5,
                    pin: Some(Pin::UseLast),
                },
            ),
            (
                Key::Char('<'),
                AccountPlan {
                    rank: Some(1),
                    reserve_percent: 0,
                    pin: Some(Pin::UseLast),
                },
            ),
        ] {
            screen.key(&key, &mut ctx);
            let (change, generation) = sent(&mut ctx);
            assert_eq!(
                change,
                ScheduleChange::Account {
                    connection: member.clone(),
                    plan
                }
            );
            last = generation;
        }
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(shown.contains("Rank 1, used to 100%, used last"), "{shown}");
        screen.key(&Key::Char('<'), &mut ctx);
        assert_eq!(ctx.queued(), 0, "a reserve already at 0 sends nothing");
        let lead = |ctx: &Ctx| ctx.message().map(|message| message.lead.clone());
        ctx.say(Message::success("Saved.", ""));
        let before = lead(&ctx);
        assert_eq!(before.as_deref(), Some("Saved."));
        screen.arrive(
            &settings(SettingsEvent::ScheduleSet(
                ctx.next_generation(),
                Err("account_reserve_invalid".into()),
            )),
            &mut ctx,
        );
        assert_eq!(
            lead(&ctx),
            before,
            "another screen's failed save is not this screen's"
        );
        while ctx.next_request().is_some() {}
        screen.arrive(
            &settings(SettingsEvent::ScheduleSet(
                last,
                Err("account_reserve_invalid".into()),
            )),
            &mut ctx,
        );
        assert_ne!(lead(&ctx), before);
        assert!(ctx.message().is_some());
        assert!(matches!(
            ctx.next_request(),
            Some(Request::Settings(job)) if matches!(*job, SettingsJob::Schedule)
        ));
        screen.key(&Key::Esc, &mut ctx);
        assert!(screen.modal(&ctx).is_none());
    }

    #[test]
    fn quick_choice_edits_keep_each_other_until_every_save_is_read_back() {
        use crate::scheduling::{AccountEntry, AccountPlan, Pin, ScheduleChange};
        let mut ctx = ctx();
        let mut screen = Pools::new();
        let member = CredentialRef::new("member-1").unwrap();
        let stored = |plan: AccountPlan| Schedule {
            accounts: vec![AccountEntry {
                connection: member.clone(),
                plan,
            }],
            ..Schedule::default()
        };
        let use_last = AccountPlan {
            pin: Some(Pin::UseLast),
            ..AccountPlan::default()
        };
        let never = |reserve_percent| AccountPlan {
            reserve_percent,
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        };
        let account = |plan| ScheduleChange::Account {
            connection: member.clone(),
            plan,
        };
        let drain = |ctx: &mut Ctx| while ctx.next_request().is_some() {};
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored(use_last)))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        drain(&mut ctx);

        let answered = |generation| settings(SettingsEvent::ScheduleSet(generation, Ok(())));
        screen.key(&Key::Char('p'), &mut ctx);
        let (pin, first) = sent(&mut ctx);
        assert_eq!(pin, account(never(0)));
        screen.key(&Key::Char('>'), &mut ctx);
        let (reserve, second) = sent(&mut ctx);
        assert_eq!(
            reserve,
            account(never(5)),
            "a reserve edit right after the pin keeps Never use"
        );

        screen.arrive(&answered(first), &mut ctx);
        drain(&mut ctx);
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored(never(0))))),
            &mut ctx,
        );
        screen.key(&Key::Char('>'), &mut ctx);
        let (more, third) = sent(&mut ctx);
        assert_eq!(
            more,
            account(never(10)),
            "a read that misses a save still in flight does not replace the edits"
        );

        let elsewhere = ctx.next_generation();
        screen.arrive(&answered(elsewhere), &mut ctx);
        screen.arrive(&answered(elsewhere), &mut ctx);
        drain(&mut ctx);
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored(never(0))))),
            &mut ctx,
        );
        screen.key(&Key::Char('>'), &mut ctx);
        let (again, fourth) = sent(&mut ctx);
        assert_eq!(
            again,
            account(never(15)),
            "another screen's answers never stand for this screen's saves"
        );

        screen.shown(&mut ctx);
        drain(&mut ctx);
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored(never(0))))),
            &mut ctx,
        );
        screen.key(&Key::Char('<'), &mut ctx);
        let (back, fifth) = sent(&mut ctx);
        assert_eq!(
            back,
            account(never(10)),
            "showing the screen again keeps the saves it still waits for"
        );

        for generation in [second, third, fourth, fifth] {
            screen.arrive(&answered(generation), &mut ctx);
        }
        drain(&mut ctx);
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored(use_last)))),
            &mut ctx,
        );
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(
            shown.contains("No rank, used to 100%, used last"),
            "once every save is answered the engine's read is shown: {shown}"
        );
        screen.key(&Key::Char('p'), &mut ctx);
        assert_eq!(change(&mut ctx), account(never(0)));
    }

    #[test]
    fn quick_choice_edits_sent_before_any_answer_all_land_on_the_versioned_schedule() {
        use crate::protocol::Outcome;
        use crate::scheduling::tests::{access, database};
        use crate::scheduling::{AccountPlan, Pin, ScheduleChange, apply, load};
        let (_directory, mut db) = database();
        let access = access(&["member-1"]);
        let member = CredentialRef::new("member-1").unwrap();
        let save = |db: &mut rusqlite::Connection, change: &ScheduleChange, version: u64| {
            let tx = db.transaction().unwrap();
            let outcome = apply(&tx, &access, change, version).unwrap();
            tx.commit().unwrap();
            outcome
        };
        let never = |reserve_percent| AccountPlan {
            reserve_percent,
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        };
        let use_last = ScheduleChange::Account {
            connection: member.clone(),
            plan: AccountPlan {
                pin: Some(Pin::UseLast),
                ..AccountPlan::default()
            },
        };
        assert_eq!(save(&mut db, &use_last, 0), Outcome::Applied);
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(load(&db).unwrap()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        while ctx.next_request().is_some() {}

        let mut quick = Vec::new();
        for key in [
            Key::Char('p'),
            Key::Char('>'),
            Key::Char('>'),
            Key::Char('>'),
            Key::Char('<'),
        ] {
            screen.key(&key, &mut ctx);
            let Some(Request::Settings(job)) = ctx.next_request() else {
                panic!("a quick edit is sent at once");
            };
            let SettingsJob::SetSchedule {
                change,
                version,
                generation,
            } = *job
            else {
                panic!("a quick edit sends one schedule change");
            };
            quick.push((change, version, generation));
        }
        assert_eq!(
            quick
                .iter()
                .map(|(_, version, _)| *version)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5],
            "each edit names the version the edit before it makes"
        );
        for (change, version, _) in &quick {
            assert_eq!(
                save(&mut db, change, *version),
                Outcome::Applied,
                "a quick edit sent before any answer is never refused as stale"
            );
        }
        let stored = load(&db).unwrap();
        assert_eq!(stored.version, 6);
        assert_eq!(
            stored.plan(&member),
            never(10),
            "no quick edit is lost and Never use survives every reserve edit"
        );
        let (first, version, _) = &quick[0];
        assert_eq!(
            save(&mut db, first, *version),
            Outcome::Rejected {
                code: "stale_revision".into()
            },
            "an edit naming an old version is still refused"
        );
        assert_eq!(load(&db).unwrap(), stored);

        for (_, _, generation) in &quick {
            screen.arrive(
                &settings(SettingsEvent::ScheduleSet(*generation, Ok(()))),
                &mut ctx,
            );
        }
        while ctx.next_request().is_some() {}
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(stored.clone()))),
            &mut ctx,
        );
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(shown.contains("Never used"), "{shown}");
        screen.key(&Key::Char('>'), &mut ctx);
        let Some(Request::Settings(job)) = ctx.next_request() else {
            panic!("the next edit is sent");
        };
        let SettingsJob::SetSchedule {
            change, version, ..
        } = *job
        else {
            panic!("the next edit sends one schedule change");
        };
        assert_eq!(
            version, stored.version,
            "after the read-back it names the stored version"
        );
        assert_eq!(save(&mut db, &change, version), Outcome::Applied);
        assert_eq!(load(&db).unwrap().plan(&member), never(15));
    }

    #[test]
    fn account_choice_shows_the_live_master_account_choice_and_m_sets_each_option() {
        use crate::scheduling::{LiveMasterAccount, ScheduleChange};
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.key(&Key::Char('o'), &mut ctx);
        assert!(matches!(
            ctx.next_request(),
            Some(Request::Settings(job)) if matches!(*job, SettingsJob::Schedule)
        ));
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(Schedule::default()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        let shown = body(&screen.modal(&ctx).expect("the editor shows").0);
        let line = |shown: &str| {
            shown
                .lines()
                .find(|line| line.contains("Live master account"))
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("{shown}"))
        };
        let help = |shown: &str| {
            let lines: Vec<&str> = shown
                .lines()
                .skip_while(|line| !line.contains("Live master account"))
                .skip(1)
                .take_while(|line| !line.trim().is_empty())
                .collect();
            let fits = lines.iter().all(|line| line.chars().count() <= 76 - 4);
            assert!(fits, "the box cuts a help line: {shown}");
            let words: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            words.join(" ")
        };
        assert!(
            line(&shown).ends_with("Keep start account"),
            "nothing stored reads the default: {shown}"
        );
        assert_eq!(help(&shown), LiveMasterAccount::Keep.detail());
        for choice in [LiveMasterAccount::MoveWhenIdle, LiveMasterAccount::Keep] {
            screen.key(&Key::Char('m'), &mut ctx);
            assert_eq!(change(&mut ctx), ScheduleChange::LiveMaster { choice });
            let shown = body(&screen.modal(&ctx).unwrap().0);
            assert!(line(&shown).ends_with(choice.label()), "{shown}");
            assert_eq!(help(&shown), choice.detail(), "{shown}");
        }
        assert_eq!(ctx.queued(), 0);
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Char('m'), &mut ctx);
        assert_eq!(
            change(&mut ctx),
            ScheduleChange::LiveMaster {
                choice: LiveMasterAccount::MoveWhenIdle
            },
            "the key works from a member's row too"
        );
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
            held_for: Vec::new(),
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
                            why: crate::scheduling::OrderWhy::NoReading,
                        },
                    ),
                    ranked("member-9", Standing::NeverUse),
                ],
                own_order_projects: 0,
            }]))),
            &mut ctx,
        );
        let shown = body(&screen.modal(&ctx).unwrap().0);
        assert!(shown.contains(" 1. ChatGPT member-1"), "{shown}");
        assert!(shown.contains("40% left"), "{shown}");
        assert!(shown.contains("    member-9"), "{shown}");
        assert!(shown.contains("never use"), "{shown}");
        screen.arrive(
            &settings(SettingsEvent::ScheduleSet(ctx.next_generation(), Ok(()))),
            &mut ctx,
        );
        let mut asked = Vec::new();
        while let Some(request) = ctx.next_request() {
            if let Request::Settings(job) = request {
                asked.push(format!("{job:?}"));
            }
        }
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].contains("Ranking"), "{asked:?}");
    }

    /// Account choice names each account as Accounts does, in the member rows and in Next
    /// accounts, and shows an id only for an account whose name is not known (OB-4).
    #[test]
    fn account_choice_names_accounts_and_falls_back_to_the_id() {
        use crate::scheduling::PoolRanking;
        let mut ctx = ctx();
        ctx.store.accounts = Load::Ready(vec![crate::tui::seam::AccountRow {
            id: CredentialRef::new("member-1").unwrap(),
            name: "Claude-ChaoticQubit".into(),
            provider: "ChatGPT".into(),
            status: "Signed in".into(),
            no_window: None,
        }]);
        let mut screen = Pools::new();
        screen.shown(&mut ctx);
        while ctx.next_request().is_some() {}
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(Schedule::default()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        let ranking: Vec<PoolRanking> = serde_json::from_value(serde_json::json!([{
            "pool": "pool-main",
            "strategy": "use_it_or_lose_it",
            "accounts": [
                {
                    "connection": "member-1",
                    "standing": {"kind": "next", "weekly_resets_ms": null,
                        "weekly_left_percent": 40, "use_last": false,
                        "why": {"kind": "no_reading"}}
                },
                {"connection": "member-9", "standing": {"kind": "never_use"}}
            ],
            "own_order_projects": 0
        }]))
        .expect("a ranking reads");
        screen.arrive(&settings(SettingsEvent::Ranking(Ok(ranking))), &mut ctx);
        let shown = body(&screen.modal(&ctx).expect("the editor shows").0);
        assert!(shown.contains("  Claude-ChaoticQubit"), "{shown}");
        assert!(shown.contains(" 1. Claude-ChaoticQubit"), "{shown}");
        assert!(
            !shown.contains("member-1"),
            "a named account never shows its id: {shown}"
        );
        assert!(
            shown.contains("    member-9"),
            "an account with no name known falls back to its id: {shown}"
        );
    }

    /// An account one project holds after its agent reported a limit keeps its place in the
    /// order and says, on its own line under it, which project holds it and until when
    /// (SECAUDIT-RATELIMIT-FORGE).
    #[test]
    fn a_project_hold_shows_on_its_own_line_under_the_account_it_holds() {
        use crate::scheduling::PoolRanking;
        const HOUR: i64 = 3_600_000;
        let mut ctx = ctx();
        let mut screen = Pools::new();
        screen.shown(&mut ctx);
        while ctx.next_request().is_some() {}
        screen.arrive(
            &settings(SettingsEvent::Schedule(Ok(Schedule::default()))),
            &mut ctx,
        );
        screen.key(&Key::Char('o'), &mut ctx);
        let ranking: Vec<PoolRanking> = serde_json::from_value(serde_json::json!([{
            "pool": "pool-main",
            "strategy": "use_it_or_lose_it",
            "accounts": [
                {
                    "connection": "member-1",
                    "standing": {"kind": "next", "weekly_resets_ms": null,
                        "weekly_left_percent": 40, "use_last": false,
                        "why": {"kind": "no_reading"}},
                    "held_for": [{"project": "project-a", "name": "Meals for two",
                        "until_ms": 14 * HOUR + 1_800_000}]
                },
                {"connection": "member-9", "standing": {"kind": "never_use"}}
            ],
            "own_order_projects": 0
        }]))
        .expect("a ranking carries the holds of projects");
        screen.arrive(&settings(SettingsEvent::Ranking(Ok(ranking))), &mut ctx);
        let shown = body(&screen.modal(&ctx).unwrap().0);
        let lines: Vec<&str> = shown.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.contains(" 1. ChatGPT member-1"))
            .unwrap_or_else(|| panic!("{shown}"));
        assert!(
            lines[at + 1].starts_with("    Held for Meals for two until "),
            "{shown}"
        );
        assert!(lines[at + 2].contains("member-9"), "{shown}");
    }

    #[test]
    fn a_standing_reads_as_one_short_reason_in_local_time() {
        use super::super::account_choice::{at_text, standing_line};
        use crate::scheduling::{OrderWhy, Standing};
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
                why: OrderWhy::LeastUsed,
            }),
            "resets Mon 09:00, 40% left"
        );
        assert_eq!(
            line(Standing::Next {
                weekly_resets_ms: None,
                weekly_left_percent: None,
                use_last: true,
                why: OrderWhy::UseLast,
            }),
            "no usage read yet, used last"
        );
        assert_eq!(line(Standing::NotSignedIn), "not signed in");
        assert_eq!(line(Standing::SignInUnconfirmed), "sign-in needs a check");
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
