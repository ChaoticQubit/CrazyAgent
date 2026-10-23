use crazyagent::config::resolve_config;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{
    ContextKind, Platform, ProjectSetup, ResearchKind, ResearchStatus,
};
use crazyagent::protocol::ProjectSpec;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 7] = [
    "desktop_project_catalog",
    "desktop_setup_project",
    "desktop_project_setup",
    "desktop_research_jobs",
    "desktop_project_context",
    "desktop_project_context_item",
    "desktop_save_research_to_memory",
];

#[test]
fn project_setup_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let client = include_str!("../../src/engine-client.ts");
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
            client.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} client"
        );
    }
}

fn setup(documents: Vec<String>) -> ProjectSetup {
    ProjectSetup {
        about: "Tracks shared chores.".into(),
        tech_stack: vec!["react".into(), "postgresql".into()],
        platforms: vec![Platform::MobileApp],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents,
    }
}

async fn flow(root: &Path, sources: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let catalog = bridge
        .project_catalog(None, vec![ResearchKind::MarketResearch])
        .await?;
    assert_eq!(catalog.default_team.master, "codex");
    assert_eq!(
        bridge
            .project_catalog(Some("pool-absent".into()), vec![])
            .await
            .unwrap_err()
            .code,
        "pool_missing"
    );

    let spec = ProjectSpec {
        name: "Chore board".into(),
        repositories: vec![],
    };
    let document = sources.join("brief.txt").to_str().unwrap().to_owned();
    let receipt = bridge
        .setup_project(spec.clone(), setup(vec![document]), 1)
        .await?;
    assert!(
        matches!(receipt.outcome, ActionOutcome::Applied),
        "{receipt:?}"
    );
    assert_eq!(receipt.intent.kind, "setup_project");
    assert!(!receipt.pending);
    let id = receipt.intent.project_id.clone().unwrap();

    let page = bridge.projects(false).await?;
    assert!(
        page.items
            .iter()
            .any(|row| row.id == id && row.name == "Chore board")
    );
    let stored = bridge.project_setup(&id).await?.unwrap();
    assert_eq!(stored.tech_stack, ["react", "postgresql"]);
    let jobs = bridge.research_jobs(&id).await?;
    assert_eq!(jobs.jobs.len(), 1);
    assert_eq!(jobs.jobs[0].status, ResearchStatus::NeedsPool);
    assert_eq!(jobs.jobs[0].status_label, "Needs a pool");

    let context = bridge.project_context(&id).await?;
    assert_eq!(
        context
            .items
            .iter()
            .map(|item| item.kind)
            .collect::<Vec<_>>(),
        [ContextKind::Brief, ContextKind::Document]
    );
    let chunk = bridge
        .project_context_item(&id, "context/documents/brief.txt".into(), 0)
        .await?;
    assert_eq!(chunk.text.as_deref(), Some("Owner notes."));
    assert_eq!(
        bridge
            .project_context_item(&id, "../engine.db".into(), 0)
            .await
            .unwrap_err()
            .code,
        "context_path_invalid"
    );

    std::fs::write(
        Path::new(&stored.folder).join("context/research/market_research.md"),
        "# Market\n\nSmall but loyal. [Ref](https://example.org)\n",
    )?;
    let saved = bridge
        .save_research_to_memory(&id, ResearchKind::MarketResearch, None, 2)
        .await?;
    assert!(matches!(saved.outcome, ActionOutcome::Applied), "{saved:?}");
    assert_eq!(saved.intent.kind, "research_memory");

    std::os::unix::fs::symlink(sources.join("brief.txt"), sources.join("link.txt"))?;
    let rejected = bridge
        .setup_project(
            spec,
            setup(vec![sources.join("link.txt").to_str().unwrap().to_owned()]),
            3,
        )
        .await?;
    assert!(matches!(rejected.outcome, ActionOutcome::Rejected));
    assert_eq!(rejected.rejection, Some("document_not_a_regular_file"));
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn desktop_creates_a_project_folder_and_reads_its_context_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-setup-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("brief.txt"), "Owner notes.").unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result =
        tokio::time::timeout(Duration::from_secs(20), flow(root.path(), sources.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
