#![cfg(unix)]

mod e2e_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResult};
use crazyagent::paths::find_in_path;
use crazyagent::protocol::ProjectId;
use crazyagent::usage::capture::Tokens;
use crazyagent::usage::report::{UsageReport, UsageRow};
use crazyagent::usage::service::SCAN_EVERY;
use e2e_support::{Engine, Owner, profile, scratch};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ATTACH_TIME: Duration = Duration::from_secs(10);
const PROJECT: &str = "usage-shop";
const FIRST_TOTAL: (u64, u64, u64) = (1_200, 200, 300);
const SECOND_TOTAL: (u64, u64, u64) = (2_000, 500, 450);
const PROBE_BEFORE: [(u64, u64, u64); 3] = [(100, 0, 10), (200, 0, 20), (300, 0, 30)];
const PROBE_AFTER: [(u64, u64, u64); 3] = [(400, 0, 40), (500, 0, 50), (600, 0, 60)];
const COUNTED_WHEN: &str =
    "the engine counts usage while its team runtime runs, on the registered harness";
const HARNESS_UNAVAILABLE: &str = "UNAVAILABLE: Codex or Claude Code (the engine counts usage only while its team runtime runs, which needs one registered harness)";

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// The real harness the engine registers so its team runtime and usage upkeep run:
/// `CRAZYAGENT_CODEX` or `CRAZYAGENT_CLAUDE` when set, else `codex` or `claude` on PATH.
/// Input: the environment. Output: the engine flag and the resolved executable; panics
/// as UNAVAILABLE when neither is installed.
fn harness() -> (&'static str, PathBuf) {
    let path = std::env::var_os("PATH");
    [
        ("CRAZYAGENT_CODEX", "codex", "--codex-executable"),
        ("CRAZYAGENT_CLAUDE", "claude", "--claude-executable"),
    ]
    .into_iter()
    .find_map(|(variable, name, flag)| {
        let found = match std::env::var_os(variable) {
            Some(named) => Some(
                std::fs::canonicalize(named)
                    .unwrap_or_else(|_| panic!("{HARNESS_UNAVAILABLE}: {variable} names no file")),
            ),
            None => find_in_path(name, path.as_deref()),
        };
        found.map(|found| (flag, found))
    })
    .unwrap_or_else(|| panic!("{HARNESS_UNAVAILABLE}"))
}

fn report_wait() -> Duration {
    SCAN_EVERY * 6
}

fn token_count((input, cached, output): (u64, u64, u64)) -> String {
    serde_json::json!({
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {"total_token_usage": {
                "input_tokens": input,
                "cached_input_tokens": cached,
                "output_tokens": output,
                "total_tokens": input + output,
            }},
        },
    })
    .to_string()
}

fn counted((input, cached, output): (u64, u64, u64)) -> Tokens {
    Tokens {
        input: input - cached,
        output,
        cache_read: cached,
        cache_write: 0,
    }
}

fn add_agent(engine: &Engine, role: &str, harness: &str) -> i64 {
    let connection = rusqlite::Connection::open(engine.path().join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    crazyagent::team::initialize(&connection).unwrap();
    let now = now_ms();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms, stopped_ms)
             VALUES (?1, ?2, ?3, 0, 'stopped', ?4, ?4, ?4)",
            rusqlite::params![PROJECT, role, harness, now],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn private_folder(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn codex_rollout(engine: &Engine, agent: i64) -> PathBuf {
    let home = crazyagent::team::runtime::agent_home(&engine.path().join("team"), agent);
    let day = home.join("sessions").join("2026").join("10").join("28");
    for folder in [home.as_path(), day.as_path()] {
        private_folder(folder);
    }
    day.join("rollout-2026-10-28T07-30-00-0199.jsonl")
}

fn append(path: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

fn row<'a>(report: &'a UsageReport, key: &str) -> Option<&'a UsageRow> {
    report.agents.iter().find(|row| row.key == key)
}

async fn report_until(
    owner: &OwnerController,
    project: &ProjectId,
    what: &str,
    done: impl Fn(&UsageReport) -> bool,
) -> UsageReport {
    let start = Instant::now();
    loop {
        let report = owner
            .usage_report(Some(project.clone()), None, 0)
            .await
            .unwrap();
        if done(&report) {
            return report;
        }
        assert!(
            start.elapsed() < report_wait(),
            "{what} within {:?} ({COUNTED_WHEN}): {report:?}",
            report_wait()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Appends each probe total to the probe agent's record and waits until the page counts it.
/// Each count after the first proves the scan before it finished over every other record.
/// Inputs: the owner client, the project, the probe record and row key, and the totals.
/// Output: the report that counted the last total.
async fn rescanned(
    owner: &OwnerController,
    project: &ProjectId,
    (record, key): (&Path, &str),
    totals: &[(u64, u64, u64)],
) -> UsageReport {
    let mut report = None;
    for &total in totals {
        append(record, &token_count(total));
        report = Some(
            report_until(
                owner,
                project,
                "the next scan counts the probe record",
                |report| row(report, key).is_some_and(|row| row.tokens == counted(total)),
            )
            .await,
        );
    }
    report.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_usage_page_counts_codex_records_once_and_names_a_harness_that_reports_none() {
    let mut engine = Engine::new("ca-av2-usage-");
    engine.harness = Some(harness());
    engine.launch();
    let mut tui = Owner::attach(&engine).await;
    assert_eq!(tui.save_profile(profile()).await, OwnerResult::Applied);
    let disk = scratch("ca-av2-usage-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    assert_eq!(tui.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        tui.create_project(PROJECT, "Usage shop").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new(PROJECT).unwrap();

    let codex = add_agent(&engine, "sub", "codex");
    let gemini = add_agent(&engine, "sub", "gemini-cli");
    let rcli = add_agent(&engine, "master", "rcli");
    let probe = add_agent(&engine, "sub", "codex");
    assert!(codex > 0 && gemini > codex && rcli > gemini && probe > rcli);
    let rollout = codex_rollout(&engine, codex);
    let probe_rollout = codex_rollout(&engine, probe);
    append(&rollout, &token_count(FIRST_TOTAL));

    let codex_key = format!("team-{codex}");
    let probe_key = format!("team-{probe}");
    let gemini_key = format!("team-{gemini}");
    let gemini_name = crazyagent::harnesses::spec("gemini-cli").unwrap().name;
    let rcli_name = crazyagent::harnesses::spec("rcli").unwrap().name;
    let desk = OwnerController::attach(engine.path(), OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap();
    report_until(
        &desk,
        &project,
        "the usage upkeep runs and marks the Gemini CLI agent",
        |report| row(report, &gemini_key).is_some_and(|row| row.not_reported_by.is_some()),
    )
    .await;
    let report = report_until(
        &desk,
        &project,
        "the scan counts the Codex record and marks the Gemini CLI agent",
        |report| {
            row(report, &codex_key).is_some_and(|row| row.tokens == counted(FIRST_TOTAL))
                && row(report, &gemini_key).is_some_and(|row| row.not_reported_by.is_some())
        },
    )
    .await;
    let codex_row = row(&report, &codex_key).unwrap();
    assert_eq!(codex_row.not_reported_by, None, "Codex usage is counted");
    let gemini_row = row(&report, &gemini_key).unwrap();
    assert_eq!(
        gemini_row.not_reported_by.as_deref(),
        Some(gemini_name),
        "the Gemini CLI agent is named as not reporting usage, not shown as zero"
    );
    assert_eq!(
        report.not_reported_by,
        vec![gemini_name.to_owned()],
        "the page names Gemini CLI once and never RCLI, whose master usage comes from its host"
    );
    assert!(!report.not_reported_by.iter().any(|name| name == rcli_name));
    assert_eq!(
        report.total,
        counted(FIRST_TOTAL),
        "the total holds only counted tokens"
    );
    assert_eq!(
        tui.controller
            .usage_report(Some(project.clone()), None, 0)
            .await
            .unwrap()
            .agents,
        report.agents,
        "the TUI reads the same rows as the desktop"
    );

    let rescan = rescanned(&desk, &project, (&probe_rollout, &probe_key), &PROBE_BEFORE).await;
    assert_eq!(
        row(&rescan, &codex_key).map(|row| row.tokens),
        Some(counted(FIRST_TOTAL)),
        "two later scans over the same Codex record add nothing to it"
    );
    assert_eq!(
        rescan.total,
        counted(FIRST_TOTAL).plus(counted(PROBE_BEFORE[2])),
        "the total grows only by the probe record's new lines"
    );

    drop(desk);
    drop(tui);
    engine.restart();
    append(&rollout, &token_count(SECOND_TOTAL));
    let desk = OwnerController::attach(engine.path(), OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap();
    let after = report_until(
        &desk,
        &project,
        "after a restart the scan counts only the growth of the Codex record",
        |report| row(report, &codex_key).is_some_and(|row| row.tokens == counted(SECOND_TOTAL)),
    )
    .await;
    assert_eq!(
        after.total,
        counted(SECOND_TOTAL).plus(counted(PROBE_BEFORE[2])),
        "the restart never counts the first line or the probe lines again"
    );
    assert_eq!(
        row(&after, &gemini_key).and_then(|row| row.not_reported_by.as_deref()),
        Some(gemini_name),
        "the mark survives the restart"
    );

    let rescan = rescanned(&desk, &project, (&probe_rollout, &probe_key), &PROBE_AFTER).await;
    assert_eq!(
        row(&rescan, &codex_key).map(|row| row.tokens),
        Some(counted(SECOND_TOTAL)),
        "later scans after the restart add nothing to the Codex record either"
    );
    assert_eq!(
        rescan.total,
        counted(SECOND_TOTAL).plus(counted(PROBE_AFTER[2])),
        "the probe record is counted once across the restart as well"
    );
    drop(desk);
    engine.stop();
}
