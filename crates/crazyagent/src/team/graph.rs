use super::{
    AgentRole, AgentStatus, TaskSource, TaskStatus, TeamAgent, TeamStatus, error, lead_tree,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, PANES_PER_PROJECT_MAX};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const MASTER_NODE: &str = "master";
/// How many stopped sub-agents the graph keeps listed, newest first: one full round of a
/// project's terminals, so the agents of the latest work stay beside their tasks.
pub const STOPPED_SHOWN_MAX: usize = PANES_PER_PROJECT_MAX;
const TEAMMATE_PREFIX: &str = "teammate-";

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

/// One assignment edge: a task from the node that leads its agent to the agent running it,
/// or from the master to the master while no agent took it yet. `text` is the task, `reply`
/// the result or the plain problem once it ended, and `source` tells a task the master
/// handed out from one the owner handed off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub task_id: i64,
    pub source: TaskSource,
    pub text: String,
    pub state: EdgeState,
    pub reply: Option<String>,
}

/// A project's team as a tree of who leads whom: the master, its harness leads, their
/// sub-agents and native teammates, and the assignment edges between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamGraph {
    pub project_id: ProjectId,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

fn edge_state(status: TaskStatus) -> EdgeState {
    match status {
        TaskStatus::Queued | TaskStatus::NeedsPool => EdgeState::Sent,
        TaskStatus::Running => EdgeState::Running,
        TaskStatus::Done => EdgeState::Done,
        TaskStatus::Failed | TaskStatus::Cancelled => EdgeState::Failed,
    }
}

fn stopped_shown(agents: &[TeamAgent]) -> Vec<i64> {
    let mut stopped: Vec<i64> = agents
        .iter()
        .filter(|agent| agent.role == AgentRole::Sub && agent.status == AgentStatus::Stopped)
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

/// Reads the branch each sub-agent of a status commits on, from its worktree record and the
/// landing store, with no git run. A branch the landing store cannot read, as on a platform
/// without landing, is left out, as the harness branch line does. Takes a connection and the
/// status. Returns the branches by agent id, or a SQLite failure of the worktree read.
pub fn branches(
    connection: &Connection,
    status: &TeamStatus,
) -> Result<BTreeMap<i64, String>, EngineError> {
    let mut found = BTreeMap::new();
    for agent in status
        .agents
        .iter()
        .filter(|agent| agent.role == AgentRole::Sub)
    {
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
        let branch = crate::landing::agent_branch(
            connection,
            Path::new(&worktree),
            Path::new(&git_dir),
        )
        .ok()
        .flatten();
        if let Some(branch) = branch {
            found.insert(agent.id, branch);
        }
    }
    assert!(found.len() <= status.agents.len());
    Ok(found)
}

/// Builds a project's who-leads-whom graph from its team status and the agents' branches.
/// The master is on top; a sub-agent with no lead is a harness lead under it, and one with
/// a listed lead sits under that lead, as `lead_tree` orders them. Live and failed
/// sub-agents and the `STOPPED_SHOWN_MAX` newest stopped ones show, except finished ones
/// while the owner hides them; a hidden lead's agents move up to the master. Each shown
/// harness's open teammate panes show under it, ended ones too while finished agents show.
/// Every listed task is an edge from the node leading its agent. Takes the status and the
/// branches. Returns the master first, then the tree depth first, and edges in task order.
pub fn graph(status: &TeamStatus, branches: &BTreeMap<i64, String>) -> TeamGraph {
    let stopped = stopped_shown(&status.agents);
    let shown = |agent: &TeamAgent| {
        agent.role == AgentRole::Sub
            && (status.show_finished || !agent.finished)
            && (agent.status != AgentStatus::Stopped || stopped.contains(&agent.id))
    };
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
    let mut nodes = vec![GraphNode {
        id: MASTER_NODE.into(),
        kind: NodeKind::Master,
        parent: None,
        label: "Master".into(),
        harness: master_harness
            .map(|harness| harness.harness.clone())
            .unwrap_or_default(),
        status: master.map(|agent| agent.status),
        status_label: master.map(|agent| agent.status_label.clone()),
        account: master.and_then(|agent| agent.account.clone()),
        branch: status.master_branch.clone(),
        terminal: master
            .filter(|agent| agent.status.live())
            .and_then(|agent| AgentId::team(agent.id).ok()),
        questions: None,
        agent: master.map(|agent| agent.id),
        pane: None,
    }];
    let listed: Vec<&TeamAgent> = status.agents.iter().filter(|agent| shown(agent)).collect();
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
        agents.insert(id.clone());
        nodes.push(GraphNode {
            id: id.clone(),
            kind: if agent.parent.is_some_and(|lead| lead != agent.id) {
                NodeKind::Sub
            } else {
                NodeKind::Lead
            },
            parent: Some(parent),
            label: agent.label.clone(),
            harness: agent.harness.clone(),
            status: Some(agent.status),
            status_label: Some(agent.status_label.clone()),
            account: agent.account.clone(),
            branch: branches.get(&agent.id).cloned(),
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
            pane: None,
        });
        for mate in agent
            .teammates
            .iter()
            .filter(|mate| status.show_finished || !mate.ended)
        {
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
            });
        }
    }
    let lead_of: BTreeMap<&str, &str> = nodes
        .iter()
        .filter_map(|node| Some((node.id.as_str(), node.parent.as_deref()?)))
        .collect();
    let edges: Vec<GraphEdge> = status
        .tasks
        .iter()
        .map(|task| {
            let to = task
                .agent
                .and_then(|agent| AgentId::team(agent).ok())
                .map(|agent| agent.to_string())
                .filter(|agent| agents.contains(agent))
                .unwrap_or_else(|| MASTER_NODE.into());
            GraphEdge {
                from: lead_of
                    .get(to.as_str())
                    .map_or_else(|| MASTER_NODE.to_owned(), |lead| (*lead).to_owned()),
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
            }
        })
        .collect();
    let unique: BTreeSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(unique.len(), nodes.len());
    assert!(nodes.iter().all(|node| {
        node.parent
            .as_deref()
            .is_none_or(|parent| unique.contains(parent))
    }));
    assert_eq!(nodes.len(), 1 + agents.len() + teammates);
    assert_eq!(edges.len(), status.tasks.len());
    TeamGraph {
        project_id: status.project_id.clone(),
        nodes,
        edges,
    }
}

#[cfg(test)]
#[path = "graph/tests.rs"]
mod tests;
