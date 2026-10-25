#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::memory::KnowledgeStatus;
use crazyagent::memory::library::{ApprovedKind, ApprovedScope, NOTE_BYTES_MAX};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::ProjectSpec;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 10] = [
    "desktop_save_selection_to_memory",
    "desktop_cancel_master_turn",
    "desktop_remember_research",
    "desktop_memories",
    "desktop_memory",
    "desktop_approved_memories",
    "desktop_add_memory",
    "desktop_promote_memory",
    "desktop_delete_memory",
    "desktop_forget_memory",
];

#[test]
fn memory_and_master_cancel_commands_are_registered_in_all_four_places() {
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
            1,
        )
        .await?;
    assert!(
        matches!(receipt.outcome, ActionOutcome::Applied),
        "{receipt:?}"
    );
    let id = receipt.intent.project_id.clone().unwrap();

    let added = bridge
        .add_memory(None, "Prefer plain words in every screen.".into(), 2)
        .await?;
    assert!(matches!(added.outcome, ActionOutcome::Applied), "{added:?}");
    assert_eq!(added.intent.kind, "memory_add");
    assert_eq!(added.intent.project_id, None);
    let local = bridge
        .add_memory(Some(&id), "Recipes need a print view.".into(), 3)
        .await?;
    assert!(matches!(local.outcome, ActionOutcome::Applied), "{local:?}");
    assert_eq!(local.intent.project_id.as_deref(), Some(id.as_str()));
    let duplicate = bridge
        .add_memory(None, "Prefer plain words in every screen.".into(), 4)
        .await?;
    assert!(matches!(duplicate.outcome, ActionOutcome::Rejected));
    assert_eq!(duplicate.rejection, Some("memory_duplicate"));
    assert_eq!(
        bridge
            .add_memory(None, "x".repeat(NOTE_BYTES_MAX + 1), 5)
            .await
            .unwrap_err()
            .code,
        "memory_note_too_long"
    );
    assert_eq!(
        bridge
            .add_memory(None, " ".into(), 5)
            .await
            .unwrap_err()
            .code,
        "memory_note_blank"
    );

    let global = bridge.memories(None, None, None).await?;
    assert_eq!(global.items.len(), 1);
    assert_eq!(
        (global.items[0].scope, global.items[0].origin),
        ("global", "written")
    );
    assert_eq!(global.next, None);
    let project = bridge.memories(Some(&id), None, None).await?;
    assert_eq!(project.items[0].text, "Recipes need a print view.");
    assert_eq!(project.items[0].project_id.as_deref(), Some(id.as_str()));
    assert!(
        bridge
            .memories(None, Some("PLAIN"), None)
            .await?
            .items
            .iter()
            .all(|m| m.text.contains("plain"))
    );

    let project_folder = bridge.project_setup(&id).await?.unwrap().folder;
    let folder = bridge.project_context(&id).await?.folder;
    assert_eq!(
        Path::new(&folder),
        Path::new(&project_folder).join(".crazyagent")
    );
    std::fs::write(
        Path::new(&folder).join("context/research/market_research.md"),
        "# Market\n\nMost cooks save recipes as photos.\n",
    )?;
    let saved = bridge
        .save_research_to_memory(&id, ResearchKind::MarketResearch, None, 6)
        .await?;
    assert!(matches!(saved.outcome, ActionOutcome::Applied), "{saved:?}");
    let waiting = bridge.project_memories(&id, None).await?;
    assert_eq!(waiting[0].status, KnowledgeStatus::Provisional);
    let selected = bridge
        .save_selection_to_memory(
            &id,
            "context/research/market_research.md".into(),
            "cooks save recipes".into(),
            20,
        )
        .await?;
    assert!(
        matches!(selected.outcome, ActionOutcome::Applied),
        "{selected:?}"
    );
    assert_eq!(selected.intent.kind, "memory_selection");
    assert_eq!(selected.intent.project_id.as_deref(), Some(id.as_str()));
    let local = bridge.memories(Some(&id), None, None).await?;
    assert_eq!(local.items[0].text, "cooks save recipes");
    assert_eq!(local.items[0].origin, "selected");
    assert_eq!(
        local.items[0].source_path.as_deref(),
        Some("context/research/market_research.md")
    );
    assert!(local.items[1..].iter().all(|m| m.source_path.is_none()));
    let stale = bridge
        .save_selection_to_memory(
            &id,
            "context/research/market_research.md".into(),
            "recipes as videos".into(),
            21,
        )
        .await?;
    assert!(matches!(stale.outcome, ActionOutcome::Rejected));
    assert_eq!(stale.rejection, Some("memory_selection_stale"));
    assert_eq!(
        bridge
            .save_selection_to_memory(&id, "context/../g".into(), "cooks".into(), 22)
            .await?
            .rejection,
        Some("context_path_invalid")
    );
    assert_eq!(
        bridge
            .save_selection_to_memory(&id, "bad\npath".into(), "cooks".into(), 22)
            .await
            .unwrap_err()
            .code,
        "context_path_invalid"
    );
    let approved = bridge.approved_memories(&id).await?;
    assert!(
        approved
            .items
            .iter()
            .any(|m| m.kind == ApprovedKind::Selection && m.text == "cooks save recipes")
    );
    let promoted = bridge
        .promote_memory(&id, &waiting[0].id, waiting[0].version, 7)
        .await?;
    assert!(
        matches!(promoted.outcome, ActionOutcome::Applied),
        "{promoted:?}"
    );
    assert_eq!(promoted.intent.kind, "memory_promote");
    assert_eq!(
        bridge.project_memories(&id, None).await?[0].status,
        KnowledgeStatus::Confirmed
    );
    let stale = bridge
        .promote_memory(&id, &waiting[0].id, waiting[0].version, 8)
        .await?;
    assert_eq!(stale.rejection, Some("memory_changed"));
    let global = bridge.memories(None, None, None).await?;
    assert_eq!(global.items[0].origin, "promoted");
    assert_eq!(
        global.items[0].from_project_id.as_deref(),
        Some(id.as_str())
    );

    let approved = bridge.approved_memories(&id).await?;
    assert!(approved.complete);
    assert!(
        approved
            .items
            .iter()
            .any(|m| m.scope == ApprovedScope::Project
                && m.kind == ApprovedKind::Research
                && m.text.contains("photos"))
    );
    assert!(approved.items.iter().any(
        |m| m.scope == ApprovedScope::Global && m.text == "Prefer plain words in every screen."
    ));

    let forgotten = bridge.forget_memory(&id, &waiting[0].id, 9).await?;
    assert!(
        matches!(forgotten.outcome, ActionOutcome::Applied),
        "{forgotten:?}"
    );
    assert_eq!(forgotten.intent.kind, "memory_forget");
    assert!(bridge.project_memories(&id, None).await?.is_empty());
    assert_eq!(
        bridge
            .forget_memory(&id, &waiting[0].id, 10)
            .await?
            .rejection,
        Some("memory_missing")
    );
    assert!(
        bridge.memories(None, None, None).await?.items[0]
            .text
            .contains("photos"),
        "the promoted global copy stays"
    );

    std::fs::write(
        Path::new(&folder).join("context/research/market_research.md"),
        "# Market\n\n## Habits\nCooks plan on Sundays.\n\n## Prices\nMost apps are free.\n",
    )?;
    let remembered = bridge
        .remember_research(&id, ResearchKind::MarketResearch, Some("Habits".into()), 13)
        .await?;
    assert!(
        matches!(remembered.outcome, ActionOutcome::Applied),
        "{remembered:?}"
    );
    assert_eq!(remembered.intent.kind, "memory_remember");
    let kept = bridge.project_memories(&id, None).await?;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].status, KnowledgeStatus::Confirmed);
    assert!(kept[0].text.contains("Cooks plan on Sundays."));
    assert!(!kept[0].text.contains("free"));
    assert_eq!(
        bridge
            .remember_research(&id, ResearchKind::MarketResearch, Some("Absent".into()), 14)
            .await?
            .rejection,
        Some("research_section_missing")
    );
    assert!(
        bridge
            .approved_memories(&id)
            .await?
            .items
            .iter()
            .any(|m| m.kind == ApprovedKind::Research && m.text.contains("Sundays")),
        "one click from a research section to an approved memory"
    );

    let target = global.items[1].id.clone();
    assert_eq!(
        bridge.memory(&target).await?.unwrap().text,
        "Prefer plain words in every screen."
    );
    let deleted = bridge.delete_memory(&target, 15).await?;
    assert!(
        matches!(deleted.outcome, ActionOutcome::Applied),
        "{deleted:?}"
    );
    assert_eq!(deleted.intent.kind, "memory_delete");
    assert!(bridge.memory(&target).await?.is_none());
    let again = bridge.delete_memory(&target, 16).await?;
    assert_eq!(again.rejection, Some("memory_missing"));
    assert_eq!(
        bridge.memory("bad id").await.unwrap_err().code,
        "invalid_memory"
    );
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn desktop_saves_promotes_and_deletes_memory_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-mem-desk-")
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
