use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::memory::KnowledgeStatus;
use crate::memory::import::ImportReport;
use crate::project_setup::{Platform, ProjectSetup, ResearchKind};
use crate::protocol::ProjectSpec;
use rusqlite::OpenFlags;
use rusqlite::types::ValueRef;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long these engine tests let the engine and its owner client wait for
/// one answer. Every test engine in the lib test binary shares one
/// process-wide git setup lock, so parallel New Projects queue behind each
/// other's real git work; the 1 s owner deadline and a 2 s client wait then
/// time out although the engine is healthy. The same value as the TUI tests'
/// `test_home::ENGINE_WAIT`.
const ENGINE_WAIT: Duration = Duration::from_secs(10);

struct Engine {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    service: Option<EngineService>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mem-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("timeout_ms = {}\n", ENGINE_WAIT.as_millis()),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let home = tempfile::Builder::new()
            .prefix("ca-mem-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let (stop, _) = tokio::sync::watch::channel(false);
        let mut engine = Self {
            root,
            home,
            service: None,
            stop,
        };
        engine.boot().await;
        let mut owner = engine.owner().await;
        let picked = std::fs::canonicalize(engine.home.path()).unwrap();
        let response = owner
            .set_projects_home(picked.to_str().unwrap().into(), 1)
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied);
        engine
    }

    async fn boot(&mut self) {
        assert!(self.service.is_none());
        let stopped = self.stop.subscribe();
        self.service = Some(
            EngineService::start_with_settings_sources(
                self.root.path(),
                "e.sock",
                &self.root.path().join("global.toml"),
                &self.root.path().join("project.toml"),
                &stopped,
            )
            .await
            .unwrap(),
        );
    }

    async fn restart(&mut self) {
        let mut service = self.service.take().unwrap();
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        self.boot().await;
    }

    async fn owner(&self) -> OwnerController {
        OwnerController::attach(self.root.path(), OwnerClientKind::Tui, ENGINE_WAIT)
            .await
            .unwrap()
    }

    fn database(&self) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        db
    }

    /// The `.crazyagent` folder holding the context of the project that
    /// `create_project` made for `project` in the projects home.
    fn folder(&self, project: &str) -> PathBuf {
        let name = crate::projects_home::slug(&format!("Project {project}")).unwrap();
        std::fs::canonicalize(self.home.path())
            .unwrap()
            .join(name)
            .join(crate::projects_home::CRAZYAGENT_DIRECTORY)
    }

    async fn stop(mut self) {
        let mut service = self.service.take().unwrap();
        service.shutdown().await.unwrap();
    }
}

fn result(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } => operation.result,
        other => panic!("owner mutation must commit: {other:?}"),
    }
}

fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

fn project_scope(project: &ProjectId) -> MemoryScope {
    MemoryScope::Project {
        project_id: project.clone(),
    }
}

async fn create_project(owner: &mut OwnerController, id: &str, generation: u64) -> ProjectId {
    let project = ProjectId::new(id).unwrap();
    let setup = ProjectSetup {
        about: "Plans weekly meals.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: format!("Project {id}"),
        repositories: vec![],
    };
    let response = owner
        .setup_project(project.clone(), spec, setup, generation)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    project
}

fn tables_holding(db: &rusqlite::Connection, needle: &str) -> Vec<String> {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut found = Vec::new();
    for table in tables {
        let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let hit = (0..columns).any(|index| match row.get_ref(index).unwrap() {
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                _ => false,
            });
            if hit {
                found.push(table.clone());
                break;
            }
        }
    }
    found
}

fn approved_direct(engine: &Engine, project: &ProjectId) -> ApprovedMemories {
    let mut db = engine.database();
    let tx = db.transaction().unwrap();
    approved_for_project(&tx, project).unwrap()
}

#[test]
fn notes_and_searches_are_bounded_with_plain_problems() {
    validate_note("Prefer Rust for engines.").unwrap();
    validate_note("Line one\n\tLine two").unwrap();
    validate_note("Warn with \u{26a0}\u{fe0f} and \u{1f468}\u{200d}\u{1f469}").unwrap();
    validate_note(&"n".repeat(NOTE_BYTES_MAX)).unwrap();
    validate_note(&format!("  {}  ", "n".repeat(NOTE_BYTES_MAX))).unwrap();
    for (text, code) in [
        ("   \n ".to_owned(), "memory_note_blank"),
        ("n".repeat(NOTE_BYTES_MAX + 1), "memory_note_too_long"),
        ("bell\u{7}".to_owned(), "memory_note_invalid"),
        ("escape\u{1b}[31m".to_owned(), "memory_note_invalid"),
        (
            "Use pnpm\u{e0041}\u{e0042}".to_owned(),
            "memory_note_invalid",
        ),
        ("Use pnpm\u{202e}llatsni".to_owned(), "memory_note_invalid"),
        ("Use\u{200b}pnpm".to_owned(), "memory_note_invalid"),
    ] {
        assert_eq!(validate_note(&text).unwrap_err().0, code);
        assert!(plain_problem(code).is_some(), "{code}");
    }
    validate_page(None, 1).unwrap();
    validate_page(Some("rust"), PAGE_ITEMS_MAX).unwrap();
    for (search, limit, code) in [
        (None, 0, "memory_page_invalid"),
        (None, PAGE_ITEMS_MAX + 1, "memory_page_invalid"),
        (Some(" ".to_owned()), 5, "memory_search_invalid"),
        (
            Some("s".repeat(SEARCH_BYTES_MAX + 1)),
            5,
            "memory_search_invalid",
        ),
        (Some("two\nlines".to_owned()), 5, "memory_search_invalid"),
    ] {
        assert_eq!(validate_page(search.as_deref(), limit).unwrap_err().0, code);
        assert!(plain_problem(code).is_some(), "{code}");
    }
    for code in [
        "memory_duplicate",
        "memory_full",
        "memory_missing",
        "memory_changed",
        "memory_not_kept",
        "memory_already_global",
        "memory_project_missing",
    ] {
        let sentence = plain_problem(code).unwrap();
        assert!(sentence.ends_with('.') && !sentence.contains('_'), "{code}");
    }
    assert_eq!(plain_problem("knowledge_scope_denied"), None);
    for code in CODES {
        assert_eq!(known_code(code), Some(code));
        assert!(plain_problem(code).is_some(), "{code}");
    }
    assert_eq!(known_code("knowledge_scope_denied"), None);
    assert_eq!(
        LibraryAction::Promote {
            project_id: ProjectId::new("p").unwrap(),
            entry_id: RequestId::new("e").unwrap(),
            version: 0,
        }
        .validate()
        .unwrap_err()
        .0,
        "memory_changed"
    );
}

#[test]
fn the_launch_section_is_plain_and_never_exceeds_its_bound() {
    assert_eq!(
        ApprovedMemories {
            items: vec![],
            complete: true
        }
        .render(),
        ""
    );
    let small = ApprovedMemories {
        items: vec![
            ApprovedMemory {
                scope: ApprovedScope::Global,
                kind: ApprovedKind::Note,
                text: "Prefer Rust.".into(),
            },
            ApprovedMemory {
                scope: ApprovedScope::Project,
                kind: ApprovedKind::Research,
                text: "Two competitors.\nBoth are paid.".into(),
            },
        ],
        complete: false,
    };
    let text = small.render();
    let project = text.find("### This project").unwrap();
    let global = text.find("### All projects").unwrap();
    assert!(project < global, "{text}");
    assert!(
        text.contains("- Two competitors.\n  Both are paid.\n"),
        "{text}"
    );
    assert!(text.contains("- Prefer Rust.\n"), "{text}");
    assert!(text.contains("left out"), "{text}");
    let mut collector = Collector {
        items: Vec::new(),
        seen: BTreeSet::new(),
        bytes: 0,
        complete: true,
    };
    for index in 0..100 {
        collector.push(
            ApprovedScope::Global,
            ApprovedKind::Note,
            &format!("{index}{}", "\nline".repeat(300)),
        );
    }
    assert!(!collector.complete);
    assert!(
        collector.items.len() < LAUNCH_ITEMS_MAX,
        "the byte bound, with its indentation, stops the set first"
    );
    assert!(collector.bytes <= LAUNCH_BYTES_MAX);
    let bounded = ApprovedMemories {
        items: collector.items,
        complete: collector.complete,
    };
    assert!(bounded.render().len() <= LAUNCH_BYTES_MAX + RENDER_HEADER_BYTES_MAX);
}

#[test]
fn storage_refuses_bad_notes_and_full_scopes_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open(directory.path().join("memory.db")).unwrap();
    db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
        .unwrap();
    crate::memory::initialize(&db).unwrap();
    let config =
        crate::config::resolve_config(&directory.path().join("g"), &directory.path().join("p"))
            .unwrap();
    let alpha = ProjectId::new("alpha").unwrap();
    let tx = db.transaction().unwrap();
    for (scope, text, code) in [
        (
            MemoryScope::Global,
            "n".repeat(NOTE_BYTES_MAX + 1),
            "memory_note_too_long",
        ),
        (MemoryScope::Global, "  ".into(), "memory_note_blank"),
        (MemoryScope::Global, "a\u{0}b".into(), "memory_note_invalid"),
        (
            project_scope(&ProjectId::new("absent").unwrap()),
            "Note".into(),
            "memory_project_missing",
        ),
    ] {
        let action = LibraryAction::Add { scope, text };
        assert_eq!(
            execute(&tx, &config, &action, 1).unwrap(),
            Outcome::Rejected { code: code.into() }
        );
    }
    let count = |tx: &Transaction<'_>| -> i64 {
        tx.query_row("SELECT COUNT(*) FROM memory_library", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&tx), 0);
    for index in 0..SCOPE_ITEMS_MAX as u64 {
        let action = LibraryAction::Add {
            scope: project_scope(&alpha),
            text: format!("note {index}"),
        };
        assert_eq!(
            execute(&tx, &config, &action, index + 1).unwrap(),
            Outcome::Applied
        );
    }
    let full = LibraryAction::Add {
        scope: project_scope(&alpha),
        text: "one too many".into(),
    };
    assert_eq!(
        execute(&tx, &config, &full, 999).unwrap(),
        Outcome::Rejected {
            code: "memory_full".into()
        }
    );
    let global = LibraryAction::Add {
        scope: MemoryScope::Global,
        text: "note 1".into(),
    };
    assert_eq!(
        execute(&tx, &config, &global, 1000).unwrap(),
        Outcome::Applied,
        "the same text may live in another scope"
    );
    assert_eq!(count(&tx), SCOPE_ITEMS_MAX as i64 + 1);
    let approved = approved_for_project(&tx, &alpha).unwrap();
    assert_eq!(approved.items.len(), LAUNCH_ITEMS_MAX);
    assert!(!approved.complete);
    assert_eq!(
        approved.items[0].text,
        format!("note {}", SCOPE_ITEMS_MAX - 1)
    );
    assert!(
        approved
            .items
            .iter()
            .all(|item| item.scope == ApprovedScope::Project)
    );
    assert!(approved.render().len() <= LAUNCH_BYTES_MAX + RENDER_HEADER_BYTES_MAX);
}

#[test]
fn notes_saved_with_hidden_characters_before_the_check_never_reach_agents() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open(directory.path().join("memory.db")).unwrap();
    db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
        .unwrap();
    crate::memory::initialize(&db).unwrap();
    let config =
        crate::config::resolve_config(&directory.path().join("g"), &directory.path().join("p"))
            .unwrap();
    let alpha = ProjectId::new("alpha").unwrap();
    let tx = db.transaction().unwrap();
    for (index, (scope, text)) in [
        (project_scope(&alpha), "Prefer Rust for engines."),
        (MemoryScope::Global, "Write plain commit messages."),
    ]
    .into_iter()
    .enumerate()
    {
        let action = LibraryAction::Add {
            scope,
            text: text.into(),
        };
        assert_eq!(
            execute(&tx, &config, &action, index as u64 + 1).unwrap(),
            Outcome::Applied
        );
    }
    let origin: String = tx
        .query_row("SELECT origin FROM memory_library LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tags = "\u{E0049}\u{E0047}\u{E004E}\u{E004F}\u{E0052}\u{E0045}";
    for (id, project, text) in [
        (
            "legacy-project",
            Some("alpha"),
            format!("Run the deploy.{tags}"),
        ),
        (
            "legacy-global",
            None,
            "Trust \u{202e}this\u{202c} text.".to_owned(),
        ),
    ] {
        assert!(crate::memory::hidden_text(&text), "{text:?}");
        tx.execute(
            "INSERT INTO memory_library(id,project,origin,text) VALUES (?1,?2,?3,?4)",
            rusqlite::params![id, project, origin, text],
        )
        .unwrap();
    }

    let approved = approved_for_project(&tx, &alpha).unwrap();
    let texts: Vec<&str> = approved
        .items
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    assert_eq!(
        texts,
        ["Prefer Rust for engines.", "Write plain commit messages."]
    );
    assert!(approved.complete);
    assert!(!crate::memory::hidden_text(&approved.render()));
}

#[tokio::test]
async fn owner_memory_survives_restart_promotes_in_one_click_and_deletes_for_good() {
    let mut engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let meals = create_project(&mut owner, "mem-meals", next()).await;
    let other = create_project(&mut owner, "mem-other", next()).await;

    let global_note = "Prefer Rust for engine work.";
    let project_note = "Ship the website before the mobile app.";
    let secret_note = "Forget-me phrase 7f3a: the old pricing idea.";
    for (scope, text) in [
        (MemoryScope::Global, global_note),
        (project_scope(&meals), project_note),
        (MemoryScope::Global, secret_note),
        (project_scope(&other), "Only for the other project."),
    ] {
        let response = owner
            .add_memory(scope, format!("  {text}\n"), next())
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied, "{text}");
    }
    assert_eq!(
        result(
            owner
                .add_memory(MemoryScope::Global, global_note.into(), next())
                .await
                .unwrap()
        ),
        rejected("memory_duplicate")
    );
    assert_eq!(
        result(
            owner
                .add_memory(
                    project_scope(&ProjectId::new("mem-absent").unwrap()),
                    "Lost note".into(),
                    next()
                )
                .await
                .unwrap()
        ),
        rejected("memory_project_missing")
    );
    assert_eq!(
        owner
            .add_memory(MemoryScope::Global, "n".repeat(NOTE_BYTES_MAX + 1), next())
            .await
            .unwrap_err()
            .0,
        "memory_note_too_long"
    );

    let globals = owner
        .memories(MemoryScope::Global, None, None)
        .await
        .unwrap();
    assert_eq!(
        globals
            .items
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        [secret_note, global_note],
        "global memory lists newest first, trimmed"
    );
    assert!(
        globals
            .items
            .iter()
            .all(|m| m.origin == MemoryOrigin::Written)
    );
    assert_eq!(globals.next, None);
    let local = owner
        .memories(project_scope(&meals), None, None)
        .await
        .unwrap();
    assert_eq!(
        local
            .items
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        [project_note]
    );
    let found = owner
        .memories(MemoryScope::Global, Some("  RUST ".into()), None)
        .await
        .unwrap();
    assert_eq!(
        found
            .items
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        [global_note],
        "search ignores case and surrounding spaces"
    );
    assert_eq!(
        owner
            .memories(MemoryScope::Global, Some("absent words".into()), None)
            .await
            .unwrap()
            .items,
        []
    );
    assert_eq!(
        owner
            .memories(
                project_scope(&ProjectId::new("mem-absent").unwrap()),
                None,
                None
            )
            .await
            .unwrap_err()
            .0,
        "memory_project_missing"
    );

    for index in 0..25 {
        let response = owner
            .add_memory(
                MemoryScope::Global,
                format!("Paged fact {index:02}"),
                next(),
            )
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied);
    }
    let first = owner
        .memories(MemoryScope::Global, None, None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), usize::from(PAGE_ITEMS_MAX));
    assert_eq!(first.items[0].text, "Paged fact 24");
    let second = owner
        .memories(MemoryScope::Global, None, first.next)
        .await
        .unwrap();
    assert_eq!(second.next, None);
    let mut seen: Vec<&str> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(seen.len(), 27);
    assert_eq!(seen[25..], [secret_note, global_note]);
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 27, "pages never overlap");
    let paged = owner
        .memories(MemoryScope::Global, Some("paged fact".into()), None)
        .await
        .unwrap();
    assert_eq!(paged.items.len(), usize::from(PAGE_ITEMS_MAX));
    let rest = owner
        .memories(MemoryScope::Global, Some("paged fact".into()), paged.next)
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 5);
    assert_eq!(rest.next, None);
    for memory in paged.items.iter().chain(&rest.items) {
        assert_eq!(
            result(
                owner
                    .delete_memory(memory.id.clone(), next())
                    .await
                    .unwrap()
            ),
            OwnerResult::Applied
        );
    }
    assert_eq!(
        owner
            .memories(MemoryScope::Global, None, None)
            .await
            .unwrap()
            .items
            .len(),
        2
    );

    std::fs::write(
        engine
            .folder("mem-meals")
            .join("context/research/market_research.md"),
        "# Market\n\nAbout 2 million households plan meals weekly.\n",
    )
    .unwrap();
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let research = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(research.len(), 1);
    assert_eq!(research[0].status, KnowledgeStatus::Provisional);
    let entry = research[0].clone();
    assert!(
        approved_direct(&engine, &meals)
            .items
            .iter()
            .all(|item| item.text != entry.text),
        "research waiting for review never reaches agents"
    );
    assert_eq!(
        result(
            owner
                .promote_memory(meals.clone(), entry.id.clone(), entry.version + 1, next())
                .await
                .unwrap()
        ),
        rejected("memory_changed")
    );
    assert_eq!(
        result(
            owner
                .promote_memory(other.clone(), entry.id.clone(), entry.version, next())
                .await
                .unwrap()
        ),
        rejected("memory_missing"),
        "a memory is promoted only from its own project"
    );
    assert_eq!(
        result(
            owner
                .promote_memory(meals.clone(), entry.id.clone(), entry.version, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let kept = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(
        kept.iter()
            .map(|e| (&e.status, e.human_reviewed, e.version))
            .collect::<Vec<_>>(),
        [(&KnowledgeStatus::Confirmed, true, entry.version + 1)],
        "one click keeps the project memory and promotes it"
    );
    let promoted = owner
        .memories(MemoryScope::Global, Some("households".into()), None)
        .await
        .unwrap();
    assert_eq!(promoted.items.len(), 1);
    assert_eq!(promoted.items[0].text, entry.text);
    assert_eq!(
        promoted.items[0].origin,
        MemoryOrigin::Promoted {
            project_id: meals.clone(),
            entry_id: entry.id.clone(),
            version: entry.version + 1,
        }
    );
    assert_eq!(
        result(
            owner
                .promote_memory(meals.clone(), entry.id.clone(), entry.version + 1, next())
                .await
                .unwrap()
        ),
        rejected("memory_already_global")
    );

    let approved = owner.approved_memories(meals.clone()).await.unwrap();
    assert_eq!(approved, approved_direct(&engine, &meals));
    assert!(approved.complete);
    let texts: Vec<(ApprovedScope, ApprovedKind, &str)> = approved
        .items
        .iter()
        .map(|item| (item.scope, item.kind, item.text.as_str()))
        .collect();
    assert_eq!(
        texts[0],
        (ApprovedScope::Project, ApprovedKind::Note, project_note)
    );
    assert_eq!(
        texts[1],
        (
            ApprovedScope::Project,
            ApprovedKind::Research,
            entry.text.as_str()
        )
    );
    assert_eq!(
        texts
            .iter()
            .filter(|(_, _, text)| *text == entry.text)
            .count(),
        1,
        "a promoted memory is given once"
    );
    assert!(texts.contains(&(ApprovedScope::Global, ApprovedKind::Note, global_note)));
    assert!(
        !texts
            .iter()
            .any(|(_, _, text)| text.contains("other project")),
        "another project's notes stay there"
    );
    let rendered = approved.render();
    assert!(rendered.contains(project_note) && rendered.contains(global_note));

    assert_eq!(
        result(
            owner
                .forget_memory(other.clone(), entry.id.clone(), entry.version, next())
                .await
                .unwrap()
        ),
        rejected("memory_missing"),
        "a memory is deleted only from its own project"
    );
    assert_eq!(
        result(
            owner
                .forget_memory(meals.clone(), entry.id.clone(), entry.version + 1, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert!(
        owner
            .project_memories(meals.clone(), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        tables_holding(&engine.database(), "households plan meals"),
        ["memory_library"],
        "only the global copy the owner promoted keeps the research text"
    );
    let forgotten = approved_direct(&engine, &meals);
    assert!(
        forgotten
            .items
            .iter()
            .all(|item| item.kind != ApprovedKind::Research)
    );
    assert!(forgotten.items.iter().any(|item| item.text == entry.text
        && item.scope == ApprovedScope::Global
        && item.kind == ApprovedKind::Promoted));
    assert_eq!(
        result(
            owner
                .forget_memory(meals.clone(), entry.id.clone(), entry.version + 1, next())
                .await
                .unwrap()
        ),
        rejected("memory_missing")
    );
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "research deleted for good can be saved again"
    );
    assert_eq!(
        owner.project_memories(meals.clone(), None).await.unwrap()[0].status,
        KnowledgeStatus::Provisional
    );
    assert_eq!(
        result(
            owner
                .remember_research(
                    meals.clone(),
                    ResearchKind::MarketResearch,
                    Some("Absent heading".into()),
                    next()
                )
                .await
                .unwrap()
        ),
        rejected("research_section_missing")
    );
    assert_eq!(
        result(
            owner
                .remember_research(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "one click keeps saved research"
    );
    let remembered = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(
        remembered
            .iter()
            .map(|e| (&e.status, e.human_reviewed))
            .collect::<Vec<_>>(),
        [(&KnowledgeStatus::Confirmed, true)]
    );
    assert_eq!(
        result(
            owner
                .remember_research(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        rejected("memory_duplicate")
    );
    let approved = owner.approved_memories(meals.clone()).await.unwrap();
    assert!(
        approved
            .items
            .iter()
            .any(|item| item.kind == ApprovedKind::Research && item.text == entry.text)
    );
    assert_eq!(
        approved
            .items
            .iter()
            .filter(|item| item.text == entry.text)
            .count(),
        1
    );

    let secret = globals.items[0].clone();
    assert_eq!(secret.text, secret_note);
    drop(owner);
    engine.restart().await;
    let mut owner = engine.owner().await;
    assert_eq!(
        owner.memory(secret.id.clone()).await.unwrap(),
        Some(secret.clone()),
        "saved memory survives an engine restart"
    );
    assert_eq!(
        owner.approved_memories(meals.clone()).await.unwrap(),
        approved,
        "agents get the same approved memories after a restart"
    );
    assert_eq!(
        result(owner.delete_memory(secret.id.clone(), 1).await.unwrap()),
        OwnerResult::Applied
    );
    assert_eq!(owner.memory(secret.id.clone()).await.unwrap(), None);
    assert_eq!(
        result(owner.delete_memory(secret.id.clone(), 2).await.unwrap()),
        rejected("memory_missing")
    );
    assert!(
        tables_holding(&engine.database(), "Forget-me phrase 7f3a").is_empty(),
        "a deleted memory leaves no copy in any engine table"
    );
    let local_id = local.items[0].id.clone();
    assert_eq!(
        result(owner.delete_memory(local_id.clone(), 3).await.unwrap()),
        OwnerResult::Applied
    );
    drop(owner);
    engine.restart().await;
    let mut owner = engine.owner().await;
    assert_eq!(owner.memory(secret.id).await.unwrap(), None);
    assert_eq!(owner.memory(local_id).await.unwrap(), None);
    assert!(
        owner
            .memories(project_scope(&meals), None, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let after = owner.approved_memories(meals.clone()).await.unwrap();
    assert!(
        after
            .items
            .iter()
            .all(|item| item.text != project_note && item.text != secret_note)
    );
    assert!(after.items.iter().any(|item| item.text == entry.text));
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn memory_edit_on_a_stale_revision_is_refused() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let meals = create_project(&mut owner, "mem-stale", next()).await;
    std::fs::write(
        engine
            .folder("mem-stale")
            .join("context/research/market_research.md"),
        "# Market\n\nFamilies plan dinners on Sundays.\n",
    )
    .unwrap();
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let opened = owner.project_memories(meals.clone(), None).await.unwrap()[0].clone();
    assert_eq!(opened.version, 1);
    let edit = |text: &str| ReviewDecision::Edit { text: text.into() };
    let desktop = "Families plan dinners on Sundays and Wednesdays.";
    let tui = "Families plan dinners.";

    assert_eq!(
        result(
            owner
                .review_memory(
                    meals.clone(),
                    opened.id.clone(),
                    opened.version,
                    edit(desktop),
                    next()
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "the first client's edit lands"
    );
    assert_eq!(
        result(
            owner
                .review_memory(
                    meals.clone(),
                    opened.id.clone(),
                    opened.version,
                    edit(tui),
                    next()
                )
                .await
                .unwrap()
        ),
        rejected("knowledge_version_conflict"),
        "an edit naming the version it opened is refused once another edit landed"
    );
    let current = owner.project_memories(meals.clone(), None).await.unwrap()[0].clone();
    assert_eq!(
        (current.version, current.text.as_str()),
        (2, desktop),
        "the refused edit changed nothing"
    );
    assert_eq!(
        result(
            owner
                .forget_memory(meals.clone(), opened.id.clone(), opened.version, next())
                .await
                .unwrap()
        ),
        rejected("memory_changed"),
        "deleting a memory edited since it was opened is refused"
    );
    assert_eq!(
        result(
            owner
                .promote_memory(meals.clone(), opened.id.clone(), opened.version, next())
                .await
                .unwrap()
        ),
        rejected("memory_changed")
    );
    assert_eq!(
        owner.project_memories(meals.clone(), None).await.unwrap(),
        vec![current.clone()],
        "the memory is still there, unchanged"
    );

    assert_eq!(
        result(
            owner
                .review_memory(
                    meals.clone(),
                    opened.id.clone(),
                    current.version,
                    edit(tui),
                    next()
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "after reading the newer version, the owner's edit lands over it"
    );
    let kept = owner.project_memories(meals.clone(), None).await.unwrap()[0].clone();
    assert_eq!((kept.version, kept.text.as_str()), (3, tui));
    assert_eq!(
        result(
            owner
                .forget_memory(meals.clone(), opened.id.clone(), kept.version, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert!(
        owner
            .project_memories(meals, None)
            .await
            .unwrap()
            .is_empty()
    );
    engine.stop().await;
}

#[test]
fn a_selection_matches_the_file_as_rendered_and_nothing_else() {
    let source = "# Market\n\nAbout **2 million** households. [Source](https://example.org/a) Growing fast.\n\n| Plan | Price |\n|---|---|\n| Free | 0 |\n";
    assert_eq!(readable("[a](b) and [c](d\ne)"), "[a] and [c](d\ne)");
    assert_eq!(
        words("About **2 million**, Ünïcode!"),
        ["about", "2", "million", "ünïcode"]
    );
    for selection in [
        "About 2 million households. Source Growing fast.",
        "MARKET",
        "Free 0",
        "households.\n  Source",
        "|---|",
    ] {
        assert!(selected_from(source, selection), "{selection}");
    }
    for selection in [
        "About 3 million",
        "Source https example",
        "fast. Plan Free",
        "million About",
        "~~~",
    ] {
        assert!(!selected_from(source, selection), "{selection}");
    }
    let list = |text: &str| -> Vec<String> { text.split(' ').map(str::to_owned).collect() };
    assert!(contains_words(&list("a a a b"), &list("a a b")));
    assert!(contains_words(&list("a b a b a c"), &list("a b a c")));
    assert!(!contains_words(&list("a b a b a b"), &list("a b a c")));
    assert!(!contains_words(&[], &list("a")));
}

#[tokio::test]
async fn a_selection_from_a_context_file_becomes_an_approved_project_memory() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = create_project(&mut owner, "mem-select", 1).await;
    let path = "context/research/market_research.md";
    let file = engine.folder("mem-select").join(path);
    std::fs::write(
        &file,
        "# Market\n\nAbout **2 million** households. [Source](https://example.org/a) Growing fast.\n",
    )
    .unwrap();
    let selection = "About 2 million households. Source Growing fast.";
    assert_eq!(
        result(
            owner
                .save_selection_to_memory(
                    project.clone(),
                    path.into(),
                    format!("  {selection}\n"),
                    2
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        result(
            owner
                .save_selection_to_memory(
                    project.clone(),
                    "context/brief.md".into(),
                    "Plans weekly meals".into(),
                    3
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied,
        "any text file in the project context can be the source"
    );
    let page = owner
        .memories(project_scope(&project), None, None)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|m| (m.text.as_str(), &m.origin))
            .collect::<Vec<_>>(),
        [
            (
                "Plans weekly meals",
                &MemoryOrigin::Selected {
                    path: "context/brief.md".into()
                }
            ),
            (selection, &MemoryOrigin::Selected { path: path.into() }),
        ]
    );
    let approved = owner.approved_memories(project.clone()).await.unwrap();
    assert_eq!(
        approved
            .items
            .iter()
            .map(|i| (i.scope, i.kind, i.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                ApprovedScope::Project,
                ApprovedKind::Selection,
                "Plans weekly meals"
            ),
            (ApprovedScope::Project, ApprovedKind::Selection, selection),
        ],
        "a saved selection is an owner action, so agents get it at once"
    );
    assert_eq!(
        result(
            owner
                .save_selection_to_memory(project.clone(), path.into(), selection.into(), 4)
                .await
                .unwrap()
        ),
        rejected("memory_duplicate")
    );

    std::fs::write(&file, "# Market\n\nThe numbers were revised.\n").unwrap();
    assert_eq!(
        result(
            owner
                .save_selection_to_memory(
                    project.clone(),
                    path.into(),
                    "About 2 million households.".into(),
                    5
                )
                .await
                .unwrap()
        ),
        rejected("memory_selection_stale"),
        "a file that changed since the owner read it is refused"
    );
    std::os::unix::fs::symlink(
        engine.root.path().join("global.toml"),
        engine
            .folder("mem-select")
            .join("context/documents/linked.md"),
    )
    .unwrap();
    for (bad, code) in [
        ("context/documents/absent.md", "context_item_missing"),
        ("context/documents/linked.md", "context_item_missing"),
        ("context/../global.toml", "context_path_invalid"),
    ] {
        assert_eq!(
            result(
                owner
                    .save_selection_to_memory(project.clone(), bad.into(), "timeout".into(), 6)
                    .await
                    .unwrap()
            ),
            rejected(code),
            "{bad}"
        );
    }
    assert_eq!(
        result(
            owner
                .save_selection_to_memory(
                    ProjectId::new("mem-absent").unwrap(),
                    path.into(),
                    "numbers".into(),
                    7
                )
                .await
                .unwrap()
        ),
        rejected("project_setup_missing")
    );
    for (bad_path, text, code) in [
        (
            "context/brief.md\n",
            "Plans".to_owned(),
            "context_path_invalid",
        ),
        ("", "Plans".to_owned(), "context_path_invalid"),
        (
            "context/brief.md",
            "n".repeat(NOTE_BYTES_MAX + 1),
            "memory_note_too_long",
        ),
        ("context/brief.md", "  ".to_owned(), "memory_note_blank"),
    ] {
        assert_eq!(
            owner
                .save_selection_to_memory(project.clone(), bad_path.into(), text, 8)
                .await
                .unwrap_err()
                .0,
            code
        );
    }
    assert_eq!(
        owner
            .save_selection_to_memory(
                project.clone(),
                "c".repeat(CONTEXT_PATH_BYTES_MAX + 1),
                "Plans".into(),
                8
            )
            .await
            .unwrap_err()
            .0,
        "context_path_invalid"
    );

    let saved = page.items[1].id.clone();
    assert_eq!(
        result(owner.delete_memory(saved.clone(), 9).await.unwrap()),
        OwnerResult::Applied
    );
    assert_eq!(owner.memory(saved).await.unwrap(), None);
    assert!(
        tables_holding(&engine.database(), selection).is_empty(),
        "a deleted selection leaves no copy in any engine table"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn research_text_with_hidden_characters_stored_before_the_check_is_never_kept_or_made_global()
{
    let mut engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let meals = create_project(&mut owner, "mem-hidden", next()).await;
    let report = engine
        .folder("mem-hidden")
        .join("context/research/market_research.md");
    std::fs::write(&report, "# Market\n\nHouseholds plan meals weekly.\n").unwrap();
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let clean = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(clean.len(), 1);
    let clean = clean[0].clone();
    let tags = "\u{E0049}\u{E0047}\u{E004E}\u{E004F}\u{E0052}\u{E0045}";
    std::fs::write(
        &report,
        format!("# Market\n\nHouseholds plan meals weekly.{tags}\n"),
    )
    .unwrap();
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        rejected("memory_text_hidden"),
        "new research text with hidden characters is refused at save"
    );
    drop(owner);

    let mut service = engine.service.take().unwrap();
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let config = crate::config::resolve_config(
        &engine.root.path().join("global.toml"),
        &engine.root.path().join("project.toml"),
    )
    .unwrap();
    let (pending_id, kept_id) = {
        let mut db = rusqlite::Connection::open_with_flags(
            engine.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        let tx = db.transaction().unwrap();
        let hidden =
            crate::project_setup::research_text(&tx, &meals, ResearchKind::MarketResearch, None)
                .unwrap()
                .unwrap();
        assert!(crate::memory::hidden_text(&hidden), "{hidden:?}");
        let written_before_the_check = |id: RequestId, status: KnowledgeStatus| KnowledgeEntry {
            id,
            version: 1,
            content_hash: crate::memory::hash(hidden.as_bytes()),
            text: hidden.clone(),
            human_reviewed: status == KnowledgeStatus::Confirmed,
            status,
            ..clean.clone()
        };
        let pending = written_before_the_check(
            crate::memory::research_entry_id(&meals, ResearchKind::MarketResearch.name(), &hidden)
                .unwrap(),
            KnowledgeStatus::Provisional,
        );
        let kept = written_before_the_check(
            crate::memory::research_entry_id(&meals, "kept-before-the-check", &hidden).unwrap(),
            KnowledgeStatus::Confirmed,
        );
        for entry in [&pending, &kept] {
            assert_eq!(
                crate::memory::save_entry(&tx, &config.settings.knowledge, entry).unwrap(),
                Outcome::Applied
            );
        }
        tx.commit().unwrap();
        (pending.id, kept.id)
    };
    engine.boot().await;
    let mut owner = engine.owner().await;

    assert_eq!(
        result(
            owner
                .remember_research(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        rejected("memory_text_hidden"),
        "a stored research entry with hidden characters is never kept"
    );
    assert_eq!(
        result(
            owner
                .promote_memory(meals.clone(), kept_id.clone(), 1, next())
                .await
                .unwrap()
        ),
        rejected("memory_note_invalid"),
        "a kept entry with hidden characters never reaches every project"
    );
    let entries = owner.project_memories(meals.clone(), None).await.unwrap();
    let pending = entries.iter().find(|entry| entry.id == pending_id).unwrap();
    assert_eq!(
        (&pending.status, pending.human_reviewed),
        (&KnowledgeStatus::Provisional, false)
    );
    assert_eq!(
        owner
            .memories(MemoryScope::Global, None, None)
            .await
            .unwrap()
            .items,
        []
    );
    assert!(
        !tables_holding(&engine.database(), tags).contains(&"memory_library".to_owned()),
        "no saved memory holds the hidden characters"
    );
    assert!(
        approved_direct(&engine, &meals)
            .items
            .iter()
            .all(|item| !crate::memory::hidden_text(&item.text)),
        "a kept entry saved before the check never reaches the project's agents"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn every_project_memory_read_for_counts_and_lists_reaches_past_one_engine_batch() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let meals = create_project(&mut owner, "mem-pages", next()).await;
    let headings: Vec<String> = (1..=11)
        .map(|index| format!("Finding {index:02}"))
        .collect();
    let report: String = headings
        .iter()
        .map(|heading| format!("## {heading}\n\nHouseholds note {heading}.\n\n"))
        .collect();
    std::fs::write(
        engine
            .folder("mem-pages")
            .join("context/research/market_research.md"),
        format!("# Market\n\n{report}"),
    )
    .unwrap();
    for heading in &headings {
        assert_eq!(
            result(
                owner
                    .save_research_to_memory(
                        meals.clone(),
                        ResearchKind::MarketResearch,
                        Some(heading.clone()),
                        next()
                    )
                    .await
                    .unwrap()
            ),
            OwnerResult::Applied,
            "{heading}"
        );
    }

    let first = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(
        first.len(),
        8,
        "one engine batch stops at the default batch size"
    );
    let all = owner.all_project_memories(meals.clone()).await.unwrap();
    assert_eq!(all.len(), headings.len(), "every suggestion is read");
    assert!(
        all.windows(2)
            .all(|pair| pair[0].id.as_str() < pair[1].id.as_str()),
        "batches continue after the last id without repeats"
    );
    assert_eq!(all[..first.len()], first[..]);
    assert!(
        all.iter()
            .all(|entry| entry.status == KnowledgeStatus::Provisional && !entry.human_reviewed)
    );
    drop(owner);
    engine.stop().await;
}

/// An IDE Approve or Reject on a waiting memory only asks: the memory stays waiting, the
/// desktop's list shows the newest ask with its decision, and the owner's own Keep at that
/// revision clears the list; an IDE decision on the reviewed revision is then stale.
#[tokio::test]
async fn an_ide_decision_waits_in_the_desktop_list_until_the_owner_reviews_it() {
    use crate::ide::bridge::memory::{Decide, decide};
    use crate::ide::bridge::{Decision, IdeReply};

    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let meals = create_project(&mut owner, "mem-ide", next()).await;
    std::fs::write(
        engine
            .folder("mem-ide")
            .join("context/research/market_research.md"),
        "# Market\n\nAbout 2 million households plan meals weekly.\n",
    )
    .unwrap();
    assert_eq!(
        result(
            owner
                .save_research_to_memory(meals.clone(), ResearchKind::MarketResearch, None, next())
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let entry = owner
        .project_memories(meals.clone(), None)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(entry.status, KnowledgeStatus::Provisional);
    assert!(owner.ide_asks(meals.clone()).await.unwrap().is_empty());

    let runtime = engine.service.as_ref().unwrap().owner_runtime().unwrap();
    let asked = |decision| Decide {
        id: entry.id.clone(),
        revision: entry.version,
        decision,
    };
    for decision in [Decision::Reject, Decision::Approve] {
        assert_eq!(
            decide(runtime, &meals, &asked(decision)).await.unwrap(),
            IdeReply::Ok {
                data: serde_json::json!({ "confirm": "desktop" }),
            }
        );
        let waiting = owner.ide_asks(meals.clone()).await.unwrap();
        assert_eq!(
            waiting
                .iter()
                .map(|ask| (ask.entry.id.clone(), ask.entry.version, ask.decision))
                .collect::<Vec<_>>(),
            [(entry.id.clone(), entry.version, decision)],
            "the newest IDE decision on a memory is the one the desktop asks"
        );
        let shown = owner.project_memories(meals.clone(), None).await.unwrap();
        assert_eq!(
            (shown[0].status.clone(), shown[0].version),
            (KnowledgeStatus::Provisional, entry.version),
            "an IDE decision writes no memory"
        );
    }

    assert_eq!(
        result(
            owner
                .review_memory(
                    meals.clone(),
                    entry.id.clone(),
                    entry.version,
                    ReviewDecision::Keep,
                    next()
                )
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert!(
        owner.ide_asks(meals.clone()).await.unwrap().is_empty(),
        "the owner's own review clears the ask"
    );
    let kept = owner.project_memories(meals.clone(), None).await.unwrap();
    assert_eq!(kept[0].status, KnowledgeStatus::Confirmed);
    assert!(kept[0].human_reviewed);
    assert!(matches!(
        decide(runtime, &meals, &asked(Decision::Reject))
            .await
            .unwrap(),
        IdeReply::Stale { .. }
    ));
    assert!(owner.ide_asks(meals.clone()).await.unwrap().is_empty());
    drop(owner);
    engine.stop().await;
}

fn import_db() -> (tempfile::TempDir, rusqlite::Connection, EffectiveConfig) {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("memory.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}'), ('beta','{}');",
    )
    .unwrap();
    crate::memory::initialize(&db).unwrap();
    let config =
        crate::config::resolve_config(&directory.path().join("g"), &directory.path().join("p"))
            .unwrap();
    (directory, db, config)
}

fn import_action(project: &str, file: &str, text: &str) -> LibraryAction {
    LibraryAction::Import {
        project_id: ProjectId::new(project).unwrap(),
        file: file.into(),
        text: text.into(),
    }
}

#[test]
fn import_saves_one_project_memory_per_file_and_refuses_reimport() {
    let (_directory, mut db, config) = import_db();
    let tx = db.transaction().unwrap();
    let text = "Plan meals on Sunday.";
    let first = import_action("alpha", "a.md", text);
    assert_eq!(execute(&tx, &config, &first, 1).unwrap(), Outcome::Applied);
    assert_eq!(
        execute(&tx, &config, &first, 2).unwrap(),
        Outcome::Rejected {
            code: "memory_duplicate".into()
        }
    );
    assert_eq!(
        execute(&tx, &config, &import_action("alpha", "b.md", text), 3).unwrap(),
        Outcome::Rejected {
            code: "memory_duplicate".into()
        },
        "the same text in the same project is a duplicate under another name"
    );
    assert_eq!(
        execute(&tx, &config, &import_action("absent", "a.md", text), 4).unwrap(),
        Outcome::Rejected {
            code: "memory_project_missing".into()
        }
    );
    let too_long = "t".repeat(TEXT_BYTES_MAX + 1);
    for (file, text) in [
        ("../x.md", text),
        ("a/b.md", text),
        ("x.txt", text),
        (".a.md", text),
        ("a.md", too_long.as_str()),
        ("a.md", " padded "),
        ("a.md", "a\u{202e}b"),
    ] {
        let action = import_action("alpha", file, text);
        assert_eq!(
            action.validate().unwrap_err().0,
            "memory_import_invalid",
            "{file}"
        );
        assert_eq!(
            execute(&tx, &config, &action, 5).unwrap(),
            Outcome::Rejected {
                code: "memory_import_invalid".into()
            },
            "{file}"
        );
    }
    let saved: Vec<(Option<String>, String, String)> = tx
        .prepare("SELECT project, origin, text FROM memory_library")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0.as_deref(), Some("alpha"));
    assert_eq!(
        serde_json::from_str::<MemoryOrigin>(&saved[0].1).unwrap(),
        MemoryOrigin::Imported {
            file: "a.md".into()
        }
    );
    assert_eq!(saved[0].2, text);
}

#[test]
fn an_imported_memory_reaches_agents_of_its_project_only_as_a_note() {
    let (_directory, mut db, config) = import_db();
    let tx = db.transaction().unwrap();
    let action = import_action("alpha", "a.md", "Plan meals on Sunday.");
    assert_eq!(execute(&tx, &config, &action, 1).unwrap(), Outcome::Applied);
    let alpha = approved_for_project(&tx, &ProjectId::new("alpha").unwrap()).unwrap();
    assert_eq!(
        alpha.items,
        vec![ApprovedMemory {
            scope: ApprovedScope::Project,
            kind: ApprovedKind::Note,
            text: "Plan meals on Sunday.".into(),
        }]
    );
    assert!(alpha.render().contains("Plan meals on Sunday."));
    let beta = approved_for_project(&tx, &ProjectId::new("beta").unwrap()).unwrap();
    assert!(beta.items.is_empty(), "{beta:?}");
}

fn memory_folder(files: &[(&str, &str)]) -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix("ca-mem-import-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    for (name, text) in files {
        std::fs::write(folder.path().join(name), text).unwrap();
    }
    folder
}

#[tokio::test]
async fn import_memories_counts_imported_and_skipped() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = create_project(&mut owner, "mem-import", 1).await;
    let response = owner
        .add_memory(project_scope(&project), "Shop on Monday.".into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let folder = memory_folder(&[
        ("a.md", "Plan meals on Sunday."),
        ("b.md", "---\ntitle: shop\n---\nShop on Monday."),
        ("c.md", "Cook twice a week."),
    ]);

    let first = owner
        .import_memories(project.clone(), folder.path(), 3)
        .await
        .unwrap();
    assert_eq!(
        first,
        ImportReport {
            imported: 2,
            skipped: 1
        }
    );
    let page = owner
        .memories(project_scope(&project), None, None)
        .await
        .unwrap();
    let mut imported: Vec<(String, String)> = page
        .items
        .iter()
        .filter_map(|memory| match &memory.origin {
            MemoryOrigin::Imported { file } => Some((file.clone(), memory.text.clone())),
            _ => None,
        })
        .collect();
    imported.sort();
    assert_eq!(
        imported,
        vec![
            ("a.md".into(), "Plan meals on Sunday.".into()),
            ("c.md".into(), "Cook twice a week.".into()),
        ]
    );
    assert_eq!(page.items.len(), 3);

    let second = owner
        .import_memories(project.clone(), folder.path(), 4)
        .await
        .unwrap();
    assert_eq!(
        second,
        ImportReport {
            imported: 0,
            skipped: 3
        }
    );
    assert_eq!(
        owner
            .import_memories(project, Path::new("relative"), 5)
            .await
            .unwrap_err()
            .0,
        "memory_import_folder_invalid"
    );
    engine.stop().await;
}

#[tokio::test]
async fn import_memories_stops_at_full() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = create_project(&mut owner, "mem-full", 1).await;
    let mut generation = 1;
    for index in 0..SCOPE_ITEMS_MAX - 1 {
        generation += 1;
        let response = owner
            .add_memory(project_scope(&project), format!("note {index}"), generation)
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied, "{index}");
    }
    let folder = memory_folder(&[
        ("a.md", "First imported."),
        ("b.md", "Second imported."),
        ("c.md", "Third imported."),
    ]);
    let report = owner
        .import_memories(project.clone(), folder.path(), generation + 1)
        .await
        .unwrap();
    assert_eq!(
        report,
        ImportReport {
            imported: 1,
            skipped: 2
        }
    );
    let page = owner
        .memories(project_scope(&project), Some("imported".into()), None)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|memory| memory.text.as_str())
            .collect::<Vec<_>>(),
        vec!["First imported."]
    );
    engine.stop().await;
}

fn agent_library() -> (tempfile::TempDir, rusqlite::Connection, EffectiveConfig) {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("memory.db")).unwrap();
    db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}'); INSERT INTO projects VALUES ('beta','{}');")
        .unwrap();
    crate::memory::initialize(&db).unwrap();
    let config =
        crate::config::resolve_config(&directory.path().join("g"), &directory.path().join("p"))
            .unwrap();
    (directory, db, config)
}

fn owner_change(
    db: &mut rusqlite::Connection,
    config: &EffectiveConfig,
    action: LibraryAction,
) -> Outcome {
    let tx = db.transaction().unwrap();
    let outcome = execute(&tx, config, &action, 1).unwrap();
    tx.commit().unwrap();
    outcome
}

fn memory_with_text(db: &mut rusqlite::Connection, text: &str) -> SavedMemory {
    let id: String = db
        .query_row("SELECT id FROM memory_library WHERE text=?1", [text], |r| {
            r.get(0)
        })
        .unwrap();
    let tx = db.transaction().unwrap();
    get(&tx, &RequestId::new(id).unwrap()).unwrap().unwrap()
}

fn recalled(db: &mut rusqlite::Connection, project: &ProjectId, query: &str) -> Vec<String> {
    recall(db, project, query)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|memory| memory.text)
        .collect()
}

fn approved_texts(db: &mut rusqlite::Connection, project: &ProjectId) -> Vec<String> {
    let tx = db.transaction().unwrap();
    approved_for_project(&tx, project)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.text)
        .collect()
}

#[test]
fn an_agent_memory_is_saved_at_once_or_held_for_the_owners_approval_by_the_setting() {
    let (_directory, mut db, config) = agent_library();
    let alpha = ProjectId::new("alpha").unwrap();
    let beta = ProjectId::new("beta").unwrap();
    assert_eq!(agent_writes(&db).unwrap(), AgentWrites::Save);

    assert_eq!(
        agent_remember(
            &mut db,
            &alpha,
            " Ada (@lead-a) ",
            false,
            "  Use cargo nextest.  "
        )
        .unwrap(),
        Ok(false)
    );
    let saved = memory_with_text(&mut db, "Use cargo nextest.");
    assert_eq!(
        saved.origin,
        MemoryOrigin::Agent {
            agent: "Ada (@lead-a)".into()
        }
    );
    assert_eq!(saved.scope, project_scope(&alpha));
    assert_eq!(recalled(&mut db, &alpha, "NEXTEST"), ["Use cargo nextest."]);
    assert!(
        recalled(&mut db, &beta, "").is_empty(),
        "a project memory stays in its project"
    );
    assert_eq!(approved_texts(&mut db, &alpha), ["Use cargo nextest."]);

    assert_eq!(
        owner_change(
            &mut db,
            &config,
            LibraryAction::AgentWrites {
                writes: AgentWrites::Ask
            }
        ),
        Outcome::Applied
    );
    assert_eq!(agent_writes(&db).unwrap(), AgentWrites::Ask);
    assert_eq!(
        agent_remember(
            &mut db,
            &alpha,
            "Master",
            true,
            "Run make check before commits."
        )
        .unwrap(),
        Ok(true)
    );
    let requested = memory_with_text(&mut db, "Run make check before commits.");
    assert_eq!(
        requested.origin,
        MemoryOrigin::Requested {
            agent: "Master".into()
        }
    );
    assert!(requested.origin.waiting());
    assert_eq!(requested.scope, MemoryScope::Global);
    assert_eq!(recalled(&mut db, &alpha, ""), ["Use cargo nextest."]);
    assert!(recalled(&mut db, &beta, "make check").is_empty());
    assert_eq!(approved_texts(&mut db, &alpha), ["Use cargo nextest."]);

    for (memory_id, code) in [
        (saved.id.clone(), "memory_not_requested"),
        (RequestId::new("agent-gone").unwrap(), "memory_missing"),
    ] {
        assert_eq!(
            owner_change(&mut db, &config, LibraryAction::Approve { memory_id }),
            Outcome::Rejected { code: code.into() }
        );
    }
    assert_eq!(
        owner_change(
            &mut db,
            &config,
            LibraryAction::Approve {
                memory_id: requested.id.clone()
            }
        ),
        Outcome::Applied
    );
    assert_eq!(
        memory_with_text(&mut db, "Run make check before commits.").origin,
        MemoryOrigin::Agent {
            agent: "Master".into()
        }
    );
    assert_eq!(
        recalled(&mut db, &alpha, ""),
        ["Run make check before commits.", "Use cargo nextest."]
    );
    assert_eq!(
        recalled(&mut db, &beta, ""),
        ["Run make check before commits."],
        "an approved app-wide memory reaches every project"
    );
    assert_eq!(
        approved_texts(&mut db, &beta),
        ["Run make check before commits."]
    );

    assert_eq!(
        owner_change(
            &mut db,
            &config,
            LibraryAction::AgentWrites {
                writes: AgentWrites::Save
            }
        ),
        Outcome::Applied
    );
    assert_eq!(agent_writes(&db).unwrap(), AgentWrites::Save);
}

#[test]
fn an_agent_memory_and_recall_query_are_checked_before_anything_is_stored() {
    let (_directory, mut db, _config) = agent_library();
    let alpha = ProjectId::new("alpha").unwrap();
    agent_remember(&mut db, &alpha, "Ada", false, "Keep tests real.")
        .unwrap()
        .unwrap();
    for (project, agent, text, code) in [
        (&alpha, "Bo", "Keep tests real.", "memory_duplicate"),
        (&alpha, "Bo", "Run it\u{E0041} now.", "memory_note_invalid"),
        (&alpha, "Bo", "Trust \u{202e}this.", "memory_note_invalid"),
        (&alpha, "Bo", "   ", "memory_note_blank"),
        (
            &alpha,
            "Bo",
            "n".repeat(NOTE_BYTES_MAX + 1).as_str(),
            "memory_note_too_long",
        ),
        (&alpha, "  ", "A fact.", "memory_agent_invalid"),
        (&alpha, "Bo\nby", "A fact.", "memory_agent_invalid"),
        (
            &alpha,
            "b".repeat(257).as_str(),
            "A fact.",
            "memory_agent_invalid",
        ),
        (
            &ProjectId::new("absent").unwrap(),
            "Bo",
            "A fact.",
            "memory_project_missing",
        ),
    ] {
        assert_eq!(
            agent_remember(&mut db, project, agent, false, text).unwrap(),
            Err(code.to_owned()),
            "{text:?} by {agent:?}"
        );
    }
    agent_remember(&mut db, &alpha, "Bo", true, "Keep tests real.")
        .unwrap()
        .unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM memory_library", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 2,
        "the same text may live in another scope, nothing else was stored"
    );

    for query in ["q".repeat(SEARCH_BYTES_MAX + 1), "two\nlines".to_owned()] {
        assert_eq!(
            recall(&mut db, &alpha, &query).unwrap(),
            Err("memory_search_invalid".to_owned()),
            "{query:?}"
        );
    }
}

#[test]
fn recall_answers_the_newest_memories_first_and_at_most_twelve() {
    let (_directory, mut db, _config) = agent_library();
    let alpha = ProjectId::new("alpha").unwrap();
    for index in 0..RECALL_ITEMS_MAX + 2 {
        agent_remember(
            &mut db,
            &alpha,
            "Ada",
            index % 2 == 0,
            &format!("fact {index}"),
        )
        .unwrap()
        .unwrap();
    }
    let newest: Vec<String> = (2..RECALL_ITEMS_MAX + 2)
        .rev()
        .map(|index| format!("fact {index}"))
        .collect();
    assert_eq!(recalled(&mut db, &alpha, "fact"), newest);
    assert_eq!(
        recalled(&mut db, &alpha, "fact 1"),
        ["fact 13", "fact 12", "fact 11", "fact 10", "fact 1"]
    );
}

#[test]
fn an_imported_project_memory_is_recalled_in_its_project_only() {
    let (_directory, mut db, config) = agent_library();
    let alpha = ProjectId::new("alpha").unwrap();
    let beta = ProjectId::new("beta").unwrap();
    assert_eq!(
        owner_change(
            &mut db,
            &config,
            LibraryAction::Import {
                project_id: alpha.clone(),
                file: "notes.md".into(),
                text: "Deploys go out on Tuesdays.".into(),
            }
        ),
        Outcome::Applied
    );
    let found = recall(&mut db, &alpha, "tuesdays").unwrap().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].origin,
        MemoryOrigin::Imported {
            file: "notes.md".into()
        }
    );
    assert!(recalled(&mut db, &beta, "").is_empty());
}
