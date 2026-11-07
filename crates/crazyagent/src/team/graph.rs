use super::{
    AgentRole, AgentStatus, TaskSource, TaskStatus, TeamAgent, TeamStatus, bound_text, error,
    lead_tree,
};
use crate::coord::{AgentState, Party, Presence, Receipt, Snapshot, Waiting};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::{AGENT_ID_BYTES_MAX, AgentId, KEPT_MAX, PANES_PER_PROJECT_MAX};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const MASTER_NODE: &str = "master";
/// The end of a message edge that is the owner, or the engine's own notes, not a node.
pub const OWNER_PARTY: &str = "owner";
pub const ENGINE_PARTY: &str = "engine";
/// The most message and overlap edges one graph carries, so the coordination lines never
/// crowd the tree out of one owner reply; an overlap's reason is cut to
/// `OVERLAP_REASON_BYTES_MAX`.
pub const MESSAGE_EDGES_MAX: usize = 32;
pub const OVERLAP_EDGES_MAX: usize = 16;
pub const OVERLAP_REASON_BYTES_MAX: usize = 160;
/// The longest task text a handoff relation carries; the task's own edge keeps it whole.
pub const HANDOFF_TASK_BYTES_MAX: usize = 160;
/// The most coordination rows one relation read scans, newest first.
const RELATION_SCAN_MAX: i64 = 256;
/// How many stopped sub-agents the graph keeps listed, newest first: one full round of a
/// project's terminals, so the agents of the latest work stay beside their tasks.
pub const STOPPED_SHOWN_MAX: usize = PANES_PER_PROJECT_MAX;
const TEAMMATE_PREFIX: &str = "teammate-";
const INSTRUCTIONS_CHANGED: &str = "Instructions changed";

const _: () = assert!(
    KEPT_MAX * (AGENT_ID_BYTES_MAX - 2)
        <= (crate::protocol::FRAME_BYTES_MAX - super::STATUS_REPLY_BYTES_MAX) / 2
);

/// What one node of the who-leads-whom tree stands for (DEC-104 Q16): the master, a harness
/// lead the master started, a sub-agent a lead started, or a native teammate a lead's
/// harness split off in its own pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Master,
    Lead,
    Sub,
    Teammate,
}

impl NodeKind {
    /// Tells whether the node is a team agent rather than the master: a harness lead, a
    /// sub-agent or a native teammate. Returns the answer.
    pub fn agent(self) -> bool {
        self != Self::Master
    }
}

/// One node of a project's team graph. `parent` names the node that leads it, none for the
/// master. `branch` is the branch it commits on, `terminal` the name to attach to, and
/// `questions` its question events still queued for the master. `agent` is its own team
/// agent row and `pane` a native teammate's pane in its lead's terminal, for Stop.
/// `presence`, `waiting` (on the owner in its terminal), `owner_questions` (open For: Me
/// questions) and `no_worktree` (a teammate working with none, Q17) come from coordination.
/// `hookless` marks an agent whose launch fires no hooks, so it has no receipts; its adoption
/// counts and flags come from its transcript like any other agent's (OQ-8). `message_note`
/// is its agent row's sentence when the Delivery setting leaves it no way to get messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: NodeKind,
    pub parent: Option<String>,
    pub label: String,
    pub harness: String,
    pub status: Option<AgentStatus>,
    pub status_label: Option<String>,
    pub account: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub terminal: Option<AgentId>,
    #[serde(default)]
    pub questions: Option<u16>,
    #[serde(default)]
    pub agent: Option<i64>,
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub presence: Option<Presence>,
    #[serde(default)]
    pub waiting: Option<Waiting>,
    #[serde(default)]
    pub owner_questions: u16,
    #[serde(default)]
    pub no_worktree: bool,
    #[serde(default)]
    pub hookless: bool,
    #[serde(default)]
    pub message_note: Option<String>,
}

/// Where one handed-out task stands, as the graph draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeState {
    Sent,
    Running,
    Done,
    Failed,
}

/// One assignment edge: a task from the node that leads its agent to the agent running it.
/// `to` is that agent's own node id even while the agent is not drawn, so its task never
/// counts for another node; such a task comes from the drawn lead that handed it out, else
/// from the master, and one no agent took yet goes from the master to the master. `text` is
/// the task, `reply` the result or the plain problem once it ended, and `source` tells a task
/// the master handed out from one the owner handed off. `lead` is the drawn node of the lead
/// that handed it out, none for a task of the master or the owner. `agent_label` is the name
/// of the sub-agent or teammate that ran it, drawn or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub task_id: i64,
    pub source: TaskSource,
    pub text: String,
    pub state: EdgeState,
    pub reply: Option<String>,
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub agent_label: Option<String>,
}

/// Unread messages from one party to another: how many, and the newest one's receipt for
/// its ticks. Each end is a node id, `OWNER_PARTY` or `ENGINE_PARTY`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEdge {
    pub from: String,
    pub to: String,
    pub unread: u32,
    pub receipt: Receipt,
}

/// Active claims of two agents that may touch the same thing, drawn as a red line: how
/// many claim pairs overlap, the first one's plain reason, and whether every pair has a
/// stale side, from an agent that is Away or Stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlapEdge {
    pub from: String,
    pub to: String,
    pub count: u32,
    pub reason: String,
    pub stale: bool,
}

/// How the upper node of one tree pair relates to the lower one, as in the approved mockup:
/// the master leads a top-level agent that leads workers of its own and delegated to one
/// that leads none, and every agent or native teammate under a lead is its teammate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Leads,
    DelegatedTo,
    TeammateOf,
}

/// What one tree pair is doing now: the upper node hands the lower one an open task, the
/// lower one waits on the owner's answer to its open question (`owner`) or one of them on
/// an open question it asked the other, the lower one reviews a worker's review-ready work,
/// named by its change set (coordination work) number, or unread messages wait between them.
/// No question or message text crosses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LiveAction {
    HandingOff { task: String },
    Messaging,
    WaitingOn { owner: bool },
    Reviewing { change_set: i64 },
}

/// The one relation edge of a tree pair, from the node that leads to the node it leads,
/// with the pair's live action while one holds. A handed-off task is cut to
/// `HANDOFF_TASK_BYTES_MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationEdge {
    pub from: String,
    pub to: String,
    pub relation: Relation,
    pub live: Option<LiveAction>,
}

/// A project's team as a tree of who leads whom: the master, its harness leads, their
/// sub-agents and native teammates, the assignment edges between them, the unread
/// message and claim overlap edges of the coordination layer, and one relation edge per
/// tree pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamGraph {
    pub project_id: ProjectId,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub messages: Vec<MessageEdge>,
    #[serde(default)]
    pub overlaps: Vec<OverlapEdge>,
    #[serde(default)]
    pub relations: Vec<RelationEdge>,
}

/// What the graph reads beside the team status: each sub-agent's branch, the project's
/// coordination snapshot, each review-ready change set with its owner, and each asker and
/// recipient of an open question between agents. The default draws the tree with no
/// branch, presence, coordination lines or live action but handoffs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphFacts {
    pub branches: BTreeMap<i64, String>,
    pub coord: Option<Snapshot>,
    pub review_ready: Vec<(i64, i64)>,
    pub asking: BTreeSet<(i64, i64)>,
}

fn edge_state(status: TaskStatus) -> EdgeState {
    match status {
        TaskStatus::Queued | TaskStatus::NeedsPool => EdgeState::Sent,
        TaskStatus::Running => EdgeState::Running,
        TaskStatus::Done => EdgeState::Done,
        TaskStatus::Failed | TaskStatus::Cancelled => EdgeState::Failed,
    }
}

/// The status words an agent's node shows: its status, and after it "Instructions changed"
/// while the owner's Badge choice flags that its instructions changed since it started.
/// Input: the agent. Output: the words.
fn status_words(agent: &TeamAgent) -> String {
    match agent.instructions_changed {
        true => format!("{} - {INSTRUCTIONS_CHANGED}", agent.status_label),
        false => agent.status_label.clone(),
    }
}

/// Tells whether an agent row is a team member the graph places under a lead: a sub-agent or
/// a native teammate the engine adopted as its own row. Takes the agent. Returns the answer.
fn team_member(agent: &TeamAgent) -> bool {
    matches!(agent.role, AgentRole::Sub | AgentRole::Teammate)
}

fn stopped_shown(agents: &[TeamAgent]) -> Vec<i64> {
    let mut stopped: Vec<i64> = agents
        .iter()
        .filter(|agent| team_member(agent) && agent.status == AgentStatus::Stopped)
        .map(|agent| agent.id)
        .collect();
    stopped.sort_unstable_by(|a, b| b.cmp(a));
    stopped.truncate(STOPPED_SHOWN_MAX);
    stopped
}

/// The node id of a native teammate: its harness agent and the digits of its pane id, so
/// it stays plain text in client element ids. Takes the agent and the pane id, such as
/// `%12`. Returns the id, such as `teammate-7-12`.
fn teammate_node(agent: i64, pane: &str) -> String {
    format!(
        "{TEAMMATE_PREFIX}{agent}-{}",
        pane.strip_prefix('%').unwrap_or(pane)
    )
}

/// Reads what the graph shows beside a status: `branches`, `crate::coord::snapshot`,
/// `review_ready` and `asking`. Takes a connection, the status and the time in ms. Returns
/// the facts, or a failure of any read.
pub fn facts(
    connection: &Connection,
    status: &TeamStatus,
    now: i64,
) -> Result<GraphFacts, EngineError> {
    Ok(GraphFacts {
        branches: branches(connection, status)?,
        coord: Some(crate::coord::snapshot(connection, &status.project_id, now)?),
        review_ready: review_ready(connection, &status.project_id)?,
        asking: asking(connection, &status.project_id)?,
    })
}

/// Reads a project's review-ready work, which each owner's reviewer reads next: each owner
/// once with its newest change set, newest first, at most `RELATION_SCAN_MAX` owners. Takes a
/// connection and the project. Returns (owner, change set number) pairs, or a SQLite failure.
fn review_ready(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<(i64, i64)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT owner_agent,id,MAX(updated) AS last FROM coord_work
             WHERE project=?1 AND state='review_ready' AND owner_agent IS NOT NULL
             GROUP BY owner_agent ORDER BY last DESC, id DESC LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), RELATION_SCAN_MAX], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?;
    rows.collect::<Result<_, _>>().map_err(error)
}

/// Reads each asker and recipient of a project's open questions between two agents, one
/// neither replied to nor ended: each pair once, newest first, at most `RELATION_SCAN_MAX`
/// pairs. Takes a connection and the project. Returns the pairs as (asker, recipient), or a
/// SQLite failure.
fn asking(
    connection: &Connection,
    project: &ProjectId,
) -> Result<BTreeSet<(i64, i64)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT from_agent,to_agent FROM coord_messages
             WHERE project=?1 AND kind='question' AND from_kind='agent' AND to_kind='agent'
             AND from_agent IS NOT NULL AND to_agent IS NOT NULL
             AND receipt NOT IN ('replied','failed','expired')
             GROUP BY from_agent,to_agent ORDER BY MAX(id) DESC LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), RELATION_SCAN_MAX], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?;
    rows.collect::<Result<_, _>>().map_err(error)
}

/// Reads the branch each team member of a status commits on, from its worktree record and the
/// landing store, with no git run. A branch the landing store cannot read, as on a platform
/// without landing, is left out, as the harness branch line does. Takes a connection and the
/// status. Returns the branches by agent id, or a SQLite failure of the worktree read.
pub fn branches(
    connection: &Connection,
    status: &TeamStatus,
) -> Result<BTreeMap<i64, String>, EngineError> {
    let mut found = BTreeMap::new();
    for agent in status.agents.iter().filter(|agent| team_member(agent)) {
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![agent.id, status.project_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((worktree, git_dir)) = row else {
            continue;
        };
        let branch =
            crate::landing::agent_branch(connection, Path::new(&worktree), Path::new(&git_dir))
                .ok()
                .flatten();
        if let Some(branch) = branch {
            found.insert(agent.id, branch);
        }
    }
    assert!(found.len() <= status.agents.len());
    Ok(found)
}

/// Names the terminal of each stopped or failed sub-agent and teammate row whose host still
/// keeps its last screen, so its card opens that finished screen read-only (N-D21). A live,
/// lead or master node, a teammate pane of its lead and a row whose screen is gone keep what
/// `graph` gave them. Takes the graph and whether the host keeps an agent's last screen.
/// Returns nothing; at most `KEPT_MAX` nodes gain a name, as no host keeps more screens.
pub fn name_kept(graph: &mut TeamGraph, kept: impl Fn(&AgentId) -> bool) {
    let mut named = 0;
    for node in &mut graph.nodes {
        let finished = matches!(node.kind, NodeKind::Sub | NodeKind::Teammate)
            && node.status.is_some_and(|status| !status.live())
            && node.terminal.is_none();
        let Some(terminal) = node
            .agent
            .filter(|_| finished)
            .and_then(|id| AgentId::team(id).ok())
        else {
            continue;
        };
        if kept(&terminal) {
            node.terminal = Some(terminal);
            named += 1;
        }
    }
    assert!(
        named <= KEPT_MAX,
        "no host keeps more last screens than KEPT_MAX"
    );
}

/// Builds a project's who-leads-whom graph from its team status and the facts read beside
/// it. The master is on top; a sub-agent with no lead is a harness lead under it, and one
/// with a listed lead sits under that lead, as `lead_tree` orders them, a teammate row too.
/// Live and failed members and the `STOPPED_SHOWN_MAX` newest stopped ones show, except
/// finished ones while the owner hides them; a hidden lead's agents move up to the master.
/// Each shown harness's open teammate panes show under it, ended ones too while finished
/// agents show; a pane the engine adopted as a teammate row shows once, as that row. Every
/// listed task is an edge from the node leading its agent to that agent, named by it; a task
/// whose agent is not drawn still goes to that agent, from the lead that handed it out when
/// that lead is drawn, else from the master. Unread messages and claim overlaps between shown agents are edges too, and each tree pair has one `relation_edges` edge. Takes the status and the
/// facts. Returns the master first, then the tree depth first, and edges in task order.
pub fn graph(status: &TeamStatus, facts: &GraphFacts) -> TeamGraph {
    let stopped = stopped_shown(&status.agents);
    let shown = |agent: &TeamAgent| {
        team_member(agent)
            && (status.show_finished || !agent.finished)
            && (agent.status != AgentStatus::Stopped || stopped.contains(&agent.id))
    };
    let states: BTreeMap<i64, &AgentState> = facts
        .coord
        .iter()
        .flat_map(|coord| &coord.agents)
        .map(|state| (state.agent, state))
        .collect();
    let mut asked: BTreeMap<i64, u16> = BTreeMap::new();
    for question in facts
        .coord
        .iter()
        .flat_map(|coord| &coord.questions_for_owner)
    {
        if let Party::Agent(agent) = question.from {
            let count = asked.entry(agent).or_default();
            *count = count.saturating_add(1);
        }
    }
    let masters = || {
        status
            .agents
            .iter()
            .rev()
            .filter(|agent| agent.role == AgentRole::Master)
    };
    let master = masters()
        .find(|agent| agent.status.live())
        .or_else(|| masters().next());
    let master_harness = status
        .harnesses
        .iter()
        .find(|harness| harness.role == AgentRole::Master);
    let master_state = master.and_then(|agent| states.get(&agent.id));
    let mut nodes = vec![GraphNode {
        id: MASTER_NODE.into(),
        kind: NodeKind::Master,
        parent: None,
        label: "Master".into(),
        harness: master_harness
            .map(|harness| harness.harness.clone())
            .unwrap_or_default(),
        status: master.map(|agent| agent.status),
        status_label: master.map(status_words),
        account: master.and_then(|agent| agent.account.clone()),
        branch: status.master_branch.clone(),
        terminal: master
            .filter(|agent| agent.status.live())
            .and_then(|agent| AgentId::team(agent.id).ok()),
        questions: None,
        agent: master.map(|agent| agent.id),
        pane: None,
        presence: master_state.map(|state| state.presence),
        waiting: master_state.and_then(|state| state.waiting),
        owner_questions: master.map_or(0, |agent| asked.get(&agent.id).copied().unwrap_or(0)),
        no_worktree: false,
        hookless: master.is_some_and(|agent| agent.hookless),
        message_note: master.and_then(|agent| agent.message_note.clone()),
    }];
    let listed: Vec<&TeamAgent> = status.agents.iter().filter(|agent| shown(agent)).collect();
    let adopted: BTreeSet<(i64, &str)> = status
        .agents
        .iter()
        .filter(|agent| agent.role == AgentRole::Teammate)
        .filter_map(|agent| Some((agent.parent?, agent.pane.as_deref()?)))
        .collect();
    let ids: BTreeSet<i64> = listed.iter().map(|agent| agent.id).collect();
    let mut agents = BTreeSet::new();
    let mut teammates = 0;
    for (agent, _) in lead_tree(&listed) {
        let Ok(terminal) = AgentId::team(agent.id) else {
            continue;
        };
        let id = terminal.to_string();
        let lead = agent
            .parent
            .filter(|lead| *lead != agent.id && ids.contains(lead));
        let parent = lead.map_or_else(
            || MASTER_NODE.to_owned(),
            |lead| AgentId::team(lead).map_or_else(|_| MASTER_NODE.to_owned(), |l| l.to_string()),
        );
        assert!(parent == MASTER_NODE || agents.contains(&parent));
        let live = agent.status.live() && !agent.owner_stopped;
        let state = states.get(&agent.id);
        agents.insert(id.clone());
        nodes.push(GraphNode {
            id: id.clone(),
            kind: if agent.role == AgentRole::Teammate {
                NodeKind::Teammate
            } else if agent.parent.is_some_and(|lead| lead != agent.id) {
                NodeKind::Sub
            } else {
                NodeKind::Lead
            },
            parent: Some(parent),
            label: agent.label.clone(),
            harness: agent.harness.clone(),
            status: Some(agent.status),
            status_label: Some(status_words(agent)),
            account: agent.account.clone(),
            branch: facts.branches.get(&agent.id).cloned(),
            terminal: agent.status.live().then(|| terminal.clone()),
            questions: Some(
                status
                    .events
                    .questions
                    .iter()
                    .find(|item| item.agent == agent.id)
                    .map_or(0, |item| item.count),
            ),
            agent: Some(agent.id),
            pane: agent.pane.clone(),
            presence: state.map(|state| state.presence),
            waiting: state.and_then(|state| state.waiting),
            owner_questions: asked.get(&agent.id).copied().unwrap_or(0),
            no_worktree: agent.role == AgentRole::Teammate
                && state.is_some_and(|state| !state.has_worktree),
            hookless: agent.hookless,
            message_note: agent.message_note.clone(),
        });
        for mate in agent.teammates.iter().filter(|mate| {
            (status.show_finished || !mate.ended)
                && !adopted.contains(&(agent.id, mate.pane.as_str()))
        }) {
            teammates += 1;
            let open = !mate.ended && !mate.stopped;
            nodes.push(GraphNode {
                id: teammate_node(agent.id, &mate.pane),
                kind: NodeKind::Teammate,
                parent: Some(id.clone()),
                label: format!("Teammate {}", mate.pane),
                harness: agent.harness.clone(),
                status: (!open).then_some(AgentStatus::Stopped),
                status_label: Some(
                    match (mate.ended, mate.stopped) {
                        (false, false) => "Running",
                        (true, _) => "Ended",
                        (false, true) => "Stopped",
                    }
                    .into(),
                ),
                account: agent.account.clone(),
                branch: None,
                terminal: (open && live).then(|| terminal.clone()),
                questions: None,
                agent: None,
                pane: Some(mate.pane.clone()),
                presence: None,
                waiting: None,
                owner_questions: 0,
                no_worktree: false,
                hookless: agent.hookless,
                message_note: None,
            });
        }
    }
    let lead_of: BTreeMap<&str, &str> = nodes
        .iter()
        .filter_map(|node| Some((node.id.as_str(), node.parent.as_deref()?)))
        .collect();
    let masters: BTreeSet<i64> = masters().map(|agent| agent.id).collect();
    let labels: BTreeMap<i64, &str> = status
        .agents
        .iter()
        .map(|agent| (agent.id, agent.label.as_str()))
        .collect();
    let edges: Vec<GraphEdge> = status
        .tasks
        .iter()
        .map(|task| {
            let drawn = |agent: i64| {
                AgentId::team(agent)
                    .ok()
                    .map(|agent| agent.to_string())
                    .filter(|agent| agents.contains(agent))
            };
            let ran = task.agent.filter(|agent| !masters.contains(agent));
            let to = ran
                .and_then(|agent| AgentId::team(agent).ok())
                .map_or_else(|| MASTER_NODE.to_owned(), |agent| agent.to_string());
            let lead = task.parent.and_then(drawn);
            let from = lead_of
                .get(to.as_str())
                .map(|node| (*node).to_owned())
                .or_else(|| lead.clone())
                .unwrap_or_else(|| MASTER_NODE.to_owned());
            assert!(from == MASTER_NODE || agents.contains(&from));
            GraphEdge {
                from,
                to,
                task_id: task.id,
                source: task.source,
                text: task.prompt.clone(),
                state: edge_state(task.status),
                reply: match task.status {
                    TaskStatus::Done => task.result.clone(),
                    TaskStatus::Failed | TaskStatus::Cancelled => task.problem.clone(),
                    _ => None,
                },
                lead,
                agent_label: ran
                    .and_then(|agent| labels.get(&agent))
                    .map(|label| (*label).to_owned()),
            }
        })
        .collect();
    let end = |party: Party| -> Option<String> {
        match party {
            Party::Owner => Some(OWNER_PARTY.into()),
            Party::Engine => Some(ENGINE_PARTY.into()),
            Party::Agent(id) if masters.contains(&id) => Some(MASTER_NODE.into()),
            Party::Agent(id) => AgentId::team(id)
                .ok()
                .map(|terminal| terminal.to_string())
                .filter(|node| agents.contains(node)),
        }
    };
    let (messages, overlaps) = facts
        .coord
        .as_ref()
        .map(|coord| (message_edges(coord, &end), overlap_edges(coord, &end)))
        .unwrap_or_default();
    let named = |agent: &i64| end(Party::Agent(*agent));
    let waits: BTreeSet<(String, String)> = facts
        .asking
        .iter()
        .filter_map(|(asker, asked)| Some((named(asker)?, named(asked)?)))
        .collect();
    let mut reviews: BTreeMap<String, i64> = BTreeMap::new();
    for (owner, change_set) in &facts.review_ready {
        let Some(agent) = status.agents.iter().find(|agent| agent.id == *owner) else {
            continue;
        };
        let reviewer = match agent.parent.filter(|lead| lead != owner) {
            Some(lead) => named(&lead),
            None => Some(MASTER_NODE.to_owned()),
        };
        if let Some(reviewer) = reviewer {
            reviews.entry(reviewer).or_insert(*change_set);
        }
    }
    let relations = relation_edges(&nodes, &edges, &messages, &waits, &reviews);
    let unique: BTreeSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(unique.len(), nodes.len());
    assert!(nodes.iter().all(|node| {
        node.parent
            .as_deref()
            .is_none_or(|parent| unique.contains(parent))
    }));
    assert_eq!(nodes.len(), 1 + agents.len() + teammates);
    assert_eq!(edges.len(), status.tasks.len());
    assert!(messages.len() <= MESSAGE_EDGES_MAX && overlaps.len() <= OVERLAP_EDGES_MAX);
    TeamGraph {
        project_id: status.project_id.clone(),
        nodes,
        edges,
        messages,
        overlaps,
        relations,
    }
}

/// The relation edge from each node's parent to it: under the master Leads when the node
/// leads others, else DelegatedTo, and TeammateOf under a lead; live with the newest open
/// task handed down, else the node's open question for the owner, else an open question
/// either way, else the node's review of its newest review-ready change set, else an unread
/// message either way. Takes the nodes, task and message edges, (asker, asked) pairs and
/// each reviewer's change set by node id. Returns one edge per node but the master's.
fn relation_edges(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    messages: &[MessageEdge],
    waits: &BTreeSet<(String, String)>,
    reviews: &BTreeMap<String, i64>,
) -> Vec<RelationEdge> {
    nodes
        .iter()
        .filter_map(|node| {
            let from = node.parent.clone()?;
            let to = node.id.clone();
            let pair = |a: &str, b: &str| (a == from && b == to) || (a == to && b == from);
            let live = edges
                .iter()
                .rev()
                .find(|edge| {
                    edge.from == from
                        && edge.to == to
                        && matches!(edge.state, EdgeState::Sent | EdgeState::Running)
                })
                .map(|edge| LiveAction::HandingOff {
                    task: bound_text(&edge.text, HANDOFF_TASK_BYTES_MAX),
                })
                .or_else(|| {
                    (node.owner_questions > 0).then_some(LiveAction::WaitingOn { owner: true })
                })
                .or_else(|| {
                    waits
                        .iter()
                        .any(|(asker, asked)| pair(asker, asked))
                        .then_some(LiveAction::WaitingOn { owner: false })
                })
                .or_else(|| {
                    reviews.get(&to).map(|change_set| LiveAction::Reviewing {
                        change_set: *change_set,
                    })
                })
                .or_else(|| {
                    messages
                        .iter()
                        .any(|edge| edge.unread > 0 && pair(&edge.from, &edge.to))
                        .then_some(LiveAction::Messaging)
                });
            let leads = nodes
                .iter()
                .any(|other| other.parent.as_deref() == Some(to.as_str()));
            let relation = match (from == MASTER_NODE, leads) {
                (true, true) => Relation::Leads,
                (true, false) => Relation::DelegatedTo,
                (false, _) => Relation::TeammateOf,
            };
            Some(RelationEdge {
                from,
                to,
                relation,
                live,
            })
        })
        .collect()
}

/// The unread message edges between parties the graph shows, one per pair once each party
/// is named by `end`, with the counts added and the first receipt the snapshot gave kept,
/// at most `MESSAGE_EDGES_MAX`. Takes the snapshot and the namer. Returns the edges.
fn message_edges(coord: &Snapshot, end: &impl Fn(Party) -> Option<String>) -> Vec<MessageEdge> {
    let mut merged: Vec<MessageEdge> = Vec::new();
    for (from, to, unread, receipt) in &coord.unread_edges {
        let (Some(from), Some(to)) = (end(*from), end(*to)) else {
            continue;
        };
        if from == to {
            continue;
        }
        if let Some(edge) = merged
            .iter_mut()
            .find(|edge| edge.from == from && edge.to == to)
        {
            edge.unread = edge.unread.saturating_add(*unread);
        } else if merged.len() < MESSAGE_EDGES_MAX {
            merged.push(MessageEdge {
                from,
                to,
                unread: *unread,
                receipt: *receipt,
            });
        }
    }
    merged
}

/// The claim overlap edges between agents the graph shows, one per pair in either order,
/// counting the overlapping claim pairs and keeping the first reason cut to
/// `OVERLAP_REASON_BYTES_MAX`, at most `OVERLAP_EDGES_MAX`. Takes the snapshot and the
/// namer. Returns the edges.
fn overlap_edges(coord: &Snapshot, end: &impl Fn(Party) -> Option<String>) -> Vec<OverlapEdge> {
    let mut merged: Vec<OverlapEdge> = Vec::new();
    for overlap in &coord.overlaps {
        let (Some(mine), Some(theirs)) = (
            end(Party::Agent(overlap.mine.agent)),
            end(Party::Agent(overlap.theirs.agent)),
        ) else {
            continue;
        };
        if mine == theirs {
            continue;
        }
        let stale = overlap.mine.stale || overlap.theirs.stale;
        if let Some(edge) = merged.iter_mut().find(|edge| {
            (edge.from == mine && edge.to == theirs) || (edge.from == theirs && edge.to == mine)
        }) {
            edge.count = edge.count.saturating_add(1);
            edge.stale &= stale;
        } else if merged.len() < OVERLAP_EDGES_MAX {
            merged.push(OverlapEdge {
                from: mine,
                to: theirs,
                count: 1,
                reason: bound_text(
                    overlap.reasons.first().map_or("", String::as_str),
                    OVERLAP_REASON_BYTES_MAX,
                ),
                stale,
            });
        }
    }
    assert!(
        merged
            .iter()
            .all(|edge| edge.reason.len() <= OVERLAP_REASON_BYTES_MAX)
    );
    merged
}

#[cfg(all(test, unix))]
#[path = "graph/tests.rs"]
mod tests;
