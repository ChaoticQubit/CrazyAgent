use super::*;
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::sdk_slice::{EngineRefusals, bundled_grants};
#[cfg(target_os = "macos")]
use bus_gateway::Grants;
use bus_gateway::{AllOn, Gateway};
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
#[cfg(target_os = "macos")]
use sdk::Tier;
use sdk::generated::editor::Provide;
use sdk_host::{Bound, Providers};

const KNOWN: &str = "editor-known";
#[cfg(target_os = "macos")]
const PID: u32 = 4242;

/// An engine database holding one project the owner set up. Output: the connection.
fn projects() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let project = ProjectId::new(KNOWN).unwrap();
    let setup = StoredSetup {
        project_id: project.clone(),
        name: "Editor".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: "/tmp/editor-known".into(),
        created_ms: 1,
        revision: 1,
    };
    connection
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            [
                project.as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            ],
        )
        .unwrap();
    connection
}

fn never_run(_: &ProjectId) -> Result<u32, &'static str> {
    panic!("the spawner ran")
}

#[cfg(target_os = "macos")]
fn started(project: &ProjectId) -> Result<u32, &'static str> {
    assert_eq!(project.as_str(), KNOWN);
    Ok(PID)
}

fn launch(project: &str) -> EditorLaunch {
    EditorLaunch {
        project: project.to_owned(),
    }
}

fn rejected(code: &str) -> Result<u32, Refusal> {
    Err(Refusal::Rejected {
        code: code.to_owned(),
    })
}

fn failed(code: &str) -> Result<u32, Refusal> {
    Err(Refusal::Failed {
        code: code.to_owned(),
    })
}

#[test]
fn a_malformed_or_over_long_project_id_is_rejected_before_any_lookup() {
    let projects = projects();
    let provider = EditorCaps::with(&projects, never_run);
    for project in ["", "../editor-known", "a/b", &"p".repeat(4096)] {
        assert_eq!(
            provider.spawn("editor-test", launch(project)),
            rejected(PROJECT_INVALID),
            "{project:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_project_the_engine_does_not_know_is_rejected() {
    let projects = projects();
    let provider = EditorCaps::with(&projects, never_run);
    assert_eq!(
        provider.spawn("editor-test", launch("editor-unknown")),
        rejected(PROJECT_UNKNOWN)
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_every_known_project_is_refused_as_the_ide_server_refuses() {
    let projects = projects();
    let provider = EditorCaps::with(&projects, never_run);
    assert_eq!(
        provider.spawn("editor-test", launch(KNOWN)),
        failed(PLATFORM_UNQUALIFIED)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_registered_provider_has_no_spawner_and_refuses_every_call() {
    let projects = projects();
    let provider = EditorCaps::unregistered(&projects);
    assert_eq!(
        provider.spawn("editor-test", launch(KNOWN)),
        failed(SPAWNER_MISSING)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_known_project_returns_only_the_spawners_pid() {
    let projects = projects();
    let provider = EditorCaps::with(&projects, started);
    let pid: Result<u32, Refusal> = provider.spawn("editor-test", launch(KNOWN));
    assert_eq!(pid, Ok(PID));
}

#[test]
fn no_bundled_plugin_holds_editor_spawn() {
    assert!(
        BUNDLED_GRANTS
            .iter()
            .all(|grant| !grant.permissions.contains(&editor::SPAWN.needs))
    );
}

#[test]
fn agent_quit_calling_editor_spawn_is_refused_and_logs_one_call_refused() {
    let projects = projects();
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(bundled_grants(), &refusals, &AllOn);
    let provider = EditorCaps::with(&projects, never_run);
    let providers = Providers {
        editor: &provider,
        net: &crate::security_caps::Absent,
        process: &crate::security_caps::Absent,
        project_rows: &crate::security_caps::Absent,
        sandbox: &crate::security_caps::Absent,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let plugin = BUNDLED_GRANTS[0].plugin;
    let host = Bound::new(gateway.bind(plugin), &providers);
    assert_eq!(
        editor::spawn(&host, launch(KNOWN)),
        Err(Refusal::NotGranted {
            capability: editor::SPAWN.name,
            needs: editor::SPAWN.needs,
        })
    );
    assert_eq!(refusals.count(plugin), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn a_test_grant_table_reaches_the_editor_provider() {
    let projects = projects();
    let grants = Grants::new(&[("editor-test", Tier::T1, &["editor.spawn"])]).unwrap();
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let provider = EditorCaps::with(&projects, started);
    let providers = Providers {
        editor: &provider,
        net: &crate::security_caps::Absent,
        process: &crate::security_caps::Absent,
        project_rows: &crate::security_caps::Absent,
        sandbox: &crate::security_caps::Absent,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let host = Bound::new(gateway.bind("editor-test"), &providers);
    assert_eq!(editor::spawn(&host, launch(KNOWN)), Ok(PID));
    assert_eq!(refusals.count("editor-test"), 0);
}
