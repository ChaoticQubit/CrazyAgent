use super::*;
use crate::config::{EffectiveConfig, resolve_config_bytes};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};

const POOLED: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-api", "account-codex", "account-claude"]
[[model_access.connections]]
id = "a-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "none" }
[[model_access.connections]]
id = "b-claude"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "model-claude", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
[[model_access.connections]]
id = "c-codex"
provider = "provider-codex"
harness = "codex"
protocol = "native"
account_ref = "account-codex"
endpoint = { kind = "native" }
model = { id = "model-codex", author = "author-codex" }
auth = { method = "native_managed", profile_ref = "profile-codex" }
[[model_access.pools]]
id = "pool-mixed"
members = ["a-api", "b-claude", "c-codex"]
projects = ["assigned-project"]
roles = []
fallback_allowed = false
[[model_access.pools]]
id = "pool-local"
members = ["a-api"]
projects = []
roles = []
fallback_allowed = false
"#;

fn config() -> EffectiveConfig {
    resolve_config_bytes(POOLED, "").unwrap()
}

fn setup() -> ProjectSetup {
    ProjectSetup {
        about: "A planner for small teams.\nWith two lines.".into(),
        tech_stack: vec!["rust".into(), "My custom engine".into()],
        platforms: vec![Platform::Website, Platform::Cli],
        research: vec![
            ResearchKind::MarketResearch,
            ResearchKind::CompetitorResearch,
        ],
        pool: Some(CredentialRef::new("pool-mixed").unwrap()),
        team: None,
        documents: vec![],
    }
}

fn database(root: &Path) -> Connection {
    let connection = Connection::open(root.join("jobs.db")).unwrap();
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    initialize(&connection).unwrap();
    connection
}

#[test]
fn setup_bounds_reject_every_oversized_or_malformed_field() {
    assert!(setup().validate().is_ok());
    let mut cases: Vec<ProjectSetup> = Vec::new();
    let mut case = setup();
    case.about = "x".repeat(ABOUT_BYTES_MAX + 1);
    cases.push(case);
    let mut case = setup();
    case.about = "bell\u{7}".into();
    cases.push(case);
    let mut case = setup();
    case.tech_stack = vec!["t".into(); TECH_ENTRIES_MAX + 1];
    cases.push(case);
    let mut case = setup();
    case.tech_stack = vec!["x".repeat(TECH_ENTRY_BYTES_MAX + 1)];
    cases.push(case);
    let mut case = setup();
    case.tech_stack = vec!["  ".into()];
    cases.push(case);
    let mut case = setup();
    case.tech_stack = vec!["rust".into(), "Rust".into()];
    cases.push(case);
    let mut case = setup();
    case.platforms = vec![Platform::Website, Platform::Website];
    cases.push(case);
    let mut case = setup();
    case.research = vec![ResearchKind::DesignResearch, ResearchKind::DesignResearch];
    cases.push(case);
    let mut case = setup();
    case.documents = vec!["relative/file.md".into()];
    cases.push(case);
    let mut case = setup();
    case.documents = vec![format!("/{}", "d".repeat(DOCUMENT_PATH_BYTES_MAX))];
    cases.push(case);
    let mut case = setup();
    case.documents = vec!["/tmp/a.md".into(); DOCUMENTS_MAX + 1];
    cases.push(case);
    let mut case = setup();
    case.team = Some(TeamPlan {
        master: "codex".into(),
        master_tools: true,
        sub_harnesses: vec![HarnessSlot {
            harness: "claude".into(),
            agents: 3,
            tools: false,
        }],
        min_agents: 5,
        max_agents: 6,
    });
    cases.push(case);
    let mut case = setup();
    case.team = Some(TeamPlan {
        master: "bad harness".into(),
        master_tools: true,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    });
    cases.push(case);
    let mut case = setup();
    case.team = Some(TeamPlan {
        master: "codex".into(),
        master_tools: true,
        sub_harnesses: vec![
            HarnessSlot {
                harness: "claude".into(),
                agents: 0,
                tools: true,
            };
            1
        ],
        min_agents: 1,
        max_agents: 1,
    });
    cases.push(case);
    for (index, case) in cases.iter().enumerate() {
        assert!(case.validate().is_err(), "case {index} must be rejected");
    }
}

#[test]
fn catalog_serves_plain_labels_and_default_team_prefers_signed_in_codex() {
    let config = config();
    let catalog = catalog(
        &config,
        Some(&CredentialRef::new("pool-mixed").unwrap()),
        &[ResearchKind::MarketResearch, ResearchKind::DesignResearch],
    )
    .unwrap();
    assert!(
        catalog
            .tech_stack
            .iter()
            .any(|o| o.id == "rust" && o.group == TechGroup::Language)
    );
    assert!(
        catalog
            .tech_stack
            .iter()
            .any(|o| o.group == TechGroup::Framework)
    );
    assert!(
        catalog
            .tech_stack
            .iter()
            .any(|o| o.group == TechGroup::Database)
    );
    let research: Vec<_> = catalog.research.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        research,
        [
            "Market research",
            "Competitor research",
            "Design research",
            "Front-end design",
            "Tech stack research"
        ]
    );
    let platforms: Vec<_> = catalog.platforms.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        platforms,
        [
            "Website",
            "Mobile app",
            "Desktop app",
            "CLI",
            "API or backend",
            "Other"
        ]
    );
    assert_eq!(catalog.document_bytes_max, 52_428_800);
    assert_eq!(catalog.project_document_bytes_max, 209_715_200);
    let team = catalog.default_team;
    assert_eq!(team.master, "codex");
    assert!(team.master_tools);
    assert_eq!(
        team.sub_harnesses,
        [HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: true
        }]
    );
    assert_eq!((team.min_agents, team.max_agents), (1, 3));
    assert!(team.validate().is_ok());

    let local = default_team(
        &config,
        Some(&CredentialRef::new("pool-local").unwrap()),
        &[],
    );
    assert_eq!(local.master, "rcli");
    assert!(local.sub_harnesses.is_empty());
    assert_eq!((local.min_agents, local.max_agents), (1, 1));
    assert_eq!(default_team(&config, None, &[]).master, "codex");
    assert!(catalog_ok_without_pool(&config));
}

fn catalog_ok_without_pool(config: &EffectiveConfig) -> bool {
    catalog(config, None, &ResearchKind::ALL).is_ok()
        && catalog(
            config,
            Some(&CredentialRef::new("pool-absent").unwrap()),
            &[],
        )
        .is_err()
}

#[test]
fn documents_copy_only_allowed_regular_files_within_bounds() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let destination = root.path().join("documents");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(source.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(source.join("Report.PDF"), b"%PDF-1.4 bytes").unwrap();
    std::fs::set_permissions(
        source.join("notes.md"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let copied = copy_documents(
        &[
            source.join("notes.md").to_str().unwrap().to_owned(),
            source.join("Report.PDF").to_str().unwrap().to_owned(),
        ],
        &destination,
        PROJECT_DOCUMENT_BYTES_MAX,
    )
    .unwrap();
    assert_eq!(
        copied,
        [
            StoredDocument {
                name: "notes.md".into(),
                bytes: 8
            },
            StoredDocument {
                name: "Report.PDF".into(),
                bytes: 14
            }
        ]
    );
    assert_eq!(
        std::fs::read(destination.join("notes.md")).unwrap(),
        b"# Notes\n"
    );
    let mode = std::fs::metadata(destination.join("notes.md"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "copied documents are never executable");

    std::os::unix::fs::symlink(source.join("notes.md"), source.join("link.md")).unwrap();
    std::fs::create_dir(source.join("folder.md")).unwrap();
    let fifo = source.join("pipe.txt");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(source.join("script.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(source.join("big.txt"), vec![b'a'; 64]).unwrap();
    std::fs::write(source.join("dup.md"), "one").unwrap();
    let rejected = [
        (source.join("link.md"), "document_not_a_regular_file"),
        (source.join("folder.md"), "document_not_a_regular_file"),
        (fifo.clone(), "document_not_a_regular_file"),
        (source.join("script.sh"), "document_type_not_allowed"),
        (source.join("absent.md"), "document_not_found"),
    ];
    for (path, code) in rejected {
        let empty = root.path().join(format!(
            "empty-{code}-{}",
            path.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir(&empty).unwrap();
        let error = copy_documents(
            &[path.to_str().unwrap().to_owned()],
            &empty,
            PROJECT_DOCUMENT_BYTES_MAX,
        )
        .unwrap_err();
        assert_eq!(error, code, "{path:?}");
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0, "{path:?}");
    }
    let small = root.path().join("small");
    std::fs::create_dir(&small).unwrap();
    assert_eq!(
        copy_documents(
            &[source.join("big.txt").to_str().unwrap().to_owned()],
            &small,
            63
        )
        .unwrap_err(),
        "project_documents_too_large"
    );
    let twice = root.path().join("twice");
    std::fs::create_dir(&twice).unwrap();
    let duplicate = source.join("dup.md").to_str().unwrap().to_owned();
    assert_eq!(
        copy_documents(
            &[duplicate.clone(), duplicate],
            &twice,
            PROJECT_DOCUMENT_BYTES_MAX
        )
        .unwrap_err(),
        "document_name_duplicate"
    );
}

fn queued(connection: &Connection, project: &str, kinds: &[ResearchKind]) {
    let tx = connection.unchecked_transaction().unwrap();
    let project = ProjectId::new(project).unwrap();
    let team = TeamPlan {
        master: "codex".into(),
        master_tools: true,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    };
    insert_jobs(&tx, &project, kinds, &team, true, 1000).unwrap();
    tx.commit().unwrap();
}

#[test]
fn two_claimers_cannot_both_claim_one_job() {
    let root = tempfile::tempdir().unwrap();
    let connection = database(root.path());
    queued(
        &connection,
        "assigned-project",
        &[ResearchKind::MarketResearch],
    );
    let config = Arc::new(config());
    let barrier = Arc::new(Barrier::new(2));
    let claims: Vec<_> = ["claimer-a", "claimer-b"]
        .into_iter()
        .map(|claimer| {
            let path = root.path().to_owned();
            let barrier = barrier.clone();
            let config = config.clone();
            std::thread::spawn(move || {
                let mut connection = database(&path);
                barrier.wait();
                claim_research_job(&mut connection, &config, claimer, 2000).unwrap()
            })
        })
        .collect();
    let claims: Vec<_> = claims.into_iter().map(|c| c.join().unwrap()).collect();
    let won: Vec<_> = claims.iter().flatten().collect();
    assert_eq!(won.len(), 1, "exactly one claimer may win: {claims:?}");
    assert_eq!(won[0].status, ResearchStatus::Running);
    assert_eq!(won[0].attempts, 1);
    let jobs = list_jobs(
        &connection.unchecked_transaction().unwrap(),
        &config,
        &ProjectId::new("assigned-project").unwrap(),
    )
    .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, ResearchStatus::Running);
    assert_eq!(jobs[0].started_ms, Some(2000));
}

#[test]
fn claims_follow_pool_assignment_and_finish_records_the_report() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = database(root.path());
    let config = config();
    {
        let tx = connection.unchecked_transaction().unwrap();
        let team = TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![
                HarnessSlot {
                    harness: "claude".into(),
                    agents: 1,
                    tools: false,
                },
                HarnessSlot {
                    harness: "opencode".into(),
                    agents: 1,
                    tools: true,
                },
            ],
            min_agents: 1,
            max_agents: 3,
        };
        insert_jobs(
            &tx,
            &ProjectId::new("unassigned-project").unwrap(),
            &[ResearchKind::DesignResearch],
            &team,
            false,
            10,
        )
        .unwrap();
        insert_jobs(
            &tx,
            &ProjectId::new("assigned-project").unwrap(),
            &[
                ResearchKind::MarketResearch,
                ResearchKind::TechStackResearch,
            ],
            &team,
            false,
            20,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let tx = connection.unchecked_transaction().unwrap();
    let waiting = list_jobs(&tx, &config, &ProjectId::new("unassigned-project").unwrap()).unwrap();
    assert_eq!(waiting[0].status, ResearchStatus::NeedsPool);
    assert_eq!(waiting[0].status_label, "Needs a pool");
    assert_eq!(waiting[0].kind_label, "Design research");
    assert_eq!(
        waiting[0].report_path,
        "context/research/design_research.md"
    );
    assert_eq!(waiting[0].harness, "claude");
    let assigned = list_jobs(&tx, &config, &ProjectId::new("assigned-project").unwrap()).unwrap();
    assert_eq!(
        assigned.iter().map(|j| j.status).collect::<Vec<_>>(),
        [ResearchStatus::Queued, ResearchStatus::Queued]
    );
    assert_eq!(
        assigned
            .iter()
            .map(|j| j.harness.as_str())
            .collect::<Vec<_>>(),
        ["claude", "opencode"]
    );
    drop(tx);

    let first = claim_research_job(&mut connection, &config, "runner", 30)
        .unwrap()
        .unwrap();
    let second = claim_research_job(&mut connection, &config, "runner", 31)
        .unwrap()
        .unwrap();
    assert_eq!(first.project_id.as_str(), "assigned-project");
    assert_eq!(second.project_id.as_str(), "assigned-project");
    assert_ne!(first.id, second.id);
    assert!(
        claim_research_job(&mut connection, &config, "runner", 32)
            .unwrap()
            .is_none(),
        "a job without a pool is never claimed"
    );

    assert!(finish_research_job(&mut connection, &first.id, "other-runner", None, 40).is_err());
    finish_research_job(&mut connection, &first.id, "runner", None, 41).unwrap();
    finish_research_job(
        &mut connection,
        &second.id,
        "runner",
        Some("harness_unavailable"),
        42,
    )
    .unwrap();
    assert!(
        finish_research_job(&mut connection, &first.id, "runner", None, 43).is_err(),
        "a finished job is final"
    );
    let tx = connection.unchecked_transaction().unwrap();
    let done = list_jobs(&tx, &config, &ProjectId::new("assigned-project").unwrap()).unwrap();
    let by_id = |id: &str| done.iter().find(|j| j.id == id).unwrap().clone();
    assert_eq!(by_id(&first.id).status, ResearchStatus::Done);
    assert_eq!(by_id(&first.id).finished_ms, Some(41));
    assert_eq!(by_id(&second.id).status, ResearchStatus::Failed);
    assert_eq!(
        by_id(&second.id).failure.as_deref(),
        Some("harness_unavailable")
    );
    let stored: String = tx
        .query_row(
            "SELECT status FROM research_jobs WHERE project='unassigned-project'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "needs_pool");
}

#[test]
fn markdown_sections_are_selected_by_heading() {
    let report = "# Market\nintro\n## Size\nbig\n### Detail\nfine\n## Trends\nup\n";
    assert_eq!(section(report, None).unwrap(), report.trim());
    assert_eq!(
        section(report, Some("Size")).unwrap(),
        "## Size\nbig\n### Detail\nfine"
    );
    assert_eq!(section(report, Some("Trends")).unwrap(), "## Trends\nup");
    assert!(section(report, Some("Absent")).is_none());
}
