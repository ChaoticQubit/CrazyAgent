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
