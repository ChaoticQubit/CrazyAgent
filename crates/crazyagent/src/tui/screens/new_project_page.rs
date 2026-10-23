use super::new_project::{Phase, PoolChoice, Reply, Wizard};
use super::new_project_view::{Look, Row, Screen as View, wizard_screen};
use super::{Flow, Place, Screen, SettingsSection};
use crate::pools::{MemberKind, PoolSummary};
use crate::project_setup::{SetupQuery, SetupView};
use crate::tui::client::OwnerEvent;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::PathBuf;

/// Gives the wizard and project views the shell's colors and glyph set.
/// Takes the shell context.
/// Returns the look with one style per design role.
pub(in crate::tui) fn look(ctx: &Ctx) -> Look {
    let theme = &ctx.theme;
    Look {
        fg: theme.text,
        heading: theme.heading,
        muted: theme.muted,
        accent: theme.accent,
        mark: theme.mark,
        success: theme.success,
        warning: theme.warning,
        error: theme.error,
        running: theme.running,
        selection: theme.selection,
        selection_dim: theme.selection_dim,
        backdrop: theme.backdrop,
        ascii: ctx.glyphs.ascii,
    }
}

/// Turns one shell key into the key event the wizard and project views take.
/// Takes the normalized shell key.
/// Returns the event, or none for a paste, which goes to `paste` instead.
pub(in crate::tui) fn key_event(key: &Key) -> Option<KeyEvent> {
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let shifted = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
    Some(match key {
        Key::Char(character) => plain(KeyCode::Char(*character)),
        Key::Ctrl(letter) => KeyEvent::new(KeyCode::Char(*letter), KeyModifiers::CONTROL),
        Key::Enter => plain(KeyCode::Enter),
        Key::Esc => plain(KeyCode::Esc),
        Key::Tab => plain(KeyCode::Tab),
        Key::BackTab => shifted(KeyCode::BackTab),
        Key::Backspace => plain(KeyCode::Backspace),
        Key::Delete => plain(KeyCode::Delete),
        Key::Left => plain(KeyCode::Left),
        Key::Right => plain(KeyCode::Right),
        Key::Up => plain(KeyCode::Up),
        Key::Down => plain(KeyCode::Down),
        Key::Home => plain(KeyCode::Home),
        Key::End => plain(KeyCode::End),
        Key::PageUp => plain(KeyCode::PageUp),
        Key::PageDown => plain(KeyCode::PageDown),
        Key::ShiftLeft => shifted(KeyCode::Left),
        Key::ShiftRight => shifted(KeyCode::Right),
        Key::Paste(_) => return None,
    })
}

/// Draws a rendered view body into the shell's body area.
/// Takes the frame, the body area and the view.
/// Returns the cursor position on screen when a field has focus inside the area.
pub(in crate::tui) fn paint(frame: &mut Frame, area: Rect, view: &View) -> Option<Position> {
    let lines: Vec<Line<'static>> = view.rows.iter().cloned().map(Row::line).collect();
    draw_lines(frame.buffer_mut(), area, &lines);
    view.cursor
        .filter(|(x, y)| *x < area.width && *y < area.height)
        .map(|(x, y)| Position::new(area.x + x, area.y + y))
}

/// Turns the view's help keys into shell hints.
/// Takes the view.
/// Returns the hints in the view's priority order.
pub(in crate::tui) fn hints_of(view: &View) -> Vec<Hint> {
    view.help
        .iter()
        .map(|(key, label)| hint(key, label))
        .collect()
}

/// Builds the wizard's pool choices from the engine's pool list.
/// Takes the pool summaries; ChatGPT accounts run on Codex and Claude accounts on Claude, other accounts have no harness count.
/// Returns one choice per pool in list order.
pub(in crate::tui) fn pool_choices_from(pools: &[PoolSummary]) -> Vec<PoolChoice> {
    pools
        .iter()
        .map(|pool| {
            let mut harnesses: Vec<(String, usize)> = Vec::new();
            for member in &pool.members {
                let harness = match member.kind {
                    MemberKind::Chatgpt => "codex",
                    MemberKind::Claude => "claude",
                    _ => continue,
                };
                match harnesses.iter_mut().find(|(known, _)| known == harness) {
                    Some((_, count)) => *count += 1,
                    None => harnesses.push((harness.to_owned(), 1)),
                }
            }
            PoolChoice {
                id: pool.id.clone(),
                name: pool.name.clone(),
                accounts: pool.members.len(),
                harnesses,
            }
        })
        .collect()
}

/// The New project wizard as a shell screen.
/// Holds the wizard, its last drawn view and the catalog read it waits for.
/// Engine reads go through the shell queue; creation waits for the owner connection identity.
pub(in crate::tui) struct NewProjectPage {
    wizard: Wizard,
    view: View,
}

impl NewProjectPage {
    /// Opens an empty wizard with the pools the shell already has.
    /// Takes the shell context for pools, the home folder and the size.
    /// Returns the screen, drawn once so the header and help line are current.
    pub fn new(ctx: &Ctx) -> Self {
        let pools = ctx
            .store
            .pools
            .ready()
            .map(|pools| pool_choices_from(pools))
            .unwrap_or_default();
        let home = ctx.home.as_ref().map(PathBuf::from);
        let mut page = Self {
            wizard: Wizard::new(pools, home),
            view: View::default(),
        };
        page.redraw(ctx);
        page
    }

    fn redraw(&mut self, ctx: &Ctx) {
        self.wizard.width = ctx.width;
        self.view = wizard_screen(&self.wizard, ctx.width, ctx.height, &look(ctx));
    }

    fn ask_catalog(&mut self, ctx: &mut Ctx) {
        if let Some((pool, research)) = self.wizard.catalog_request() {
            ctx.request(Request::Setup(SetupQuery::Catalog { pool, research }));
        }
    }

    fn reply(&mut self, reply: Reply, ctx: &mut Ctx) -> Flow {
        let flow = match reply {
            Reply::Ignored => Flow::Ignored,
            Reply::Handled => Flow::Handled,
            Reply::Leave | Reply::Background => Flow::Back,
            Reply::CreatePool => Flow::Open(Place::NewPool),
            Reply::CheckHarnesses => Flow::Open(Place::Settings(SettingsSection::Harnesses)),
            Reply::Create(_) => {
                self.wizard.failed("creation_not_ready");
                Flow::Handled
            }
        };
        self.ask_catalog(ctx);
        self.redraw(ctx);
        flow
    }

    fn set_pools(&mut self, pools: &[PoolSummary]) {
        let first = self.wizard.pools.is_empty() && self.wizard.pool.is_none();
        self.wizard.set_pools(pool_choices_from(pools));
        if first {
            self.wizard.pool = self.wizard.pools.first().map(|pool| pool.id.clone());
        }
    }
}

impl Screen for NewProjectPage {
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
        self.wizard.typing || self.wizard.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        self.wizard.width = ctx.width;
        let reply = match key {
            Key::Paste(text) => self.wizard.paste(text),
            other => match key_event(other) {
                Some(event) => self.wizard.key(event),
                None => Reply::Ignored,
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
        let reply = self.wizard.key(KeyEvent::new(code, KeyModifiers::SHIFT));
        self.reply(reply, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Pools(Ok(pools)) => self.set_pools(pools),
            Arrival::Owner(event) => {
                if let OwnerEvent::Setup(event) = event.as_ref()
                    && let SetupQuery::Catalog { pool, research } = &event.query
                {
                    let result = match &event.result {
                        Ok(SetupView::Catalog(catalog)) => Ok(catalog.as_ref().clone()),
                        Ok(_) => Err("project_catalog_unavailable".to_owned()),
                        Err(problem) => Err(problem.clone()),
                    };
                    self.wizard
                        .apply_catalog(pool.clone(), research.clone(), result);
                }
            }
            _ => return Flow::Handled,
        }
        self.ask_catalog(ctx);
        self.redraw(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        self.ask_catalog(ctx);
        self.redraw(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        self.wizard.width = area.width;
        self.view = wizard_screen(
            &self.wizard,
            area.width,
            area.height.saturating_add(4),
            &look(ctx),
        );
        paint(frame, area, &self.view)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.wizard.phase, Phase::Creating { .. })
    }

    fn draft(&self) -> bool {
        !self.wizard.name.value().trim().is_empty()
            || !self.wizard.about.value().trim().is_empty()
            || !self.wizard.documents.is_empty()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::NewProject)
    }
}
