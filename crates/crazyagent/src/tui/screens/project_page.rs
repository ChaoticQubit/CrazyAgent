use super::new_project_page::{hints_of, key_event, look, paint, pool_choices_from};
use super::new_project_view::{Row, Screen as View};
use super::project::{ProjectReply, ProjectScreen, Tab, plain_problem, project_pool};
use super::project_time::local_offset;
use super::project_view::project_screen;
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::project_setup::SetupQuery;
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::client::OwnerEvent;
use crate::tui::clock::now_ms;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, PoolChange, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::{Path, PathBuf};

const SENT_MAX: usize = 8;

fn tab_of(tab: ProjectTab) -> Tab {
    Tab::ALL[ProjectTab::ALL
        .iter()
        .position(|each| *each == tab)
        .expect("every tab is listed")]
}

fn project_tab(tab: Tab) -> ProjectTab {
    ProjectTab::ALL[Tab::ALL
        .iter()
        .position(|each| *each == tab)
        .expect("every tab is listed")]
}

/// Opens a project folder or file with the system's default app.
/// Takes an absolute path from the engine's project record.
/// Returns the plain problem when the path is not absolute, does not exist or nothing opened it.
pub(in crate::tui) fn open_path(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.is_absolute() || !target.exists() {
        return Err("That file or folder is not there any more.".into());
    }
    let opener = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(opener)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "Could not open it on this computer.".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open it on this computer.".into())
    }
}

/// One project as a shell screen.
/// Holds the project state, its last drawn view and the engine reads it sent.
/// Reads go one at a time through the shell queue; only replies to its own reads are applied.
pub(in crate::tui) struct ProjectPage {
    screen: ProjectScreen,
    view: View,
    sent: Vec<SetupQuery>,
    assigning: Option<String>,
}

impl ProjectPage {
    /// Opens a project on one tab.
    /// Takes the project id and name from Home, the tab and the shell context.
    /// Returns the screen; engine reads start when it is shown.
    pub fn new(project: ProjectId, name: String, tab: ProjectTab, ctx: &Ctx) -> Self {
        let mut screen = ProjectScreen::new(project, name, ctx.home.as_ref().map(PathBuf::from));
        screen.open(tab_of(tab));
        let now = now_ms();
        screen.now_ms = now;
        screen.offset = local_offset(now);
        let mut page = Self {
            screen,
            view: View::default(),
            sent: Vec::new(),
            assigning: None,
        };
        page.refresh_pool(ctx);
        page.redraw(ctx);
        page
    }

    fn refresh_pool(&mut self, ctx: &Ctx) {
        let (Some(pools), Some(scopes)) =
            (ctx.store.pools.ready(), ctx.store.pool_projects.ready())
        else {
            self.screen.pool_ready = false;
            return;
        };
        let picked = self
            .screen
            .setup
            .as_ref()
            .and_then(|setup| setup.pool.as_ref());
        self.screen.pool = project_pool(pools, scopes, &self.screen.project, picked);
        self.screen.set_pool_choices(pool_choices_from(pools));
        self.screen.pool_ready = true;
    }

    fn assign(&mut self, pool: CredentialRef, ctx: &mut Ctx) {
        if self.assigning.is_some() {
            ctx.say(Message::warning(
                "Still assigning the pool.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        let name = self
            .screen
            .pool_choices
            .iter()
            .find(|choice| choice.id == pool)
            .map_or_else(|| "The pool".to_owned(), |choice| choice.name.clone());
        self.assigning = Some(name);
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool,
                project: self.screen.project.clone(),
            },
        });
    }

    fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        let Some(name) = self.assigning.take() else {
            return;
        };
        match result {
            Ok(()) => ctx.say(Message::success(
                &format!("{name} is assigned."),
                "Work can start now.",
            )),
            Err(code) => ctx.say(Message::error(
                "The pool was not assigned.",
                &plain_problem(code),
            )),
        }
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
    }

    fn redraw(&mut self, ctx: &Ctx) {
        self.screen.width = ctx.width;
        self.view = project_screen(&mut self.screen, ctx.width, ctx.height, &look(ctx));
    }

    fn pump(&mut self, ctx: &mut Ctx) {
        if self.sent.len() >= SENT_MAX {
            return;
        }
        if let Some(query) = self.screen.next_query(now_ms()) {
            self.sent.push(query.clone());
            ctx.request(Request::Setup(query));
        }
    }

    fn send(&mut self, query: SetupQuery, ctx: &mut Ctx) {
        if self.sent.len() >= SENT_MAX {
            ctx.say(Message::warning(
                "Still working.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        self.sent.push(query.clone());
        ctx.request(Request::Setup(query));
    }

    fn reply(&mut self, reply: ProjectReply, ctx: &mut Ctx) -> Flow {
        let flow = match reply {
            ProjectReply::Ignored => Flow::Ignored,
            ProjectReply::Handled => Flow::Handled,
            ProjectReply::Home => Flow::Back,
            ProjectReply::Change(action) => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::Change {
                        action: *action,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Review {
                entry,
                version,
                decision,
            } => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::ReviewMemory {
                        project: self.screen.project.clone(),
                        entry_id: entry,
                        version,
                        decision,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Open(path) => {
                match open_path(&path) {
                    Ok(()) => ctx.say(Message::success("Opened.", "")),
                    Err(problem) => ctx.say(Message::error(&problem, "")),
                }
                Flow::Handled
            }
            ProjectReply::Assign(pool) => {
                self.assign(pool, ctx);
                Flow::Handled
            }
            ProjectReply::PickPool => {
                ctx.say(Message::warning(
                    "There are no pools yet.",
                    "Create one here, then assign it in the project's Settings tab.",
                ));
                Flow::Open(Place::Settings(SettingsSection::Pools))
            }
            ProjectReply::Send(_) | ProjectReply::StopTurn | ProjectReply::Retry => {
                ctx.say(Message::warning(
                    "Not ready yet.",
                    "This part of the project screen is still being connected.",
                ));
                Flow::Handled
            }
        };
        self.pump(ctx);
        self.redraw(ctx);
        flow
    }
}

impl Screen for ProjectPage {
    fn crumbs(&self) -> Vec<String> {
        self.view.crumbs.clone()
    }

    fn status(&self, _ctx: &Ctx) -> Line<'static> {
        self.view.status.clone().map(Row::line).unwrap_or_default()
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        hints_of(&self.view)
    }

    fn typing(&self) -> bool {
        self.screen.edit.is_some() || (self.screen.tab == Tab::Conversation && self.screen.typing)
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        self.screen.width = ctx.width;
        let reply = match key {
            Key::Paste(text) => self.screen.paste(text),
            other => match key_event(other) {
                Some(event) => self.screen.key(event),
                None => ProjectReply::Ignored,
            },
        };
        self.reply(reply, ctx)
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        let code = if forward {
            KeyCode::Right
        } else {
            KeyCode::Left
        };
        let reply = self.screen.key(KeyEvent::new(code, KeyModifiers::SHIFT));
        self.reply(reply, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let event = match arrival {
            Arrival::Owner(event) => event,
            Arrival::PoolEdited(result) => {
                self.assigned(result, ctx);
                self.redraw(ctx);
                return Flow::Handled;
            }
            Arrival::Pools(_) | Arrival::PoolProjects(_) => {
                self.refresh_pool(ctx);
                self.redraw(ctx);
                return Flow::Handled;
            }
            _ => return Flow::Handled,
        };
        let OwnerEvent::Setup(event) = event.as_ref() else {
            return Flow::Handled;
        };
        let Some(index) = self.sent.iter().position(|query| query == &event.query) else {
            return Flow::Handled;
        };
        let query = self.sent.remove(index);
        self.screen.now_ms = now_ms();
        self.screen.apply(&query, event.result.clone());
        self.refresh_pool(ctx);
        self.pump(ctx);
        self.redraw(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
        self.refresh_pool(ctx);
        self.pump(ctx);
        self.redraw(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        self.screen.width = area.width;
        self.screen.now_ms = now_ms();
        self.view = project_screen(
            &mut self.screen,
            area.width,
            area.height.saturating_add(4),
            &look(ctx),
        );
        paint(frame, area, &self.view)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.screen.counts().0 > 0
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Project {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
            tab: project_tab(self.screen.tab),
        })
    }
}
