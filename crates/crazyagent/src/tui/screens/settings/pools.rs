use super::{draw_page, step};
use crate::config::{POOL_MEMBERS_MAX, plain_problem};
use crate::pools::{MemberStatus, PoolSummary};
use crate::protocol::CredentialRef;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
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

/// Settings, Pools: every pool with its size, readiness and the projects using
/// it.
/// Input: keys and pool arrivals. Output: the pool table or the empty state.
pub struct Pools {
    selected: usize,
    offset: usize,
    rows: usize,
    confirm: Option<Confirm>,
    deleting: bool,
}

struct Confirm {
    id: CredentialRef,
    name: String,
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
            deleting: false,
        }
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
                self.confirm = None;
                self.deleting = true;
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
        if used_by(pool, false, ctx) != "Not used" {
            ctx.say(Message::error(
                plain_problem("pool_in_use").expect("pool_in_use has a sentence"),
                "",
            ));
            return;
        }
        self.confirm = Some(Confirm {
            id: pool.id.clone(),
            name: pool.name.clone(),
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
        let count = ctx.store.pools.ready().map_or(0, Vec::len);
        match key {
            Key::Char('a') => Flow::Open(Place::NewPool),
            Key::Char('d') if count > 0 && !self.deleting => {
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
        match arrival {
            Arrival::Pools(Ok(pools)) => self.follow(pools.len()),
            Arrival::PoolEdited(result) if self.deleting => {
                self.deleting = false;
                match result {
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
        let confirm = self.confirm.as_ref()?;
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        Some((
            Modal {
                width: 54,
                title: "Delete pool".into(),
                body: vec![
                    Line::raw(format!("Delete {}?", confirm.name)),
                    Line::default(),
                    Line::raw("No project uses it. Its accounts stay saved."),
                    Line::default(),
                    buttons(
                        &["Cancel", "Delete"],
                        Some(usize::from(confirm.delete_focused)),
                        &ctx.theme,
                    ),
                ],
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
                Line::from(vec![
                    Span::styled(ctx.spinner(), theme.running),
                    Span::raw(" Loading pools"),
                ]),
            ],
        };
        draw_page(frame, area, SettingsSection::Pools, content, None, ctx)
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        self.deleting || matches!(ctx.store.pools, Load::Loading | Load::Idle)
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

    #[test]
    fn a_used_pool_cannot_be_deleted_and_says_why() {
        let mut ctx = ctx();
        let mut screen = Pools::new();
        assert!(matches!(
            screen.key(&Key::Char('d'), &mut ctx),
            Flow::Handled
        ));
        assert!(screen.modal(&ctx).is_none());
        assert_eq!(
            ctx.message().unwrap().lead,
            "This pool is used by a project. Remove it from the project first."
        );
        assert_eq!(ctx.queued(), 0);
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

        screen.arrive(&Arrival::PoolEdited(Err("pool_in_use".into())), &mut ctx);
        assert_eq!(
            ctx.message().unwrap().lead,
            "This pool is used by a project. Remove it from the project first."
        );
        assert!(matches!(ctx.next_request(), Some(Request::Pools)));
        assert!(!screen.deleting);
    }
}
