use super::*;
use crate::config::ModelAccess;
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::usage::capture::{Attribution, RunKind, count_reported};
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
    created_ms: i64,
    finished_ms: Option<i64>,
) -> i64 {
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,finished_ms)
         VALUES (?1,'owner','claude-code',1,?2,?3,?4,?5,?6)",
        params![project, prompt, status, agent, created_ms, finished_ms],
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
        task(&db, project, 1, status, "Work", MIDNIGHT - DAY, finished_ms);
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

/// Merges count the merges the Overview's Merges filter shows in the range, a lead's merge
/// and the master's merge into main; into main counts the master's, and undone those an Undo
/// merge reverted. An owner's Accept is a change, not a merge. One project counts only its
/// own merges, None counts all.
#[test]
fn merges_count_lead_and_main_merges_in_the_range_with_those_undone() {
    use crate::activity::{ActivityKind, MASTER_ACTOR, NewActivity};
    let (_directory, db) = database();
    db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY)")
        .unwrap();
    crate::activity::initialize(&db).unwrap();
    let beta = ProjectId::new("beta").unwrap();
    for (project, kind, change_set, at_ms) in [
        (
            alpha(),
            ActivityKind::MergedIntoMain,
            "set-1",
            MIDNIGHT + HOUR,
        ),
        (
            alpha(),
            ActivityKind::MergedIntoMain,
            "set-2",
            MIDNIGHT + 2 * HOUR,
        ),
        (
            alpha(),
            ActivityKind::LeadMerged,
            "set-3",
            MIDNIGHT + 3 * HOUR,
        ),
        (
            alpha(),
            ActivityKind::Accepted,
            "set-4",
            MIDNIGHT + 4 * HOUR,
        ),
        (
            alpha(),
            ActivityKind::MergedIntoMain,
            "set-5",
            MIDNIGHT - HOUR,
        ),
        (
            beta.clone(),
            ActivityKind::MergedIntoMain,
            "set-6",
            MIDNIGHT + HOUR,
        ),
    ] {
        let item = NewActivity {
            kind,
            actor: MASTER_ACTOR,
            agent: None,
            goal: None,
            text: "Merged",
            change_set: Some(change_set),
        };
        crate::activity::record(&db, &project, &item, at_ms).unwrap();
    }
    crate::activity::mark_undone(&db, &alpha(), "set-2");
    let one = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    assert_eq!(
        (one.merges, one.merges_into_main, one.merges_undone),
        (3, 2, 1)
    );
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(
        (all.merges, all.merges_into_main, all.merges_undone),
        (4, 3, 1)
    );
}

/// Each project row of the breakdown counts that project's merges in the range as the
/// Merges tile does, a lead's merge and the master's merge into main; account and harness
/// rows count none.
#[test]
fn each_project_row_counts_its_merges_in_the_range() {
    use crate::activity::{ActivityKind, MASTER_ACTOR, NewActivity};
    let (_directory, db) = database();
    db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY)")
        .unwrap();
    crate::activity::initialize(&db).unwrap();
    for project in ["alpha", "beta"] {
        used(
            &db,
            MIDNIGHT + HOUR,
            project,
            "acct-a",
            "team-1",
            "codex",
            "s1",
            10,
            0,
            0,
            0,
        );
    }
    let beta = ProjectId::new("beta").unwrap();
    for (project, kind, change_set, at_ms) in [
        (
            alpha(),
            ActivityKind::MergedIntoMain,
            "set-1",
            MIDNIGHT + HOUR,
        ),
        (
            alpha(),
            ActivityKind::LeadMerged,
            "set-2",
            MIDNIGHT + 2 * HOUR,
        ),
        (
            alpha(),
            ActivityKind::Accepted,
            "set-3",
            MIDNIGHT + 2 * HOUR,
        ),
        (
            alpha(),
            ActivityKind::MergedIntoMain,
            "set-4",
            MIDNIGHT - HOUR,
        ),
        (
            beta.clone(),
            ActivityKind::LeadMerged,
            "set-5",
            MIDNIGHT + HOUR,
        ),
    ] {
        let item = NewActivity {
            kind,
            actor: MASTER_ACTOR,
            agent: None,
            goal: None,
            text: "Merged",
            change_set: Some(change_set),
        };
        crate::activity::record(&db, &project, &item, at_ms).unwrap();
    }
    let merges = |rows: &[GroupRow]| -> Vec<(String, u64)> {
        rows.iter()
            .map(|row| (row.key.clone(), row.merges))
            .collect()
    };
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(
        merges(&all.projects),
        [("alpha".to_owned(), 2), ("beta".to_owned(), 1)]
    );
    assert!(
        all.accounts
            .iter()
            .chain(&all.harnesses)
            .all(|row| row.merges == 0)
    );
    let one = read(&db, Some(&beta), MIDNIGHT, NOW).unwrap();
    assert_eq!(merges(&one.projects), [("beta".to_owned(), 1)]);
}

/// A project with merges in the range and no tokens there still has its row, with zero
/// tokens after the rows that used tokens, so the project rows add up to the Merges tile.
#[test]
fn a_project_with_merges_and_no_tokens_in_the_range_has_its_row() {
    use crate::activity::{ActivityKind, MASTER_ACTOR, NewActivity};
    let (_directory, db) = database();
    db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY)")
        .unwrap();
    crate::activity::initialize(&db).unwrap();
    used(
        &db,
        MIDNIGHT + HOUR,
        "alpha",
        "acct-a",
        "team-1",
        "codex",
        "s1",
        10,
        0,
        0,
        0,
    );
    let gamma = ProjectId::new("gamma").unwrap();
    for (project, kind, change_set) in [
        (alpha(), ActivityKind::MergedIntoMain, "set-1"),
        (gamma.clone(), ActivityKind::MergedIntoMain, "set-2"),
        (gamma.clone(), ActivityKind::LeadMerged, "set-3"),
    ] {
        let item = NewActivity {
            kind,
            actor: MASTER_ACTOR,
            agent: None,
            goal: None,
            text: "Merged",
            change_set: Some(change_set),
        };
        crate::activity::record(&db, &project, &item, MIDNIGHT + HOUR).unwrap();
    }
    let merges_only = GroupRow {
        key: "gamma".to_owned(),
        tokens: Tokens::default(),
        agents: 0,
        runs: 0,
        projects: vec!["gamma".to_owned()],
        more_projects: 0,
        merges: 2,
    };
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    assert_eq!(
        all.projects
            .iter()
            .map(|row| (row.key.as_str(), row.merges))
            .collect::<Vec<_>>(),
        [("alpha", 1), ("gamma", 2)]
    );
    assert_eq!(all.projects[1], merges_only);
    assert_eq!(
        all.projects.iter().map(|row| row.merges).sum::<u64>(),
        all.merges
    );
    let one = read(&db, Some(&gamma), MIDNIGHT, NOW).unwrap();
    assert_eq!(one.projects, [merges_only]);
    assert_eq!(one.merges, 2);
}

/// A store without the activity table counts no merges.
#[test]
fn merges_are_zero_without_the_activity_table() {
    let (_directory, db) = database();
    let metrics = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    assert_eq!(
        (
            metrics.merges,
            metrics.merges_into_main,
            metrics.merges_undone
        ),
        (0, 0, 0)
    );
}

const MINUTE: i64 = 60 * 1000;

/// A project whose team runs Codex and Claude Code sub-agents, as onboarding stores one.
/// Input: the project. Output: the stored setup.
fn setup(project: &str) -> StoredSetup {
    let slot = |harness: &str| HarnessSlot {
        harness: harness.into(),
        agents: 2,
        tools: true,
    };
    StoredSetup {
        project_id: ProjectId::new(project).unwrap(),
        name: "Metrics".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![slot("codex"), slot("claude-code")],
            min_agents: 1,
            max_agents: 4,
        },
        documents: vec![],
        folder: "/private/tmp/unused".into(),
        created_ms: 1,
        revision: 1,
    }
}

/// Queues a team task through the engine's own admission, which records the master's live
/// session on it, never the agent's. Inputs: the store, the setup, the harness, the prompt
/// and the time. Output: the task id.
fn admitted(db: &Connection, setup: &StoredSetup, harness: &str, prompt: &str, at: i64) -> i64 {
    let tx = db.unchecked_transaction().unwrap();
    let id = crate::team::admit(
        &tx,
        &ModelAccess::default(),
        setup,
        Some(harness),
        prompt,
        crate::team::TaskSource::Owner,
        None,
        at,
    )
    .unwrap()
    .unwrap();
    tx.commit().unwrap();
    id
}

/// Starts a task on an agent with the statement the team runtime runs at a start.
/// Inputs: the store, the task, the agent and the time.
fn started(db: &Connection, task: i64, agent: i64, at: i64) {
    db.execute(
        "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL WHERE id=?1",
        params![task, agent, at],
    )
    .unwrap();
}

/// Ends a running task as done with the statement the team runtime runs when its agent
/// reports. Inputs: the store, the task and the time.
fn finished(db: &Connection, task: i64, at: i64) {
    db.execute(
        "UPDATE team_tasks SET status=?2, agent=CASE WHEN ?2='queued' THEN NULL ELSE agent END, result=COALESCE(?3,result), error=?4, finished_ms=?5, output_bytes=?6 WHERE id=?1 AND status='running'",
        params![task, "done", "Done.", None::<String>, at, 5],
    )
    .unwrap();
}

/// Counts tokens a team agent's harness reported in one session, through the attribution
/// and the counting the team runtime uses; agent 0 is the project's master.
/// Inputs: the store, the project, the agent, its harness and account, the session, the
/// input and output tokens and the time.
#[allow(clippy::too_many_arguments)]
fn counted(
    db: &Connection,
    project: &str,
    agent: i64,
    harness: &str,
    account: &str,
    session: &str,
    input: u64,
    output: u64,
    at: i64,
) {
    let attribution = crate::usage::service::team_attribution(
        agent,
        project,
        agent == 0,
        harness,
        Some(account),
        &ModelAccess::default(),
    );
    let tokens = Tokens {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
    };
    count_reported(db, &attribution, tokens, session, at).unwrap();
}

/// A run is one agent session, titled by the latest task its agent ran while the run used
/// tokens: started by the run's last counted tokens and not finished before the run's first
/// hour. The task's session is the master's, so it never decides. Another agent's task, a
/// master run and a run before the range get no title. Newest runs come first. The read for
/// every project titles each project's runs by that project's tasks only.
#[test]
fn runs_are_titled_by_the_task_their_agent_ran_while_they_used_tokens_newest_first() {
    let (_directory, db) = database();
    let alpha_setup = setup("alpha");
    let login = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Fix the login\nwith details",
        MIDNIGHT + 5 * MINUTE,
    );
    started(&db, login, 7, MIDNIGHT + 10 * MINUTE);
    let other = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Another agent's work",
        MIDNIGHT,
    );
    started(&db, other, 9, MIDNIGHT);
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-1",
        100,
        10,
        MIDNIGHT + 20 * MINUTE,
    );
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-1",
        200,
        20,
        MIDNIGHT + 38 * MINUTE,
    );
    finished(&db, other, MIDNIGHT + 30 * MINUTE);
    finished(&db, login, MIDNIGHT + 40 * MINUTE);
    let checkout = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Checkout form validation\nmore words",
        MIDNIGHT + HOUR,
    );
    started(&db, checkout, 7, MIDNIGHT + HOUR + 10 * MINUTE);
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-2",
        300,
        30,
        MIDNIGHT + HOUR + 30 * MINUTE,
    );
    finished(&db, checkout, MIDNIGHT + HOUR + 50 * MINUTE);
    let rename = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Rename the helper",
        MIDNIGHT + 2 * HOUR,
    );
    started(&db, rename, 7, MIDNIGHT + 2 * HOUR + 5 * MINUTE);
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-3",
        40,
        4,
        MIDNIGHT + 2 * HOUR + 8 * MINUTE,
    );
    finished(&db, rename, MIDNIGHT + 2 * HOUR + 10 * MINUTE);
    let retry = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Add the retry",
        MIDNIGHT + 2 * HOUR + 20 * MINUTE,
    );
    started(&db, retry, 7, MIDNIGHT + 2 * HOUR + 30 * MINUTE);
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-4",
        50,
        5,
        MIDNIGHT + 2 * HOUR + 45 * MINUTE,
    );
    finished(&db, retry, MIDNIGHT + 2 * HOUR + 50 * MINUTE);
    counted(
        &db,
        "alpha",
        0,
        "claude-code",
        "acct-a",
        "m-1",
        40,
        4,
        MIDNIGHT + 3 * HOUR,
    );
    let rollout = "rollout-2026-11-07T04-00-00-0199aa";
    let calendar = admitted(
        &db,
        &alpha_setup,
        "codex",
        "Calendar API endpoints",
        MIDNIGHT + 3 * HOUR + 50 * MINUTE,
    );
    started(&db, calendar, 8, MIDNIGHT + 4 * HOUR);
    counted(
        &db,
        "alpha",
        8,
        "codex",
        "acct-b",
        rollout,
        300,
        30,
        MIDNIGHT + 4 * HOUR + 30 * MINUTE,
    );
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "old",
        1,
        1,
        MIDNIGHT - HOUR,
    );
    let beta_docs = admitted(
        &db,
        &setup("beta"),
        "claude-code",
        "Beta docs",
        MIDNIGHT + 3 * HOUR + 40 * MINUTE,
    );
    started(&db, beta_docs, 9, MIDNIGHT + 3 * HOUR + 50 * MINUTE);
    counted(
        &db,
        "beta",
        9,
        "claude-code",
        "acct-a",
        "b-1",
        1,
        1,
        MIDNIGHT + 4 * HOUR,
    );
    let sessions: Vec<Option<String>> = db
        .prepare("SELECT session FROM team_tasks ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(sessions.iter().all(Option::is_none), "{sessions:?}");
    let runs = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap().runs;
    let shown: Vec<_> = runs
        .iter()
        .map(|run| {
            (
                run.agent.as_str(),
                run.session.as_str(),
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
                rollout,
                Some(calendar),
                Some("Calendar API endpoints"),
                Some("running"),
                MIDNIGHT + 4 * HOUR + 30 * MINUTE,
                330
            ),
            (
                "master-alpha",
                "m-1",
                None,
                None,
                None,
                MIDNIGHT + 3 * HOUR,
                44
            ),
            (
                "team-7",
                "c-4",
                Some(retry),
                Some("Add the retry"),
                Some("done"),
                MIDNIGHT + 2 * HOUR + 45 * MINUTE,
                55
            ),
            (
                "team-7",
                "c-3",
                Some(rename),
                Some("Rename the helper"),
                Some("done"),
                MIDNIGHT + 2 * HOUR + 8 * MINUTE,
                44
            ),
            (
                "team-7",
                "c-2",
                Some(checkout),
                Some("Checkout form validation"),
                Some("done"),
                MIDNIGHT + HOUR + 30 * MINUTE,
                330
            ),
            (
                "team-7",
                "c-1",
                Some(login),
                Some("Fix the login"),
                Some("done"),
                MIDNIGHT + 38 * MINUTE,
                330
            ),
        ]
    );
    let (beta, every): (Vec<_>, Vec<_>) = read(&db, None, MIDNIGHT, NOW)
        .unwrap()
        .runs
        .into_iter()
        .partition(|run| run.project == "beta");
    assert_eq!(
        every, runs,
        "the read for every project titles alpha's runs the same"
    );
    let beta: Vec<_> = beta
        .iter()
        .map(|run| {
            (
                run.agent.as_str(),
                run.task,
                run.title.as_deref(),
                run.last_ms,
            )
        })
        .collect();
    assert_eq!(
        beta,
        [(
            "team-9",
            Some(beta_docs),
            Some("Beta docs"),
            MIDNIGHT + 4 * HOUR
        )],
        "beta's run is titled by beta's task, never by alpha's task of the same agent id"
    );
}

/// A run's end is when its tokens were last counted, not the start of that usage hour: a
/// run counted at 10:58 ended at 10:58, and counting it again at 11:03 moves its end there,
/// in the read for one project and in the read for every project.
#[test]
fn a_run_ends_when_its_tokens_were_last_counted_not_at_the_start_of_that_hour() {
    let (_directory, db) = database();
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-1",
        10,
        1,
        MIDNIGHT + 10 * HOUR + 58 * MINUTE,
    );
    let ended = |db: &Connection| read(db, Some(&alpha()), MIDNIGHT, NOW).unwrap().runs[0].last_ms;
    assert_eq!(ended(&db), MIDNIGHT + 10 * HOUR + 58 * MINUTE);
    counted(
        &db,
        "alpha",
        7,
        "claude-code",
        "acct-a",
        "c-1",
        10,
        1,
        MIDNIGHT + 11 * HOUR + 3 * MINUTE,
    );
    assert_eq!(ended(&db), MIDNIGHT + 11 * HOUR + 3 * MINUTE);
    let every = read(&db, None, MIDNIGHT, NOW).unwrap().runs;
    assert_eq!(every[0].last_ms, MIDNIGHT + 11 * HOUR + 3 * MINUTE);
}

/// A run counted before last-use times were kept has only its usage hours, so its task
/// window runs to the end of its last hour: the task its agent started at 10:20 titles the
/// session counted in the 10:00 hour, and a task started at 11:05 does not. Ended stays at
/// that hour's start, the last time known.
#[test]
fn a_run_counted_before_last_use_times_were_kept_is_titled_by_a_task_started_in_its_last_hour() {
    let (_directory, db) = database();
    let alpha_setup = setup("alpha");
    let export = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Fix the export",
        MIDNIGHT + 10 * HOUR + 15 * MINUTE,
    );
    started(&db, export, 7, MIDNIGHT + 10 * HOUR + 20 * MINUTE);
    finished(&db, export, MIDNIGHT + 10 * HOUR + 40 * MINUTE);
    let later = admitted(
        &db,
        &alpha_setup,
        "claude-code",
        "Later work",
        MIDNIGHT + 11 * HOUR,
    );
    started(&db, later, 7, MIDNIGHT + 11 * HOUR + 5 * MINUTE);
    used(
        &db,
        MIDNIGHT + 10 * HOUR,
        "alpha",
        "acct-a",
        "team-7",
        "claude-code",
        "c-1",
        10,
        1,
        0,
        0,
    );
    let runs = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap().runs;
    let shown: Vec<_> = runs
        .iter()
        .map(|run| (run.task, run.title.as_deref(), run.last_ms))
        .collect();
    assert_eq!(
        shown,
        [(Some(export), Some("Fix the export"), MIDNIGHT + 10 * HOUR)]
    );
}

/// A read whose rows would not fit one reply frame keeps the newest runs and the largest
/// accounts and harnesses that do, within `REPORT_BYTES_MAX` encoded bytes: names at their
/// bounds whose control characters each encode to six bytes are counted as encoded.
#[test]
fn a_read_too_large_for_one_reply_keeps_the_newest_runs_and_largest_groups_that_fit() {
    let (_directory, db) = database();
    let wide = |bytes: usize, at: usize| format!("{}{at:02}", "\u{1}".repeat(bytes - 2));
    for at in 0..ROWS_MAX {
        let attribution = Attribution {
            kind: RunKind::Agent,
            project: Some(alpha()),
            pool: None,
            account: None,
            agent: wide(192, at),
            harness: wide(128, at),
        };
        let tokens = Tokens {
            input: 10 + at as u64,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        count_reported(
            &db,
            &attribution,
            tokens,
            &wide(128, at),
            MIDNIGHT + HOUR + at as i64 * MINUTE,
        )
        .unwrap();
    }
    let read = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    let encoded = serde_json::to_vec(&read).unwrap().len();
    assert!(
        encoded <= crate::usage::report::REPORT_BYTES_MAX,
        "{encoded} bytes"
    );
    assert!(
        !read.runs.is_empty() && read.runs.len() < ROWS_MAX,
        "{}",
        read.runs.len()
    );
    assert_eq!(read.runs[0].agent, wide(192, ROWS_MAX - 1));
    assert_eq!(read.harnesses[0].key, wide(128, ROWS_MAX - 1));
}

/// A range that starts before the epoch is refused, so no span or bar start overflows.
#[test]
fn a_range_that_starts_before_the_epoch_is_refused() {
    let (_directory, db) = database();
    for since_ms in [i64::MIN, -1] {
        assert_eq!(
            read(&db, None, since_ms, NOW).unwrap_err().0,
            "usage_request_invalid"
        );
    }
    assert!(read(&db, None, 0, NOW).is_ok());
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

/// The read for every project is the sum of the reads for each project: the same task
/// counts, chart bars, runs, accounts and harnesses. It also ranks projects and agents by the
/// headline, and names each account's and harness's projects and each agent's harnesses and
/// accounts, once each in name order.
#[test]
fn the_read_for_every_project_is_the_sum_of_each_project_and_names_who_used_what() {
    let (_directory, db) = database();
    let beta = ProjectId::new("beta").unwrap();
    task(&db, "alpha", 7, "running", "Alpha work", MIDNIGHT, None);
    task(
        &db,
        "alpha",
        8,
        "done",
        "Alpha done",
        MIDNIGHT,
        Some(MIDNIGHT + HOUR),
    );
    task(&db, "beta", 9, "queued", "Beta waits", MIDNIGHT, None);
    task(
        &db,
        "beta",
        10,
        "failed",
        "Beta failed",
        MIDNIGHT,
        Some(MIDNIGHT + 3 * HOUR),
    );
    for (
        hour_ms,
        project,
        account,
        agent,
        harness,
        session,
        input,
        output,
        cache_read,
        cache_write,
    ) in [
        (
            MIDNIGHT + HOUR,
            "alpha",
            "acct-a",
            "team-7",
            "claude-code",
            "s1",
            1000,
            100,
            50,
            10,
        ),
        (
            MIDNIGHT + HOUR,
            "alpha",
            "acct-b",
            "team-7",
            "claude-code",
            "s1",
            200,
            20,
            0,
            0,
        ),
        (
            MIDNIGHT + 3 * HOUR,
            "alpha",
            "acct-a",
            "team-8",
            "codex",
            "s2",
            300,
            30,
            0,
            0,
        ),
        (
            MIDNIGHT + HOUR,
            "beta",
            "acct-a",
            "team-9",
            "codex",
            "s3",
            400,
            40,
            4000,
            0,
        ),
        (
            MIDNIGHT + 5 * HOUR,
            "beta",
            "acct-c",
            "master-beta",
            "claude-code",
            "m1",
            600,
            60,
            0,
            6,
        ),
    ] {
        used(
            &db,
            hour_ms,
            project,
            account,
            agent,
            harness,
            session,
            input,
            output,
            cache_read,
            cache_write,
        );
    }
    let one = read(&db, Some(&alpha()), MIDNIGHT, NOW).unwrap();
    let two = read(&db, Some(&beta), MIDNIGHT, NOW).unwrap();
    let all = read(&db, None, MIDNIGHT, NOW).unwrap();
    let counts = |m: &Metrics| [m.running, m.tasks_waiting, m.tasks_done, m.tasks_failed];
    let summed: Vec<u64> = counts(&one)
        .iter()
        .zip(counts(&two))
        .map(|(a, b)| a + b)
        .collect();
    assert_eq!(counts(&all).to_vec(), summed);
    let add =
        |into: &mut std::collections::BTreeMap<String, [u64; 6]>, key: String, row: [u64; 6]| {
            let total = into.entry(key).or_default();
            for (sum, value) in total.iter_mut().zip(row) {
                *sum += value;
            }
        };
    let parts = |tokens: &Tokens, agents: u64, runs: u64| {
        [
            tokens.input,
            tokens.output,
            tokens.cache_read,
            tokens.cache_write,
            agents,
            runs,
        ]
    };
    let mut bars = std::collections::BTreeMap::new();
    let mut runs = std::collections::BTreeMap::new();
    let mut accounts = std::collections::BTreeMap::new();
    let mut harnesses = std::collections::BTreeMap::new();
    for m in [&one, &two] {
        for bar in &m.series {
            add(
                &mut bars,
                bar.start_ms.to_string(),
                parts(&bar.tokens, 0, 0),
            );
        }
        for run in &m.runs {
            let key = format!("{}/{}/{}", run.project, run.agent, run.session);
            add(&mut runs, key, parts(&run.tokens, 0, 0));
        }
        for row in &m.accounts {
            add(
                &mut accounts,
                row.key.clone(),
                parts(&row.tokens, row.agents, row.runs),
            );
        }
        for row in &m.harnesses {
            add(
                &mut harnesses,
                row.key.clone(),
                parts(&row.tokens, row.agents, row.runs),
            );
        }
    }
    let keyed = |rows: Vec<(String, [u64; 6])>| {
        rows.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        keyed(
            all.series
                .iter()
                .map(|bar| (bar.start_ms.to_string(), parts(&bar.tokens, 0, 0)))
                .collect()
        ),
        bars
    );
    assert_eq!(
        keyed(
            all.runs
                .iter()
                .map(|run| (
                    format!("{}/{}/{}", run.project, run.agent, run.session),
                    parts(&run.tokens, 0, 0)
                ))
                .collect()
        ),
        runs
    );
    assert_eq!(
        keyed(
            all.accounts
                .iter()
                .map(|row| (row.key.clone(), parts(&row.tokens, row.agents, row.runs)))
                .collect()
        ),
        accounts
    );
    assert_eq!(
        keyed(
            all.harnesses
                .iter()
                .map(|row| (row.key.clone(), parts(&row.tokens, row.agents, row.runs)))
                .collect()
        ),
        harnesses
    );
    let groups = |rows: &[GroupRow]| {
        rows.iter()
            .map(|row| {
                (
                    row.key.clone(),
                    row.tokens.headline(),
                    row.agents,
                    row.runs,
                    row.projects.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    assert_eq!(
        groups(&all.projects),
        [
            ("alpha".into(), 1660, 2, 2, names(&["alpha"])),
            ("beta".into(), 1106, 2, 2, names(&["beta"])),
        ]
    );
    assert_eq!(
        groups(&one.projects),
        [("alpha".into(), 1660, 2, 2, names(&["alpha"]))]
    );
    assert_eq!(
        groups(&all.accounts),
        [
            ("acct-a".into(), 1880, 3, 3, names(&["alpha", "beta"])),
            ("acct-c".into(), 666, 1, 1, names(&["beta"])),
            ("acct-b".into(), 220, 1, 1, names(&["alpha"])),
        ]
    );
    assert_eq!(all.harnesses[0].projects, names(&["alpha", "beta"]));
    let agents = all
        .agents
        .iter()
        .map(|row| {
            (
                row.project.clone(),
                row.agent.clone(),
                row.tokens.headline(),
                row.harnesses.clone(),
                row.accounts.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        agents,
        [
            (
                "alpha".into(),
                "team-7".into(),
                1330,
                names(&["claude-code"]),
                names(&["acct-a", "acct-b"])
            ),
            (
                "beta".into(),
                "master-beta".into(),
                666,
                names(&["claude-code"]),
                names(&["acct-c"])
            ),
            (
                "beta".into(),
                "team-9".into(),
                440,
                names(&["codex"]),
                names(&["acct-a"])
            ),
            (
                "alpha".into(),
                "team-8".into(),
                330,
                names(&["codex"]),
                names(&["acct-a"])
            ),
        ]
    );
    assert_eq!(all.agents[2].tokens.cache_read, 4000);
    assert!(one.agents.iter().all(|row| row.project == "alpha"));
    assert_eq!(one.agents.len(), 2);
}

/// Each account and harness names at most `NAMES_MAX` projects and each agent at most
/// `NAMES_MAX` harnesses and accounts, the first in name order, and counts the rest, so the
/// read for every project stays small however many projects share an account.
#[test]
fn name_lists_keep_the_first_eight_names_in_name_order_and_count_the_rest() {
    let (_directory, db) = database();
    for at in (0..11).rev() {
        let project = format!("p-{at:02}");
        used(
            &db,
            MIDNIGHT + HOUR,
            &project,
            "acct-a",
            "team-1",
            "codex",
            "s1",
            1,
            1,
            0,
            0,
        );
    }
    for at in (0..10).rev() {
        let account = format!("acct-{at:02}");
        used(
            &db,
            MIDNIGHT + 2 * HOUR,
            "p-00",
            &account,
            "team-7",
            "codex",
            "s7",
            1,
            1,
            0,
            0,
        );
    }
    let read = read(&db, None, MIDNIGHT, NOW).unwrap();
    let first = |stem: &str| {
        (0..NAMES_MAX)
            .map(|at| format!("{stem}-{at:02}"))
            .collect::<Vec<_>>()
    };
    let account = read
        .accounts
        .iter()
        .find(|row| row.key == "acct-a")
        .unwrap();
    assert_eq!((&account.projects, account.more_projects), (&first("p"), 3));
    let codex = read
        .harnesses
        .iter()
        .find(|row| row.key == "codex")
        .unwrap();
    assert_eq!((&codex.projects, codex.more_projects), (&first("p"), 3));
    let agent = read
        .agents
        .iter()
        .find(|row| row.agent == "team-7")
        .unwrap();
    assert_eq!(
        (
            &agent.accounts,
            agent.more_accounts,
            &agent.harnesses,
            agent.more_harnesses
        ),
        (&first("acct"), 2, &vec!["codex".to_owned()], 0)
    );
}

/// The read for every project fits one reply frame too: with more projects and agents than
/// `ROWS_MAX`, each agent named at its bound and using accounts named at theirs, all made of
/// characters that encode to six bytes, it keeps the largest projects and agents that fit
/// within `REPORT_BYTES_MAX`.
#[test]
fn a_read_for_every_project_too_large_for_one_reply_keeps_the_largest_projects_and_agents_that_fit()
{
    let (_directory, db) = database();
    let wide = |bytes: usize, at: usize| format!("{}{at:02}", "\u{1}".repeat(bytes - 2));
    for at in 0..ROWS_MAX + 5 {
        for account in 0..NAMES_MAX {
            used(
                &db,
                MIDNIGHT + HOUR,
                &format!("p-{at:02}"),
                &wide(128, account),
                &wide(192, at),
                "codex",
                "s",
                10 + at as i64,
                0,
                0,
                0,
            );
        }
    }
    let read = read(&db, None, MIDNIGHT, NOW);
    assert!(read.is_ok(), "{:?}", read.err());
    let read = read.unwrap();
    let encoded = serde_json::to_vec(&read).unwrap().len();
    assert!(
        encoded <= crate::usage::report::REPORT_BYTES_MAX,
        "{encoded} bytes"
    );
    assert!(
        !read.agents.is_empty() && read.agents.len() < ROWS_MAX,
        "{}",
        read.agents.len()
    );
    assert_eq!(read.agents[0].agent, wide(192, ROWS_MAX + 4));
    assert_eq!(read.projects[0].key, format!("p-{:02}", ROWS_MAX + 4));
}

/// Starts a sub-agent with the statement the team runtime runs at a spawn, then sets its
/// status. Inputs: the store, the project, the harness and the status. Output: its id.
fn sub_agent(db: &Connection, project: &str, harness: &str, status: &str) -> i64 {
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,parent,mode)
         VALUES (?1,?2,?3,?4,?5,'starting',?6,?6,?7,(SELECT mode FROM team_tasks WHERE id=?8 AND project=?1))",
        params![project, "sub", harness, true, "conn-a", MIDNIGHT, None::<i64>, None::<i64>],
    )
    .unwrap();
    let id = db.last_insert_rowid();
    db.execute(
        "UPDATE team_agents SET status=?2 WHERE id=?1",
        params![id, status],
    )
    .unwrap();
    id
}

/// Runs and agents carry the name every client shows for their agent: a sub-agent's persona
/// and role handle, "Sub-agent N" with no name kept, "Master" for a project's master, the
/// number only for a finished sub-agent whose persona a running one now holds (DEC-111 Q19),
/// and none for an agent the store no longer has. One read names every project's agents.
#[test]
fn runs_and_agents_carry_the_name_every_client_shows_for_their_agent() {
    let (_directory, db) = database();
    let named = sub_agent(&db, "alpha", "claude-code", "working");
    let persona = crate::team::names::assign_persona(&db, &alpha(), named).unwrap();
    let handle = crate::team::names::assign_role(&db, &alpha(), named, "onboarding fix")
        .unwrap()
        .unwrap();
    let plain = sub_agent(&db, "beta", "codex", "idle");
    let finished = sub_agent(&db, "alpha", "codex", "stopped");
    crate::team::names::assign_persona(&db, &alpha(), finished).unwrap();
    let running = sub_agent(&db, "alpha", "codex", "working");
    crate::team::names::assign_persona(&db, &alpha(), running).unwrap();
    db.execute(
        "UPDATE team_names SET persona=(SELECT persona FROM team_names WHERE agent=?1) WHERE agent=?2",
        params![finished, running],
    )
    .unwrap();
    let held = crate::team::names::name(&db, running)
        .unwrap()
        .unwrap()
        .persona;
    for (project, agent, session) in [
        ("alpha", named, "s-named"),
        ("beta", plain, "s-plain"),
        ("alpha", finished, "s-finished"),
        ("alpha", running, "s-running"),
        ("alpha", 0, "s-master"),
        ("alpha", 999, "s-gone"),
    ] {
        counted(
            &db,
            project,
            agent,
            "codex",
            "acct-a",
            session,
            10,
            1,
            MIDNIGHT + HOUR,
        );
    }
    let expected = std::collections::BTreeMap::from([
        (
            format!("team-{named}"),
            Some(format!("{persona} (@{handle})")),
        ),
        (format!("team-{plain}"), Some(format!("Sub-agent {plain}"))),
        (
            format!("team-{finished}"),
            Some(format!("Sub-agent {finished}")),
        ),
        (format!("team-{running}"), Some(held)),
        ("master-alpha".to_owned(), Some("Master".to_owned())),
        ("team-999".to_owned(), None),
    ]);
    let read = read(&db, None, MIDNIGHT, NOW).unwrap();
    let runs: std::collections::BTreeMap<_, _> = read
        .runs
        .iter()
        .map(|run| (run.agent.clone(), run.label.clone()))
        .collect();
    assert_eq!(runs, expected);
    let agents: std::collections::BTreeMap<_, _> = read
        .agents
        .iter()
        .map(|row| (row.agent.clone(), row.label.clone()))
        .collect();
    assert_eq!(agents, expected);
}
