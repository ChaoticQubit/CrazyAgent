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
            .tempdir_in("/private/tmp")
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
            .tempdir_in("/private/tmp")
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
        "# Market\n\n## Size\nAbout 2 million households. [Source](https://example.org/a)\n\n## Trends\n{}\n",
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
        engine.rows(
            "SELECT count(*) FROM research_jobs WHERE project='setup-pooled'
            AND status='cancelled' AND cancel_requested=1"
        ),
        1
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
