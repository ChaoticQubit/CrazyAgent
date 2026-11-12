use crate::engine::owner::AgentInstructions;
use crate::protocol::{CredentialRef, ProjectId};
use crate::team::graph::{LiveAction, Relation, RelationEdge};
use crate::team::{AgentStatus, EdgeState, GraphEdge, GraphNode, NodeKind, TaskSource, TeamGraph};
use crate::terminal::{AgentId, TerminalFrame};
use crate::tui::client::OwnerEvent;
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::{Key, ShiftArrows};
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
        waits_for: None,
        owner_questions: 0,
        no_worktree: false,
        hookless: false,
        message_note: None,
        started_ms: None,
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
        relations: Vec::new(),
        master_live_terminal: true,
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
            no_window: None,
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
        shell.ctx.next_screen_request(),
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
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[..], [request] if questions_read(request)),
        "a team graph read is followed by one Questions read"
    );
    shell
}

fn questions_read(request: &Request) -> bool {
    matches!(
        request,
        Request::Setup(crate::project_setup::SetupQuery::CoordQuestions { project: asked })
            if *asked == project()
    )
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
    std::iter::from_fn(|| shell.ctx.next_screen_request()).collect()
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

fn relation(from: &str, to: &str, relation: Relation, live: Option<LiveAction>) -> RelationEdge {
    RelationEdge {
        from: from.into(),
        to: to.into(),
        relation,
        live,
    }
}

/// A graph with the master waiting on the owner over its line to agent-2. Output: the graph.
fn waiting_on_owner() -> Box<TeamGraph> {
    let mut team = graph();
    team.relations = vec![
        relation(
            "master",
            "agent-2",
            Relation::Leads,
            Some(LiveAction::WaitingOn { owner: true }),
        ),
        relation("agent-2", "agent-3", Relation::TeammateOf, None),
        relation("master", "agent-5", Relation::DelegatedTo, None),
    ];
    team
}

/// An owner wait keeps its whole words on the agent's line or moves: at 120 columns the line
/// never reads "waiting on: ...", and the detail row names the owner.
#[test]
fn an_owner_wait_stays_whole_or_moves_to_the_detail_row() {
    let mut shell = fresh(120, 36);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(waiting_on_owner()),
    });
    let shown = render(&mut shell);
    let at = shown
        .iter()
        .position(|row| row.contains("├─ ▸ claude-1"))
        .unwrap_or_else(|| panic!("{}", shown.join("\n")));
    assert!(!shown[at].contains("waiting"), "{}", shown[at]);
    assert!(
        shown[at + 1].contains("waiting on: your answer"),
        "{}",
        shown.join("\n")
    );
}

/// An agent with no account or branch has no detail row, so a moved owner wait gets a note row.
#[test]
fn an_owner_wait_on_an_agent_with_no_detail_row_moves_to_a_note_row() {
    let mut team = waiting_on_owner();
    let lead = team
        .nodes
        .iter_mut()
        .find(|node| node.id == "agent-2")
        .unwrap();
    lead.account = None;
    lead.branch = None;
    let mut shell = fresh(80, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(team),
    });
    let shown = render(&mut shell);
    let note = shown
        .iter()
        .position(|row| row.contains("waiting on:"))
        .unwrap_or_else(|| panic!("{}", shown.join("\n")));
    assert!(
        shown[note..=note + 1]
            .iter()
            .any(|row| row.contains("answer")),
        "{}",
        shown.join("\n")
    );
}

#[test]
fn each_tree_line_names_its_relation_and_the_live_action_while_it_happens() {
    let mut team = graph();
    team.relations = vec![
        relation(
            "master",
            "agent-2",
            Relation::Leads,
            Some(LiveAction::HandingOff {
                task: "Fix the login bug\nThe token expires early.".into(),
            }),
        ),
        relation("agent-2", "agent-3", Relation::TeammateOf, None),
        relation("master", "agent-5", Relation::DelegatedTo, None),
    ];
    let wider = team.clone();
    let mut shell = fresh(80, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(team),
    });
    let rows = render(&mut shell);
    let mut expected = Vec::new();
    for line in [
        "> ▸ Master  Working",
        "  │  ChatGPT Pro - main",
        "  ├─ ▸ claude-1  Working",
        "  │  │  • handing off: Fix …",
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
    ] {
        expected.push(format!("│{line:<28}│"));
    }
    let drawn: Vec<String> = rows[3..17].iter().map(|row| chars(row, 2, 32)).collect();
    assert_eq!(
        drawn, expected,
        "at 80x24 each agent's line keeps its status word: a live action whose first word does not fit moves to the agent's detail row, and a standing word shows whole or not at all"
    );
    let mut bare = graph();
    bare.relations.clear();
    let mut plain_shell = fresh(80, 24);
    plain_shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(bare),
    });
    let drawn_lines = |rows: &[String]| {
        rows[3..22]
            .iter()
            .filter(|row| !chars(row, 3, 31).trim().is_empty())
            .count()
    };
    assert_eq!(
        drawn_lines(&rows),
        drawn_lines(&render(&mut plain_shell)),
        "the labels take no row of their own"
    );
    for (columns, lines) in [(80, 24), (100, 30), (120, 36), (160, 40), (200, 50)] {
        let mut sized = fresh(columns, lines);
        sized.arrive(Arrival::TeamGraph {
            project: project(),
            result: Ok(wider.clone()),
        });
        let shown = render(&mut sized);
        let line = |name: &str| {
            shown
                .iter()
                .find(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} at {columns}x{lines}:\n{}", shown.join("\n")))
                .clone()
        };
        let lead = line("├─ ▸ claude-1");
        assert!(lead.contains("Working"), "{columns}x{lines}: {lead}");
        assert!(
            !lead.contains("•") || lead.contains("• handing"),
            "{columns}x{lines}: {lead}"
        );
        let standing = line("└─ ▸ codex-1");
        assert!(
            standing.contains("Starting"),
            "{columns}x{lines}: {standing}"
        );
        assert!(
            !standing.contains("  d") || standing.contains("delegated to"),
            "{columns}x{lines}: {standing}"
        );
        if columns == 200 {
            assert!(lead.contains("• handing off: …  Working"), "{lead}");
            assert!(standing.contains("Starting  delegated to"), "{standing}");
        }
    }
    let label = |relation, live| {
        super::edge_label(&RelationEdge {
            from: "a".into(),
            to: "b".into(),
            relation,
            live,
        })
    };
    assert_eq!(label(Relation::Leads, None), "leads");
    assert_eq!(label(Relation::DelegatedTo, None), "delegated to");
    assert_eq!(label(Relation::TeammateOf, None), "teammate of");
    assert_eq!(
        label(
            Relation::Leads,
            Some(LiveAction::Reviewing {
                agent: " \n\t ".into()
            })
        ),
        "reviewing: change set"
    );
    assert_eq!(
        label(
            Relation::DelegatedTo,
            Some(LiveAction::HandingOff {
                task: "\n  Fix\tthe   login bug\nThe token expires early.".into()
            })
        ),
        "handing off: Fix the login bug"
    );
    assert_eq!(
        label(
            Relation::Leads,
            Some(LiveAction::HandingOff {
                task: " \n\t ".into()
            })
        ),
        "handing off: Untitled task"
    );
    assert_eq!(
        label(Relation::Leads, Some(LiveAction::Messaging)),
        "messaging"
    );
    assert_eq!(
        label(
            Relation::DelegatedTo,
            Some(LiveAction::WaitingOn { owner: false })
        ),
        "waiting on"
    );
    assert_eq!(
        label(
            Relation::DelegatedTo,
            Some(LiveAction::WaitingOn { owner: true })
        ),
        "waiting on: your answer"
    );
    assert_eq!(
        label(
            Relation::Leads,
            Some(LiveAction::Reviewing {
                agent: "Wren (@checkout)".into()
            })
        ),
        "reviewing: Wren (@checkout)'s change set"
    );
    assert_eq!(
        label(
            Relation::Leads,
            Some(LiveAction::Reviewing {
                agent: "Wren\u{1b}[31m\nnow".into()
            })
        ),
        "reviewing: Wren[31m now's change set",
        "a reviewed name keeps no control character"
    );
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
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(
        &sent[0],
        Request::TerminalDetach { agent } if agent.as_str() == "team-1"
    ));
    assert!(questions_read(&sent[1]), "{sent:?}");

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
        instructions_changed: false,
        message_note: None,
    };
    TeamStatus {
        project_id: project(),
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
        message_settings: crate::team::agent_messages::MessageSettings::default(),
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        leftovers: crate::team::LeftoverMode::End,
        leftover_holds: Vec::new(),
        ask_before_merge: false,
        master_lands: None,
        access_unreadable: false,
        reask_empty_reply: true,
        master_idle_stop: crate::team::MasterIdleStop::default(),
        master_live_terminal: true,
        catch_up_main: crate::team::CatchUpMain::On,
        reuse_idle: crate::team::ReuseIdle::Off,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        live_idle_stopped: false,
        settings_version: 0,
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

#[test]
fn the_finished_box_keeps_the_picked_agent_drawn_when_the_list_is_longer_than_the_box() {
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::{AgentRole, TeamAction, TeamAgent};
    let mut status = finished_team();
    let stopped = status.agents[1].clone();
    status.agents = (3..15)
        .map(|id| TeamAgent {
            id,
            label: AgentRole::Sub.agent_label(id),
            ..stopped.clone()
        })
        .collect();
    let mut shell = fresh(120, 24);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('f'));
    requests(&mut shell);
    shell.arrive(setup_reply(
        SetupQuery::Team { project: project() },
        Ok(SetupView::Team(Box::new(status))),
    ));
    for _ in 0..8 {
        shell.key(Key::Down);
    }
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Finished (12)"), "{text}");
    assert!(
        text.contains("> Sub-agent 6"),
        "the ninth agent, picked with eight Downs, is drawn: {text}"
    );
    assert!(text.contains("more above"), "{text}");
    shell.key(Key::Char('w'));
    let sent = requests(&mut shell);
    assert!(
        matches!(
            &sent[..],
            [Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::WakeAgent { agent: 6, .. },
                ..
            })]
        ),
        "w wakes the agent drawn as picked: {sent:?}"
    );
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
            action:
                TeamAction::FinishedAgents {
                    project_id,
                    show,
                    version,
                },
            generation,
        }),
    ] = &sent[..]
    else {
        panic!("{sent:?}");
    };
    assert_eq!((project_id, *show, *version), (&project(), true, 0));
    shell.key(Key::Char('h'));
    assert!(requests(&mut shell).is_empty(), "one change at a time");
    shell.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: TeamAction::FinishedAgents {
                project_id: project(),
                show: true,
                version: 0,
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
            action:
                TeamAction::FinishedAgents {
                    show: true,
                    version: 0,
                    ..
                },
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
                version: 0,
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

/// The Finished agents setting names the team settings version the box read; one the engine
/// refuses as stale says so, reads the team again at once, and the next change starts from
/// the newer setting and names its version.
#[test]
fn a_finished_agents_change_from_an_older_read_is_refused_and_the_box_reads_the_team_again() {
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::TeamAction;
    let mut shell = fresh(120, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('f'));
    requests(&mut shell);
    let read = SetupQuery::Team { project: project() };
    let mut older = finished_team();
    older.settings_version = 3;
    shell.arrive(setup_reply(
        read.clone(),
        Ok(SetupView::Team(Box::new(older))),
    ));
    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    let [Request::Setup(SetupQuery::TeamChange { action, generation })] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(
        action,
        &TeamAction::FinishedAgents {
            project_id: project(),
            show: true,
            version: 3,
        },
        "the change names the version the box read"
    );
    shell.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: action.clone(),
            generation: *generation,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Rejected {
                code: crate::storage::versioned::STALE_CODE.to_owned(),
            },
        )),
    ));
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[..], [Request::Setup(SetupQuery::Team { project: asked })] if *asked == project()),
        "a stale change reads the team again at once: {sent:?}"
    );
    let text = render(&mut shell).join("\n");
    assert!(
        text.contains("Could not change where finished agents show."),
        "{text}"
    );
    assert!(
        text.contains("Finished agents: Hide."),
        "the refused change is not shown as saved: {text}"
    );
    let mut newer = finished_team();
    newer.show_finished = true;
    newer.settings_version = 4;
    shell.arrive(setup_reply(read, Ok(SetupView::Team(Box::new(newer)))));
    assert!(
        render(&mut shell)
            .join("\n")
            .contains("Finished agents: Show."),
        "the newer setting from the other window shows"
    );
    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    assert!(
        matches!(
            &sent[..],
            [Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::FinishedAgents {
                    show: false,
                    version: 4,
                    ..
                },
                ..
            })]
        ),
        "the next change starts from the newer setting and names its version: {sent:?}"
    );
}

/// The live master switch the master view sends on its read names the team settings version
/// it read; a switch the engine refuses as stale drops that change, reads the switch again and
/// starts the live master once more from the newer read while that read still shows it off;
/// any other refusal of a start keeps the master shown as refused, never as on, and the next
/// read does not start it again.
#[test]
fn a_live_master_switch_from_an_older_read_reads_the_switch_again() {
    use crate::project_setup::{SetupQuery, SetupView};
    let mut view = master_view(80, 24);
    requests(&mut view);
    let read = |view: &mut Shell, version: u64| {
        let mut status = finished_team();
        status.live_master = Some(false);
        status.settings_version = version;
        view.arrive(setup_reply(
            SetupQuery::Team { project: project() },
            Ok(SetupView::Team(Box::new(status))),
        ));
    };
    let started = |view: &mut Shell| {
        requests(view)
            .into_iter()
            .find_map(|request| match request {
                Request::Setup(SetupQuery::TeamChange {
                    action: action @ crate::team::TeamAction::LiveMaster { on: true, .. },
                    generation,
                }) => Some((action, generation)),
                _ => None,
            })
    };
    read(&mut view, 5);
    let (action, generation) = started(&mut view).expect("the read starts the live master");
    assert_eq!(
        action,
        crate::team::TeamAction::LiveMaster {
            project_id: project(),
            on: true,
            version: 5,
        },
        "the switch names the version the master view read"
    );
    view.arrive(setup_reply(
        SetupQuery::TeamChange {
            action: action.clone(),
            generation,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Rejected {
                code: crate::storage::versioned::STALE_CODE.to_owned(),
            },
        )),
    ));
    let sent = requests(&mut view);
    assert!(
        sent.iter().any(|request| matches!(request, Request::Setup(SetupQuery::Team { project: asked }) if *asked == project())),
        "a stale switch reads the switch again at once: {sent:?}"
    );
    assert!(
        !sent.iter().any(|request| live_master_change(request, true)),
        "a stale switch is not resent from the old read: {sent:?}"
    );

    read(&mut view, 6);
    let (action, generation) =
        started(&mut view).expect("the newer read starts the live master once more");
    assert_eq!(
        action,
        crate::team::TeamAction::LiveMaster {
            project_id: project(),
            on: true,
            version: 6,
        },
        "the next switch names the newer version"
    );
    view.arrive(setup_reply(
        SetupQuery::TeamChange { action, generation },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Rejected {
                code: "team_master_no_terminal".to_owned(),
            },
        )),
    ));
    let rows = render(&mut view);
    assert!(
        !rows[2].contains("Live master on."),
        "an engine refusal is not shown as on: {}",
        rows[2]
    );
    read(&mut view, 6);
    assert!(
        started(&mut view).is_none(),
        "a refused start is not sent again by the next read"
    );
}

/// Keys the owner types while the master view starts the live master wait through a stale
/// refusal of that start: the view reads the switch again, starts the master from the newer
/// read and types the keys in once its terminal attaches, and never says they were lost.
#[test]
fn keys_typed_before_a_stale_live_master_start_reach_the_terminal_after_the_retry() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(false), None));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| live_master_change(request, true)),
        "the read starts the live master: {sent:?}"
    );
    for character in "hello".chars() {
        shell.key(Key::Char(character));
    }
    shell.arrive(turned(true, Err(crate::storage::versioned::STALE_CODE)));
    let text = render(&mut shell).join("\n");
    assert!(
        !text.contains("What you typed for the master was not sent."),
        "the keys wait for the retry: {text}"
    );
    let sent = requests(&mut shell);
    assert!(sent.iter().any(switch_read), "{sent:?}");
    assert!(
        sent.iter().all(|request| input(request).is_none()),
        "{sent:?}"
    );

    shell.arrive(switch(Some(false), None));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| live_master_change(request, true)),
        "the newer read starts the live master once more: {sent:?}"
    );
    shell.arrive(turned(true, Ok(())));
    requests(&mut shell);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[0], Request::TerminalAttach { agent } if agent.as_str() == "team-1"),
        "{sent:?}"
    );
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    let sent = requests(&mut shell);
    let typed: Vec<_> = sent.iter().filter_map(input).collect();
    assert_eq!(typed, vec![("team-1", &b"hello"[..])], "{sent:?}");
}

/// A retry whose switch read fails starts nothing more, so the keys the owner typed for the
/// master are dropped at once and the owner is told to type them again.
#[test]
fn a_retry_whose_switch_read_fails_drops_the_keys_typed_for_the_master_and_says_so() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(false), None));
    requests(&mut shell);
    for character in "hello".chars() {
        shell.key(Key::Char(character));
    }
    shell.arrive(turned(true, Err(crate::storage::versioned::STALE_CODE)));
    requests(&mut shell);
    shell.arrive(failed_read("team_status_unavailable"));
    let text = render(&mut shell).join("\n");
    assert!(
        text.contains("What you typed for the master was not sent."),
        "{text}"
    );
    let sent = requests(&mut shell);
    assert!(
        !sent.iter().any(|request| live_master_change(request, true)),
        "a failed read starts nothing: {sent:?}"
    );
}

/// A master view retries its live master start once per open: after a second stale refusal
/// the next read that still shows the master off starts nothing more, so two windows that
/// keep changing the team settings never make it loop.
#[test]
fn a_second_stale_live_master_switch_in_one_open_is_not_retried() {
    use crate::project_setup::{SetupQuery, SetupView};
    let mut view = master_view(80, 24);
    requests(&mut view);
    let read = |view: &mut Shell, version: u64| {
        let mut status = finished_team();
        status.live_master = Some(false);
        status.settings_version = version;
        view.arrive(setup_reply(
            SetupQuery::Team { project: project() },
            Ok(SetupView::Team(Box::new(status))),
        ));
    };
    let started = |view: &mut Shell| {
        requests(view)
            .into_iter()
            .find_map(|request| match request {
                Request::Setup(SetupQuery::TeamChange {
                    action: action @ crate::team::TeamAction::LiveMaster { on: true, .. },
                    generation,
                }) => Some((action, generation)),
                _ => None,
            })
    };
    let stale = |view: &mut Shell, action, generation| {
        view.arrive(setup_reply(
            SetupQuery::TeamChange { action, generation },
            Ok(SetupView::Changed(
                crate::engine::owner::OwnerResult::Rejected {
                    code: crate::storage::versioned::STALE_CODE.to_owned(),
                },
            )),
        ));
    };
    read(&mut view, 5);
    let (action, generation) = started(&mut view).expect("the read starts the live master");
    stale(&mut view, action, generation);
    read(&mut view, 6);
    let (action, generation) =
        started(&mut view).expect("the newer read starts the live master once more");
    stale(&mut view, action, generation);
    read(&mut view, 7);
    assert!(
        started(&mut view).is_none(),
        "a second stale switch in one open is not retried"
    );
}

#[test]
fn the_finished_setting_counts_the_running_and_all_agents_the_team_tree_lists() {
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::TeamAction;
    let mut shell = fresh(120, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('f'));
    requests(&mut shell);
    shell.arrive(setup_reply(
        SetupQuery::Team { project: project() },
        Ok(SetupView::Team(Box::new(finished_team()))),
    ));
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Finished agents: Hide."), "{text}");
    assert!(text.contains("Team tree: Running (1)  All (1)"), "{text}");

    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    let [
        Request::Setup(SetupQuery::TeamChange {
            action:
                TeamAction::FinishedAgents {
                    show: true,
                    version,
                    ..
                },
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
                version: *version,
            },
            generation: *generation,
        },
        Ok(SetupView::Changed(
            crate::engine::owner::OwnerResult::Applied,
        )),
    ));
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Finished agents: Show."), "{text}");
    assert!(text.contains("Team tree: Running (1)  All (3)"), "{text}");
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
        message_settings: crate::team::agent_messages::MessageSettings::default(),
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        leftovers: crate::team::LeftoverMode::End,
        leftover_holds: Vec::new(),
        ask_before_merge: false,
        master_lands: None,
        access_unreadable: false,
        reask_empty_reply: true,
        master_idle_stop: crate::team::MasterIdleStop::default(),
        master_live_terminal: true,
        catch_up_main: crate::team::CatchUpMain::On,
        reuse_idle: crate::team::ReuseIdle::Off,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        live_switch: None,
        paused: false,
        live_master: None,
        live_problem: None,
        live_idle_stopped: false,
        settings_version: 0,
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
                instructions_changed: false,
                message_note: None,
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
            action: crate::team::TeamAction::LiveMaster { project_id, on, .. },
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
                    version: 0,
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
    team_read(master_status(on, problem, live_switch))
}

fn master_status(
    on: Option<bool>,
    problem: Option<&str>,
    live_switch: Option<crate::team::SessionSwitch>,
) -> crate::team::TeamStatus {
    crate::team::TeamStatus {
        project_id: project(),
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
        message_settings: crate::team::agent_messages::MessageSettings::default(),
        agent_branches: crate::team::BranchMode::Named,
        master_branch: None,
        land_on: None,
        background_at_end: crate::team::BackgroundAtEnd::Wait,
        leftovers: crate::team::LeftoverMode::End,
        leftover_holds: Vec::new(),
        ask_before_merge: false,
        master_lands: None,
        access_unreadable: false,
        reask_empty_reply: true,
        master_idle_stop: crate::team::MasterIdleStop::default(),
        master_live_terminal: true,
        catch_up_main: crate::team::CatchUpMain::On,
        reuse_idle: crate::team::ReuseIdle::Off,
        new_folder_files_max: crate::team::NEW_FOLDER_FILES_DEFAULT,
        coordination: None,
        live_switch,
        paused: false,
        live_master: on,
        live_problem: problem.map(str::to_owned),
        live_idle_stopped: false,
        settings_version: 0,
        min_agents: 0,
        max_agents: 1,
        harnesses: Vec::new(),
        agents: Vec::new(),
        tasks: Vec::new(),
        messages: Vec::new(),
        plan: Vec::new(),
        events: crate::team::events::EventsView::default(),
        partial: false,
    }
}

fn team_read(status: crate::team::TeamStatus) -> Arrival {
    Arrival::Owner(Box::new(crate::tui::client::OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::Team { project: project() },
            result: Ok(crate::project_setup::SetupView::Team(Box::new(status))),
        },
    ))))
}

/// A team read whose saved master harness is `saved` and whose live master agent, when
/// one runs, uses `running`. Inputs: the switch, the saved and the running harness ids.
/// Output: the arrival.
fn harnessed(on: Option<bool>, saved: &str, running: Option<&str>) -> Arrival {
    use crate::team::{AgentRole, TeamAgent, TeamHarness};
    let mut status = master_status(on, None, None);
    status.harnesses = vec![TeamHarness {
        harness: saved.into(),
        role: AgentRole::Master,
        agents_max: 1,
        tools: true,
        live: 1,
        delegation: None,
        delegation_label: None,
        runnable: true,
    }];
    status.agents = running
        .into_iter()
        .map(|harness| TeamAgent {
            id: 1,
            role: AgentRole::Master,
            label: AgentRole::Master.agent_label(1),
            harness: harness.into(),
            tools: true,
            account: None,
            worktree: None,
            status: AgentStatus::Idle,
            status_label: AgentStatus::Idle.label().into(),
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
        })
        .collect();
    team_read(status)
}

/// The master view's header names the harness the running live master uses, the saved one
/// while none runs, and the saved one from the next start while the two differ (N-D26,
/// OP-28); its state words stay first.
#[test]
fn the_master_view_head_names_the_running_harness_and_the_saved_one_from_the_next_start() {
    let mut shell = master_view(120, 24);
    requests(&mut shell);
    shell.arrive(harnessed(Some(true), "codex", Some("claude-code")));
    let rows = render(&mut shell);
    assert!(
        rows[2].contains(
            "Live master on. Harness: Claude Code; Codex from the next start. It starts when you open this view."
        ),
        "{}",
        rows[2]
    );
    shell.arrive(harnessed(Some(true), "codex", Some("codex")));
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master on. Harness: Codex. It starts when you open this view."),
        "{}",
        rows[2]
    );
    shell.arrive(harnessed(Some(true), "codex", None));
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master on. Harness: Codex. It starts when you open this view."),
        "the saved harness while none runs: {}",
        rows[2]
    );
    shell.arrive(turned(false, Ok(())));
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master stopped. Harness: Codex."),
        "an accepted stop keeps the harness last read: {}",
        rows[2]
    );
}

/// The master view reads the team again when the team shows another master agent, so the
/// header follows the live master that restarted on the saved harness.
#[test]
fn the_master_view_reads_the_harness_again_when_another_master_agent_starts() {
    let mut shell = master_view(120, 24);
    requests(&mut shell);
    let mut graph = graph();
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph.clone()),
    });
    requests(&mut shell);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph.clone()),
    });
    assert!(
        !requests(&mut shell).iter().any(switch_read),
        "the same master agent reads nothing"
    );
    let master = graph
        .nodes
        .iter_mut()
        .find(|node| node.kind == NodeKind::Master)
        .unwrap();
    master.agent = Some(master.agent.unwrap_or(0) + 1);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph),
    });
    assert!(
        requests(&mut shell).iter().any(switch_read),
        "a new master agent reads the team"
    );
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
            "{:<63}n New session",
            "Live master on. It starts when you open this view."
        ),
        "at 80 columns the state stays whole and l Sessions stays in the hints"
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
    for shown in ["n New session", "l Sessions", "Esc Back"] {
        assert!(rows[23].contains(shown), "{shown}: {}", rows[23]);
    }
    for gone in [
        "Stop live master",
        "Start live master",
        "Earlier conversation",
    ] {
        assert!(!rows[23].contains(gone), "{gone}: {}", rows[23]);
    }
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

/// Opening the master view turns a stored switch that is off on, once per open and whatever
/// stopped it, and shows it starting rather than the reason it stopped. The header and hints
/// carry no stop or start key. A stop the engine makes right after says why and that opening
/// the view or a new session starts it again; a switch already on is left alone.
#[test]
fn the_master_view_turns_an_off_switch_on_at_open() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    let why = "The live master could not start: no signed-in account.";
    shell.arrive(switch(Some(false), Some(why)));
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        live_master_change(&sent[0], true),
        "a stored switch false turns on at open: {sent:?}"
    );
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!(
            "{:<63}n New session",
            "Live master on. It starts when you open this view."
        )
    );
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "Starting the master's terminal. It opens once the master signs in and"
        ),
        "the start replaces the reason the master stopped"
    );
    assert!(!rows[2].contains("  s "), "{}", rows[2]);
    assert!(rows[23].contains("Esc Back"), "{}", rows[23]);
    for gone in [
        "Stop live master",
        "Start live master",
        "Earlier conversation",
        "Press s",
    ] {
        assert!(
            rows.iter().all(|row| !row.contains(gone)),
            "{gone}: {rows:?}"
        );
    }

    shell.arrive(turned(true, Ok(())));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [read] if switch_read(read)),
        "an accepted change reads the switch again: {sent:?}"
    );
    let again =
        "The live master was turned off: it could not start. Start it again from the master view.";
    shell.arrive(switch(Some(false), Some(again)));
    assert!(
        requests(&mut shell).is_empty(),
        "the view turns the switch on once per open"
    );
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!("{:<51}n New session  l Sessions", "Live master stopped.")
    );
    assert!(
        rows[4].contains("The live master was turned off"),
        "{}",
        rows[4]
    );
    let restart = rows[4..10].join("\n");
    assert!(
        restart.contains("It starts again when you open this view again or press n for a new"),
        "{restart}"
    );
    assert!(restart.contains("session."), "{restart}");
    shell.key(Key::Char('s'));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| !live_master_change(sent, true) && !live_master_change(sent, false)),
        "s neither stops nor starts the live master"
    );
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    assert!(
        requests(&mut shell).is_empty(),
        "a stopped master's terminal is not opened"
    );

    let mut running = master_view(100, 24);
    requests(&mut running);
    running.arrive(switch(Some(true), None));
    let sent = requests(&mut running);
    assert!(
        sent.iter().all(|sent| !live_master_change(sent, true)),
        "a switch already on is not turned on again: {sent:?}"
    );
    assert_eq!(
        sent.len(),
        1,
        "a switch already on gets the owner's session check once (LMS-1): {sent:?}"
    );
    assert!(session_check(&sent[0]), "{sent:?}");
    running.arrive(switch(Some(true), None));
    assert!(
        requests(&mut running).is_empty(),
        "the session check goes out once per open"
    );
    let rows = render(&mut running);
    assert_eq!(
        chars(&rows[2], 2, 98),
        format!(
            "{:<71}n New session  l Sessions",
            "Live master on. It starts when you open this view."
        ),
        "a wide view shows both session keys"
    );
}

/// A master view on a project with Live terminal Off starts nothing and goes back to the
/// console, whatever the run flag says (DEC-117 Q4).
#[test]
fn a_master_view_on_live_terminal_off_starts_nothing_and_goes_back() {
    for on in [None, Some(false), Some(true)] {
        let mut ctx = Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        );
        ctx.width = 80;
        ctx.height = 24;
        ctx.link = Link::Ready;
        let place = Place::Master {
            id: project(),
            name: "Tidepool".into(),
        };
        let mut screen = open(&place, &mut ctx);
        screen.shown(&mut ctx);
        std::iter::from_fn(|| ctx.next_screen_request()).for_each(drop);
        let mut status = master_status(on, None, None);
        status.master_live_terminal = false;
        let flow = screen.arrive(&team_read(status), &mut ctx);
        assert!(
            matches!(flow, crate::tui::screens::Flow::Back),
            "the view goes back with the switch at {on:?}"
        );
        let sent: Vec<Request> = std::iter::from_fn(|| ctx.next_screen_request()).collect();
        assert!(sent.is_empty(), "nothing starts: {sent:?}");
        assert_eq!(
            ctx.message().map(|message| message.lead.as_str()),
            Some("Live terminal is off for this project.")
        );
    }
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
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        live_master_change(&sent[0], true),
        "the open turns the stored switch on: {sent:?}"
    );
    shell.arrive(failed_read("engine_unavailable"));
    assert!(requests(&mut shell).is_empty());
    let rows = render(&mut shell);
    assert!(
        rows[2].contains("Live master on."),
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
    assert!(rows[2].contains("n New session"), "{}", rows[2]);
    assert!(rows[23].contains("Ctrl-] Leave terminal"), "{}", rows[23]);

    shell.key(LEADER);
    let rows = render(&mut shell);
    assert!(rows[23].contains("Esc Back"), "{}", rows[23]);
    shell.key(Key::Char('s'));
    let sent = requests(&mut shell);
    assert!(
        sent.iter()
            .all(|request| !live_master_change(request, false)),
        "no key of the master view stops the live master: {sent:?}"
    );
}

/// The master view is the master's terminal from its first key (OP-12, N-T1): what the
/// owner types while the terminal is still starting waits and goes in once it attaches, so
/// a letter such as l is typed rather than taken as the Sessions key, and Shift+Enter
/// (Ctrl-J) stays a new line in the one message Enter then sends. A paste waits for the
/// open terminal, whose paste mode is not known before it.
#[test]
fn keys_typed_while_the_master_terminal_starts_are_typed_in_once_it_attaches() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), None));
    requests(&mut shell);
    for character in "first line".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Ctrl('j'));
    for character in "second line".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Paste("pasted".into()));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().all(|request| input(request).is_none()),
        "nothing is typed into a terminal that is not there yet: {sent:?}"
    );
    assert_eq!(
        shell.ctx.sessions_key, None,
        "l and n are typed, never the Sessions keys"
    );
    let rows = render(&mut shell);
    assert!(rows[0].contains("Tidepool  /  Master"), "{}", rows[0]);
    assert!(rows[23].contains("Ctrl-] Leave terminal"), "{}", rows[23]);
    assert!(
        rows.iter()
            .any(|row| row.contains("Paste once the master's terminal is open.")),
        "{rows:#?}"
    );

    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert!(
        matches!(&sent[0], Request::TerminalAttach { agent } if agent.as_str() == "team-1"),
        "{sent:?}"
    );
    assert!(
        sent.iter().all(|request| input(request).is_none()),
        "the keys wait for the engine to attach the terminal: {sent:?}"
    );
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        input(&sent[0]),
        Some(("team-1", &b"first line\nsecond line"[..]))
    );
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(input(&sent[0]), Some(("team-1", &b"\r"[..])));
}

/// Esc while the master's terminal is still starting leaves the view: it and the keys before
/// it are never typed into the terminal later, where an Esc would interrupt the master.
#[test]
fn esc_while_the_master_terminal_starts_is_never_typed_into_it_later() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), None));
    requests(&mut shell);
    shell.key(Key::Char('h'));
    shell.key(Key::Esc);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    let sent = requests(&mut shell);
    assert!(
        sent.iter().all(|request| input(request).is_none()),
        "{sent:?}"
    );
}

fn attached_frame(shell: &mut Shell, name: &str) {
    shell.arrive(Arrival::Terminal {
        agent: terminal(name),
        frame: TerminalFrame::Attached {
            agent_id: terminal(name),
            cols: 74,
            rows: 17,
        },
    });
}

fn type_hi_and_enter(shell: &mut Shell) {
    for character in "hi".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let sent = requests(shell);
    assert!(sent.iter().all(|sent| input(sent).is_none()), "{sent:?}");
}

fn typed_while_starting(team_read: bool) -> Shell {
    let mut shell = master_view(80, 24);
    shell.arrive(switch(Some(true), None));
    if team_read {
        shell.arrive(Arrival::TeamGraph {
            project: project(),
            result: Ok(graph()),
        });
    }
    requests(&mut shell);
    type_hi_and_enter(&mut shell);
    shell
}

fn said_not_sent(shell: &mut Shell) -> bool {
    render(shell)
        .iter()
        .any(|row| row.contains("What you typed for the master was not sent."))
}

fn typed_in(shell: &mut Shell) -> Vec<Request> {
    requests(shell)
        .into_iter()
        .filter(|sent| input(sent).is_some())
        .collect()
}

fn attach_asked(sent: &[Request], name: &str) -> bool {
    sent.iter()
        .any(|sent| matches!(sent, Request::TerminalAttach { agent } if agent.as_str() == name))
}

/// NT1-M1: a master terminal that closes before it opens drops the keys kept for it and
/// says so; the master's terminal opened again gets none of them.
#[test]
fn a_master_terminal_closed_before_it_opens_drops_the_keys_kept_for_it() {
    let mut shell = typed_while_starting(true);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Closed {
            reason: "exited".into(),
        },
    });
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert!(attach_asked(&sent, "team-1"), "{sent:?}");
    attached_frame(&mut shell, "team-1");
    let typed = typed_in(&mut shell);
    assert!(typed.is_empty(), "{typed:?}");
}

/// F1: a rejected master terminal drops the keys kept for it and says so; the master's
/// next terminal gets none of them when it opens.
#[test]
fn a_rejected_master_terminal_drops_the_keys_kept_for_it_and_the_next_one_gets_none() {
    let mut shell = typed_while_starting(true);
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Rejected {
            code: "terminal_unknown".into(),
        },
    });
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
    let mut moved = graph();
    moved.nodes[0].terminal = Some(terminal("team-9"));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(moved),
    });
    let sent = requests(&mut shell);
    assert!(attach_asked(&sent, "team-9"), "{sent:?}");
    attached_frame(&mut shell, "team-9");
    let typed = typed_in(&mut shell);
    assert!(typed.is_empty(), "{typed:?}");
}

/// NT1-M1: a master that moves to another terminal before its first one opens (failover)
/// drops the keys kept for the first and says so; the new terminal gets none of them.
#[test]
fn a_master_moved_to_a_new_terminal_before_it_opens_drops_the_keys_kept_for_the_old_one() {
    let mut shell = typed_while_starting(true);
    let mut moved = graph();
    moved.nodes[0].terminal = Some(terminal("team-9"));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(moved),
    });
    let sent = requests(&mut shell);
    assert!(attach_asked(&sent, "team-9"), "{sent:?}");
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
    attached_frame(&mut shell, "team-9");
    let typed = typed_in(&mut shell);
    assert!(typed.is_empty(), "{typed:?}");
}

/// NT1-M1: a refused live master start drops the keys kept for the master's terminal and
/// says so; a terminal that opens later gets none of them.
#[test]
fn a_refused_live_master_start_drops_the_keys_kept_for_its_terminal() {
    let mut shell = master_view(80, 24);
    shell.arrive(switch(None, None));
    requests(&mut shell);
    type_hi_and_enter(&mut shell);
    shell.arrive(turned(true, Err("team_master_no_terminal")));
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    attached_frame(&mut shell, "team-1");
    let typed = typed_in(&mut shell);
    assert!(typed.is_empty(), "{typed:?}");
}

/// NT1-M1: Stop all while the master's terminal starts drops the keys kept for it and says
/// so; once the team runs again, the terminal that opens gets none of them.
#[test]
fn stop_all_while_the_master_terminal_starts_drops_the_keys_kept_for_it() {
    let mut shell = typed_while_starting(false);
    shell.arrive(paused_read(Some(true)));
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
    shell.arrive(switch(Some(true), None));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert!(attach_asked(&sent, "team-1"), "{sent:?}");
    attached_frame(&mut shell, "team-1");
    let typed = typed_in(&mut shell);
    assert!(typed.is_empty(), "{typed:?}");
}

/// NT1-M1: the live master turned off while its terminal starts drops the keys kept for it
/// and says so.
#[test]
fn the_live_master_turned_off_while_its_terminal_starts_drops_the_keys_kept_for_it() {
    let mut shell = master_view(80, 24);
    shell.arrive(switch(None, None));
    shell.arrive(turned(true, Ok(())));
    requests(&mut shell);
    type_hi_and_enter(&mut shell);
    shell.arrive(switch(
        Some(false),
        Some("The live master was turned off: it could not start."),
    ));
    assert!(said_not_sent(&mut shell), "{:?}", render(&mut shell));
}

fn attached_master_view(shift_arrows: ShiftArrows) -> Shell {
    let mut shell = master_view(80, 24);
    shell.ctx.shift_arrows = shift_arrows;
    shell.arrive(switch(Some(true), None));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    shell.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    requests(&mut shell);
    shell
}

/// N-T1 B, Switch tabs (the default): Shift-←→ while the master's terminal has the keys
/// leave the master view for the project's previous or next tab, attached or still
/// starting, and are never typed in; keys kept while it started go with the view.
#[test]
fn shift_arrows_in_the_master_terminal_switch_project_tabs_by_default() {
    let mut shell = attached_master_view(ShiftArrows::default());
    assert_eq!(ShiftArrows::default(), ShiftArrows::SwitchTabs);
    let rows = render(&mut shell);
    assert!(rows[23].contains("Shift-←→ Tabs"), "{}", rows[23]);
    shell.key(Key::ShiftRight);
    let sent = requests(&mut shell);
    assert!(
        sent.iter().all(|sent| input(sent).is_none()),
        "Shift-→ is never typed in: {sent:?}"
    );
    assert!(
        sent.iter().any(
            |sent| matches!(sent, Request::TerminalDetach { agent } if agent.as_str() == "team-1")
        ),
        "{sent:?}"
    );
    assert_eq!(shell.ctx.project_step, Some((project(), true)));

    let mut starting = master_view(80, 24);
    starting.arrive(switch(Some(true), None));
    requests(&mut starting);
    starting.key(Key::Char('h'));
    starting.key(Key::ShiftLeft);
    assert_eq!(starting.ctx.project_step, Some((project(), false)));
    starting.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    starting.arrive(Arrival::Terminal {
        agent: terminal("team-1"),
        frame: TerminalFrame::Attached {
            agent_id: terminal("team-1"),
            cols: 74,
            rows: 17,
        },
    });
    let sent = requests(&mut starting);
    assert!(
        sent.iter().all(|sent| input(sent).is_none()),
        "the key kept before Shift-← went with the view: {sent:?}"
    );
}

/// N-T1 B, Send to the agent: Shift-←→ while the master's terminal has the keys are typed
/// into it and the view stays; after Ctrl-] they move the project tab in either choice.
#[test]
fn shift_arrows_in_the_master_terminal_go_to_the_agent_when_set_so() {
    let mut shell = attached_master_view(ShiftArrows::SendToAgent);
    let rows = render(&mut shell);
    assert!(!rows[23].contains("Tabs"), "{}", rows[23]);
    shell.key(Key::ShiftRight);
    let sent = requests(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(input(&sent[0]), Some(("team-1", &b"\x1b[1;2C"[..])));
    assert_eq!(shell.ctx.project_step, None);

    shell.key(LEADER);
    shell.key(Key::ShiftLeft);
    assert_eq!(shell.ctx.project_step, Some((project(), false)));
    let mut switching = attached_master_view(ShiftArrows::SwitchTabs);
    switching.key(LEADER);
    switching.key(Key::ShiftRight);
    assert_eq!(switching.ctx.project_step, Some((project(), true)));
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
                    version: 0,
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
        kept: None,
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
    assert!(
        text.contains("Worktrees of 2 finished agents (4 MB)"),
        "{text}"
    );
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
    assert_eq!(message.lead, "Removed the worktrees of 2 finished agents.");
    assert_eq!(
        message.rest,
        "Freed 4 MB. Their conversations, history and logs stay."
    );
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
        text.contains("Worktrees of 20 finished agents (20 MB)"),
        "{text}"
    );
    for (y, id) in (3..11).zip(1..) {
        assert_eq!(
            pane_row(&rows, y),
            padded(&format!("Agent {id} (@task-{id})  sub-agent, Codex  1 MB")),
            "{text}"
        );
    }
    assert_eq!(pane_row(&rows, 11), padded("and 12 more (12 MB)"), "{text}");
    assert_eq!(pane_row(&rows, 12), padded(""), "{text}");
    assert_eq!(
        pane_row(&rows, 13),
        padded("Each agent's worktree, its code checkout")
    );
    assert_eq!(
        pane_row(&rows, 15),
        padded("conversations, history and logs stay, and")
    );
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
    assert_eq!(message.lead, "Removed 2 worktrees, kept 1:");
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
fn the_master_view_names_its_way_back_as_back() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    let rows = render(&mut shell);
    assert!(rows[23].contains("Esc Back"), "{}", rows[23]);
    assert!(!rows[23].contains("Earlier conversation"), "{}", rows[23]);
}

/// The master view's header says where the live session's master harness last switched and
/// what the new harness got, whole at 80 columns next to the session keys, and keeps saying
/// it once the live master stops; the first read of this open that finds it off turns it on.
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
            "{:<63}n New session",
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
            "{:<63}n New session",
            "Live master stopped. Switched to Claude Code, started fresh."
        )
    );
    requests(&mut shell);
    shell.arrive(switch_off_unswitched());
    let sent = requests(&mut shell);
    assert_eq!(
        sent.iter()
            .filter(|sent| live_master_change(sent, true))
            .count(),
        0,
        "a switch seen on stays off until the owner acts: {sent:?}"
    );
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[2], 2, 78),
        format!("{:<51}n New session  l Sessions", "Live master stopped.")
    );
}

fn switch_off_unswitched() -> Arrival {
    switched(Some(false), None, None)
}

#[test]
fn the_instructions_box_says_the_same_for_every_harness_when_none_is_loaded_or_reported() {
    let ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    );
    let view = AgentInstructions {
        agent: 5,
        harness: "claude-code".into(),
        launched: true,
        text: "Use tdd.\n".into(),
        skills: Vec::new(),
        sessions: 2,
        loaded: Vec::new(),
        skills_used: Vec::new(),
        files: Vec::new(),
    };
    let text = |view: AgentInstructions| -> String {
        super::instruction_lines(&Load::Ready(Box::new(view)), 120, &ctx)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for harness in ["claude-code", "codex"] {
        let reported = text(AgentInstructions {
            harness: harness.into(),
            ..view.clone()
        });
        assert!(
            reported.contains(
                "Instructions: none loaded by the harness itself; CrazyAgent gives its instructions at launch"
            ),
            "DEC-141 PA-14: one line for every harness: {reported}"
        );
        let unreported = text(AgentInstructions {
            harness: harness.into(),
            sessions: 0,
            ..view.clone()
        });
        assert!(
            unreported.contains("Instructions: not reported yet")
                && unreported
                    .contains("A headless run reports nothing here while it keeps no transcript."),
            "DEC-141 PA-14: {harness}: {unreported}"
        );
        assert!(!unreported.contains("Claude run"), "{unreported}");
    }
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
        "AGENTS.md  300 bytes, left out: the same text as the instructions or a file above",
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

/// Serves one request the Questions box sends with a real engine through the owner
/// controller, as the TUI's owner worker does. Inputs: the controller and the request.
/// Output: the arrival the Team screen receives.
#[cfg(unix)]
async fn served(
    controller: &mut crate::client_controller::OwnerController,
    request: Request,
) -> Arrival {
    use crate::engine::owner::OwnerResponse;
    use crate::project_setup::{SetupQuery, SetupView};
    let Request::Setup(query) = request else {
        panic!("the Questions box sent a request other than a setup read or change");
    };
    let result = match &query {
        SetupQuery::CoordQuestions { project } => controller
            .coord_questions(project.clone())
            .await
            .map(|inbox| SetupView::CoordQuestions(Box::new(inbox)))
            .map_err(|problem| problem.0),
        SetupQuery::TeamChange { action, generation } => {
            match controller.team_action(action.clone(), *generation).await {
                Ok(
                    OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation },
                ) => Ok(SetupView::Changed(operation.result)),
                Ok(other) => panic!("the team change answered {other:?}"),
                Err(problem) => Err(problem.0),
            }
        }
        other => panic!("the Questions box never sends {other:?}"),
    };
    Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent { query, result },
    ))))
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q_lists_the_engines_questions_an_answer_reaches_the_asker_and_enter_opens_a_waiting_terminal()
 {
    use crate::coord::{MessageKind, NewMessage, Party, QuestionFor, Severity, Waiting};
    use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
    use crate::project_setup::{SetupQuery, SetupView};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("ca-tui-questions-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service =
        crate::engine::bootstrap::EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
    let ordinary = crate::engine::Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = crate::client_controller::OwnerController::attach_configured(
        ordinary,
        OwnerClientKind::Tui,
    )
    .await
    .unwrap();
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let created = controller
        .setup_project(
            project(),
            crate::protocol::ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            crate::project_setup::ProjectSetup {
                about: "Agents ask the owner.".into(),
                tech_stack: vec![],
                platforms: vec![crate::project_setup::Platform::Cli],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            2,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));
    let mut engine = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    engine
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let now = crate::team::now_ms();
    let agent = |status: &str| {
        engine
            .execute(
                "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
                 VALUES (?1, 'sub', 'claude-code', 1, ?2, ?3, ?3)",
                rusqlite::params![project().as_str(), status, now],
            )
            .unwrap();
        engine.last_insert_rowid()
    };
    let (asker, waiter) = (agent("working"), agent("working"));
    let question = crate::coord::send(
        &engine,
        NewMessage {
            project: project(),
            from: Party::Agent(asker),
            to: Party::Owner,
            kind: MessageKind::Question,
            question: Some((QuestionFor::Me, Severity::High)),
            work: None,
            reply_to: None,
            body: "May I delete the old fixtures?\nAssumption: I keep them until you say.".into(),
            idempotency_key: None,
        },
        now,
    )
    .unwrap();
    crate::coord::set_waiting(&engine, waiter, Some(Waiting::Permission), now).unwrap();
    crate::team::questions::tick(&mut engine, now + 1).unwrap();
    let read = controller.coord_questions(project()).await.unwrap();
    assert_eq!(
        read.questions
            .iter()
            .map(|asked| (
                asked.id,
                asked.agent,
                asked.text.as_str(),
                asked.assumption.as_deref()
            ))
            .collect::<Vec<_>>(),
        [(
            question,
            asker,
            "May I delete the old fixtures?",
            Some("I keep them until you say.")
        )]
    );
    assert_eq!(
        read.waiting
            .iter()
            .map(|waiting| (waiting.agent, waiting.waiting, waiting.terminal.clone()))
            .collect::<Vec<_>>(),
        [(waiter, Waiting::Permission, AgentId::team(waiter).ok())],
        "the asker the urgent question paused is not listed as a terminal wait"
    );

    let mut shell = fresh(160, 40);
    let graph = controller.team_graph(project()).await.unwrap();
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(Box::new(graph)),
    });
    let first = served(&mut controller, shell.ctx.next_screen_request().unwrap()).await;
    assert!(shell.ctx.next_screen_request().is_none());
    shell.arrive(first);
    let rows = render(&mut shell);
    assert!(rows[39].contains("q Questions (2)"), "{}", rows[39]);

    shell.key(Key::Char('q'));
    let listed = served(&mut controller, shell.ctx.next_screen_request().unwrap()).await;
    shell.arrive(listed);
    let rows = render(&mut shell).join("\n");
    assert!(rows.contains("May I delete the old fixtures?"), "{rows}");
    assert!(rows.contains("I keep them until you say."), "{rows}");
    assert_eq!(read.questions[0].for_label, "Me");
    assert!(
        rows.lines()
            .any(|row| row.contains("For: Me") && row.contains("Urgent, the agent is paused")),
        "the question's head says whom it is for (OP-50): {rows}"
    );

    shell.key(Key::Enter);
    for typed in "Keep them.".chars() {
        shell.key(Key::Char(typed));
    }
    shell.key(Key::Enter);
    let change = shell.ctx.next_screen_request().unwrap();
    let Request::Setup(SetupQuery::TeamChange { action, .. }) = &change else {
        panic!("Enter on a written answer sends the team change");
    };
    assert_eq!(
        action,
        &crate::team::TeamAction::AnswerQuestion {
            project_id: project(),
            message: question,
            text: "Keep them.".into(),
        }
    );
    let answered = served(&mut controller, change).await;
    let Arrival::Owner(event) = &answered else {
        unreachable!()
    };
    let OwnerEvent::Setup(event) = event.as_ref() else {
        unreachable!()
    };
    assert!(matches!(
        event.result,
        Ok(SetupView::Changed(OwnerResult::Applied))
    ));
    shell.arrive(answered);
    let rows = render(&mut shell).join("\n");
    assert!(rows.contains("Answer sent to"), "{rows}");
    let replies: i64 = engine
        .query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE kind='reply' AND reply_to=?1 AND from_kind='owner' AND to_agent=?2 AND body='Keep them.'",
            rusqlite::params![question, asker],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        replies, 1,
        "the answer reaches the asker as the owner's reply"
    );

    let reread = served(&mut controller, shell.ctx.next_screen_request().unwrap()).await;
    shell.arrive(reread);
    assert!(shell.ctx.next_screen_request().is_none());
    shell.key(Key::Enter);
    let opened = requests(&mut shell);
    assert!(
        matches!(
            opened.first(),
            Some(Request::TerminalAttach { agent }) if Some(agent) == AgentId::team(waiter).ok().as_ref()
        ),
        "Enter on the waiting agent opens its terminal"
    );

    let again = controller
        .team_action(
            crate::team::TeamAction::AnswerQuestion {
                project_id: project(),
                message: question,
                text: "Again.".into(),
            },
            90,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            &again,
            OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
                if operation.result == OwnerResult::Rejected { code: "coord_question_closed".into() }
        ),
        "{again:?}"
    );
    drop(engine);
    drop(controller);
    service.shutdown().await.unwrap();
}

/// DEC-108: with the live master switched on and no terminal yet, a sign-in wait line from
/// the engine replaces the starting text in the master pane, so the owner sees why it waits.
#[test]
fn a_live_master_waiting_for_sign_in_rechecks_shows_the_wait_line() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(
        Some(true),
        Some(crate::team::master::SIGN_INS_CHECKING),
    ));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [check] if session_check(check)),
        "a switch already on is not turned on again; only the owner's session check goes out (LMS-1): {sent:?}"
    );
    let rows = render(&mut shell);
    assert!(
        rows[2].trim_start().starts_with("Live master on."),
        "the switch reads on: {rows:#?}"
    );
    assert_eq!(
        chars(&rows[3], 2, 78),
        format!("┌─ Master {}┐", "─".repeat(65))
    );
    let pane: Vec<String> = rows[4..9]
        .iter()
        .map(|row| chars(row, 2, 78))
        .take_while(|row| row.starts_with('│'))
        .map(|row| row.trim_matches('│').trim().to_owned())
        .collect();
    assert_eq!(
        pane.join(" ").trim(),
        crate::team::master::SIGN_INS_CHECKING,
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .all(|row| !row.contains("Starting the master's terminal")),
        "{rows:#?}"
    );
}

/// LMS-1: a started live master that waits for its account's profile has a terminal the
/// graph names but no pane yet; the engine's wait line shows in the master pane while the
/// terminal has not started, also when the engine closes the early attach, and goes once
/// the engine clears it.
#[test]
fn a_started_live_master_waiting_for_its_profile_shows_the_wait_line_until_its_terminal_starts() {
    let waiting = crate::team::master::PROFILE_IN_USE;
    let pane = |shell: &mut Shell| {
        let rows = render(shell);
        let lines: Vec<String> = rows[4..12]
            .iter()
            .map(|row| chars(row, 2, 78))
            .take_while(|row| row.starts_with('│'))
            .map(|row| row.trim_matches('│').trim().to_owned())
            .collect();
        (lines.join(" ").trim().to_owned(), rows)
    };
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), Some(waiting)));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(
            |request| matches!(request, Request::TerminalAttach { agent } if agent.as_str() == "team-1")
        ),
        "the graph names the master's terminal, so the view attaches: {sent:?}"
    );
    let (shown, rows) = pane(&mut shell);
    assert_eq!(shown, waiting, "while the attach waits: {rows:#?}");
    close_master(&mut shell, "terminal_agent_unknown");
    let (shown, rows) = pane(&mut shell);
    assert_eq!(shown, waiting, "once the early attach is closed: {rows:#?}");
    assert!(
        rows.iter()
            .all(|row| !row.contains("This agent has stopped")),
        "{rows:#?}"
    );
    shell.arrive(switch(Some(true), None));
    let (shown, rows) = pane(&mut shell);
    assert!(!shown.contains("Waiting for the account"), "{rows:#?}");
}

/// LMS-1 r1 L1: Stop all leaves the engine's wait line in the switch status, but the
/// paused team's master pane shows the paused team, not that the master waits for its
/// account's profile.
#[test]
fn a_paused_team_hides_the_masters_profile_wait_line() {
    let waiting = crate::team::master::PROFILE_IN_USE;
    let shows = |rows: &[String]| {
        rows.iter()
            .any(|row| row.contains("Waiting for the account"))
    };
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), Some(waiting)));
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    let rows = render(&mut shell);
    assert!(
        shows(&rows),
        "the running team shows the wait line: {rows:#?}"
    );
    shell.arrive(paused_with(Some(true), Some(waiting)));
    let rows = render(&mut shell);
    assert!(!shows(&rows), "Stop all hides it: {rows:#?}");
}

fn paused_read(on: Option<bool>) -> Arrival {
    paused_with(on, None)
}

fn paused_with(on: Option<bool>, problem: Option<&str>) -> Arrival {
    let mut arrival = switch(on, problem);
    let Arrival::Owner(event) = &mut arrival else {
        unreachable!()
    };
    let OwnerEvent::Setup(setup) = event.as_mut() else {
        unreachable!()
    };
    let Ok(crate::project_setup::SetupView::Team(status)) = &mut setup.result else {
        unreachable!()
    };
    status.paused = true;
    arrival
}

fn resumed(result: Result<(), &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::TeamChange {
                action: crate::team::TeamAction::Resume {
                    project_id: project(),
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

fn session_check(request: &Request) -> bool {
    matches!(
        request,
        Request::Setup(crate::project_setup::SetupQuery::OpenLiveSession { project: asked, .. })
            if *asked == project()
    )
}

fn resume_sent(request: &Request) -> bool {
    matches!(
        request,
        Request::Setup(crate::project_setup::SetupQuery::TeamChange {
            action: crate::team::TeamAction::Resume { project_id },
            ..
        }) if *project_id == project()
    )
}

const IDLE_LINE: &str = "The live master was turned off: it was idle for 15 min. \
                         It continues the same conversation when you start it again.";

/// A team read of a switch an idle stop turned off. Output: the arrival.
fn idle_stopped_read() -> Arrival {
    let mut status = master_status(Some(false), Some(IDLE_LINE), None);
    status.live_idle_stopped = true;
    team_read(status)
}

/// A master view showing the running live master's terminal. Output: the shell.
fn open_running_master() -> Shell {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(true), None));
    requests(&mut shell);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
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
            bytes: b"$ make\r\ndone".to_vec(),
        },
    });
    requests(&mut shell);
    shell
}

/// A graph read that shows the master stopped. Output: the arrival.
fn stopped_master_graph() -> Arrival {
    let mut stopped = graph();
    stopped.nodes[0].status = Some(AgentStatus::Stopped);
    stopped.nodes[0].terminal = None;
    Arrival::TeamGraph {
        project: project(),
        result: Ok(stopped),
    }
}

fn starts_asked(shell: &mut Shell) -> usize {
    requests(shell)
        .iter()
        .filter(|sent| live_master_change(sent, true))
        .count()
}

/// A master idle-stopped on the open view reads why, stays stopped and says s starts it
/// again, one start for the key, the terminal's keys included (DEC-117 Q4).
#[test]
fn a_master_idle_stopped_on_the_open_view_says_why_and_s_starts_it_again() {
    let mut shell = open_running_master();
    shell.arrive(stopped_master_graph());
    let sent = requests(&mut shell);
    assert!(sent.iter().any(switch_read), "the view reads why: {sent:?}");
    shell.arrive(idle_stopped_read());
    assert_eq!(
        starts_asked(&mut shell),
        0,
        "the stop holds until the owner acts"
    );
    let seen = render(&mut shell).join("\n");
    assert!(seen.contains("The live master was turned off"), "{seen}");
    assert!(seen.contains("s starts it again"), "{seen}");
    assert!(seen.contains("Start again"), "{seen}");
    assert!(!seen.contains("This agent has stopped"), "{seen}");
    shell.key(Key::Char('s'));
    assert_eq!(starts_asked(&mut shell), 1);
    shell.key(Key::Char('s'));
    assert_eq!(starts_asked(&mut shell), 0, "one start for one stop");
}

/// Stop all over a master an idle stop ended: the team's Start all is the one start offered,
/// and s asks for nothing, so no key starts the master under the owner's Stop all.
#[test]
fn a_master_idle_stopped_under_stop_all_offers_no_start_again() {
    let mut shell = open_running_master();
    shell.arrive(stopped_master_graph());
    requests(&mut shell);
    let mut status = master_status(Some(false), Some(IDLE_LINE), None);
    status.live_idle_stopped = true;
    status.paused = true;
    shell.arrive(team_read(status));
    assert_eq!(
        starts_asked(&mut shell),
        0,
        "Stop all holds the master stopped"
    );
    let seen = render(&mut shell).join("\n");
    assert!(!seen.contains("s starts it again"), "{seen}");
    assert!(!seen.contains("Start again"), "{seen}");
    shell.key(Key::Char('s'));
    assert_eq!(
        starts_asked(&mut shell),
        0,
        "s asks for no start under Stop all"
    );
}

/// UI-11-L2: opening the master view under Stop all with the switch off asks for no start,
/// so the switch is not written behind a stopped team, and the read after Start all starts
/// it.
#[test]
fn opening_the_master_view_under_stop_all_asks_for_no_start_until_the_team_runs() {
    for on in [None, Some(false)] {
        let mut shell = master_view(80, 24);
        requests(&mut shell);
        shell.arrive(paused_read(on));
        assert_eq!(
            starts_asked(&mut shell),
            0,
            "Stop all holds the switch: {on:?}"
        );
        shell.arrive(switch(on, None));
        assert_eq!(starts_asked(&mut shell), 1, "the team runs again: {on:?}");
    }
}

/// A master that stopped for any other reason keeps today's words and s does nothing.
#[test]
fn a_master_stopped_for_another_reason_offers_no_start_again() {
    let mut shell = open_running_master();
    shell.arrive(stopped_master_graph());
    requests(&mut shell);
    shell.arrive(switch(
        Some(false),
        Some("The live master was turned off: it had to start 3 times within 10 minutes. Start it again from the master view."),
    ));
    requests(&mut shell);
    let seen = render(&mut shell).join("\n");
    assert!(!seen.contains("Start again"), "{seen}");
    shell.key(Key::Char('s'));
    assert_eq!(starts_asked(&mut shell), 0);
}

/// A master view with no terminal after an idle stop offers s too (DEC-117 Q4).
#[test]
fn a_master_idle_stopped_with_no_terminal_offers_s() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(switch(Some(false), None));
    assert_eq!(starts_asked(&mut shell), 1, "opening the view starts it");
    shell.arrive(turned(true, Ok(())));
    requests(&mut shell);
    shell.arrive(idle_stopped_read());
    assert_eq!(starts_asked(&mut shell), 0, "once per open");
    let seen = render(&mut shell).join("\n");
    assert!(seen.contains("s starts it again"), "{seen}");
    assert!(
        !seen.contains("It starts again when you open this view"),
        "{seen}"
    );
    shell.key(Key::Char('s'));
    assert_eq!(starts_asked(&mut shell), 1);
}

#[test]
fn master_view_on_a_paused_team_offers_start_all() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(paused_read(Some(true)));
    assert!(
        requests(&mut shell).is_empty(),
        "a stopped team's switch already on is not turned on again"
    );
    let rows = render(&mut shell);
    assert!(rows[2].contains("Team stopped."), "{}", rows[2]);
    assert!(!rows[2].contains("Live master on"), "{}", rows[2]);
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "The team is stopped (Stop all). Start the team to run the master."
        )
    );
    assert_eq!(
        chars(&rows[5], 2, 78),
        format!(
            "│{:<74}│",
            "S starts every agent of this project again; the master starts with them."
        )
    );
    assert!(
        rows.iter()
            .all(|row| !row.contains("Starting the master's terminal")),
        "{rows:#?}"
    );
    assert!(rows[23].contains("S Start all"), "{}", rows[23]);

    shell.key(Key::Char('S'));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [change] if resume_sent(change)),
        "{sent:?}"
    );
    shell.arrive(resumed(Ok(())));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [read, check] if switch_read(read) && session_check(check)),
        "an accepted Start all reads the team again and sends the owner's session check (LMS-1): {sent:?}"
    );
    shell.arrive(switch(Some(true), None));
    assert!(requests(&mut shell).is_empty());
    let rows = render(&mut shell);
    assert!(rows[2].contains("Live master on"), "{}", rows[2]);
    assert!(!rows[2].contains("Team stopped."), "{}", rows[2]);
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "Starting the master's terminal. It opens once the master signs in and"
        )
    );
    assert!(!rows[23].contains("S Start all"), "{}", rows[23]);
    shell.key(Key::Char('S'));
    assert!(
        requests(&mut shell).is_empty(),
        "S starts nothing once the team runs"
    );
}

#[test]
fn a_refused_start_all_from_the_master_view_says_why_and_keeps_the_offer() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(paused_read(Some(true)));
    shell.key(Key::Char('S'));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [change] if resume_sent(change)),
        "{sent:?}"
    );
    shell.arrive(resumed(Err("trusted_connection_unavailable")));
    assert!(
        requests(&mut shell).is_empty(),
        "a refused change reads nothing"
    );
    let lead = shell.ctx.message().unwrap().lead.clone();
    assert_eq!(
        lead, "Not connected to the engine. Retrying.",
        "the same words as the team panel's Start all"
    );
    assert!(!lead.contains('_'), "no raw code reaches the owner: {lead}");
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!(
            "│{:<74}│",
            "The team is stopped (Stop all). Start the team to run the master."
        )
    );
    shell.key(Key::Char('S'));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [change] if resume_sent(change)),
        "S asks again after a refusal: {sent:?}"
    );
}

#[test]
fn a_refused_live_master_start_says_why_in_words_not_its_code() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent {
            query: crate::project_setup::SetupQuery::TeamChange {
                action: crate::team::TeamAction::LiveMaster {
                    project_id: project(),
                    on: true,
                    version: 0,
                },
                generation: 1,
            },
            result: Err("trusted_connection_unavailable".into()),
        },
    )))));
    let rows = render(&mut shell);
    assert_eq!(
        chars(&rows[4], 2, 78),
        format!("│{:<74}│", "The master terminal could not start.")
    );
    assert_eq!(
        chars(&rows[5], 2, 78),
        format!("│{:<74}│", "Not connected to the engine. Retrying.")
    );
    assert!(
        rows.iter()
            .all(|row| !row.contains("trusted_connection_unavailable")),
        "{rows:#?}"
    );
}

#[test]
fn a_hookless_agent_row_says_only_its_receipts_are_unavailable_for_its_account_kind() {
    let mut shell = fresh(120, 30);
    let mut graph = graph();
    graph.nodes[2].hookless = true;
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph),
    });
    let rows = render(&mut shell);
    let tree: Vec<String> = rows.iter().map(|row| chars(row, 2, 46)).collect();
    let under = tree
        .iter()
        .position(|row| row.contains("claude-2"))
        .expect("the hookless agent is drawn");
    let words: String = tree[under + 1..]
        .iter()
        .take_while(|row| !row.contains("codex-1"))
        .map(|row| {
            row.trim_matches(|c: char| c == '│' || c == ' ' || c == '└' || c == '├' || c == '─')
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        words.trim(),
        "Receipts unavailable for this account kind",
        "adoption counts and flags show for every agent (OQ-8): {tree:#?}"
    );
    let hooked = tree
        .iter()
        .position(|row| row.contains("claude-1"))
        .unwrap();
    assert!(
        !tree[hooked + 1].contains("Receipts"),
        "a hooked agent keeps its account row only: {tree:#?}"
    );
}

fn messages_page(agent: i64, older: bool) -> Box<crate::team::agent_messages::AgentMessages> {
    use crate::team::agent_messages::{
        AgentMessage, AgentMessages, EntryKind, MessageSettings, Source, Speaker,
    };
    let said = |id: i64, from: Speaker, kind: EntryKind, source: Source, text: &str| AgentMessage {
        at_ms: 1_793_000_000_000 + id * 60_000,
        source,
        id,
        from,
        kind,
        text: Some(text.into()),
        task: Some(7),
        state: None,
        result: None,
        result_more: false,
    };
    Box::new(AgentMessages {
        project_id: project(),
        agent,
        entries: vec![
            said(
                2,
                Speaker::Agent,
                EntryKind::Question,
                Source::Event,
                "Should the token\u{1b}[2J last a day?",
            ),
            said(
                1,
                Speaker::Master,
                EntryKind::Task,
                Source::Task,
                "Fix the login bug",
            ),
        ],
        older,
        settings: MessageSettings::default(),
    })
}

fn messages_asked(shell: &mut Shell) -> Vec<crate::project_setup::SetupQuery> {
    requests(shell)
        .into_iter()
        .filter_map(|request| match request {
            Request::Setup(query @ crate::project_setup::SetupQuery::TeamMessages { .. }) => {
                Some(query)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn m_on_an_agent_card_opens_its_messages_in_the_right_box_and_esc_closes_it() {
    use crate::project_setup::{SetupQuery, SetupView};
    let mut team = graph();
    team.nodes[1].id = "team-2".into();
    team.edges[0].to = "team-2".into();
    for (width, height) in [(120, 30), (80, 24)] {
        let mut shell = fresh(width, height);
        shell.arrive(Arrival::TeamGraph {
            project: project(),
            result: Ok(team.clone()),
        });
        let screen = render(&mut shell).join("\n");
        if width == 120 {
            assert!(screen.contains("m Messages"), "{screen}");
        }
        shell.key(Key::Down);
        shell.key(Key::Char('m'));
        let read = SetupQuery::TeamMessages {
            project: project(),
            agent: 2,
            before: None,
        };
        assert_eq!(messages_asked(&mut shell), std::slice::from_ref(&read));
        let screen = render(&mut shell).join("\n");
        assert!(screen.contains("Messages: claude-1"), "{screen}");
        assert!(screen.contains("Reading messages..."), "{screen}");

        shell.arrive(setup_reply(
            read.clone(),
            Ok(SetupView::TeamMessages(messages_page(2, true))),
        ));
        let rows = render(&mut shell);
        let screen = rows.join("\n");
        assert!(
            screen.contains("PgUp at the top reads older messages."),
            "{screen}"
        );
        assert!(
            screen.contains("Master > task: Fix the login bug"),
            "{screen}"
        );
        assert!(
            rows.iter().any(|row| row.contains("> ")
                && row.contains("Agent < question: Should the token")),
            "the newest entry is picked\n{screen}"
        );
        assert!(!screen.contains('\u{1b}'), "{screen}");
        assert!(screen.contains("Esc Back"), "{screen}");
        assert!(
            rows.iter()
                .any(|row| row.contains("│>") && row.contains("claude-1")),
            "the tree keeps its cursor while the list is open\n{screen}"
        );

        shell.key(Key::Home);
        shell.key(Key::PageUp);
        let older = messages_asked(&mut shell);
        assert!(
            matches!(
                older.as_slice(),
                [SetupQuery::TeamMessages {
                    agent: 2,
                    before: Some(cursor),
                    ..
                }] if cursor.id == 1
            ),
            "{older:?}"
        );
        shell.key(Key::Esc);
        let screen = render(&mut shell).join("\n");
        assert!(!screen.contains("Messages: claude-1"), "{screen}");
        assert!(screen.contains("Enter on an agent opens"), "{screen}");
        shell.key(Key::Char('m'));
        assert_eq!(
            messages_asked(&mut shell),
            std::slice::from_ref(&read),
            "m opens the same list again"
        );
    }
}

#[test]
fn m_on_the_master_card_says_where_its_conversation_is_and_reads_nothing() {
    let mut shell = team();
    shell.key(Key::Home);
    shell.key(Key::Char('m'));
    assert!(messages_asked(&mut shell).is_empty());
    let message = shell
        .ctx
        .message()
        .expect("m on the master says why it opens nothing");
    assert_eq!(message.lead, "The master has no Messages list.");
    assert_eq!(
        message.rest,
        crate::tui::screens::team_messages::MASTER_ELSEWHERE
    );
    shell.key(Key::Down);
    shell.key(Key::Char('m'));
    assert!(
        messages_asked(&mut shell).is_empty(),
        "a card without a team agent id reads nothing"
    );
    assert_eq!(
        shell
            .ctx
            .message()
            .expect("m on a card without a team agent says why")
            .lead,
        "This card has no messages to show."
    );
}

/// Owner 2026-11-05: the team tree prints the engine's message note under the agent it is
/// on, wrapped whole, as the app shows the same string; an agent without one shows none.
#[test]
fn an_agent_the_delivery_setting_leaves_without_messages_says_so_under_its_row() {
    let mut shell = fresh(120, 30);
    let mut graph = graph();
    graph.nodes[3].message_note = Some(crate::coord::delivery::NO_CHANNEL.to_owned());
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph),
    });
    let rows = render(&mut shell);
    let tree: Vec<String> = rows.iter().map(|row| chars(row, 2, 46)).collect();
    let trimmed = |row: &String| -> String {
        row.trim_matches(|c: char| c == '│' || c == ' ' || c == '└' || c == '├' || c == '─')
            .to_owned()
    };
    let under = tree
        .iter()
        .position(|row| row.contains("codex-1"))
        .expect("the Codex agent is drawn");
    assert!(
        tree[under + 1].contains("ChatGPT Plus"),
        "its account row stays first: {tree:#?}"
    );
    let words: Vec<String> = tree[under + 2..]
        .iter()
        .map(trimmed)
        .take_while(|row| !row.is_empty())
        .collect();
    assert_eq!(
        words.join(" "),
        crate::coord::delivery::NO_CHANNEL,
        "{tree:#?}"
    );
    assert!(
        tree.iter()
            .all(|row| !row.contains("Can't receive") || tree[under + 2..].contains(row)),
        "only the noted agent says it: {tree:#?}"
    );
}

fn fill_queue(shell: &mut Shell) {
    let mut filled = 0;
    while filled < 64
        && shell
            .ctx
            .try_request(Request::TeamGraph { project: project() })
    {
        filled += 1;
    }
    assert!(filled < 64, "the request queue is bounded");
}

#[test]
fn a_team_tab_settings_change_the_full_queue_refuses_says_busy_and_can_be_made_again() {
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::TeamAction;
    let mut shell = fresh(100, 40);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('s'));
    requests(&mut shell);
    shell.arrive(setup_reply(
        SetupQuery::Team { project: project() },
        Ok(SetupView::Team(Box::new(finished_team()))),
    ));
    requests(&mut shell);
    fill_queue(&mut shell);
    shell.key(Key::Char('w'));
    let text = render(&mut shell).join("\n");
    assert!(
        text.contains("CrazyAgent is busy"),
        "the refused change is said, not dropped: {text}"
    );
    assert!(!text.contains("Still saving"), "{text}");
    requests(&mut shell);
    shell.key(Key::Char('w'));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| matches!(
            request,
            Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::KeepWarm { .. },
                ..
            })
        )),
        "the change goes out once the queue has room: {sent:?}"
    );
}

#[test]
fn a_team_tab_settings_read_the_full_queue_refuses_is_asked_again_on_the_next_key() {
    use crate::project_setup::SetupQuery;
    let mut shell = team();
    fill_queue(&mut shell);
    shell.key(Key::Char('s'));
    requests(&mut shell);
    shell.key(Key::Down);
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| matches!(
            request,
            Request::Setup(SetupQuery::Team { project: asked }) if *asked == project()
        )),
        "the dropped read is not counted as out: {sent:?}"
    );
}

fn busy(shell: &Shell) -> bool {
    shell
        .ctx
        .message()
        .is_some_and(|said| said.rest.contains("CrazyAgent is busy"))
}

#[test]
fn a_live_master_start_the_full_queue_refuses_says_busy_instead_of_waiting_forever() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    fill_queue(&mut shell);
    shell.arrive(switch(Some(false), None));
    let rows = render(&mut shell);
    assert!(
        rows.iter()
            .any(|row| row.contains("The master terminal could not start.")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("CrazyAgent is busy")),
        "{rows:#?}"
    );
}

#[test]
fn an_open_session_check_the_full_queue_refuses_says_busy() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    fill_queue(&mut shell);
    shell.arrive(switch(Some(true), None));
    assert!(busy(&shell), "{:?}", shell.ctx.message());
}

#[test]
fn a_start_all_the_full_queue_refuses_says_busy_and_s_asks_again() {
    let mut shell = master_view(80, 24);
    requests(&mut shell);
    shell.arrive(paused_read(Some(true)));
    requests(&mut shell);
    fill_queue(&mut shell);
    shell.key(Key::Char('S'));
    let lead = shell.ctx.message().unwrap().lead.clone();
    assert!(
        lead.contains("CrazyAgent is busy"),
        "the refused Start all is said in words: {lead}"
    );
    requests(&mut shell);
    shell.key(Key::Char('S'));
    let sent = requests(&mut shell);
    assert!(
        matches!(sent.as_slice(), [change] if resume_sent(change)),
        "S asks again once the queue has room: {sent:?}"
    );
}

/// Opens the Finished box of a team with two finished agents and picks Sub-agent 3, the one
/// that can wake. Takes nothing. Returns the shell with an empty request queue.
fn finished_box() -> Shell {
    use crate::project_setup::{SetupQuery, SetupView};
    let mut shell = fresh(120, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('f'));
    requests(&mut shell);
    shell.arrive(setup_reply(
        SetupQuery::Team { project: project() },
        Ok(SetupView::Team(Box::new(finished_team()))),
    ));
    shell.key(Key::Down);
    requests(&mut shell);
    shell
}

#[test]
fn a_wake_the_full_queue_refuses_says_busy_and_w_wakes_again() {
    use crate::project_setup::SetupQuery;
    use crate::team::TeamAction;
    let mut shell = finished_box();
    fill_queue(&mut shell);
    shell.key(Key::Char('w'));
    let text = render(&mut shell).join("\n");
    assert!(text.contains("Could not wake Sub-agent 3."), "{text}");
    assert!(busy(&shell), "{:?}", shell.ctx.message());
    requests(&mut shell);
    shell.key(Key::Char('w'));
    let sent = requests(&mut shell);
    assert!(
        matches!(
            sent.as_slice(),
            [Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::WakeAgent { agent: 3, .. },
                ..
            })]
        ),
        "w wakes again once the queue has room: {sent:?}"
    );
}

#[test]
fn a_finished_setting_switch_the_full_queue_refuses_says_busy_and_h_switches_again() {
    use crate::project_setup::SetupQuery;
    use crate::team::TeamAction;
    let mut shell = finished_box();
    fill_queue(&mut shell);
    shell.key(Key::Char('h'));
    let lead = shell.ctx.message().unwrap().lead.clone();
    assert_eq!(lead, "Could not change where finished agents show.");
    assert!(busy(&shell), "{:?}", shell.ctx.message());
    requests(&mut shell);
    shell.key(Key::Char('h'));
    let sent = requests(&mut shell);
    assert!(
        matches!(
            sent.as_slice(),
            [Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::FinishedAgents { .. },
                ..
            })]
        ),
        "h switches again once the queue has room: {sent:?}"
    );
}

#[test]
fn an_answer_the_full_queue_refuses_is_kept_said_busy_and_sent_again_with_enter() {
    use crate::coord::{QuestionFor, Severity};
    use crate::project_setup::{SetupQuery, SetupView};
    use crate::team::TeamAction;
    use crate::team::questions::{Inbox, OwnerQuestion};
    let mut shell = fresh(120, 30);
    shell.arrive(Arrival::TeamGraph {
        project: project(),
        result: Ok(graph()),
    });
    requests(&mut shell);
    shell.key(Key::Char('q'));
    requests(&mut shell);
    shell.arrive(setup_reply(
        SetupQuery::CoordQuestions { project: project() },
        Ok(SetupView::CoordQuestions(Box::new(Inbox {
            project_id: project(),
            questions: vec![OwnerQuestion {
                id: 41,
                agent: 3,
                label: "Sub-agent 3".into(),
                severity: Severity::High,
                asked_for: QuestionFor::Me,
                for_label: "Me".into(),
                text: "May I delete the old fixtures?".into(),
                assumption: None,
                created_ms: 1,
                terminal: None,
            }],
            more: 0,
            waiting: Vec::new(),
        }))),
    ));
    shell.key(Key::Enter);
    for typed in "Keep them.".chars() {
        shell.key(Key::Char(typed));
    }
    requests(&mut shell);
    fill_queue(&mut shell);
    shell.key(Key::Enter);
    let lead = shell.ctx.message().unwrap().lead.clone();
    assert_eq!(lead, "The answer was not sent.");
    assert!(busy(&shell), "{:?}", shell.ctx.message());
    assert!(
        render(&mut shell).join("\n").contains("Keep them."),
        "the owner's answer stays in the box"
    );
    requests(&mut shell);
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert!(
        matches!(
            sent.as_slice(),
            [Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::AnswerQuestion { message: 41, text, .. },
                ..
            })] if text == "Keep them."
        ),
        "Enter sends the same answer once the queue has room: {sent:?}"
    );
}

#[test]
fn a_cleanup_read_the_full_queue_refuses_says_busy_and_c_reads_again() {
    use crate::tui::client::cleanup::CleanupQuery;
    let mut shell = team();
    fill_queue(&mut shell);
    shell.key(Key::Char('c'));
    assert!(busy(&shell), "{:?}", shell.ctx.message());
    assert!(
        !render(&mut shell)
            .join("\n")
            .contains("Reading finished agents"),
        "the box does not wait for a read that never went out"
    );
    requests(&mut shell);
    shell.key(Key::Char('c'));
    assert_eq!(
        cleanup_sent(&mut shell),
        vec![CleanupQuery::Finished { project: project() }]
    );
}

#[test]
fn a_cleanup_removal_the_full_queue_refuses_says_nothing_was_removed() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    shell.arrive(cleanup_reply(
        read,
        Ok(CleanupReply::Finished(vec![finished(
            4,
            "Ada (@login-page)",
            1024 * 1024,
        )])),
    ));
    fill_queue(&mut shell);
    shell.key(Key::Char('y'));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Nothing was removed.");
    assert!(busy(&shell), "{message:?}");
    assert!(
        !render(&mut shell).join("\n").contains("Removing"),
        "the box does not wait for a removal that never went out"
    );
}

#[test]
fn a_terminal_attach_the_full_queue_refuses_says_busy_and_enter_attaches_again() {
    let mut shell = team();
    fill_queue(&mut shell);
    shell.key(Key::Enter);
    let text = render(&mut shell).join("\n");
    assert!(
        text.contains("The terminal closed."),
        "the refused attach closes the box instead of waiting for frames: {text}"
    );
    assert!(text.contains("CrazyAgent is busy"), "{text}");
    requests(&mut shell);
    shell.key(LEADER);
    shell.key(Key::Enter);
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|request| matches!(
            request,
            Request::TerminalAttach { agent } if agent.as_str() == "team-1"
        )),
        "Enter on the agent asks for its terminal again: {sent:?}"
    );
}

fn held(id: i64, label: &str, code: &str) -> crate::team::cleanup::FinishedAgent {
    crate::team::cleanup::FinishedAgent {
        bytes: 0,
        kept: Some(code.into()),
        ..finished(id, label, 0)
    }
}

#[test]
fn an_agent_a_check_keeps_is_listed_with_its_reason_and_y_removes_only_the_others() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    let agents = vec![
        finished(4, "Ada (@login-page)", 3 * 1024 * 1024),
        held(3, "Lin (@docs)", "cleanup_commits_unlanded"),
    ];
    shell.arrive(cleanup_reply(read, Ok(CleanupReply::Finished(agents))));
    let rows = render(&mut shell);
    let text = rows.join("\n");
    assert!(
        text.contains("Worktrees of 1 finished agent (3 MB)"),
        "{text}"
    );
    assert_eq!(
        pane_row(&rows, 3),
        padded("Ada (@login-page)  sub-agent, Codex  3 MB")
    );
    assert_eq!(
        pane_row(&rows, 4),
        padded("Lin (@docs) stays: it has commits that have")
    );
    assert_eq!(pane_row(&rows, 5), padded("not landed"));
    assert!(
        text.contains("y Remove and free 3 MB   n Keep them"),
        "{text}"
    );
    shell.key(Key::Char('y'));
    assert_eq!(
        cleanup_sent(&mut shell),
        vec![CleanupQuery::Remove {
            project: project(),
            agents: vec![4],
        }],
        "an agent a check keeps is never sent"
    );
}

#[test]
fn when_a_check_keeps_every_finished_agent_the_box_says_why_and_y_removes_nothing() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let mut shell = team();
    shell.key(Key::Char('c'));
    let read = CleanupQuery::Finished { project: project() };
    assert_eq!(cleanup_sent(&mut shell), vec![read.clone()]);
    shell.arrive(cleanup_reply(
        read,
        Ok(CleanupReply::Finished(vec![held(
            3,
            "Lin (@docs)",
            "cleanup_changes_open",
        )])),
    ));
    let rows = render(&mut shell);
    let text = rows.join("\n");
    assert!(text.contains("Finished agents"), "{text}");
    assert_eq!(
        pane_row(&rows, 3),
        padded("Lin (@docs) stays: its changes wait for")
    );
    assert_eq!(pane_row(&rows, 4), padded("your review"));
    assert!(text.contains("Nothing can go now.   Esc Close"), "{text}");
    assert!(!text.contains("y Remove"), "{text}");
    shell.key(Key::Char('y'));
    assert!(requests(&mut shell).is_empty(), "y removes nothing");
    assert!(render(&mut shell).join("\n").contains("Lin (@docs) stays"));
    shell.key(Key::Esc);
    assert!(!render(&mut shell).join("\n").contains("Lin (@docs) stays"));
}

#[test]
fn every_code_a_check_keeps_an_agent_with_has_its_own_reason() {
    for code in [
        "cleanup_agent_has_task",
        "cleanup_agent_not_found",
        "cleanup_agent_running",
        "cleanup_changes_open",
        "cleanup_changes_restorable",
        "cleanup_commits_unlanded",
        "cleanup_refs_unlanded",
        "cleanup_worktree_dirty",
        "cleanup_worktree_invalid",
        "cleanup_worktree_leftover",
        "cleanup_worktree_unreadable",
    ] {
        assert!(
            !super::cleanup::kept_reason(code).starts_with("a check keeps it"),
            "{code}"
        );
    }
    assert_eq!(
        super::cleanup::kept_reason("cleanup_new_check"),
        "a check keeps it (cleanup_new_check)"
    );
}
