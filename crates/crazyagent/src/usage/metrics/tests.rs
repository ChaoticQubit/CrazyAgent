use super::*;
use crate::usage::report::ROWS_MAX;
use rusqlite::params;

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;
const MIDNIGHT: i64 = 1_792_972_800_000;
const NOW: i64 = MIDNIGHT + 20 * HOUR;

fn database() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let db = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::team::initialize(&db).unwrap();
    crate::usage::initialize(&db).unwrap();
    (directory, db)
}

#[allow(clippy::too_many_arguments)]
fn task(
    db: &Connection,
    project: &str,
    agent: i64,
    status: &str,
    prompt: &str,
    session: Option<&str>,
    created_ms: i64,
    finished_ms: Option<i64>,
) -> i64 {
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,finished_ms,session)
         VALUES (?1,'owner','claude-code',1,?2,?3,?4,?5,?6,?7)",
        params![project, prompt, status, agent, created_ms, finished_ms, session],
    )
    .unwrap();
    db.last_insert_rowid()
}

#[allow(clippy::too_many_arguments)]
fn used(
    db: &Connection,
    hour_ms: i64,
    project: &str,
    account: &str,
    agent: &str,
    harness: &str,
    session: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
) {
    db.execute(
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,input,output,cache_read,cache_write)
         VALUES (?1,'agent',?2,'pool-a',?3,?4,?5,?6,?7,?8,?9,?10)",
        params![hour_ms, project, account, agent, harness, session, input, output, cache_read, cache_write],
    )
    .unwrap();
}

fn alpha() -> ProjectId {
    ProjectId::new("alpha").unwrap()
}

/// Running and waiting count the tasks as they are now; done and failed count the tasks
/// that finished in the range. One project counts only its own tasks, None counts all.
#[test]
fn tasks_count_running_and_waiting_now_and_done_or_failed_in_the_range() {
    let (_directory, db) = database();
    for (project, status, finished_ms) in [
        ("alpha", "running", None),
        ("alpha", "queued", None),
        ("alpha", "needs_pool", None),
        ("alpha", "done", Some(MIDNIGHT + HOUR)),
        ("alpha", "done", Some(MIDNIGHT + 2 * HOUR)),
        ("alpha", "done", Some(MIDNIGHT - HOUR)),
        ("alpha", "failed", Some(MIDNIGHT + 3 * HOUR)),
        ("alpha", "cancelled", Some(MIDNIGHT + 3 * HOUR)),
        ("beta", "running", None),
        ("beta", "done", Some(MIDNIGHT + HOUR)),
    ] {
        task(
            &db,
            project,
            1,
            status,
            "Work",
            None,
            MIDNIGHT - DAY,
            finished_ms,
        );
    }
    let one = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    assert_eq!(
        (
            one.running,
            one.tasks_waiting,
            one.tasks_done,
            one.tasks_failed
        ),
        (1, 2, 2, 1)
    );
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(
        (
            all.running,
            all.tasks_waiting,
            all.tasks_done,
            all.tasks_failed
        ),
        (2, 2, 3, 1)
    );
}

/// A run is one agent session. It is titled by the latest team task of that agent that ran
/// in the session, matched exactly (Claude) or as the end of a Codex rollout name; a master
/// session or another agent's task leaves it untitled. Newest runs come first.
#[test]
fn runs_are_agent_sessions_titled_by_their_latest_task_newest_first() {
    let (_directory, db) = database();
    let rollout = "rollout-2026-11-07T00-00-00-abc123";
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "c-1",
        100,
        10,
        5000,
        50,
    );
    used(
        &db,
        MIDNIGHT + 2 * HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "c-1",
        200,
        20,
        6000,
        0,
    );
    used(
        &db,
        MIDNIGHT + 5 * HOUR,
        "alpha",
        "acct-b",
        "team-8",
        "codex",
        rollout,
        300,
        30,
        900,
        0,
    );
    used(
        &db,
        MIDNIGHT + 3 * HOUR,
        "alpha",
        "acct-a",
        "master-alpha",
        "claude-code",
        "m-1",
        40,
        4,
        0,
        0,
    );
    used(
        &db,
        MIDNIGHT + 4 * HOUR,
        "beta",
        "acct-a",
        "team-9",
        "claude-code",
        "b-1",
        1,
        1,
        0,
        0,
    );
    used(
        &db,
        MIDNIGHT - HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "old",
        1,
        1,
        0,
        0,
    );
    task(
        &db,
        "alpha",
        7,
        "done",
        "Fix the login\nwith details",
        Some("c-1"),
        MIDNIGHT,
        Some(MIDNIGHT + HOUR),
    );
    let newest = task(
        &db,
        "alpha",
        7,
        "done",
        "Checkout form validation\nmore words",
        Some("c-1"),
        MIDNIGHT + HOUR,
        Some(MIDNIGHT + 2 * HOUR),
    );
    let calendar = task(
        &db,
        "alpha",
        8,
        "running",
        "Calendar API endpoints",
        Some("abc123"),
        MIDNIGHT + 4 * HOUR,
        None,
    );
    task(
        &db,
        "alpha",
        9,
        "done",
        "Another agent in the same session",
        Some("m-1"),
        MIDNIGHT,
        Some(MIDNIGHT + HOUR),
    );
    let runs = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap().runs;
    let shown: Vec<_> = runs
        .iter()
        .map(|run| {
            (
                run.agent.as_str(),
                run.task,
                run.title.as_deref(),
                run.status.as_deref(),
                run.last_ms,
                run.tokens.headline(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "team-8",
                Some(calendar),
                Some("Calendar API endpoints"),
                Some("running"),
                MIDNIGHT + 5 * HOUR,
                330
            ),
            ("master-alpha", None, None, None, MIDNIGHT + 3 * HOUR, 44),
            (
                "team-7",
                Some(newest),
                Some("Checkout form validation"),
                Some("done"),
                MIDNIGHT + 2 * HOUR,
                380
            ),
        ]
    );
    assert_eq!(runs[0].session, rollout);
    assert_eq!(runs[2].tokens.cache_read, 11_000);
}

/// Accounts and harnesses rank by the headline, never by cache re-reads, and count the
/// agents and runs that used them in the range.
#[test]
fn accounts_and_harnesses_rank_by_the_headline_and_count_their_agents_and_runs() {
    let (_directory, db) = database();
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "s1",
        1000,
        100,
        0,
        0,
    );
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-a",
        "team-8",
        "codex",
        "s2",
        500,
        50,
        0,
        0,
    );
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-b",
        "team-9",
        "claude-code",
        "s3",
        10,
        1,
        9_000_000,
        0,
    );
    used(
        &db,
        MIDNIGHT + 2 * HOUR,
        "alpha",
        "acct-b",
        "team-9",
        "claude-code",
        "s4",
        10,
        1,
        0,
        0,
    );
    used(
        &db,
        MIDNIGHT + HOUR,
        "beta",
        "acct-c",
        "team-1",
        "codex",
        "s5",
        99_999,
        0,
        0,
        0,
    );
    let one = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    let rows = |groups: &[GroupRow]| -> Vec<(String, u64, u64, u64)> {
        groups
            .iter()
            .map(|row| (row.key.clone(), row.tokens.headline(), row.agents, row.runs))
            .collect()
    };
    assert_eq!(
        rows(&one.accounts),
        [("acct-a".into(), 1650, 2, 2), ("acct-b".into(), 22, 1, 2)]
    );
    assert_eq!(
        rows(&one.harnesses),
        [
            ("claude-code".into(), 1122, 2, 3),
            ("codex".into(), 550, 1, 1)
        ]
    );
    assert_eq!(one.accounts[1].tokens.cache_read, 9_000_000);
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(all.accounts[0].key, "acct-c");
    assert_eq!(all.harnesses[0].key, "codex");
}

/// The chart follows the range: two-hour bars for a day, day bars for a week and five-day
/// bars for a month, each anchored at the range start and holding only that project's
/// tokens in the range.
#[test]
fn the_chart_has_two_hour_bars_today_day_bars_for_a_week_and_five_day_bars_for_a_month() {
    let (_directory, db) = database();
    for (hour_ms, project, input) in [
        (MIDNIGHT + HOUR, "alpha", 1),
        (MIDNIGHT + 2 * HOUR, "alpha", 2),
        (MIDNIGHT + 3 * HOUR, "alpha", 4),
        (MIDNIGHT + 9 * HOUR, "alpha", 8),
        (MIDNIGHT - 2 * DAY, "alpha", 16),
        (MIDNIGHT - 12 * DAY, "alpha", 32),
        (MIDNIGHT + HOUR, "beta", 64),
    ] {
        used(
            &db,
            hour_ms,
            project,
            "acct-a",
            "team-7",
            "claude-code",
            "s1",
            input,
            0,
            0,
            0,
        );
    }
    let bars = |since_ms: i64| {
        let read = read(&db, Some(&alpha()), since_ms, NOW).unwrap();
        let series: Vec<(i64, u64)> = read
            .series
            .iter()
            .map(|bar| (bar.start_ms, bar.tokens.input))
            .collect();
        (read.bucket_ms, series)
    };
    assert_eq!(
        bars(MIDNIGHT),
        (
            2 * HOUR,
            vec![
                (MIDNIGHT, 1),
                (MIDNIGHT + 2 * HOUR, 6),
                (MIDNIGHT + 8 * HOUR, 8)
            ]
        )
    );
    assert_eq!(
        bars(MIDNIGHT - 6 * DAY),
        (DAY, vec![(MIDNIGHT - 2 * DAY, 16), (MIDNIGHT, 15)])
    );
    assert_eq!(
        bars(MIDNIGHT - 29 * DAY),
        (
            5 * DAY,
            vec![(MIDNIGHT - 14 * DAY, 32), (MIDNIGHT - 4 * DAY, 31)]
        )
    );
}

/// A store with usage but no team tables still reads its runs, untitled, and counts no
/// tasks.
#[test]
fn a_store_without_team_tables_still_reads_its_runs() {
    let directory = tempfile::tempdir().unwrap();
    let db = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::usage::initialize(&db).unwrap();
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "s1",
        5,
        5,
        0,
        0,
    );
    let read = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!((read.running, read.tasks_done), (0, 0));
    assert_eq!(read.runs.len(), 1);
    assert_eq!(read.runs[0].title, None);
}

/// Runs, accounts and harnesses list at most `ROWS_MAX` rows each: the newest runs and the
/// largest accounts and harnesses.
#[test]
fn at_most_rows_max_runs_accounts_and_harnesses_are_listed() {
    let (_directory, db) = database();
    for at in 0..(ROWS_MAX as i64 + 5) {
        let name = format!("k{at}");
        used(
            &db,
            MIDNIGHT + HOUR + at,
            "alpha",
            &name,
            &name,
            &name,
            &name,
            10 + at,
            0,
            0,
            0,
        );
    }
    let read = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(read.runs.len(), ROWS_MAX);
    assert_eq!(read.accounts.len(), ROWS_MAX);
    assert_eq!(read.harnesses.len(), ROWS_MAX);
    assert_eq!(read.runs[0].agent, format!("k{}", ROWS_MAX + 4));
    assert_eq!(read.accounts[0].key, format!("k{}", ROWS_MAX + 4));
}

/// A range that starts after now is refused.
#[test]
fn a_range_that_starts_after_now_is_refused() {
    let (_directory, db) = database();
    assert_eq!(
        read(&db, None, NOW + 1, NOW).unwrap_err().0,
        "usage_request_invalid"
    );
}
