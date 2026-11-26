use super::*;

/// RV-DS241-159: only rate-limited ends in a row count toward the cap, so a run that is
/// saved, stopped, failed before its harness started or taken back stale starts the count
/// again. Each such end after two rate-limited ones leaves the count at zero, and the job
/// taken back stale takes two more rate-limited ends before the third fails it.
#[test]
fn an_end_other_than_a_rate_limit_starts_the_rate_limit_count_again() {
    let root = engine_root();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    register_project(&root.database, "alpha", &folder);
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let account = reference("codex-connection");
    let limited = |id: i64| -> i64 {
        connection
            .query_row(
                "SELECT limited FROM research_jobs WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let limited_twice = |kind: &str| {
        let id = jobs.enqueue("alpha", kind, "codex", false).unwrap();
        for _ in 0..2 {
            let held = jobs.claim("alpha", "runner-a", 1).unwrap().unwrap();
            assert_eq!(held.id, id);
            assert_eq!(
                jobs.rate_limited(&held, &account).unwrap().0,
                ResearchStatus::Queued
            );
        }
        assert_eq!(limited(id), 2);
        let held = jobs.claim("alpha", "runner-a", 1).unwrap().unwrap();
        assert_eq!(held.id, id);
        held
    };

    let saved = limited_twice("market_research");
    jobs.finish(&saved, "context/research/market.md").unwrap();
    assert_eq!(limited(saved.id), 0, "a saved run starts the count again");

    let stopped = limited_twice("competitor_research");
    jobs.cancelled(&stopped).unwrap();
    assert_eq!(
        limited(stopped.id),
        0,
        "a stopped run starts the count again"
    );

    let unspent = limited_twice("design_research");
    assert_eq!(
        jobs.fail_unspent(&unspent, "Provider refused").unwrap(),
        ResearchStatus::Failed
    );
    assert_eq!(
        limited(unspent.id),
        0,
        "a run failed before its harness started starts the count again"
    );

    let stale = limited_twice("tech_stack_research");
    assert_eq!(jobs.recover_stale(0, 10).unwrap(), 1);
    assert_eq!(
        limited(stale.id),
        0,
        "a run taken back stale starts the count again"
    );
    let mut ends = Vec::new();
    for _ in 0..3 {
        let held = jobs.claim("alpha", "runner-a", 1).unwrap().unwrap();
        assert_eq!(held.id, stale.id);
        ends.push(jobs.rate_limited(&held, &account).unwrap().0);
    }
    assert_eq!(
        ends,
        [
            ResearchStatus::Queued,
            ResearchStatus::Queued,
            ResearchStatus::Failed
        ]
    );
}
