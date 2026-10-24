use super::{AgentStatus, TaskSource};
use crate::protocol::ProjectId;
use crate::terminal::AgentId;
use serde::{Deserialize, Serialize};

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
