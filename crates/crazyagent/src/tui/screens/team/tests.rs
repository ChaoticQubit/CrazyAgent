use crate::engine::owner::AgentInstructions;
use crate::protocol::{CredentialRef, ProjectId};
use crate::team::{AgentStatus, EdgeState, GraphEdge, GraphNode, NodeKind, TaskSource, TeamGraph};
use crate::terminal::{AgentId, TerminalFrame};
use crate::tui::client::OwnerEvent;
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::screens::{Place, open};
use crate::tui::seam::{AccountRow, Arrival, NOT_READY, Request};
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
        branch: None,
        terminal: None,
        questions: None,
        agent: None,
        pane: None,
        presence: None,
        waiting: None,
        owner_questions: 0,
        no_worktree: false,
        hookless: false,
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
        ..node(id, NodeKind::Lead, parent, label)
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
            Some("acct-pro"),
            Some("team-1"),
        )
    };
    let worker = GraphNode {
        kind: NodeKind::Sub,
        ..agent(
            "agent-3",
            Some("agent-2"),
            "claude-2",
            AgentStatus::Idle,
            None,
            None,
        )
    };
    Box::new(TeamGraph {
        project_id: project(),
        nodes: vec![
            master,
            agent(
                "agent-2",
                Some("master"),
                "claude-1",
                AgentStatus::Working,
                Some("acct-max"),
                Some("team-2"),
            ),
            worker,
            agent(
                "agent-5",
                Some("master"),
                "codex-1",
                AgentStatus::Starting,
                Some("acct-plus"),
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
                lead: None,
                agent_label: None,
            },
            GraphEdge {
                from: "master".into(),
                to: "agent-5".into(),
                task_id: 8,
                source: TaskSource::Owner,
                text: "Write tests".into(),
                state: EdgeState::Done,
                reply: Some("Added 3 tests.".into()),
                lead: None,
                agent_label: None,
            },
        ],
        messages: Vec::new(),
        overlaps: Vec::new(),
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
    ctx.store.accounts = Load::Ready(
        [
            ("acct-pro", "ChatGPT Pro - main"),
            ("acct-max", "Claude Max - work"),
            ("acct-plus", "ChatGPT Plus"),
        ]
        .into_iter()
        .map(|(id, name)| AccountRow {
            id: CredentialRef::new(id).unwrap(),
            name: name.into(),
            provider: String::new(),
            status: String::new(),
        })
        .collect(),
    );
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
fn the_tree_draws_who_leads_whom_and_the_tasks_from_a_team_graph() {
    let mut shell = team();
    let rows = render(&mut shell);
    let mut expected = vec![format!("┌─ Team {}┐", "─".repeat(21))];
    for line in [
        "> ▸ Master  Working",
        "  │  ChatGPT Pro - main",
        "  ├─ ▸ claude-1  Working",
        "  │  │  Claude Max - work",
        "  │  └─ ◌ claude-2  Waiting…",
        "  └─ ▸ codex-1  Starting",
        "     ChatGPT Plus",
        "",
        "  Tasks",
        "  ▸ claude-1  Fix the login…",
        "  ✓ codex-1  Write tests",
        "",
        "",
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
fn long_names_and_task_text_are_cut_with_an_ellipsis_behind_the_status_glyph() {
    let mut team = graph();
    team.nodes[1].label = "Ada Lovelace (@onboarding-fix-for-reused-ids)".into();
    team.nodes[1].status = Some(AgentStatus::Failed);
    team.edges[0].text = "\n  Fix\tthe   login\u{7} bug\u{1b}[31m now\nsecond line".into();
    let mut shell = fresh(80, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(team),
    });
    let rows = render(&mut shell);
    let tree: Vec<String> = rows[3..21].iter().map(|row| chars(row, 3, 31)).collect();
    assert!(tree[2].starts_with("  ├─ ✗ Ada Lovelace"), "{}", tree[2]);
    assert!(tree[2].ends_with('…'), "{}", tree[2]);
    assert!(tree[9].starts_with("  ▸ Ada Lovelace"), "{}", tree[9]);
    assert!(tree[9].ends_with('…'), "{}", tree[9]);
    assert!(
        rows.iter()
            .all(|row| row.chars().all(|character| !character.is_control())),
        "no control character reaches the screen"
    );
    assert_eq!(
        super::title("\n  Fix\tthe   login\u{7} bug\u{1b}[31m now\nsecond line"),
        "Fix the login bug[31m now"
    );
    assert_eq!(super::title(" \t\n\u{7}\n"), "Untitled task");
    assert_eq!(super::plain("a\tb\u{1b}c\nd"), "a bc\nd");
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
        chars(&rows[12], 2, 32),
        format!("│{:<28}│", "> ▸ claude-1  Fix the login…")
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
        format!("│{:<28}│", "> ▸ Master  Working")
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
    assert!(rows[23].contains("Ctrl-] Back to the team"), "{}", rows[23]);
    shell.key(Key::Char('x'));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| input(sent).is_none())
    );
    shell.key(LEADER);
    let rows = render(&mut shell);
    assert!(rows[23].contains("Enter Open"), "{}", rows[23]);
    assert!(!rows[23].contains("Tab Terminal"), "{}", rows[23]);
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
fn an_ended_terminal_keeps_its_last_screen_says_the_run_finished_and_takes_no_keys() {
    let mut shell = team();
    attach_master(&mut shell, b"$ cargo test\r\nok");
    close_master(&mut shell, "terminal_ended");
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded("$ cargo test"));
    assert_eq!(pane_row(&rows, 4), padded("ok"));
    assert_eq!(pane_row(&rows, 19), padded("This run finished."));
    assert_eq!(pane_row(&rows, 20), padded("Its last screen stays here."));
    assert!(rows[23].contains("Ctrl-] Back to the team"), "{}", rows[23]);
    for key in [Key::Char('x'), Key::Tab, Key::Char('y')] {
        shell.key(key);
    }
    assert!(
        requests(&mut shell).is_empty(),
        "no key reaches a terminal that ended"
    );
    shell.key(LEADER);
    let rows = render(&mut shell);
    assert!(rows[23].contains("Enter Open"), "{}", rows[23]);
    assert!(!rows[23].contains("Tab Terminal"), "{}", rows[23]);
}

#[test]
fn a_finished_run_shows_its_finished_label_above_its_last_screen() {
    let mut shell = team();
    attach_master(
        &mut shell,
        b"$ cargo test\r\nok\x1b[0m\x1b[1;1H\x1b[L\x1b[7m Finished at 10:42 \x1b[0m\x1b[?25l\x1b[1;1H",
    );
    close_master(&mut shell, "terminal_ended");
    let rows = render(&mut shell);
    assert_eq!(pane_row(&rows, 3), padded(" Finished at 10:42"));
    assert_eq!(pane_row(&rows, 4), padded("$ cargo test"));
    assert_eq!(pane_row(&rows, 5), padded("ok"));
    assert_eq!(pane_row(&rows, 19), padded("This run finished."));
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
        "keys typed after the stop never reach the stopped agent"
    );
    let rows = render(&mut shell);
    assert!(chars(&rows[3], 2, 32).contains("Master"), "{}", rows[3]);
    assert!(chars(&rows[3], 2, 32).contains("Stopped"), "{}", rows[3]);
    assert_eq!(pane_row(&rows, 3), padded("$ make"));
    assert_eq!(pane_row(&rows, 19), padded("This agent has stopped."));

    shell.key(LEADER);
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
    shell.key(Key::Char('x'));
    let sent = requests(&mut shell);
    assert_eq!(
        input(&sent[0]),
        Some(("team-1", &b"x"[..])),
        "the terminal kept the keys while the engine was away"
    );
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

fn finished_team() -> crate::team::TeamStatus {
    use crate::team::{AgentRole, TaskStatus, TeamAgent, TeamStatus, TeamTask};
    let agent = |id: i64, status: AgentStatus, finished: bool| TeamAgent {
        id,
        role: AgentRole::Sub,
        label: AgentRole::Sub.agent_label(id),
        harness: "codex".into(),
        tools: true,
        account: None,
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
        finished,
        wakeable: false,
        wake_note: None,
        parent: None,
        owner_stopped: false,
        teammates: Vec::new(),
        pane: None,
        hookless: false,
    };
    TeamStatus {
        project_id: project(),
        keep_warm: false,
        turn_limit_secs: None,
        session_limit_secs: None,
        idle_stop_secs: None,
        on_harness_switch: crate::team::SwitchMode::Carry,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        show_finished: false,
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        ask_before_merge: false,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        min_agents: 1,
        max_agents: 5,
        harnesses: Vec::new(),
        agents: vec![
            agent(2, AgentStatus::Working, false),
            TeamAgent {
                wakeable: true,
                ..agent(3, AgentStatus::Stopped, true)
            },
            TeamAgent {
                problem: Some("The agent process closed.".into()),
                wake_note: Some(
                    "Its worktree was cleaned up, so it cannot be woken. Hand its work to a new agent."
                        .into(),
                ),
                ..agent(4, AgentStatus::Failed, true)
            },
        ],
        tasks: vec![TeamTask {
            id: 9,
            source: TaskSource::Master,
            harness: "codex".into(),
            tools: true,
            prompt: "Write the docs\nwith care".into(),
            status: TaskStatus::Done,
            status_label: TaskStatus::Done.label().into(),
            agent: Some(3),
            attempts: 1,
            result: Some("Done.".into()),
            problem: None,
            delivered: true,
            output_bytes: 0,
            created_ms: 1,
            started_ms: Some(1),
            finished_ms: Some(2),
            parent: None,
        }],
        messages: Vec::new(),
        plan: Vec::new(),
        events: Default::default(),
        partial: false,
    }
}

fn setup_reply(
    query: crate::project_setup::SetupQuery,
    result: Result<crate::project_setup::SetupView, &str>,
) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query,
            result: result.map_err(str::to_owned),
        },
    ))))
}

#[test]
fn f_lists_finished_agents_w_wakes_one_by_its_team_id_and_h_switches_the_setting() {
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::TeamAction;
    let mut shell = fresh(120, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    assert!(render(&mut shell).join("\n").contains("f Finished"));
    shell.key(Key::Char('f'));
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[..], [Request::Setup(SetupQuery::Team { project: asked })] if *asked == project()),
        "{sent:?}"
    );
    assert!(
        render(&mut shell)
            .join("\n")
            .contains("Reading finished agents...")
    );
    let read = SetupQuery::Team { project: project() };
    shell.arrive(setup_reply(
        read.clone(),
        Ok(SetupView::Team(Box::new(finished_team()))),
    ));
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Finished (2)"), "{text}");
    assert!(text.contains("Finished agents: Hide."), "{text}");
    let failed = text
        .find("> Sub-agent 4")
        .expect("the newest finished agent is picked");
    let stopped = text.find("  Sub-agent 3").expect("the older one follows");
    assert!(failed < stopped, "{text}");
    assert!(text.contains("The agent process closed."), "{text}");
    assert!(text.contains("Last task: Write the docs"), "{text}");
    assert!(
        !text.contains("Sub-agent 2  "),
        "a working agent is not finished: {text}"
    );

    assert!(
        text.contains("Its worktree was cleaned up"),
        "the engine's reason shows on the row: {text}"
    );
    shell.key(Key::Char('w'));
    assert!(
        requests(&mut shell).is_empty(),
        "an agent that cannot wake sends nothing"
    );
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Sub-agent 4 cannot be woken."), "{text}");
    shell.key(Key::Down);
    shell.key(Key::Char('w'));
    let sent = requests(&mut shell);
    let [
        Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::WakeAgent { project_id, agent },
            generation,
        }),
    ] = &sent[..]
    else {
        panic!("{sent:?}");
    };
    assert_eq!((project_id, *agent), (&project(), 3));
    let woke = *generation;
    shell.key(Key::Char('w'));
    assert!(requests(&mut shell).is_empty(), "one wake at a time");
    shell.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: TeamAction::WakeAgent {
                project_id: project(),
                agent: 3,
            },
            generation: woke,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Rejected {
                code: "team_agent_running".to_owned(),
            },
        )),
    ));
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Could not wake Sub-agent 3."), "{text}");
    assert!(text.contains("It is already running."), "{text}");
    assert!(
        !text.contains("Waking Sub-agent 3."),
        "a wake the engine refused is not reported as waking: {text}"
    );
    assert!(
        requests(&mut shell).is_empty(),
        "a refused wake reads nothing again"
    );

    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    let [
        Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents { project_id, show },
            generation,
        }),
    ] = &sent[..]
    else {
        panic!("{sent:?}");
    };
    assert_eq!((project_id, *show), (&project(), true));
    shell.key(Key::Char('h'));
    assert!(requests(&mut shell).is_empty(), "one change at a time");
    shell.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents {
                project_id: project(),
                show: true,
            },
            generation: *generation,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Rejected {
                code: "project_setup_missing".to_owned(),
            },
        )),
    ));
    assert!(
        requests(&mut shell).is_empty(),
        "a refused change reads nothing again"
    );
    let text = render(&mut shell).join("\n");
    assert!(
        text.contains("Could not change where finished agents show."),
        "{text}"
    );
    assert!(
        text.contains("Finished agents: Hide."),
        "a refused change keeps the setting: {text}"
    );
    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    let [
        Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents { show: true, .. },
            generation,
        }),
    ] = &sent[..]
    else {
        panic!("{sent:?}");
    };
    shell.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents {
                project_id: project(),
                show: true,
            },
            generation: *generation,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Applied,
        )),
    ));
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[..], [Request::TeamGraph { project: asked }] if *asked == project()),
        "{sent:?}"
    );
    assert!(
        render(&mut shell)
            .join("\n")
            .contains("Finished agents: Show.")
    );
    shell.key(Key::Char('f'));
    assert!(!render(&mut shell).join("\n").contains("Finished (2)"));
}

#[test]
fn the_owners_stopped_codex_team_lists_every_sub_agent_with_its_account_and_tasks() {
    use crate::team::{AgentRole, TaskStatus, TeamAgent, TeamHarness, TeamStatus, TeamTask};
    let harness = |role: AgentRole, agents_max: u8| TeamHarness {
        harness: "codex".into(),
        role,
        agents_max,
        tools: true,
        live: 0,
        delegation: None,
        delegation_label: None,
        runnable: true,
    };
    let status = TeamStatus {
        project_id: project(),
        keep_warm: false,
        turn_limit_secs: None,
        session_limit_secs: None,
        idle_stop_secs: None,
        on_harness_switch: crate::team::SwitchMode::Carry,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        show_finished: false,
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        ask_before_merge: false,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        min_agents: 1,
        max_agents: 5,
        harnesses: vec![harness(AgentRole::Master, 1), harness(AgentRole::Sub, 4)],
        agents: (1..=4)
            .map(|id| TeamAgent {
                id,
                role: AgentRole::Sub,
                label: AgentRole::Sub.agent_label(id),
                harness: "codex".into(),
                tools: true,
                account: Some("acct-plus".into()),
                worktree: Some(format!(".crazyagent/worktrees/agent-{id}")),
                status: AgentStatus::Stopped,
                status_label: AgentStatus::Stopped.label().into(),
                task: None,
                turns: 1,
                problem: None,
                process_group: None,
                created_ms: 1,
                heartbeat_ms: 1,
                stopped_ms: Some(2),
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
            })
            .collect(),
        tasks: [1, 2, 3, 4, 3]
            .into_iter()
            .zip(1..)
            .map(|(agent, id)| TeamTask {
                id,
                source: TaskSource::Master,
                harness: "codex".into(),
                tools: true,
                prompt: format!("Browsky task {id}"),
                status: TaskStatus::Done,
                status_label: TaskStatus::Done.label().into(),
                agent: Some(agent),
                attempts: 1,
                result: Some(format!("Result {id}")),
                problem: None,
                delivered: true,
                output_bytes: 0,
                created_ms: 1,
                started_ms: Some(1),
                finished_ms: Some(2),
                parent: None,
            })
            .collect(),
        messages: Vec::new(),
        plan: Vec::new(),
        events: Default::default(),
        partial: false,
    };
    let mut shell = fresh(160, 40);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(Box::new(crate::team::graph(&status, &Default::default()))),
    });
    let rows = render(&mut shell);
    let text = rows.join("\n");
    for id in 1..=4 {
        let row = rows
            .iter()
            .find(|row| row.contains(&format!("Sub-agent {id} ")))
            .unwrap_or_else(|| panic!("Sub-agent {id} is listed:\n{text}"));
        assert!(row.contains("Stopped"), "{row}");
    }
    assert_eq!(text.matches("ChatGPT Plus").count(), 4, "{text}");
    assert!(!text.contains("acct-plus"), "{text}");
    for (id, agent) in [(1, 1), (2, 2), (3, 3), (4, 4), (5, 3)] {
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!(" Sub-agent {agent}  Browsky task {id}"))),
            "task {id} points at Sub-agent {agent}:\n{text}"
        );
    }
}

fn received(query: &ProcessQuery, result: Result<AgentInstructions, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query: query.clone(),
        result: result
            .map(|view| ProcessReply::Agent(Box::new(view)))
            .map_err(str::to_owned),
    }))))
}

fn asked(shell: &mut Shell) -> Vec<ProcessQuery> {
    requests(shell)
        .into_iter()
        .filter_map(|request| match request {
            Request::Process(query) => Some(*query),
            _ => None,
        })
        .collect()
}

#[test]
fn i_shows_what_the_selected_agent_or_the_master_received_at_its_last_launch() {
    let mut team = graph();
    team.nodes[3].id = "team-5".into();
    team.edges[1].to = "team-5".into();
    let mut shell = fresh(100, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(team),
    });
    for _ in 0..3 {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('i'));
    let codex = ProcessQuery::Agent {
        project: project(),
        agent: Some(5),
    };
    assert_eq!(asked(&mut shell), std::slice::from_ref(&codex));
    let screen = render(&mut shell).join("\n");
    assert!(screen.contains("Instructions: codex-1"), "{screen}");
    assert!(
        screen.contains("Reading what this agent received"),
        "{screen}"
    );

    shell.arrive(received(
        &codex,
        Ok(AgentInstructions {
            agent: 5,
            harness: "codex".into(),
            launched: true,
            text: "# Instructions from CrazyAgent\n\nUse tdd.\u{1b}[31m red\n".into(),
            skills: vec!["tdd".into(), "code-review".into()],
            sessions: 1,
            loaded: vec![crate::agent_home::LoadedFile {
                name: "AGENTS.md".into(),
                path: "/w/agent-5".into(),
            }],
            skills_used: vec!["tdd".into(), "systematic-debugging".into()],
            files: Vec::new(),
        }),
    ));
    let screen = render(&mut shell).join("\n");
    for shown in [
        "Instructions: AGENTS.md",
        "Skills used: tdd, systematic-debugging",
        "AGENTS.md  /w/agent-5",
        "AGENTS.md",
        "50 bytes",
        "# Instructions from CrazyAgent",
        "Use tdd. red",
        "Skills",
        "tdd",
        "code-review",
    ] {
        assert!(screen.contains(shown), "{shown} missing in\n{screen}");
    }
    assert!(
        !screen.contains('\u{1b}'),
        "stored text never reaches the terminal as a command"
    );
    assert!(
        screen
            .lines()
            .any(|row| row.contains("│>") && row.contains("▸ codex-1")),
        "the tree keeps its cursor while the box is open\n{screen}"
    );

    shell.key(Key::Home);
    shell.key(Key::Char('i'));
    let master = ProcessQuery::Agent {
        project: project(),
        agent: None,
    };
    assert_eq!(
        asked(&mut shell),
        std::slice::from_ref(&master),
        "the master card reads the newest master"
    );
    shell.arrive(received(&codex, Err("agent_instructions_unsafe")));
    let screen = render(&mut shell).join("\n");
    assert!(
        screen.contains("Reading what this agent received"),
        "a reply for another card is ignored\n{screen}"
    );
    shell.arrive(received(&master, Err("agent_instructions_unsafe")));
    let rows = render(&mut shell);
    let screen = rows.join("\n");
    let first = rows
        .iter()
        .position(|row| row.contains("This agent's instructions file is a link or not a plain"))
        .unwrap_or_else(|| panic!("the refusal sentence is shown\n{screen}"));
    assert!(
        rows[first + 1].contains("file, so CrazyAgent won't read it."),
        "the sentence wraps onto the box's next row\n{screen}"
    );
    assert!(!screen.contains("agent_instructions_unsafe"), "{screen}");

    shell.key(Key::Char('i'));
    assert_eq!(
        asked(&mut shell),
        std::slice::from_ref(&master),
        "i reads the card again"
    );
    shell.arrive(received(
        &master,
        Ok(AgentInstructions {
            agent: 1,
            harness: "claude-code".into(),
            launched: false,
            text: String::new(),
            skills: Vec::new(),
            sessions: 0,
            loaded: Vec::new(),
            skills_used: Vec::new(),
            files: Vec::new(),
        }),
    ));
    let screen = render(&mut shell).join("\n");
    assert!(screen.contains("hasn't launched yet"), "{screen}");

    shell.key(Key::Esc);
    let screen = render(&mut shell).join("\n");
    assert!(
        !screen.contains("Instructions: Master"),
        "Esc closes the box\n{screen}"
    );
    assert!(
        screen.contains("Tidepool  /  Team"),
        "and stays on the team\n{screen}"
    );
}

#[test]
fn the_instructions_box_scrolls_a_long_text_and_a_task_or_terminal_closes_it() {
    let mut team = graph();
    team.nodes[3].id = "team-5".into();
    let mut shell = fresh(100, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(team),
    });
    for _ in 0..3 {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('i'));
    let codex = ProcessQuery::Agent {
        project: project(),
        agent: Some(5),
    };
    assert_eq!(asked(&mut shell), std::slice::from_ref(&codex));
    let text: String = (1..=80).map(|line| format!("RULE-{line:02}\n")).collect();
    shell.arrive(received(
        &codex,
        Ok(AgentInstructions {
            agent: 5,
            harness: "codex".into(),
            launched: true,
            text,
            skills: Vec::new(),
            sessions: 0,
            loaded: Vec::new(),
            skills_used: Vec::new(),
            files: Vec::new(),
        }),
    ));
    let screen = render(&mut shell).join("\n");
    assert!(
        screen.contains("RULE-01") && !screen.contains("RULE-40"),
        "{screen}"
    );
    assert!(
        screen.contains("Instructions: not reported yet")
            && screen.contains("Skills used: not reported yet"),
        "a launched agent whose harness wrote no session record says so\n{screen}"
    );
    for _ in 0..3 {
        shell.key(Key::PageDown);
    }
    let screen = render(&mut shell).join("\n");
    assert!(
        screen.contains("RULE-40") && !screen.contains("RULE-01"),
        "{screen}"
    );
    for _ in 0..20 {
        shell.key(Key::PageDown);
    }
    let screen = render(&mut shell).join("\n");
    assert!(
        screen.contains("No skills were installed at its last launch."),
        "the end stays reachable and in view\n{screen}"
    );
    shell.key(Key::PageUp);
    shell.key(Key::PageUp);

    shell.key(Key::Down);
    shell.key(Key::Enter);
    let screen = render(&mut shell).join("\n");
    assert!(screen.contains("Task"), "{screen}");
    assert!(
        !screen.contains("RULE-"),
        "a task replaces the box\n{screen}"
    );
}

fn master_view(width: u16, height: u16) -> Shell {
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
    let place = Place::Master {
        id: project(),
        name: "Tidepool".into(),
    };
    let screen = open(&place, &mut ctx);
    Shell::with(ctx, screen)
}

fn live_master_change(request: &Request, turned: bool) -> bool {
    matches!(
        request,
        Request::Setup(crate::project_setup::SetupQuery::TeamChange {
            action: crate::team::TeamAction::LiveMaster { project_id, on },
            ..
        }) if *project_id == project() && *on == turned
    )
}

fn turned(on: bool, result: Result<(), &str>) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::TeamChange {
                action: crate::team::TeamAction::LiveMaster {
                    project_id: project(),
                    on,
                },
                generation: 1,
            },
            result: result
                .map(|()| {
                    crate::project_setup::SetupView::Changed(
                        crate::engine::owner::OwnerResult::Applied,
                    )
                })
                .map_err(str::to_owned),
        },
    ))))
}

fn failed_read(code: &str) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::Team { project: project() },
            result: Err(code.into()),
        },
    ))))
}

fn switch_read(request: &Request) -> bool {
    matches!(
        request,
        Request::Setup(crate::project_setup::SetupQuery::Team { project: asked }) if *asked == project()
    )
}

fn switch(on: Option<bool>, problem: Option<&str>) -> Arrival {
    switched(on, problem, None)
}

fn switched(
    on: Option<bool>,
    problem: Option<&str>,
    live_switch: Option<crate::team::SessionSwitch>,
) -> Arrival {
    let status = crate::team::TeamStatus {
        project_id: project(),
        keep_warm: false,
        turn_limit_secs: None,
        session_limit_secs: None,
        idle_stop_secs: None,
        on_harness_switch: crate::team::SwitchMode::Carry,
        sub_agent_depth: crate::team::DEPTH_DEFAULT,
        child_landing: crate::team::ChildLanding::Lead,
        max_harnesses: None,
        max_teammates: None,
        token_budget: None,
        budget_used: None,
        budget_reached: false,
        spread_accounts: false,
        show_finished: false,
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        ask_before_merge: false,
        coordination: None,
        live_switch,
        paused: false,
        live_master: on,
        live_problem: problem.map(str::to_owned),
        min_agents: 0,
        max_agents: 1,
        harnesses: Vec::new(),
        agents: Vec::new(),
        tasks: Vec::new(),
        messages: Vec::new(),
        plan: Vec::new(),
        events: crate::team::events::EventsView::default(),
        partial: false,
    };
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::Team { project: project() },
            result: Ok(crate::project_setup::SetupView::Team(Box::new(status))),
        },
    ))))
}

#[test]
fn the_master_view_turns_the_live_master_on_once_and_its_terminal_fills_the_body() {
    let mut shell = master_view(80, 24);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(switch_read(&sent[0]), "{sent:?}");
    assert!(matches!(&sent[1], Request::TeamGraph { project: asked } if *asked == project()));
    shell.arrive(switch(None, None));
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        live_master_change(&sent[0], true),
        "a switch never set turns on at the first open: {sent:?}"
    );
    shell.arrive(switch(None, None));
    assert!(requests(&mut shell).is_empty(), "it turns on once");
    let rows = render(&mut shell);
    assert!(rows[0].contains("Tidepool  /  Master"), "{}", rows[0]);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!(
            "{:<58}s Stop live master",
            "Live master on. It starts when you open this view."
        )
    );
    assert_eq!(
        chars(&rows[3], 2, 78),
        format!("┌─ Master {}┐", "─".repeat(65))
    );
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "Starting the master's terminal. It opens once the master signs in and"
        )
    );

    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(&sent[0], Request::TerminalAttach { agent } if agent.as_str() == "team-1"));
    assert!(matches!(
        &sent[1],
        Request::TerminalResize { agent, cols: 74, rows: 17 } if agent.as_str() == "team-1"
    ));

    shell.refresh();
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert_eq!(
        sent.len(),
        2,
        "only the switch and the graph are read again: {sent:?}"
    );
    assert!(switch_read(&sent[0]), "{sent:?}");
    assert!(matches!(&sent[1], Request::TeamGraph { .. }));

    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Output {
            bytes: b"> what next?".to_vec(),
        },
    });
    let rows = render(&mut shell);
    assert_eq!(chars(&rows[4], 3, 77), format!("{:<74}", "> what next?"));
    assert!(rows[23].contains("Ctrl-] Leave terminal"), "{}", rows[23]);
    assert!(rows[23].contains("Ctrl-J New line"), "{}", rows[23]);

    for (key, bytes) in [
        (Key::Char('y'), &b"y"[..]),
        (Key::Ctrl('j'), &b"\n"[..]),
        (Key::Esc, &b"\x1b"[..]),
        (Key::Enter, &b"\r"[..]),
    ] {
        shell.key(key);
        let sent = requests(&mut shell);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(input(&sent[0]), Some(("team-1", bytes)));
    }

    shell.key(LEADER);
    assert!(
        requests(&mut shell).is_empty(),
        "Ctrl-] leaves the terminal open"
    );
    let rows = render(&mut shell);
    assert!(rows[23].contains("s Stop live master"), "{}", rows[23]);
    assert!(
        rows[23].contains("Esc Earlier conversation"),
        "{}",
        rows[23]
    );
    shell.key(Key::Char('z'));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| input(sent).is_none())
    );
    shell.key(Key::Esc);
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [Request::TerminalDetach { agent }] if agent.as_str() == "team-1"),
        "{sent:?}"
    );
}

#[test]
fn the_master_view_header_stops_and_starts_the_live_master_and_says_why_it_stopped() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), None));
    assert!(
        requests(&mut shell).is_empty(),
        "a switch already on is not turned on again"
    );
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(LEADER);
    shell.key(Key::Char('s'));
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(&sent[0], Request::TerminalDetach { agent } if agent.as_str() == "team-1"));
    assert!(live_master_change(&sent[1], false), "{sent:?}");
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!("{:<57}s Start live master", "Live master stopped.")
    );
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "The live master is stopped. Its terminal opens again once you start it."
        )
    );
    assert_eq!(
        chars(&rows[5], 2, 78),
        format!("│{:<74}│", "Press s to start it.")
    );
    assert!(rows[23].contains("s Start live master"), "{}", rows[23]);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    assert!(
        requests(&mut shell).is_empty(),
        "a stopped master's terminal is not opened"
    );

    let mut guarded = master_view(80, 24);
    requests(&mut guarded);
    let why = "The live master could not start: no signed-in account.";
    guarded.arrive(switch(Some(false), Some(why)));
    assert!(
        requests(&mut guarded).is_empty(),
        "a switch turned off is not turned on by opening the view"
    );
    let rows = render(&mut guarded);
    assert_eq!(chars(&rows[4], 2, 78), format!("│{why:<74}│"));
    assert_eq!(
        chars(&rows[5], 2, 78),
        format!("│{:<74}│", "Press s to start it.")
    );
    guarded.key(Key::Char('s'));
    let sent = requests(&mut guarded);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(live_master_change(&sent[0], true), "{sent:?}");
    let rows = render(&mut guarded);
    assert!(rows[2].contains("Live master on."), "{}", rows[2]);
    assert!(
        rows[4].contains("Starting the master's terminal."),
        "{}",
        rows[4]
    );

    guarded.arrive(turned(true, Ok(())));
    let sent = requests(&mut guarded);
    assert!(
        matches!(sent.as_slice(), [read] if switch_read(read)),
        "an accepted change reads the switch again: {sent:?}"
    );
    let again =
        "The live master was turned off: it could not start. Start it again from the master view.";
    guarded.arrive(switch(Some(false), Some(again)));
    let rows = render(&mut guarded);
    assert!(
        rows[2].contains("Live master stopped."),
        "the engine stopping it right after the owner's start is shown: {}",
        rows[2]
    );
    assert!(
        rows[4].contains("The live master was turned off"),
        "{}",
        rows[4]
    );
    assert!(rows[23].contains("s Start live master"), "{}", rows[23]);
}

#[test]
fn a_failed_switch_read_never_starts_the_live_master() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(failed_read("team_state_unavailable"));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| !live_master_change(sent, true)),
        "a read that failed is not a switch never set"
    );
    shell.arrive(switch(Some(false), None));
    shell.arrive(failed_read("engine_unavailable"));
    assert!(requests(&mut shell).is_empty());
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master stopped."),
        "a failed read keeps the switch it had: {}",
        rows[2]
    );
}

#[test]
fn keys_typed_after_the_master_terminal_closes_never_turn_the_live_master_off() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), None));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    shell.key(Key::Char('y'));
    let sent = requests(&mut shell);
    assert_eq!(input(&sent[0]), Some(("team-1", &b"y"[..])));

    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Closed {
            reason: "owner_socket_replaced".into(),
        },
    });
    for character in "es, ship it".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Esc);
    let sent = requests(&mut shell);
    assert!(
        sent.is_empty(),
        "keys typed after the close reach neither the terminal nor the master's switch: {sent:?}"
    );
    let rows = render(&mut shell);
    assert!(rows[0].contains("Tidepool  /  Master"), "{}", rows[0]);
    assert!(rows[2].contains("s Stop live master"), "{}", rows[2]);
    assert!(rows[23].contains("Ctrl-] Leave terminal"), "{}", rows[23]);

    shell.key(LEADER);
    let rows = render(&mut shell);
    assert!(rows[23].contains("s Stop live master"), "{}", rows[23]);
    shell.key(Key::Char('s'));
    let sent = requests(&mut shell);
    assert!(
        sent.iter()
            .any(|request| live_master_change(request, false)),
        "s still stops the live master once the owner left the terminal: {sent:?}"
    );
}

#[test]
fn the_master_view_follows_the_master_to_its_new_terminal_and_says_when_it_cannot_start() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    let mut moved = graph();
    moved.nodes[0].terminal = Some(terminal("team-9"));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(moved),
    });
    let sent = requests(&mut shell);
    assert!(matches!(&sent[0], Request::TerminalDetach { agent } if agent.as_str() == "team-1"));
    assert!(matches!(&sent[1], Request::TerminalAttach { agent } if agent.as_str() == "team-9"));

    let mut refused = master_view(80, 24);
    requests(&mut refused);
    refused.arrive(Arrival::Owner(Box::new(
        crate::tui::client::OwnerEvent::Setup(Box::new(crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::TeamChange {
                action: crate::team::TeamAction::LiveMaster {
                    project_id: project(),
                    on: true,
                },
                generation: 1,
            },
            result: Err("team_master_no_terminal".into()),
        })),
    )));
    let rows = render(&mut refused);
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!("│{:<74}│", "The master terminal could not start.")
    );
    assert_eq!(
        chars(&rows[5], 2, 78),
        format!(
            "│{:<74}│",
            "This master harness has no terminal app. Its conversation stays on the"
        )
    );
    refused.key(Key::Esc);
    assert!(requests(&mut refused).is_empty());
}

fn finished(id: i64, label: &str, bytes: u64) -> crate::team::cleanup::FinishedAgent {
    crate::team::cleanup::FinishedAgent {
        id,
        role: crate::team::AgentRole::Sub,
        label: label.into(),
        harness: "codex".into(),
        stopped_ms: Some(2),
        bytes,
        counted_all: true,
    }
}

fn cleanup_reply(
    query: crate::tui::client::cleanup::CleanupQuery,
    result: Result<crate::tui::client::cleanup::CleanupReply, String>,
) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Cleanup(Box::new(
        crate::tui::client::cleanup::CleanupEvent { query, result },
    ))))
}

fn cleanup_sent(shell: &mut Shell) -> Vec<crate::tui::client::cleanup::CleanupQuery> {
    requests(shell)
        .into_iter()
        .map(|request| match request {
            Request::Cleanup(query) => *query,
            other => panic!("only cleanup is sent, not {other:?}"),
        })
        .collect()
}

#[test]
fn c_lists_the_finished_agents_with_their_size_and_y_removes_every_one() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Reading finished agents"), "{text}");

    let agents = vec![
        finished(4, "Ada (@login-page)", 3 * 1024 * 1024),
        finished(2, "Grace (@signup)", 1024 * 1024),
    ];
    shell.arrive(cleanup_reply(read, Ok(CleanupReply::Finished(agents))));
    let rows = render(&mut shell);
    let text = rows.join("\n");
    assert!(text.contains("Clean up 2 finished agents (4 MB)"), "{text}");
    assert_eq!(
        pane_row(&rows, 3),
        padded("Ada (@login-page)  sub-agent, Codex  3 MB")
    );
    assert_eq!(
        pane_row(&rows, 4),
        padded("Grace (@signup)  sub-agent, Codex  1 MB")
    );
    assert!(
        text.contains("y Remove and free 4 MB   n Keep them"),
        "{text}"
    );

    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "the open box takes every key"
    );
    shell.key(Key::Char('y'));
    let remove = CleanupQuery::Remove {
        project: project(),
        agents: vec![4, 2],
    };
    assert_eq!(cleanup_sent(&mut shell), vec![remove.clone()]);
    assert!(render(&mut shell).join("\n").contains("Removing"));
    shell.key(Key::Esc);
    assert!(
        render(&mut shell).join("\n").contains("Removing"),
        "a removal under way stays on screen"
    );

    shell.arrive(cleanup_reply(
        remove,
        Ok(CleanupReply::Removed(crate::team::cleanup::Removal {
            agents: vec![4, 2],
            kept: None,
        })),
    ));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Removed 2 finished agents.");
    assert_eq!(message.rest, "Freed 4 MB.");
    let rows = render(&mut shell);
    assert_eq!(
        pane_row(&rows, 3),
        padded("Enter on an agent opens its live terminal")
    );
}

#[test]
fn twenty_agents_at_80x24_keep_the_question_and_y_removes_every_one_read() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    let agents: Vec<_> = (1..=20)
        .map(|id| finished(id, &format!("Agent {id} (@task-{id})"), 1024 * 1024))
        .collect();
    shell.arrive(cleanup_reply(
        read.clone(),
        Ok(CleanupReply::Finished(agents)),
    ));
    let rows = render(&mut shell);
    let text = rows.join("\n");
    assert!(
        text.contains("Clean up 20 finished agents (20 MB)"),
        "{text}"
    );
    for (y, id) in (3..13).zip(1..) {
        assert_eq!(
            pane_row(&rows, y),
            padded(&format!("Agent {id} (@task-{id})  sub-agent, Codex  1 MB")),
            "{text}"
        );
    }
    assert_eq!(pane_row(&rows, 13), padded("and 10 more (10 MB)"), "{text}");
    assert_eq!(pane_row(&rows, 14), padded(""), "{text}");
    assert!(pane_row(&rows, 15).starts_with("Each agent's working copy"));
    assert_eq!(
        pane_row(&rows, 20),
        padded("y Remove and free 20 MB   n Keep them"),
        "the question is the box's last line:\n{text}"
    );

    shell.arrive(cleanup_reply(
        read,
        Ok(CleanupReply::Finished(vec![finished(21, "Late", 1)])),
    ));
    shell.key(Key::Char('y'));
    assert_eq!(
        cleanup_sent(&mut shell),
        vec![CleanupQuery::Remove {
            project: project(),
            agents: (1..=20).collect(),
        }],
        "y removes the agents the box opened with, shown or counted"
    );
    let rows = render(&mut shell);
    assert!(
        pane_row(&rows, 20).contains("Removing"),
        "{}",
        rows.join("\n")
    );
}

#[test]
fn a_removal_that_stopped_part_way_says_how_many_went_and_why_one_stayed() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    cleanup_sent(&mut shell);
    let agents = vec![
        finished(4, "Ada (@login-page)", 3 * 1024 * 1024),
        finished(3, "Lin (@docs)", 1024 * 1024),
        finished(2, "Grace (@signup)", 1024 * 1024),
    ];
    shell.arrive(cleanup_reply(read, Ok(CleanupReply::Finished(agents))));
    shell.key(Key::Char('y'));
    let sent = cleanup_sent(&mut shell);
    assert_eq!(
        sent,
        vec![CleanupQuery::Remove {
            project: project(),
            agents: vec![4, 3, 2],
        }]
    );
    shell.arrive(cleanup_reply(
        sent[0].clone(),
        Ok(CleanupReply::Removed(crate::team::cleanup::Removal {
            agents: vec![4, 3],
            kept: Some("cleanup_worktree_dirty".into()),
        })),
    ));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Removed 2, kept 1:");
    assert_eq!(
        message.rest,
        "An agent's working copy has changes that are not saved anywhere else."
    );
    assert!(!render(&mut shell).join("\n").contains("Grace (@signup)"));
}

#[test]
fn n_keeps_the_agents_and_an_empty_list_a_refusal_or_another_project_change_nothing() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let read = CleanupQuery::Finished { project: project() };
    let mut shell = team();
    shell.key(Key::Char('c'));
    cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(
        read.clone(),
        Ok(CleanupReply::Finished(Vec::new())),
    ));
    assert_eq!(shell.ctx.message().unwrap().lead, "Nothing to clean up.");
    let rows = render(&mut shell);
    assert_eq!(
        pane_row(&rows, 3),
        padded("Enter on an agent opens its live terminal")
    );

    shell.key(Key::Char('c'));
    cleanup_sent(&mut shell);
    let other = CleanupQuery::Finished {
        project: ProjectId::new("project-other").unwrap(),
    };
    shell.arrive(cleanup_reply(
        other,
        Ok(CleanupReply::Finished(vec![finished(9, "Elsewhere", 1)])),
    ));
    assert!(
        render(&mut shell)
            .join("\n")
            .contains("Reading finished agents")
    );
    let agents = vec![finished(3, "Lin (@docs)", 2048)];
    shell.arrive(cleanup_reply(
        read.clone(),
        Ok(CleanupReply::Finished(agents.clone())),
    ));
    shell.key(Key::Char('n'));
    assert!(requests(&mut shell).is_empty(), "n removes nothing");
    assert!(!render(&mut shell).join("\n").contains("Lin (@docs)"));

    shell.key(Key::Char('c'));
    cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(
        read.clone(),
        Ok(CleanupReply::Finished(agents)),
    ));
    shell.key(Key::Char('y'));
    let sent = cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(
        sent[0].clone(),
        Err("cleanup_worktree_dirty".into()),
    ));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Cleanup kept an agent and stopped there.");
    assert_eq!(
        message.rest,
        "An agent's working copy has changes that are not saved anywhere else."
    );
    assert!(!render(&mut shell).join("\n").contains("Lin (@docs)"));

    shell.key(Key::Char('c'));
    cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(read, Err(NOT_READY.into())));
    assert_eq!(
        shell.ctx.message().unwrap().rest,
        "Cleanup is not available in this engine yet."
    );
    for code in crate::engine::owner::CLEANUP_CODES {
        if code.starts_with("cleanup_")
            && !code.ends_with("_bounds")
            && *code != "cleanup_request_invalid"
        {
            assert_ne!(
                super::cleanup::words(code),
                "The engine could not do this.",
                "{code}"
            );
        }
    }
}

#[test]
fn a_removal_the_engine_could_not_confirm_says_so_and_asks_for_a_fresh_read() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let read = CleanupQuery::Finished { project: project() };
    let mut shell = team();
    shell.key(Key::Char('c'));
    cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(
        read.clone(),
        Ok(CleanupReply::Finished(vec![finished(
            3,
            "Lin (@docs)",
            2048,
        )])),
    ));
    shell.key(Key::Char('y'));
    let sent = cleanup_sent(&mut shell);
    shell.arrive(cleanup_reply(
        sent[0].clone(),
        Err("cleanup_outcome_unknown".into()),
    ));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Could not confirm what cleanup did.");
    assert_eq!(
        message.rest,
        "The engine did not answer in time and may still be working. Press c to read the list again."
    );

    shell.key(Key::Char('c'));
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    shell.arrive(cleanup_reply(read, Err("cleanup_outcome_unknown".into())));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Could not read finished agents.");
    assert_eq!(
        message.rest,
        "The engine did not answer in time and may still be working. Press c to read the list again."
    );
}

#[test]
fn the_master_view_names_its_way_back_as_the_earlier_conversation() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    let rows = render(&mut shell);
    assert!(
        rows[23].contains("Esc Earlier conversation"),
        "{}",
        rows[23]
    );
    assert!(!rows[23].contains("Esc Back"), "{}", rows[23]);
}

/// The master view's header says where the live session's master harness last switched and
/// what the new harness got, next to the switch key, and keeps saying it after the owner
/// turns the live master off.
#[test]
fn the_master_view_head_says_the_live_session_switched_harness() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    let switch = |mode| crate::team::SessionSwitch {
        harness: "Claude Code".into(),
        mode,
        note: crate::master_sessions::switched_note("Claude Code", mode),
    };
    shell.arrive(switched(
        Some(true),
        None,
        Some(switch(crate::team::SwitchMode::Carry)),
    ));
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!(
            "{:<58}s Stop live master",
            "Live master on. Switched to Claude Code, carried over."
        )
    );
    shell.arrive(switched(
        Some(true),
        None,
        Some(switch(crate::team::SwitchMode::Fresh)),
    ));
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master on. Switched to Claude Code, started fresh."),
        "{}",
        rows[2]
    );
    shell.arrive(turned(false, Ok(())));
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!(
            "{:<57}s Start live master",
            "Live master stopped. Switched to Claude Code, started …"
        )
    );
    shell.arrive(switch_off_unswitched());
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!("{:<57}s Start live master", "Live master stopped.")
    );
}

fn switch_off_unswitched() -> Arrival {
    switched(Some(false), None, None)
}

#[test]
fn the_instructions_box_lists_each_instruction_file_the_engine_recorded() {
    use crate::engine::owner::InstructionRecord;
    use crate::instruction_files::State;
    let ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    );
    let record = |name: &str, bytes: u64, state: State| InstructionRecord {
        name: name.into(),
        bytes,
        state,
    };
    let view = AgentInstructions {
        agent: 5,
        harness: "claude-code".into(),
        launched: true,
        text: "Use tdd.\n".into(),
        skills: Vec::new(),
        sessions: 0,
        loaded: Vec::new(),
        skills_used: Vec::new(),
        files: vec![
            record("CLAUDE.md", 1_229, State::Given),
            record("AGENTS.md", 300, State::Same),
            record("CLAUDE.local.md", 0, State::Unsafe),
            record("~/.claude/CLAUDE.md", 40_000, State::TooLarge),
        ],
    };
    let text = |view: AgentInstructions| -> String {
        super::instruction_lines(&Load::Ready(Box::new(view)), 120, &ctx)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let shown = text(view.clone());
    for line in [
        "Instruction files",
        "CLAUDE.md  1.2 KB, given",
        "AGENTS.md  300 bytes, left out: the same text as a file above",
        "CLAUDE.local.md  0 bytes, left out: a link or not a plain file",
        "~/.claude/CLAUDE.md  39.1 KB, left out: larger than CrazyAgent gives",
    ] {
        assert!(shown.contains(line), "{line} missing in\n{shown}");
    }
    assert!(
        shown.find("\nInstruction files\n").unwrap() < shown.find("\nSkills\n").unwrap(),
        "{shown}"
    );
    let none = text(AgentInstructions {
        files: Vec::new(),
        ..view
    });
    assert!(
        none.contains("No instruction files were found at its last launch."),
        "{none}"
    );
}
