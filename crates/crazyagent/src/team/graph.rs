use super::{AgentRole, AgentStatus, TaskSource, TaskStatus, TeamAgent, TeamStatus};
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, PANES_PER_PROJECT_MAX};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MASTER_NODE: &str = "master";
/// How many stopped sub-agents the graph keeps listed, newest first: one full round of a
/// project's terminals, so the agents of the latest work stay beside their tasks.
pub const STOPPED_SHOWN_MAX: usize = PANES_PER_PROJECT_MAX;

/// What one graph node stands for: the master, one sub-agent harness group, or one sub-agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Master,
    HarnessGroup,
    Agent,
}

/// One node of a project's team graph.
/// `parent` links an agent to its harness group and a group to the master.
/// `terminal` is the name to attach to, for the master and agents with a terminal.
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
    pub agents: Option<u8>,
    pub terminal: Option<AgentId>,
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

/// One task edge from the master to the agent running it, or to its harness group before that.
/// `text` is the task, `reply` the result or the plain problem once it ended.
/// `source` tells a task the master handed out from one the owner handed off.
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

/// A project's team as a graph: the master, harness groups with counts, agents and task edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamGraph {
    pub project_id: ProjectId,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

fn group(harness: &str) -> String {
    format!("group:{harness}")
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

/// Builds a project's team graph from its team status, with no extra read.
/// Takes the status; live and failed sub-agents and the `STOPPED_SHOWN_MAX` newest stopped
/// ones become nodes, and every listed task an edge.
/// Returns the master first, then harness groups, then agents, and edges in task order.
pub fn graph(status: &TeamStatus) -> TeamGraph {
    let stopped = stopped_shown(&status.agents);
    let shown = |agent: &TeamAgent| {
        agent.role == AgentRole::Sub
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
        agents: None,
        terminal: master
            .filter(|agent| agent.status.live())
            .and_then(|agent| AgentId::team(agent.id).ok()),
    }];
    let mut groups = BTreeSet::new();
    for harness in status
        .harnesses
        .iter()
        .filter(|harness| harness.role == AgentRole::Sub)
    {
        if !groups.insert(group(&harness.harness)) {
            continue;
        }
        nodes.push(GraphNode {
            id: group(&harness.harness),
            kind: NodeKind::HarnessGroup,
            parent: Some(MASTER_NODE.into()),
            label: harness.harness.clone(),
            harness: harness.harness.clone(),
            status: None,
            status_label: Some(format!("{} of {} agents", harness.live, harness.agents_max)),
            account: None,
            agents: Some(u8::try_from(harness.live.max(0)).unwrap_or(u8::MAX)),
            terminal: None,
        });
    }
    let mut agents = BTreeSet::new();
    for agent in status.agents.iter().filter(|agent| shown(agent)) {
        let Ok(terminal) = AgentId::team(agent.id) else {
            continue;
        };
        let parent = Some(group(&agent.harness))
            .filter(|parent| groups.contains(parent))
            .unwrap_or_else(|| MASTER_NODE.into());
        agents.insert(terminal.to_string());
        nodes.push(GraphNode {
            id: terminal.to_string(),
            kind: NodeKind::Agent,
            parent: Some(parent),
            label: agent.label.clone(),
            harness: agent.harness.clone(),
            status: Some(agent.status),
            status_label: Some(agent.status_label.clone()),
            account: agent.account.clone(),
            agents: None,
            terminal: agent.status.live().then_some(terminal),
        });
    }
    let edges: Vec<GraphEdge> = status
        .tasks
        .iter()
        .map(|task| {
            let agent = task
                .agent
                .and_then(|agent| AgentId::team(agent).ok())
                .map(|agent| agent.to_string())
                .filter(|agent| agents.contains(agent));
            let fallback = Some(group(&task.harness))
                .filter(|parent| groups.contains(parent))
                .unwrap_or_else(|| MASTER_NODE.into());
            GraphEdge {
                from: MASTER_NODE.into(),
                to: agent.unwrap_or(fallback),
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
    assert!(nodes.len() <= 1 + status.harnesses.len() + status.agents.len());
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
