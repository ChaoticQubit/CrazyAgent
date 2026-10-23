use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crate::memory::{HumanReview, KnowledgeStatus, ReviewDecision};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const CATALOG: &str = r#"
timeout_ms = 1000
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
        std::fs::write(&global, CATALOG).unwrap();
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
        Self {
            root,
            service: Some(service),
            _stop: stop,
        }
    }

    async fn owner(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(2),
        )
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
        catalog.default_team.master, "rcli",
        "codex has no sign-in in this engine, so the configured endpoint leads"
    );
    assert_eq!(catalog.default_team.sub_harnesses[0].agents, 2);
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

    let folder = std::fs::canonicalize(engine.root.path())
        .unwrap()
        .join("projects/setup-pooled");
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
    assert_eq!(
        std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(stored.folder, folder.to_str().unwrap());
    assert_eq!(stored.team.master, "rcli");
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
            ("Market research", "Queued", "rcli"),
            ("Competitor research", "Queued", "rcli")
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
    assert!(!folder.with_file_name("setup-linked").exists());
    assert!(!folder.with_file_name(".staging-setup-linked").exists());
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
    let inbox = owner
        .read(OwnerRead::Knowledge {
            review: HumanReview::Inbox {
                project_id: project.clone(),
                after: None,
            },
        })
        .await
        .unwrap();
    let OwnerData::Domain { outcome } = inbox.data else {
        panic!("knowledge inbox required");
    };
    let Outcome::Knowledge { entries } = *outcome else {
        panic!("knowledge entries required");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, KnowledgeStatus::Provisional);
    assert!(
        entries[0]
            .text
            .starts_with("## Size\nAbout 2 million households.")
    );
    let entry = entries[0].id.clone();
    owner
        .read(OwnerRead::ProjectSetup {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    owner
        .begin_mutation(
            OwnerMutation::Knowledge {
                review: HumanReview::Review {
                    entry_id: entry,
                    version: 1,
                    decision: ReviewDecision::Keep,
                },
            },
            8,
        )
        .unwrap();
    assert_eq!(
        result(owner.send_pending().await.unwrap()),
        OwnerResult::Applied
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
    let folder = std::path::PathBuf::from(
        owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap()
            .folder,
    );

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
