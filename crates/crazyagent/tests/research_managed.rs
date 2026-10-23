mod research_support;

use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use research_support::managed_project;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const RUN_LIMIT: Duration = Duration::from_secs(1200);

/// Queues one research job for `project` in the running engine's database,
/// waits for the engine's own research service to finish it on the pool's
/// signed-in managed account and returns the report it wrote.
fn engine_research(runtime: &Path, project: &str, kind: &str, harness: &str) -> String {
    let jobs = ResearchJobs::open(&runtime.join("engine.db")).unwrap();
    let id = jobs.enqueue(project, kind, harness, true).unwrap();
    let deadline = Instant::now() + RUN_LIMIT;
    let job = loop {
        let job = jobs.get(id).unwrap().unwrap();
        if matches!(
            job.status,
            ResearchStatus::Done | ResearchStatus::Failed | ResearchStatus::Cancelled
        ) {
            break job;
        }
        assert!(
            Instant::now() < deadline,
            "research job {id} is still {}: {:?}",
            job.status.label(),
            job.error
        );
        std::thread::sleep(Duration::from_secs(1));
    };
    assert_eq!(job.status, ResearchStatus::Done, "{:?}", job.error);
    assert_eq!(
        job.report_path.as_deref(),
        Some(format!("context/research/{kind}.md").as_str())
    );
    fs::read_to_string(
        runtime
            .join("projects")
            .join(project)
            .join("context")
            .join("research")
            .join(format!("{kind}.md")),
    )
    .unwrap()
}

#[test]
fn research_runs_on_a_signed_in_managed_codex_account() {
    let (runtime, project) = managed_project("CRAZYAGENT_MANAGED_CODEX_PROJECT", "Codex");
    let report = engine_research(&runtime, &project, "competitor_research", "codex");
    assert!(report.starts_with("# Competitor research\n"), "{report}");
    assert!(report.contains("Researched by Codex"), "{report}");
    assert!(report.contains("## Links and references"), "{report}");
    assert!(report.contains("https://"), "{report}");
}

#[test]
fn research_runs_on_a_signed_in_managed_claude_account() {
    let (runtime, project) = managed_project("CRAZYAGENT_MANAGED_CLAUDE_PROJECT", "Claude Code");
    let report = engine_research(&runtime, &project, "market_research", "claude-code");
    assert!(report.starts_with("# Market research\n"), "{report}");
    assert!(report.contains("Researched by Claude Code"), "{report}");
    assert!(report.contains("## Links and references"), "{report}");
    assert!(report.contains("https://"), "{report}");
}
