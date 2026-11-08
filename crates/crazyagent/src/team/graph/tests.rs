use super::*;
use crate::team::{DelegationPath, TeamHarness, TeamTask, Teammate};

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
        persona: None,
        handle: None,
        finished: false,
        wakeable: false,
        wake_note: None,
        parent: None,
        owner_stopped: false,
        teammates: Vec::new(),
        pane: None,
        hookless: false,
        instructions_changed: false,
        message_note: None,
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
        parent: None,
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
        turn_limit_secs: None,
        session_limit_secs: None,
        idle_stop_secs: None,
        on_harness_switch: crate::team::SwitchMode::Carry,
        apply_harness_switch: crate::team::SwitchApply::WhenIdle,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        terminals_per_window: crate::team::TerminalsPerWindow::Six,
        show_finished: false,
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        ask_before_merge: false,
        master_lands: None,
        reask_empty_reply: true,
        catch_up_main: crate::team::CatchUpMain::On,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        message_settings: crate::team::agent_messages::MessageSettings::default(),
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        settings_version: 0,
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
            TeamAgent {
                persona: Some("Ada".into()),
                handle: Some("charts".into()),
                label: "Ada (@charts)".into(),
                ..agent(3, AgentRole::Sub, "claude-code", AgentStatus::Working)
            },
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
        events: Default::default(),
        partial: false,
    }
}

fn none() -> GraphFacts {
    GraphFacts::default()
}

fn teammate(pane: &str, ended: bool, stopped: bool) -> Teammate {
    Teammate {
        pane: pane.into(),
        ended,
        stopped,
        seen_ms: 1,
    }
}

/// The base status with a lead tree under harness lead 3: worker 7, its own worker 8, and
/// two teammate panes of 3's terminal, one open and one ended.
fn tree() -> TeamStatus {
    let mut status = status();
    status.master_branch = Some("main".into());
    status.agents[2].teammates = vec![teammate("%12", false, false), teammate("%13", true, true)];
    status.agents.push(TeamAgent {
        parent: Some(3),
        ..agent(7, AgentRole::Sub, "claude-code", AgentStatus::Working)
    });
    status.agents.push(TeamAgent {
        parent: Some(7),
        ..agent(8, AgentRole::Sub, "codex", AgentStatus::Idle)
    });
    status
        .tasks
        .push(task(15, "claude-code", TaskStatus::Running, Some(7)));
    status
        .tasks
        .push(task(16, "codex", TaskStatus::Queued, Some(8)));
    status
}

fn kinds(graph: &TeamGraph) -> Vec<(&str, NodeKind, Option<&str>)> {
    graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.kind, node.parent.as_deref()))
        .collect()
}

fn agents(graph: &TeamGraph) -> Vec<String> {
    graph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Lead | NodeKind::Sub))
        .map(|node| node.id.clone())
        .collect()
}

#[test]
fn the_team_graph_is_a_tree_of_who_leads_whom() {
    let facts = GraphFacts {
        branches: BTreeMap::from([
            (3, "crazyagent/charts".to_owned()),
            (7, "crazyagent/charts--axis".to_owned()),
        ]),
        coord: None,
        ..none()
    };
    let graph = graph(&tree(), &facts);
    assert_eq!(graph.project_id.as_str(), "tidepool");
    assert_eq!(
        kinds(&graph),
        [
            ("master", NodeKind::Master, None),
            ("team-3", NodeKind::Lead, Some("master")),
            ("teammate-3-12", NodeKind::Teammate, Some("team-3")),
            ("team-7", NodeKind::Sub, Some("team-3")),
            ("team-8", NodeKind::Sub, Some("team-7")),
            ("team-4", NodeKind::Lead, Some("master")),
            ("team-5", NodeKind::Lead, Some("master")),
            ("team-6", NodeKind::Lead, Some("master")),
        ],
        "the master leads every harness lead, each lead its own workers depth first, and an ended teammate pane leaves"
    );

    let master = &graph.nodes[0];
    assert_eq!(master.harness, "codex");
    assert_eq!(master.status, Some(AgentStatus::Idle));
    assert_eq!(master.account.as_deref(), Some("acct-2"));
    assert_eq!(master.terminal, Some(AgentId::team(2).unwrap()));
    assert_eq!(master.branch.as_deref(), Some("main"));
    assert_eq!(master.agent, Some(2), "the live master is the master node");
    assert_eq!(master.label, "Master");

    let node = |id: &str| graph.nodes.iter().find(|node| node.id == id).unwrap();
    let lead = node("team-3");
    assert_eq!(lead.label, "Ada (@charts)");
    assert_eq!(lead.status, Some(AgentStatus::Working));
    assert_eq!(lead.terminal, Some(AgentId::team(3).unwrap()));
    assert_eq!(lead.branch.as_deref(), Some("crazyagent/charts"));
    assert_eq!((lead.agent, lead.pane.as_deref()), (Some(3), None));
    assert_eq!(
        node("team-7").branch.as_deref(),
        Some("crazyagent/charts--axis")
    );
    assert_eq!(
        node("team-8").branch,
        None,
        "an agent with no worktree has no branch"
    );

    let mate = node("teammate-3-12");
    assert_eq!(mate.label, "Teammate %12");
    assert_eq!(mate.harness, "claude-code");
    assert_eq!(mate.account.as_deref(), Some("acct-3"));
    assert_eq!(mate.status, None);
    assert_eq!(mate.status_label.as_deref(), Some("Running"));
    assert_eq!(
        mate.terminal,
        Some(AgentId::team(3).unwrap()),
        "an open teammate pane opens its lead's terminal"
    );
    assert_eq!((mate.agent, mate.pane.as_deref()), (None, Some("%12")));
    assert_eq!(mate.questions, None);

    assert_eq!(
        node("team-4").terminal,
        None,
        "a stopped agent has no live terminal"
    );
    assert_eq!(node("team-5").label, "Sub-agent 5");
    assert_eq!(
        node("team-5").terminal,
        None,
        "a failed agent has no live terminal"
    );

    let edges: Vec<(i64, &str, &str, EdgeState, Option<&str>)> = graph
        .edges
        .iter()
        .map(|edge| {
            assert_eq!(edge.text, format!("Task {}", edge.task_id));
            (
                edge.task_id,
                edge.from.as_str(),
                edge.to.as_str(),
                edge.state,
                edge.reply.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        edges,
        [
            (10, "master", "team-4", EdgeState::Done, Some("Result 10")),
            (11, "master", "team-3", EdgeState::Running, None),
            (12, "master", "master", EdgeState::Sent, None),
            (
                13,
                "master",
                "team-5",
                EdgeState::Failed,
                Some("Problem 13")
            ),
            (14, "master", "master", EdgeState::Sent, None),
            (15, "team-3", "team-7", EdgeState::Running, None),
            (16, "team-7", "team-8", EdgeState::Sent, None),
        ],
        "each task runs from the node leading its agent, and one no agent took waits on the master"
    );
}

#[test]
fn a_hidden_lead_moves_its_workers_up_to_the_master() {
    let mut status = tree();
    status.agents[2].finished = true;
    status.agents[2].teammates.clear();
    let graph = graph(&status, &none());
    assert_eq!(
        kinds(&graph),
        [
            ("master", NodeKind::Master, None),
            ("team-4", NodeKind::Lead, Some("master")),
            ("team-5", NodeKind::Lead, Some("master")),
            ("team-6", NodeKind::Lead, Some("master")),
            ("team-7", NodeKind::Sub, Some("master")),
            ("team-8", NodeKind::Sub, Some("team-7")),
        ],
        "a worker keeps its kind while its finished lead is hidden"
    );
    assert_eq!(
        graph
            .edges
            .iter()
            .find(|edge| edge.task_id == 15)
            .map(|edge| (edge.from.as_str(), edge.to.as_str())),
        Some(("master", "team-7"))
    );
}

#[test]
fn stopped_and_ended_teammates_show_as_stopped_and_ended_ones_only_with_finished_agents() {
    let mut status = tree();
    status.agents[2].teammates = vec![teammate("%12", false, true), teammate("%13", true, true)];
    let shown = |graph: &TeamGraph| -> Vec<(String, Option<AgentStatus>, Option<String>, bool)> {
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Teammate)
            .map(|node| {
                (
                    node.id.clone(),
                    node.status,
                    node.status_label.clone(),
                    node.terminal.is_some(),
                )
            })
            .collect()
    };
    assert_eq!(
        shown(&graph(&status, &none())),
        [(
            "teammate-3-12".to_owned(),
            Some(AgentStatus::Stopped),
            Some("Stopped".to_owned()),
            false
        )],
        "a teammate the owner stopped shows stopped with no terminal while its pane closes"
    );
    status.show_finished = true;
    assert_eq!(
        shown(&graph(&status, &none()))[1],
        (
            "teammate-3-13".to_owned(),
            Some(AgentStatus::Stopped),
            Some("Ended".to_owned()),
            false
        )
    );
    status.show_finished = false;
    status.agents[2].teammates = vec![teammate("%12", false, false)];
    status.agents[2].status = AgentStatus::Stopped;
    assert!(
        shown(&graph(&status, &none())).iter().all(|mate| !mate.3),
        "a teammate of a stopped harness has no terminal to open"
    );
}

#[test]
fn finished_sub_agents_leave_the_graph_until_the_owner_shows_them() {
    let held = TeamAgent {
        task: Some(11),
        ..agent(7, AgentRole::Sub, "codex", AgentStatus::Idle)
    };
    assert!(!held.finished_with(0), "an idle agent holding a task waits");
    assert!(!agent(8, AgentRole::Sub, "codex", AgentStatus::Starting).finished_with(0));
    assert!(!agent(2, AgentRole::Master, "codex", AgentStatus::Idle).finished_with(0));
    assert!(!agent(1, AgentRole::Master, "codex", AgentStatus::Stopped).finished_with(0));
    let idle = agent(6, AgentRole::Sub, "gemini", AgentStatus::Idle);
    assert!(
        !idle.finished_with(0),
        "an idle agent that ran a turn waits for work in view"
    );
    assert!(
        TeamAgent {
            status: AgentStatus::Stopped,
            ..idle.clone()
        }
        .finished_with(0),
        "it is finished once it stops"
    );
    assert!(
        !TeamAgent {
            turns: 0,
            ..idle.clone()
        }
        .finished_with(0),
        "an idle agent with no turn since it started, as one just woken, stays"
    );
    assert!(
        !idle.finished_with(1),
        "an agent with a question waiting stays"
    );

    let mut status = status();
    for agent in &mut status.agents {
        agent.finished = agent.finished_with(0);
    }
    let finished: Vec<i64> = status
        .agents
        .iter()
        .filter(|agent| agent.finished)
        .map(|agent| agent.id)
        .collect();
    assert_eq!(
        finished,
        [4, 5],
        "stopped and failed sub-agents are finished"
    );
    let hidden = graph(&status, &none());
    assert_eq!(agents(&hidden), ["team-3", "team-6"]);
    assert_eq!(hidden.nodes[0].kind, NodeKind::Master);
    assert_eq!(
        hidden.edges.len(),
        status.tasks.len(),
        "every task keeps its edge"
    );
    let ran = |graph: &TeamGraph, id: i64| {
        graph
            .edges
            .iter()
            .find(|edge| edge.task_id == id)
            .map(|edge| (edge.from.clone(), edge.to.clone(), edge.agent_label.clone()))
    };
    let own = |from: &str, to: &str, label: Option<&str>| {
        Some((from.to_owned(), to.to_owned(), label.map(str::to_owned)))
    };
    assert_eq!(
        ran(&hidden, 10),
        own("master", "team-4", Some("Sub-agent 4")),
        "a hidden finished agent's task keeps its agent and name, not the master's"
    );
    assert_eq!(
        ran(&hidden, 13),
        own("master", "team-5", Some("Sub-agent 5"))
    );
    assert_eq!(
        ran(&hidden, 11),
        own("master", "team-3", Some("Ada (@charts)"))
    );
    assert_eq!(
        ran(&hidden, 12),
        own("master", "master", None),
        "a task no agent took yet waits on the master"
    );
    status.tasks[0].parent = Some(3);
    status.tasks[3].parent = Some(99);
    status
        .tasks
        .push(task(17, "codex", TaskStatus::Done, Some(2)));
    let led = graph(&status, &none());
    assert_eq!(
        ran(&led, 10),
        own("team-3", "team-4", Some("Sub-agent 4")),
        "a hidden agent's task comes from the drawn lead that handed it out and stays its own"
    );
    assert_eq!(
        ran(&led, 13),
        own("master", "team-5", Some("Sub-agent 5")),
        "with no drawn lead it comes from the master and stays its own"
    );
    assert_eq!(
        ran(&led, 17),
        own("master", "master", None),
        "the master's own task stays on the master"
    );
    status.show_finished = true;
    assert_eq!(
        agents(&graph(&status, &none())),
        ["team-3", "team-4", "team-5", "team-6"]
    );
}

#[test]
fn an_agent_whose_instructions_changed_says_so_in_its_status_until_it_starts_again() {
    let mut changed = tree();
    for agent in &mut changed.agents {
        agent.instructions_changed = matches!(agent.id, 7 | 8) || agent.role == AgentRole::Master;
    }
    let graph = graph(&changed, &none());
    let label = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .and_then(|node| node.status_label.clone())
            .unwrap()
    };
    assert_eq!(label("team-7"), "Working - Instructions changed");
    assert_eq!(label("team-8"), "Waiting for work - Instructions changed");
    assert_eq!(label("team-3"), AgentStatus::Working.label());
    assert!(
        label("master").ends_with(" - Instructions changed"),
        "{}",
        label("master")
    );
}

#[test]
fn a_node_says_when_its_running_task_started_and_nothing_while_it_runs_none() {
    let mut team = status();
    team.show_finished = true;
    team.tasks[0].started_ms = Some(1_600_000_000_000);
    team.tasks[1].started_ms = Some(1_700_000_120_000);
    team.tasks.push(TeamTask {
        started_ms: Some(1_700_000_000_000),
        ..task(15, "claude-code", TaskStatus::Running, Some(3))
    });
    team.tasks.push(TeamTask {
        started_ms: Some(1_700_000_060_000),
        ..task(16, "codex", TaskStatus::Running, Some(2))
    });
    let graph = graph(&team, &none());
    let started = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.started_ms)
    };
    assert_eq!(
        started("team-3"),
        Some(Some(1_700_000_000_000)),
        "the earliest of its two running tasks"
    );
    assert_eq!(started("master"), Some(Some(1_700_000_060_000)));
    assert_eq!(started("team-4"), Some(None), "its only task is done");
    assert_eq!(started("team-6"), Some(None), "idle, no task");
}

#[test]
fn a_team_with_no_agents_yet_is_the_master_alone() {
    let mut empty = status();
    empty.agents.clear();
    empty.tasks.clear();
    let graph = graph(&empty, &none());
    assert_eq!(kinds(&graph), [("master", NodeKind::Master, None)]);
    assert_eq!(graph.nodes[0].status, None);
    assert_eq!(graph.nodes[0].terminal, None);
    assert_eq!(graph.nodes[0].agent, None);
    assert!(graph.edges.is_empty());
}

#[test]
fn only_agent_nodes_carry_their_queued_question_count() {
    let mut status = tree();
    status.events.questions = vec![crate::team::events::AgentQuestions { agent: 3, count: 4 }];
    let graph = graph(&status, &none());
    let questions = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no node {id}"))
            .questions
    };
    assert_eq!(questions("team-3"), Some(4));
    assert_eq!(questions("team-5"), Some(0));
    assert_eq!(questions("master"), None);
    assert_eq!(questions("teammate-3-12"), None);
    let text = serde_json::to_value(&graph.nodes[0]).unwrap();
    let mut old = text.clone();
    for field in [
        "questions",
        "branch",
        "agent",
        "pane",
        "presence",
        "waiting",
        "owner_questions",
        "no_worktree",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    let read = serde_json::from_value::<GraphNode>(old).unwrap();
    assert_eq!(
        (read.questions, read.branch, read.agent, read.pane),
        (None, None, None, None),
        "a node from a build before these fields still reads"
    );
    assert_eq!(
        (
            read.presence,
            read.waiting,
            read.owner_questions,
            read.no_worktree
        ),
        (None, None, 0, false)
    );
    let mut old = serde_json::to_value(&graph).unwrap();
    old.as_object_mut().unwrap().remove("messages");
    old.as_object_mut().unwrap().remove("overlaps");
    let read = serde_json::from_value::<TeamGraph>(old).unwrap();
    assert!(read.messages.is_empty() && read.overlaps.is_empty());
    assert_eq!(
        serde_json::to_value(NodeKind::Teammate).unwrap(),
        serde_json::json!("teammate")
    );
}

#[test]
fn stopped_sub_agents_of_the_latest_round_stay_in_the_graph_with_their_tasks() {
    let mut owner = status();
    owner.harnesses = vec![
        harness("codex", AgentRole::Master, 0, 1),
        harness("codex", AgentRole::Sub, 0, 4),
    ];
    let kept = i64::try_from(crate::terminal::PANES_PER_PROJECT_MAX).unwrap();
    owner.agents = (1..=kept + 2)
        .map(|id| agent(id, AgentRole::Sub, "codex", AgentStatus::Stopped))
        .collect();
    owner.tasks = vec![
        task(1, "codex", TaskStatus::Done, Some(1)),
        task(2, "codex", TaskStatus::Done, Some(3)),
        task(3, "codex", TaskStatus::Done, Some(kept + 2)),
        task(4, "codex", TaskStatus::Done, Some(3)),
    ];
    let graph = graph(&owner, &none());
    let agents: Vec<&GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Lead)
        .collect();
    assert_eq!(
        agents.len(),
        crate::terminal::PANES_PER_PROJECT_MAX,
        "the newest stopped sub-agents stay listed, the oldest leave"
    );
    assert_eq!(agents[0].id, "team-3");
    assert_eq!(
        agents.last().unwrap().label,
        format!("Sub-agent {}", kept + 2)
    );
    for node in &agents {
        assert_eq!(node.status, Some(AgentStatus::Stopped));
        assert_eq!(node.status_label.as_deref(), Some("Stopped"));
        assert_eq!(node.parent.as_deref(), Some("master"));
        assert_eq!(node.terminal, None, "a stopped agent has no live terminal");
    }
    let targets: Vec<(i64, String)> = graph
        .edges
        .iter()
        .map(|edge| (edge.task_id, edge.to.clone()))
        .collect();
    assert_eq!(
        targets,
        [
            (1, "team-1".to_owned()),
            (2, "team-3".to_owned()),
            (3, format!("team-{}", kept + 2)),
            (4, "team-3".to_owned()),
        ],
        "every stopped agent's tasks point at it, an unlisted one's too, so none waits on the master"
    );
    assert!(
        graph.nodes.iter().all(|node| node.id != "team-1"),
        "the oldest stopped agent is not drawn"
    );
    assert_eq!(
        graph.edges[0].agent_label.as_deref(),
        Some("Sub-agent 1"),
        "its task keeps its name"
    );
}

#[cfg(unix)]
#[test]
fn branches_come_from_each_agent_worktree_record_with_no_git_run() {
    let directory = tempfile::tempdir().unwrap();
    let connection = crate::landing::open(directory.path()).unwrap();
    crate::team::initialize(&connection).unwrap();
    let mut status = tree();
    let linked = directory.path().join("worktrees").join("agent-3");
    let own = directory.path().join("worktrees").join("agent-7");
    for (agent, worktree, git_dir) in [
        (
            3,
            &linked,
            directory
                .path()
                .join(".git")
                .join("worktrees")
                .join("agent-3"),
        ),
        (7, &own, crate::agent_git::git_dir(&own)),
        (2, &own, crate::agent_git::git_dir(&own)),
    ] {
        connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    status.project_id.as_str(),
                    directory.path().to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
    }
    let found = branches(&connection, &status).unwrap();
    assert_eq!(
        found,
        BTreeMap::from([
            (3, "crazyagent/agent-3".to_owned()),
            (7, "agent/agent-7".to_owned()),
        ]),
        "a linked worktree and an own clone name their branch by layout, and the master has none"
    );
    status.project_id = ProjectId::new("other").unwrap();
    assert!(
        branches(&connection, &status).unwrap().is_empty(),
        "another project's worktree records are not read"
    );
}

/// OS-8 Wait is visible on the graph: an agent's node names the file it waits for, and an
/// agent waiting for none names none.
#[test]
fn a_node_names_the_file_its_agent_waits_for() {
    let facts = GraphFacts {
        branches: BTreeMap::new(),
        coord: Some(Snapshot {
            agents: vec![
                AgentState {
                    waits_for: Some("src/lexer.rs".into()),
                    ..state(3, Presence::Live, None)
                },
                state(7, Presence::Live, None),
            ],
            overlaps: Vec::new(),
            questions_for_owner: Vec::new(),
            unread_edges: Vec::new(),
        }),
        ..none()
    };
    let graph = graph(&tree(), &facts);
    let node = |id: &str| graph.nodes.iter().find(|node| node.id == id).unwrap();
    assert_eq!(node("team-3").waits_for.as_deref(), Some("src/lexer.rs"));
    assert_eq!(node("team-7").waits_for, None);
}

fn state(agent: i64, presence: Presence, waiting: Option<Waiting>) -> AgentState {
    AgentState {
        agent,
        presence,
        waiting,
        waits_for: None,
        has_worktree: true,
        unread: 0,
        tool_counts: Vec::new(),
        flags: Vec::new(),
    }
}

fn claim(id: i64, agent: i64, stale: bool) -> crate::coord::Claim {
    crate::coord::Claim {
        id,
        agent,
        work: 1,
        pattern: format!("src/{id}.rs"),
        stale,
    }
}

fn question(id: i64, from: Party) -> crate::coord::Message {
    crate::coord::Message {
        id,
        project: ProjectId::new("tidepool").unwrap(),
        from,
        to: Party::Owner,
        kind: crate::coord::MessageKind::Question,
        question: Some((
            crate::coord::QuestionFor::Me,
            crate::coord::Severity::Normal,
        )),
        work: None,
        reply_to: None,
        body: format!("Question {id}"),
        receipt: Receipt::Sent,
        created: 1,
    }
}

#[test]
fn coordination_shows_presence_waits_unread_messages_and_overlaps_between_shown_agents() {
    let overlap = |mine: crate::coord::Claim, theirs: crate::coord::Claim, reason: &str| {
        crate::coord::Overlap {
            mine,
            theirs,
            reasons: vec![reason.into()],
            left_out: false,
        }
    };
    let facts = GraphFacts {
        branches: BTreeMap::new(),
        coord: Some(Snapshot {
            agents: vec![
                state(2, Presence::Live, None),
                state(3, Presence::Idle, Some(Waiting::Permission)),
                state(7, Presence::Away, None),
            ],
            overlaps: vec![
                overlap(claim(1, 3, false), claim(2, 6, false), "same file src/1.rs"),
                overlap(claim(3, 6, false), claim(4, 3, true), "same folder src"),
                overlap(claim(5, 7, true), claim(6, 8, true), &"x".repeat(400)),
                overlap(
                    claim(7, 3, false),
                    claim(8, 99, false),
                    "an agent not shown",
                ),
            ],
            questions_for_owner: vec![
                question(1, Party::Agent(7)),
                question(2, Party::Agent(7)),
                question(3, Party::Engine),
            ],
            unread_edges: vec![
                (Party::Owner, Party::Agent(3), 2, Receipt::Typed),
                (Party::Agent(2), Party::Agent(3), 1, Receipt::Delivered),
                (Party::Agent(1), Party::Agent(3), 4, Receipt::Sent),
                (Party::Agent(3), Party::Agent(7), 1, Receipt::Sent),
                (Party::Engine, Party::Agent(8), 1, Receipt::Sent),
                (Party::Agent(99), Party::Agent(3), 1, Receipt::Sent),
                (Party::Agent(3), Party::Agent(3), 1, Receipt::Sent),
            ],
        }),
        ..none()
    };
    let graph = graph(&tree(), &facts);
    let node = |id: &str| graph.nodes.iter().find(|node| node.id == id).unwrap();
    assert_eq!(
        (node("master").presence, node("master").waiting),
        (Some(Presence::Live), None),
        "the live master's own state shows on the master node"
    );
    assert_eq!(
        (node("team-3").presence, node("team-3").waiting),
        (Some(Presence::Idle), Some(Waiting::Permission))
    );
    assert_eq!(node("team-7").owner_questions, 2);
    assert_eq!(node("team-3").owner_questions, 0);
    assert_eq!(
        (node("team-8").presence, node("teammate-3-12").presence),
        (None, None),
        "an agent the snapshot does not list, and a pane teammate, have no presence"
    );
    let messages: Vec<(&str, &str, u32, Receipt)> = graph
        .messages
        .iter()
        .map(|edge| {
            (
                edge.from.as_str(),
                edge.to.as_str(),
                edge.unread,
                edge.receipt,
            )
        })
        .collect();
    assert_eq!(
        messages,
        [
            ("owner", "team-3", 2, Receipt::Typed),
            ("master", "team-3", 5, Receipt::Delivered),
            ("team-3", "team-7", 1, Receipt::Sent),
            ("engine", "team-8", 1, Receipt::Sent),
        ],
        "every master row is the master node, so its counts add up; an unshown agent and a self message leave"
    );
    let overlaps: Vec<(&str, &str, u32, bool)> = graph
        .overlaps
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str(), edge.count, edge.stale))
        .collect();
    assert_eq!(
        overlaps,
        [
            ("team-3", "team-6", 2, false),
            ("team-7", "team-8", 1, true)
        ],
        "one red line per pair either way, stale only when every overlap has a stale side"
    );
    assert_eq!(graph.overlaps[0].reason, "same file src/1.rs");
    assert_eq!(graph.overlaps[1].reason.len(), OVERLAP_REASON_BYTES_MAX);
}

#[test]
fn coordination_edges_stay_within_their_caps() {
    let mut status = status();
    status.agents = (1..=12)
        .map(|id| agent(id, AgentRole::Sub, "codex", AgentStatus::Working))
        .collect();
    let pairs: Vec<(i64, i64)> = (1..=12)
        .flat_map(|a| (1..=12).map(move |b| (a, b)))
        .filter(|(a, b)| a != b)
        .collect();
    let facts = GraphFacts {
        branches: BTreeMap::new(),
        coord: Some(Snapshot {
            agents: Vec::new(),
            overlaps: pairs
                .iter()
                .filter(|(a, b)| a < b)
                .map(|(a, b)| crate::coord::Overlap {
                    mine: claim(*a, *a, false),
                    theirs: claim(*b, *b, false),
                    reasons: vec!["same folder src".into()],
                    left_out: false,
                })
                .collect(),
            questions_for_owner: Vec::new(),
            unread_edges: pairs
                .iter()
                .map(|(a, b)| (Party::Agent(*a), Party::Agent(*b), 1, Receipt::Sent))
                .collect(),
        }),
        ..none()
    };
    let graph = graph(&status, &facts);
    assert_eq!(graph.messages.len(), MESSAGE_EDGES_MAX);
    assert_eq!(graph.overlaps.len(), OVERLAP_EDGES_MAX);
}

#[test]
fn an_adopted_teammate_row_replaces_its_pane_and_opens_its_own_terminal() {
    let mut status = tree();
    status.agents[5].teammates = vec![teammate("%12", false, false)];
    status.agents.push(TeamAgent {
        parent: Some(3),
        pane: Some("%12".into()),
        persona: Some("Bo".into()),
        label: "Bo".into(),
        ..agent(9, AgentRole::Teammate, "claude-code", AgentStatus::Working)
    });
    let facts = GraphFacts {
        branches: BTreeMap::new(),
        coord: Some(Snapshot {
            agents: vec![AgentState {
                has_worktree: false,
                ..state(9, Presence::Live, None)
            }],
            overlaps: Vec::new(),
            questions_for_owner: Vec::new(),
            unread_edges: vec![(Party::Owner, Party::Agent(9), 1, Receipt::Sent)],
        }),
        ..none()
    };
    type MateRow = (String, Option<String>, Option<i64>, Option<String>);
    let mates = |graph: &TeamGraph| -> Vec<MateRow> {
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Teammate)
            .map(|node| {
                (
                    node.id.clone(),
                    node.parent.clone(),
                    node.agent,
                    node.pane.clone(),
                )
            })
            .collect()
    };
    let shown = graph(&status, &facts);
    assert_eq!(
        mates(&shown),
        [
            (
                "team-9".to_owned(),
                Some("team-3".to_owned()),
                Some(9),
                Some("%12".to_owned())
            ),
            (
                "teammate-6-12".to_owned(),
                Some("team-6".to_owned()),
                None,
                Some("%12".to_owned())
            ),
        ],
        "lead 3's pane %12 shows once, as its row; the same pane id under lead 6 is its own"
    );
    let row = shown.nodes.iter().find(|node| node.id == "team-9").unwrap();
    assert_eq!(row.terminal, Some(AgentId::team(9).unwrap()));
    assert_eq!(row.label, "Bo");
    assert!(
        row.no_worktree,
        "a teammate working with no worktree of its own is amber"
    );
    assert_eq!(row.presence, Some(Presence::Live));
    assert_eq!(
        shown
            .messages
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect::<Vec<_>>(),
        [("owner", "team-9")]
    );
    let last = status.agents.len() - 1;
    status.agents[last].status = AgentStatus::Stopped;
    status.agents[last].finished = true;
    assert_eq!(
        mates(&graph(&status, &facts)),
        [(
            "teammate-6-12".to_owned(),
            Some("team-6".to_owned()),
            None,
            Some("%12".to_owned())
        )],
        "a finished teammate row leaves with its pane while finished agents are hidden"
    );
    status.show_finished = true;
    assert!(
        mates(&graph(&status, &facts))
            .iter()
            .all(|mate| mate.0 != "teammate-3-12"),
        "a shown finished row still stands for its pane"
    );
}

/// The agents list orders each lead's workers under it, depth first and in the given order
/// among siblings (N2): a worker whose lead is not listed starts a tree of its own, and two
/// agents that name each other as lead are each listed once, at the top.
#[test]
fn lead_tree_orders_workers_under_their_leads() {
    let with = |id, parent| TeamAgent {
        parent,
        ..agent(id, AgentRole::Sub, "codex", AgentStatus::Working)
    };
    let agents = [
        with(1, None),
        with(2, None),
        with(3, Some(1)),
        with(4, Some(3)),
        with(5, Some(2)),
        with(6, Some(1)),
        with(7, Some(99)),
        with(8, Some(9)),
        with(9, Some(8)),
    ];
    let listed: Vec<&TeamAgent> = agents.iter().collect();
    let tree: Vec<(i64, usize)> = crate::team::lead_tree(&listed)
        .into_iter()
        .map(|(agent, depth)| (agent.id, depth))
        .collect();
    assert_eq!(
        tree,
        vec![
            (1, 0),
            (3, 1),
            (4, 2),
            (6, 1),
            (2, 0),
            (5, 1),
            (7, 0),
            (8, 0),
            (9, 0)
        ]
    );
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[cfg(unix)]
#[test]
fn branches_name_what_each_real_linked_worktree_and_own_clone_has_checked_out() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let repo = root.join("repo");
    let worktrees = root.join("worktrees");
    for folder in [&repo, &worktrees, &root.join("engine")] {
        std::fs::create_dir(folder).unwrap();
    }
    git(&repo, &["init", "-q", "--template=", "-b", "main"]);
    git(&repo, &["config", "user.name", "Repo Owner"]);
    git(&repo, &["config", "user.email", "owner@example.invalid"]);
    std::fs::write(repo.join("README"), "tidepool\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "Start"]);
    let connection = crate::landing::open(&root.join("engine")).unwrap();
    crate::team::initialize(&connection).unwrap();
    let status = tree();
    let linked = |id: i64, task: Option<&str>| {
        let worktree = worktrees.join(format!("agent-{id}"));
        if let Some(name) = task {
            crate::landing::claim_task_branch(&connection, &repo, &worktree, name).unwrap();
        }
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let git_dir =
            std::path::PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]));
        crate::landing::name_agent_branch(&connection, &repo, &worktree, &git_dir).unwrap();
        (id, worktree, git_dir)
    };
    let clone = |id: i64, task: Option<&str>| {
        let worktree = worktrees.join(format!("agent-{id}"));
        match task {
            Some(name) => {
                let branch =
                    crate::landing::claim_task_branch(&connection, &repo, &worktree, name).unwrap();
                crate::landing::add_agent_clone_on(&repo, &worktree, None, &branch).unwrap();
            }
            None => crate::landing::add_agent_clone(&repo, &worktree).unwrap(),
        }
        let git_dir = crate::agent_git::git_dir(&worktree);
        (id, worktree, git_dir)
    };
    let made = [
        linked(3, Some("charts")),
        linked(6, None),
        clone(4, Some("login")),
        clone(5, None),
    ];
    for (agent, worktree, git_dir) in &made {
        connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    status.project_id.as_str(),
                    repo.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
    }
    let found = branches(&connection, &status).unwrap();
    assert_eq!(
        found,
        BTreeMap::from([
            (3, "crazyagent/charts".to_owned()),
            (4, "crazyagent/login".to_owned()),
            (5, "agent/agent-5".to_owned()),
            (6, "crazyagent/agent-6".to_owned()),
        ]),
        "a task branch the engine stored wins; else a linked worktree is crazyagent/<folder> and an own clone agent/<folder>"
    );
    for (agent, worktree, git_dir) in &made {
        let checked_out = git(
            worktree,
            &[
                "--git-dir",
                git_dir.to_str().unwrap(),
                "--work-tree",
                worktree.to_str().unwrap(),
                "symbolic-ref",
                "--short",
                "HEAD",
            ],
        );
        assert_eq!(
            Some(&checked_out),
            found.get(agent),
            "agent {agent}: the branch read with no git run is the one git has checked out"
        );
    }
}

#[cfg(unix)]
#[test]
fn hookless_agent_marks_its_node() {
    let directory = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let setup = crate::project_setup::StoredSetup {
        project_id: ProjectId::new("team-hookless").unwrap(),
        name: "Team".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 4,
        },
        documents: vec![],
        folder: "/private/tmp/unused".into(),
        created_ms: 1,
        revision: 1,
    };
    connection
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1,?2)",
            rusqlite::params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let agent = |role: &str, hookless: bool| {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless) VALUES ('team-hookless',?1,'codex',0,'working',1,1,?2)",
                rusqlite::params![role, hookless],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let master = agent("master", true);
    let hooked = agent("sub", false);
    let bare = agent("sub", true);
    let graph = crate::team::owner_graph(&connection, &setup, None).unwrap();
    let marked: Vec<(Option<i64>, bool)> = graph
        .nodes
        .iter()
        .map(|node| (node.agent, node.hookless))
        .collect();
    assert_eq!(
        marked,
        [
            (Some(master), true),
            (Some(hooked), false),
            (Some(bare), true)
        ],
        "{:?}",
        graph.nodes
    );
}

/// A stopped or failed sub-agent or teammate row whose host still keeps its last screen names
/// that terminal, so its card opens the finished screen (N-D21). Live nodes, stopped leads, a
/// teammate pane of its lead and a finished row whose screen is gone keep what `graph` gave.
#[test]
fn a_finished_worker_whose_last_screen_is_kept_names_its_terminal() {
    let mut status = tree();
    status.show_finished = true;
    for (id, role, parent, state) in [
        (10, AgentRole::Sub, 3, AgentStatus::Stopped),
        (11, AgentRole::Sub, 7, AgentStatus::Failed),
        (12, AgentRole::Teammate, 3, AgentStatus::Stopped),
        (13, AgentRole::Sub, 3, AgentStatus::Stopped),
    ] {
        status.agents.push(TeamAgent {
            parent: Some(parent),
            finished: true,
            pane: (role == AgentRole::Teammate).then(|| "%14".into()),
            ..agent(id, role, "codex", state)
        });
    }
    let kept: BTreeSet<AgentId> = [3, 4, 5, 7, 10, 11, 12]
        .into_iter()
        .map(|id| AgentId::team(id).unwrap())
        .collect();
    let before = graph(&status, &none());
    let mut after = before.clone();
    name_kept(&mut after, |agent| kept.contains(agent));
    assert_eq!(after.nodes.len(), before.nodes.len());
    let mut named: Vec<(&str, Option<String>)> = after
        .nodes
        .iter()
        .zip(&before.nodes)
        .filter(|(now, was)| now != was)
        .map(|(now, _)| {
            (
                now.id.as_str(),
                now.terminal.as_ref().map(ToString::to_string),
            )
        })
        .collect();
    named.sort();
    assert_eq!(
        named,
        [
            ("team-10", Some("team-10".to_owned())),
            ("team-11", Some("team-11".to_owned())),
            ("team-12", Some("team-12".to_owned())),
        ],
        "only the finished sub-agents and the teammate row with a kept screen gain a terminal"
    );
    let terminal = |graph: &TeamGraph, id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.terminal.clone())
    };
    assert_eq!(
        terminal(&after, "team-13"),
        Some(None),
        "no screen kept, no terminal"
    );
    assert_eq!(
        terminal(&after, "team-4"),
        Some(None),
        "a stopped lead keeps none"
    );
    assert_eq!(
        terminal(&after, "team-5"),
        Some(None),
        "a failed lead keeps none"
    );
    assert_eq!(
        terminal(&after, "teammate-3-13"),
        Some(None),
        "an ended pane of its lead is no row of its own"
    );
    assert_eq!(
        terminal(&after, "team-7"),
        Some(Some(AgentId::team(7).unwrap()))
    );
}

/// A host that claims more kept screens than any host keeps trips the bound that keeps the
/// named graph inside one owner reply.
#[test]
#[should_panic(expected = "no host keeps more last screens")]
fn naming_more_kept_screens_than_a_host_keeps_is_refused() {
    let mut status = tree();
    status.show_finished = true;
    status.agents.extend(
        (0..=crate::terminal::KEPT_MAX as i64).map(|offset| TeamAgent {
            parent: Some(3),
            finished: true,
            ..agent(100 + offset, AgentRole::Sub, "codex", AgentStatus::Failed)
        }),
    );
    let mut shown = graph(&status, &none());
    assert!(
        shown
            .nodes
            .iter()
            .filter(|node| node.status == Some(AgentStatus::Failed) && node.kind == NodeKind::Sub)
            .count()
            > crate::terminal::KEPT_MAX
    );
    name_kept(&mut shown, |_| true);
}

fn relations(graph: &TeamGraph) -> Vec<(&str, &str, Relation, Option<LiveAction>)> {
    graph
        .relations
        .iter()
        .map(|edge| {
            (
                edge.from.as_str(),
                edge.to.as_str(),
                edge.relation,
                edge.live.clone(),
            )
        })
        .collect()
}

fn handing(task: &str) -> Option<LiveAction> {
    Some(LiveAction::HandingOff { task: task.into() })
}

fn unread(edges: Vec<(Party, Party, u32, Receipt)>) -> Option<Snapshot> {
    Some(Snapshot {
        agents: Vec::new(),
        overlaps: Vec::new(),
        questions_for_owner: Vec::new(),
        unread_edges: edges,
    })
}

fn finish(status: &mut TeamStatus, tasks: &[i64]) {
    for task in status
        .tasks
        .iter_mut()
        .filter(|task| tasks.contains(&task.id))
    {
        task.status = TaskStatus::Done;
        task.result = Some(format!("Result {}", task.id));
    }
}

#[test]
fn relations_name_each_pair_and_a_live_handoff_overrides_then_falls_back() {
    let facts = GraphFacts {
        coord: unread(vec![
            (Party::Agent(7), Party::Agent(3), 1, Receipt::Delivered),
            (Party::Agent(8), Party::Agent(7), 1, Receipt::Delivered),
        ]),
        ..none()
    };
    let mut status = tree();
    let shown = graph(&status, &facts);
    assert_eq!(
        relations(&shown),
        [
            ("master", "team-3", Relation::Leads, None),
            ("team-3", "teammate-3-12", Relation::TeammateOf, None),
            (
                "team-3",
                "team-7",
                Relation::TeammateOf,
                Some(LiveAction::Messaging)
            ),
            ("team-7", "team-8", Relation::TeammateOf, handing("Task 16")),
            ("master", "team-4", Relation::DelegatedTo, None),
            ("master", "team-5", Relation::DelegatedTo, None),
            ("master", "team-6", Relation::DelegatedTo, None),
        ],
        "as in the approved mockup the master leads an agent that leads workers and delegates to one that leads none, every agent under a lead is its teammate, a task sent and not yet picked up hands off over an unread message, and a running one leaves its pair's standing word or message"
    );
    let pairs: BTreeSet<(&str, &str)> = shown
        .relations
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    let tree_pairs: BTreeSet<(&str, &str)> = shown
        .nodes
        .iter()
        .filter_map(|node| Some((node.parent.as_deref()?, node.id.as_str())))
        .collect();
    assert_eq!(
        (pairs.len(), &pairs),
        (shown.relations.len(), &tree_pairs),
        "every pair of connected agents has exactly one relation edge"
    );

    finish(&mut status, &[16]);
    let finished = graph(&status, &facts);
    assert_eq!(
        relations(&finished)
            .into_iter()
            .map(|(from, to, _, live)| (from, to, live))
            .take(4)
            .collect::<Vec<_>>(),
        [
            ("master", "team-3", None),
            ("team-3", "teammate-3-12", None),
            ("team-3", "team-7", Some(LiveAction::Messaging)),
            ("team-7", "team-8", Some(LiveAction::Messaging)),
        ],
        "a finished handoff leaves the pair at the unread message still between them"
    );
}

/// A task hands off only while it is sent and not yet picked up, a moment as in the approved
/// mockup: once its agent runs it, the pair shows its standing word again, or the child's
/// wait on the owner's answer, as the agents with no task do (T14-F1).
#[test]
fn a_running_task_leaves_its_pair_s_standing_word_or_owner_wait_and_a_sent_one_hands_off() {
    let mut status = tree();
    let facts = GraphFacts {
        coord: Some(Snapshot {
            questions_for_owner: vec![question(1, Party::Agent(7))],
            ..unread(Vec::new()).unwrap()
        }),
        ..none()
    };
    let shown = |status: &TeamStatus| -> Vec<(String, String, Relation, Option<LiveAction>)> {
        relations(&graph(status, &facts))
            .into_iter()
            .take(4)
            .map(|(from, to, relation, live)| (from.to_owned(), to.to_owned(), relation, live))
            .collect()
    };
    let row =
        |from: &str, to: &str, relation, live| (from.to_owned(), to.to_owned(), relation, live);
    assert_eq!(
        shown(&status),
        [
            row("master", "team-3", Relation::Leads, None),
            row("team-3", "teammate-3-12", Relation::TeammateOf, None),
            row(
                "team-3",
                "team-7",
                Relation::TeammateOf,
                Some(LiveAction::WaitingOn { owner: true })
            ),
            row("team-7", "team-8", Relation::TeammateOf, handing("Task 16")),
        ],
        "a running task hides neither the standing word nor the wait on the owner's answer, and only the task not yet picked up hands off"
    );
    for task in status.tasks.iter_mut().filter(|task| task.id == 16) {
        task.status = TaskStatus::Running;
    }
    assert_eq!(
        shown(&status)[3],
        row("team-7", "team-8", Relation::TeammateOf, None),
        "the handoff ends when its agent picks the task up"
    );
    for task in status.tasks.iter_mut().filter(|task| task.id == 16) {
        task.status = TaskStatus::NeedsPool;
    }
    assert_eq!(
        shown(&status)[3],
        row("team-7", "team-8", Relation::TeammateOf, handing("Task 16")),
        "a task waiting for a pool is still sent and not yet picked up"
    );
}

#[test]
fn waits_reviews_and_messages_show_on_their_pair_under_a_handoff() {
    let mut status = tree();
    status
        .tasks
        .push(task(17, "claude-code", TaskStatus::Queued, Some(3)));
    let long = "x".repeat(400);
    status
        .tasks
        .iter_mut()
        .find(|task| task.id == 16)
        .unwrap()
        .prompt
        .clone_from(&long);
    let mut facts = GraphFacts {
        coord: Some(Snapshot {
            questions_for_owner: vec![question(1, Party::Agent(5)), question(2, Party::Engine)],
            ..unread(vec![(Party::Agent(6), Party::Agent(2), 1, Receipt::Sent)]).unwrap()
        }),
        review_ready: vec![8, 7, 4],
        asking: BTreeSet::from([(7, 3), (8, 7), (5, 2), (99, 3)]),
        ..none()
    };
    let waiting = |owner| Some(LiveAction::WaitingOn { owner });
    let reviewing = |agent: &str| {
        Some(LiveAction::Reviewing {
            agent: agent.into(),
        })
    };
    let cut = handing(&long[..HANDOFF_TASK_BYTES_MAX]);
    assert_eq!(
        relations(&graph(&status, &facts))
            .into_iter()
            .map(|(from, to, _, live)| (from, to, live))
            .collect::<Vec<_>>(),
        [
            ("master", "team-3", handing("Task 17")),
            ("team-3", "teammate-3-12", None),
            ("team-3", "team-7", waiting(false)),
            ("team-7", "team-8", cut),
            ("master", "team-4", None),
            ("master", "team-5", waiting(true)),
            ("master", "team-6", Some(LiveAction::Messaging)),
        ],
        "the newest open task hands off over a wait and a review, an agent with a question for the owner waits on the owner's answer over its question to the master, the master's own review of a top-level agent sits on no line, and a message shows alone"
    );

    finish(&mut status, &[11, 15, 16, 17]);
    let rested = |facts: &GraphFacts| -> Vec<Option<LiveAction>> {
        relations(&graph(&status, facts))
            .into_iter()
            .map(|(_, _, _, live)| live)
            .collect()
    };
    assert_eq!(
        rested(&facts),
        [
            reviewing("Sub-agent 7"),
            None,
            waiting(false),
            waiting(false),
            None,
            waiting(true),
            Some(LiveAction::Messaging),
        ],
        "a lead reviewing its worker's change set shows on the line down to that lead, and a wait between two agents shows over the review"
    );
    facts.asking.remove(&(7, 3));
    facts.coord.as_mut().unwrap().questions_for_owner.clear();
    assert_eq!(
        rested(&facts),
        [
            reviewing("Sub-agent 7"),
            None,
            reviewing("Sub-agent 8"),
            waiting(false),
            None,
            waiting(false),
            Some(LiveAction::Messaging),
        ],
        "once answered, the worker's line names the agent whose change set it reviews as Changes names it, and an agent whose owner question is answered still waits on the master"
    );
}

#[test]
fn relation_facts_read_review_ready_work_and_open_questions_between_agents() {
    use crate::coord::{Finish, MessageKind, NewMessage, QuestionFor, Severity};
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    for project in ["tidepool", "other"] {
        let row = crate::protocol::StoredProject {
            project_id: ProjectId::new(project).unwrap(),
            spec: crate::protocol::ProjectSpec {
                name: project.into(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![project, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
    }
    let add = |project: &str, role: &str, parent: Option<i64>| -> i64 {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent) VALUES (?1,?2,'codex',1,'working',1,1,?3)",
                params![project, role, parent],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let project = ProjectId::new("tidepool").unwrap();
    let other = ProjectId::new("other").unwrap();
    let master = add("tidepool", "master", None);
    let lead = add("tidepool", "sub", None);
    let sub = add("tidepool", "sub", Some(lead));
    add("other", "master", None);
    let stranger = add("other", "sub", None);
    let ready = |project: &ProjectId, agent: i64| {
        let (work, _) =
            crate::coord::start_work(&connection, project, agent, "Tests".into(), vec![], 1)
                .unwrap();
        crate::coord::finish(
            &connection,
            work,
            1,
            Finish::ReviewReady,
            "Green.".into(),
            agent,
            1,
        )
        .unwrap();
        work
    };
    let reviewed = ready(&project, sub);
    ready(&other, stranger);
    crate::coord::start_work(&connection, &project, lead, "Lead".into(), vec![], 1).unwrap();
    let ask = |project: &ProjectId, from: i64, target: QuestionFor| {
        crate::coord::send(
            &connection,
            NewMessage {
                project: project.clone(),
                from: Party::Agent(from),
                to: Party::Owner,
                kind: MessageKind::Question,
                question: Some((target, Severity::Normal)),
                work: None,
                reply_to: None,
                body: "Which branch?".into(),
                idempotency_key: None,
            },
            1,
        )
        .unwrap()
    };
    let asked = ask(&project, sub, QuestionFor::Lead);
    ask(&project, lead, QuestionFor::Master);
    ask(&project, sub, QuestionFor::Me);
    ask(&other, stranger, QuestionFor::Master);

    assert_eq!(
        review_ready(&connection, &project).unwrap(),
        [sub],
        "only this project's review-ready work counts, by the agent that owns it, not active work"
    );
    assert_eq!(
        asking(&connection, &project).unwrap(),
        BTreeSet::from([(sub, lead), (lead, master)]),
        "a question for the lead or the master waits on that agent; one for the owner is no agent's"
    );
    crate::coord::reply(&connection, Party::Agent(lead), asked, "main".into(), 2).unwrap();
    crate::coord::finish(
        &connection,
        reviewed,
        2,
        Finish::Complete,
        "Reviewed.".into(),
        lead,
        2,
    )
    .unwrap();
    assert_eq!(
        (
            review_ready(&connection, &project).unwrap(),
            asking(&connection, &project).unwrap()
        ),
        (Vec::new(), BTreeSet::from([(lead, master)])),
        "an answered question and completed work leave the facts"
    );
}

#[test]
fn relation_facts_keep_each_owner_and_pair_past_one_agents_many_rows() {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let project = ProjectId::new("tidepool").unwrap();
    let work = |owner: i64, updated: i64| -> i64 {
        connection
            .execute(
                "INSERT INTO coord_work(project,title,owner_agent,state,version,created,updated) VALUES ('tidepool','Tests',?1,'review_ready',1,?2,?2)",
                params![owner, updated],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let question = |from: i64, to: i64| {
        connection
            .execute(
                "INSERT INTO coord_messages(project,from_kind,from_agent,to_kind,to_agent,kind,body,receipt,created,receipt_at) VALUES ('tidepool','agent',?1,'agent',?2,'question','Which branch?','sent',1,1)",
                params![from, to],
            )
            .unwrap();
    };
    work(4, 1);
    question(4, 2);
    for at in 0..=RELATION_SCAN_MAX {
        work(3, 10 + at);
        question(3, 2);
    }
    assert_eq!(
        review_ready(&connection, &project).unwrap(),
        [3, 4],
        "one owner's many review-ready rows count once, newest first, and leave room for every other owner"
    );
    assert_eq!(
        asking(&connection, &project).unwrap(),
        BTreeSet::from([(3, 2), (4, 2)]),
        "one pair's many open questions count once and leave room for every other pair"
    );
}

#[test]
fn a_review_shows_on_its_drawn_reviewer_line_whether_or_not_its_worker_is_drawn() {
    let mut status = tree();
    status.agents[2].finished = true;
    status.agents[2].teammates.clear();
    status.agents.last_mut().unwrap().finished = true;
    finish(&mut status, &[11, 15, 16]);
    let long = format!("Ada{}", "\u{e9}".repeat(REVIEWED_AGENT_BYTES_MAX));
    status.agents.last_mut().unwrap().label.clone_from(&long);
    let facts = GraphFacts {
        review_ready: vec![8, 7],
        ..none()
    };
    let mut cut = REVIEWED_AGENT_BYTES_MAX;
    while !long.is_char_boundary(cut) {
        cut -= 1;
    }
    assert_eq!(
        relations(&graph(&status, &facts)),
        [
            ("master", "team-4", Relation::DelegatedTo, None),
            ("master", "team-5", Relation::DelegatedTo, None),
            ("master", "team-6", Relation::DelegatedTo, None),
            (
                "master",
                "team-7",
                Relation::DelegatedTo,
                Some(LiveAction::Reviewing {
                    agent: long[..cut].to_owned()
                })
            ),
        ],
        "a drawn lead reviewing its hidden worker's change set names that worker on its own line, cut at a character within the bound, and a review by a hidden lead sits on no line"
    );
    assert_eq!(
        cut,
        REVIEWED_AGENT_BYTES_MAX - 1,
        "the bound falls inside a character"
    );
}
