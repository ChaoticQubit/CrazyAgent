use super::*;
use crate::agent_git::Layout;
use crate::connectors::grants::{self, Who};
use crate::connectors::launch::SERVER_NAME;
use crate::connectors::mcp_client::tests::{BOARD, KEY};
use crate::sandbox::policy::{AgentNetwork, AgentPolicy};
use std::os::unix::fs::PermissionsExt;

/// Opens the fixture's team runtime with a Codex stand-in that is never run and an engine
/// file as its delegate tool, so a tools-on launch can carry the connector entry. Input:
/// the fixture. Output: the runtime.
fn connector_runtime(fixture: &Fixture) -> TeamRuntime {
    std::fs::set_permissions(
        fixture.base.join("engine.db"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let folder = fixture.base.join("engine-bin");
    std::fs::create_dir(&folder).unwrap();
    let engine = folder.join("crazyagent");
    engine_file(&engine);
    let access = fixture.access.clone();
    TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_delegation_tool(&engine)
    .unwrap()
}

/// A managed account whose profile home holds a placeholder sign-in, as a team agent on a
/// signed-in account has. Input: the runtime. Output: the account.
fn managed_account(runtime: &TeamRuntime) -> TeamAccount {
    let profile = reference("managed-profile");
    let managed = runtime.custody.as_ref().unwrap().profile_home(&profile);
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(managed.join("auth.json"), "{}").unwrap();
    TeamAccount {
        connection: reference("managed-connection"),
        reach: TeamReach::Managed {
            profile_ref: profile,
            model: None,
        },
    }
}

/// Plans one tools-on Codex sub-agent through the launch the engine uses, which installs
/// its Seatbelt profile. Inputs: the runtime, the fixture, the agent id and the account.
/// Output: the plan.
fn sub_agent(runtime: &TeamRuntime, fixture: &Fixture, id: i64, account: &TeamAccount) -> Plan {
    let (_, plan, _) = runtime
        .prepare(
            &fixture.setup,
            id,
            AgentRole::Sub,
            "codex",
            true,
            account,
            false,
            Layout::Linked,
            false,
        )
        .unwrap();
    assert!(plan.sandbox_profile.is_file());
    plan
}

/// Trusts the `board` connector, binds its key and turns it on for the fixture's project,
/// for every agent. Input: the fixture.
fn grant_board(fixture: &Fixture) {
    crate::app_plugins::store::add(
        &fixture.connection,
        &fixture.base,
        BOARD,
        &crate::semantic::sha256_hex(BOARD.as_bytes()),
        false,
        1,
    )
    .unwrap();
    crate::app_plugins::store::set_key(
        &fixture.connection,
        &fixture.base,
        "board",
        "api_key",
        KEY.as_bytes().to_vec(),
    )
    .unwrap();
    grants::set(
        &fixture.connection,
        &fixture.base,
        PROJECT,
        "board",
        true,
        Who::Both,
        2,
    )
    .unwrap();
}

/// The network lines of a launch's installed profile, its agent folder and model proxy
/// port named the same for every agent, so two agents' remotes compare. Inputs: the
/// runtime, the plan and the agent id. Output: the lines.
fn network_lines(runtime: &TeamRuntime, plan: &Plan, id: i64) -> Vec<String> {
    let mut text = std::fs::read_to_string(&plan.sandbox_profile)
        .unwrap()
        .replace(runtime.agent_folder(id).to_str().unwrap(), "AGENT");
    if let Some(proxy) = &plan.proxy {
        text = text.replace(&format!("localhost:{}", proxy.port()), "localhost:PROXY");
    }
    text.lines()
        .filter(|line| line.contains("network"))
        .map(str::to_owned)
        .collect()
}

/// Task 3r row 5 on the real team launch: at every agent network choice, a tools-on agent
/// of a project with a granted connector gets one gateway launch, its own launch folder in
/// its profile and the ca-connectors entry in its per-turn context, while its network
/// lines stay the ones an agent with no grant gets; its run's end removes the launch.
#[test]
fn an_only_network_project_gets_connectors_and_its_remotes_are_unchanged() {
    for network in [AgentNetwork::None, AgentNetwork::Https, AgentNetwork::Open] {
        let fixture = Fixture::new(2);
        crate::sandbox::policy::save(
            &fixture.connection,
            PROJECT,
            &AgentPolicy {
                network,
                ..AgentPolicy::default()
            },
        )
        .unwrap();
        let runtime = connector_runtime(&fixture);
        for id in [1, 2] {
            assert_eq!(
                fixture.agent(AgentRole::Sub, "codex", AgentStatus::Starting),
                id
            );
        }
        let account = managed_account(&runtime);
        let project = ProjectId::new(PROJECT).unwrap();
        let has_entry = |id: i64| {
            runtime
                .context(&project, id, AgentRole::Sub, true, Layout::Linked)
                .unwrap()
                .plugins()
                .servers
                .iter()
                .any(|server| server.name == SERVER_NAME)
        };

        let plain = sub_agent(&runtime, &fixture, 1, &account);
        assert!(
            runtime.connector_launches.lock().unwrap().is_empty(),
            "{network:?}: a project with no grant gets no launch"
        );
        assert!(!has_entry(1), "{network:?}");

        grant_board(&fixture);

        let granted = sub_agent(&runtime, &fixture, 2, &account);
        let launch = runtime
            .connector_launches
            .lock()
            .unwrap()
            .get(&2)
            .cloned()
            .unwrap_or_else(|| panic!("{network:?}: a granted tools-on agent gets a launch"));
        assert_eq!(launch.snapshot.connectors, ["board"], "{network:?}");
        assert!(launch.folder.is_dir(), "{network:?}");
        assert_eq!(granted.network, plain.network, "{network:?}");
        assert_eq!(granted.network, network, "{network:?}");
        let profile = std::fs::read_to_string(&granted.sandbox_profile).unwrap();
        assert!(
            profile.contains(launch.folder.to_str().unwrap()),
            "{network:?}: the profile grants the launch folder"
        );
        assert!(
            !std::fs::read_to_string(&plain.sandbox_profile)
                .unwrap()
                .contains(crate::connectors::gateway::LAUNCHES_FOLDER),
            "{network:?}: an agent with no launch is given no launch folder"
        );
        assert_eq!(
            network_lines(&runtime, &granted, 2),
            network_lines(&runtime, &plain, 1),
            "{network:?}: a connector adds no remote"
        );
        assert!(has_entry(2), "{network:?}: each turn carries the entry");
        assert!(!has_entry(1), "{network:?}: another agent's turn does not");

        runtime.end_connector_launch(2);
        assert!(
            runtime.connector_launches.lock().unwrap().is_empty(),
            "{network:?}"
        );
        assert!(
            !launch.folder.exists(),
            "{network:?}: the run's end removes its launch folder"
        );
        assert!(
            !has_entry(2),
            "{network:?}: an ended run's turn has no entry"
        );
    }
}

/// Task 3r: the engine's shutdown ends every connector launch whose run it does not leave
/// running for the next engine, record and folder, as a run's own end does; no launch of
/// a stopped run waits for the leftover bound.
#[test]
fn an_engine_shutdown_ends_the_launch_of_every_run_it_stops() {
    let fixture = Fixture::new(1);
    let mut runtime = connector_runtime(&fixture);
    grant_board(&fixture);
    let project = ProjectId::new(PROJECT).unwrap();
    runtime
        .open_connector_launch(&project, 7, AgentRole::Sub, true)
        .unwrap();
    let folder = runtime.connector_launches.lock().unwrap()[&7]
        .folder
        .clone();
    assert!(folder.is_dir());
    assert_eq!(runtime.shutdown().unwrap(), Vec::<AgentId>::new());
    assert!(runtime.connector_launches.lock().unwrap().is_empty());
    assert!(!folder.exists(), "the stopped run's launch folder is gone");
}
