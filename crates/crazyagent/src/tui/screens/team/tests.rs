use crate::protocol::ProjectId;
use crate::team::{AgentStatus, EdgeState, GraphEdge, GraphNode, NodeKind, TaskSource, TeamGraph};
use crate::terminal::{AgentId, TerminalFrame};
use crate::tui::ctx::{Ctx, Link};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::screens::{Place, open};
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::shell::Shell;
use crate::tui::terminal_pane::LEADER;
use crate::tui::theme::{Appearance, Colors, Depth};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn project() -> ProjectId {
    ProjectId::new("project-tidepool").unwrap()
}

fn terminal(name: &str) -> AgentId {
    AgentId::new(name).unwrap()
}

fn node(id: &str, kind: NodeKind, parent: Option<&str>, label: &str) -> GraphNode {
    GraphNode {
        id: id.into(),
        kind,
        parent: parent.map(Into::into),
        label: label.into(),
        harness: String::new(),
        status: None,
        status_label: None,
        account: None,
        agents: None,
        terminal: None,
    }
}

fn agent(
    id: &str,
    parent: Option<&str>,
    label: &str,
    status: AgentStatus,
    account: Option<&str>,
    name: Option<&str>,
) -> GraphNode {
    GraphNode {
        status: Some(status),
        account: account.map(Into::into),
        terminal: name.map(terminal),
        ..node(id, NodeKind::Agent, parent, label)
    }
}

fn graph() -> Box<TeamGraph> {
    let master = GraphNode {
        kind: NodeKind::Master,
        ..agent(
            "master",
            None,
            "Master",
            AgentStatus::Working,
            Some("ChatGPT Pro - main"),
            Some("team-1"),
        )
    };
    let claude = GraphNode {
        agents: Some(2),
        ..node(
            "group-claude",
            NodeKind::HarnessGroup,
            Some("master"),
            "Claude Code",
        )
    };
    let codex = GraphNode {
        agents: Some(1),
        ..node(
            "group-codex",
            NodeKind::HarnessGroup,
            Some("master"),
            "Codex",
        )
    };
    Box::new(TeamGraph {
        project_id: project(),
        nodes: vec![
            master,
            claude,
            agent(
                "agent-2",
                Some("group-claude"),
                "claude-1",
                AgentStatus::Working,
                Some("Claude Max - work"),
                Some("team-2"),
            ),
            agent(
                "agent-3",
                Some("group-claude"),
                "claude-2",
                AgentStatus::Idle,
                None,
                None,
            ),
            codex,
            agent(
                "agent-5",
                Some("group-codex"),
                "codex-1",
                AgentStatus::Starting,
                Some("ChatGPT Plus"),
                Some("team-5"),
            ),
        ],
        edges: vec![
            GraphEdge {
                from: "master".into(),
                to: "agent-2".into(),
                task_id: 7,
                source: TaskSource::Master,
                text: "Fix the login bug\nThe token expires early.".into(),
                state: EdgeState::Running,
                reply: None,
            },
            GraphEdge {
                from: "master".into(),
                to: "agent-5".into(),
                task_id: 8,
                source: TaskSource::Owner,
                text: "Write tests".into(),
                state: EdgeState::Done,
                reply: Some("Added 3 tests.".into()),
            },
        ],
    })
}

fn fresh(width: u16, height: u16) -> Shell {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    );
    ctx.width = width;
    ctx.height = height;
    ctx.link = Link::Ready;
    let place = Place::Team {
        id: project(),
        name: "Tidepool".into(),
    };
    let screen = open(&place, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    assert!(matches!(
        shell.ctx.next_request(),
        Some(Request::TeamGraph { project: asked }) if asked == project()
    ));
    shell
}

fn team() -> Shell {
    let mut shell = fresh(80, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    shell
}

fn render(shell: &mut Shell) -> Vec<String> {
    let (width, height) = (shell.ctx.width, shell.ctx.height);
    let mut screen = Terminal::new(TestBackend::new(width, height)).unwrap();
    screen.draw(|frame| shell.draw(frame)).unwrap();
    let buffer = screen.backend().buffer().clone();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

fn chars(row: &str, from: usize, to: usize) -> String {
    row.chars().skip(from).take(to - from).collect()
}

fn requests(shell: &mut Shell) -> Vec<Request> {
    std::iter::from_fn(|| shell.ctx.next_request()).collect()
}

fn input(request: &Request) -> Option<(&str, &[u8])> {
    match request {
        Request::TerminalInput { agent, bytes } => Some((agent.as_str(), bytes.as_slice())),
        _ => None,
    }
}

#[test]
fn the_tree_draws_the_master_groups_agents_and_tasks_from_a_team_graph() {
    let mut shell = team();
    let rows = render(&mut shell);
    let mut expected = vec![format!("┌─ Team {}┐", "─".repeat(21))];
    for line in [
        "> Master  ▸ Working",
        "  │  ChatGPT Pro - main",
        "  ├─ Claude Code (2)",
        "  │  ├─ claude-1  ▸ Working",
        "  │  │  Claude Max - work",
        "  │  └─ claude-2  ◌ Waiting…",
        "  └─ Codex (1)",
        "     └─ codex-1  ▸ Starting",
        "        ChatGPT Plus",
        "",
        "  Tasks",
        "  ▸ Master → claude-1  Fix …",
        "  ✓ Master → codex-1  Write…",
        "",
        "",
        "",
        "",
        "",
    ] {
        expected.push(format!("│{line:<28}│"));
    }
    expected.push(format!("└{}┘", "─".repeat(28)));
    let drawn: Vec<String> = rows[2..22].iter().map(|row| chars(row, 2, 32)).collect();
    assert_eq!(drawn, expected);
    assert!(rows[0].contains("Tidepool  /  Team"), "{}", rows[0]);
}

#[test]
fn the_terminal_box_says_how_to_open_a_terminal_before_any_attach() {
    let mut shell = team();
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 33, 78),
        format!("┌─ Terminal {}┐", "─".repeat(32))
    );
    assert_eq!(
        chars(&rows[3], 33, 78),
        format!("│{:<43}│", "Enter on an agent opens its live terminal")
    );
    assert_eq!(chars(&rows[4], 33, 78), format!("│{:<43}│", "here."));
}

#[test]
fn enter_attaches_the_agent_terminal_at_the_pane_size_and_draws_its_output() {
    let mut shell = team();
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(
        &sent[0],
        Request::TerminalAttach { agent } if agent.as_str() == "team-1"
    ));
    assert!(matches!(
        &sent[1],
        Request::TerminalResize { agent, cols: 43, rows: 18 } if agent.as_str() == "team-1"
    ));

    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 43,
            rows: 18,
        },
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Output {
            bytes: b"$ cargo test\r\nok".to_vec(),
        },
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-2"),
        frame: TerminalFrame::Output {
            bytes: b"not this agent".to_vec(),
        },
    });
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 33, 78),
        format!("┌─ Master {}┐", "─".repeat(34))
    );
    assert_eq!(chars(&rows[3], 34, 77), format!("{:<43}", "$ cargo test"));
    assert_eq!(chars(&rows[4], 34, 77), format!("{:<43}", "ok"));
    assert!(rows[23].contains("Ctrl-] Back to the team"), "{}", rows[23]);
}

#[test]
fn a_focused_terminal_takes_ctrl_c_esc_and_go_to_until_ctrl_close_bracket() {
    let mut shell = team();
    shell.key(Key::Enter);
    requests(&mut shell);
    for (key, byte) in [
        (Key::Ctrl('c'), 0x03),
        (Key::Esc, 0x1b),
        (Key::Ctrl('k'), 0x0b),
        (Key::Char('?'), b'?'),
    ] {
        shell.key(key);
        let sent = requests(&mut shell);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(input(&sent[0]), Some(("team-1", &[byte][..])));
        assert!(!shell.quitting());
    }

    shell.key(LEADER);
    assert!(
        requests(&mut shell).is_empty(),
        "the leader key is not typed"
    );
    let rows = render(&mut shell);
    assert!(rows[23].contains("Enter Open"), "{}", rows[23]);
    assert!(rows[23].contains("Tab Terminal"), "{}", rows[23]);

    shell.key(Key::Ctrl('c'));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| input(sent).is_none())
    );
    let rows = render(&mut shell);
    assert!(
        shell.quitting() || rows[23].contains("Confirm"),
        "Ctrl-C quits again once the tree has focus: {}",
        rows[23]
    );
}

#[test]
fn tab_returns_to_the_attached_terminal() {
    let mut shell = team();
    shell.key(Key::Enter);
    shell.key(LEADER);
    requests(&mut shell);
    shell.key(Key::Tab);
    shell.key(Key::Char('x'));
    let sent = requests(&mut shell);
    assert_eq!(input(&sent[0]), Some(("team-1", &b"x"[..])));
}

#[test]
fn keys_typed_before_the_engine_takes_them_join_one_terminal_input() {
    let mut shell = team();
    shell.key(Key::Enter);
    requests(&mut shell);
    for character in "ls -la".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(input(&sent[0]), Some(("team-1", &b"ls -la\r"[..])));
}

#[test]
fn a_window_resize_resizes_the_attached_terminal_once() {
    let mut shell = team();
    shell.key(Key::Enter);
    requests(&mut shell);
    shell.resize(120, 36);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(matches!(
        &sent[0],
        Request::TerminalResize { agent, cols: 69, rows: 30 } if agent.as_str() == "team-1"
    ));
    shell.resize(120, 36);
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn enter_on_a_task_shows_its_route_status_source_text_and_reply() {
    let mut shell = team();
    for _ in 0..4 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "a task opens no terminal");
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 33, 78),
        format!("┌─ Task {}┐", "─".repeat(36))
    );
    let lines = [
        "Master → claude-1",
        "▸ Running",
        "Handed out by the master.",
        "",
        "Fix the login bug",
        "The token expires early.",
        "",
        "Reply",
        "No reply yet.",
    ];
    for (offset, line) in lines.iter().enumerate() {
        assert_eq!(chars(&rows[3 + offset], 34, 77), format!("{line:<43}"));
    }
    assert_eq!(
        chars(&rows[14], 2, 32),
        format!("│{:<28}│", "> ▸ Master → claude-1  Fix …")
    );

    shell.key(Key::Down);
    shell.key(Key::Enter);
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[5], 34, 77),
        format!("{:<43}", "Handed off by you.")
    );
    assert_eq!(chars(&rows[7], 34, 77), format!("{:<43}", "Write tests"));
    assert_eq!(
        chars(&rows[10], 34, 77),
        format!("{:<43}", "Added 3 tests.")
    );
}

#[test]
fn an_engine_without_team_support_says_not_ready() {
    let mut shell = fresh(80, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Err(NOT_READY.into()),
    });
    let rows = render(&mut shell);
    let drawn: Vec<String> = rows[3..6].iter().map(|row| chars(row, 2, 32)).collect();
    assert_eq!(
        drawn,
        [
            format!("│{:<28}│", "  Not ready yet."),
            format!("│{:<28}│", "  This engine cannot show"),
            format!("│{:<28}│", "  the team yet."),
        ]
    );
}

#[test]
fn a_failed_refresh_keeps_the_team_on_screen() {
    let mut shell = team();
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Err("engine_unavailable".into()),
    });
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[3], 2, 32),
        format!("│{:<28}│", "> Master  ▸ Working")
    );
}

#[test]
fn a_graph_for_another_project_is_ignored() {
    let mut shell = fresh(80, 24);
    let mut other = graph();
    other.project_id = ProjectId::new("project-other").unwrap();
    shell.arrive(Arrival::TeamGraph {
        project: other.project_id.clone(),
        result: Ok(other),
    });
    let rows = render(&mut shell);
    assert!(!chars(&rows[3], 2, 32).contains("Master"));
}

#[test]
fn a_rejected_attach_says_not_ready_and_gives_the_keys_back_to_the_tree() {
    let mut shell = team();
    shell.key(Key::Enter);
    requests(&mut shell);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Rejected {
            code: NOT_READY.into(),
        },
    });
    let rows = render(&mut shell);
    assert_eq!(chars(&rows[3], 34, 77), format!("{:<43}", "Not ready yet."));
    assert_eq!(
        chars(&rows[4], 34, 77),
        format!("{:<43}", "This engine cannot stream agent terminals")
    );
    assert_eq!(chars(&rows[5], 34, 77), format!("{:<43}", "yet."));
    assert!(rows[23].contains("Enter Open"), "{}", rows[23]);
    assert!(!rows[23].contains("Tab Terminal"), "{}", rows[23]);
    shell.key(Key::Char('x'));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| input(sent).is_none())
    );
}

#[test]
fn an_agent_without_a_terminal_says_so_and_sends_nothing() {
    let mut shell = team();
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty());
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "This agent has no terminal yet."
    );
}

#[test]
fn opening_another_agent_detaches_the_first_and_esc_detaches_on_leaving() {
    let mut shell = team();
    shell.key(Key::Enter);
    shell.key(LEADER);
    requests(&mut shell);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert!(matches!(
        &sent[0],
        Request::TerminalDetach { agent } if agent.as_str() == "team-1"
    ));
    assert!(matches!(
        &sent[1],
        Request::TerminalAttach { agent } if agent.as_str() == "team-2"
    ));

    shell.key(LEADER);
    shell.key(Key::Esc);
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| matches!(
            request,
            Request::TerminalDetach { agent } if agent.as_str() == "team-2"
        )),
        "{sent:?}"
    );
}

fn attach_master(shell: &mut Shell, output: &[u8]) {
    shell.key(Key::Enter);
    requests(shell);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 43,
            rows: 18,
        },
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Output {
            bytes: output.to_vec(),
        },
    });
}

fn close_master(shell: &mut Shell, reason: &str) {
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Closed {
            reason: reason.into(),
        },
    });
}

fn pane_row(rows: &[String], y: usize) -> String {
    chars(&rows[y], 34, 77)
}

fn padded(text: &str) -> String {
    format!("{text:<43}")
}

#[test]
fn skipped_output_shows_as_a_marker_line_in_the_terminal() {
    let mut shell = team();
    attach_master(
        &mut shell,
        b"\x1b[31mbuilding\r\n\r\n\x1b[0m[5000 bytes output skipped]\r\nstill going",
    );
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("building"));
    assert_eq!(pane_row(&rows, 5), padded("[5000 bytes output skipped]"));
    assert_eq!(pane_row(&rows, 6), padded("still going"));
}

#[test]
fn an_ended_terminal_keeps_its_last_screen_says_the_agent_stopped_and_takes_no_keys() {
    let mut shell = team();
    attach_master(&mut shell, b"$ cargo test\r\nok");
    close_master(&mut shell, "terminal_ended");
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("$ cargo test"));
    assert_eq!(pane_row(&rows, 4), padded("ok"));
    assert_eq!(pane_row(&rows, 19), padded("This agent has stopped."));
    assert_eq!(pane_row(&rows, 20), padded("Its terminal is closed."));
    assert!(rows[23].contains("Enter Open"), "{}", rows[23]);
    assert!(!rows[23].contains("Tab Terminal"), "{}", rows[23]);
    for key in [Key::Char('x'), Key::Tab, Key::Char('y')] {
        shell.key(key);
    }
    assert!(
        requests(&mut shell).is_empty(),
        "no key reaches a terminal that ended"
    );
}

#[test]
fn an_attach_refused_for_a_dead_agent_says_it_stopped() {
    let mut shell = team();
    shell.key(Key::Enter);
    requests(&mut shell);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Rejected {
            code: "terminal_agent_unknown".into(),
        },
    });
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("This agent has stopped."));
    assert_eq!(pane_row(&rows, 4), padded("Its terminal is closed."));
    shell.key(Key::Char('x'));
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn a_team_read_showing_the_agent_stopped_detaches_and_keeps_the_last_screen() {
    let mut shell = team();
    attach_master(&mut shell, b"$ make\r\ndone");
    let mut stopped = graph();
    stopped.nodes[0].status = Some(AgentStatus::Stopped);
    stopped.nodes[0].terminal = None;
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(stopped),
    });
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(matches!(
        &sent[0],
        Request::TerminalDetach { agent } if agent.as_str() == "team-1"
    ));

    shell.key(Key::Char('x'));
    assert!(
        requests(&mut shell).is_empty(),
        "keys go to the tree, not the stopped agent"
    );
    let rows = render(&mut shell);
    assert!(chars(&rows[3], 2, 32).contains("Master"), "{}", rows[3]);
    assert!(chars(&rows[3], 2, 32).contains("Stopped"), "{}", rows[3]);
    assert_eq!(pane_row(&rows, 3), padded("$ make"));
    assert_eq!(pane_row(&rows, 19), padded("This agent has stopped."));

    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "a stopped agent is not attached again"
    );
    assert_eq!(shell.ctx.message().unwrap().lead, "This agent has stopped.");
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("$ make"));
}

#[test]
fn a_terminal_lost_with_the_engine_opens_again_once_the_engine_is_back() {
    let mut shell = team();
    attach_master(&mut shell, b"before restart");
    close_master(&mut shell, "terminal_closed");
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("before restart"));
    assert_eq!(pane_row(&rows, 19), padded("Lost the engine connection."));
    assert_eq!(
        pane_row(&rows, 20),
        padded("It opens again once the engine is back.")
    );
    assert!(
        requests(&mut shell).is_empty(),
        "nothing is retried before the engine is back"
    );

    shell.arrive(Arrival::Disconnected("engine_unavailable".into()));
    shell.arrive(Arrival::Connected(None));
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert!(matches!(&sent[0], Request::TeamGraph { project: asked } if *asked == project()));
    assert!(matches!(
        &sent[1],
        Request::TerminalAttach { agent } if agent.as_str() == "team-1"
    ));
    assert!(matches!(
        &sent[2],
        Request::TerminalResize { agent, cols: 43, rows: 18 } if agent.as_str() == "team-1"
    ));

    attach_frames_after_reconnect(&mut shell);
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("after restart"));
    assert_eq!(pane_row(&rows, 19), padded(""));
    assert_eq!(pane_row(&rows, 20), padded(""));
    shell.key(Key::Tab);
    shell.key(Key::Char('x'));
    let sent = requests(&mut shell);
    assert_eq!(input(&sent[0]), Some(("team-1", &b"x"[..])));
}

fn attach_frames_after_reconnect(shell: &mut Shell) {
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 43,
            rows: 18,
        },
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Output {
            bytes: b"after restart".to_vec(),
        },
    });
}

#[test]
fn an_ended_terminal_is_not_opened_again_when_the_engine_comes_back() {
    let mut shell = team();
    attach_master(&mut shell, b"bye");
    close_master(&mut shell, "terminal_ended");
    shell.arrive(Arrival::Disconnected("engine_unavailable".into()));
    shell.arrive(Arrival::Connected(None));
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(matches!(&sent[0], Request::TeamGraph { .. }));
}
