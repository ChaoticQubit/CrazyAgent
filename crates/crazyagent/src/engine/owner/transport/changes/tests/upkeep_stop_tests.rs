use super::*;

/// An engine stop that comes while its landing upkeep prunes thousands of due discarded
/// change sets returns within its budget: the pass ends at its next row, leaves no
/// connection on the database, and every row it did not reach stays discarded and due.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_stop_during_a_large_prune_returns_within_its_budget_and_leaves_the_rest_due() {
    const DISCARDED: i64 = 20_000;
    const BUDGET: Duration = Duration::from_secs(2);
    let tmp = tempfile::Builder::new()
        .prefix("ca-pst-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.join("global.toml"), "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(
        root.join("global.toml"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = started(root, &stopped).await;
    tokio::time::timeout(Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let gone = root.join("gone");
    let db = landing::open(root).unwrap();
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO landing_change_sets(id, project, agent_kind, agent_id, label, \
         repository, worktree, git_dir, base, head, state, last_outcome, turn, prompt_line, \
         files, added, removed, conflicts, created_ms, updated_ms, restorable_until_ms) \
         SELECT printf('%032x', i), 'project-1', 'sub', 'agent-' || i, 'Agent', ?2, \
         ?2 || '/agent-' || i, ?2 || '/.git/worktrees/agent-' || i, ?3, ?3, 'discarded', \
         'succeeded', 'turn-1', 'Add a file', 1, 1, 0, '[]', 1, 1, 1 FROM n",
        rusqlite::params![DISCARDED, gone.to_str().unwrap(), "a".repeat(40)],
    )
    .unwrap();
    drop(db);
    let mut service = started(root, &stopped).await;
    let began = std::time::Instant::now();
    let shutdown = service.shutdown();
    tokio::pin!(shutdown);
    let stopped_by = match tokio::time::timeout(BUDGET, &mut shutdown).await {
        Ok(result) => result,
        Err(_) => shutdown.await,
    };
    let elapsed = began.elapsed();
    stopped_by.unwrap();
    assert!(
        elapsed <= BUDGET,
        "the stop waited {elapsed:?} for its prune pass, past its budget of {BUDGET:?}"
    );
    let probe = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let alone = probe.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;");
    assert!(
        alone.is_ok(),
        "a connection of the stopped engine was left on its database: {alone:?}"
    );
    let (pruned, due): (i64, i64) = probe
        .query_row(
            "SELECT count(*) FILTER (WHERE pruned=1), \
             count(*) FILTER (WHERE pruned=0 AND restorable_until_ms <= 1) \
             FROM landing_change_sets WHERE state='discarded'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(pruned < DISCARDED, "the stopped pass pruned every row");
    assert_eq!(
        pruned + due,
        DISCARDED,
        "a row the pass did not reach was lost"
    );
}
