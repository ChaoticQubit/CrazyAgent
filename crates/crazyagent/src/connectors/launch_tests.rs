use super::*;
use crate::app_plugins::store;
use crate::connectors::gateway;
use crate::connectors::grants::{self, Role, Who};
use crate::connectors::mcp_client::tests::{BOARD, KEY};
use crate::connectors::settings::{self, Wait};
use crate::connectors::tests::{Case, add, case};
use crate::launch_context::{AgentContext, Installed};
use crate::plugins::LaunchPlugins;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

const ENGINE: &str = "/usr/local/bin/crazyagent";
const PROJECT: &str = "alpha";
const OTHER: &str = "beta";
const AGENT: &str = "Agent 1";
const AGENT_ROLE: Role = Role::Agent { tools_on: true };

/// A real engine root with `board` trusted, its key saved and granted to `alpha` only.
fn granted() -> Case {
    let case = case();
    add(&case, BOARD, false);
    store::set_key(
        &case.db,
        &case.root,
        "board",
        "api_key",
        KEY.as_bytes().to_vec(),
    )
    .unwrap();
    grants::set(&case.db, &case.root, PROJECT, "board", true, Who::Both, 2).unwrap();
    case
}

/// A fresh private folder that holds agent homes, kept alive by the returned guard.
fn homes() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let base = folder.path().canonicalize().unwrap();
    (folder, base)
}

/// A new 0700 agent home named `name` in `base`.
fn home(base: &Path, name: &str) -> PathBuf {
    let home = base.join(name);
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    home
}

/// The Claude Code MCP config a launch wrote, found after its `--mcp-config` argument.
fn claude_mcp(installed: &Installed) -> Value {
    let arguments = installed.arguments();
    let at = arguments
        .iter()
        .position(|argument| argument == "--mcp-config")
        .expect("a Claude Code launch names its MCP config");
    serde_json::from_str(&fs::read_to_string(&arguments[at + 1]).unwrap()).unwrap()
}

/// Every plain file under `folder`, never following a link.
fn files_under(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        let kind = fs::symlink_metadata(&path).unwrap().file_type();
        if kind.is_dir() {
            files_under(&path, found);
        } else if kind.is_file() {
            found.push(path);
        }
    }
}

#[test]
fn a_granted_project_launch_has_the_one_gateway_entry_and_an_ungranted_one_has_none() {
    let case = granted();
    assert_eq!(
        gateway::prepare(&case.db, &case.root, OTHER, AGENT_ROLE, AGENT).unwrap(),
        None,
        "a project with no grant gets no launch, so no entry"
    );
    assert_eq!(
        gateway::prepare(
            &case.db,
            &case.root,
            PROJECT,
            Role::Agent { tools_on: false },
            AGENT
        )
        .unwrap(),
        None,
        "an agent with tools off gets no launch"
    );
    let launch = gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .expect("a granted project gets a launch");
    let folder = launch.folder.to_str().unwrap().to_owned();
    let server = entry(Path::new(ENGINE), &launch).unwrap();
    assert_eq!(server.name, SERVER_NAME);
    assert!(server.secrets.is_empty());
    let context = AgentContext::for_tests(LaunchPlugins::default())
        .with_connectors(server)
        .unwrap();
    let (_kept, base) = homes();

    let codex = home(&base, "codex");
    let installed = context.install("codex", &codex, true).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let servers = config
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    assert_eq!(servers.keys().collect::<Vec<_>>(), [SERVER_NAME]);
    let server = servers[SERVER_NAME].as_table().unwrap();
    assert_eq!(server["command"].as_str(), Some(ENGINE));
    assert_eq!(
        server["args"],
        toml::Value::Array(vec!["connector-mcp".into(), folder.clone().into()])
    );
    assert!(!server.contains_key("env"));
    drop(installed);

    let claude = home(&base, "claude");
    let installed = context.install("claude-code", &claude, true).unwrap();
    let mcp = claude_mcp(&installed);
    let servers = mcp["mcpServers"].as_object().unwrap();
    assert_eq!(servers.keys().collect::<Vec<_>>(), [SERVER_NAME]);
    let timeout = u64::try_from(HARNESS_WAIT.as_millis()).unwrap();
    assert_eq!(
        servers[SERVER_NAME],
        json!({
            "type": "stdio",
            "command": ENGINE,
            "args": ["connector-mcp", folder],
            "env": {},
            "timeout": timeout,
        })
    );
    drop(installed);

    assert_eq!(
        entry(Path::new("crazyagent"), &launch).unwrap_err().0,
        "connector_request_invalid"
    );
    let taken = AgentContext::for_tests(LaunchPlugins::default())
        .with_connectors(entry(Path::new(ENGINE), &launch).unwrap())
        .unwrap()
        .with_connectors(entry(Path::new(ENGINE), &launch).unwrap());
    assert_eq!(
        taken.unwrap_err().0,
        "launch_context_connectors_taken",
        "a second entry under the same name is refused"
    );
}

/// DEC-168 addendum 14 Q2: the connector entry tells each harness to wait past the longest
/// approval, Codex by `tool_timeout_sec` and `startup_timeout_sec` and Claude Code by its
/// per-server `timeout` in milliseconds, so a held write is answered before either gives up.
#[test]
fn the_connector_entry_waits_past_the_longest_approval_in_both_harnesses() {
    let case = granted();
    let launch = gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .unwrap();
    let context = AgentContext::for_tests(LaunchPlugins::default())
        .with_connectors(entry(Path::new(ENGINE), &launch).unwrap())
        .unwrap();
    let (_kept, base) = homes();
    let longest = Wait::ALL.iter().map(|wait| wait.minutes()).max().unwrap();
    assert!(HARNESS_WAIT.as_secs() > u64::from(longest) * 60);

    let codex = home(&base, "codex");
    let installed = context.install("codex", &codex, true).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let server = config["mcp_servers"][SERVER_NAME].as_table().unwrap();
    let seconds = |key: &str| server.get(key).and_then(toml::Value::as_integer);
    let tool = i64::try_from(HARNESS_WAIT.as_secs()).unwrap();
    assert_eq!(seconds("tool_timeout_sec"), Some(tool));
    let startup = i64::try_from(STARTUP_WAIT.as_secs()).unwrap();
    assert_eq!(seconds("startup_timeout_sec"), Some(startup));
    drop(installed);

    let claude = home(&base, "claude");
    let installed = context.install("claude-code", &claude, true).unwrap();
    let mcp = claude_mcp(&installed);
    let timeout = u64::try_from(HARNESS_WAIT.as_millis()).unwrap();
    assert_eq!(mcp["mcpServers"][SERVER_NAME]["timeout"], json!(timeout));
    drop(installed);
}

#[test]
fn no_connector_secret_is_in_the_agent_config_argv_or_env() {
    let case = granted();
    let launch = gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .unwrap();
    let context = AgentContext::for_tests(LaunchPlugins::default())
        .with_connectors(entry(Path::new(ENGINE), &launch).unwrap())
        .unwrap();
    assert!(
        context
            .plugins()
            .servers
            .iter()
            .all(|server| server.secrets.is_empty())
    );
    let (_kept, base) = homes();
    for harness in ["codex", "claude-code"] {
        let home = home(&base, harness);
        let installed = context.install(harness, &home, true).unwrap();
        let mut files = Vec::new();
        files_under(&home, &mut files);
        files_under(&launch.folder, &mut files);
        assert!(!files.is_empty());
        for file in &files {
            let text = String::from_utf8_lossy(&fs::read(file).unwrap()).into_owned();
            assert!(!text.contains(KEY), "{} holds the key", file.display());
            for header in ["Authorization", "http_headers", "bearer_token"] {
                assert!(!text.contains(header), "{} names {header}", file.display());
            }
        }
        assert!(
            files
                .iter()
                .any(|file| fs::read_to_string(file)
                    .is_ok_and(|text| text.contains("connector-mcp"))),
            "the entry was written, so the scan covers it"
        );
        for argument in installed.arguments() {
            assert!(!argument.to_string_lossy().contains(KEY));
        }
        drop(installed);
    }
}

#[test]
fn native_connectors_off_adds_the_switch_for_claude_code_and_codex() {
    use crate::connectors::settings::NativeConnectors::{Off, On};
    assert_eq!(
        native_switch("claude-code", Off),
        NativeSwitch {
            claude_ai_off: true,
            codex_apps_off: false
        }
    );
    assert_eq!(
        native_switch("codex", Off),
        NativeSwitch {
            claude_ai_off: false,
            codex_apps_off: true
        }
    );
    assert_eq!(native_switch("rcli", Off), NativeSwitch::default());
    for harness in ["claude-code", "codex", "rcli"] {
        assert_eq!(native_switch(harness, On), NativeSwitch::default());
    }

    let (_kept, base) = homes();
    for (at, native) in [Some(Off), Some(On), None].into_iter().enumerate() {
        let context = AgentContext::for_tests(LaunchPlugins {
            native_connectors: native,
            ..LaunchPlugins::default()
        });
        for tools in [true, false] {
            let off = native == Some(Off) || (native == Some(On) && !tools);
            let codex = home(&base, &format!("codex-{at}-{tools}"));
            let installed = context.install("codex", &codex, tools).unwrap();
            let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
                .unwrap()
                .parse()
                .unwrap();
            let apps = config
                .get("features")
                .and_then(|features| features.get("apps"))
                .and_then(toml::Value::as_bool);
            assert_eq!(apps, off.then_some(false), "{native:?} tools {tools}");
            drop(installed);

            let claude = home(&base, &format!("claude-{at}-{tools}"));
            let installed = context.install("claude-code", &claude, tools).unwrap();
            assert_eq!(installed.native_connectors_off(), off);
            let settings: Value = serde_json::from_str(&crate::harnesses::launch::claude_settings(
                None,
                Some(&installed),
            ))
            .unwrap();
            assert_eq!(
                settings.get("disableClaudeAiConnectors"),
                off.then_some(&Value::Bool(true)),
                "{native:?} tools {tools}"
            );
            drop(installed);
        }
    }

    let case = case();
    let project = crate::protocol::ProjectId::new(PROJECT).unwrap();
    let native = |db: &rusqlite::Connection| {
        let tx = db.unchecked_transaction().unwrap();
        crate::storage::plugins::launch_set(&tx, &case.root, &project)
            .unwrap()
            .native_connectors
    };
    assert_eq!(native(&case.db), Some(Off), "the default setting is Off");
    let (mut stored, version) = settings::load(&case.db).unwrap();
    stored.native = On;
    settings::save(&case.db, &stored, version).unwrap().unwrap();
    assert_eq!(
        native(&case.db),
        Some(On),
        "a launch reads the owner's setting"
    );
}

#[test]
fn an_rcli_agent_install_gets_no_connector_entry_and_no_native_switch() {
    let case = granted();
    let launch = gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .unwrap();
    let context = AgentContext::for_tests(LaunchPlugins::default())
        .with_connectors(entry(Path::new(ENGINE), &launch).unwrap())
        .unwrap();
    assert!(
        context
            .plugins()
            .servers
            .iter()
            .any(|server| server.name == SERVER_NAME)
    );
    let (_kept, base) = homes();
    let rcli = home(&base, "rcli");
    assert_eq!(
        context.install("rcli", &rcli, true).unwrap_err().0,
        "launch_harness_unsupported",
        "no rcli launch is written with the entry"
    );
    let mut files = Vec::new();
    files_under(&rcli, &mut files);
    assert!(files.is_empty());
    assert_eq!(
        native_switch("rcli", crate::connectors::settings::NativeConnectors::Off),
        NativeSwitch::default()
    );
}

#[test]
fn a_cleared_key_or_a_grant_turned_off_is_gone_at_the_next_launch() {
    let case = granted();
    let first = gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .unwrap();
    assert_eq!(first.snapshot.connectors, ["board"]);
    let server = entry(Path::new(ENGINE), &first).unwrap();
    assert!(
        matches!(&server.spec, ServerSpec::Stdio { args, .. }
            if args.last().map(String::as_str) == first.folder.to_str()),
        "the entry names the launch folder"
    );
    grants::set(&case.db, &case.root, PROJECT, "board", false, Who::Both, 3).unwrap();
    assert_eq!(
        gateway::prepare(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT).unwrap(),
        None,
        "a grant turned off leaves the next launch with no entry"
    );
    assert!(
        first.folder.is_dir(),
        "a running launch keeps its folder until its run ends"
    );
}

/// With no gateway serving the engine root, a granted agent or master launch gets no
/// connector launch and no folder, so its harness is handed no connector server that nobody
/// answers; once a gateway serves the root the same launch gets one.
#[test]
fn a_launch_in_an_engine_with_no_gateway_gets_no_connector_and_a_served_one_does() {
    let case = granted();
    assert!(!gateway::serving(&case.root));
    for (role, agent) in [(AGENT_ROLE, AGENT), (Role::Master, "Master")] {
        assert_eq!(
            gateway::prepare_if_served(&case.db, &case.root, PROJECT, role, agent).unwrap(),
            None,
            "{agent} gets no connector while no gateway serves its engine"
        );
    }
    assert!(
        !case.root.join(gateway::LAUNCHES_FOLDER).exists(),
        "and no launch folder is made"
    );
    assert_eq!(
        gateway::unserved_line(AGENT_ROLE),
        "connectors are not available to this agent run: this engine runs no connector gateway"
    );
    assert_eq!(
        gateway::unserved_line(Role::Master),
        "connectors are not available to this master turn: this engine runs no connector gateway"
    );
    let running = gateway::start(&case.root).unwrap();
    let launch = gateway::prepare_if_served(&case.db, &case.root, PROJECT, AGENT_ROLE, AGENT)
        .unwrap()
        .expect("a served engine readies the granted launch");
    assert!(launch.folder.is_dir());
    let name = launch.folder.file_name().unwrap().to_str().unwrap();
    gateway::end(&case.db, &case.root, name).unwrap();
    running.stop();
}

/// Both engine paths that ready a launch, a team agent's run and a master turn, go through
/// the served gate, so neither hands a harness a connector server while no gateway runs.
#[test]
fn every_engine_launch_readies_its_connectors_through_the_served_gate() {
    for (path, source) in [
        ("team/runtime.rs", include_str!("../team/runtime.rs")),
        (
            "engine/owner/transport/master_host_launch.rs",
            include_str!("../engine/owner/transport/master_host_launch.rs"),
        ),
    ] {
        assert!(
            source.contains("connectors::gateway::prepare_if_served("),
            "{path} readies its launch through the served gate"
        );
        assert!(
            !source.contains("connectors::gateway::prepare("),
            "{path} never readies a launch around the gate"
        );
    }
}

/// The engine starts its gateway before its team, whose first tick can launch a run: a run
/// launched before the gateway served the root would go without connectors for its whole
/// life under the served gate.
#[test]
fn the_engine_starts_its_gateway_before_its_team() {
    let boot = include_str!("../bin/crazyagent.rs");
    let gateway = boot
        .find("connectors::gateway::start(")
        .expect("the engine starts a gateway");
    let team = boot
        .find("team::runtime::start_for_engine(")
        .expect("the engine starts a team");
    assert!(gateway < team, "the gateway serves before the team ticks");
    assert_eq!(
        boot.matches("connectors::gateway::start(").count(),
        1,
        "one gateway per engine"
    );
}
