use crate::agent_home::Harness;
use crate::config::plain_problem;
use crate::engine::owner::{AgentInstructions, OwnerResult};
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::graph::{LiveAction, Relation, RelationEdge};
use crate::team::{
    AgentRole, AgentStatus, EdgeState, GraphEdge, GraphNode, NodeKind, TaskSource, TeamAction,
    TeamGraph,
};
use crate::terminal::{AgentId, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame};
use crate::tui::client::OwnerEvent;
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::now_ms;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::glyphs::{Glyphs, Status};
use crate::tui::keys::{Key, ShiftArrows};
use crate::tui::screens::new_project::{harness_name, same_harness};
use crate::tui::screens::new_project_page::{key_event, look};
use crate::tui::screens::new_project_view::Row;
use crate::tui::screens::process::{message, size};
use crate::tui::screens::project_markdown::sanitize;
use crate::tui::screens::project_team::{TeamReply, TeamView, account_name, team_plain};
use crate::tui::screens::project_time::local_offset;
use crate::tui::screens::team_messages::{ListKey, MASTER_ELSEWHERE, Messages};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::terminal_pane::{LEADER, TerminalPane};
use crate::tui::text::{cut, join, list_row, pad, styled, width, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;

mod cleanup;
mod finished;
mod questions;
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
const MASTER_ON_WHEN: &str = "It starts when you open this view.";
const MASTER_ON_SWITCHED: &str = "Live master on.";
const MASTER_OFF: &str = "Live master stopped.";
pub(in crate::tui) const MASTER_NO_SESSION: &str =
    "No master session opened, so this conversation won't be in Sessions.";
const MASTER_STOPPED: &str = "The live master is stopped.";
const MASTER_RESTART: &str =
    "It starts again when you open this view again or press n for a new session.";
const MASTER_START_AGAIN: &str = "s starts it again, on the same conversation.";
const MASTER_HEAD_ROWS: u16 = 1;
const MASTER_PAUSED: &str = "The team is stopped (Stop all). Start the team to run the master.";
const MASTER_PAUSED_START: &str =
    "S starts every agent of this project again; the master starts with them.";
const MASTER_TEAM_STOPPED: &str = "Team stopped.";
const MASTER_PASTE_WAIT: &str = "Paste once the master's terminal is open.";
const MASTER_HELD_FULL: &str = "The master's terminal is still starting.";
const MASTER_HELD_REST: &str =
    "What you typed so far goes in once it opens; the rest was not kept.";
const MASTER_HELD_DROPPED: &str = "What you typed for the master was not sent.";
const MASTER_HELD_AGAIN: &str = "Its terminal did not open. Type it again once it is open.";
/// What the owner reads when the master view closes because Live terminal is Off.
const LIVE_TERMINAL_OFF: &str = "Live terminal is off for this project.";
/// Where that master's conversation is and how to change the setting.
const LIVE_TERMINAL_HOW: &str =
    "Its conversation is the console. V in the team settings turns it on.";
/// The most keys kept while the master's terminal starts. Keys, not bytes: a key's bytes
/// depend on the modes of the terminal, known once it opens; a kept key is at most 6
/// bytes, so all of them come to at most 24 KiB, sent in `INPUT_BYTES_MAX` chunks.
const HELD_KEYS_MAX: usize = 4096;
/// The master view header's session keys, widest first; the state picks the widest that
/// fits beside it whole.
const MASTER_CONTROLS: [&str; 2] = ["n New session  l Sessions", "n New session"];

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
    Agent(usize),
    Detail {
        account: Option<String>,
        branch: Option<String>,
        live: Option<String>,
    },
    Note(String),
    Heading(&'static str),
    Blank,
    Edge(usize),
}

struct TreeRow {
    item: Option<Item>,
    prefix: String,
    body: Body,
}

/// The live master switch as the engine last told it: on, off, or never set, why the
/// master last could not start, the team's saved master harness and the harness the
/// running live master uses.
#[derive(Debug)]
struct LiveSwitch {
    on: Option<bool>,
    problem: Option<String>,
    idle: bool,
    switched: Option<String>,
    saved: Option<String>,
    running: Option<String>,
    version: u64,
}

struct Attached {
    agent: AgentId,
    teammate: Option<String>,
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

/// A project's live team: a boxed tree of who leads whom, the master over its
/// harness leads and each lead over its sub-agents and native teammates, each
/// with its status, account and branch, the tasks handed out under it, and the
/// selected agent's live terminal beside it.
/// Input: TeamGraph reads, terminal frames and keys; while the terminal has
/// focus every key goes to the agent until Ctrl-]. Output: TeamGraph,
/// TerminalAttach, TerminalInput, TerminalResize and TerminalDetach requests.
/// s opens the team settings over the whole body, read with `SetupQuery::Team`.
/// The master view is the same screen showing only the live master's terminal; it starts the
/// live master on a read that shows it off, and once more per open after a stale refusal.
pub struct Team {
    project: ProjectId,
    name: String,
    master: bool,
    asked: bool,
    retried: bool,
    checked: bool,
    switch: Option<LiveSwitch>,
    master_agent: Option<Option<i64>>,
    master_live: Option<bool>,
    want: Option<bool>,
    refused: Option<String>,
    paused: bool,
    graph: Load<Box<TeamGraph>>,
    cursor: usize,
    offset: usize,
    focus: Focus,
    attached: Option<Attached>,
    task: Option<i64>,
    cleanup: cleanup::Cleanup,
    finished: finished::Finished,
    questions: questions::Questions,
    instructions: Option<Instructions>,
    messages: Option<Messages>,
    settings: Option<TeamView>,
    held: Vec<Key>,
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
            retried: false,
            checked: false,
            switch: None,
            master_agent: None,
            master_live: None,
            want: None,
            refused: None,
            paused: false,
            graph: Load::Loading,
            cursor: 0,
            offset: 0,
            focus: Focus::Tree,
            attached: None,
            task: None,
            cleanup: cleanup::Cleanup::new(),
            finished: finished::Finished::new(),
            questions: questions::Questions::new(),
            instructions: None,
            messages: None,
            settings: None,
            held: Vec::new(),
        }
    }

    /// Opens the team settings of this project over the team tree: keep warm, limits,
    /// wake rules, branches, merging, stops, caps and the token budget.
    /// Input: the context. Output: none; the first team read is queued.
    fn open_settings(&mut self, ctx: &mut Ctx) {
        let mut view = TeamView::new(self.project.clone());
        view.offset = local_offset(now_ms());
        self.settings = Some(view);
        self.pump_settings(ctx);
    }

    /// Sends the open team settings' next read once one is due, and the harness catalog
    /// read once their master harness picker asks for it; a read the full queue refuses is
    /// forgotten so the next key asks again.
    /// Input: the context. Output: none; a Setup or a Settings request may be queued.
    fn pump_settings(&mut self, ctx: &mut Ctx) {
        let Some(view) = self.settings.as_mut() else {
            return;
        };
        if view.wants_catalog() {
            ctx.request(Request::Settings(Box::new(SettingsJob::Harnesses)));
        }
        if let Some(query) = view.next_query(now_ms())
            && !ctx.try_request(Request::Setup(query))
        {
            view.forget_reads();
        }
    }

    /// Takes a key while the team settings are open: the settings take it first, a change
    /// goes to the engine as `SetupQuery::TeamChange`, a setup save as `SetupQuery::Change`,
    /// and Esc the settings left alone closes them. Inputs: the key and the context.
    /// Output: handled.
    fn settings_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(view) = self.settings.as_mut() else {
            return Flow::Ignored;
        };
        let reply = match key {
            Key::Paste(text) if view.paste(text) => TeamReply::Handled,
            Key::Paste(_) => TeamReply::Ignored,
            other => key_event(other).map_or(TeamReply::Ignored, |event| view.key(event)),
        };
        match reply {
            TeamReply::Change(action) => {
                let generation = ctx.next_generation();
                ctx.request(Request::Setup(SetupQuery::TeamChange {
                    action,
                    generation,
                }));
            }
            TeamReply::Save(action) => {
                let generation = ctx.next_generation();
                ctx.request(Request::Setup(SetupQuery::Change { action, generation }));
            }
            TeamReply::Ignored if *key == Key::Esc => self.settings = None,
            TeamReply::Ignored | TeamReply::Handled => {}
        }
        self.pump_settings(ctx);
        Flow::Handled
    }

    /// The master view of one project: a header with the live master's state, then the
    /// live master's own terminal filling the body, opened as soon as the team shows it.
    /// Every open turns the master on; it stops only on the master idle stop or app quit.
    /// The terminal has the keys from the start; what is typed before it opens waits.
    /// Inputs: the project id and name. Output: the screen, waiting for the master.
    pub fn master(project: ProjectId, name: String) -> Self {
        Self {
            master: true,
            focus: Focus::Terminal,
            ..Self::new(project, name)
        }
    }

    /// Whether the master view is still waiting for a terminal that is coming: none yet or
    /// one the engine has not attached, the team not stopped, no refused start and the
    /// switch not off. Input: none. Output: true while typed keys wait for the terminal.
    fn starting(&self) -> bool {
        self.master
            && self
                .attached
                .as_ref()
                .is_none_or(|attached| !attached.started && attached.closed.is_none())
            && !self.paused
            && self.refused.is_none()
            && self.live_on() != Some(false)
    }

    /// Whether keys go to a terminal: the terminal has focus and, in the master view, one is
    /// attached or coming; a stopped, refused or switched-off master keeps the view keys.
    /// Input: none. Output: true while every key but Ctrl-] goes to the terminal.
    fn terminal_keys(&self) -> bool {
        self.focus == Focus::Terminal
            && (!self.master || self.attached.is_some() || self.starting())
    }

    /// Keeps one key typed while the master's terminal starts, up to `HELD_KEYS_MAX` keys
    /// (not bytes) in all; a paste is not kept, since its bracketing needs the open
    /// terminal's mode. Inputs: the key and the context. Output: none; a dropped key says why.
    fn hold(&mut self, key: &Key, ctx: &mut Ctx) {
        assert!(self.starting());
        if matches!(key, Key::Paste(_)) {
            ctx.say(Message::warning(MASTER_PASTE_WAIT, MASTER_HELD_FULL));
            return;
        }
        if self.held.len() >= HELD_KEYS_MAX {
            ctx.say(Message::warning(MASTER_HELD_FULL, MASTER_HELD_REST));
            return;
        }
        self.held.push(key.clone());
    }

    /// Drops the keys kept for a master terminal that will not get them: one that closed or
    /// was rejected before it opened, one dropped or replaced by another, or a start ended
    /// by a refusal, Stop all or the switch turned off. Says so when any were kept.
    /// Input: the context. Output: none.
    fn drop_held(&mut self, ctx: &mut Ctx) {
        if self.held.is_empty() {
            return;
        }
        self.held.clear();
        ctx.say(Message::warning(MASTER_HELD_DROPPED, MASTER_HELD_AGAIN));
    }

    /// Whether the live master is on as the owner sees it: the owner's last change until the
    /// engine shows it, else the engine's switch; None until read or when never set.
    fn live_on(&self) -> Option<bool> {
        self.want
            .or_else(|| self.switch.as_ref().and_then(|switch| switch.on))
    }

    /// Whether an idle stop ended the live master, the one stop that offers s to start it
    /// again from this view. Input: none. Output: the answer.
    fn idle_stopped(&self) -> bool {
        self.master
            && self.live_on() == Some(false)
            && self.switch.as_ref().is_some_and(|switch| switch.idle)
    }

    fn start_live(&mut self, ctx: &mut Ctx) {
        self.asked = true;
        self.want = Some(true);
        self.refused = None;
        if let Some(switch) = self.switch.as_mut() {
            switch.problem = None;
        }
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::LiveMaster {
                project_id: self.project.clone(),
                on: true,
                version: self.switch.as_ref().map_or(0, |switch| switch.version),
            },
            generation,
        }));
    }

    /// Sends the owner's default Open check for a switch already on, once per open of a
    /// running team's master view and after each accepted Start all, so an existing
    /// project's live master with no session gets one and restarts on it (LMS-1).
    /// Input: the context. Output: none; the check is queued.
    fn check_session(&mut self, ctx: &mut Ctx) {
        self.checked = true;
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::OpenLiveSession {
            project: self.project.clone(),
            generation,
        }));
    }

    fn read_switch(&self, ctx: &mut Ctx) {
        ctx.request(Request::Setup(SetupQuery::Team {
            project: self.project.clone(),
        }));
    }

    /// Takes a team status read, a live switch change or a session check of this master
    /// view: a good read replaces the switch, and the first one of this open turns a switch
    /// that is not on on, or checks the session of a running team's switch already on, and a
    /// switch seen on stays stopped after its next stop until the owner acts; a
    /// failed read changes nothing but ends a start waiting to be retried; an accepted change
    /// is the switch until the next read, which is read at once, and an accepted Start all
    /// checks the session too; a change refused as stale reads the switch again, and the first
    /// such start of this open stays wanted, keeping the owner's typed keys, until that read
    /// starts it once more; any other refusal keeps the switch; an Open that opened no session
    /// is named.
    /// Inputs: the setup query, its result and the context. Output: none.
    fn took_setup(
        &mut self,
        query: &SetupQuery,
        result: &Result<SetupView, String>,
        ctx: &mut Ctx,
    ) {
        if !self.master {
            return;
        }
        match query {
            SetupQuery::Team { project } if *project == self.project => {
                let Ok(SetupView::Team(status)) = result else {
                    if !self.asked && self.want == Some(true) {
                        self.want = None;
                    }
                    return;
                };
                self.paused = status.paused;
                let switch = LiveSwitch {
                    on: status.live_master,
                    problem: status.live_problem.clone(),
                    idle: status.live_idle_stopped,
                    switched: status.live_switch.as_ref().map(switched_words),
                    saved: status
                        .harnesses
                        .iter()
                        .find(|harness| harness.role == AgentRole::Master)
                        .map(|harness| harness.harness.clone()),
                    running: status
                        .agents
                        .iter()
                        .find(|agent| agent.role == AgentRole::Master && agent.status.live())
                        .map(|agent| agent.harness.clone()),
                    version: status.settings_version,
                };
                if self.want.is_some() && self.want == switch.on {
                    self.want = None;
                }
                let off = switch.on != Some(true);
                self.switch = Some(switch);
                if off && !self.asked {
                    self.start_live(ctx);
                } else if !off && !self.paused && !self.asked && !self.checked {
                    self.check_session(ctx);
                }
                self.asked |= !off && !self.paused;
            }
            SetupQuery::TeamChange {
                action:
                    TeamAction::LiveMaster {
                        project_id,
                        on,
                        version,
                    },
                ..
            } if *project_id == self.project => match result {
                Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code)
                    if code == crate::storage::versioned::STALE_CODE =>
                {
                    if self.retried {
                        self.want = None;
                    } else {
                        self.retried = true;
                        self.asked = false;
                    }
                    ctx.say(Message::warning(
                        "The live master switch changed in another window.",
                        plain_problem(code).unwrap_or(code.as_str()),
                    ));
                    self.read_switch(ctx);
                }
                Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                    self.want = None;
                    self.refused = Some(code.clone());
                }
                Ok(view) => {
                    self.want = None;
                    let last = self.switch.take();
                    let (switched, saved, running) = last.map_or((None, None, None), |switch| {
                        (switch.switched, switch.saved, switch.running)
                    });
                    self.switch = Some(LiveSwitch {
                        on: Some(*on),
                        problem: None,
                        idle: false,
                        switched,
                        saved,
                        running,
                        version: version + 1,
                    });
                    if let SetupView::LiveWithoutSession(why) = view {
                        ctx.say(Message::error(MASTER_NO_SESSION, &unopened(why)));
                    }
                    self.read_switch(ctx);
                }
            },
            SetupQuery::TeamChange {
                action: TeamAction::Resume { project_id },
                ..
            } if *project_id == self.project => match result {
                Ok(_) => {
                    self.read_switch(ctx);
                    self.check_session(ctx);
                }
                Err(code) => ctx.say(Message::error(&refusal(code), "")),
            },
            SetupQuery::OpenLiveSession { project, .. } if *project == self.project => {
                if let Ok(SetupView::LiveWithoutSession(why)) | Err(why) = result {
                    ctx.say(Message::error(MASTER_NO_SESSION, &unopened(why)));
                }
            }
            _ => {}
        }
    }

    /// Starts every agent of a stopped team again from the master view, the same change as
    /// Start all on the team page; the live master then starts with the team.
    /// Input: the context. Output: none; the change and a fresh team read are queued.
    fn start_team(&mut self, ctx: &mut Ctx) {
        assert!(self.master && self.paused);
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::Resume {
                project_id: self.project.clone(),
            },
            generation,
        }));
    }

    /// Reads the team again when the graph shows the master stopped after showing it running,
    /// so the open master view says why it stopped. Input: the context. Output: none; a team
    /// read may be queued.
    fn note_master_stopped(&mut self, ctx: &mut Ctx) {
        let live = self.graph.ready().and_then(|graph| {
            graph
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Master)
                .map(|node| node.status.is_none_or(|status| status.live()))
        });
        let last = std::mem::replace(&mut self.master_live, live);
        if self.master && last == Some(true) && live == Some(false) {
            self.read_switch(ctx);
        }
    }

    /// Reads the team again when the graph shows another master agent than it showed last,
    /// so the master view's harness follows a live master that started again (OP-28).
    /// Input: the context. Output: none; a team read may be queued.
    fn note_master_agent(&mut self, ctx: &mut Ctx) {
        if !self.master {
            return;
        }
        let Some(agent) = self.graph.ready().map(|graph| {
            graph
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Master)
                .and_then(|node| node.agent)
        }) else {
            return;
        };
        if self
            .master_agent
            .replace(agent)
            .is_some_and(|last| last != agent)
        {
            self.read_switch(ctx);
        }
    }

    /// Opens the master's terminal once the team lists it, and the new one
    /// after the master moved to another account.
    /// Input: the context. Output: none; attach and size requests are queued.
    fn follow_master(&mut self, ctx: &mut Ctx) {
        if !self.master || self.live_on() == Some(false) {
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
            Key::Char('S') if self.paused => self.start_team(ctx),
            Key::Char('s') if self.idle_stopped() => self.start_live(ctx),
            Key::Char('r') => ctx.request(Request::TeamGraph {
                project: self.project.clone(),
            }),
            Key::Char(code @ ('n' | 'l')) => {
                ctx.sessions_key = Some((self.project.clone(), *code));
                self.detach(ctx);
                return Flow::Back;
            }
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
            .map(|graph| tree(graph, glyphs, usize::MAX))
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

    /// Takes one frame of the attached terminal; at its `Attached` frame the keys held while
    /// the master's terminal started are typed in, in the order typed.
    /// Inputs: the agent, the frame and the context. Output: none.
    fn stream(&mut self, agent: &AgentId, frame: &TerminalFrame, ctx: &mut Ctx) {
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
                let bytes: Vec<u8> = std::mem::take(&mut self.held)
                    .iter()
                    .flat_map(|key| attached.pane.encode(key))
                    .collect();
                for chunk in bytes.chunks(INPUT_BYTES_MAX) {
                    ctx.request(Request::TerminalInput {
                        agent: attached.agent.clone(),
                        bytes: chunk.to_vec(),
                    });
                }
            }
            TerminalFrame::Output { bytes } => attached.pane.feed(bytes),
            TerminalFrame::Closed { reason } => attached.closed = Some(reason.clone()),
            TerminalFrame::Rejected { code } => attached.closed = Some(code.clone()),
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
        let teammate = target
            .pane
            .clone()
            .filter(|_| target.kind == NodeKind::Teammate && target.agent.is_none());
        if teammate
            .as_deref()
            .is_some_and(|pane| !crate::terminal::pane_id_valid(pane))
        {
            ctx.say(Message::warning(
                "This teammate's pane can't be opened.",
                "Press r to read the team again.",
            ));
            return;
        }
        let (id, label) = (target.id.clone(), target.label.clone());
        self.task = None;
        self.instructions = None;
        if self.attached.as_ref().is_some_and(|attached| {
            attached.agent == agent && attached.teammate == teammate && attached.closed.is_none()
        }) {
            self.focus = Focus::Terminal;
            return;
        }
        self.detach(ctx);
        let (rows, cols) = pane_size(ctx, self.master);
        ctx.request(attach_request(&agent, teammate.as_deref()));
        ctx.request(Request::TerminalResize {
            agent: agent.clone(),
            cols,
            rows,
        });
        self.attached = Some(Attached {
            agent,
            teammate,
            node: id,
            label,
            pane: TerminalPane::new(rows, cols),
            size: (rows, cols),
            started: false,
            closed: None,
        });
        self.focus = Focus::Terminal;
    }

    /// Whether a team read says this project's "Live terminal" setting is Off, which closes
    /// the master view, since that master's conversation is the console (DEC-117 Q4).
    /// Input: the setup event. Output: true for a good team read of this project showing Off.
    fn live_terminal_off(&self, event: &crate::tui::client::SetupEvent) -> bool {
        self.master
            && matches!(&event.query, SetupQuery::Team { project } if *project == self.project)
            && matches!(&event.result, Ok(SetupView::Team(status)) if !status.master_live_terminal)
    }

    fn detach(&mut self, ctx: &mut Ctx) {
        if self.attached.is_some() {
            self.drop_held(ctx);
        }
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
        ctx.request(attach_request(
            &attached.agent,
            attached.teammate.as_deref(),
        ));
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
    }

    /// Opens the terminal of the agent a Questions row names, as Enter on its node does,
    /// and moves the tree cursor to that node. Inputs: the terminal and the context.
    /// Output: none; an agent the tree does not show yet gets a plain message.
    fn open_agent(&mut self, agent: &AgentId, ctx: &mut Ctx) {
        let node = self.graph.ready().and_then(|graph| {
            graph
                .nodes
                .iter()
                .position(|node| node.terminal.as_ref() == Some(agent))
        });
        let Some(node) = node else {
            ctx.say(Message::warning(
                "That agent is not in the team tree yet.",
                "Press r to read the team again.",
            ));
            return;
        };
        if let Some(at) = self
            .items(ctx.glyphs)
            .iter()
            .position(|item| *item == Item::Node(node))
        {
            self.cursor = at;
        }
        self.open(node, ctx);
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

    /// Opens the Messages list of one card's sub-agent in the right box, the same list the
    /// Team view opens; the master card says where the master's own conversation is.
    /// Inputs: the node index and context. Output: none; the first page read is queued.
    fn open_messages(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        if target.kind == NodeKind::Master {
            ctx.say(Message::warning(
                "The master has no Messages list.",
                MASTER_ELSEWHERE,
            ));
            return;
        }
        let Some(agent) = AgentId::new(&target.id).ok().and_then(|id| id.team_agent()) else {
            ctx.say(Message::warning(
                "This card has no messages to show.",
                "Only team sub-agents have a Messages list.",
            ));
            return;
        };
        let now = crate::tui::clock::now_ms();
        self.task = None;
        self.instructions = None;
        self.messages = Some(Messages::new(
            self.project.clone(),
            agent,
            &target.label,
            local_offset(now),
        ));
        self.ask_messages(ctx);
    }

    /// Sends the open Messages list's next page read, if one is due.
    fn ask_messages(&mut self, ctx: &mut Ctx) {
        let now = crate::tui::clock::now_ms();
        if let Some(query) = self.messages.as_mut().and_then(|list| list.next_query(now)) {
            ctx.request(Request::Setup(query));
        }
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
            Ok(
                ProcessReply::Text(_)
                | ProcessReply::Current(_)
                | ProcessReply::Saved(_)
                | ProcessReply::Settings(_)
                | ProcessReply::Adoption(_)
                | ProcessReply::Files(_)
                | ProcessReply::Uncommitted(_)
                | ProcessReply::Committed(_),
            ) => return,
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
        if attached.closed.is_none() {
            ctx.request(Request::TerminalResize {
                agent: attached.agent.clone(),
                cols,
                rows,
            });
        }
    }

    /// Takes a key while the terminal has the keys: Ctrl-] gives them back to the team or
    /// master view, any other key goes to the open terminal. While the master's terminal
    /// starts, keys wait for it and Esc goes back; a master view with no terminal coming
    /// gives the keys to the view. A terminal that closed keeps the keys and drops them
    /// until Ctrl-], so keys typed as it closed never act on the view, but for s after an
    /// idle stop, which starts the master again.
    /// Inputs: the key and the context. Output: the flow.
    fn terminal_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if *key == LEADER {
            self.focus = Focus::Tree;
            return Flow::Handled;
        }
        if self.master
            && ctx.shift_arrows == ShiftArrows::SwitchTabs
            && matches!(key, Key::ShiftLeft | Key::ShiftRight)
        {
            return self.step_tab(*key == Key::ShiftRight, ctx);
        }
        if self.starting() {
            if *key == Key::Esc {
                self.held.clear();
                self.detach(ctx);
                return Flow::Back;
            }
            self.hold(key, ctx);
            return Flow::Handled;
        }
        let Some(attached) = self.attached.as_mut() else {
            self.focus = Focus::Tree;
            if self.master {
                return self.master_key(key, ctx);
            }
            return Flow::Handled;
        };
        if attached.closed.is_some() {
            if *key == Key::Char('s') && self.idle_stopped() {
                self.start_live(ctx);
            }
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

    /// Leaves the master view for the project's previous or next tab, as the desktop tab
    /// strip beside its live terminal does; keys kept while the terminal started go with
    /// the view. Inputs: the direction and the context. Output: back to the project page.
    fn step_tab(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        assert!(self.master);
        self.held.clear();
        self.detach(ctx);
        ctx.project_step = Some((self.project.clone(), forward));
        Flow::Back
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
        let rows = tree(graph, glyphs, limit.saturating_sub(2));
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
                let room = limit.saturating_sub(2).saturating_sub(width(&row.prefix));
                list_row(
                    join([Line::raw(row.prefix.clone()), body(row, graph, ctx, room)]),
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
                ran_by(graph, edge)
            )),
            styled(
                format!("{} {word}", glyphs.status(kind)),
                kind.role(),
                theme,
            ),
            styled(
                match (&edge.lead, edge.source) {
                    (Some(lead), _) => format!("Handed out by {}.", label(graph, lead)),
                    (None, TaskSource::Master) => "Handed out by the master.".to_owned(),
                    (None, TaskSource::Owner) => "Handed off by you.".to_owned(),
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

    /// The master view's header: whether the live master is on, the harness it runs and a
    /// saved one that waits for its next start, where the live session's master harness
    /// last switched, and the session keys at the right edge. The state is kept whole: when
    /// it does not fit beside both keys, only n New session shows and l Sessions stays in
    /// the hints. Inputs: the width and context. Output: the line.
    fn master_head(&self, limit: usize, ctx: &Ctx) -> Line<'static> {
        let switch = self.switch.as_ref();
        let switched = switch.and_then(|switch| switch.switched.as_deref());
        let off = self.live_on() == Some(false);
        let lead = match (self.paused, off, switched) {
            (true, _, _) => MASTER_TEAM_STOPPED,
            (false, true, _) => MASTER_OFF,
            (false, false, _) => MASTER_ON_SWITCHED,
        };
        let rest = match (self.paused, off, switched) {
            (true, _, _) => None,
            (false, _, Some(switched)) => Some(switched),
            (false, true, None) => None,
            (false, false, None) => Some(MASTER_ON_WHEN),
        };
        let harness = switch
            .and_then(|switch| harness_words(switch.saved.as_deref(), switch.running.as_deref()));
        let state = [Some(lead), harness.as_deref(), rest]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let control = MASTER_CONTROLS
            .into_iter()
            .find(|control| width(&state) + 2 + width(control) <= limit)
            .unwrap_or(MASTER_CONTROLS[1])
            .to_owned();
        let room = limit.saturating_sub(width(&control));
        if room < width(MASTER_OFF) + 2 {
            return styled(
                cut(&control, limit, ctx.glyphs.ellipsis),
                Role::Muted,
                &ctx.theme,
            );
        }
        let state = cut(&state, room - 2, ctx.glyphs.ellipsis);
        join([
            Line::raw(format!("{state:<room$}")),
            styled(control, Role::Muted, &ctx.theme),
        ])
    }

    fn draw_right(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let inner = inside(area);
        let limit = usize::from(inner.width).max(1);
        if let Some(list) = &self.messages {
            let lines = list
                .lines(limit, usize::from(inner.height))
                .into_iter()
                .map(|line| match line.role {
                    Some(role) => styled(line.text, role, theme),
                    None => Line::raw(line.text),
                })
                .collect();
            boxed(frame, area, &list.title(), false, lines, ctx);
            return None;
        }
        if let Some((title, lines)) = self.cleanup.lines(limit, usize::from(inner.height), ctx) {
            boxed(frame, area, &title, false, lines, ctx);
            return None;
        }
        if let Some((title, lines)) = self.finished.lines(limit, usize::from(inner.height), ctx) {
            boxed(frame, area, &title, false, lines, ctx);
            return None;
        }
        if let Some((title, lines, cursor)) =
            self.questions.lines(limit, usize::from(inner.height), ctx)
        {
            let typing = cursor.is_some();
            boxed(frame, area, &title, typing, lines, ctx);
            return cursor.map(|(row, column)| Position {
                x: inner.x.saturating_add(
                    u16::try_from(column)
                        .unwrap_or(u16::MAX)
                        .min(inner.width.saturating_sub(1)),
                ),
                y: inner.y.saturating_add(
                    u16::try_from(row)
                        .unwrap_or(u16::MAX)
                        .min(inner.height.saturating_sub(1)),
                ),
            });
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
        if self.master && self.paused && self.attached.is_none() {
            let mut lines: Vec<Line<'static>> = wrap(MASTER_PAUSED, limit)
                .into_iter()
                .map(Line::raw)
                .collect();
            lines.extend(
                wrap(MASTER_PAUSED_START, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        if let Some(code) = self.refused.as_deref().filter(|_| self.attached.is_none()) {
            let mut lines = vec![Line::raw("The master terminal could not start.")];
            lines.extend(
                wrap(&refusal(code), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        if self.master && self.live_on() == Some(false) && self.attached.is_none() {
            let problem = self
                .switch
                .as_ref()
                .and_then(|switch| switch.problem.as_deref());
            let mut lines: Vec<Line<'static>> = wrap(problem.unwrap_or(MASTER_STOPPED), limit)
                .into_iter()
                .map(Line::raw)
                .collect();
            lines.extend(
                wrap(
                    if self.idle_stopped() {
                        MASTER_START_AGAIN
                    } else {
                        MASTER_RESTART
                    },
                    limit,
                )
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        let idle_line = self
            .idle_stopped()
            .then(|| {
                self.switch
                    .as_ref()
                    .and_then(|switch| switch.problem.clone())
            })
            .flatten();
        let Some(attached) = self.attached.as_mut() else {
            let waiting = if self.master {
                self.switch
                    .as_ref()
                    .and_then(|switch| switch.problem.as_deref())
                    .unwrap_or(MASTER_WAITING)
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
        if let Some(waiting) = self
            .switch
            .as_ref()
            .and_then(|switch| switch.problem.as_deref())
            .filter(|_| self.master && !self.paused && !attached.started)
        {
            let lines = wrap(waiting, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme))
                .collect();
            boxed(frame, area, &attached.label, false, lines, ctx);
            return None;
        }
        if let Some(code) = &attached.closed {
            let (lead, rest) = idle_line
                .as_deref()
                .map_or(closed_words(code), |line| (line, MASTER_START_AGAIN));
            let mut lines: Vec<Line<'static>> =
                wrap(lead, limit).into_iter().map(Line::raw).collect();
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
        if self.terminal_keys() && self.master {
            let mut hints = vec![hint("Ctrl-]", "Leave terminal"), hint("Ctrl-J", "New line")];
            if self.idle_stopped() {
                hints.insert(0, hint("s", "Start again"));
            }
            if self.starting() {
                hints.push(hint("Esc", "Back"));
            }
            if ctx.shift_arrows == ShiftArrows::SwitchTabs {
                hints.push(hint(
                    &format!("Shift-{}{}", ctx.glyphs.left, ctx.glyphs.right),
                    "Tabs",
                ));
            }
            return hints;
        }
        if self.terminal_keys() {
            return vec![hint("Ctrl-]", "Back to the team")];
        }
        if self.master {
            let mut hints = Vec::new();
            if self.paused {
                hints.push(hint("S", "Start all"));
            }
            if self.idle_stopped() {
                hints.push(hint("s", "Start again"));
            }
            if self
                .attached
                .as_ref()
                .is_some_and(|attached| attached.closed.is_none())
            {
                hints.push(hint("Enter", "Terminal"));
            }
            hints.push(hint("n", "New session"));
            hints.push(hint("l", "Sessions"));
            hints.push(hint("Esc", "Back"));
            hints.push(hint("r", "Refresh"));
            return hints;
        }
        if let Some(view) = &self.settings {
            return view
                .help()
                .into_iter()
                .map(|(key, label)| hint(key, label))
                .collect();
        }
        if let Some(list) = &self.messages {
            return list
                .help()
                .into_iter()
                .map(|(key, label)| hint(key, label))
                .collect();
        }
        if self.cleanup.open() {
            return self.cleanup.hints();
        }
        if self.finished.open() {
            return self.finished.hints();
        }
        if self.questions.open() {
            return self.questions.hints(ctx);
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = vec![
            hint(&vertical, "Move"),
            hint("Enter", "Open"),
            hint("i", "Instructions"),
            hint("m", "Messages"),
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
                let questions = match self.questions.counts() {
                    Some((asked, waiting)) if asked + waiting > 0 => {
                        format!("Questions ({})", asked + waiting)
                    }
                    _ => "Questions".to_owned(),
                };
                hints.push(hint("q", &questions));
                hints.push(hint("f", "Finished"));
                hints.push(hint("c", "Clean up"));
                hints.push(hint("Esc", "Back"));
            }
        }
        hints.push(hint("s", "Settings"));
        hints
    }

    fn raw(&self) -> bool {
        self.terminal_keys()
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        if !self.master {
            return Flow::Ignored;
        }
        self.step_tab(forward, ctx)
    }

    fn typing(&self) -> bool {
        self.settings.as_ref().is_some_and(TeamView::typing) || self.questions.typing()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.focus == Focus::Terminal {
            return self.terminal_key(key, ctx);
        }
        if self.master {
            return self.master_key(key, ctx);
        }
        if self.settings.is_some() {
            return self.settings_key(key, ctx);
        }
        if let Some(list) = self.messages.as_mut() {
            if list.key(key) == ListKey::Closed {
                self.messages = None;
            }
            self.ask_messages(ctx);
            return Flow::Handled;
        }
        if !self.cleanup.open()
            && !self.finished.open()
            && let Some(took) = self.questions.key(key, &self.project, ctx)
        {
            if let questions::Took::Open(agent) = took {
                self.open_agent(&agent, ctx);
            }
            return Flow::Handled;
        }
        if !self.cleanup.open()
            && let Some(flow) = self.finished.key(key, &self.project, ctx)
        {
            return flow;
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
            Key::Char('m') => {
                if let Some(Item::Node(node)) = items.get(self.cursor) {
                    self.open_messages(*node, ctx);
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
            Key::Char('s') => self.open_settings(ctx),
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
        self.finished.arrive(arrival, &self.project, ctx);
        self.questions.arrive(arrival, &self.project, ctx);
        if let Some(list) = self.messages.as_mut() {
            match arrival {
                Arrival::Owner(event) => {
                    if let OwnerEvent::Setup(event) = event.as_ref() {
                        list.apply(&event.query, &event.result);
                    }
                }
                Arrival::Disconnected(_) => list.forget_reads(),
                _ => {}
            }
        }
        match arrival {
            Arrival::TeamGraph { project, result } if *project == self.project => {
                self.take_graph(result, ctx.glyphs);
                self.end_stopped(ctx);
                self.note_master_agent(ctx);
                self.note_master_stopped(ctx);
                self.follow_master(ctx);
                if !self.master {
                    questions::Questions::read(&self.project, ctx);
                }
            }
            Arrival::Terminal { agent, frame } => self.stream(agent, frame, ctx),
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Process(event) => self.take_instructions(event),
                OwnerEvent::Setup(event) => {
                    if self.live_terminal_off(event) {
                        self.detach(ctx);
                        ctx.say(Message::warning(LIVE_TERMINAL_OFF, LIVE_TERMINAL_HOW));
                        return Flow::Back;
                    }
                    self.took_setup(&event.query, &event.result, ctx);
                    if let Some(view) = self.settings.as_mut() {
                        view.apply(&event.query, event.result.clone());
                    }
                }
                OwnerEvent::Settings(event) => {
                    if let (SettingsEvent::Harnesses(result), Some(view)) =
                        (event.as_ref(), self.settings.as_mut())
                    {
                        view.take_catalog(result);
                    }
                }
                _ => {}
            },
            Arrival::Disconnected(_) => {
                if let Some(view) = self.settings.as_mut() {
                    view.forget_reads();
                }
            }
            _ => {}
        }
        self.ask_messages(ctx);
        if !self.starting() {
            self.drop_held(ctx);
        }
        self.pump_settings(ctx);
        self.sync(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.master {
            self.read_switch(ctx);
        }
        self.pump_settings(ctx);
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
            let (head, body) = master_split(whole(area));
            let line = self.master_head(usize::from(head.width), ctx);
            draw_lines(frame.buffer_mut(), head, &[line]);
            return self.draw_right(frame, body, ctx);
        }
        if let Some(view) = &self.settings {
            let area = whole(area);
            let inner = inside(area);
            let accounts = ctx.store.accounts.ready().map_or(&[][..], Vec::as_slice);
            let (rows, _, caret) = view.draw(
                usize::from(inner.width),
                usize::from(inner.height),
                accounts,
                &look(ctx),
            );
            let lines = rows.into_iter().map(Row::line).collect();
            boxed(frame, area, "Team settings", true, lines, ctx);
            let (row, column) = caret?;
            let (row, column) = (u16::try_from(row).ok()?, u16::try_from(column).ok()?);
            return (row < inner.height && column < inner.width)
                .then(|| Position::new(inner.x + column, inner.y + row));
        }
        let (left, right) = split(area);
        let inner = inside(left);
        let lines = self.tree_lines(usize::from(inner.width), usize::from(inner.height), ctx);
        boxed(frame, left, "Team", self.focus == Focus::Tree, lines, ctx);
        self.draw_right(frame, right, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.graph, Load::Idle | Load::Loading)
            || self.settings.as_ref().is_some_and(TeamView::busy)
    }

    fn draft(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|view| view.draft.is_some())
    }

    fn loss(&self) -> &'static str {
        "The task you are writing will be lost."
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
/// The rows of the team tree: each master, then the agents it leads depth first, each
/// with a detail row for its account and branch, then the task rows. An agent whose lead
/// is not in the graph hangs under the master, or starts the tree when there is none.
/// Inputs: the graph, glyphs and the width a row's prefix and text get, which wraps the
/// words of an agent launched without hooks. Output: the rows, at least the blank and
/// heading rows.
fn tree(graph: &TeamGraph, glyphs: &Glyphs, width: usize) -> Vec<TreeRow> {
    let known: BTreeSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    let masters: Vec<usize> = (0..graph.nodes.len())
        .filter(|index| graph.nodes[*index].kind == NodeKind::Master)
        .collect();
    let orphans: Vec<usize> = (0..graph.nodes.len())
        .filter(|index| {
            let node = &graph.nodes[*index];
            node.kind.agent()
                && node
                    .parent
                    .as_deref()
                    .is_none_or(|parent| !known.contains(parent))
        })
        .collect();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (position, master) in masters.iter().enumerate() {
        let mut kids = children(graph, *master);
        if position == 0 {
            kids.extend(&orphans);
        }
        seen.insert(*master);
        out.push(TreeRow {
            item: Some(Item::Node(*master)),
            prefix: String::new(),
            body: Body::Master(*master),
        });
        detail_row(
            &mut out,
            &graph.nodes[*master],
            stem(glyphs, kids.is_empty()),
            width,
            None,
        );
        lead_rows(graph, glyphs, &kids, "", width, &mut seen, &mut out);
    }
    if masters.is_empty() {
        lead_rows(graph, glyphs, &orphans, "", width, &mut seen, &mut out);
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
    assert!(seen.len() <= graph.nodes.len());
    assert!(out.len() >= 2);
    out
}

fn children(graph: &TeamGraph, lead: usize) -> Vec<usize> {
    let id = graph.nodes[lead].id.as_str();
    (0..graph.nodes.len())
        .filter(|index| {
            graph.nodes[*index].kind.agent() && graph.nodes[*index].parent.as_deref() == Some(id)
        })
        .collect()
}

fn branch_glyph(glyphs: &Glyphs, last: bool) -> String {
    format!(
        "{}{} ",
        if last { glyphs.bottom_left } else { glyphs.tee },
        glyphs.hline
    )
}

fn stem(glyphs: &Glyphs, last: bool) -> String {
    if last {
        "   ".to_owned()
    } else {
        format!("{}  ", glyphs.vline)
    }
}

/// Adds the rows of agents that share one lead, each followed by its detail row and the
/// agents it leads in turn.
/// A node already drawn is skipped, so a cycle in the parent links cannot loop. Inputs: the
/// graph, glyphs, the agents, the indent their branches hang from, the row width, the drawn
/// nodes and the rows. Output: none; rows are appended.
fn lead_rows(
    graph: &TeamGraph,
    glyphs: &Glyphs,
    members: &[usize],
    indent: &str,
    width: usize,
    seen: &mut BTreeSet<usize>,
    out: &mut Vec<TreeRow>,
) {
    let members: Vec<usize> = members
        .iter()
        .copied()
        .filter(|member| !seen.contains(member))
        .collect();
    for (index, member) in members.iter().enumerate() {
        if !seen.insert(*member) {
            continue;
        }
        let last = index + 1 == members.len();
        let prefix = format!("{indent}{}", branch_glyph(glyphs, last));
        let room = width.saturating_sub(crate::tui::text::width(&prefix));
        let moved = moved_action(graph, &graph.nodes[*member], glyphs, room);
        out.push(TreeRow {
            item: Some(Item::Node(*member)),
            prefix,
            body: Body::Agent(*member),
        });
        let base = format!("{indent}{}", stem(glyphs, last));
        let kids: Vec<usize> = children(graph, *member)
            .into_iter()
            .filter(|kid| !seen.contains(kid))
            .collect();
        let detail = if kids.is_empty() {
            base.clone()
        } else {
            format!("{base}{}", stem(glyphs, false))
        };
        detail_row(out, &graph.nodes[*member], detail, width, moved);
        lead_rows(graph, glyphs, &kids, &base, width, seen, out);
    }
}

/// The engine's relation edge for the line from an agent's lead down to it: the edge from
/// the agent's parent, when that parent is in the graph, as the tree then hangs the agent
/// under it. Inputs: the graph and the agent. Output: the edge's index, or None.
fn relation_of(graph: &TeamGraph, node: &GraphNode) -> Option<usize> {
    let parent = node.parent.as_deref()?;
    if !graph.nodes.iter().any(|other| other.id == parent) {
        return None;
    }
    graph
        .relations
        .iter()
        .position(|edge| edge.from == parent && edge.to == node.id)
}

/// The cells an agent's line takes around its relation words: its status glyph and label,
/// then two spaces and its status word. Inputs: the node and the glyphs. Output: the two
/// widths; the second is 0 for a node with no status word.
fn line_cells(node: &GraphNode, glyphs: &Glyphs) -> (usize, usize) {
    let glyph = node
        .status
        .map_or(0, |status| width(glyphs.status(agent_status(status))) + 1);
    let word = node
        .status_label
        .as_deref()
        .or(node.status.map(|status| status.label()));
    (
        glyph + width(&node.label),
        word.map_or(0, |word| 2 + width(word)),
    )
}

/// Says whether a live action stays on its agent's own line: the status word keeps its place
/// and at least the action's first word shows, or all of it for a wait on the owner, whose
/// last words say who it waits on. Inputs: the node, its edge, the action's words, the glyphs
/// and the cells left after the row's prefix. Output: true to show it there, cut to fit.
fn live_fits(
    node: &GraphNode,
    edge: &RelationEdge,
    label: &str,
    glyphs: &Glyphs,
    room: usize,
) -> bool {
    let (head, word) = line_cells(node, glyphs);
    let lead = 3 + width(glyphs.dot);
    let first = label.split(' ').next().unwrap_or(label);
    let least = if matches!(edge.live, Some(LiveAction::WaitingOn { owner: true })) {
        width(label)
    } else {
        width(label).min(width(first) + width(glyphs.ellipsis))
    };
    head + word + lead + least <= room
}

/// The live action an agent's detail row shows because it did not fit on the agent's own
/// line. Inputs: the graph, the node, the glyphs and the cells left after the agent row's
/// prefix. Output: the action's words, or None when there is none to move.
fn moved_action(
    graph: &TeamGraph,
    node: &GraphNode,
    glyphs: &Glyphs,
    room: usize,
) -> Option<String> {
    let edge = &graph.relations[relation_of(graph, node)?];
    edge.live.as_ref()?;
    let label = edge_label(edge);
    (!live_fits(node, edge, &label, glyphs, room)).then_some(label)
}

/// Adds the rows under an agent: its account and branch on one row, then, for an agent
/// whose launch fires no hooks, that its receipts are unavailable, then the engine's note
/// when the Delivery setting leaves it no way to get messages, each wrapped to the width
/// left after the prefix so the words are never cut. A live action that did not fit on the
/// agent's own line goes first on the account row, or as the first note with no account row.
/// Inputs: the rows, the node, the prefix, the row width and that action. Output: none; rows
/// are appended.
fn detail_row(
    out: &mut Vec<TreeRow>,
    node: &GraphNode,
    prefix: String,
    width: usize,
    mut live: Option<String>,
) {
    if node.account.is_some() || node.branch.is_some() {
        out.push(TreeRow {
            item: None,
            prefix: prefix.clone(),
            body: Body::Detail {
                account: node.account.clone(),
                branch: node.branch.clone(),
                live: live.take(),
            },
        });
    }
    let room = width
        .saturating_sub(crate::tui::text::width(&prefix))
        .max(1);
    let hookless = node
        .hookless
        .then(|| format!("Receipts {}", crate::coord::adoption::HOOKLESS_WORDS));
    for words in live
        .into_iter()
        .chain(hookless)
        .chain(node.message_note.as_deref().map(plain))
    {
        out.extend(wrap(&words, room).into_iter().map(|line| TreeRow {
            item: None,
            prefix: prefix.clone(),
            body: Body::Note(line),
        }));
    }
}

/// One tree row's text after its prefix: a master or agent row's status glyph and label, the
/// live action on the line from its lead, its status word and that line's standing relation;
/// a task row's status glyph, the agent it went to and its title. An agent row keeps its
/// status word: the live action is cut to the cells left or moves to the detail row, and a
/// standing relation shows whole or not at all. Anything else is cut from the right with an
/// ellipsis, so the glyph always shows. Inputs: the row, the graph, the context and the cells
/// left after the prefix. Output: the line.
fn body(row: &TreeRow, graph: &TeamGraph, ctx: &Ctx, room: usize) -> Line<'static> {
    let theme = &ctx.theme;
    let glyphs = ctx.glyphs;
    match &row.body {
        Body::Master(node) | Body::Agent(node) => {
            let node = &graph.nodes[*node];
            let edge = relation_of(graph, node).map(|pair| &graph.relations[pair]);
            let (head, word) = line_cells(node, glyphs);
            let live = edge
                .filter(|edge| edge.live.is_some())
                .map(|edge| (edge, edge_label(edge)))
                .filter(|(edge, label)| live_fits(node, edge, label, glyphs, room))
                .map(|(_, label)| {
                    assert!(
                        head + word < room,
                        "a live action needs cells beside the status"
                    );
                    Span::styled(
                        cut(
                            &format!("  {} {label}", glyphs.dot),
                            room - head - word,
                            glyphs.ellipsis,
                        ),
                        theme.style(Role::Accent),
                    )
                });
            let standing = edge
                .filter(|edge| edge.live.is_none())
                .map(edge_label)
                .filter(|label| head + word + 2 + width(label) <= room)
                .map(|label| Span::styled(format!("  {label}"), theme.muted));
            let mut spans = Vec::new();
            match node.status {
                Some(status) => {
                    let kind = agent_status(status);
                    let word = node
                        .status_label
                        .clone()
                        .unwrap_or_else(|| status.label().to_owned());
                    spans.push(Span::styled(glyphs.status(kind), theme.style(kind.role())));
                    spans.push(Span::raw(format!(" {}", node.label)));
                    spans.extend(live);
                    spans.push(Span::styled(format!("  {word}"), theme.style(kind.role())));
                }
                None => {
                    spans.push(Span::raw(node.label.clone()));
                    spans.extend(live);
                    spans.extend(
                        node.status_label
                            .as_ref()
                            .map(|word| Span::styled(format!("  {word}"), theme.muted)),
                    );
                }
            }
            spans.extend(standing);
            Line::from(spans)
        }
        Body::Detail {
            account,
            branch,
            live,
        } => {
            let account = account.as_ref().map(|account| {
                account_name(
                    Some(account),
                    ctx.store.accounts.ready().map_or(&[], Vec::as_slice),
                )
            });
            let rest = account
                .into_iter()
                .chain(branch.iter().cloned())
                .collect::<Vec<_>>()
                .join("  ");
            let Some(live) = live else {
                return styled(rest, Role::Muted, theme);
            };
            Line::from(vec![
                Span::styled(format!("{} {live}", glyphs.dot), theme.style(Role::Accent)),
                Span::styled(format!("  {rest}"), theme.muted),
            ])
        }
        Body::Note(text) => styled(text.clone(), Role::Muted, theme),
        Body::Heading(text) => styled(*text, Role::Muted, theme),
        Body::Blank => Line::default(),
        Body::Edge(edge) => {
            let edge = &graph.edges[*edge];
            let (kind, _) = edge_status(edge.state);
            Line::from(vec![
                Span::styled(glyphs.status(kind), theme.style(kind.role())),
                Span::raw(format!(" {}  ", ran_by(graph, edge))),
                Span::styled(title(&edge.text), theme.muted),
            ])
        }
    }
}

/// The words one tree line shows, as the desktop graph shows them: the live action while it
/// happens, a handoff naming its task's title, a wait on the owner naming the owner's answer
/// and a review the agent whose change set it reviews, else the relation. Input: the
/// engine's relation edge. Output: the label.
fn edge_label(edge: &RelationEdge) -> String {
    match &edge.live {
        Some(LiveAction::HandingOff { task }) => format!("handing off: {}", title(task)),
        Some(LiveAction::Messaging) => "messaging".to_owned(),
        Some(LiveAction::WaitingOn { owner: true }) => "waiting on: your answer".to_owned(),
        Some(LiveAction::WaitingOn { owner: false }) => "waiting on".to_owned(),
        Some(LiveAction::Reviewing { agent }) => {
            let name = plain(agent)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if name.is_empty() {
                "reviewing: change set".to_owned()
            } else {
                format!("reviewing: {name}'s change set")
            }
        }
        None => match edge.relation {
            Relation::Leads => "leads",
            Relation::DelegatedTo => "delegated to",
            Relation::TeammateOf => "teammate of",
        }
        .to_owned(),
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
/// The request that opens a terminal view: the agent's own terminal, or one pane of its
/// window, such as a native teammate's, through `OwnerClient::terminal_attach_pane`.
/// Takes the terminal name and the pane. Returns the request.
fn attach_request(agent: &AgentId, teammate: Option<&str>) -> Request {
    match teammate {
        Some(pane) => Request::TerminalAttachPane {
            agent: agent.clone(),
            pane: pane.to_owned(),
        },
        None => Request::TerminalAttach {
            agent: agent.clone(),
        },
    }
}

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

/// One plain sentence for a refused master view team change, the same words the team panel
/// gives. Input: the engine's code. Output: the config's sentence when it names the code,
/// else the team panel's sentence, which names an unknown code after a retry sentence.
fn refusal(code: &str) -> String {
    plain_problem(code).map_or_else(|| team_plain(code), str::to_owned)
}

/// The plain words for why the owner's Open opened no master session (LMS-M1): the sentence
/// a code has, or the reason itself when it is a sentence already. Input: the code or
/// reason. Output: the words.
pub(in crate::tui) fn unopened(why: &str) -> String {
    if crate::tui::text::plain_code(why) {
        refusal(why)
    } else {
        why.to_owned()
    }
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

/// Names the agent a task went to: the sub-agent or teammate that ran it, drawn or not, so a
/// stopped agent's task keeps its name, else the node it waits on. Takes the graph and the
/// edge. Returns the name.
fn ran_by(graph: &TeamGraph, edge: &GraphEdge) -> String {
    edge.agent_label
        .as_deref()
        .map_or_else(|| label(graph, &edge.to), ToOwned::to_owned)
}

/// The lines of an agent's Instructions box: what its harness reported loading and
/// invoking, the instruction file with its size and text, then the skills installed for
/// it, or why nothing can be shown.
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
    let mut lines = reported_lines(view, limit, ctx);
    lines.extend(match view.text.is_empty() {
        true => muted("No app or project instructions were given at its last launch."),
        false => vec![Line::from(vec![
            Span::styled(file, theme.heading),
            Span::styled(format!("  {}", size(view.text.len())), theme.muted),
        ])],
    });
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
    lines.push(styled("Instruction files", Role::Heading, theme));
    match view.files.is_empty() {
        true => lines.extend(muted("No instruction files were found at its last launch.")),
        false => lines.extend(view.files.iter().flat_map(|file| {
            wrap(
                &format!(
                    "{} {}  {}, {}",
                    ctx.glyphs.dot,
                    plain(&file.name),
                    size(file.bytes.try_into().unwrap_or(usize::MAX)),
                    file_state(file.state)
                ),
                limit,
            )
            .into_iter()
            .map(Line::raw)
        })),
    }
    lines.push(Line::default());
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

const CLAUDE_GIVEN: &str =
    "none loaded by Claude itself; CrazyAgent gives its instructions in the launch prompt";
const CLAUDE_NO_RECORD: &str =
    "A headless Claude run reports nothing while Keep transcripts for headless Claude runs is off.";

/// The lines saying what an agent's harness reported in its own session records: the
/// instruction file names it loaded and the skills it invoked, then each file's place.
/// Claude loads no instruction file itself, since CrazyAgent gives it the instructions in
/// its launch prompt, and a headless Claude run writes no session record while its project
/// keeps no transcripts, so the lines say so.
/// Inputs: the view, the box width and context. Output: the lines and a blank line.
fn reported_lines(view: &AgentInstructions, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    let claude = view.harness == "claude-code";
    let mut names: Vec<&str> = Vec::new();
    for file in &view.loaded {
        if !names.contains(&file.name.as_str()) {
            names.push(&file.name);
        }
    }
    let listed = |list: &[&str], none: &str| match list.is_empty() {
        true => none.to_owned(),
        false => list.join(", "),
    };
    let used: Vec<&str> = view.skills_used.iter().map(String::as_str).collect();
    let (files, skills) = match view.sessions {
        0 => ("not reported yet".to_owned(), "not reported yet".to_owned()),
        _ => (
            listed(
                &names,
                if claude {
                    CLAUDE_GIVEN
                } else {
                    "none loaded by its harness"
                },
            ),
            listed(&used, "none"),
        ),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for text in [
        format!("Instructions: {files}"),
        format!("Skills used: {skills}"),
    ] {
        lines.extend(wrap(&plain(&text), limit).into_iter().map(Line::raw));
    }
    if claude && view.sessions == 0 {
        lines.extend(
            wrap(CLAUDE_NO_RECORD, limit)
                .into_iter()
                .map(|line| styled(line, Role::Muted, &ctx.theme)),
        );
    }
    for file in &view.loaded {
        let place = plain(&format!("{} {}  {}", ctx.glyphs.dot, file.name, file.path));
        lines.extend(
            wrap(&place, limit)
                .into_iter()
                .map(|line| styled(line, Role::Muted, &ctx.theme)),
        );
    }
    lines.push(Line::default());
    lines
}

/// What became of one instruction file at an agent's last launch, in the owner's words.
/// Input: the engine's record of it. Output: the words.
fn file_state(state: crate::instruction_files::State) -> &'static str {
    use crate::instruction_files::State;
    match state {
        State::Given => "given",
        State::Same => "left out: the same text as the instructions or a file above",
        State::TooLarge => "left out: larger than CrazyAgent gives",
        State::Unsafe => "left out: a link or not a plain file",
        State::Invalid => "left out: not plain text",
        State::Unreadable => "left out: couldn't be read",
    }
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

/// The master header's words for the master's harness: the running one, else the saved
/// one, and the saved one from the next start while the two differ (OP-28).
/// Takes the saved and the running harness ids. Returns the words, none when both are unknown.
fn harness_words(saved: Option<&str>, running: Option<&str>) -> Option<String> {
    let shown = running.or(saved)?;
    let title = harness_name(&plain(shown), None);
    Some(match saved {
        Some(saved) if !same_harness(saved, shown) => format!(
            "Harness: {title}; {} from the next start.",
            harness_name(&plain(saved), None)
        ),
        _ => format!("Harness: {title}."),
    })
}

/// The master header's words for the live session's last master harness switch.
/// Takes the switch. Returns "Switched to <harness>, carried over" or ", started fresh".
fn switched_words(switch: &crate::team::SessionSwitch) -> String {
    let what = match switch.mode {
        crate::team::SwitchMode::Carry => "carried over",
        crate::team::SwitchMode::Fresh => "started fresh",
    };
    format!("Switched to {}, {what}.", plain(&switch.harness))
}

/// Splits the master view into its header row with the live master switch and the body.
/// Takes the area inside the side margins. Returns the header and the body.
fn master_split(area: Rect) -> (Rect, Rect) {
    let rows = MASTER_HEAD_ROWS.min(area.height);
    (
        Rect::new(area.x, area.y, area.width, rows),
        Rect::new(area.x, area.y + rows, area.width, area.height - rows),
    )
}

/// The terminal size the pane gets at the current window size, within the
/// engine's terminal size bounds.
/// Input: the context and whether the master view fills the body. Output:
/// (rows, columns).
fn pane_size(ctx: &Ctx, master: bool) -> (u16, u16) {
    let body = body_area(ctx);
    let inner = inside(if master {
        master_split(whole(body)).1
    } else {
        split(body).1
    });
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
