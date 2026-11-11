#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::questions::{Ask, EngineSource, QuestionStatus, Settlement};
use crazyagent_desktop::bridge::notify::notice;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 3] = [
    "desktop_notify_question",
    "desktop_notification_target",
    "desktop_system_notifications",
];

#[test]
fn notification_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/notify.ts");
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
    assert!(
        !capability.contains("notification:"),
        "the window gets no notification plugin command; the app shows them itself"
    );
}

/// The parameters of one command binding in `lib.rs`, one per line as rustfmt writes them.
/// Input: the command name. Output: each parameter line, trimmed.
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
fn the_window_names_only_the_project_and_the_question() {
    assert_eq!(
        parameters("desktop_notify_question"),
        [
            "webview: Webview,",
            "state: State<'_, NativeState>,",
            "notices: State<'_, Notices>,",
            "project_id: String,",
            "question_id: u64,"
        ]
    );
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
    let project = ProjectId::new(id.clone())?;

    let db = crazyagent::questions::open(root)?;
    let asked = crazyagent::questions::raise(
        &db,
        &project,
        &EngineSource::Mapping,
        &Ask {
            question: "Which database should the app use?\nIt stores recipes.".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
            recommended: Some("SQLite".into()),
            context: Some("It runs on one laptop.".into()),
        },
    )?;

    let record = bridge.notified_question(id.clone(), asked)?.await?;
    assert_eq!(record.id, asked);
    assert_eq!(record.project, project);
    assert_eq!(record.status, QuestionStatus::Pending);
    assert_eq!(
        record.question,
        "Which database should the app use?\nIt stores recipes."
    );
    let shown = notice(Some("Recipe box"), &record);
    assert_eq!(shown.title, "Recipe box needs your answer");
    assert_eq!(
        shown.body, "Project mapping: Which database should the app use?",
        "the text comes from the engine's record, first line only"
    );
    assert_eq!(
        bridge
            .notified_question(id.clone(), u64::MAX)?
            .await
            .unwrap_err()
            .code,
        "questions_not_found"
    );
    assert!(
        bridge
            .notified_question("another-project".into(), asked)?
            .await
            .is_err(),
        "another project's notification never reads this question"
    );

    assert_eq!(
        crazyagent::questions::dismiss(&db, asked, record.version)?,
        Settlement::Settled
    );
    drop(db);
    assert_eq!(
        bridge
            .notified_question(id.clone(), asked)?
            .await
            .unwrap_err()
            .code,
        "questions_not_found",
        "a settled question is never notified"
    );
    Ok(())
}

#[tokio::test]
async fn a_notification_reads_its_pending_question_from_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-n-")
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
