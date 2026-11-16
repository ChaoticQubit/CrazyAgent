use super::*;

/// Opens a team runtime whose only harness is the stand-in ACP harness, a script that does
/// nothing. Input: the fixture. Output: the runtime.
fn listed_runtime(fixture: &Fixture) -> TeamRuntime {
    let bin = fixture.base.join("listed-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let standin = bin.join("standin-acp");
    std::fs::write(&standin, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&standin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("standin-acp", standin)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
}

/// The fixture's keyless endpoint as a team agent reaches it. Output: the account.
fn keyless_endpoint() -> TeamAccount {
    TeamAccount {
        connection: reference(ENDPOINT),
        reach: TeamReach::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        },
    }
}

/// T19b prerequisite (fix8 Q12 A): a team agent of a listed harness runs in its T18 private
/// home, `<agent home>/<harness id>/<account>`, where the listed launch's lock and files are
/// walked; the agent home itself stays its sandbox's home.
#[test]
fn a_team_agent_of_a_listed_harness_runs_in_its_private_home_inside_the_agent_home() {
    let fixture = Fixture::new(1);
    let runtime = listed_runtime(&fixture);
    let account = keyless_endpoint();
    assert_eq!(
        fixture.agent(AgentRole::Sub, "standin-acp", AgentStatus::Starting),
        1
    );

    let prepared = runtime.prepare(
        &fixture.setup,
        1,
        AgentRole::Sub,
        "standin-acp",
        false,
        &account,
        false,
        crate::agent_git::Layout::Linked,
        false,
    );

    let (_, _, mode) = prepared.unwrap();
    let Mode::Kept { account, .. } = &mode else {
        panic!("an endpoint agent is kept between turns");
    };
    let own = agent_home(&fixture.base.join("team"), 1);
    assert_eq!(
        account.home(),
        own.join("standin-acp").join(ENDPOINT).as_path()
    );
    let bits = std::fs::metadata(account.home())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(bits & 0o777, 0o700);
}

/// T19b 19b-2: a team agent of a listed harness on an endpoint account is handed that
/// account by the stored connection's wire format: the context it starts with carries the
/// hand-over its manifest's `accounts` entry fills, here the keyless address and the local
/// placeholder.
#[test]
fn a_team_agent_of_a_listed_harness_starts_with_its_accounts_hand_over() {
    let fixture = Fixture::new(1);
    let runtime = listed_runtime(&fixture);
    let account = keyless_endpoint();
    assert_eq!(
        fixture.agent(AgentRole::Sub, "standin-acp", AgentStatus::Starting),
        1
    );

    let prepared = runtime.prepare(
        &fixture.setup,
        1,
        AgentRole::Sub,
        "standin-acp",
        false,
        &account,
        false,
        crate::agent_git::Layout::Linked,
        false,
    );

    let (_, _, mode) = prepared.unwrap();
    let Mode::Kept { context, .. } = &mode else {
        panic!("an endpoint agent is kept between turns");
    };
    let handed = context.handover();
    assert!(handed.is_some(), "a listed agent is handed its account");
    let handed = handed.unwrap();
    let local = crate::model_gateway::relay::LOCAL_PLACEHOLDER;
    assert_eq!(handed.base_url(), "http://127.0.0.1:11434/v1");
    assert_eq!(handed.token(), local);
    assert_eq!(
        handed.env(),
        [
            (
                "OPENAI_BASE_URL".to_owned(),
                "http://127.0.0.1:11434/v1".to_owned()
            ),
            ("OPENAI_API_KEY".to_owned(), local.to_owned()),
            ("STANDIN_MODEL".to_owned(), "qwen2.5:0.5b".to_owned()),
        ]
    );
}

/// T19b 19b-2 guard row: a listed agent's hand-over is read from the stored connection, so
/// an agent whose account is no longer in the live access is refused in plain words.
#[test]
fn a_team_agent_of_a_listed_harness_whose_account_left_the_pool_is_refused() {
    let fixture = Fixture::new(1);
    let runtime = listed_runtime(&fixture);
    let account = TeamAccount {
        connection: reference("gone-account"),
        ..keyless_endpoint()
    };
    assert_eq!(
        fixture.agent(AgentRole::Sub, "standin-acp", AgentStatus::Starting),
        1
    );

    let prepared = runtime.prepare(
        &fixture.setup,
        1,
        AgentRole::Sub,
        "standin-acp",
        false,
        &account,
        false,
        crate::agent_git::Layout::Linked,
        false,
    );

    let refused = prepared.err().unwrap_or_default();
    assert!(refused.contains("team_account_left_pool"), "{refused}");
}

/// rv-sec3 T19b F2: a listed harness's launch writes no hooks, so its team agent is saved
/// hookless, and one with tools is refused under Overlaps Block, as a hookless Claude Code
/// agent is, since the edit guard is a hook.
#[test]
fn a_tools_on_team_agent_of_a_listed_harness_is_hookless_and_refused_under_block() {
    use crate::coord::settings::{CoordSettings, Overlaps, write};
    let fixture = Fixture::new(1);
    let runtime = listed_runtime(&fixture);
    write(
        &fixture.connection,
        &ProjectId::new(PROJECT).unwrap(),
        &CoordSettings {
            overlaps: Overlaps::Block,
            ..CoordSettings::default()
        },
    )
    .unwrap();
    let with_tools = fixture.agent(AgentRole::Sub, "standin-acp", AgentStatus::Starting);

    let prepared = runtime.prepare(
        &fixture.setup,
        with_tools,
        AgentRole::Sub,
        "standin-acp",
        true,
        &keyless_endpoint(),
        false,
        crate::agent_git::Layout::Linked,
        false,
    );

    let refused = prepared.err().unwrap_or_default();
    assert!(
        refused.contains(crate::protocol::plain_code("overlap_block_needs_hooks")),
        "{refused}"
    );
    assert!(hookless_saved(&fixture, with_tools));
}

const MASTER_REPLY: &str = "MASTER-19B-HANDED page.md to a sub-agent";

const SUB_REPORT: &str = "SUB-19B-REPORT page.md is written";

/// One ACP `agent_message_chunk` of the stand-in's session, as its `turn.out` replays it.
/// Input: the text. Output: the JSON line.
fn chunk(text: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "standin-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }
        }
    })
    .to_string()
}

/// The stand-in ACP script as Row B's runtime runs it: once it has made its state folder in
/// the private home, the master's copy (agent 1) writes a first turn that calls the engine's
/// `delegate` tool for a stand-in sub-agent and then replies `MASTER_REPLY`, and every other
/// agent's copy replies `SUB_REPORT`. Output: the script text.
fn delegating_standin() -> String {
    const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin.sh");
    const MADE: &str = "mkdir -p \"$state\" || refuse \"the folder $state cannot be made.\"\n";
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"standin","version":"1"}}}"#;
    let delegate = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delegate","arguments":{"harness":"standin-acp","task":"Write page.md"}}}"#;
    let (master, sub) = (chunk(MASTER_REPLY), chunk(SUB_REPORT));
    for line in [initialize, delegate, master.as_str(), sub.as_str()] {
        assert!(!line.contains('\''), "{line}");
    }
    let turns = format!(
        "case $home in\n*/agents/agent-1/home/*)\n\tprintf '%s\\n' '{initialize}' '{delegate}' >\"$state/turn-1.mcp\"\n\tprintf '%s\\n' '{master}' >\"$state/turn.out\"\n\t;;\n*) printf '%s\\n' '{sub}' >\"$state/turn.out\" ;;\nesac\n"
    );
    assert_eq!(STANDIN.matches(MADE).count(), 1);
    STANDIN.replace(MADE, &format!("{MADE}{turns}"))
}

/// T19b Row B (QUESTION-2013 A, item 3), the runtime e2e: a tools-on master of the stand-in
/// ACP harness on the keyless endpoint calls `delegate` through the engine's team server, the
/// runtime takes the call at its reply's end and starts a stand-in sub-agent on the task, and
/// the master's next turn, kept between messages, carries the sub-agent's report as a
/// message from the engine. Written not run (DEC-170): expected to pass at d9414065d.
#[test]
fn a_listed_master_delegates_to_a_listed_sub_agent_and_its_next_turn_carries_the_report() {
    use crate::protocol::Outcome;
    use crate::team::TeamAction;
    let mut fixture = Fixture::new(1);
    fixture.setup.team.master = "standin-acp".into();
    fixture.setup.team.master_tools = true;
    fixture.setup.team.sub_harnesses = vec![HarnessSlot {
        harness: "standin-acp".into(),
        agents: 1,
        tools: true,
    }];
    fixture
        .connection
        .execute(
            "UPDATE project_setups SET body=?1 WHERE project=?2",
            params![serde_json::to_string(&fixture.setup).unwrap(), PROJECT],
        )
        .unwrap();
    let bin = fixture.base.join("listed-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let standin = bin.join("standin-acp");
    std::fs::write(&standin, delegating_standin()).unwrap();
    std::fs::set_permissions(&standin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    let engine = crate::team::runtime::live_stop_hook_tests::engine_executable();
    let mut team = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("standin-acp", standin)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_delegation_tool(&engine)
    .unwrap();
    let tx = fixture.connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &TeamAction::Message {
                project_id: fixture.setup.project_id.clone(),
                text: "Plan the page".into(),
            },
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let log = agent_home(&fixture.base.join("team"), 1)
        .join("standin-acp")
        .join(ENDPOINT)
        .join("standin")
        .join("stdin-1");
    let prompts = || -> Vec<String> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("\"session/prompt\""))
            .map(str::to_owned)
            .collect()
    };

    fixture.until(&mut team, "the master hears the sub-agent's report", |_| {
        prompts().len() >= 2
    });

    assert_eq!(masters(&fixture.connection), [1]);
    let tasks: Vec<(String, String)> = fixture
        .connection
        .prepare("SELECT harness,prompt FROM team_tasks WHERE project=?1 ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tasks,
        [("standin-acp".to_owned(), "Write page.md".to_owned())]
    );
    let prompts = prompts();
    assert!(prompts[0].contains("Plan the page"), "{prompts:#?}");
    assert!(
        prompts[1].contains("Message from the engine:"),
        "{prompts:#?}"
    );
    assert!(prompts[1].contains(SUB_REPORT), "{prompts:#?}");
    let master = fixture.status().messages.iter().any(|message| {
        message.author == MessageAuthor::Master && message.text.contains(MASTER_REPLY)
    });
    assert!(master, "{:#?}", fixture.status().messages);
    team.shutdown().unwrap();
}
