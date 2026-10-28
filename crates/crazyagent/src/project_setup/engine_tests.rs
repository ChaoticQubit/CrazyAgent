use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::memory::{KnowledgeStatus, ReviewDecision};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// How long these engine tests let the engine and its owner client wait for
/// one answer. Every test engine in the lib test binary shares one
/// process-wide git setup lock, so parallel New Projects queue behind each
/// other's real git work; the 1 s owner deadline and a 2 s client wait then
/// time out although the engine is healthy. The same value as the TUI tests'
/// `test_home::ENGINE_WAIT`.
const ENGINE_WAIT: Duration = Duration::from_secs(10);

const CATALOG: &str = r#"
[model_access]
accounts = ["account-codex", "account-api"]
[[model_access.connections]]
id = "a-codex"
provider = "provider-codex"
harness = "codex"
protocol = "native"
account_ref = "account-codex"
endpoint = { kind = "native" }
model = { id = "model-codex", author = "author-codex" }
auth = { method = "native_managed", profile_ref = "profile-codex" }
[[model_access.connections]]
id = "b-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "none" }
[[model_access.pools]]
id = "pool-team"
members = ["a-codex", "b-api"]
projects = ["setup-pooled"]
roles = []
fallback_allowed = false
"#;

struct Engine {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-setup-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("timeout_ms = {}\n{CATALOG}", ENGINE_WAIT.as_millis()),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let home = tempfile::Builder::new()
            .prefix("ca-setup-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let engine = Self {
            root,
            home,
            service: Some(service),
            _stop: stop,
        };
        let mut owner = engine.owner().await;
        let response = owner
            .set_projects_home(engine.home().to_str().unwrap().into(), 1)
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied);
        engine
    }

    /// The canonical projects home where this engine creates projects.
    fn home(&self) -> PathBuf {
        std::fs::canonicalize(self.home.path()).unwrap()
    }

    async fn owner(&self) -> OwnerController {
        OwnerController::attach(self.root.path(), OwnerClientKind::Tui, ENGINE_WAIT)
            .await
            .unwrap()
    }

    fn rows(&self, sql: &str) -> i64 {
        rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
    }

    async fn stop(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

fn result(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } => operation.result,
        other => panic!("owner mutation must commit: {other:?}"),
    }
}

fn spec(name: &str) -> ProjectSpec {
    ProjectSpec {
        name: name.into(),
        repositories: vec![],
    }
}

#[tokio::test]
async fn new_project_creates_its_folder_documents_team_and_research_jobs() {
    let engine = Engine::start().await;
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("notes.md"), "# Notes\nKeep it small.\n").unwrap();
    std::fs::write(sources.path().join("pitch.pdf"), b"%PDF-1.4 pitch").unwrap();
    let mut owner = engine.owner().await;

    let catalog = owner
        .project_catalog(
            Some(CredentialRef::new("pool-team").unwrap()),
            vec![
                ResearchKind::MarketResearch,
                ResearchKind::CompetitorResearch,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        catalog.default_team.master, "codex",
        "codex has no sign-in in this engine, and RCLI never leads the default team"
    );
    assert_eq!(catalog.default_team.sub_harnesses[0].harness, "codex");
    assert_eq!(catalog.default_team.sub_harnesses[0].agents, 2);
    assert_eq!(
        catalog.default_team_status, None,
        "Codex can run on the pool's keyless endpoint"
    );
    assert_eq!(
        owner
            .project_catalog(Some(CredentialRef::new("pool-absent").unwrap()), vec![])
            .await
            .unwrap_err()
            .0,
        "pool_missing"
    );

    let project = ProjectId::new("setup-pooled").unwrap();
    let setup = ProjectSetup {
        about: "Plans weekly meals.".into(),
        tech_stack: vec!["typescript".into(), "Home-grown queue".into()],
        platforms: vec![Platform::Website, Platform::MobileApp],
        research: vec![
            ResearchKind::MarketResearch,
            ResearchKind::CompetitorResearch,
        ],
        pool: Some(CredentialRef::new("pool-team").unwrap()),
        team: None,
        documents: vec![
            sources.path().join("notes.md").to_str().unwrap().into(),
            sources.path().join("pitch.pdf").to_str().unwrap().into(),
        ],
    };
    let response = owner
        .setup_project(project.clone(), spec("Meal planner"), setup.clone(), 1)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);

    let project_folder = engine.home().join("meal-planner");
    let folder = project_folder.join(".crazyagent");
    let brief = std::fs::read_to_string(folder.join("context/brief.md")).unwrap();
    for expected in [
        "# Meal planner",
        "Plans weekly meals.",
        "- typescript",
        "- Home-grown queue",
        "- Mobile app",
        "- Market research -> context/research/market_research.md",
        "- context/documents/pitch.pdf",
    ] {
        assert!(brief.contains(expected), "{expected}\n{brief}");
    }
    assert_eq!(
        std::fs::read(folder.join("context/documents/pitch.pdf")).unwrap(),
        b"%PDF-1.4 pitch"
    );
    assert!(folder.join("context/research").is_dir());
    assert!(folder.join("worktrees").is_dir());
    let mode = std::fs::metadata(&project_folder)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o700, 0o700);
    assert_eq!(
        mode & 0o022,
        0,
        "only the owner may write the project folder"
    );
    assert!(!project_folder.join("context").exists());

    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(stored.folder, project_folder.to_str().unwrap());
    assert_eq!(stored.team.master, "codex");
    assert!(stored.team.master_tools);
    assert_eq!(stored.documents.len(), 2);

    let jobs = owner.research_jobs(project.clone()).await.unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (
                job.kind_label.as_str(),
                job.status_label.as_str(),
                job.harness.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("Market research", "Queued", "codex"),
            ("Competitor research", "Queued", "codex")
        ]
    );

    let unpooled = ProjectId::new("setup-unpooled").unwrap();
    let response = owner
        .setup_project(
            unpooled.clone(),
            spec("Waiting project"),
            ProjectSetup {
                research: vec![ResearchKind::DesignResearch],
                pool: None,
                documents: vec![],
                ..setup.clone()
            },
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let waiting = owner.research_jobs(unpooled.clone()).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].status, ResearchStatus::NeedsPool);
    assert_eq!(waiting[0].status_label, "Needs a pool");

    let duplicate = owner
        .setup_project(project.clone(), spec("Again"), setup.clone(), 3)
        .await
        .unwrap();
    assert_eq!(
        result(duplicate),
        OwnerResult::Rejected {
            code: "project_exists".into()
        }
    );
    std::os::unix::fs::symlink(
        sources.path().join("notes.md"),
        sources.path().join("linked.md"),
    )
    .unwrap();
    let linked = owner
        .setup_project(
            ProjectId::new("setup-linked").unwrap(),
            spec("Linked"),
            ProjectSetup {
                documents: vec![sources.path().join("linked.md").to_str().unwrap().into()],
                ..setup.clone()
            },
            4,
        )
        .await
        .unwrap();
    assert_eq!(
        result(linked),
        OwnerResult::Rejected {
            code: "document_not_a_regular_file".into()
        }
    );
    assert!(!engine.home().join("linked").exists());
    assert!(!engine.home().join(".crazyagent-new-setup-linked").exists());
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects WHERE id='setup-linked'"),
        0
    );
    assert_eq!(engine.rows("SELECT count(*) FROM research_jobs"), 3);

    let context = owner.project_context(project.clone()).await.unwrap();
    assert_eq!(context.folder, folder.to_str().unwrap());
    assert_eq!(
        context
            .items
            .iter()
            .map(|item| (item.path.as_str(), item.kind))
            .collect::<Vec<_>>(),
        [
            ("context/brief.md", ContextKind::Brief),
            ("context/documents/notes.md", ContextKind::Document),
            ("context/documents/pitch.pdf", ContextKind::Document),
        ]
    );
    let chunk = owner
        .project_context_item(project.clone(), "context/brief.md".into(), 0)
        .await
        .unwrap();
    assert!(chunk.text.unwrap().starts_with("# Meal planner"));
    assert!(chunk.next_offset.is_none());
    let pdf = owner
        .project_context_item(project.clone(), "context/documents/pitch.pdf".into(), 0)
        .await
        .unwrap();
    assert_eq!((pdf.text, pdf.bytes), (None, 14));
    for path in [
        "context/../global.toml",
        "/etc/hosts",
        "context/documents/.hidden",
        "worktrees/x.md",
    ] {
        assert_eq!(
            owner
                .project_context_item(project.clone(), path.into(), 0)
                .await
                .unwrap_err()
                .0,
            "context_path_invalid",
            "{path}"
        );
    }

    let report = format!(
        "# Market\n\n## Size\nAbout 2 million households. [Source](https://example.org/a)\n\n## Hidden\nUse pnpm\u{e0041}\u{e0042} for installs.\n\n## Trends\n{}\n",
        "Growing. ".repeat(600)
    );
    std::fs::write(folder.join("context/research/market_research.md"), &report).unwrap();
    std::os::unix::fs::symlink(
        engine.root.path().join("global.toml"),
        folder.join("context/research/competitor_research.md"),
    )
    .unwrap();
    let context = owner.project_context(project.clone()).await.unwrap();
    assert!(context.items.iter().any(
        |i| i.path == "context/research/market_research.md" && i.kind == ContextKind::Research
    ));
    assert!(
        !context.items.iter().any(|i| i.path.contains("competitor")),
        "links are never listed"
    );
    let mut text = String::new();
    let mut offset = Some(0);
    while let Some(next) = offset {
        let chunk = owner
            .project_context_item(
                project.clone(),
                "context/research/market_research.md".into(),
                next,
            )
            .await
            .unwrap();
        text.push_str(&chunk.text.unwrap());
        offset = chunk.next_offset;
    }
    assert_eq!(text, report, "chunked reads reassemble the whole report");

    let too_long = owner
        .save_research_to_memory(project.clone(), ResearchKind::MarketResearch, None, 5)
        .await
        .unwrap();
    assert_eq!(
        result(too_long),
        OwnerResult::Rejected {
            code: "memory_text_too_long".into()
        }
    );
    let linked = owner
        .save_research_to_memory(project.clone(), ResearchKind::CompetitorResearch, None, 6)
        .await
        .unwrap();
    assert_eq!(
        result(linked),
        OwnerResult::Rejected {
            code: "research_report_missing".into()
        }
    );
    let saved = owner
        .save_research_to_memory(
            project.clone(),
            ResearchKind::MarketResearch,
            Some("Size".into()),
            7,
        )
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    let entries = owner.project_memories(project.clone(), None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, KnowledgeStatus::Provisional);
    assert!(
        entries[0]
            .text
            .starts_with("## Size\nAbout 2 million households.")
    );
    let entry = entries[0].id.clone();
    assert_eq!(
        result(
            owner
                .review_memory(project.clone(), entry, 1, ReviewDecision::Keep, 8)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        owner
            .project_memories(project.clone(), None)
            .await
            .unwrap()
            .iter()
            .map(|entry| (&entry.status, entry.human_reviewed))
            .collect::<Vec<_>>(),
        [(&KnowledgeStatus::Confirmed, true)]
    );
    let status: String = rusqlite::Connection::open_with_flags(
        engine.root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT json_extract(e.body,'$.status') FROM knowledge_entries e JOIN knowledge_heads h ON e.id=h.id AND e.version=h.version",
        [],
        |row| row.get(0),
    )
    .unwrap();
    assert_eq!(status, "Confirmed", "one owner click confirms the memory");
    let again = owner
        .save_research_to_memory(
            project.clone(),
            ResearchKind::MarketResearch,
            Some("Size".into()),
            9,
        )
        .await
        .unwrap();
    assert_eq!(
        result(again),
        OwnerResult::Rejected {
            code: "memory_entry_exists".into()
        }
    );
    let hidden = owner
        .save_research_to_memory(
            project.clone(),
            ResearchKind::MarketResearch,
            Some("Hidden".into()),
            10,
        )
        .await
        .unwrap();
    assert_eq!(
        result(hidden),
        OwnerResult::Rejected {
            code: "memory_text_hidden".into()
        }
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn owner_adds_documents_and_deletes_research_and_documents_through_the_engine() {
    let engine = Engine::start().await;
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("first.md"), "first").unwrap();
    std::fs::write(sources.path().join("later.txt"), "later notes").unwrap();
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-editable").unwrap();
    let setup = ProjectSetup {
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![sources.path().join("first.md").to_str().unwrap().into()],
    };
    assert_eq!(
        result(
            owner
                .setup_project(project.clone(), spec("Editable"), setup, 1)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let folder = owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .state_folder();

    let added = owner
        .add_project_documents(
            project.clone(),
            vec![sources.path().join("later.txt").to_str().unwrap().into()],
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(added), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        stored
            .documents
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["first.md", "later.txt"]
    );
    let brief = std::fs::read_to_string(folder.join("context/brief.md")).unwrap();
    assert!(brief.contains("- context/documents/later.txt"), "{brief}");
    let duplicate = owner
        .add_project_documents(
            project.clone(),
            vec![sources.path().join("first.md").to_str().unwrap().into()],
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        result(duplicate),
        OwnerResult::Rejected {
            code: "document_name_duplicate".into()
        }
    );
    assert_eq!(
        std::fs::read(folder.join("context/documents/first.md")).unwrap(),
        b"first"
    );

    std::fs::write(
        folder.join("context/research/market_research.md"),
        "# Market\n",
    )
    .unwrap();
    for (path, generation) in [
        ("context/research/market_research.md", 4),
        ("context/documents/first.md", 5),
    ] {
        let deleted = owner
            .delete_project_context(project.clone(), path.into(), generation)
            .await
            .unwrap();
        assert_eq!(result(deleted), OwnerResult::Applied, "{path}");
        assert!(!folder.join(path).exists(), "{path}");
    }
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        stored
            .documents
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["later.txt"]
    );
    let brief = std::fs::read_to_string(folder.join("context/brief.md")).unwrap();
    assert!(!brief.contains("first.md"), "{brief}");
    assert!(brief.contains("- context/documents/later.txt"), "{brief}");
    for (path, code, generation) in [
        ("context/brief.md", "context_item_protected", 6),
        ("context/documents/first.md", "context_item_missing", 7),
    ] {
        let refused = owner
            .delete_project_context(project.clone(), path.into(), generation)
            .await
            .unwrap();
        assert_eq!(
            result(refused),
            OwnerResult::Rejected { code: code.into() },
            "{path}"
        );
    }
    std::fs::create_dir(folder.join("context/research/nested.md")).unwrap();
    let directory = owner
        .delete_project_context(project.clone(), "context/research/nested.md".into(), 8)
        .await
        .unwrap();
    assert_eq!(
        result(directory),
        OwnerResult::Rejected {
            code: "context_item_missing".into()
        }
    );
    assert!(folder.join("context/research/nested.md").is_dir());
    let outside = sources.path().join("outside.md");
    std::fs::write(&outside, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("context/research/linked.md")).unwrap();
    let linked = owner
        .delete_project_context(project.clone(), "context/research/linked.md".into(), 9)
        .await
        .unwrap();
    assert_eq!(
        result(linked),
        OwnerResult::Rejected {
            code: "context_item_missing".into()
        }
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert!(folder.join("context/brief.md").is_file());
    drop(owner);
    engine.stop().await;
}

fn pooled_setup(research: Vec<ResearchKind>) -> ProjectSetup {
    ProjectSetup {
        about: "Plans weekly meals.".into(),
        tech_stack: vec!["typescript".into()],
        platforms: vec![Platform::Website],
        research,
        pool: Some(CredentialRef::new("pool-team").unwrap()),
        team: None,
        documents: vec![],
    }
}

fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

#[tokio::test]
async fn owner_updates_a_project_setup_only_from_the_revision_it_read() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert_eq!(
        owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    let market = owner.research_jobs(project.clone()).await.unwrap()[0].id;

    let solo = TeamPlan {
        master: "codex".into(),
        master_tools: false,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    };
    let edited = ProjectSetup {
        about: "Plans meals for two.".into(),
        tech_stack: vec![" rust ".into()],
        platforms: vec![Platform::Cli],
        team: Some(solo.clone()),
        ..pooled_setup(vec![ResearchKind::CompetitorResearch])
    };
    let response = owner
        .update_project_setup(
            project.clone(),
            1,
            Some("Meals for two".into()),
            edited.clone(),
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.name, "Meals for two");
    assert_eq!(stored.about, "Plans meals for two.");
    assert_eq!(stored.tech_stack, ["rust"]);
    assert_eq!(stored.platforms, [Platform::Cli]);
    assert_eq!(stored.research, [ResearchKind::CompetitorResearch]);
    assert_eq!(stored.team, solo);
    assert_eq!(
        engine.rows(
            "SELECT count(*) FROM projects WHERE id='setup-pooled'
            AND json_extract(body,'$.spec.name')='Meals for two'"
        ),
        1
    );
    let brief = std::fs::read_to_string(stored.state_folder().join("context/brief.md")).unwrap();
    assert!(brief.contains("# Meals for two"), "{brief}");
    assert!(brief.contains("Plans meals for two."), "{brief}");
    let jobs = owner.research_jobs(project.clone()).await.unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind, job.status, job.cancel_requested))
            .collect::<Vec<_>>(),
        [
            (
                ResearchKind::MarketResearch,
                ResearchStatus::Cancelled,
                true
            ),
            (
                ResearchKind::CompetitorResearch,
                ResearchStatus::Queued,
                false
            ),
        ]
    );
    assert_eq!(jobs[0].id, market);
    assert_eq!((jobs[1].harness.as_str(), jobs[1].tools), ("codex", false));

    let stale = owner
        .update_project_setup(project.clone(), 1, None, edited.clone(), 3)
        .await
        .unwrap();
    assert_eq!(result(stale), rejected("stale_revision"));
    let both = ProjectSetup {
        research: vec![
            ResearchKind::MarketResearch,
            ResearchKind::CompetitorResearch,
        ],
        ..edited.clone()
    };
    let response = owner
        .update_project_setup(project.clone(), 2, None, both, 4)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let jobs = owner.research_jobs(project.clone()).await.unwrap();
    assert_eq!(jobs.len(), 2, "research turned back on reuses its job");
    assert_eq!(
        (jobs[0].id, jobs[0].status, jobs[0].harness.as_str()),
        (market, ResearchStatus::Queued, "codex")
    );
    assert!(!jobs[0].cancel_requested);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        (stored.revision, stored.name.as_str()),
        (3, "Meals for two")
    );

    let absent = ProjectSetup {
        pool: Some(CredentialRef::new("pool-absent").unwrap()),
        ..edited.clone()
    };
    let missing = owner
        .update_project_setup(project.clone(), 3, None, absent, 5)
        .await
        .unwrap();
    assert_eq!(result(missing), rejected("pool_missing"));
    let with_documents = SetupAction::Update {
        project_id: project.clone(),
        expected_revision: 3,
        name: None,
        setup: ProjectSetup {
            documents: vec!["/private/tmp/notes.md".into()],
            ..edited.clone()
        },
    };
    assert_eq!(
        with_documents.validate().unwrap_err().0,
        "project_setup_bounds"
    );
    let blank_name = SetupAction::Update {
        project_id: project.clone(),
        expected_revision: 3,
        name: Some(" ".into()),
        setup: edited,
    };
    assert_eq!(blank_name.validate().unwrap_err().0, "project_spec_bounds");
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn team_plan_writes_refuse_harnesses_that_cannot_run_team_agents_but_saved_plans_read() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let opencode = TeamPlan {
        master: "opencode".into(),
        master_tools: true,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    };
    let refused = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            ProjectSetup {
                team: Some(opencode),
                ..pooled_setup(vec![])
            },
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(refused), rejected("team_harness_unavailable"));
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects WHERE id='setup-pooled'"),
        0
    );
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let written_before = rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute(
            "UPDATE project_setups SET body=json_set(body,'$.team.master','opencode')
            WHERE project='setup-pooled'",
            [],
        )
        .unwrap();
    assert_eq!(written_before, 1);

    let saved = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        saved.team.master, "opencode",
        "a saved plan still reads back"
    );
    let status = owner.team_status(project.clone()).await.unwrap();
    assert_eq!(
        (
            status.harnesses[0].harness.as_str(),
            status.harnesses[0].runnable
        ),
        ("opencode", false)
    );
    let renamed = owner
        .update_project_setup(
            project.clone(),
            saved.revision,
            Some("Meals".into()),
            ProjectSetup {
                team: Some(saved.team.clone()),
                ..pooled_setup(vec![])
            },
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        result(renamed),
        OwnerResult::Applied,
        "an unchanged saved plan still saves"
    );
    let added = TeamPlan {
        sub_harnesses: vec![HarnessSlot {
            harness: "gemini-cli".into(),
            agents: 1,
            tools: true,
        }],
        max_agents: 2,
        ..saved.team.clone()
    };
    let refused = owner
        .update_project_setup(
            project.clone(),
            saved.revision + 1,
            Some("Other".into()),
            ProjectSetup {
                team: Some(added),
                ..pooled_setup(vec![])
            },
            4,
        )
        .await
        .unwrap();
    assert_eq!(result(refused), rejected("team_harness_unavailable"));
    let kept = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        (
            kept.revision,
            kept.name.as_str(),
            kept.team.sub_harnesses.len()
        ),
        (saved.revision + 1, "Meals", 0)
    );
    let codex = TeamPlan {
        master: "codex".into(),
        ..kept.team.clone()
    };
    let fixed = owner
        .update_project_setup(
            project.clone(),
            kept.revision,
            None,
            ProjectSetup {
                team: Some(codex),
                ..pooled_setup(vec![])
            },
            5,
        )
        .await
        .unwrap();
    assert_eq!(result(fixed), OwnerResult::Applied);
    let status = owner.team_status(project).await.unwrap();
    assert!(status.harnesses[0].runnable);
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn owner_stops_and_retries_research_and_reads_its_plain_log() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![
                ResearchKind::MarketResearch,
                ResearchKind::CompetitorResearch,
            ]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let jobs = owner.research_jobs(project.clone()).await.unwrap();
    let (first, second) = (jobs[0].id, jobs[1].id);

    let cancelled = owner.cancel_research(project.clone(), first, 2).await;
    assert_eq!(result(cancelled.unwrap()), OwnerResult::Applied);
    let again = owner.cancel_research(project.clone(), first, 3).await;
    assert_eq!(result(again.unwrap()), rejected("research_job_finished"));
    let waiting = owner.retry_research(project.clone(), second, 4).await;
    assert_eq!(
        result(waiting.unwrap()),
        rejected("research_job_not_retryable")
    );
    let unknown = owner.cancel_research(project.clone(), 9999, 5).await;
    assert_eq!(result(unknown.unwrap()), rejected("research_job_missing"));
    let retried = owner.retry_research(project.clone(), first, 6).await;
    assert_eq!(result(retried.unwrap()), OwnerResult::Applied);
    let job = owner
        .research_jobs(project.clone())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (job.id, job.status, job.attempts, job.cancel_requested),
        (first, ResearchStatus::Queued, 0, false)
    );

    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let claimed = store
        .claim("setup-pooled", "runner-test", 4)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, first);
    let stopping = owner.cancel_research(project.clone(), first, 7).await;
    assert_eq!(result(stopping.unwrap()), OwnerResult::Applied);
    let job = owner
        .research_jobs(project.clone())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (job.status, job.cancel_requested),
        (ResearchStatus::Running, true)
    );
    assert!(
        store.heartbeat(&claimed, 64).unwrap(),
        "the runner's heartbeat sees the stop and kills the agent's process group"
    );
    store.cancelled(&claimed).unwrap();
    assert_eq!(
        owner.research_jobs(project.clone()).await.unwrap()[0].status,
        ResearchStatus::Cancelled
    );

    assert!(
        owner
            .research_log(project.clone(), second)
            .await
            .unwrap()
            .is_empty()
    );
    let agents = engine.root.path().join("research/agents");
    let older = agents.join(format!("job-{first}-1"));
    let newest = agents.join(format!("job-{first}-1-1"));
    for run in [&older, &newest] {
        std::fs::create_dir_all(run).unwrap();
    }
    std::fs::write(older.join("stderr.log"), "old run\n").unwrap();
    std::fs::write(
        newest.join("events.jsonl"),
        concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Found three competitors.\"}}\n",
            "{\"type\":\"error\",\"message\":\"Rate limited, waiting\"}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        newest.join("stderr.log"),
        "warning:\tslow network\nAuthorization: Bearer abc123\nusing key sk-proj-ABCDEFGHIJKLMNOPQRSTUVWX now\n",
    )
    .unwrap();
    let lines = owner.research_log(project.clone(), first).await.unwrap();
    assert_eq!(
        lines,
        [
            "Found three competitors.",
            "Rate limited, waiting",
            "warning: slow network",
            "[hidden: this line may contain a secret]",
            "using key [hidden] now",
        ]
    );

    let busy = agents.join(format!("job-{second}-2"));
    std::fs::create_dir_all(&busy).unwrap();
    let steps: String = (0..500).map(|n| format!("step {n}\n")).collect();
    std::fs::write(busy.join("stderr.log"), steps).unwrap();
    let lines = owner.research_log(project.clone(), second).await.unwrap();
    assert_eq!(lines.len(), RESEARCH_LOG_LINES_MAX);
    assert_eq!(
        (
            lines[0].as_str(),
            lines[RESEARCH_LOG_LINES_MAX - 1].as_str()
        ),
        ("step 300", "step 499")
    );
    let wide = agents.join(format!("job-{second}-3"));
    std::fs::create_dir_all(&wide).unwrap();
    let mut output = format!("{}\n", "x".repeat(RESEARCH_LOG_LINE_CHARS_MAX)).repeat(100);
    output.push_str(&"y".repeat(500));
    std::fs::write(wide.join("output.md"), output).unwrap();
    let lines = owner.research_log(project.clone(), second).await.unwrap();
    assert_eq!(lines.len(), 61, "the newest lines that fit one owner frame");
    assert!(lines.iter().map(String::len).sum::<usize>() <= RESEARCH_LOG_BYTES_MAX);
    assert_eq!(
        lines.last().unwrap(),
        &format!("{}...", "y".repeat(RESEARCH_LOG_LINE_CHARS_MAX))
    );
    assert_eq!(
        owner
            .research_log(project.clone(), 9999)
            .await
            .unwrap_err()
            .0,
        "research_job_missing"
    );
    drop(owner);
    engine.stop().await;
}

/// Reads one project's newest research job of a kind. Inputs: the owner client, the project
/// and the kind. Output: the job.
async fn job_of(
    owner: &mut OwnerController,
    project: &ProjectId,
    kind: ResearchKind,
) -> ResearchJob {
    owner
        .research_jobs(project.clone())
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find(|job| job.kind == kind)
        .expect("the project has a job of this kind")
}

#[tokio::test]
async fn a_retried_job_whose_research_was_turned_off_is_not_cancelled_by_the_next_save() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let planning = ResearchKind::RoadmapPlanning;
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let planned = owner.plan_roadmap(project.clone(), 2).await.unwrap();
    assert_eq!(result(planned), OwnerResult::Applied);
    let saved = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(saved.revision, 2);
    assert!(saved.research.contains(&planning));

    let mut off = saved.editable();
    off.research = vec![ResearchKind::MarketResearch];
    let response = owner
        .update_project_setup(project.clone(), 2, None, off, 3)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let before_retry = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(before_retry.revision, 3);
    assert!(!before_retry.research.contains(&planning));
    let job = job_of(&mut owner, &project, planning).await;
    assert_eq!(job.status, ResearchStatus::Cancelled);

    let retried = owner.retry_research(project.clone(), job.id, 4).await;
    assert_eq!(result(retried.unwrap()), OwnerResult::Applied);
    let after_retry = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        after_retry.revision, 4,
        "turning the kind back on raises the revision"
    );
    assert_eq!(
        after_retry.research,
        [ResearchKind::MarketResearch, planning],
        "the retried kind is on again"
    );
    let queued = job_of(&mut owner, &project, planning).await;
    assert_eq!(
        (queued.id, queued.status, queued.cancel_requested),
        (job.id, ResearchStatus::Queued, false)
    );

    let mut drafted_before = before_retry.editable();
    drafted_before.about = "Plans meals for two.".into();
    let stale = owner
        .update_project_setup(project.clone(), 3, None, drafted_before, 5)
        .await
        .unwrap();
    assert_eq!(
        result(stale),
        rejected("stale_revision"),
        "an edit drafted before the retry cannot turn the kind off unseen"
    );
    let mut fresh = after_retry.editable();
    fresh.about = "Plans meals for two.".into();
    let response = owner
        .update_project_setup(project.clone(), 4, None, fresh, 6)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let kept = job_of(&mut owner, &project, planning).await;
    assert_eq!(
        (kept.id, kept.status, kept.cancel_requested),
        (job.id, ResearchStatus::Queued, false),
        "the next save keeps the retried job"
    );

    let market = job_of(&mut owner, &project, ResearchKind::MarketResearch).await;
    let stopped = owner.cancel_research(project.clone(), market.id, 7).await;
    assert_eq!(result(stopped.unwrap()), OwnerResult::Applied);
    let retried = owner.retry_research(project.clone(), market.id, 8).await;
    assert_eq!(result(retried.unwrap()), OwnerResult::Applied);
    assert_eq!(
        owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap()
            .revision,
        5,
        "a kind still on keeps the revision"
    );
    drop(owner);
    engine.stop().await;
}

/// Releasing an account cancels the research job and closes the master conversation that
/// hold it, does not fail on a holder whose conversation already ended, and reads the
/// account free once the remaining holds end.
#[tokio::test]
async fn releasing_an_account_cancels_its_research_and_closes_its_master() {
    use crate::agents::Holder;
    use crate::protocol::{CredentialRef, SessionId};
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let job = owner.research_jobs(project.clone()).await.unwrap()[0].id;
    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let claimed = store
        .claim("setup-pooled", "runner-test", 4)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, job);
    let agents = engine
        .service
        .as_ref()
        .unwrap()
        .owner_runtime()
        .unwrap()
        .agents()
        .unwrap();
    let account = CredentialRef::new("a-codex").unwrap();
    let research = Holder::Research {
        project: project.clone(),
        job,
        title: "Market research".into(),
    };
    let ended = Holder::Master {
        project: project.clone(),
        session: SessionId::new("master-ended").unwrap(),
    };
    let held = agents.hold(account.clone(), research.clone());
    let closed = agents.hold(account.clone(), ended.clone());

    let used = owner.account_use(account.clone()).await.unwrap();
    assert!(used.agents.is_empty());
    assert_eq!(used.holders, vec![ended.clone(), research]);

    let runner = std::thread::spawn(move || {
        let asked = std::time::Instant::now();
        while !store.heartbeat(&claimed, 0).unwrap() {
            assert!(
                asked.elapsed() < Duration::from_secs(10),
                "no cancel reached the job"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(held);
    });
    let left = owner
        .release_within(account.clone(), 2, Duration::from_secs(1))
        .await
        .unwrap();
    runner.join().unwrap();
    assert!(left.agents.is_empty());
    assert_eq!(left.holders, vec![ended]);
    drop(closed);
    assert!(owner.account_use(account).await.unwrap().is_free());
    let job = owner.research_jobs(project).await.unwrap().remove(0);
    assert!(job.cancel_requested);
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn owner_removes_a_project_and_keeps_its_folder() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let folder = owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .folder;
    assert_eq!(
        engine.rows("SELECT count(*) FROM research_jobs WHERE project='setup-pooled'"),
        1
    );
    rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO team_owner_prompts(project,agent,task,harness,text,created_ms)
            VALUES ('setup-pooled',1,NULL,'codex','Check the login page',0);
            INSERT INTO team_agent_conversations(agent,project,harness,connection,conversation,record)
            VALUES (1,'setup-pooled','codex','connection-1','conversation-1','/tmp/record.jsonl');",
        )
        .unwrap();

    let removed = owner.remove_project(project.clone(), 2).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects WHERE id='setup-pooled'"),
        0
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM project_setups WHERE project='setup-pooled'"),
        0
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM research_jobs WHERE project='setup-pooled'"),
        0,
        "the project's research jobs go with it"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM team_owner_prompts WHERE project='setup-pooled'"),
        0,
        "the owner's prompts to its agents go with it"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM team_agent_conversations WHERE project='setup-pooled'"),
        0,
        "its agents' kept conversations go with it"
    );
    assert!(
        Path::new(&folder)
            .join(".crazyagent/context/brief.md")
            .is_file(),
        "the project folder stays where it was"
    );
    assert!(
        owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .is_none()
    );
    let again = owner.remove_project(project.clone(), 3).await.unwrap();
    assert_eq!(result(again), rejected("invalid_project"));
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn owner_saves_checked_read_only_folders_and_refuses_unsafe_ones() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![],
            version: 0
        }
    );

    let shared = tempfile::Builder::new()
        .prefix("ca-read-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let real = std::fs::canonicalize(shared.path()).unwrap();
    let docs = real.join("docs");
    std::fs::create_dir(&docs).unwrap();
    let alias = real.join("alias");
    std::os::unix::fs::symlink(&docs, &alias).unwrap();
    let file = real.join("notes.txt");
    std::fs::write(&file, "notes").unwrap();
    let text = |path: &Path| path.to_str().unwrap().to_owned();

    let saved = owner
        .set_project_read_paths(project.clone(), 0, vec![text(&alias), text(&docs)], 2)
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![text(&docs)],
            version: 1
        },
        "a link is saved as the folder it names, once"
    );

    let owner_home = crate::sandbox::toolchains::ToolHomes::owner()
        .unwrap()
        .home()
        .to_path_buf();
    let probe = engine.root.path().join("read-probe");
    std::fs::create_dir(&probe).unwrap();
    let refusals = [
        ("docs".to_owned(), "read_path_not_absolute"),
        (text(&real.join("gone")), "read_path_missing"),
        (text(&file), "read_path_not_folder"),
        (text(&owner_home), "read_path_holds_home"),
        (text(owner_home.parent().unwrap()), "read_path_holds_home"),
        ("/usr/local".to_owned(), "read_path_credentials"),
        (text(&probe), "read_path_engine_data"),
        (
            text(&std::fs::canonicalize(engine.root.path()).unwrap()),
            "read_path_engine_data",
        ),
    ]
    .into_iter()
    .chain(
        cfg!(target_os = "macos")
            .then(|| (text(&owner_home.join("Library")), "read_path_credentials")),
    );
    let mut generation = 3;
    for (typed, code) in refusals {
        let refused = owner
            .set_project_read_paths(
                project.clone(),
                1,
                vec![text(&docs), typed.clone()],
                generation,
            )
            .await
            .unwrap();
        assert_eq!(result(refused), rejected(code), "{typed}");
        generation += 1;
    }
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![text(&docs)],
            version: 1
        },
        "a refused change keeps the saved folders and their version"
    );

    let removed = owner
        .remove_project(project.clone(), generation + 1)
        .await
        .unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert_eq!(
        engine.rows("SELECT count(*) FROM project_read_paths WHERE project='setup-pooled'"),
        0,
        "the project's read-only folders go with it"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn stale_project_setup_write_is_refused() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let created = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(created), OwnerResult::Applied);
    let opened = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(opened.revision, 1, "both windows open revision 1");

    let first = ProjectSetup {
        about: "Plans meals for two.".into(),
        ..pooled_setup(vec![])
    };
    let landed = owner
        .update_project_setup(project.clone(), 1, Some("Meals for two".into()), first, 2)
        .await
        .unwrap();
    assert_eq!(result(landed), OwnerResult::Applied);
    let brief_path = opened.state_folder().join("context/brief.md");
    let brief = std::fs::read_to_string(&brief_path).unwrap();

    let second = ProjectSetup {
        about: "Plans a week of meals.".into(),
        tech_stack: vec!["rust".into()],
        ..pooled_setup(vec![])
    };
    for (revision, generation) in [(1, 3), (0, 4), (99, 5)] {
        let refused = owner
            .update_project_setup(
                project.clone(),
                revision,
                Some("Week planner".into()),
                second.clone(),
                generation,
            )
            .await
            .unwrap();
        assert_eq!(
            result(refused),
            rejected("stale_revision"),
            "a save from revision {revision} never replaces revision 2"
        );
    }
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        (
            stored.revision,
            stored.name.as_str(),
            stored.about.as_str(),
            stored.tech_stack.len()
        ),
        (2, "Meals for two", "Plans meals for two.", 0),
        "the stale window changed nothing"
    );
    assert_eq!(
        std::fs::read_to_string(&brief_path).unwrap(),
        brief,
        "the brief keeps the edit that landed"
    );

    let redone = owner
        .update_project_setup(project.clone(), 2, None, second, 6)
        .await
        .unwrap();
    assert_eq!(result(redone), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        (stored.revision, stored.about.as_str()),
        (3, "Plans a week of meals."),
        "after a re-read the redo lands"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn stale_read_paths_write_is_refused() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let created = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(created), OwnerResult::Applied);
    let shared = tempfile::Builder::new()
        .prefix("ca-stale-read-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let real = std::fs::canonicalize(shared.path()).unwrap();
    let (docs, assets) = (real.join("docs"), real.join("assets"));
    std::fs::create_dir(&docs).unwrap();
    std::fs::create_dir(&assets).unwrap();
    let text = |path: &Path| path.to_str().unwrap().to_owned();

    let opened = owner.project_read_paths(project.clone()).await.unwrap();
    assert_eq!(opened.version, 0, "folders nobody saved are at version 0");
    let landed = owner
        .set_project_read_paths(project.clone(), opened.version, vec![text(&docs)], 2)
        .await
        .unwrap();
    assert_eq!(result(landed), OwnerResult::Applied);

    for (version, generation) in [(opened.version, 3), (7, 4)] {
        let refused = owner
            .set_project_read_paths(project.clone(), version, vec![text(&assets)], generation)
            .await
            .unwrap();
        assert_eq!(
            result(refused),
            rejected("stale_revision"),
            "a save from version {version} never replaces version 1"
        );
    }
    let invalid_and_stale = owner
        .set_project_read_paths(project.clone(), 0, vec!["relative".into()], 5)
        .await
        .unwrap();
    assert_eq!(
        result(invalid_and_stale),
        rejected("read_path_not_absolute"),
        "a refused folder is named before the version is checked"
    );
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![text(&docs)],
            version: 1
        },
        "the stale window changed nothing"
    );
    assert_eq!(
        engine.rows(
            "SELECT version FROM versioned_items WHERE item='read-paths:setup-pooled'
             AND writer='owner'"
        ),
        1
    );

    let redone = owner
        .set_project_read_paths(project.clone(), 1, vec![text(&docs), text(&assets)], 6)
        .await
        .unwrap();
    assert_eq!(result(redone), OwnerResult::Applied);
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![text(&docs), text(&assets)],
            version: 2
        },
        "after a re-read the redo lands"
    );

    let removed = owner.remove_project(project.clone(), 7).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert_eq!(
        engine.rows("SELECT version FROM versioned_items WHERE item='read-paths:setup-pooled'"),
        3,
        "a removed project's folders move their version on once more (V2V-1)"
    );
    let again = owner
        .setup_project(
            project.clone(),
            spec("Meal planner again"),
            pooled_setup(vec![]),
            8,
        )
        .await
        .unwrap();
    assert_eq!(result(again), OwnerResult::Applied);
    assert_eq!(
        owner.project_read_paths(project.clone()).await.unwrap(),
        ReadPaths {
            paths: vec![],
            version: 3
        },
        "the project added again under the same id starts with no folders"
    );
    for (version, generation) in [(0, 9), (1, 10), (2, 11)] {
        let refused = owner
            .set_project_read_paths(project.clone(), version, vec![text(&assets)], generation)
            .await
            .unwrap();
        assert_eq!(
            result(refused),
            rejected("stale_revision"),
            "a save read before the removal at version {version} is refused"
        );
    }
    let fresh = owner
        .set_project_read_paths(project.clone(), 3, vec![text(&assets)], 12)
        .await
        .unwrap();
    assert_eq!(result(fresh), OwnerResult::Applied);
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn owner_saves_a_projects_agent_access_and_toolchains_and_refuses_bad_policies() {
    use crate::sandbox::policy::{AgentAccess, AgentPolicy, ToolchainChoice};
    use crate::sandbox::toolchains::Toolchain;
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);

    let fresh = owner.project_agent_policy(project.clone()).await.unwrap();
    assert_eq!(
        fresh.access,
        AgentAccess::Sandboxed,
        "a project with no saved level stays Sandboxed"
    );
    let levels: Vec<AgentAccess> = fresh.levels.iter().map(|level| level.access).collect();
    assert_eq!(levels, AgentAccess::ALL);
    assert!(fresh.levels.iter().all(|level| level.unavailable.is_none()));
    let rust = fresh
        .toolchains
        .iter()
        .find(|row| row.toolchain == Toolchain::Rust)
        .expect("the owner's rustup and cargo are found");
    assert!(rust.installed);
    assert_eq!(fresh.version, 0, "a policy nobody saved is at version 0");

    let policy = AgentPolicy {
        access: AgentAccess::OwnBranch,
        toolchains: vec![ToolchainChoice {
            toolchain: Toolchain::Rust,
            allowed: !rust.allowed,
        }],
    };
    let saved = owner
        .set_project_agent_policy(project.clone(), fresh.version, policy.clone(), 2)
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    let view = owner.project_agent_policy(project.clone()).await.unwrap();
    assert_eq!(view.access, AgentAccess::OwnBranch);
    assert_eq!(view.version, 1);
    let stale = owner
        .set_project_agent_policy(
            project.clone(),
            fresh.version,
            AgentPolicy {
                access: AgentAccess::Sandboxed,
                toolchains: Vec::new(),
            },
            7,
        )
        .await
        .unwrap();
    assert_eq!(
        result(stale),
        rejected("stale_revision"),
        "a save from the policy another window replaced never lands"
    );
    assert_eq!(
        owner.project_agent_policy(project.clone()).await.unwrap(),
        view,
        "the refused save left the saved policy and its version"
    );
    assert_eq!(
        view.toolchains
            .iter()
            .find(|row| row.toolchain == Toolchain::Rust)
            .map(|row| row.allowed),
        Some(!rust.allowed)
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM project_agent_policy WHERE project='setup-pooled'"),
        1
    );

    let twice = AgentPolicy {
        access: AgentAccess::Full,
        toolchains: vec![
            ToolchainChoice {
                toolchain: Toolchain::Node,
                allowed: true,
            },
            ToolchainChoice {
                toolchain: Toolchain::Node,
                allowed: false,
            },
        ],
    };
    assert_eq!(twice.validate(), Err("agent_policy_invalid"));
    let action = SetupAction::SetAgentPolicy {
        project_id: project.clone(),
        version: view.version,
        policy: twice,
    };
    assert_eq!(action.validate().unwrap_err().0, "agent_policy_invalid");
    assert_eq!(
        owner
            .project_agent_policy(project.clone())
            .await
            .unwrap()
            .access,
        AgentAccess::OwnBranch
    );

    let full = AgentPolicy {
        access: AgentAccess::Full,
        toolchains: Vec::new(),
    };
    let saved = owner
        .set_project_agent_policy(project.clone(), view.version, full, 3)
        .await
        .unwrap();
    assert_eq!(result(saved), OwnerResult::Applied);
    let view = owner.project_agent_policy(project.clone()).await.unwrap();
    assert_eq!(view.access, AgentAccess::Full);
    assert_eq!(
        view.toolchains
            .iter()
            .find(|row| row.toolchain == Toolchain::Rust)
            .map(|row| row.allowed),
        Some(rust.used),
        "a toolchain with no choice follows the project again"
    );

    let unknown = ProjectId::new("setup-missing").unwrap();
    assert_eq!(
        owner
            .project_agent_policy(unknown.clone())
            .await
            .unwrap_err()
            .0,
        "invalid_project"
    );
    let removed = owner.remove_project(project.clone(), 4).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert_eq!(
        engine.rows("SELECT count(*) FROM project_agent_policy WHERE project='setup-pooled'"),
        0,
        "the project's agent access goes with it"
    );
    assert_eq!(
        engine.rows("SELECT version FROM versioned_items WHERE item='agent-policy:setup-pooled'"),
        3,
        "removal moves the access version past every screen read before it (V2V-1)"
    );
    for item in [
        "instruction-settings:setup-pooled",
        "process:project:setup-pooled",
    ] {
        assert_eq!(
            engine.rows(&format!(
                "SELECT version FROM versioned_items WHERE item='{item}'"
            )),
            1,
            "{item} moves on at removal even when it was never saved"
        );
    }

    let again = owner
        .setup_project(
            project.clone(),
            spec("Meal planner again"),
            pooled_setup(vec![]),
            5,
        )
        .await
        .unwrap();
    assert_eq!(result(again), OwnerResult::Applied);
    let readded = owner.project_agent_policy(project.clone()).await.unwrap();
    assert_eq!(readded.access, AgentAccess::Sandboxed);
    assert_eq!(readded.version, 3);
    let from_before = owner
        .set_project_agent_policy(
            project.clone(),
            view.version,
            AgentPolicy {
                access: AgentAccess::Full,
                toolchains: Vec::new(),
            },
            6,
        )
        .await
        .unwrap();
    assert_eq!(
        result(from_before),
        rejected("stale_revision"),
        "a screen that showed Full before the removal never gives the project added again Full"
    );
    assert_eq!(
        owner.project_agent_policy(project.clone()).await.unwrap(),
        readded,
        "the project added again stays Sandboxed"
    );
    let chosen = owner
        .set_project_agent_policy(
            project.clone(),
            readded.version,
            AgentPolicy {
                access: AgentAccess::OwnBranch,
                toolchains: Vec::new(),
            },
            7,
        )
        .await
        .unwrap();
    assert_eq!(result(chosen), OwnerResult::Applied);
    drop(owner);
    engine.stop().await;
}

/// A team whose full status no longer fits one owner reply, from 120 long tasks, 80 stopped
/// sub-agents with long problems and long escalations, still reads, and the owner still
/// pauses it, cancels its tasks and turns the live master off.
#[tokio::test]
async fn the_owner_pauses_and_cancels_a_team_whose_full_status_passes_one_reply() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let connection = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    connection.busy_timeout(ENGINE_WAIT).unwrap();
    for sql in [
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<120)
         INSERT INTO team_tasks(project,source,harness,tools,prompt,status,attempts,result,delivered,created_ms,started_ms,finished_ms)
         SELECT ?1,'owner','codex',0,i||' '||hex(zeroblob(2000)),'done',1,hex(zeroblob(2000)),1,i,i,i FROM n",
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<80)
         INSERT INTO team_agents(project,role,harness,tools,status,error,created_ms,heartbeat_ms,stopped_ms)
         SELECT ?1,'sub','codex',0,'stopped',hex(zeroblob(1000)),i,i,i FROM n",
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<25)
         INSERT INTO team_notifications(project,kind,text,created_ms)
         SELECT ?1,'escalation',hex(zeroblob(1000)),i FROM n",
    ] {
        assert!(connection.execute(sql, ["setup-pooled"]).unwrap() > 0, "{sql}");
    }
    drop(connection);
    let status = owner.team_status(project.clone()).await.unwrap();
    assert!(status.partial, "the read left older history out to fit");
    for (generation, action) in [
        (
            2,
            crate::team::TeamAction::Pause {
                project_id: project.clone(),
            },
        ),
        (
            3,
            crate::team::TeamAction::Cancel {
                project_id: project.clone(),
                task: None,
            },
        ),
        (
            4,
            crate::team::TeamAction::LiveMaster {
                project_id: project.clone(),
                on: false,
            },
        ),
    ] {
        let response = owner.team_action(action.clone(), generation).await.unwrap();
        assert_eq!(result(response), OwnerResult::Applied, "{action:?}");
    }
    assert_eq!(
        engine.rows("SELECT count(*) FROM team_paused_projects WHERE project='setup-pooled'"),
        1
    );
    drop(owner);
    engine.stop().await;
}
