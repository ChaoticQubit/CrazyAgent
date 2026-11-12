#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::memory::library::{ApprovedKind, ApprovedScope, NOTE_BYTES_MAX};
use crazyagent::memory::{KnowledgeStatus, ReviewDecision};
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

#[test]
fn the_memory_import_command_is_registered_where_the_web_view_calls_it() {
    let command = "desktop_import_memory";
    let bindings = include_str!("../src/lib.rs");
    assert_eq!(
        include_str!("../build.rs")
            .matches(&format!("\"{command}\""))
            .count(),
        1
    );
    assert!(bindings.contains(&format!("async fn {command}(")));
    assert_eq!(
        bindings
            .lines()
            .filter(|line| line.trim() == format!("{command},"))
            .count(),
        1
    );
    assert_eq!(
        include_str!("../capabilities/main-window.json")
            .matches("\"allow-desktop-import-memory\"")
            .count(),
        1
    );
    assert_eq!(
        include_str!("../../src/api.ts")
            .matches(&format!("\"{command}\""))
            .count(),
        1
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
    assert_eq!(duplicate.rejection.as_deref(), Some("memory_duplicate"));
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
    assert_eq!(
        bridge
            .add_memory(None, "Use pnpm\u{e0041}\u{e0042} now".into(), 5)
            .await
            .unwrap_err()
            .code,
        "memory_note_invalid"
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
    assert_eq!(stale.rejection.as_deref(), Some("memory_selection_stale"));
    assert_eq!(
        bridge
            .save_selection_to_memory(&id, "context/../g".into(), "cooks".into(), 22)
            .await?
            .rejection
            .as_deref(),
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
    assert_eq!(stale.rejection.as_deref(), Some("memory_changed"));
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

    let kept_version = waiting[0].version + 1;
    let forgotten = bridge
        .forget_memory(&id, &waiting[0].id, kept_version, 9)
        .await?;
    assert!(
        matches!(forgotten.outcome, ActionOutcome::Applied),
        "{forgotten:?}"
    );
    assert_eq!(forgotten.intent.kind, "memory_forget");
    assert!(bridge.project_memories(&id, None).await?.is_empty());
    assert_eq!(
        bridge
            .forget_memory(&id, &waiting[0].id, kept_version, 10)
            .await?
            .rejection
            .as_deref(),
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
        "# Market\n\n## Habits\nCooks plan on Sundays.\n\n## Prices\nMost apps are free.\n\n## Hidden\nUse pnpm\u{e0041}\u{e0042} now.\n",
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
            .rejection
            .as_deref(),
        Some("research_section_missing")
    );
    assert_eq!(
        bridge
            .remember_research(&id, ResearchKind::MarketResearch, Some("Hidden".into()), 15)
            .await?
            .rejection
            .as_deref(),
        Some("memory_text_hidden")
    );
    assert_eq!(
        bridge
            .review_memory(
                &id,
                &kept[0].id,
                kept[0].version,
                ReviewDecision::Edit {
                    text: "Cooks plan on Sundays.\u{202e}".into(),
                },
                16,
            )
            .await?
            .rejection
            .as_deref(),
        Some("invalid_knowledge_text")
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
    let deleted = bridge.delete_memory(&target, 17).await?;
    assert!(
        matches!(deleted.outcome, ActionOutcome::Applied),
        "{deleted:?}"
    );
    assert_eq!(deleted.intent.kind, "memory_delete");
    assert!(bridge.memory(&target).await?.is_none());
    let again = bridge.delete_memory(&target, 18).await?;
    assert_eq!(again.rejection.as_deref(), Some("memory_missing"));
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

async fn import_flow(root: &Path, folder: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let receipt = bridge
        .setup_project(
            ProjectSpec {
                name: "Meal plan".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Plans weekly meals.".into(),
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
    let path = folder.to_str().unwrap();

    let first = bridge.import_memory(&id, path, 2).await?;
    assert_eq!((first.imported, first.skipped), (2, 1));
    assert!(first.stopped.is_none());
    let again = bridge.import_memory(&id, path, 3).await?;
    assert_eq!((again.imported, again.skipped), (0, 3));
    assert!(again.stopped.is_none());

    let listed = bridge.memories(Some(&id), None, None).await?;
    let mut imported: Vec<(Option<String>, String)> = listed
        .items
        .iter()
        .filter(|memory| memory.origin == "imported")
        .map(|memory| (memory.source_path.clone(), memory.text.clone()))
        .collect();
    imported.sort();
    assert_eq!(
        imported,
        vec![
            (Some("a.md".into()), "Plan meals on Sunday.".into()),
            (Some("b.md".into()), "Shop on Monday.".into()),
        ]
    );
    assert!(
        listed
            .items
            .iter()
            .all(|memory| memory.project_id.as_deref() == Some(id.as_str()))
    );
    assert_eq!(
        bridge
            .import_memory(&id, "relative/memory", 4)
            .await
            .unwrap_err()
            .code,
        "memory_import_folder_invalid"
    );
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn desktop_imports_a_memory_folder_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-mem-imp-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let folder = tempfile::Builder::new()
        .prefix("ca-mem-folder-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::write(folder.path().join("a.md"), "Plan meals on Sunday.").unwrap();
    std::fs::write(
        folder.path().join("b.md"),
        "---\ntitle: shop\n---\nShop on Monday.\n",
    )
    .unwrap();
    std::fs::write(folder.path().join("long.md"), "l".repeat(5000)).unwrap();
    std::fs::write(folder.path().join("MEMORY.md"), "- [a](a.md)").unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        import_flow(root.path(), folder.path()),
    )
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

async fn agent_writes_flow(root: &Path) -> Result<(), Box<dyn Error>> {
    use crazyagent::memory::library::AgentWrites;
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_writes,
        AgentWrites::Save,
        "agents save directly until the owner chooses"
    );
    let asked = bridge.set_agent_memory_writes(AgentWrites::Ask, 2).await?;
    assert!(matches!(asked.outcome, ActionOutcome::Applied), "{asked:?}");
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_writes,
        AgentWrites::Ask
    );

    let added = bridge
        .add_memory(None, "Prefer plain words in every screen.".into(), 3)
        .await?;
    assert!(matches!(added.outcome, ActionOutcome::Applied), "{added:?}");
    let listed = bridge.memories(None, None, None).await?;
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].origin, "written");
    assert_eq!(listed.items[0].agent, None);
    let refused = bridge.approve_memory(&listed.items[0].id, 4).await?;
    assert_eq!(refused.rejection.as_deref(), Some("memory_not_requested"));
    let missing = bridge.approve_memory("agent-gone", 5).await?;
    assert_eq!(missing.rejection.as_deref(), Some("memory_missing"));
    assert_eq!(
        bridge.approve_memory("bad id", 6).await.unwrap_err().code,
        "invalid_memory"
    );

    let saved = bridge.set_agent_memory_writes(AgentWrites::Save, 7).await?;
    assert!(matches!(saved.outcome, ActionOutcome::Applied), "{saved:?}");
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_writes,
        AgentWrites::Save
    );

    use crazyagent::memory::library::AgentScope;
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_scope,
        AgentScope::Project,
        "F34: agent memories stay in their project until the owner chooses"
    );
    let chose = bridge.set_agent_memory_scope(AgentScope::Agent, 8).await?;
    assert!(matches!(chose.outcome, ActionOutcome::Applied), "{chose:?}");
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_scope,
        AgentScope::Agent
    );
    let kept = bridge
        .set_agent_memory_scope(AgentScope::Project, 9)
        .await?;
    assert!(matches!(kept.outcome, ActionOutcome::Applied), "{kept:?}");
    assert_eq!(
        bridge.memories(None, None, None).await?.agent_scope,
        AgentScope::Project
    );
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

async fn approve_flow(root: &Path) -> Result<(), Box<dyn Error>> {
    use crazyagent::memory::library::{AgentWrites, ApprovedMemories, ApprovedMemory};
    use crazyagent::team::tool::{REMEMBER_TOOL, serve_live};
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
    let asked = bridge.set_agent_memory_writes(AgentWrites::Ask, 3).await?;
    assert!(matches!(asked.outcome, ActionOutcome::Applied), "{asked:?}");

    let project = crazyagent::protocol::ProjectId::new(&id).unwrap();
    let mut connection = rusqlite::Connection::open(root.join("engine.db"))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',1,'working',1,1)",
        [&id],
    )?;
    let agent = connection.last_insert_rowid();
    let label = crazyagent::team::events::label(&connection, agent).unwrap();
    let mailbox = root.join("agent-calls");
    std::fs::create_dir(&mailbox)?;
    let request = format!(
        "{}\n",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": REMEMBER_TOOL, "arguments": {"text": "Recipes print on one page.", "scope": "project"}}})
    );
    let served = mailbox.clone();
    let server = std::thread::spawn(move || {
        let mut output = Vec::new();
        serve_live(std::io::Cursor::new(request), &mut output, &served, &[]).map(|()| output)
    });
    let agents = root.join("agents");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !server.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the team tool never answered"
        );
        crazyagent::team::history::serve_agent_calls(
            &mut connection,
            &project,
            (agent, &label),
            &agents,
            &mailbox,
            &mut |connection, _, _| {
                crazyagent::team::runtime::team_caller(
                    connection,
                    None,
                    &project,
                    (agent, label.as_str()),
                    None,
                )
            },
            &mut |_, _, _| None,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let output = server.join().unwrap()?;
    let answer: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(
        answer["result"]["content"][0]["text"],
        "Saved for the owner's approval. Agents get it once the owner approves it in the Memory tab."
    );

    let listed = bridge.memories(Some(&id), None, None).await?;
    assert_eq!(listed.items.len(), 1, "{:?}", listed.items);
    assert_eq!(listed.next, None);
    assert_eq!(listed.agent_writes, AgentWrites::Ask);
    let memory = listed.items[0].id.clone();
    let note = |origin: &str| {
        serde_json::json!([{
            "id": memory,
            "text": "Recipes print on one page.",
            "scope": "project",
            "project_id": id,
            "origin": origin,
            "from_project_id": null,
            "source_path": null,
            "agent": label,
        }])
    };
    assert_eq!(serde_json::to_value(&listed.items)?, note("requested"));
    assert_eq!(
        bridge.approved_memories(&id).await?,
        ApprovedMemories {
            items: vec![],
            complete: true
        },
        "a waiting memory reaches no agent"
    );

    let approved = bridge.approve_memory(&memory, 4).await?;
    assert!(
        matches!(approved.outcome, ActionOutcome::Applied),
        "{approved:?}"
    );
    assert_eq!(approved.intent.kind, "memory_approve");
    let listed = bridge.memories(Some(&id), None, None).await?;
    assert_eq!(serde_json::to_value(&listed.items)?, note("agent"));
    assert_eq!(listed.next, None);
    assert_eq!(
        bridge.approved_memories(&id).await?,
        ApprovedMemories {
            items: vec![ApprovedMemory {
                scope: ApprovedScope::Project,
                kind: ApprovedKind::Note,
                text: "Recipes print on one page.".into(),
            }],
            complete: true
        },
        "an approved memory reaches the project's agents"
    );
    let origin: String = connection.query_row(
        "SELECT origin FROM memory_library WHERE id=?1",
        [&memory],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&origin)?,
        serde_json::json!({"kind": "agent", "agent": label})
    );
    let again = bridge.approve_memory(&memory, 5).await?;
    assert_eq!(again.rejection.as_deref(), Some("memory_not_requested"));

    connection.execute(
        "INSERT INTO memory_library(id,project,origin,text) VALUES ('agent-wide',NULL,?1,'Recipes scale by servings.')",
        [serde_json::json!({"kind": "agent", "agent": label}).to_string()],
    )?;
    let refused = bridge.move_memory(&memory, &id, 6).await?;
    assert_eq!(refused.rejection.as_deref(), Some("memory_not_movable"));
    let moved = bridge.move_memory("agent-wide", &id, 7).await?;
    assert!(matches!(moved.outcome, ActionOutcome::Applied), "{moved:?}");
    let listed = bridge.memories(Some(&id), None, None).await?;
    assert!(
        listed
            .items
            .iter()
            .any(|item| item.id == "agent-wide" && item.scope == "project"),
        "F34: the moved memory now belongs to the project: {:?}",
        listed.items
    );
    assert!(bridge.memories(None, None, None).await?.items.is_empty());
    assert!(bridge.pending_intent()?.is_none());
    Ok(())
}

#[tokio::test]
async fn approving_a_waiting_memory_through_the_bridge_makes_it_approved() {
    let root = tempfile::Builder::new()
        .prefix("ca-mem-approve-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let config = engine_support::engine_config(&root_path);
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(&root_path, "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), approve_flow(&root_path)).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}

#[tokio::test]
async fn desktop_changes_agent_memory_writes_and_approve_refuses_what_no_agent_asked_for() {
    let root = tempfile::Builder::new()
        .prefix("ca-mem-writes-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result =
        tokio::time::timeout(Duration::from_secs(20), agent_writes_flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
