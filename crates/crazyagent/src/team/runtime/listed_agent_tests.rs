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
