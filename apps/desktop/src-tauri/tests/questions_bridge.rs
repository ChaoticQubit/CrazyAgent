#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::questions::{Ask, EngineSource};
use crazyagent_desktop::bridge::questions::QuestionCount;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 5] = [
    "desktop_questions",
    "desktop_answer_question",
    "desktop_dismiss_question",
    "desktop_dismiss_questions",
    "desktop_question_counts",
];

#[test]
fn question_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-questions.ts");
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
fn the_window_passes_only_ids_versions_and_the_choice() {
    let window = ["webview: Webview,", "state: State<'_, NativeState>,"];
    assert_eq!(
        parameters("desktop_answer_question"),
        [
            &window[..],
            &[
                "id: u64,",
                "version: u64,",
                "picks: Vec<u8>,",
                "comment: Option<String>,"
            ]
        ]
        .concat()
    );
    assert_eq!(
        parameters("desktop_dismiss_question"),
        [&window[..], &["id: u64,", "version: u64,"]].concat()
    );
    assert_eq!(
        parameters("desktop_dismiss_questions"),
        [&window[..], &["items: Vec<(u64, u64)>,"]].concat()
    );
}

/// A bulk dismissal far longer than `LISTED_MAX` is refused at once, before the check that
/// scans its items for a question named twice, so a huge list never holds the bridge lock
/// while that scan runs (RV-DS241-47).
#[test]
fn a_bulk_dismissal_longer_than_the_limit_is_refused_before_its_items_are_scanned() {
    let bridge = NativeBridge::new(None);
    let items: Vec<(u64, u64)> = (1..=40_000).map(|id| (id, 0)).collect();
    let began = std::time::Instant::now();
    let refused = bridge
        .dismiss_questions(items)
        .err()
        .map(|error| error.code);
    let took = began.elapsed();
    assert_eq!(refused, Some("questions_bulk_invalid"));
    assert!(
        took < Duration::from_millis(100),
        "the refusal took {took:?}"
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
    assert!(bridge.question_counts()?.await?.is_empty());

    let db = crazyagent::questions::open(root)?;
    crazyagent::questions::raise(
        &db,
        &project,
        &EngineSource::Mapping,
        &Ask {
            question: "Which database should the app use?\nIt stores recipes.".into(),
            kind: crazyagent::questions::QuestionKind::Single,
            options: vec!["Postgres".into(), "SQLite".into()],
            recommended: Some("SQLite".into()),
            context: Some("It runs on one laptop.".into()),
        },
    )?;
    crazyagent::questions::raise(
        &db,
        &project,
        &EngineSource::Mapping,
        &Ask {
            question: "Should recipes be public?".into(),
            kind: crazyagent::questions::QuestionKind::Text,
            options: vec![],
            recommended: None,
            context: None,
        },
    )?;
    drop(db);

    let page = bridge.questions(id.clone(), None)?.await?;
    assert_eq!(page.items.len(), 2, "{page:?}");
    let asked = page
        .items
        .iter()
        .find(|item| item.question.starts_with("Which database"))
        .unwrap()
        .clone();
    let open = page
        .items
        .iter()
        .find(|item| item.question.starts_with("Should recipes"))
        .unwrap()
        .clone();
    assert_eq!(asked.project_id, id);
    assert_eq!(asked.asker, "engine:mapping");
    assert_eq!(asked.options, ["Postgres", "SQLite"]);
    assert_eq!(asked.recommended.as_deref(), Some("SQLite"));
    assert_eq!(asked.context.as_deref(), Some("It runs on one laptop."));
    assert_eq!((asked.status, open.status), ("pending", "pending"));
    assert_eq!(asked.answer, None);
    assert!(asked.id != open.id);
    assert_eq!(
        bridge.question_counts()?.await?,
        [QuestionCount {
            project_id: id.clone(),
            pending: 2
        }]
    );

    assert_eq!(
        bridge
            .answer_question(asked.id, asked.version, Vec::new(), None)
            .err()
            .map(|error| error.code),
        Some("questions_answer_invalid")
    );
    bridge
        .answer_question(asked.id, asked.version, vec![1], None)?
        .await?;
    assert_eq!(
        bridge
            .answer_question(asked.id, asked.version, vec![0], None)?
            .await
            .unwrap_err()
            .code,
        "questions_stale",
        "a second answer to the version read is stale"
    );
    bridge.dismiss_question(open.id, open.version)?.await?;
    assert_eq!(
        bridge
            .dismiss_question(open.id, open.version)?
            .await
            .unwrap_err()
            .code,
        "questions_stale"
    );

    let settled = bridge.questions(id.clone(), None)?.await?;
    let answered = settled
        .items
        .iter()
        .find(|item| item.id == asked.id)
        .unwrap();
    assert_eq!(answered.status, "answered");
    assert_eq!(answered.answer.as_deref(), Some("SQLite"));
    assert!(answered.version > asked.version);
    assert!(answered.settled_ms.is_some());
    let dismissed = settled
        .items
        .iter()
        .find(|item| item.id == open.id)
        .unwrap();
    assert_eq!(dismissed.status, "dismissed");
    assert_eq!(dismissed.closed_by, None);
    assert!(bridge.question_counts()?.await?.is_empty());

    let db = crazyagent::questions::open(root)?;
    let mut raised = Vec::new();
    for question in ["Add a print view?", "Add tags?"] {
        raised.push(crazyagent::questions::raise(
            &db,
            &project,
            &EngineSource::Mapping,
            &Ask {
                question: question.into(),
                kind: crazyagent::questions::QuestionKind::Text,
                options: vec![],
                recommended: None,
                context: None,
            },
        )?);
    }
    drop(db);
    for items in [
        Vec::new(),
        vec![(raised[0], 0), (raised[0], 0)],
        (1..=crazyagent::questions::LISTED_MAX as u64 + 1)
            .map(|id| (id, 0))
            .collect(),
    ] {
        assert_eq!(
            bridge
                .dismiss_questions(items)
                .err()
                .map(|error| error.code),
            Some("questions_bulk_invalid")
        );
    }
    let outcome = bridge
        .dismiss_questions(vec![
            (raised[0], 0),
            (raised[1], 0),
            (open.id, open.version),
        ])?
        .await?;
    assert_eq!(outcome.settled, raised);
    assert_eq!(outcome.stale, [open.id]);
    assert!(outcome.refused.is_empty());
    assert!(bridge.question_counts()?.await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn desktop_lists_answers_and_dismisses_questions_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-q-")
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
