use super::*;
use crate::team::{DelegationPath, TeamHarness, TeamTask};

fn agent(id: i64, role: AgentRole, harness: &str, status: AgentStatus) -> TeamAgent {
    TeamAgent {
        id,
        role,
        label: role.agent_label(id),
        harness: harness.into(),
        tools: true,
        account: Some(format!("acct-{id}")),
        worktree: None,
        status,
        status_label: status.label().into(),
        task: None,
        turns: 1,
        problem: None,
        process_group: None,
        created_ms: 1,
        heartbeat_ms: 1,
        stopped_ms: None,
    }
}

fn task(id: i64, harness: &str, status: TaskStatus, agent: Option<i64>) -> TeamTask {
    TeamTask {
        id,
        source: TaskSource::Master,
        harness: harness.into(),
        tools: true,
        prompt: format!("Task {id}"),
        status,
        status_label: status.label().into(),
        agent,
        attempts: 1,
        result: (status == TaskStatus::Done).then(|| format!("Result {id}")),
        problem: (status == TaskStatus::Failed).then(|| format!("Problem {id}")),
        delivered: false,
        output_bytes: 0,
        created_ms: 1,
        started_ms: None,
        finished_ms: None,
    }
}

fn harness(name: &str, role: AgentRole, live: i64, agents_max: u8) -> TeamHarness {
    TeamHarness {
        harness: name.into(),
        role,
        agents_max,
        tools: true,
        live,
        delegation: (role == AgentRole::Master).then_some(DelegationPath::Reply),
        delegation_label: None,
        runnable: true,
    }
}

fn status() -> TeamStatus {
    TeamStatus {
        project_id: ProjectId::new("tidepool").unwrap(),
        keep_warm: false,
        paused: false,
        min_agents: 1,
        max_agents: 4,
        harnesses: vec![
            harness("codex", AgentRole::Master, 1, 1),
            harness("claude-code", AgentRole::Sub, 2, 3),
            harness("codex", AgentRole::Sub, 0, 1),
        ],
        agents: vec![
            agent(1, AgentRole::Master, "codex", AgentStatus::Stopped),
            agent(2, AgentRole::Master, "codex", AgentStatus::Idle),
            agent(3, AgentRole::Sub, "claude-code", AgentStatus::Working),
            agent(4, AgentRole::Sub, "claude-code", AgentStatus::Stopped),
            agent(5, AgentRole::Sub, "claude-code", AgentStatus::Failed),
            agent(6, AgentRole::Sub, "gemini", AgentStatus::Idle),
        ],
        tasks: vec![
            task(10, "claude-code", TaskStatus::Done, Some(4)),
            task(11, "claude-code", TaskStatus::Running, Some(3)),
            task(12, "codex", TaskStatus::Queued, None),
            task(13, "claude-code", TaskStatus::Failed, Some(5)),
            task(14, "opencode", TaskStatus::NeedsPool, None),
        ],
        messages: Vec::new(),
        plan: Vec::new(),
    }
}

#[test]
fn the_team_graph_shows_the_master_harness_groups_agents_and_task_edges() {
    let graph = graph(&status());
    assert_eq!(graph.project_id.as_str(), "tidepool");
    let ids: Vec<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "master",
            "group:claude-code",
            "group:codex",
            "team-3",
            "team-5",
            "team-6"
        ],
        "the stopped sub-agent is left out and the live master is the master node"
    );

    let master = &graph.nodes[0];
    assert_eq!(master.kind, NodeKind::Master);
    assert_eq!(master.harness, "codex");
    assert_eq!(master.status, Some(AgentStatus::Idle));
    assert_eq!(master.account.as_deref(), Some("acct-2"));
    assert_eq!(master.terminal, Some(AgentId::team(2).unwrap()));

    let claude = &graph.nodes[1];
    assert_eq!(claude.kind, NodeKind::HarnessGroup);
    assert_eq!(claude.parent.as_deref(), Some("master"));
    assert_eq!(claude.agents, Some(2));
    assert_eq!(claude.status_label.as_deref(), Some("2 of 3 agents"));
    assert_eq!(claude.terminal, None);

    let working = &graph.nodes[3];
    assert_eq!(working.kind, NodeKind::Agent);
    assert_eq!(working.parent.as_deref(), Some("group:claude-code"));
    assert_eq!(working.status, Some(AgentStatus::Working));
    assert_eq!(working.terminal, Some(AgentId::team(3).unwrap()));
    assert_eq!(
        working.label, "Sub-agent 3",
        "an agent node carries the engine's one name for the agent"
    );
    let failed = &graph.nodes[4];
    assert_eq!(failed.status, Some(AgentStatus::Failed));
    assert_eq!(failed.terminal, None, "a failed agent has no live terminal");
    assert_eq!(
        graph.nodes[5].parent.as_deref(),
        Some("master"),
        "an agent whose harness left the plan hangs off the master"
    );

    let edges: Vec<(i64, &str, EdgeState, Option<&str>)> = graph
        .edges
        .iter()
        .map(|edge| {
            assert_eq!(edge.from, "master");
            assert_eq!(edge.text, format!("Task {}", edge.task_id));
            (
                edge.task_id,
                edge.to.as_str(),
                edge.state,
                edge.reply.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        edges,
        [
            (10, "group:claude-code", EdgeState::Done, Some("Result 10")),
            (11, "team-3", EdgeState::Running, None),
            (12, "group:codex", EdgeState::Sent, None),
            (13, "team-5", EdgeState::Failed, Some("Problem 13")),
            (14, "master", EdgeState::Sent, None),
        ]
    );
}

#[test]
fn a_team_with_no_agents_yet_is_the_master_and_its_groups() {
    let mut empty = status();
    empty.agents.clear();
    empty.tasks.clear();
    let graph = graph(&empty);
    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.nodes[0].status, None);
    assert_eq!(graph.nodes[0].terminal, None);
    assert!(graph.edges.is_empty());
}
