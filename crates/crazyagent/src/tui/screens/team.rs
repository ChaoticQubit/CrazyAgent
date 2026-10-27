use crate::agent_home::Harness;
use crate::config::plain_problem;
use crate::engine::owner::AgentInstructions;
use crate::project_setup::SetupQuery;
use crate::protocol::ProjectId;
use crate::team::{AgentStatus, EdgeState, GraphNode, NodeKind, TaskSource, TeamAction, TeamGraph};
use crate::terminal::{AgentId, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame};
use crate::tui::client::OwnerEvent;
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::glyphs::{Glyphs, Status};
use crate::tui::keys::Key;
use crate::tui::screens::process::{message, size};
use crate::tui::screens::project_markdown::sanitize;
use crate::tui::screens::project_team::account_name;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::terminal_pane::{LEADER, TerminalPane};
use crate::tui::text::{cut, join, list_row, pad, styled, width, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

mod cleanup;
#[cfg(test)]
mod tests;

const MARGIN: u16 = 2;
const CHROME_ROWS: u16 = 4;
const HEADER_ROWS: u16 = 2;
const TREE_MIN: u16 = 30;
const TREE_MAX: u16 = 44;
const BOX_MIN: u16 = 8;
const ENDED: &str = "terminal_ended";
const STOPPED: &str = "agent_stopped";
const PAGE_STEP: usize = 10;
const MASTER_WAITING: &str =
    "Starting the master's terminal. It opens once the master signs in and starts.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Tree,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Node(usize),
    Edge(usize),
}

enum Body {
    Master(usize),
    Group { node: usize, count: usize },
    Agent(usize),
    Account(String),
    Heading(&'static str),
    Blank,
    Edge(usize),
}

struct TreeRow {
    item: Option<Item>,
    prefix: String,
    body: Body,
}

struct Attached {
    agent: AgentId,
    node: String,
    label: String,
    pane: TerminalPane,
    size: (u16, u16),
    started: bool,
    closed: Option<String>,
}

/// The read-only box with what one team card's agent received at its last launch.
struct Instructions {
    label: String,
    query: ProcessQuery,
    read: Load<Box<AgentInstructions>>,
    top: usize,
}

/// A project's live team: a boxed tree of the master, one group per harness
/// and each agent with its status and account, the tasks the master handed out
/// under it, and the selected agent's live terminal beside it.
/// Input: TeamGraph reads, terminal frames and keys; while the terminal has
/// focus every key goes to the agent until Ctrl-]. Output: TeamGraph,
/// TerminalAttach, TerminalInput, TerminalResize and TerminalDetach requests.
/// The master view is the same screen showing only the live master's terminal.
pub struct Team {
    project: ProjectId,
    name: String,
    master: bool,
    asked: bool,
    refused: Option<String>,
    graph: Load<Box<TeamGraph>>,
    cursor: usize,
    offset: usize,
    focus: Focus,
    attached: Option<Attached>,
    task: Option<i64>,
    cleanup: cleanup::Cleanup,
    instructions: Option<Instructions>,
}

impl Team {
    /// The Team screen of one project, waiting for its first graph read.
    /// Inputs: the project id and name. Output: the screen with the tree focused.
    pub fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            master: false,
            asked: false,
            refused: None,
            graph: Load::Loading,
            cursor: 0,
            offset: 0,
            focus: Focus::Tree,
            attached: None,
            task: None,
            cleanup: cleanup::Cleanup::new(),
            instructions: None,
        }
    }

    /// The master view of one project: the live master's own terminal filling
    /// the body, opened as soon as the team shows it. Showing it asks the
    /// engine to keep the master running live.
    /// Inputs: the project id and name. Output: the screen, waiting for the master.
    pub fn master(project: ProjectId, name: String) -> Self {
        Self {
            master: true,
            ..Self::new(project, name)
        }
    }

    fn ask_live(&mut self, ctx: &mut Ctx) {
        if !self.master || self.asked {
            return;
        }
        self.asked = true;
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::LiveMaster {
                project_id: self.project.clone(),
                on: true,
            },
            generation,
        }));
    }

    fn took_change(&mut self, query: &SetupQuery, result: &Result<impl Sized, String>) {
        let SetupQuery::TeamChange {
            action: TeamAction::LiveMaster { project_id, .. },
            ..
        } = query
        else {
            return;
        };
        if !self.master || *project_id != self.project {
            return;
        }
        self.refused = result.as_ref().err().cloned();
    }

    /// Opens the master's terminal once the team lists it, and the new one
    /// after the master moved to another account.
    /// Input: the context. Output: none; attach and size requests are queued.
    fn follow_master(&mut self, ctx: &mut Ctx) {
        if !self.master {
            return;
        }
        let Some((node, terminal)) = self.graph.ready().and_then(|graph| {
            graph.nodes.iter().enumerate().find_map(|(index, node)| {
                let live = node.status.is_none_or(|status| status.live());
                (node.kind == NodeKind::Master && live)
                    .then(|| node.terminal.clone().map(|terminal| (index, terminal)))
                    .flatten()
            })
        }) else {
            return;
        };
        let current = self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.agent == terminal && attached.closed.is_none());
        if !current {
            self.open(node, ctx);
        }
    }

    fn master_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let open = self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.closed.is_none());
        match key {
            Key::Enter | Key::Tab if open => self.focus = Focus::Terminal,
            Key::Char('r') => ctx.request(Request::TeamGraph {
                project: self.project.clone(),
            }),
            Key::Esc => {
                self.detach(ctx);
                return Flow::Back;
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn items(&self, glyphs: &Glyphs) -> Vec<Item> {
        self.graph
            .ready()
            .map(|graph| tree(graph, glyphs))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.item)
            .collect()
    }

    fn take_graph(&mut self, result: &Result<Box<TeamGraph>, String>, glyphs: &Glyphs) {
        match result {
            Ok(graph) => self.graph = Load::Ready(graph.clone()),
            Err(code) if code == NOT_READY => self.graph = Load::NotReady,
            Err(_) if self.graph.ready().is_some() => {}
            Err(code) => self.graph = Load::Failed(code.clone()),
        }
        let count = self.items(glyphs).len();
        self.cursor = self.cursor.min(count.saturating_sub(1));
        let known = self.task.is_some_and(|task| {
            self.graph
                .ready()
                .is_some_and(|graph| graph.edges.iter().any(|edge| edge.task_id == task))
        });
        if !known {
            self.task = None;
        }
    }

    fn stream(&mut self, agent: &AgentId, frame: &TerminalFrame) {
        let Some(attached) = self
            .attached
            .as_mut()
            .filter(|attached| attached.agent == *agent)
        else {
            return;
        };
        match frame {
            TerminalFrame::Attached { cols, rows, .. } => {
                attached.pane.resize(
                    (*rows).clamp(SIZE_MIN, SIZE_MAX),
                    (*cols).clamp(SIZE_MIN, SIZE_MAX),
                );
                attached.pane.show(&[], None);
                attached.started = true;
            }
            TerminalFrame::Output { bytes } => attached.pane.feed(bytes),
            TerminalFrame::Closed { reason } => {
                attached.closed = Some(reason.clone());
                self.focus = Focus::Tree;
            }
            TerminalFrame::Rejected { code } => {
                attached.closed = Some(code.clone());
                self.focus = Focus::Tree;
            }
        }
    }

    fn open(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        if target.status.is_some_and(|status| !status.live()) {
            self.task = None;
            self.instructions = None;
            ctx.say(Message::warning(
                "This agent has stopped.",
                "Its terminal is closed.",
            ));
            return;
        }
        let Some(agent) = target.terminal.clone() else {
            ctx.say(Message::warning(
                "This agent has no terminal yet.",
                "It gets one when it starts.",
            ));
            return;
        };
        let (id, label) = (target.id.clone(), target.label.clone());
        self.task = None;
        self.instructions = None;
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.agent == agent && attached.closed.is_none())
        {
            self.focus = Focus::Terminal;
            return;
        }
        self.detach(ctx);
        let (rows, cols) = pane_size(ctx, self.master);
        ctx.request(Request::TerminalAttach {
            agent: agent.clone(),
        });
        ctx.request(Request::TerminalResize {
            agent: agent.clone(),
            cols,
            rows,
        });
        self.attached = Some(Attached {
            agent,
            node: id,
            label,
            pane: TerminalPane::new(rows, cols),
            size: (rows, cols),
            started: false,
            closed: None,
        });
        self.focus = Focus::Terminal;
    }

    fn detach(&mut self, ctx: &mut Ctx) {
        if let Some(attached) = self.attached.take()
            && attached.closed.is_none()
        {
            ctx.request(Request::TerminalDetach {
                agent: attached.agent,
            });
        }
        self.focus = Focus::Tree;
    }

    /// Opens the attached terminal again after the engine connection came
    /// back; a terminal that ended stays closed.
    /// Input: the context. Output: none; attach and size requests are queued.
    fn reattach(&mut self, ctx: &mut Ctx) {
        let (rows, cols) = pane_size(ctx, self.master);
        let Some(attached) = self
            .attached
            .as_mut()
            .filter(|attached| attached.closed.as_deref().is_some_and(lost))
        else {
            return;
        };
        attached.closed = None;
        attached.size = (rows, cols);
        ctx.request(Request::TerminalAttach {
            agent: attached.agent.clone(),
        });
        ctx.request(Request::TerminalResize {
            agent: attached.agent.clone(),
            cols,
            rows,
        });
    }

    /// Closes the attached terminal once the team shows its agent stopped,
    /// so no key reaches a pane whose agent is gone.
    /// Input: the context. Output: none; a detach is queued for a live view.
    fn end_stopped(&mut self, ctx: &mut Ctx) {
        let graph = &self.graph;
        let Some(attached) = self.attached.as_mut() else {
            return;
        };
        let stopped = graph
            .ready()
            .and_then(|graph| graph.nodes.iter().find(|node| node.id == attached.node))
            .and_then(|node| node.status)
            .is_some_and(|status| !status.live());
        if !stopped || attached.closed.as_deref().is_some_and(dead) {
            return;
        }
        if attached.closed.is_none() {
            ctx.request(Request::TerminalDetach {
                agent: attached.agent.clone(),
            });
        }
        attached.closed = Some(STOPPED.into());
        self.focus = Focus::Tree;
    }

    fn show_task(&mut self, edge: usize) {
        self.task = self
            .graph
            .ready()
            .and_then(|graph| graph.edges.get(edge))
            .map(|edge| edge.task_id);
        self.instructions = None;
        self.focus = Focus::Tree;
    }

    /// Asks the engine what one card's agent received at its last launch and opens the
    /// box for it: the master card reads the project's newest master.
    /// Inputs: the node index and context. Output: none; a Process request is queued.
    fn read_instructions(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        let agent = match target.kind {
            NodeKind::Master => None,
            _ => match AgentId::new(&target.id).ok().and_then(|id| id.team_agent()) {
                Some(agent) => Some(agent),
                None => {
                    ctx.say(Message::warning(
                        "This card has no instructions to show.",
                        "Only the master and team agents get instructions at launch.",
                    ));
                    return;
                }
            },
        };
        let label = target.label.clone();
        let query = ProcessQuery::Agent {
            project: self.project.clone(),
            agent,
        };
        ctx.request(Request::Process(Box::new(query.clone())));
        self.task = None;
        self.instructions = Some(Instructions {
            label,
            query,
            read: Load::Loading,
            top: 0,
        });
    }

    fn take_instructions(&mut self, event: &ProcessEvent) {
        let Some(open) = self
            .instructions
            .as_mut()
            .filter(|open| open.query == event.query)
        else {
            return;
        };
        open.read = match &event.result {
            Ok(ProcessReply::Agent(view)) => Load::Ready(view.clone()),
            Ok(ProcessReply::Text(_)) => return,
            Err(code) if code == NOT_READY => Load::NotReady,
            Err(code) => Load::Failed(code.clone()),
        };
        open.top = 0;
    }

    fn sync(&mut self, ctx: &mut Ctx) {
        let (rows, cols) = pane_size(ctx, self.master);
        let Some(attached) = self.attached.as_mut() else {
            return;
        };
        if attached.size == (rows, cols) {
            return;
        }
        attached.size = (rows, cols);
        attached.pane.resize(rows, cols);
        if attached.closed.is_none() {
            ctx.request(Request::TerminalResize {
                agent: attached.agent.clone(),
                cols,
                rows,
            });
        }
    }

    fn terminal_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if *key == LEADER && self.master {
            self.detach(ctx);
            return Flow::Back;
        }
        if *key == LEADER {
            self.focus = Focus::Tree;
            return Flow::Handled;
        }
        let Some(attached) = self.attached.as_mut() else {
            self.focus = Focus::Tree;
            return Flow::Handled;
        };
        if attached.closed.is_some() {
            self.focus = Focus::Tree;
            return Flow::Handled;
        }
        for chunk in attached.pane.encode(key).chunks(INPUT_BYTES_MAX) {
            ctx.request(Request::TerminalInput {
                agent: attached.agent.clone(),
                bytes: chunk.to_vec(),
            });
        }
        Flow::Handled
    }

    fn tree_lines(&mut self, limit: usize, height: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let notice = |lead: Line<'static>, rest: &str| {
            let mut lines = vec![join([Line::raw("  "), lead])];
            lines.extend(
                wrap(rest, limit.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|text| join([Line::raw("  "), styled(text, Role::Muted, theme)])),
            );
            lines
        };
        let graph = match &self.graph {
            Load::Idle | Load::Loading => {
                return vec![join([Line::raw("  "), ctx.loading("Loading the team")])];
            }
            Load::NotReady => {
                return notice(
                    Line::raw("Not ready yet."),
                    "This engine cannot show the team yet.",
                );
            }
            Load::Failed(code) => {
                return notice(
                    styled(
                        format!("{} Failed to load the team.", glyphs.status(Status::Fail)),
                        Role::Error,
                        theme,
                    ),
                    plain_problem(code).unwrap_or("Press r to try again."),
                );
            }
            Load::Ready(graph) => graph,
        };
        if graph.nodes.is_empty() {
            return notice(
                Line::raw("No agents yet."),
                "Agents show here once the master starts work.",
            );
        }
        let rows = tree(graph, glyphs);
        let selected = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.item.is_some())
            .nth(self.cursor)
            .map_or(0, |(index, _)| index);
        if selected < self.offset {
            self.offset = selected;
        }
        if selected >= self.offset + height {
            self.offset = selected + 1 - height;
        }
        self.offset = self.offset.min(rows.len().saturating_sub(height));
        let focused = self.focus == Focus::Tree;
        rows.iter()
            .enumerate()
            .skip(self.offset)
            .take(height)
            .map(|(index, row)| {
                list_row(
                    join([Line::raw(row.prefix.clone()), body(row, graph, ctx)]),
                    limit,
                    row.item.is_some() && index == selected,
                    focused,
                    theme,
                    glyphs,
                )
            })
            .collect()
    }

    fn task_lines(&self, limit: usize, ctx: &Ctx) -> Option<Vec<Line<'static>>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let task = self.task?;
        let graph = self.graph.ready()?;
        let edge = graph.edges.iter().find(|edge| edge.task_id == task)?;
        let (kind, word) = edge_status(edge.state);
        let mut lines = vec![
            Line::raw(format!(
                "{} {} {}",
                label(graph, &edge.from),
                glyphs.right,
                label(graph, &edge.to)
            )),
            styled(
                format!("{} {word}", glyphs.status(kind)),
                kind.role(),
                theme,
            ),
            styled(
                match edge.source {
                    TaskSource::Master => "Handed out by the master.",
                    TaskSource::Owner => "Handed off by you.",
                },
                Role::Muted,
                theme,
            ),
            Line::default(),
        ];
        lines.extend(wrap(&plain(&edge.text), limit).into_iter().map(Line::raw));
        lines.push(Line::default());
        lines.push(styled("Reply", Role::Muted, theme));
        match &edge.reply {
            Some(reply) => lines.extend(wrap(&plain(reply), limit).into_iter().map(Line::raw)),
            None => lines.push(styled("No reply yet.", Role::Muted, theme)),
        }
        Some(lines)
    }

    fn draw_right(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let inner = inside(area);
        let limit = usize::from(inner.width).max(1);
        if let Some((title, lines)) = self.cleanup.lines(limit, usize::from(inner.height), ctx) {
            boxed(frame, area, &title, false, lines, ctx);
            return None;
        }
        if let Some(open) = self.instructions.as_mut() {
            let lines = instruction_lines(&open.read, limit, ctx);
            open.top = open
                .top
                .min(lines.len().saturating_sub(usize::from(inner.height)));
            let title = format!("Instructions: {}", plain(&open.label));
            let shown = lines.into_iter().skip(open.top).collect();
            boxed(frame, area, &title, false, shown, ctx);
            return None;
        }
        if let Some(lines) = self.task_lines(limit, ctx) {
            boxed(frame, area, "Task", false, lines, ctx);
            return None;
        }
        let focused = self.focus == Focus::Terminal;
        if let Some(code) = self.refused.as_deref().filter(|_| self.attached.is_none()) {
            let mut lines = vec![Line::raw("The master terminal could not start.")];
            lines.extend(
                wrap(plain_problem(code).unwrap_or(code), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        let Some(attached) = self.attached.as_mut() else {
            let waiting = if self.master {
                MASTER_WAITING
            } else {
                "Enter on an agent opens its live terminal here."
            };
            let lines = wrap(waiting, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme))
                .collect();
            let title = if self.master { "Master" } else { "Terminal" };
            boxed(frame, area, title, false, lines, ctx);
            return None;
        };
        if let Some(code) = &attached.closed {
            let (lead, rest) = closed_words(code);
            let mut lines = vec![Line::raw(lead)];
            lines.extend(
                wrap(rest, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            if !attached.started {
                boxed(frame, area, &attached.label, false, lines, ctx);
                return None;
            }
            boxed(frame, area, &attached.label, false, Vec::new(), ctx);
            attached
                .pane
                .draw(frame.buffer_mut(), inner, ctx.depth, false);
            let rows = inner
                .height
                .min(u16::try_from(lines.len()).unwrap_or(u16::MAX));
            let banner = Rect::new(inner.x, inner.y + inner.height - rows, inner.width, rows);
            let lines: Vec<Line<'static>> = lines
                .into_iter()
                .take(usize::from(rows))
                .map(|line| pad(line, limit, ctx.glyphs))
                .collect();
            draw_lines(frame.buffer_mut(), banner, &lines);
            return None;
        }
        boxed(frame, area, &attached.label, focused, Vec::new(), ctx);
        if inner.width > 0
            && inner.height > 0
            && attached.pane.size() != (inner.height, inner.width)
        {
            attached.pane.resize(inner.height, inner.width);
        }
        attached
            .pane
            .draw(frame.buffer_mut(), inner, ctx.depth, focused)
    }
}

impl Screen for Team {
    fn crumbs(&self) -> Vec<String> {
        let page = if self.master { "Master" } else { "Team" };
        vec![self.name.clone(), page.into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.focus == Focus::Terminal && self.master {
            return vec![hint("Ctrl-]", "Back"), hint("Ctrl-J", "New line")];
        }
        if self.focus == Focus::Terminal {
            return vec![hint("Ctrl-]", "Back to the team")];
        }
        if self.master {
            let mut hints = Vec::new();
            if self
                .attached
                .as_ref()
                .is_some_and(|attached| attached.closed.is_none())
            {
                hints.push(hint("Enter", "Terminal"));
            }
            hints.push(hint("r", "Refresh"));
            hints.push(hint("Esc", "Back"));
            return hints;
        }
        if self.cleanup.open() {
            return self.cleanup.hints();
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = vec![
            hint(&vertical, "Move"),
            hint("Enter", "Open"),
            hint("i", "Instructions"),
        ];
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.closed.is_none())
        {
            hints.push(hint("Tab", "Terminal"));
        }
        hints.push(hint("r", "Refresh"));
        match self.instructions {
            Some(_) => {
                hints.push(hint("PgUp/PgDn", "Scroll"));
                hints.push(hint("Esc", "Close"));
            }
            None => {
                hints.push(hint("c", "Clean up"));
                hints.push(hint("Esc", "Back"));
            }
        }
        hints
    }

    fn raw(&self) -> bool {
        self.focus == Focus::Terminal
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.focus == Focus::Terminal {
            return self.terminal_key(key, ctx);
        }
        if self.master {
            return self.master_key(key, ctx);
        }
        if let Some(flow) = self.cleanup.key(key, &self.project, ctx) {
            return flow;
        }
        let items = self.items(ctx.glyphs);
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(items.len().saturating_sub(1)),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = items.len().saturating_sub(1),
            Key::Enter => match items.get(self.cursor) {
                Some(Item::Node(node)) => self.open(*node, ctx),
                Some(Item::Edge(edge)) => self.show_task(*edge),
                None => {}
            },
            Key::Tab => {
                if self
                    .attached
                    .as_ref()
                    .is_some_and(|attached| attached.closed.is_none())
                {
                    self.task = None;
                    self.instructions = None;
                    self.focus = Focus::Terminal;
                }
            }
            Key::Char('i') => {
                if let Some(Item::Node(node)) = items.get(self.cursor) {
                    self.read_instructions(*node, ctx);
                }
            }
            Key::PageDown if self.instructions.is_some() => {
                if let Some(open) = self.instructions.as_mut() {
                    open.top = open.top.saturating_add(PAGE_STEP);
                }
            }
            Key::PageUp if self.instructions.is_some() => {
                if let Some(open) = self.instructions.as_mut() {
                    open.top = open.top.saturating_sub(PAGE_STEP);
                }
            }
            Key::Char('r') => ctx.request(Request::TeamGraph {
                project: self.project.clone(),
            }),
            Key::Esc if self.instructions.is_some() => self.instructions = None,
            Key::Esc => {
                self.detach(ctx);
                return Flow::Back;
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        self.cleanup.arrive(arrival, &self.project, ctx);
        match arrival {
            Arrival::TeamGraph { project, result } if *project == self.project => {
                self.take_graph(result, ctx.glyphs);
                self.end_stopped(ctx);
                self.follow_master(ctx);
            }
            Arrival::Terminal { agent, frame } => self.stream(agent, frame),
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Process(event) => self.take_instructions(event),
                OwnerEvent::Setup(event) => self.took_change(&event.query, &event.result),
                _ => {}
            },
            Arrival::Connected(_) => self.asked = false,
            _ => {}
        }
        self.sync(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        self.ask_live(ctx);
        ctx.request(Request::TeamGraph {
            project: self.project.clone(),
        });
        self.reattach(ctx);
        self.sync(ctx);
    }

    fn resized(&mut self, ctx: &mut Ctx) {
        self.sync(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        if self.master {
            return self.draw_right(frame, whole(area), ctx);
        }
        let (left, right) = split(area);
        let inner = inside(left);
        let lines = self.tree_lines(usize::from(inner.width), usize::from(inner.height), ctx);
        boxed(frame, left, "Team", self.focus == Focus::Tree, lines, ctx);
        self.draw_right(frame, right, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.graph, Load::Idle | Load::Loading)
    }

    fn place(&self) -> Option<Place> {
        let (id, name) = (self.project.clone(), self.name.clone());
        Some(if self.master {
            Place::Master { id, name }
        } else {
            Place::Team { id, name }
        })
    }
}

/// The tree rows of a team, following each node's parent: the master and its
/// account, then each harness group with its agents and their accounts,
/// agents whose parent is no group last, then a blank row and the tasks.
/// Inputs: the graph and glyphs. Output: rows in display order; master,
/// agent and task rows are selectable.
fn tree(graph: &TeamGraph, glyphs: &Glyphs) -> Vec<TreeRow> {
    let branch = |last: bool| {
        format!(
            "{}{} ",
            if last { glyphs.bottom_left } else { glyphs.tee },
            glyphs.hline
        )
    };
    let stem = |last: bool| {
        if last {
            "   ".to_owned()
        } else {
            format!("{}  ", glyphs.vline)
        }
    };
    let of = |kind: NodeKind| -> Vec<usize> {
        graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == kind)
            .map(|(index, _)| index)
            .collect()
    };
    let groups = of(NodeKind::HarnessGroup);
    let agents = of(NodeKind::Agent);
    let under = |agent: usize, group: usize| {
        graph.nodes[agent].parent.as_deref() == Some(graph.nodes[group].id.as_str())
    };
    let orphans: Vec<usize> = agents
        .iter()
        .copied()
        .filter(|agent| !groups.iter().any(|group| under(*agent, *group)))
        .collect();
    let branches = groups.len() + orphans.len();
    let mut out = Vec::new();
    for master in of(NodeKind::Master) {
        out.push(TreeRow {
            item: Some(Item::Node(master)),
            prefix: String::new(),
            body: Body::Master(master),
        });
        account_row(&mut out, &graph.nodes[master], stem(branches == 0));
    }
    for (position, group) in groups.iter().enumerate() {
        let last = position + 1 == branches;
        let members: Vec<usize> = agents
            .iter()
            .copied()
            .filter(|agent| under(*agent, *group))
            .collect();
        out.push(TreeRow {
            item: None,
            prefix: branch(last),
            body: Body::Group {
                node: *group,
                count: graph.nodes[*group]
                    .agents
                    .map_or(members.len(), usize::from),
            },
        });
        for (index, member) in members.iter().enumerate() {
            let member_last = index + 1 == members.len();
            out.push(TreeRow {
                item: Some(Item::Node(*member)),
                prefix: format!("{}{}", stem(last), branch(member_last)),
                body: Body::Agent(*member),
            });
            account_row(
                &mut out,
                &graph.nodes[*member],
                format!("{}{}", stem(last), stem(member_last)),
            );
        }
    }
    for (index, orphan) in orphans.iter().enumerate() {
        let last = groups.len() + index + 1 == branches;
        out.push(TreeRow {
            item: Some(Item::Node(*orphan)),
            prefix: branch(last),
            body: Body::Agent(*orphan),
        });
        account_row(&mut out, &graph.nodes[*orphan], stem(last));
    }
    out.push(TreeRow {
        item: None,
        prefix: String::new(),
        body: Body::Blank,
    });
    out.push(TreeRow {
        item: None,
        prefix: String::new(),
        body: Body::Heading(if graph.edges.is_empty() {
            "No tasks yet."
        } else {
            "Tasks"
        }),
    });
    out.extend((0..graph.edges.len()).map(|edge| TreeRow {
        item: Some(Item::Edge(edge)),
        prefix: String::new(),
        body: Body::Edge(edge),
    }));
    assert!(out.len() >= 2);
    out
}

fn account_row(out: &mut Vec<TreeRow>, node: &GraphNode, prefix: String) {
    if let Some(account) = &node.account {
        out.push(TreeRow {
            item: None,
            prefix,
            body: Body::Account(account.clone()),
        });
    }
}

/// One tree row's text after its prefix. A master or agent row leads with its
/// status glyph, then its label and status word; a task row leads with its
/// status glyph, then the agent it went to and its title. The row is cut from
/// the right with an ellipsis, so the glyph always shows. Inputs: the row, the
/// graph and the context. Output: the line.
fn body(row: &TreeRow, graph: &TeamGraph, ctx: &Ctx) -> Line<'static> {
    let theme = &ctx.theme;
    let glyphs = ctx.glyphs;
    match &row.body {
        Body::Master(node) | Body::Agent(node) => {
            let node = &graph.nodes[*node];
            match node.status {
                Some(status) => {
                    let kind = agent_status(status);
                    let word = node
                        .status_label
                        .clone()
                        .unwrap_or_else(|| status.label().to_owned());
                    Line::from(vec![
                        Span::styled(glyphs.status(kind), theme.style(kind.role())),
                        Span::raw(format!(" {}", node.label)),
                        Span::styled(format!("  {word}"), theme.style(kind.role())),
                    ])
                }
                None => Line::raw(node.label.clone()),
            }
        }
        Body::Group { node, count } => Line::from(vec![
            Span::styled(graph.nodes[*node].label.clone(), theme.heading),
            Span::styled(format!(" ({count})"), theme.muted),
        ]),
        Body::Account(account) => styled(
            account_name(
                Some(account),
                ctx.store.accounts.ready().map_or(&[], Vec::as_slice),
            ),
            Role::Muted,
            theme,
        ),
        Body::Heading(text) => styled(*text, Role::Muted, theme),
        Body::Blank => Line::default(),
        Body::Edge(edge) => {
            let edge = &graph.edges[*edge];
            let (kind, _) = edge_status(edge.state);
            Line::from(vec![
                Span::styled(glyphs.status(kind), theme.style(kind.role())),
                Span::raw(format!(" {}  ", label(graph, &edge.to))),
                Span::styled(title(&edge.text), theme.muted),
            ])
        }
    }
}

/// Says whether a terminal code means the agent's terminal ended for good.
/// Input: a Closed or Rejected code. Output: true for an ended run, a stopped agent or an
/// unknown pane.
fn dead(code: &str) -> bool {
    matches!(code, ENDED | STOPPED | "terminal_agent_unknown")
}

/// Says whether a terminal code means the engine connection dropped, so the
/// terminal opens again once the engine is back.
/// Input: a Closed or Rejected code. Output: true for connection losses.
fn lost(code: &str) -> bool {
    matches!(
        code,
        "terminal_closed"
            | "terminal_owner_unavailable"
            | "terminal_outcome_unknown"
            | "owner_socket_replaced"
            | "trusted_owner_connection_failed"
    )
}

/// The words shown for a closed or refused terminal.
/// Input: the code. Output: the lead line and the plain reason under it.
fn closed_words(code: &str) -> (&'static str, &'static str) {
    match code {
        NOT_READY => (
            "Not ready yet.",
            "This engine cannot stream agent terminals yet.",
        ),
        ENDED => ("This run finished.", "Its last screen stays here."),
        code if dead(code) => ("This agent has stopped.", "Its terminal is closed."),
        code if lost(code) => (
            "Lost the engine connection.",
            "It opens again once the engine is back.",
        ),
        code => (
            "The terminal closed.",
            plain_problem(code).unwrap_or("Enter on the agent opens it again."),
        ),
    }
}

fn agent_status(status: AgentStatus) -> Status {
    match status {
        AgentStatus::Starting | AgentStatus::Working => Status::Active,
        AgentStatus::Idle => Status::Waiting,
        AgentStatus::Stopped => Status::Off,
        AgentStatus::Failed => Status::Fail,
    }
}

fn edge_status(state: EdgeState) -> (Status, &'static str) {
    match state {
        EdgeState::Sent => (Status::Waiting, "Sent"),
        EdgeState::Running => (Status::Active, "Running"),
        EdgeState::Done => (Status::Ok, "Done"),
        EdgeState::Failed => (Status::Fail, "Failed"),
    }
}

fn label(graph: &TeamGraph, id: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|node| node.id == id)
        .map_or_else(|| id.to_owned(), |node| node.label.clone())
}

/// The lines of an agent's Instructions box: the instruction file with its size and
/// text, then the skills installed for it, or why nothing can be shown.
/// Inputs: the read, the box width and context. Output: the lines, unscrolled.
fn instruction_lines(
    read: &Load<Box<AgentInstructions>>,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let theme = &ctx.theme;
    let muted = |text: &str| -> Vec<Line<'static>> {
        wrap(text, limit)
            .into_iter()
            .map(|line| styled(line, Role::Muted, theme))
            .collect()
    };
    let view = match read {
        Load::Idle | Load::Loading => return muted("Reading what this agent received..."),
        Load::NotReady => return muted("Instructions aren't ready in this engine yet."),
        Load::Failed(code) => {
            return wrap(&message(code), limit)
                .into_iter()
                .map(Line::raw)
                .collect();
        }
        Load::Ready(view) if !view.launched => {
            return muted(
                "This agent hasn't launched yet. What it receives shows here after its first launch.",
            );
        }
        Load::Ready(view) => view,
    };
    let file = Harness::from_id(&view.harness).map_or("Instructions", Harness::instruction_file);
    let mut lines = match view.text.is_empty() {
        true => muted("No app or project instructions were given at its last launch."),
        false => vec![Line::from(vec![
            Span::styled(file, theme.heading),
            Span::styled(format!("  {}", size(view.text.len())), theme.muted),
        ])],
    };
    lines.push(Line::default());
    for text in sanitize(&view.text).lines() {
        match text.is_empty() {
            true => lines.push(Line::default()),
            false => lines.extend(wrap(text, limit).into_iter().map(Line::raw)),
        }
    }
    if !view.text.is_empty() {
        lines.push(Line::default());
    }
    lines.push(styled("Skills", Role::Heading, theme));
    match view.skills.is_empty() {
        true => lines.extend(muted("No skills were installed at its last launch.")),
        false => lines.extend(
            view.skills
                .iter()
                .map(|skill| Line::raw(format!("{} {}", ctx.glyphs.dot, plain(skill)))),
        ),
    }
    lines
}

/// Task text safe to draw in a terminal cell grid: tabs become spaces and
/// other control characters but newlines are dropped, so nothing moves the
/// cursor or overlaps. Input: the text. Output: the cleaned text.
fn plain(text: &str) -> String {
    text.chars()
        .map(|character| if character == '\t' { ' ' } else { character })
        .filter(|character| *character == '\n' || !character.is_control())
        .collect()
}

/// The one-line title of a task: its first non-blank line, cleaned by
/// `plain`, with whitespace runs read as one space. Input: the task text.
/// Output: the title, or "Untitled task" for blank text.
pub(in crate::tui) fn title(text: &str) -> String {
    plain(text)
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| "Untitled task".to_owned())
}

/// The body rectangle the shell gives screens, from the window size.
/// Input: the context. Output: the area between the header and message bands.
fn body_area(ctx: &Ctx) -> Rect {
    Rect::new(
        0,
        HEADER_ROWS,
        ctx.width,
        ctx.height.saturating_sub(CHROME_ROWS),
    )
}

/// Splits the body into the tree box on the left and the terminal box on the
/// right, inside the two-column margins.
/// Input: the body area. Output: (tree, terminal) rectangles.
fn split(area: Rect) -> (Rect, Rect) {
    let usable = area.width.saturating_sub(2 * MARGIN);
    let tree = (usable / 5 * 2).clamp(TREE_MIN, TREE_MAX).min(usable);
    let left = Rect::new(area.x + MARGIN, area.y, tree, area.height);
    let right = Rect::new(
        left.x + tree + 1,
        area.y,
        usable.saturating_sub(tree + 1),
        area.height,
    );
    (left, right)
}

/// The master view's terminal box: the whole body inside the side margins.
/// Input: the body area. Output: the terminal rectangle.
fn whole(area: Rect) -> Rect {
    Rect::new(
        area.x + MARGIN,
        area.y,
        area.width.saturating_sub(2 * MARGIN),
        area.height,
    )
}

fn inside(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// The terminal size the pane gets at the current window size, within the
/// engine's terminal size bounds.
/// Input: the context and whether the master view fills the body. Output:
/// (rows, columns).
fn pane_size(ctx: &Ctx, master: bool) -> (u16, u16) {
    let body = body_area(ctx);
    let inner = inside(if master { whole(body) } else { split(body).1 });
    (
        inner.height.clamp(SIZE_MIN, SIZE_MAX),
        inner.width.clamp(SIZE_MIN, SIZE_MAX),
    )
}

/// Draws a box with a title in its top border and lines inside.
/// Inputs: frame, area, title, whether it has focus, the lines and context.
/// Output: none; lines past the box height are dropped, long ones cut.
fn boxed(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    lines: Vec<Line<'static>>,
    ctx: &Ctx,
) {
    if area.width < BOX_MIN || area.height < 3 {
        return;
    }
    let theme = &ctx.theme;
    let glyphs = ctx.glyphs;
    let full = usize::from(area.width);
    let inner = full - 2;
    let border = if focused { theme.accent } else { theme.muted };
    let title = cut(title, full - 6, glyphs.ellipsis);
    let fill = full - 5 - width(&title);
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{}{} ", glyphs.top_left, glyphs.hline), border),
        Span::styled(title, if focused { theme.accent } else { theme.heading }),
        Span::styled(
            format!(" {}{}", glyphs.hline.repeat(fill), glyphs.top_right),
            border,
        ),
    ])];
    let mut lines = lines.into_iter();
    for _ in 0..usize::from(area.height) - 2 {
        out.push(join([
            Line::from(Span::styled(glyphs.vline, border)),
            pad(lines.next().unwrap_or_default(), inner, glyphs),
            Line::from(Span::styled(glyphs.vline, border)),
        ]));
    }
    out.push(Line::from(Span::styled(
        format!(
            "{}{}{}",
            glyphs.bottom_left,
            glyphs.hline.repeat(inner),
            glyphs.bottom_right
        ),
        border,
    )));
    assert_eq!(out.len(), usize::from(area.height));
    draw_lines(frame.buffer_mut(), area, &out);
}
