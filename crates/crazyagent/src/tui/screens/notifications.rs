use super::{Flow, Place, ProjectTab, Screen, SettingsSection, Spot};
use crate::notifications::{
    FoldChange, Notice, NoticeAction, NoticeFilter, NoticeFold, NoticeKind, NoticeQuery,
    NoticeTarget, PAGE_MAX,
};
use crate::protocol::ProjectId;
use crate::tui::client::OwnerEvent;
use crate::tui::client::notifications::{NoticesEvent, NoticesQuery, NoticesReply};
use crate::tui::clock::{ago, local_offset, now_ms};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{list_row, pad};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;

const GUTTER: u16 = 2;
const HEAD_ROWS: usize = 2;
const PAGES_MAX: usize = 10;
const ITEMS_MAX: usize = PAGE_MAX as usize * PAGES_MAX;
const FILTERS: [(NoticeFilter, &str, &str); 4] = [
    (NoticeFilter::All, "All", "No notifications yet."),
    (
        NoticeFilter::Pending,
        "Needs action",
        "Nothing needs your action.",
    ),
    (NoticeFilter::Unread, "Unread", "Everything is read."),
    (NoticeFilter::Done, "Done", "No resolved items yet."),
];
const GONE: &str = "That project is no longer in your projects.";
const LOADING: &str = "Your projects are still loading. Try again in a moment.";
const UPDATES: &str = "Install updates from the desktop app.";

/// The notification center list (DEC-136a decisions 9 and 10), opened with n or Go to:
/// one engine store read by filter (All, Needs action, Unread, Done) and project, same-kind
/// items of a project folded into one row, newest first. Enter opens the item's page and
/// marks it read; Space marks rows for r, u and d; a marks every item of the picked
/// project read. Takes engine replies through `arrive`; never invents items.
pub(in crate::tui) struct Notifications {
    filter: NoticeFilter,
    project: Option<ProjectId>,
    items: Load<Vec<Notice>>,
    more: bool,
    cursor: usize,
    top: usize,
    marked: BTreeSet<i64>,
    changing: BTreeSet<u64>,
    asked: Option<NoticesQuery>,
    stale: bool,
}

impl Notifications {
    /// Opens the list on All over every project with nothing read yet. Input: none.
    /// Output: the screen; the first read goes out when it is shown.
    pub fn new() -> Self {
        Self {
            filter: NoticeFilter::All,
            project: None,
            items: Load::Idle,
            more: false,
            cursor: 0,
            top: 0,
            marked: BTreeSet::new(),
            changing: BTreeSet::new(),
            asked: None,
            stale: false,
        }
    }

    fn rows(&self) -> &[Notice] {
        self.items.ready().map_or(&[][..], Vec::as_slice)
    }

    /// Sends one page read for the filter and project, unless a read is on its way, in
    /// which case it is read again once that one lands. Inputs: the paging bound, none for
    /// the first page, and the context. Output: none.
    fn read(&mut self, before: Option<i64>, ctx: &mut Ctx) {
        if self.asked.is_some() {
            self.stale |= before.is_none();
            return;
        }
        let query = NoticesQuery::Page(NoticeQuery {
            filter: self.filter,
            project_id: self.project.clone(),
            kind: None,
            fold: true,
            before,
            after: None,
            limit: PAGE_MAX,
        });
        if self.items.ready().is_none() {
            self.items = Load::Loading;
        }
        self.asked = Some(query.clone());
        ctx.request(Request::Notices(Box::new(query)));
    }

    /// Starts over on a new filter or project: forgets rows, marks and the read on its way.
    /// Input: the context. Output: none.
    fn restart(&mut self, ctx: &mut Ctx) {
        self.items = Load::Loading;
        self.more = false;
        self.cursor = 0;
        self.top = 0;
        self.marked.clear();
        self.changing.clear();
        self.asked = None;
        self.stale = false;
        self.read(None, ctx);
    }

    fn take_page(&mut self, event: &NoticesEvent) {
        let NoticesQuery::Page(query) = &event.query else {
            return;
        };
        match &event.result {
            Ok(NoticesReply::Page(page)) => {
                let mut rows = if query.before.is_some() {
                    self.items.ready().cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                let room = ITEMS_MAX.saturating_sub(rows.len());
                rows.extend(page.items.iter().take(room).cloned());
                self.more = page.more && rows.len() < ITEMS_MAX;
                let ids: BTreeSet<i64> = rows.iter().map(|row| row.id).collect();
                self.marked.retain(|id| ids.contains(id));
                self.cursor = self.cursor.min(rows.len().saturating_sub(1));
                assert!(rows.len() <= ITEMS_MAX);
                self.items = Load::Ready(rows);
            }
            Ok(_) => {}
            Err(code) => {
                if self.items.ready().is_none() {
                    self.items = Load::Failed(code.clone());
                }
            }
        }
    }

    /// Takes the reply to a change: one that stands reads the list again and, once every
    /// piece of the list's own latest change stands, clears the marks; a refused one says
    /// so and keeps the marks to try again. Inputs: the reply and the context. Output: none.
    fn take_change(&mut self, event: &NoticesEvent, ctx: &mut Ctx) {
        let NoticesQuery::Act { action, generation } = &event.query else {
            return;
        };
        if !matches!(
            action,
            NoticeAction::Read { .. }
                | NoticeAction::Unread { .. }
                | NoticeAction::Delete { .. }
                | NoticeAction::Folds { .. }
                | NoticeAction::ReadAll { .. }
        ) {
            return;
        }
        let own = self.changing.remove(generation);
        match &event.result {
            Ok(_) => {
                if own && self.changing.is_empty() {
                    self.marked.clear();
                }
                self.read(None, ctx);
            }
            Err(code) => {
                if own {
                    self.changing.clear();
                }
                ctx.say(Message::error(
                    "The notifications were not changed.",
                    super::settings_accounts::plain(code),
                ));
            }
        }
    }

    /// Sends one change to every item of the marked rows, or of the row under the cursor
    /// when none is marked, as folds in pieces of at most `PAGE_MAX`; the marks stay until
    /// every piece stands. Inputs: the change and the context. Output: none.
    fn change(&mut self, change: FoldChange, ctx: &mut Ctx) {
        let rows = self.rows();
        let folds: Vec<NoticeFold> = if self.marked.is_empty() {
            rows.get(self.cursor)
                .map(|row| vec![fold_of(row, self.filter)])
                .unwrap_or_default()
        } else {
            rows.iter()
                .filter(|row| self.marked.contains(&row.id))
                .map(|row| fold_of(row, self.filter))
                .collect()
        };
        self.changing.clear();
        for piece in folds.chunks(usize::from(PAGE_MAX)) {
            assert!(!piece.is_empty() && piece.len() <= usize::from(PAGE_MAX));
            let folds = piece.to_vec();
            self.changing
                .insert(send(NoticeAction::Folds { folds, change }, ctx));
        }
        assert!(self.changing.len() <= PAGES_MAX);
    }

    fn open(&mut self, ctx: &mut Ctx) -> Flow {
        let Some(row) = self.rows().get(self.cursor).cloned() else {
            return Flow::Handled;
        };
        open_notice(&row, self.filter, ctx)
    }

    fn walk_project(&mut self, forward: bool, ctx: &mut Ctx) {
        let ids: Vec<ProjectId> = ctx
            .store
            .projects
            .ready()
            .map(|cards| cards.iter().map(|card| card.id.clone()).collect())
            .unwrap_or_default();
        let mut scopes: Vec<Option<ProjectId>> = vec![None];
        scopes.extend(ids.into_iter().map(Some));
        let at = scopes
            .iter()
            .position(|scope| *scope == self.project)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % scopes.len()
        } else {
            (at + scopes.len() - 1) % scopes.len()
        };
        self.project = scopes.swap_remove(next);
        self.restart(ctx);
    }

    fn walk_filter(&mut self, forward: bool, ctx: &mut Ctx) {
        let at = FILTERS
            .iter()
            .position(|(filter, _, _)| *filter == self.filter)
            .expect("the filter is one of FILTERS");
        let next = if forward {
            (at + 1) % FILTERS.len()
        } else {
            (at + FILTERS.len() - 1) % FILTERS.len()
        };
        self.filter = FILTERS[next].0;
        self.restart(ctx);
    }

    fn head(&self, ctx: &Ctx) -> Line<'static> {
        let theme = &ctx.theme;
        let mut spans = Vec::new();
        for (filter, label, _) in FILTERS {
            if filter == self.filter {
                spans.push(Span::styled(format!("[{label}]"), theme.heading));
            } else {
                spans.push(Span::styled(format!(" {label} "), theme.muted));
            }
            spans.push(Span::raw(" "));
        }
        let project = self
            .project
            .as_ref()
            .map_or_else(|| "All projects".to_owned(), |id| project_name(id, ctx));
        spans.push(Span::raw(format!("   Project: {project}")));
        if !self.marked.is_empty() {
            spans.push(Span::styled(
                format!("   {} selected", self.marked.len()),
                theme.accent,
            ));
        }
        Line::from(spans)
    }

    fn row(&self, row: &Notice, width: usize, label_width: usize, ctx: &Ctx) -> Line<'static> {
        let theme = &ctx.theme;
        let mut spans = Vec::new();
        if !self.marked.is_empty() {
            let mark = if self.marked.contains(&row.id) {
                "[x] "
            } else {
                "[ ] "
            };
            spans.push(Span::raw(mark));
        }
        let dot = if row.unread > 0 { ctx.glyphs.dot } else { " " };
        spans.push(Span::styled(format!("{dot} "), theme.accent));
        spans.push(Span::styled(
            format!("{:<label_width$}  ", row.kind.label()),
            theme.heading,
        ));
        spans.push(Span::raw(row.text.clone()));
        let mut notes = Vec::new();
        if let Some(project) = &row.project_id {
            notes.push(project_name(project, ctx));
        }
        if row.folded > 1 {
            notes.push(format!("{} items", row.folded));
        }
        if row.resolved_ms.is_some() {
            notes.push("done".to_owned());
        }
        notes.push(ago(row.created_ms, now_ms(), local_offset()));
        spans.push(Span::styled(format!("  {}", notes.join("  ")), theme.muted));
        if row.pending > 0 {
            spans.push(Span::styled("  needs action", theme.warning));
        }
        pad(Line::from(spans), width, ctx.glyphs)
    }
}

/// The fold one row stands for under the filter it was listed in, which reaches every item
/// the row holds at or below its newest id, past the ids the row names. Inputs: the row and
/// the filter. Output: the fold.
fn fold_of(row: &Notice, filter: NoticeFilter) -> NoticeFold {
    NoticeFold {
        kind: row.kind,
        project_id: row.project_id.clone(),
        filter,
        newest: row.id,
    }
}

/// Opens one row's page on the item it names and marks what the row holds read: one item
/// by its id, a fold through Folds under the filter it was listed in. Shared by the list and
/// a project's Needs you. Inputs: the row, the filter and the context. Output: the flow,
/// or Handled with a message when the TUI has no such page.
pub(in crate::tui) fn open_notice(row: &Notice, filter: NoticeFilter, ctx: &mut Ctx) -> Flow {
    if row.unread > 0 {
        let action = if row.folded == 1 {
            NoticeAction::Read { ids: vec![row.id] }
        } else {
            NoticeAction::Folds {
                folds: vec![fold_of(row, filter)],
                change: FoldChange::Read,
            }
        };
        send(action, ctx);
    }
    match place_of(&row.target, ctx) {
        Ok((Place::Home, _)) => Flow::Go(Place::Home),
        Ok((place, Some(spot))) => Flow::OpenAt(place, spot),
        Ok((place, None)) => Flow::Open(place),
        Err(text) => {
            ctx.say(Message::warning(text, ""));
            Flow::Handled
        }
    }
}

/// Queues one change to the store with a fresh generation. Inputs: the change and the
/// context. Output: the generation its reply carries; a full queue answers it as the
/// owner being busy.
fn send(action: NoticeAction, ctx: &mut Ctx) -> u64 {
    let generation = ctx.next_generation();
    ctx.request(Request::Notices(Box::new(NoticesQuery::Act {
        action,
        generation,
    })));
    generation
}

/// A project's name from the projects the shell read, or its id while it is not listed.
/// Inputs: the project and the context. Output: the name.
fn project_name(id: &ProjectId, ctx: &Ctx) -> String {
    ctx.store
        .projects
        .ready()
        .and_then(|cards| cards.iter().find(|card| card.id == *id))
        .map_or_else(|| id.as_str().to_owned(), |card| card.name.clone())
}

/// The TUI page that fixes or shows one item's target and the item to pick there: the
/// notified question or change set, an agent's question or wait in its Team screen's
/// Questions box, or the first suggested memory. Inputs: the target and the context, whose project list names the
/// project. Output: the place and the item, or the sentence the message line shows when
/// the TUI has no such page or the project is not listed.
fn place_of(target: &NoticeTarget, ctx: &Ctx) -> Result<(Place, Option<Spot>), &'static str> {
    let project = |id: &ProjectId| -> Result<(ProjectId, String), &'static str> {
        let cards = ctx.store.projects.ready().ok_or(LOADING)?;
        cards
            .iter()
            .find(|card| card.id == *id)
            .map(|card| (card.id.clone(), card.name.clone()))
            .ok_or(GONE)
    };
    let tab = |id: &ProjectId, tab: ProjectTab| {
        project(id).map(|(id, name)| Place::Project { id, name, tab })
    };
    let spot = match target {
        NoticeTarget::Questions {
            question: Some(question),
            ..
        } => u64::try_from(*question).ok().map(Spot::Question),
        NoticeTarget::Questions {
            question: None,
            agent: Some(agent),
            ..
        } => Some(Spot::Agent(*agent)),
        NoticeTarget::ChangeSet {
            change_set: Some(change_set),
            ..
        } => Some(Spot::ChangeSet(change_set.clone())),
        NoticeTarget::Memories { .. } => Some(Spot::MemoryReview),
        _ => None,
    };
    let place = match target {
        NoticeTarget::Questions {
            project_id,
            question: None,
            agent: Some(_),
        } => project(project_id).map(|(id, name)| Place::Team { id, name }),
        NoticeTarget::Questions { project_id, .. } => tab(project_id, ProjectTab::Questions),
        NoticeTarget::ProjectPool { project_id } => tab(project_id, ProjectTab::Settings),
        NoticeTarget::ChangeSet { project_id, .. } => tab(project_id, ProjectTab::Changes),
        NoticeTarget::Agents { project_id } => tab(project_id, ProjectTab::Agents),
        NoticeTarget::Conversation { project_id } => tab(project_id, ProjectTab::Conversation),
        NoticeTarget::Memories { project_id } => {
            project(project_id).map(|(id, name)| Place::ProjectMemory { id, name })
        }
        NoticeTarget::AccountSignIn { .. } | NoticeTarget::Accounts => {
            Ok(Place::Settings(SettingsSection::Accounts))
        }
        NoticeTarget::Usage => Ok(Place::Usage),
        NoticeTarget::Harnesses => Ok(Place::Settings(SettingsSection::Harnesses)),
        NoticeTarget::Home => Ok(Place::Home),
        NoticeTarget::Updates => Err(UPDATES),
    }?;
    assert!(
        spot.is_none()
            || matches!(
                place,
                Place::Project { .. } | Place::Team { .. } | Place::ProjectMemory { .. }
            )
    );
    Ok((place, spot))
}

impl Screen for Notifications {
    fn crumbs(&self) -> Vec<String> {
        vec!["Notifications".into()]
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = vec![
            hint(&vertical, "Move"),
            hint("Enter", "Open"),
            hint("Space", "Select"),
            hint("r", "Read"),
            hint("u", "Unread"),
            hint("d", "Delete"),
            hint("a", "All read"),
            hint("f", "Filter"),
            hint("p", "Project"),
            hint("s", "Settings"),
        ];
        hints.push(if self.marked.is_empty() {
            hint("Esc", "Back")
        } else {
            hint("Esc", "Clear selection")
        });
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.rows().len();
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => {
                self.cursor = (self.cursor + 1).min(count.saturating_sub(1));
                if self.more && count > 0 && self.cursor + 1 == count {
                    let last = self.rows().last().map(|row| row.id);
                    self.read(last, ctx);
                }
            }
            Key::Enter => return self.open(ctx),
            Key::Char(' ') => {
                if let Some(id) = self.rows().get(self.cursor).map(|row| row.id)
                    && !self.marked.remove(&id)
                {
                    self.marked.insert(id);
                }
            }
            Key::Char('r') => self.change(FoldChange::Read, ctx),
            Key::Char('u') => self.change(FoldChange::Unread, ctx),
            Key::Char('d') => self.change(FoldChange::Delete, ctx),
            Key::Char('a') => {
                let project_id = self.project.clone();
                self.changing = BTreeSet::from([send(NoticeAction::ReadAll { project_id }, ctx)]);
            }
            Key::Char('f') => self.walk_filter(true, ctx),
            Key::Char('p') => self.walk_project(true, ctx),
            Key::Char('P') => self.walk_project(false, ctx),
            Key::Char('s') => return Flow::Open(Place::NotificationSettings),
            Key::Esc if !self.marked.is_empty() => {
                self.marked.clear();
                self.changing.clear();
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        self.walk_filter(forward, ctx);
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => {
                if let OwnerEvent::Notices(event) = event.as_ref() {
                    if self.asked.as_ref() == Some(&event.query) {
                        self.asked = None;
                        self.take_page(event);
                        if std::mem::take(&mut self.stale) {
                            self.read(None, ctx);
                        }
                    } else {
                        self.take_change(event, ctx);
                    }
                }
            }
            Arrival::Disconnected(_) => {
                self.asked = None;
                self.stale = false;
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.rows().len() <= PAGE_MAX as usize {
            self.read(None, ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width);
        let room = usize::from(inner.height).saturating_sub(HEAD_ROWS);
        let mut lines = vec![pad(self.head(ctx), width, ctx.glyphs), Line::default()];
        match &self.items {
            Load::Ready(rows) if rows.is_empty() => {
                let empty = FILTERS
                    .iter()
                    .find(|(filter, _, _)| *filter == self.filter)
                    .map_or("", |(_, _, empty)| empty);
                lines.push(Line::styled(empty, ctx.theme.muted));
            }
            Load::Ready(rows) => {
                if self.cursor < self.top {
                    self.top = self.cursor;
                } else if room > 0 && self.cursor >= self.top + room {
                    self.top = self.cursor + 1 - room;
                }
                let label_width = NoticeKind::ALL
                    .iter()
                    .map(|kind| kind.label().chars().count())
                    .max()
                    .unwrap_or(0);
                for (index, row) in rows.iter().enumerate().skip(self.top).take(room) {
                    let content = self.row(row, width.saturating_sub(2), label_width, ctx);
                    lines.push(list_row(
                        content,
                        width,
                        index == self.cursor,
                        true,
                        &ctx.theme,
                        ctx.glyphs,
                    ));
                }
            }
            Load::Failed(code) => {
                lines.push(Line::styled(
                    "Could not read the notifications.",
                    ctx.theme.warning,
                ));
                lines.push(Line::styled(
                    super::settings_accounts::plain(code),
                    ctx.theme.muted,
                ));
            }
            Load::Idle | Load::Loading | Load::NotReady => {
                if !ctx.quiet {
                    lines.push(Line::styled(
                        format!("Loading notifications{}", ctx.glyphs.ellipsis),
                        ctx.theme.muted,
                    ));
                }
            }
        }
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Notifications)
    }
}

#[cfg(all(test, unix))]
mod tests;
