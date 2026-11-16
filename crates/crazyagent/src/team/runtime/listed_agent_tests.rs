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

    assert!(prepared.is_ok(), "{:?}", prepared.as_ref().err());
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

    assert!(prepared.is_ok(), "{:?}", prepared.as_ref().err());
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

/// rv-code4 T19b F2: a tools-on team agent of a listed harness runs with HOME at its private
/// home, while its temp folder, its own `git` and its cargo home are the ones `prepare` made
/// in the agent home its sandbox grants, so the launch never falls back to the private home
/// for TMPDIR or points CARGO_HOME at a folder nothing prepared.
#[test]
fn a_tools_on_listed_team_agent_launches_with_the_agent_homes_temp_folder_and_cargo_home() {
    struct TmpRemoved(PathBuf);
    impl Drop for TmpRemoved {
        fn drop(&mut self) {
            let _ = crate::sandbox::toolchains::remove_tmp(&self.0);
        }
    }
    let fixture = Fixture::new(1);
    let runtime = listed_runtime(&fixture);
    let id = fixture.agent(AgentRole::Sub, "standin-acp", AgentStatus::Starting);
    let own = agent_home(&fixture.base.join("team"), id);
    let _removed = TmpRemoved(own.clone());

    let prepared = runtime.prepare(
        &fixture.setup,
        id,
        AgentRole::Sub,
        "standin-acp",
        true,
        &keyless_endpoint(),
        false,
        crate::agent_git::Layout::Linked,
        false,
    );

    assert!(prepared.is_ok(), "{:?}", prepared.as_ref().err());
    let (_, plan, mode) = prepared.unwrap();
    let Mode::Kept { account, context } = mode else {
        panic!("an endpoint agent is kept between turns");
    };
    let request = plan.request(account, plan.access);
    assert!(request.is_ok(), "{:?}", request.as_ref().err());
    let request = request.unwrap();
    let built = crate::harnesses::launch::command_with_context(&request, &context);
    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let (command, _installed) = built.unwrap();
    let env = |wanted: &str| {
        command
            .get_envs()
            .find(|(name, _)| *name == wanted)
            .and_then(|(_, value)| value)
            .map(PathBuf::from)
    };
    let scratch = crate::sandbox::toolchains::existing_tmp(&own);
    assert!(scratch.is_some(), "prepare made the agent's temp folder");
    let scratch = scratch.unwrap();
    assert_eq!(env("HOME"), Some(own.join("standin-acp").join(ENDPOINT)));
    assert_eq!(env("TMPDIR").as_deref(), Some(scratch.as_path()));
    assert_eq!(
        crate::sandbox::toolchains::private_folder_pin(&scratch),
        plan.scratch_pin
    );
    assert_eq!(env("CARGO_HOME"), Some(own.join(".cargo")));
    assert!(own.join(".cargo").is_dir());
    let git = scratch.join("bin");
    if git.join("git").is_file() {
        let path = env("PATH").unwrap_or_default();
        assert!(
            path.as_os_str()
                .to_string_lossy()
                .starts_with(&format!("{}:", git.display())),
            "{path:?}"
        );
    }
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

/// The stand-in ACP script with a shell block it runs once it has made its state folder in
/// the private home, which writes that agent's turn fixtures there. Input: the block.
/// Output: the script text.
fn standin_with(turns: &str) -> String {
    const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin.sh");
    const MADE: &str = "mkdir -p \"$state\" || refuse \"the folder $state cannot be made.\"\n";
    assert_eq!(STANDIN.matches(MADE).count(), 1);
    STANDIN.replace(MADE, &format!("{MADE}{turns}"))
}

/// The stand-in ACP script whose every turn replies with one text. Input: the text.
/// Output: the script text.
fn replying_standin(text: &str) -> String {
    let reply = chunk(text);
    assert!(!reply.contains('\''), "{reply}");
    standin_with(&format!("printf '%s\\n' '{reply}' >\"$state/turn.out\"\n"))
}

/// The stand-in ACP script as Row B's runtime runs it: the master's copy (agent 1) writes a
/// first turn that calls the engine's `delegate` tool for a stand-in sub-agent and then
/// replies `MASTER_REPLY`, and every other agent's copy replies `SUB_REPORT`. Output: the
/// script text.
fn delegating_standin() -> String {
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"standin","version":"1"}}}"#;
    let delegate = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delegate","arguments":{"harness":"standin-acp","task":"Write page.md"}}}"#;
    let (master, sub) = (chunk(MASTER_REPLY), chunk(SUB_REPORT));
    for line in [initialize, delegate, master.as_str(), sub.as_str()] {
        assert!(!line.contains('\''), "{line}");
    }
    standin_with(&format!(
        "case $home in\n*/agents/agent-1/home/*)\n\tprintf '%s\\n' '{initialize}' '{delegate}' >\"$state/turn-1.mcp\"\n\tprintf '%s\\n' '{master}' >\"$state/turn.out\"\n\t;;\n*) printf '%s\\n' '{sub}' >\"$state/turn.out\" ;;\nesac\n"
    ))
}

/// Saves a plan whose master and sub-agent harness is the stand-in ACP harness, with the
/// tools switch given for both. Inputs: the fixture and the switch.
fn plan_listed(fixture: &mut Fixture, tools: bool) {
    fixture.setup.team.master = "standin-acp".into();
    fixture.setup.team.master_tools = tools;
    fixture.setup.team.sub_harnesses = vec![HarnessSlot {
        harness: "standin-acp".into(),
        agents: 1,
        tools,
    }];
    fixture
        .connection
        .execute(
            "UPDATE project_setups SET body=?1 WHERE project=?2",
            params![serde_json::to_string(&fixture.setup).unwrap(), PROJECT],
        )
        .unwrap();
}

/// Opens a team runtime whose stand-in ACP harness runs one script, with the built engine as
/// its delegation tool when one is given. Inputs: the fixture, the script and the engine.
/// Output: the runtime.
fn scripted_runtime(fixture: &Fixture, script: &str, engine: Option<&Path>) -> TeamRuntime {
    let bin = fixture.base.join("listed-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let standin = bin.join("standin-acp");
    std::fs::write(&standin, script).unwrap();
    std::fs::set_permissions(&standin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    let runtime = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("standin-acp", standin)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap();
    match engine {
        Some(engine) => runtime.with_delegation_tool(engine).unwrap(),
        None => runtime,
    }
}

/// Sends the owner's team message, as the owner socket does. Inputs: the fixture and the
/// text.
fn say(fixture: &mut Fixture, text: &str) {
    let tx = fixture.connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &crate::team::TeamAction::Message {
                project_id: fixture.setup.project_id.clone(),
                text: text.into(),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
}

/// Tells whether the master stored a reply that holds a text. Inputs: the fixture and the
/// text. Output: the answer.
fn master_said(fixture: &Fixture, text: &str) -> bool {
    fixture
        .status()
        .messages
        .iter()
        .any(|message| message.author == MessageAuthor::Master && message.text.contains(text))
}

/// The `session/prompt` lines every run of one agent's stand-in logged in its private home,
/// in run order. Inputs: the fixture and the agent. Output: the lines.
fn prompts_of(fixture: &Fixture, agent: i64) -> Vec<String> {
    let state = agent_home(&fixture.base.join("team"), agent)
        .join("standin-acp")
        .join(ENDPOINT)
        .join("standin");
    (1..=8)
        .flat_map(|run| {
            std::fs::read_to_string(state.join(format!("stdin-{run}")))
                .unwrap_or_default()
                .lines()
                .filter(|line| line.contains("\"session/prompt\""))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// rv-code4 T19b F5: a listed team agent starts through its plan's request, which checks the
/// hooks state `prepare` recorded, now hookless for every listed harness, and runs: a
/// tools-off stand-in ACP master on the keyless endpoint answers the owner's message.
#[test]
fn a_listed_team_master_starts_and_answers_the_owners_message() {
    let mut fixture = Fixture::new(1);
    plan_listed(&mut fixture, false);
    let mut team = scripted_runtime(&fixture, &replying_standin(MASTER_REPLY), None);

    say(&mut fixture, "Plan the page");

    fixture.until(&mut team, "the listed master answers the owner", |f| {
        master_said(f, MASTER_REPLY)
    });
    let master = masters(&fixture.connection);
    assert_eq!(master.len(), 1);
    assert!(hookless_saved(&fixture, master[0]));
    assert!(
        prompts_of(&fixture, master[0])
            .iter()
            .any(|prompt| prompt.contains("Plan the page"))
    );
    team.shutdown().unwrap();
}

/// T19b Row B (QUESTION-2013 A, item 3), the runtime e2e: a tools-on master of the stand-in
/// ACP harness on the keyless endpoint calls `delegate` through the engine's team server, the
/// runtime takes the call at its reply's end and starts a stand-in sub-agent on the task, and
/// the master's next turn, kept between messages, carries the sub-agent's report as a
/// message from the engine. It needs the built engine, as the stop hook rows do. Written not
/// run (DEC-170).
#[test]
fn a_listed_master_delegates_to_a_listed_sub_agent_and_its_next_turn_carries_the_report() {
    let mut fixture = Fixture::new(1);
    plan_listed(&mut fixture, true);
    let engine = crate::team::runtime::live_stop_hook_tests::engine_executable();
    let mut team = scripted_runtime(&fixture, &delegating_standin(), Some(&engine));
    let tasks = |fixture: &Fixture| -> Vec<(String, String)> {
        fixture
            .connection
            .prepare("SELECT harness,prompt FROM team_tasks WHERE project=?1 ORDER BY id")
            .unwrap()
            .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    say(&mut fixture, "Plan the page");

    fixture.until(&mut team, "the master's delegate call makes a task", |f| {
        !tasks(f).is_empty()
    });
    assert_eq!(
        tasks(&fixture),
        [("standin-acp".to_owned(), "Write page.md".to_owned())]
    );
    assert!(master_said(&fixture, MASTER_REPLY));
    assert_eq!(masters(&fixture.connection), [1]);
    fixture.until(&mut team, "the master hears the sub-agent's report", |f| {
        prompts_of(f, 1)
            .iter()
            .any(|prompt| prompt.contains(SUB_REPORT))
    });
    let prompts = prompts_of(&fixture, 1);
    assert!(prompts[0].contains("Plan the page"), "{prompts:#?}");
    let heard = prompts
        .iter()
        .find(|prompt| prompt.contains(SUB_REPORT))
        .unwrap();
    assert!(heard.contains("Message from the engine:"), "{heard}");
    team.shutdown().unwrap();
}
