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
use crate::tui::screens::project_team::{self, TeamReply, TeamView, account_name, team_plain};
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
mod screen;
mod state;
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
const MASTER_TRY_AGAIN: &str = "s tries again.";
const MASTER_OVERDUE: &str = "The master's terminal hasn't opened after a minute.";
/// How long the master view waits for the master's terminal before it stops waiting and
/// offers s to try again, in milliseconds, the desktop's `OPEN_LIMIT_MS` (DEC-141).
const OPEN_LIMIT_MS: i64 = 60_000;
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
    limits: Option<crate::team::master_limits::LimitsView>,
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
/// live master on a read that shows it off, and once more per open after a stale refusal;
/// a read naming a master with no live terminal takes it back to the console instead.
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
    waiting_since: Option<i64>,
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
    messages_for: Option<String>,
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
    let hookless = node.hookless.then(|| {
        format!(
            "Receipts {}",
            crate::coord::adoption::hookless_words(&node.harness)
        )
    });
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
/// Input: a Closed or Rejected code. Output: true for an ended run, a stopped agent, an
/// unknown pane or a lead whose teammates' server ended ([`crate::terminal::ended_code`]).
fn dead(code: &str) -> bool {
    matches!(code, ENDED | STOPPED) || crate::terminal::ended_code(code)
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

const GIVEN_AT_LAUNCH: &str =
    "none loaded by the harness itself; CrazyAgent gives its instructions at launch";
const NO_RECORD: &str = "A headless run reports nothing here while it keeps no transcript.";

/// The lines saying what an agent's harness reported in its own session records: the
/// instruction file names it loaded and the skills it invoked, then each file's place. The
/// words are the same for every harness (DEC-141 PA-14): with no file loaded, the line says
/// CrazyAgent gave the instructions at launch, and before any record it says a headless run
/// that keeps no transcript reports nothing. Inputs: the view, the box width and context.
/// Output: the lines and a blank line.
fn reported_lines(view: &AgentInstructions, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
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
        _ => (listed(&names, GIVEN_AT_LAUNCH), listed(&used, "none")),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for text in [
        format!("Instructions: {files}"),
        format!("Skills used: {skills}"),
    ] {
        lines.extend(wrap(&plain(&text), limit).into_iter().map(Line::raw));
    }
    if view.sessions == 0 {
        lines.extend(
            wrap(NO_RECORD, limit)
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
