use super::{back_to, crumbs_of, draw_form, place_of, plugin_plain};
use crate::coord::tool_switches::Switch;
use crate::plugins::ToolView;
use crate::protocol::ProjectId;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::new_project_view::printable;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{cells, cut, join, list_row, styled, thumb, with_bar};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const TAB_ROWS: usize = 2;
const HEAD_ROWS: usize = 3;
const NAME_WIDTH: usize = 24;
const STATE_WIDTH: usize = 20;
const BUILTIN_NAME: &str = "CrazyAgent tools";

/// The switch a tool has where the page shows it: in a project its own switch, else the
/// every-project one. Inputs: the tool and whether the page is a project's. Output: the switch.
pub fn effective(tool: &ToolView, in_project: bool) -> Switch {
    if in_project {
        tool.here.unwrap_or(tool.global)
    } else {
        tool.global
    }
}

/// Whether any tool has a switch stored in the page's scope, so Restore defaults clears
/// something. Inputs: the tools and whether the page is a project's. Output: the answer.
pub fn changed_here(tools: &[ToolView], in_project: bool) -> bool {
    tools.iter().any(|tool| {
        if in_project {
            tool.here.is_some()
        } else {
            tool.global != Switch::On
        }
    })
}

/// One server's tool switches, from Settings or from one project's Settings tab: the
/// CrazyAgent built-in tools or one MCP server's checked tools, each On or Off, with Delete
/// for a built-in tool and Restore defaults. Tab moves between the servers it was opened with.
/// Input: keys and the engine's tool lists. Output: the page; only a read for the same project
/// and server lands, so a project page never holds an every-project read.
pub struct ToolsPage {
    project: Option<(ProjectId, String)>,
    servers: Vec<(Option<String>, String)>,
    at: usize,
    load: Load<Vec<ToolView>>,
    waiting: bool,
    changing: Option<String>,
    selected: usize,
    top: usize,
    window: usize,
}

impl ToolsPage {
    /// The page with nothing read yet: the CrazyAgent built-in tools first, then each MCP
    /// server given, starting on the server at `at`. Inputs: the MCP servers as id and name,
    /// and the index to start on, where 0 is the built-in tools. Output: the screen.
    pub fn new(servers: Vec<(String, String)>, at: usize) -> Self {
        let mut list = vec![(None, BUILTIN_NAME.to_owned())];
        list.extend(servers.into_iter().map(|(id, name)| (Some(id), name)));
        let at = at.min(list.len() - 1);
        Self {
            project: None,
            servers: list,
            at,
            load: Load::Idle,
            waiting: false,
            changing: None,
            selected: 0,
            top: 0,
            window: 1,
        }
    }

    /// The same page opened from one project, showing and switching the tools there.
    /// Inputs: the project id and name. Output: the screen.
    pub fn for_project(mut self, project: ProjectId, name: String) -> Self {
        self.project = Some((project, name));
        self
    }

    fn project_id(&self) -> Option<ProjectId> {
        self.project.as_ref().map(|(project, _)| project.clone())
    }

    fn server_id(&self) -> Option<String> {
        self.servers[self.at].0.clone()
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if !matches!(self.load, Load::Ready(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        send(
            ctx,
            SettingsJob::Tools {
                project: self.project_id(),
                server: self.server_id(),
            },
        );
    }

    fn shown_tools(&self) -> Vec<&ToolView> {
        let in_project = self.project.is_some();
        match &self.load {
            Load::Ready(tools) => tools
                .iter()
                .filter(|tool| effective(tool, in_project) != Switch::Hidden)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn picked(&self) -> Option<ToolView> {
        self.shown_tools()
            .get(self.selected)
            .map(|tool| (*tool).clone())
    }

    fn change(&mut self, job: SettingsJob, done: String, ctx: &mut Ctx) {
        if self.changing.is_some() || self.waiting {
            return;
        }
        self.changing = Some(done);
        send(ctx, job);
    }

    fn set(&mut self, tool: &ToolView, switch: Switch, done: String, ctx: &mut Ctx) {
        let job = SettingsJob::SetTool {
            project: self.project_id(),
            server: self.server_id(),
            tool: tool.tool.clone(),
            switch,
        };
        self.change(job, done, ctx);
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Some(tool) = self.picked() else {
            return;
        };
        let on = effective(&tool, self.project.is_some()) == Switch::On;
        let next = if on { Switch::Off } else { Switch::On };
        let place = if self.project.is_some() {
            "in this project"
        } else {
            "for every project"
        };
        let word = if on { "off" } else { "on" };
        let done = format!("{} is {word} {place}.", printable(tool.tool.clone()));
        self.set(&tool, next, done, ctx);
    }

    fn hide(&mut self, ctx: &mut Ctx) {
        let Some(tool) = self.picked() else {
            return;
        };
        if !tool.builtin {
            ctx.say(Message::error(
                "Only a CrazyAgent tool can be deleted.",
                "Turn an MCP server's tool off instead.",
            ));
            return;
        }
        let done = format!(
            "{} deleted. Restore defaults brings it back.",
            printable(tool.tool.clone())
        );
        self.set(&tool, Switch::Hidden, done, ctx);
    }

    fn restore(&mut self, ctx: &mut Ctx) {
        let Load::Ready(tools) = &self.load else {
            return;
        };
        if !changed_here(tools, self.project.is_some()) {
            ctx.say(Message::success(
                "Every tool here already uses its default.",
                "",
            ));
            return;
        }
        let job = SettingsJob::RestoreTools {
            project: self.project_id(),
            server: self.server_id(),
        };
        self.change(job, "Tool switches restored.".to_owned(), ctx);
    }

    fn switch_server(&mut self, forward: bool, ctx: &mut Ctx) {
        if self.servers.len() < 2 || self.waiting || self.changing.is_some() {
            return;
        }
        let count = self.servers.len();
        self.at = if forward {
            (self.at + 1) % count
        } else {
            (self.at + count - 1) % count
        };
        self.load = Load::Idle;
        self.selected = 0;
        self.top = 0;
        self.reload(ctx);
    }

    /// The body lines of the page, before scrolling, with the row of the selected tool.
    /// Inputs: the content width and context. Output: the lines and the selected line, if any.
    pub fn lines(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<usize>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let in_project = self.project.is_some();
        let name = printable(self.servers[self.at].1.clone());
        let place = if in_project {
            "On or off for this project. A switch here overrides the one in Settings."
        } else {
            "On or off for every project. Each project can override a switch."
        };
        let mut lines = vec![
            join([
                styled(name, Role::Accent, theme),
                styled(
                    format!("  {} of {}", self.at + 1, self.servers.len()),
                    Role::Muted,
                    theme,
                ),
            ]),
            styled(cut(place, limit, glyphs.ellipsis), Role::Muted, theme),
            Line::default(),
        ];
        assert_eq!(lines.len(), HEAD_ROWS);
        let tools = match &self.load {
            Load::Ready(tools) => tools,
            Load::Failed(code) => {
                lines.push(Line::raw(plugin_plain(code)));
                lines.push(Line::raw("Press r to try again."));
                return (lines, None);
            }
            Load::NotReady => {
                lines.push(Line::raw("Tool switches are not ready in this build."));
                return (lines, None);
            }
            Load::Idle | Load::Loading => {
                lines.push(styled("Reading the tools...", Role::Muted, theme));
                return (lines, None);
            }
        };
        let shown = self.shown_tools();
        let hidden = tools.len() - shown.len();
        if shown.is_empty() {
            lines.push(styled(
                if tools.is_empty() {
                    "No tools yet. Check the server in its details to list its tools."
                } else {
                    "Every tool here is deleted. Press r to restore the defaults."
                },
                Role::Muted,
                theme,
            ));
        }
        let widths = [
            NAME_WIDTH,
            STATE_WIDTH,
            limit
                .saturating_sub(2 + NAME_WIDTH + STATE_WIDTH + 4)
                .max(8),
        ];
        let first = lines.len();
        for (index, tool) in shown.iter().enumerate() {
            let on = effective(tool, in_project) == Switch::On;
            let state = match (on, in_project && tool.here.is_some()) {
                (true, true) => styled("On here", Role::Success, theme),
                (true, false) => styled("On", Role::Success, theme),
                (false, true) => styled("Off here", Role::Muted, theme),
                (false, false) => styled("Off", Role::Muted, theme),
            };
            let values = vec![
                Line::raw(printable(tool.tool.clone())),
                state,
                styled(printable(tool.description.clone()), Role::Muted, theme),
            ];
            lines.push(list_row(
                cells(&widths, values, 2, glyphs),
                limit,
                index == self.selected,
                true,
                theme,
                glyphs,
            ));
        }
        if hidden > 0 {
            lines.push(Line::default());
            lines.push(styled(
                format!(
                    "{hidden} deleted. Press r to restore the defaults and bring {} back.",
                    if hidden == 1 { "it" } else { "them" }
                ),
                Role::Muted,
                theme,
            ));
        }
        let selected = (!shown.is_empty()).then_some(first + self.selected);
        (lines, selected)
    }
}

impl Screen for ToolsPage {
    fn crumbs(&self) -> Vec<String> {
        crumbs_of(self.project.as_ref(), "Tools")
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        let mut hints = vec![
            hint("\u{2191}\u{2193}", "Move"),
            hint(
                "Space",
                if self.project.is_some() {
                    "On or off here"
                } else {
                    "On or off"
                },
            ),
        ];
        if self.picked().is_some_and(|tool| tool.builtin) {
            hints.push(hint("d", "Delete"));
        }
        hints.push(hint("r", "Restore defaults"));
        if self.servers.len() > 1 {
            hints.push(hint("Tab", "Next server"));
        }
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.shown_tools().len();
        match key {
            Key::Esc => return back_to(self.project.as_ref(), ctx),
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Char('r') => self.restore(ctx),
            Key::Char(' ') => self.toggle(ctx),
            Key::Char('d') => self.hide(ctx),
            Key::Tab => self.switch_server(true, ctx),
            Key::BackTab => self.switch_server(false, ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            Key::Home => self.selected = 0,
            Key::End => self.selected = count.saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::Tools {
            project,
            server,
            changed,
            result,
        }) = settings_event(arrival)
        else {
            return Flow::Handled;
        };
        if *project != self.project_id() || *server != self.server_id() {
            return Flow::Handled;
        }
        if *changed {
            let Some(done) = self.changing.take() else {
                return Flow::Handled;
            };
            match result {
                Ok(tools) => {
                    self.load = Load::Ready(tools.clone());
                    ctx.say(Message::success(&done, ""));
                }
                Err(code) => {
                    ctx.say(Message::error(plugin_plain(code), ""));
                    self.reload(ctx);
                }
            }
        } else {
            if !std::mem::take(&mut self.waiting) {
                return Flow::Handled;
            }
            match result {
                Ok(tools) => self.load = Load::Ready(tools.clone()),
                Err(code) if matches!(self.load, Load::Ready(_)) => ctx.say(Message::error(
                    "Could not refresh the tools.",
                    plugin_plain(code),
                )),
                Err(code) => self.load = Load::Failed(code.clone()),
            }
        }
        self.selected = self
            .selected
            .min(self.shown_tools().len().saturating_sub(1));
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4 + 1).max(8);
        let (lines, selected) = self.lines(limit, ctx);
        let tabs = if self.project.is_some() { 0 } else { TAB_ROWS };
        let window = (area.height as usize).saturating_sub(tabs).max(1);
        self.window = window;
        if let Some(row) = selected {
            if self.selected == 0 {
                self.top = 0;
            } else if row < self.top {
                self.top = row;
            } else if row >= self.top + window {
                self.top = row + 1 - window;
            }
        }
        self.top = self.top.min(lines.len().saturating_sub(window));
        let total = lines.len();
        let shown: Vec<Line<'static>> = lines.into_iter().skip(self.top).take(window).collect();
        let content = if total > window {
            let (top, size) = thumb(window, self.top, total);
            with_bar(shown, limit + 1, top, size, &ctx.theme, ctx.glyphs)
        } else {
            shown
        };
        assert!(content.len() <= window);
        draw_form(frame, area, self.project.is_some(), content, None, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting || self.changing.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(place_of(self.project.as_ref()))
    }
}
