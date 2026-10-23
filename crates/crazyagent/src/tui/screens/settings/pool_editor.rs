use super::draw_page;
use crate::config::{POOL_MEMBERS_MAX, plain_problem};
use crate::protocol::CredentialRef;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{AccountRow, Arrival, PoolChange, Request};
use crate::tui::text::{cells, join, list_row, restyle, row, status, styled, thumb, with_bar};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, checkbox};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;

const NAME_CHARS_MAX: usize = 64;
const FILTER_CHARS_MAX: usize = 100;
const FIELD_WIDTH_MAX: usize = 64;
const WIDTHS: [usize; 3] = [30, 17, 22];
const ROWS_MIN: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Name,
    Table,
    Filter,
    Save,
    Cancel,
}

/// Settings, Pools, New pool or Edit pool: a name field and a checkbox table
/// of every account, with a filter and "Show selected only".
/// Input: keys, the account list and the save reply. Output: one pool
/// create or update request with at most 15 members.
pub struct PoolEditor {
    id: Option<CredentialRef>,
    name: TextField,
    members: BTreeSet<CredentialRef>,
    focus: Focus,
    cursor: usize,
    offset: usize,
    rows: usize,
    filter: TextField,
    selected_only: bool,
    saving: bool,
}

impl PoolEditor {
    /// A new editor, prefilled from the pool when one is given.
    /// Inputs: the pool id to edit, or None for a new pool, and the context.
    /// Output: the screen with the name field focused.
    pub fn new(id: Option<&CredentialRef>, ctx: &Ctx) -> Self {
        let mut name = TextField::new("Pool name", NAME_CHARS_MAX);
        let mut members = BTreeSet::new();
        if let Some(pool) = id.and_then(|id| {
            ctx.store
                .pools
                .ready()
                .and_then(|pools| pools.iter().find(|pool| &pool.id == id))
        }) {
            name.set(&pool.name);
            members = pool
                .members
                .iter()
                .map(|member| member.id.clone())
                .collect();
        }
        Self {
            id: id.cloned(),
            name,
            members,
            focus: Focus::Name,
            cursor: 0,
            offset: 0,
            rows: ROWS_MIN,
            filter: TextField::new("Filter", FILTER_CHARS_MAX),
            selected_only: false,
            saving: false,
        }
    }

    fn title(&self) -> &'static str {
        if self.id.is_some() {
            "Edit pool"
        } else {
            "New pool"
        }
    }

    /// The accounts shown: sorted by name, then narrowed by the filter words
    /// and "Show selected only".
    /// Input: the context. Output: the rows in display order.
    fn view<'a>(&self, ctx: &'a Ctx) -> Vec<&'a AccountRow> {
        let Some(rows) = ctx.store.accounts.ready() else {
            return Vec::new();
        };
        let words: Vec<String> = self
            .filter
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut view: Vec<&AccountRow> = rows
            .iter()
            .filter(|row| !self.selected_only || self.members.contains(&row.id))
            .filter(|row| {
                let text = format!("{} {} {}", row.name, row.provider, row.status).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .collect();
        view.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        view
    }

    fn page(&self) -> usize {
        self.rows.saturating_sub(1).max(1)
    }

    fn follow(&mut self, count: usize) {
        self.cursor = self.cursor.min(count.saturating_sub(1));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + self.rows {
            self.offset = self.cursor + 1 - self.rows;
        }
        self.offset = self.offset.min(count.saturating_sub(self.rows));
        assert!(
            count == 0 || (self.offset <= self.cursor && self.cursor < self.offset + self.rows)
        );
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Some(id) = self.view(ctx).get(self.cursor).map(|row| row.id.clone()) else {
            return;
        };
        if self.members.remove(&id) {
            return;
        }
        if self.members.len() >= POOL_MEMBERS_MAX {
            ctx.say(Message::warning(
                "A pool holds at most 15 accounts.",
                "Unselect one to add another.",
            ));
            return;
        }
        self.members.insert(id);
        assert!(self.members.len() <= POOL_MEMBERS_MAX);
    }

    fn submit(&mut self, ctx: &mut Ctx) {
        let name = self.name.value().trim().to_owned();
        if name.is_empty()
            || name.chars().count() > NAME_CHARS_MAX
            || name.chars().any(char::is_control)
        {
            self.name.error = Some(if name.is_empty() {
                "Enter a pool name.".into()
            } else {
                plain_problem("pool_name_bounds")
                    .expect("pool_name_bounds has a sentence")
                    .into()
            });
            self.focus = Focus::Name;
            return;
        }
        if self.members.is_empty() {
            ctx.say(Message::error(
                "Choose at least one account.",
                "Space selects one.",
            ));
            self.focus = Focus::Table;
            return;
        }
        let members: Vec<CredentialRef> = self.members.iter().cloned().collect();
        let change = match &self.id {
            Some(id) => PoolChange::Update {
                id: id.clone(),
                name,
                members,
            },
            None => PoolChange::Create { name, members },
        };
        self.saving = true;
        ctx.request(Request::EditPool { change });
    }

    fn step_focus(&mut self, forward: bool) {
        let order = [Focus::Name, Focus::Table, Focus::Save, Focus::Cancel];
        let current = if self.focus == Focus::Filter {
            Focus::Table
        } else {
            self.focus
        };
        let index = order
            .iter()
            .position(|focus| *focus == current)
            .expect("every focus is in the order");
        self.focus = order[if forward {
            (index + 1) % order.len()
        } else {
            (index + order.len() - 1) % order.len()
        }];
        if self.focus != Focus::Name && self.name.value().trim().is_empty() {
            self.name.error = Some("Enter a pool name.".into());
        } else if !self.name.value().trim().is_empty() {
            self.name.error = None;
        }
    }

    fn table_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.view(ctx).len();
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(count.saturating_sub(1)),
            Key::PageUp => self.cursor = self.cursor.saturating_sub(self.page()),
            Key::PageDown => self.cursor = (self.cursor + self.page()).min(count.saturating_sub(1)),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = count.saturating_sub(1),
            Key::Char(' ') => self.toggle(ctx),
            Key::Char('/') => self.focus = Focus::Filter,
            Key::Char('v') => {
                self.selected_only = !self.selected_only;
                self.cursor = 0;
            }
            Key::Enter => self.focus = Focus::Save,
            _ => return Flow::Ignored,
        }
        let count = self.view(ctx).len();
        self.follow(count);
        Flow::Handled
    }

    fn hints_for(&self) -> Vec<Hint> {
        match self.focus {
            Focus::Table => vec![
                hint("Space", "Select"),
                hint("/", "Filter"),
                hint("v", "Selected only"),
                hint("Tab", "Next"),
                hint("Esc", "Cancel"),
            ],
            Focus::Filter => vec![hint("Enter", "Keep filter"), hint("Esc", "Clear filter")],
            Focus::Save => vec![
                hint("Enter", "Save pool"),
                hint("Shift-Tab", "Previous"),
                hint("Esc", "Cancel"),
            ],
            Focus::Cancel => vec![hint("Enter", "Cancel"), hint("Shift-Tab", "Previous")],
            Focus::Name => vec![
                hint("Tab", "Next field"),
                hint("Enter", "Next"),
                hint("Esc", "Cancel"),
            ],
        }
    }

    fn table(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, String) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let total = match &ctx.store.accounts {
            Load::Ready(rows) => rows.len(),
            Load::NotReady => {
                return (
                    vec![
                        Line::raw("Not ready yet."),
                        styled(
                            "This build cannot list your accounts, so members cannot change here yet.",
                            Role::Muted,
                            theme,
                        ),
                    ],
                    String::new(),
                );
            }
            Load::Failed(_) => {
                return (
                    vec![status("Failed to load accounts.", theme, glyphs)],
                    String::new(),
                );
            }
            Load::Idle | Load::Loading => {
                return (vec![ctx.loading("Loading accounts")], String::new());
            }
        };
        let view = self.view(ctx);
        if view.is_empty() {
            let text = if total == 0 {
                "No accounts yet. Add one in Settings, Accounts."
            } else {
                "Nothing matches."
            };
            return (vec![Line::raw(text)], String::new());
        }
        let shown: Vec<Line<'static>> = view
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(self.rows)
            .map(|(index, account)| {
                list_row(
                    cells(
                        &WIDTHS,
                        vec![
                            Line::raw(checkbox(self.members.contains(&account.id), &account.name)),
                            Line::raw(format!(
                                "{} {}",
                                glyphs.provider(&account.provider),
                                account.provider
                            )),
                            status(&account.status, theme, glyphs),
                        ],
                        2,
                        glyphs,
                    ),
                    limit - 1,
                    index == self.cursor,
                    matches!(self.focus, Focus::Table | Focus::Filter),
                    theme,
                    glyphs,
                )
            })
            .collect();
        let visible = shown.len();
        let (top, size) = thumb(visible, self.offset, view.len());
        let last = self.offset + visible;
        (
            with_bar(shown, limit, top, size, theme, glyphs),
            format!("Showing {}-{last} of {}", self.offset + 1, view.len()),
        )
    }
}

impl Screen for PoolEditor {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Pools".into(), self.title().into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        self.hints_for()
    }

    fn typing(&self) -> bool {
        matches!(self.focus, Focus::Name | Focus::Filter)
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.saving {
            return Flow::Handled;
        }
        match (self.focus, key) {
            (Focus::Filter, Key::Esc) => {
                self.filter.clear();
                self.focus = Focus::Table;
                self.cursor = 0;
                let count = self.view(ctx).len();
                self.follow(count);
                return Flow::Handled;
            }
            (_, Key::Esc) => return Flow::Back,
            (Focus::Filter, Key::Enter) => {
                self.focus = Focus::Table;
                return Flow::Handled;
            }
            (_, Key::Tab) => {
                self.step_focus(true);
                return Flow::Handled;
            }
            (_, Key::BackTab) => {
                self.step_focus(false);
                return Flow::Handled;
            }
            _ => {}
        }
        match self.focus {
            Focus::Name => {
                if matches!(key, Key::Enter) {
                    self.step_focus(true);
                    return Flow::Handled;
                }
                if self.name.key(key) {
                    if !self.name.value().trim().is_empty() {
                        self.name.error = None;
                    }
                    return Flow::Handled;
                }
                Flow::Ignored
            }
            Focus::Filter => {
                if self.filter.key(key) {
                    self.cursor = 0;
                    let count = self.view(ctx).len();
                    self.follow(count);
                    return Flow::Handled;
                }
                Flow::Ignored
            }
            Focus::Table => self.table_key(key, ctx),
            Focus::Save => match key {
                Key::Enter => {
                    self.submit(ctx);
                    Flow::Handled
                }
                Key::Left | Key::Right => {
                    self.focus = Focus::Cancel;
                    Flow::Handled
                }
                _ => Flow::Ignored,
            },
            Focus::Cancel => match key {
                Key::Enter => Flow::Back,
                Key::Left | Key::Right => {
                    self.focus = Focus::Save;
                    Flow::Handled
                }
                _ => Flow::Ignored,
            },
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::PoolEdited(result) if self.saving => {
                self.saving = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success("Pool saved.", ""));
                        ctx.request(Request::Pools);
                        Flow::Back
                    }
                    Err(code) if code == "pool_name_bounds" => {
                        self.name.error = plain_problem(code).map(Into::into);
                        self.focus = Focus::Name;
                        Flow::Handled
                    }
                    Err(code) => {
                        ctx.say(Message::error(
                            plain_problem(code).unwrap_or("Could not save the pool."),
                            "Press Enter to try again.",
                        ));
                        Flow::Handled
                    }
                }
            }
            _ => Flow::Handled,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let wide = ctx.wide();
        let limit = area.width as usize - 4;
        let full = self.members.len() >= POOL_MEMBERS_MAX;
        let counter = format!("{} of {POOL_MEMBERS_MAX} selected", self.members.len());
        let mut content = vec![row(
            limit,
            styled(self.title(), Role::Accent, theme),
            styled(
                counter,
                if full { Role::Warning } else { Role::Heading },
                theme,
            ),
            ctx.glyphs,
        )];
        if wide {
            content.push(Line::default());
        }
        let mut cursor = None;
        let (field, at) = self.name.lines(
            limit.min(FIELD_WIDTH_MAX),
            self.focus == Focus::Name,
            theme,
            ctx.glyphs,
        );
        if let Some(at) = at {
            cursor = Some(Position::new(at.x, at.y + content.len() as u16));
        }
        content.extend(field);
        content.push(Line::default());
        let filter = if self.focus == Focus::Filter || !self.filter.is_empty() {
            let typed = format!("/ {}", self.filter.value());
            if self.focus == Focus::Filter {
                cursor = Some(Position::new(
                    (10 + crate::tui::text::width(&typed)) as u16,
                    content.len() as u16,
                ));
            }
            Span::styled(
                typed,
                if self.focus == Focus::Filter {
                    theme.accent
                } else {
                    theme.muted
                },
            )
        } else {
            Span::styled("/ Filter", theme.muted)
        };
        content.push(row(
            limit,
            Line::from(vec![
                Span::styled("Accounts", theme.accent),
                Span::raw("  "),
                filter,
            ]),
            styled(
                if self.selected_only {
                    "v  Show all accounts"
                } else {
                    "v  Show selected only"
                },
                Role::Muted,
                theme,
            ),
            ctx.glyphs,
        ));
        content.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &WIDTHS,
                    ["    Account", "Provider", "Status"]
                        .map(Line::raw)
                        .to_vec(),
                    2,
                    ctx.glyphs,
                ),
                theme.muted,
            ),
        ]));
        let room = area.height as usize - 2;
        self.rows = room.saturating_sub(content.len() + 3).max(ROWS_MIN);
        let count = self.view(ctx).len();
        self.follow(count);
        let (table, showing) = self.table(limit, ctx);
        content.extend(table);
        content.push(styled(showing, Role::Muted, theme));
        content.push(Line::default());
        if self.saving {
            content.push(ctx.loading("Saving the pool"));
        } else {
            let focused = match self.focus {
                Focus::Save => Some(0),
                Focus::Cancel => Some(1),
                _ => None,
            };
            content.push(buttons(&["Save pool", "Cancel"], focused, theme));
        }
        draw_page(frame, area, SettingsSection::Pools, content, cursor, ctx)
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        self.saving || matches!(ctx.store.accounts, Load::Idle | Load::Loading)
    }

    fn place(&self) -> Option<Place> {
        Some(match &self.id {
            Some(id) => Place::EditPool(id.clone()),
            None => Place::NewPool,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::UNICODE;
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn account(index: usize) -> AccountRow {
        AccountRow {
            id: CredentialRef::new(format!("account-{index:03}")).unwrap(),
            name: format!("Seat {index:03}"),
            provider: "Claude".into(),
            status: "Signed in".into(),
        }
    }

    fn ctx(count: usize) -> Ctx {
        let mut ctx = Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        );
        ctx.store.accounts = Load::Ready((0..count).map(account).collect());
        ctx
    }

    fn typed(editor: &mut PoolEditor, text: &str, ctx: &mut Ctx) {
        for character in text.chars() {
            editor.key(&Key::Char(character), ctx);
        }
    }

    #[test]
    fn the_sixteenth_account_is_refused_with_the_limit_message() {
        let mut ctx = ctx(20);
        let mut editor = PoolEditor::new(None, &ctx);
        typed(&mut editor, "Build team", &mut ctx);
        editor.key(&Key::Tab, &mut ctx);
        for _ in 0..16 {
            editor.key(&Key::Char(' '), &mut ctx);
            editor.key(&Key::Down, &mut ctx);
        }
        assert_eq!(editor.members.len(), 15);
        assert!(!editor.members.contains(&account(15).id));
        assert_eq!(
            ctx.message().unwrap().lead,
            "A pool holds at most 15 accounts."
        );
        editor.key(&Key::Up, &mut ctx);
        editor.key(&Key::Char(' '), &mut ctx);
        assert_eq!(editor.members.len(), 15, "the refused row stays unselected");
        editor.key(&Key::Home, &mut ctx);
        editor.key(&Key::Char(' '), &mut ctx);
        assert_eq!(editor.members.len(), 14, "unselecting always works");
    }

    #[test]
    fn filter_and_selected_only_narrow_the_rows() {
        let mut ctx = ctx(30);
        let mut editor = PoolEditor::new(None, &ctx);
        editor.key(&Key::Tab, &mut ctx);
        editor.key(&Key::Char(' '), &mut ctx);
        editor.key(&Key::Char('/'), &mut ctx);
        assert!(editor.typing());
        typed(&mut editor, "SEAT 02", &mut ctx);
        let names: Vec<String> = editor
            .view(&ctx)
            .iter()
            .map(|row| row.name.clone())
            .collect();
        assert_eq!(names.len(), 11);
        assert_eq!(names[0], "Seat 002");
        assert!(names.iter().all(|name| name.contains("02")));
        editor.key(&Key::Esc, &mut ctx);
        assert!(!editor.typing());
        assert_eq!(editor.view(&ctx).len(), 30, "Esc clears the filter");
        editor.key(&Key::Char('v'), &mut ctx);
        assert_eq!(editor.view(&ctx).len(), 1);
        assert_eq!(editor.view(&ctx)[0].name, "Seat 000");
    }

    #[test]
    fn saving_needs_a_name_and_one_account_then_sends_one_create() {
        let mut ctx = ctx(3);
        let mut editor = PoolEditor::new(None, &ctx);
        editor.key(&Key::Tab, &mut ctx);
        editor.key(&Key::Tab, &mut ctx);
        assert_eq!(editor.focus, Focus::Save);
        editor.key(&Key::Enter, &mut ctx);
        assert_eq!(editor.focus, Focus::Name);
        assert_eq!(editor.name.error.as_deref(), Some("Enter a pool name."));
        assert_eq!(ctx.queued(), 0);

        typed(&mut editor, "  Build team ", &mut ctx);
        assert_eq!(editor.name.error, None);
        editor.key(&Key::Tab, &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        assert_eq!(ctx.message().unwrap().lead, "Choose at least one account.");
        assert_eq!(editor.focus, Focus::Table);
        assert_eq!(ctx.queued(), 0);

        editor.key(&Key::Down, &mut ctx);
        editor.key(&Key::Char(' '), &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        match ctx.next_request() {
            Some(Request::EditPool {
                change: PoolChange::Create { name, members },
            }) => {
                assert_eq!(name, "Build team");
                assert_eq!(members, vec![account(1).id]);
            }
            other => panic!("unexpected request {other:?}"),
        }
        assert!(editor.saving);
        assert!(
            matches!(editor.key(&Key::Esc, &mut ctx), Flow::Handled),
            "keys wait while saving"
        );

        let flow = editor.arrive(&Arrival::PoolEdited(Err("pool_exists".into())), &mut ctx);
        assert!(matches!(flow, Flow::Handled));
        assert_eq!(
            ctx.message().unwrap().lead,
            "A pool with this id already exists."
        );
        editor.key(&Key::Enter, &mut ctx);
        assert!(matches!(ctx.next_request(), Some(Request::EditPool { .. })));
        let flow = editor.arrive(&Arrival::PoolEdited(Ok(())), &mut ctx);
        assert!(matches!(flow, Flow::Back));
        assert_eq!(ctx.message().unwrap().lead, "Pool saved.");
        assert!(matches!(ctx.next_request(), Some(Request::Pools)));
    }

    #[test]
    fn editing_keeps_the_pool_id_and_current_members() {
        use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
        let mut ctx = ctx(5);
        let id = CredentialRef::new("pool-main").unwrap();
        ctx.store.pools = Load::Ready(vec![PoolSummary {
            id: id.clone(),
            name: "Main".into(),
            summary: String::new(),
            members: vec![PoolMember {
                id: account(2).id,
                kind: MemberKind::Claude,
                provider: "Claude".into(),
                status: MemberStatus::SignedIn,
                status_label: "Signed in".into(),
            }],
            project_count: 1,
        }]);
        let mut editor = PoolEditor::new(Some(&id), &ctx);
        assert_eq!(editor.crumbs(), ["Settings", "Pools", "Edit pool"]);
        assert_eq!(editor.place(), Some(Place::EditPool(id.clone())));
        typed(&mut editor, " 2", &mut ctx);
        editor.key(&Key::Tab, &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        editor.key(&Key::Enter, &mut ctx);
        match ctx.next_request() {
            Some(Request::EditPool {
                change:
                    PoolChange::Update {
                        id: sent,
                        name,
                        members,
                    },
            }) => {
                assert_eq!(sent, id);
                assert_eq!(name, "Main 2");
                assert_eq!(members, vec![account(2).id]);
            }
            other => panic!("unexpected request {other:?}"),
        }
    }

    #[test]
    fn the_window_follows_the_cursor() {
        let mut ctx = ctx(40);
        let mut editor = PoolEditor::new(None, &ctx);
        editor.rows = 9;
        editor.key(&Key::Tab, &mut ctx);
        for _ in 0..15 {
            editor.key(&Key::Down, &mut ctx);
        }
        assert_eq!((editor.cursor, editor.offset), (15, 7));
        editor.key(&Key::End, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (39, 31));
        editor.key(&Key::PageUp, &mut ctx);
        assert_eq!(
            (editor.cursor, editor.offset),
            (31, 31),
            "a page is the visible rows minus one"
        );
        editor.key(&Key::PageUp, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (23, 23));
        editor.key(&Key::PageDown, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (31, 23));
        editor.key(&Key::PageDown, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (39, 31));
        editor.key(&Key::PageDown, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (39, 31));
        editor.key(&Key::Home, &mut ctx);
        assert_eq!((editor.cursor, editor.offset), (0, 0));
    }
}
