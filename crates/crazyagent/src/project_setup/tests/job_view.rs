use super::*;

/// Only a failed or stopped job can run again, and only on a system where the engine runs
/// research at all: none can on Linux or Windows, where the runner refuses every job.
#[test]
fn only_a_failed_or_stopped_job_where_research_runs_can_run_again() {
    let runs = crate::research::runner::confined_unavailable(std::env::consts::OS).is_none();
    assert_eq!(runs, !cfg!(any(target_os = "linux", target_os = "windows")));
    for status in [
        ResearchStatus::Queued,
        ResearchStatus::NeedsPool,
        ResearchStatus::Running,
        ResearchStatus::Done,
    ] {
        assert!(!retryable(status), "{status:?}");
    }
    for status in [ResearchStatus::Failed, ResearchStatus::Cancelled] {
        assert_eq!(retryable(status), runs, "{status:?}");
    }
}

#[test]
fn a_rate_limited_wait_reads_back_the_minute_the_research_service_wrote() {
    let job = |status: ResearchStatus, detail: &str| ResearchJob {
        id: 1,
        project_id: ProjectId::new("assigned-project").unwrap(),
        kind: ResearchKind::MarketResearch,
        kind_label: "Market research".into(),
        status,
        status_label: status.label().into(),
        report_path: "context/research/market_research.md".into(),
        harness: "claude".into(),
        tools: false,
        attempts: 0,
        claimer: None,
        error: Some(detail.into()),
        cancel_requested: false,
        output_bytes: 0,
        created_ms: 1,
        started_ms: None,
        heartbeat_ms: None,
        finished_ms: None,
        retryable: false,
    };
    let mut checked = 0;
    for seconds in (0..4_102_444_800_u64)
        .step_by(7_919_993)
        .chain([951_782_400, 1_792_729_920])
    {
        let written = crate::research::runner::utc_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
        );
        let waiting = job(
            ResearchStatus::NeedsPool,
            &format!("rate_limited until {written}: All paused"),
        );
        let minute = i64::try_from(seconds / 60 * 60_000).unwrap();
        assert_eq!(
            waiting.rate_limited(),
            Some((minute, "All paused")),
            "{written}"
        );
        checked += 1;
    }
    assert!(checked > 500);
    let queued = job(
        ResearchStatus::Queued,
        "rate_limited until 2026-10-23 04:32 UTC: All paused",
    );
    assert_eq!(queued.rate_limited(), None);
    for detail in [
        "No account in this project's pool can run this research",
        "rate_limited until 2026-10-23 04:32: All paused",
        "rate_limited until 2026-13-23 04:32 UTC: All paused",
        "rate_limited until 2026-10-23 24:00 UTC: All paused",
        "rate_limited until 2026/10/23 04:32 UTC: All paused",
        "rate_limited until 2026-10-2x 04:32 UTC: All paused",
    ] {
        assert_eq!(
            job(ResearchStatus::NeedsPool, detail).rate_limited(),
            None,
            "{detail}"
        );
    }
}
