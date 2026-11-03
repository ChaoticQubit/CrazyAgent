#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::harness_settings::{APP_ONLY, Access, Scope, catalog, catalog_page, store};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::storage::versioned;
use crazyagent_desktop::bridge::harness_settings::{
    HarnessSettingsPage, SettingRow, page_of, written,
};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use serde_json::{Value, json};
use std::error::Error;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FIXTURE: &[u8] =
    include_bytes!("../../../../crates/crazyagent/tests/fixtures/harness-settings/codex.json");
const QUALIFIED: Option<&str> = Some("0.51.0");

const COMMANDS: [&str; 3] = [
    "desktop_harness_settings",
    "desktop_set_harness_setting",
    "desktop_reset_harness_setting",
];

#[test]
fn harness_settings_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-harness-settings.ts");
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
fn the_window_passes_only_the_harness_project_key_value_and_version() {
    let window = ["window: WebviewWindow,", "state: State<'_, NativeState>,"];
    let page = ["harness: String,", "project_id: Option<String>,"];
    assert_eq!(
        parameters("desktop_harness_settings"),
        [&window[..], &page[..]].concat()
    );
    assert_eq!(
        parameters("desktop_set_harness_setting"),
        [
            &window[..],
            &page[..],
            &["key: String,", "value: serde_json::Value,", "version: u64,"]
        ]
        .concat()
    );
    assert_eq!(
        parameters("desktop_reset_harness_setting"),
        [&window[..], &page[..], &["key: String,", "version: u64,"]].concat()
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
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result =
        tokio::time::timeout(Duration::from_secs(20), flow(root.path().to_path_buf())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

async fn engine_reads(root: std::path::PathBuf) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    for spec in crazyagent::harnesses::specs() {
        let id = spec.id.to_owned();
        let read = bridge.harness_settings(id.clone(), None)?.await;
        let Some(shipped) = catalog::builtin(&id)? else {
            assert_eq!(
                read.unwrap_err().code,
                "harness_settings_no_catalog",
                "{id}"
            );
            for refused in [
                bridge
                    .set_harness_setting(id.clone(), None, "a".into(), json!(true), 0)?
                    .await,
                bridge
                    .reset_harness_setting(id.clone(), None, "a".into(), 0)?
                    .await,
            ] {
                assert_eq!(refused.unwrap_err().code, "harness_settings_no_catalog");
            }
            continue;
        };
        let page = read?;
        assert_eq!(
            (page.harness.as_str(), page.name.as_str()),
            (spec.id, spec.name)
        );
        let keys: Vec<&str> = page.keys.iter().map(|row| row.key.as_str()).collect();
        let listed: Vec<&str> = shipped.keys.iter().map(|key| key.key.as_str()).collect();
        assert_eq!(
            &keys[..listed.len()],
            &listed[..],
            "{id} keeps catalog order"
        );
        for row in &page.keys {
            let lock = shipped.key(&row.key).and_then(|key| key.locked.as_ref());
            assert_eq!(matches!(row.access, Access::Locked { .. }), lock.is_some());
            if let Some(lock) = lock {
                assert_eq!((&row.value, row.source), (&lock.engine_value, "engine"));
            }
        }
    }
    let refused = [
        bridge.harness_settings("Codex".into(), None).err(),
        bridge
            .set_harness_setting(
                "codex".into(),
                None,
                "hide_agent_reasoning".into(),
                Value::Null,
                0,
            )
            .err(),
        bridge
            .set_harness_setting("codex".into(), None, "../x".into(), json!(true), 0)
            .err(),
        bridge
            .reset_harness_setting("codex".into(), Some(String::new()), "a".into(), 0)
            .err(),
    ];
    assert_eq!(
        refused.map(|error| error.map(|error| error.code)),
        [
            Some("harness_settings_request_invalid"),
            Some("harness_settings_value_invalid"),
            Some("harness_settings_key_invalid"),
            Some("harness_settings_project_unknown"),
        ],
        "the window's malformed input is refused before the engine is asked"
    );
    Ok(())
}

/// Every harness reaches the window through the real engine: its page with locked keys at
/// the engine's value, or the engine's no-catalog refusal for a read, a set and a reset.
#[tokio::test]
async fn the_window_reads_each_harness_page_or_its_no_catalog_refusal_through_the_real_engine() {
    with_engine("ca-desk-hs-", engine_reads).await;
}

fn fixture_page(
    root: &Path,
    installed: Option<&str>,
    scope: &Scope,
    project_id: Option<String>,
) -> HarnessSettingsPage {
    let fixture = catalog::parse(FIXTURE).unwrap();
    let db = versioned::open(root).unwrap();
    let head = catalog_page(&fixture, installed, None).unwrap();
    let values = store::values(&db, &fixture, installed, scope, None).unwrap();
    assert_eq!(
        (head.next, values.next),
        (None, None),
        "the fixture fits one page"
    );
    page_of(vec![head], vec![values], project_id).unwrap()
}

fn row<'a>(page: &'a HarnessSettingsPage, key: &str) -> &'a SettingRow {
    page.keys.iter().find(|row| row.key == key).unwrap()
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

async fn locked_and_layered(root: std::path::PathBuf) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let id = new_project(&mut bridge).await?;
    let project = Scope::Project(ProjectId::new(id.clone())?);
    let fixture = catalog::parse(FIXTURE)?;
    let db = versioned::open(&root)?;

    let app = fixture_page(&root, QUALIFIED, &Scope::App, None);
    assert_eq!(
        (app.harness.as_str(), app.name.as_str()),
        ("codex", "Codex")
    );
    assert_eq!((app.installed.as_deref(), app.qualified), (QUALIFIED, true));
    assert_eq!(app.qualified_versions, ["0.50.0", "0.51.0"]);
    assert_eq!(app.project_id, None);
    let keys: Vec<&str> = app.keys.iter().map(|row| row.key.as_str()).collect();
    let listed: Vec<&str> = fixture.keys.iter().map(|key| key.key.as_str()).collect();
    assert_eq!(keys, listed);
    let sandbox = row(&app, "sandbox_mode");
    assert_eq!(
        sandbox.access,
        Access::Locked {
            reason: "CrazyAgent runs every agent in its own sandbox.".into()
        }
    );
    assert_eq!(
        (&sandbox.value, sandbox.source, &sandbox.here),
        (&json!("workspace-write"), "engine", &Value::Null)
    );
    assert_eq!(
        sandbox.options,
        ["read-only", "workspace-write", "danger-full-access"]
    );
    assert_eq!(
        serde_json::to_value(sandbox)?["access"],
        json!({"kind": "locked", "reason": "CrazyAgent runs every agent in its own sandbox."}),
        "the window reads the lock and its reason as the TS SettingAccess shape"
    );
    let effort = row(&app, "model_reasoning_effort").clone();
    assert_eq!(effort.access, Access::Editable);
    assert_eq!((effort.kind, effort.source), ("enum", "default"));
    assert_eq!(
        (&effort.value, &effort.here),
        (&json!("medium"), &Value::Null)
    );
    let limit = row(&app, "project_doc_max_bytes");
    assert_eq!(
        (limit.kind, limit.min, limit.max),
        ("integer", Some(0), Some(1_048_576))
    );
    assert_eq!(row(&app, "tui.status_label").max_bytes, Some(32));

    written(store::set(
        &db,
        &fixture,
        QUALIFIED,
        &Scope::App,
        "model_reasoning_effort",
        &json!("high"),
        effort.version,
    )?)?;
    let stale = store::set(
        &db,
        &fixture,
        QUALIFIED,
        &Scope::App,
        "model_reasoning_effort",
        &json!("low"),
        effort.version,
    )?;
    assert_eq!(written(stale).unwrap_err().code, "harness_settings_stale");
    let forced = store::set(
        &db,
        &fixture,
        QUALIFIED,
        &Scope::App,
        "sandbox_mode",
        &json!("danger-full-access"),
        sandbox.version,
    )?;
    assert_eq!(
        written(forced).unwrap_err().code,
        "harness_settings_key_locked"
    );
    let saved = fixture_page(&root, QUALIFIED, &Scope::App, None);
    let high = row(&saved, "model_reasoning_effort");
    assert_eq!(
        (&high.here, &high.value, high.source),
        (&json!("high"), &json!("high"), "app")
    );
    assert!(high.version > effort.version);
    assert_eq!(
        row(&saved, "sandbox_mode").value,
        json!("workspace-write"),
        "no set reaches a locked key"
    );

    let scoped = fixture_page(&root, QUALIFIED, &project, Some(id.clone()));
    assert_eq!(scoped.project_id.as_deref(), Some(id.as_str()));
    let inherited = row(&scoped, "model_reasoning_effort").clone();
    assert_eq!(
        (&inherited.here, &inherited.value, inherited.source),
        (&Value::Null, &json!("high"), "app")
    );
    assert_eq!(
        row(&scoped, "tui.status_label").access,
        Access::ReadOnly {
            reason: APP_ONLY.into()
        }
    );
    let app_only = store::set(
        &db,
        &fixture,
        QUALIFIED,
        &project,
        "tui.status_label",
        &json!("hi"),
        0,
    )?;
    assert_eq!(
        written(app_only).unwrap_err().code,
        "harness_settings_key_app_only"
    );
    written(store::set(
        &db,
        &fixture,
        QUALIFIED,
        &project,
        "model_reasoning_effort",
        &json!("minimal"),
        inherited.version,
    )?)?;
    let own = row(
        &fixture_page(&root, QUALIFIED, &project, Some(id.clone())),
        "model_reasoning_effort",
    )
    .clone();
    assert_eq!(
        (&own.here, &own.value, own.source),
        (&json!("minimal"), &json!("minimal"), "project")
    );
    assert_eq!(
        row(
            &fixture_page(&root, QUALIFIED, &Scope::App, None),
            "model_reasoning_effort"
        )
        .value,
        json!("high")
    );
    written(store::reset(
        &db,
        &fixture,
        QUALIFIED,
        &project,
        "model_reasoning_effort",
        own.version,
    )?)?;
    let back = row(
        &fixture_page(&root, QUALIFIED, &project, Some(id.clone())),
        "model_reasoning_effort",
    )
    .clone();
    assert_eq!(
        (&back.here, &back.value, back.source),
        (&Value::Null, &json!("high"), "app")
    );

    let old = fixture_page(&root, Some("0.49.0"), &Scope::App, None);
    assert!(!old.qualified);
    for row in &old.keys {
        match &row.access {
            Access::Editable => panic!("{} is editable on an unqualified version", row.key),
            Access::Locked { .. } => assert_eq!(row.source, "engine"),
            Access::ReadOnly { reason } => assert_eq!(reason, "Not qualified for Codex 0.49.0"),
        }
    }
    let unqualified = store::set(
        &db,
        &fixture,
        Some("0.49.0"),
        &Scope::App,
        "hide_agent_reasoning",
        &json!(true),
        0,
    )?;
    assert_eq!(
        written(unqualified).unwrap_err().code,
        "harness_settings_key_not_qualified"
    );
    Ok(())
}

/// The window's rows come from the engine's real store: a locked key with its reason and the
/// engine value, and no set reaching it; an app value, a project override and its reset;
/// app-only and unqualified keys read-only with the engine's reason; a stale set refused.
#[tokio::test]
async fn a_locked_key_reaches_the_window_with_its_reason_and_the_engine_value() {
    with_engine("ca-desk-hsl-", locked_and_layered).await;
}

async fn disagreeing(root: std::path::PathBuf) -> Result<(), Box<dyn Error>> {
    let fixture = catalog::parse(FIXTURE)?;
    let db = versioned::open(&root)?;
    let head = catalog_page(&fixture, QUALIFIED, None)?;
    let values = || store::values(&db, &fixture, QUALIFIED, &Scope::App, None);
    assert!(page_of(vec![head.clone()], vec![values()?], None).is_ok());
    let mut missing = values()?;
    missing.settings.pop();
    let mut other = values()?;
    other.harness = "claude-code".into();
    let mut twice = values()?;
    let first = twice.settings[0].clone();
    twice.settings.push(first);
    let mut unlocked = values()?;
    let at = unlocked
        .settings
        .iter()
        .position(|row| row.key == "sandbox_mode")
        .unwrap();
    unlocked.settings[at].access = Access::Editable;
    let mut wrong = values()?;
    let at = wrong
        .settings
        .iter()
        .position(|row| row.key == "project_doc_max_bytes")
        .unwrap();
    wrong.settings[at].effective = json!("many");
    let mut stray = values()?;
    let mut extra = stray.settings[0].clone();
    extra.key = "no_longer_listed".into();
    stray.settings.push(extra);
    for (name, page) in [
        ("a catalog key without a row", missing),
        ("another harness", other),
        ("a row twice", twice),
        ("a catalog-locked key shown editable", unlocked),
        ("an editable value of the wrong type", wrong),
        ("an unlisted key shown editable", stray),
    ] {
        assert_eq!(
            page_of(vec![head.clone()], vec![page], None)
                .unwrap_err()
                .code,
            "owner_read_failed",
            "{name}"
        );
    }
    Ok(())
}

/// Pages that do not agree with the catalog never reach the window as a page it could
/// trust: a missing, doubled or foreign row, a lock dropped, or a value of the wrong type.
#[tokio::test]
async fn engine_pages_that_disagree_with_the_catalog_are_refused() {
    with_engine("ca-desk-hsd-", disagreeing).await;
}
