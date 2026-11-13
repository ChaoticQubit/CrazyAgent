#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::ModelAdded;
use crazyagent::harness_settings::{Access, Source};
use crazyagent::model_choice::rules::{FallbackRule, PickRule};
use crazyagent::model_choice::{ALLOWED_MAX, NOT_SUPPORTED, Origin, Row, Slot, shipped};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::ProjectSpec;
use crazyagent_desktop::bridge::model_choice::ModelsPage;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use serde_json::{Value, json};
use std::error::Error;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const COMMANDS: [&str; 7] = [
    "desktop_model_choice",
    "desktop_set_model_choice",
    "desktop_reset_model_choice",
    "desktop_add_model",
    "desktop_remove_model",
    "desktop_model_rules",
    "desktop_set_model_rule",
];
const HARNESS: &str = "claude-code";

#[test]
fn model_choice_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-model-choice.ts");
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
fn the_window_passes_only_the_harness_project_slot_value_model_rule_and_version() {
    let window = ["webview: Webview,", "state: State<'_, NativeState>,"];
    let page = ["harness: String,", "project_id: Option<String>,"];
    let slot = ["slot: String,"];
    let version = ["version: u64,"];
    let model = ["harness: String,", "model: String,"];
    assert_eq!(
        parameters("desktop_model_choice"),
        [&window[..], &page[..]].concat()
    );
    assert_eq!(
        parameters("desktop_set_model_choice"),
        [
            &window[..],
            &page[..],
            &slot[..],
            &["value: serde_json::Value,"],
            &version[..]
        ]
        .concat()
    );
    assert_eq!(
        parameters("desktop_reset_model_choice"),
        [&window[..], &page[..], &slot[..], &version[..]].concat()
    );
    assert_eq!(
        parameters("desktop_add_model"),
        [&window[..], &model[..], &["provider: Option<String>,"]].concat()
    );
    assert_eq!(
        parameters("desktop_remove_model"),
        [&window[..], &model[..]].concat()
    );
    assert_eq!(parameters("desktop_model_rules"), window);
    assert_eq!(
        parameters("desktop_set_model_rule"),
        [
            &window[..],
            &["rule: String,", "value: String,"],
            &version[..]
        ]
        .concat()
    );
}

/// Runs one flow against a real engine in a private temporary root, then stops the engine.
/// Inputs: the root prefix and the flow. Output: none; a failed flow fails the test.
async fn with_engine<F, T>(prefix: &str, flow: F)
where
    F: FnOnce(std::path::PathBuf) -> T,
    T: Future<Output = Result<(), Box<dyn Error>>>,
{
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    assert!(base.is_absolute());
    let config = engine_support::engine_config(&base);
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(&base, "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(base)).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

fn slot(page: &ModelsPage, slot: Slot) -> &Row {
    let row = page
        .models
        .rows
        .iter()
        .find(|row| row.slot == slot)
        .unwrap();
    assert_eq!(row.slot, slot);
    row
}

async fn read(bridge: &NativeBridge, project: Option<&str>) -> Result<ModelsPage, Box<dyn Error>> {
    Ok(bridge
        .model_choice(HARNESS.into(), project.map(str::to_owned))?
        .await?)
}

async fn app_flow(root: std::path::PathBuf) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let shipped = shipped(HARNESS).unwrap();

    let page = read(&bridge, None).await?;
    assert_eq!(page.project_id, None);
    assert_eq!(page.models.harness, HARNESS);
    assert_eq!(page.qualified_versions, shipped.qualified_versions);
    assert_eq!(page.sources, shipped.sources);
    let slots: Vec<Slot> = page.models.rows.iter().map(|row| row.slot).collect();
    assert_eq!(slots, Slot::ALL);
    let ids: Vec<&str> = page
        .models
        .candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, shipped.models);
    assert!(
        page.models
            .candidates
            .iter()
            .all(|c| c.origin == Origin::Catalog)
    );
    let master = slot(&page, Slot::Master).clone();
    assert_eq!(
        (
            &master.here,
            &master.effective,
            master.source,
            &master.access
        ),
        (&None, &Value::Null, Source::Default, &Access::Editable)
    );

    bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "master".into(),
            json!("opus"),
            master.version,
        )?
        .await?;
    let page = read(&bridge, None).await?;
    let saved = slot(&page, Slot::Master);
    assert_eq!(
        (&saved.here, &saved.effective, saved.source),
        (&Some(json!("opus")), &json!("opus"), Source::App)
    );
    assert!(saved.version > master.version);
    let stale = bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "master".into(),
            json!("sonnet"),
            master.version,
        )?
        .await
        .unwrap_err();
    assert_eq!(stale.code, "model_choice_stale");
    bridge
        .reset_model_choice(HARNESS.into(), None, "master".into(), saved.version)?
        .await?;
    let page = read(&bridge, None).await?;
    assert_eq!(
        slot(&page, Slot::Master).here,
        None,
        "a reset clears the slot"
    );

    let agent = slot(&page, Slot::Agent).clone();
    let unknown = bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "agent".into(),
            json!("not-offered-1"),
            agent.version,
        )?
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "model_choice_value_invalid");
    let allowed = slot(&page, Slot::Allowed).clone();
    assert_eq!(allowed.effective, json!([]));
    bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "allowed".into(),
            json!(["opus", "sonnet"]),
            allowed.version,
        )?
        .await?;
    let page = read(&bridge, None).await?;
    assert_eq!(
        slot(&page, Slot::Allowed).effective,
        json!(["opus", "sonnet"])
    );

    let added = bridge
        .add_model(HARNESS.into(), "my-model-1".into(), None)?
        .await?;
    assert_eq!(added, ModelAdded::Pending);
    let rcli = bridge
        .add_model(
            "rcli".into(),
            "my-model-2".into(),
            Some("openrouter".into()),
        )?
        .await?;
    assert_eq!(rcli, ModelAdded::Pending);
    let page = read(&bridge, None).await?;
    let mine = page
        .models
        .candidates
        .iter()
        .find(|c| c.id == "my-model-1")
        .unwrap();
    assert_eq!(mine.origin, Origin::Pending);
    let pending = bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "agent".into(),
            json!("my-model-1"),
            slot(&page, Slot::Agent).version,
        )?
        .await
        .unwrap_err();
    assert_eq!(
        pending.code, "model_choice_value_invalid",
        "a pending id cannot be chosen before its test launch passes"
    );
    assert_eq!(
        bridge
            .add_model(HARNESS.into(), "opus".into(), None)?
            .await?,
        ModelAdded::Listed
    );
    let invalid = bridge
        .add_model(HARNESS.into(), "bad id!".into(), None)?
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "model_choice_value_invalid");
    assert!(
        bridge
            .remove_model(HARNESS.into(), "my-model-1".into())?
            .await?
    );
    assert!(
        !bridge
            .remove_model(HARNESS.into(), "my-model-1".into())?
            .await?
    );

    let rules = bridge.model_rules()?.await?;
    assert_eq!(
        (rules.pick, rules.fallback),
        (PickRule::Strict, FallbackRule::Notice)
    );
    bridge
        .set_model_rule("pick".into(), "hint".into(), rules.pick_version)?
        .await?;
    let stale = bridge
        .set_model_rule("pick".into(), "default".into(), rules.pick_version)?
        .await
        .unwrap_err();
    assert_eq!(stale.code, "model_choice_stale");
    bridge
        .set_model_rule("fallback".into(), "stop".into(), rules.fallback_version)?
        .await?;
    let after = bridge.model_rules()?.await?;
    assert_eq!(
        (after.pick, after.fallback),
        (PickRule::Hint, FallbackRule::Stop)
    );
    assert!(after.pick_version > rules.pick_version);
    assert!(after.fallback_version > rules.fallback_version);

    let other = bridge.model_choice("opencode".into(), None)?.await?;
    assert!(other.qualified_versions.is_empty() && other.sources.is_empty());
    for row in &other.models.rows {
        assert_eq!(
            (&row.access, row.source),
            (
                &Access::ReadOnly {
                    reason: NOT_SUPPORTED.into()
                },
                Source::NotApplied
            )
        );
    }
    let unsupported = bridge
        .set_model_choice("opencode".into(), None, "master".into(), json!("x1"), 0)?
        .await
        .unwrap_err();
    assert_eq!(unsupported.code, "model_choice_not_supported");

    let too_many: Vec<String> = (0..=ALLOWED_MAX).map(|n| format!("m{n}")).collect();
    let refused = [
        bridge.model_choice("Claude".into(), None).err(),
        bridge
            .model_choice(HARNESS.into(), Some(String::new()))
            .err(),
        bridge
            .set_model_choice(HARNESS.into(), None, "boss".into(), json!("opus"), 0)
            .err(),
        bridge
            .set_model_choice(HARNESS.into(), None, "master".into(), json!(1), 0)
            .err(),
        bridge
            .set_model_choice(HARNESS.into(), None, "master".into(), Value::Null, 0)
            .err(),
        bridge
            .set_model_choice(HARNESS.into(), None, "allowed".into(), json!(too_many), 0)
            .err(),
        bridge
            .set_model_choice(HARNESS.into(), None, "allowed".into(), json!("opus"), 0)
            .err(),
        bridge
            .add_model(HARNESS.into(), "a".repeat(257), None)
            .err(),
        bridge
            .set_model_rule("pick".into(), "sometimes".into(), 0)
            .err(),
        bridge
            .set_model_rule("speed".into(), "hint".into(), 0)
            .err(),
    ];
    assert_eq!(
        refused.map(|error| error.map(|error| error.code)),
        [
            Some("model_choice_request_invalid"),
            Some("model_choice_project_unknown"),
            Some("model_choice_request_invalid"),
            Some("model_choice_value_invalid"),
            Some("model_choice_value_invalid"),
            Some("model_choice_value_invalid"),
            Some("model_choice_value_invalid"),
            Some("model_choice_value_invalid"),
            Some("model_choice_request_invalid"),
            Some("model_choice_request_invalid"),
        ],
        "the window's malformed input is refused before the engine is asked"
    );
    Ok(())
}

/// The app-level Models section reaches the window through the real engine: read, set,
/// stale, reset, an allowed list, an owner id that stays pending, a catalog id already
/// listed, an invalid id, remove, both rules, a harness whose model CrazyAgent does not
/// choose, and the window's malformed input refused before the engine is asked.
#[tokio::test]
async fn the_window_reads_and_changes_app_model_choice_through_the_real_engine() {
    with_engine("ca-desk-mc-", app_flow).await;
}

async fn new_project(bridge: &mut NativeBridge) -> Result<String, Box<dyn Error>> {
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
    Ok(receipt.intent.project_id.clone().unwrap())
}

async fn project_flow(root: std::path::PathBuf) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let id = new_project(&mut bridge).await?;
    let project = Some(id.as_str());

    let page = read(&bridge, project).await?;
    assert_eq!(page.project_id.as_deref(), project);
    let master = slot(&page, Slot::Master).clone();
    assert_eq!((&master.here, master.source), (&None, Source::Default));
    let app = slot(&read(&bridge, None).await?, Slot::Master).version;
    bridge
        .set_model_choice(HARNESS.into(), None, "master".into(), json!("opus"), app)?
        .await?;
    let inherited = slot(&read(&bridge, project).await?, Slot::Master).clone();
    assert_eq!(
        (&inherited.here, &inherited.effective, inherited.source),
        (&None, &json!("opus"), Source::App),
        "a project with nothing set uses the app value"
    );
    bridge
        .set_model_choice(
            HARNESS.into(),
            Some(id.clone()),
            "master".into(),
            json!("sonnet"),
            inherited.version,
        )?
        .await?;
    let own = slot(&read(&bridge, project).await?, Slot::Master).clone();
    assert_eq!(
        (&own.here, &own.effective, own.source),
        (&Some(json!("sonnet")), &json!("sonnet"), Source::Project)
    );
    assert_eq!(
        slot(&read(&bridge, None).await?, Slot::Master).effective,
        json!("opus"),
        "the project value leaves the app value alone"
    );

    let app_list = slot(&read(&bridge, None).await?, Slot::Allowed).version;
    bridge
        .set_model_choice(
            HARNESS.into(),
            None,
            "allowed".into(),
            json!(["opus"]),
            app_list,
        )?
        .await?;
    let list = slot(&read(&bridge, project).await?, Slot::Allowed).clone();
    assert_eq!((&list.here, list.source), (&None, Source::App));
    bridge
        .set_model_choice(
            HARNESS.into(),
            Some(id.clone()),
            "allowed".into(),
            json!([]),
            list.version,
        )?
        .await?;
    let empty = slot(&read(&bridge, project).await?, Slot::Allowed).clone();
    assert_eq!(
        (&empty.here, &empty.effective, empty.source),
        (&Some(json!([])), &json!([]), Source::Project),
        "an own empty list replaces the app list"
    );

    bridge
        .reset_model_choice(
            HARNESS.into(),
            Some(id.clone()),
            "master".into(),
            own.version,
        )?
        .await?;
    bridge
        .reset_model_choice(
            HARNESS.into(),
            Some(id.clone()),
            "allowed".into(),
            empty.version,
        )?
        .await?;
    let cleared = read(&bridge, project).await?;
    assert_eq!(
        (
            slot(&cleared, Slot::Master).source,
            slot(&cleared, Slot::Allowed).source
        ),
        (Source::App, Source::App),
        "clearing the project values goes back to the app values"
    );
    Ok(())
}

/// A project's Models section reaches the window through the real engine: inherit the app
/// value, set its own, keep an own empty allowed list, and clear back to the app values.
#[tokio::test]
async fn the_window_overrides_and_clears_project_model_choice_through_the_real_engine() {
    with_engine("ca-desk-mcp-", project_flow).await;
}
