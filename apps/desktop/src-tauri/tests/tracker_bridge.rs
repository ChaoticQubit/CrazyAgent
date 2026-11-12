#![cfg(unix)]

mod engine_support;

use crazyagent::engine::EngineError;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{IDE_CODES, TRACKER_CODES};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::ProjectSpec;
use crazyagent_desktop::bridge::tracker::{operation, reveal_error, tracker_error};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use serde_json::json;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 4] = [
    "desktop_tracker_op",
    "desktop_tracker_projects",
    "desktop_tracker_attach",
    "desktop_tracker_open_change_set",
];

const WINDOW: &str = include_str!("../../src/api-tracker.ts");
const SCHEMA: &str = include_str!("../../../../crates/trackwright-core/schema/v1.json");

#[test]
fn tracker_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("async fn {command}(")),
            "{command} binding"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} handler"
        );
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&permission).count(),
            1,
            "{command} capability"
        );
        assert_eq!(
            WINDOW.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} window"
        );
    }
}

/// The parameters of one command binding in `lib.rs`, one per line as rustfmt writes them.
fn parameters(command: &str) -> Vec<String> {
    let bindings = include_str!("../src/lib.rs");
    let start = bindings
        .find(&format!("async fn {command}("))
        .unwrap_or_else(|| panic!("{command} binding"));
    let rest = &bindings[start..];
    let signature = &rest[..rest.find(") -> ").expect("a return type")];
    signature
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The window names an operation and its input, a project and its key and name, or a
/// change set; never an actor, a version of its own making or any other field. The owner is
/// the actor the engine records.
#[test]
fn the_window_passes_only_the_protocol_fields() {
    let window = ["window: WebviewWindow,", "state: State<'_, NativeState>,"];
    assert_eq!(
        parameters("desktop_tracker_op"),
        [&window[..], &["op: String,", "args: serde_json::Value,"]].concat()
    );
    assert_eq!(parameters("desktop_tracker_projects"), window);
    assert_eq!(
        parameters("desktop_tracker_attach"),
        [
            &window[..],
            &["project_id: String,", "key: String,", "name: String,"]
        ]
        .concat()
    );
    assert_eq!(
        parameters("desktop_tracker_open_change_set"),
        [&window[..], &["project_id: String,", "change_set: String,"]].concat()
    );
}

/// Every operation schema v1 names reaches the engine with its input object unchanged;
/// any other name, or an input that is not one object, is refused before the engine is
/// asked.
#[test]
fn only_schema_v1_operations_with_one_input_object_pass() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let ops = schema["ops"].as_object().unwrap();
    assert_eq!(ops.len(), 30);
    for name in ops.keys() {
        let args = json!({"id": "RB-1", "cursor": "events:50"});
        let (kind, passed) = operation(name, args.clone()).unwrap();
        assert_eq!(kind.as_str(), name);
        assert_eq!(passed, args, "{name} input passes unchanged");
    }
    for name in [
        "",
        "drop",
        "Show",
        "show ",
        "schema\n",
        "tracker",
        "list;show",
    ] {
        assert_eq!(
            operation(name, json!({})).unwrap_err().code,
            "unknown_op",
            "{name:?}"
        );
    }
    for args in [json!(null), json!([]), json!("RB-1"), json!(3), json!(true)] {
        assert_eq!(
            operation("show", args.clone()).unwrap_err().code,
            "args_invalid",
            "{args}"
        );
    }
    for code in ["unknown_op", "args_invalid", "project_key_invalid"] {
        assert!(TRACKER_CODES.contains(&code), "{code}");
    }
    assert!(IDE_CODES.contains(&"changes_not_found"));
}

/// Every code a tracker or an Open in IDE request can answer reaches the window as itself,
/// and the window has a sentence for each; anything else is an unknown outcome with the
/// engine code behind it, and raw text never reaches the window.
#[test]
fn every_tracker_and_reveal_code_maps_to_a_sentence() {
    assert!(!TRACKER_CODES.is_empty() && !IDE_CODES.is_empty());
    for code in TRACKER_CODES.iter().chain(IDE_CODES) {
        assert!(
            WINDOW.contains(&format!("\n  {code}: \"")),
            "{code} has no sentence in api-tracker.ts"
        );
    }
    for code in TRACKER_CODES {
        assert_eq!(tracker_error(EngineError((*code).into())).code, *code);
    }
    for code in IDE_CODES {
        assert_eq!(reveal_error(EngineError((*code).into())).code, *code);
    }
    for error in [tracker_error, reveal_error] {
        assert_eq!(
            error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let raw = error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }
    let unlisted = tracker_error(EngineError("tracker_store_torn".into()));
    assert_eq!(unlisted.code, "tracker_outcome_unknown");
    assert_eq!(unlisted.cause.as_deref(), Some("tracker_store_torn"));
    let unlisted = reveal_error(EngineError("ide_store_torn".into()));
    assert_eq!(unlisted.code, "ide_outcome_unknown");
    assert_eq!(unlisted.cause.as_deref(), Some("ide_store_torn"));
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let receipt = bridge
        .setup_project(
            ProjectSpec {
                name: "Recipe box".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Keeps family recipes.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Website],
                research: vec![ResearchKind::MarketResearch],
                pool: None,
                team: None,
                documents: vec![],
            },
            2,
        )
        .await?;
    assert!(
        matches!(receipt.outcome, ActionOutcome::Applied),
        "{receipt:?}"
    );
    let id = receipt.intent.project_id.clone().unwrap();
    assert!(
        bridge
            .tracker_projects()?
            .await?
            .iter()
            .all(|tracked| tracked.project.as_str() != id)
    );

    assert_eq!(
        bridge
            .tracker_attach(id.clone(), "rb".into(), "Recipe box".into())
            .err()
            .map(|error| error.code),
        Some("project_key_invalid")
    );
    let tracked = bridge
        .tracker_attach(id.clone(), "RB".into(), "Recipe box".into())?
        .await?;
    assert_eq!(
        (
            tracked.project.as_str(),
            tracked.key.as_str(),
            tracked.name.as_str()
        ),
        (id.as_str(), "RB", "Recipe box")
    );
    assert!(bridge.tracker_projects()?.await?.contains(&tracked));
    assert_eq!(
        bridge
            .tracker_attach(id.clone(), "RC".into(), "Recipe box".into())?
            .await
            .unwrap_err()
            .code,
        "project_exists"
    );

    let created = bridge
        .tracker_op(
            "create".into(),
            json!({
                "project": "RB",
                "title": "Import recipes from a file",
                "status": "triage",
                "request_id": "desk-create-1",
            }),
        )?
        .await?;
    assert_eq!(created["api"], 1, "{created}");
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["data"]["id"], "RB-1");
    assert_eq!(created["data"]["status"], "triage");
    assert_eq!(
        created["data"]["created_by"], "owner",
        "the owner is the actor"
    );

    let listed = bridge
        .tracker_op("list".into(), json!({"project": "RB", "status": "triage"}))?
        .await?;
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["data"]["tickets"][0]["id"], "RB-1");
    assert_eq!(listed["data"]["tickets"][0]["status"], "triage");
    assert_eq!(listed["cursor"], serde_json::Value::Null);

    let shown = bridge
        .tracker_op("show".into(), json!({"id": "RB-1"}))?
        .await?;
    assert_eq!(shown["ok"], true, "{shown}");
    let version = shown["data"]["ticket"]["version"].as_u64().unwrap();
    assert!(
        shown["data"]["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );

    let moved = bridge
        .tracker_op(
            "move".into(),
            json!({"id": "RB-1", "version": version, "status": "todo", "request_id": "desk-move-1"}),
        )?
        .await?;
    assert_eq!(moved["ok"], true, "{moved}");
    assert_eq!(moved["data"]["status"], "todo");
    assert!(moved["data"]["version"].as_u64().unwrap() > version);

    let stale = bridge
        .tracker_op(
            "move".into(),
            json!({"id": "RB-1", "version": version, "status": "backlog", "request_id": "desk-move-2"}),
        )?
        .await?;
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["code"], "stale_version");
    assert_eq!(stale["next"][0]["op"], "show");

    let posing = bridge
        .tracker_op(
            "show".into(),
            json!({"id": "RB-1", "actor": "agent:team-7"}),
        )?
        .await?;
    assert_eq!(posing["ok"], false, "{posing}");
    assert_eq!(
        posing["code"], "unknown_field",
        "the window cannot name an actor"
    );

    let history = bridge
        .tracker_op("history".into(), json!({"id": "RB-1"}))?
        .await?;
    let ops: Vec<&str> = history["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["create", "move"], "history pages run oldest first");

    assert_eq!(
        bridge
            .tracker_op("drop".into(), json!({}))
            .err()
            .map(|error| error.code),
        Some("unknown_op")
    );
    assert_eq!(
        bridge
            .tracker_open_change_set(id.clone(), "../worktree".into())
            .err()
            .map(|error| error.code),
        Some("changes_not_found")
    );
    Ok(())
}

#[tokio::test]
async fn desktop_attaches_lists_shows_and_moves_tickets_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-t-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
