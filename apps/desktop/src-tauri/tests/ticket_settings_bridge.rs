#![cfg(unix)]

mod engine_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent_desktop::bridge::ticket_settings::{TICKET_KEYS, TicketSettingsPage};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use serde_json::{Value, json};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 2] = ["desktop_ticket_settings", "desktop_save_ticket_settings"];

#[test]
fn ticket_settings_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-ticket-settings.ts");
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
            window.matches(&format!("\"{command}\"")).count(),
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

#[test]
fn the_window_passes_only_the_scope_the_values_and_the_version() {
    let window = ["window: WebviewWindow,", "state: State<'_, NativeState>,"];
    assert_eq!(
        parameters("desktop_ticket_settings"),
        [&window[..], &["project_id: Option<String>,"]].concat()
    );
    assert_eq!(
        parameters("desktop_save_ticket_settings"),
        [
            &window[..],
            &[
                "project_id: Option<String>,",
                "values: serde_json::Value,",
                "version: u64,"
            ]
        ]
        .concat()
    );
}

fn in_force<'a>(page: &'a TicketSettingsPage, key: &str) -> &'a Value {
    let row = page.keys.iter().find(|row| row.key == key).unwrap();
    &row.effective
}

fn here<'a>(page: &'a TicketSettingsPage, key: &str) -> &'a Value {
    &page.keys.iter().find(|row| row.key == key).unwrap().here
}

fn every_proposed_default_in_force(page: &TicketSettingsPage) {
    let proposed = [
        ("close", json!("evidence")),
        ("create", json!("triage")),
        ("agent_edit", json!("status_and_criteria")),
        ("cycle_days", json!(14)),
        ("rollover", json!("auto")),
        ("import", json!("auto_once")),
        ("manual_evidence", json!("owner")),
        ("ci_evidence", json!("engine")),
    ];
    let keys: Vec<&str> = page.keys.iter().map(|row| row.key).collect();
    assert_eq!(keys, TICKET_KEYS);
    for (key, value) in proposed {
        let shown = page.keys.iter().find(|row| row.key == key).unwrap();
        assert_eq!(shown.proposed, value, "{key}");
        assert!(shown.options.contains(&value), "{key}");
        assert_eq!(shown.below, value, "{key}");
        assert_eq!(shown.effective, value, "{key}");
        assert_eq!(shown.here, Value::Null, "{key}");
        assert_eq!(shown.custom.is_some(), key == "cycle_days", "{key}");
    }
    let days = page
        .keys
        .iter()
        .find(|row| row.key == "cycle_days")
        .unwrap();
    assert_eq!(days.options, [json!(7), json!(14), json!(21), json!(28)]);
    assert_eq!(
        days.custom.map(|custom| (custom.min, custom.max)),
        Some((1, 90))
    );
}

async fn project(bridge: &mut NativeBridge) -> Result<String, Box<dyn Error>> {
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
    Ok(receipt.intent.project_id.unwrap())
}

async fn app_flow(bridge: &NativeBridge) -> Result<u64, Box<dyn Error>> {
    let first = bridge.ticket_settings(None)?.await?;
    assert_eq!(first.project_id, None);
    every_proposed_default_in_force(&first);
    for (values, code) in [
        (json!({"sandbox": "off"}), "tracker_request_invalid"),
        (json!(["close"]), "tracker_request_invalid"),
        (json!({"close": "sometimes"}), "policy_value_invalid"),
        (json!({"cycle_days": 91}), "policy_value_invalid"),
    ] {
        let refused = bridge.save_ticket_settings(None, values, first.version);
        assert_eq!(refused.err().map(|error| error.code), Some(code));
    }
    assert_eq!(bridge.ticket_settings(None)?.await?, first);

    let values = json!({"close": "owner", "cycle_days": 10});
    bridge
        .save_ticket_settings(None, values, first.version)?
        .await?;
    let saved = bridge.ticket_settings(None)?.await?;
    assert!(saved.version > first.version);
    assert_eq!(here(&saved, "close"), &json!("owner"));
    assert_eq!(in_force(&saved, "close"), &json!("owner"));
    assert_eq!(here(&saved, "cycle_days"), &json!(10));
    assert_eq!(in_force(&saved, "cycle_days"), &json!(10));
    assert_eq!(here(&saved, "rollover"), &Value::Null);

    let stale = bridge
        .save_ticket_settings(None, json!({"close": "agents"}), first.version)?
        .await;
    assert_eq!(
        stale.err().map(|error| error.code),
        Some("tracker_settings_stale")
    );
    assert_eq!(bridge.ticket_settings(None)?.await?, saved);
    Ok(saved.version)
}

async fn project_flow(root: &Path, bridge: &NativeBridge, id: &str) -> Result<(), Box<dyn Error>> {
    let unattached = bridge.ticket_settings(Some(id.into()))?.await;
    assert_eq!(
        unattached.err().map(|error| error.code),
        Some("project_not_attached")
    );
    let observer = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .map_err(|error| error.to_string())?;
    let client = observer.shared_client().map_err(|error| error.0)?;
    client
        .tracker_attach(
            ProjectId::new(id)?,
            serde_json::from_value(json!("RB"))?,
            "Recipe box".into(),
        )
        .await
        .map_err(|error| error.0)?;

    let first = bridge.ticket_settings(Some(id.into()))?.await?;
    assert_eq!(first.project_id.as_deref(), Some(id));
    for key in TICKET_KEYS {
        assert_eq!(here(&first, key), &Value::Null, "{key}");
    }
    let close = first.keys.iter().find(|row| row.key == "close").unwrap();
    assert_eq!(
        (&close.below, &close.effective),
        (&json!("owner"), &json!("owner"))
    );
    assert_eq!(in_force(&first, "cycle_days"), &json!(10));

    bridge
        .save_ticket_settings(Some(id.into()), json!({"close": "agents"}), first.version)?
        .await?;
    let set = bridge.ticket_settings(Some(id.into()))?.await?;
    assert!(set.version > first.version);
    let close = set.keys.iter().find(|row| row.key == "close").unwrap();
    assert_eq!(close.here, json!("agents"));
    assert_eq!(close.below, json!("owner"));
    assert_eq!(close.effective, json!("agents"));
    assert_eq!(
        in_force(&bridge.ticket_settings(None)?.await?, "close"),
        &json!("owner")
    );

    bridge
        .save_ticket_settings(Some(id.into()), json!({}), set.version)?
        .await?;
    let reset = bridge.ticket_settings(Some(id.into()))?.await?;
    assert!(reset.version > set.version);
    for key in TICKET_KEYS {
        assert_eq!(here(&reset, key), &Value::Null, "{key}");
    }
    assert_eq!(in_force(&reset, "close"), &json!("owner"));
    Ok(())
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert_eq!(
        bridge.ticket_settings(None).err().map(|error| error.code),
        Some("not_connected")
    );
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let id = project(&mut bridge).await?;
    assert_eq!(
        bridge
            .ticket_settings(Some(String::new()))
            .err()
            .map(|error| error.code),
        Some("tracker_request_invalid")
    );
    app_flow(&bridge).await?;
    project_flow(root, &bridge, &id).await
}

#[tokio::test]
async fn desktop_reads_saves_and_resets_ticket_settings_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-ts-")
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
