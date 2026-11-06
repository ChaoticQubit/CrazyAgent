use super::new_project_page::{key_event, look};
use super::new_project_view::Row;
use super::project_team::{TABS, TeamReply, TeamView};
use super::{Flow, Place, Screen};
use crate::project_setup::{SetupAction, SetupQuery};
use crate::protocol::ProjectId;
use crate::team::TeamAction;
use crate::tui::client::OwnerEvent;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::{local_offset, now_ms};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const GUTTER: u16 = 2;

/// One project's team settings as their own screen, opened with s from the project's
/// Agents tab or its Team screen: the task and session limits, the idle stop, the On
/// harness switch setting, keep warm and the wake rules, beside the live team.
/// Takes engine reads through `arrive` and keys through `key`; never invents data.
pub(in crate::tui) struct TeamSettings {
    name: String,
    view: TeamView,
}

impl TeamSettings {
    /// Opens the team settings of one project with nothing read yet.
    /// Takes the project and its name.
    /// Returns the screen; the first team read goes out when it is shown.
    pub fn new(project: ProjectId, name: String) -> Self {
        let mut view = TeamView::new(project);
        view.offset = local_offset();
        Self { name, view }
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.view.wants_catalog() {
            ctx.request(Request::Settings(Box::new(SettingsJob::Harnesses)));
        }
        if let Some(query) = self.view.next_query(now_ms())
            && !ctx.try_request(Request::Setup(query))
        {
            self.view.forget_reads();
        }
    }

    fn change(&mut self, action: TeamAction, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action,
            generation,
        }));
    }

    /// Sends a project setup save from the settings, as the master harness picker gives it,
    /// as `SetupQuery::Change`; a full request queue answers it as the owner being busy.
    /// Takes the save and the context. Returns nothing.
    fn save(&mut self, action: SetupAction, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::Change { action, generation }));
    }
}

impl Screen for TeamSettings {
    fn crumbs(&self) -> Vec<String> {
        vec![self.name.clone(), "Team settings".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        self.view
            .help()
            .into_iter()
            .filter(|item| *item != TABS)
            .map(|item| match item {
                ("Esc", "Home") => hint("Esc", "Back"),
                (key, label) => hint(key, label),
            })
            .collect()
    }

    fn typing(&self) -> bool {
        self.view.typing()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let reply = match key {
            Key::Paste(text) => {
                if self.view.paste(text) {
                    TeamReply::Handled
                } else {
                    TeamReply::Ignored
                }
            }
            other => key_event(other).map_or(TeamReply::Ignored, |event| self.view.key(event)),
        };
        match reply {
            TeamReply::Ignored => return Flow::Ignored,
            TeamReply::Handled => {}
            TeamReply::Change(action) => self.change(action, ctx),
            TeamReply::Save(action) => self.save(action, ctx),
        }
        self.read(ctx);
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Setup(event) => {
                    self.view.apply(&event.query, event.result.clone());
                    self.read(ctx);
                }
                OwnerEvent::Settings(event) => {
                    if let SettingsEvent::Harnesses(result) = event.as_ref() {
                        self.view.take_catalog(result);
                    }
                }
                _ => {}
            },
            Arrival::Disconnected(_) => self.view.forget_reads(),
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        self.read(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let accounts = ctx.store.accounts.ready().map_or(&[][..], Vec::as_slice);
        let (rows, _, caret) = self.view.draw(
            usize::from(inner.width),
            usize::from(inner.height),
            accounts,
            &look(ctx),
        );
        let lines: Vec<Line<'static>> = rows.into_iter().map(Row::line).collect();
        draw_lines(frame.buffer_mut(), inner, &lines);
        let (row, column) = caret?;
        let (x, y) = (u16::try_from(column).ok()?, u16::try_from(row).ok()?);
        (x < inner.width && y < inner.height).then(|| Position::new(inner.x + x, inner.y + y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.view.busy()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::TeamSettings {
            id: self.view.project.clone(),
            name: self.name.clone(),
        })
    }
}
