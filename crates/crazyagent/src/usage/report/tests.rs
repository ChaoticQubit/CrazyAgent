use super::*;
use crate::protocol::CredentialRef;
use crate::usage::capture::{Attribution, Format, RunKind, register_run, scan_home, scan_runs};
use crate::usage::{LimitSource, LimitWindow, Observation, record_observation};
use std::io::Write;
use std::path::Path;

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;
const MIDNIGHT: i64 = 1_792_972_800_000;

fn line(id: &str, input: u64, output: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "message": {"id": id, "usage": {
                "input_tokens": input, "output_tokens": output,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
            }}
        })
    )
}

fn append(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn who(
    project: &str,
    pool: Option<&str>,
    account: &str,
    agent: &str,
    kind: RunKind,
) -> Attribution {
    Attribution {
        kind,
        project: crate::protocol::ProjectId::new(project).unwrap(),
        pool: pool.map(|pool| CredentialRef::new(pool).unwrap()),
        account: CredentialRef::new(account).unwrap(),
        agent: agent.into(),
        harness: "claude-code".into(),
    }
}

#[test]
fn the_report_sums_ranges_groups_local_days_lines_and_limits() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let homes = tempfile::tempdir().unwrap();
    let master = homes.path().join("master");
    let sub = homes.path().join("sub");
    let research = homes.path().join("research");
    append(&master.join("projects/f/m.jsonl"), &line("a", 100, 10));
    scan_home(
        &db,
        &master,
        Format::Claude,
        &who(
            "alpha",
            Some("pool-a"),
            "claude-1",
            "team-1",
            RunKind::Master,
        ),
        MIDNIGHT - 2 * DAY,
    )
    .unwrap();
    append(&sub.join("projects/f/s.jsonl"), &line("b", 40, 4));
    scan_home(
        &db,
        &sub,
        Format::Claude,
        &who(
            "alpha",
            Some("pool-a"),
            "claude-2",
            "team-2",
            RunKind::Agent,
        ),
        MIDNIGHT + 3 * HOUR,
    )
    .unwrap();
    append(&sub.join("projects/f/s.jsonl"), &line("c", 1, 1));
    scan_home(
        &db,
        &sub,
        Format::Claude,
        &who(
            "alpha",
            Some("pool-a"),
            "claude-2",
            "team-2",
            RunKind::Agent,
        ),
        MIDNIGHT + 23 * HOUR,
    )
    .unwrap();
    append(&research.join("projects/f/r.jsonl"), &line("d", 7, 3));
    register_run(
        &db,
        &research,
        &who("beta", None, "claude-1", "research-r1", RunKind::Research),
        MIDNIGHT,
    )
    .unwrap();
    scan_runs(
        &db,
        |project, _| (project == "beta").then(|| CredentialRef::new("pool-b").unwrap()),
        MIDNIGHT + HOUR,
    )
    .unwrap();
    db.execute_batch(
        "CREATE TABLE landing_change_sets (project TEXT, state TEXT, added INTEGER, removed INTEGER, updated_ms INTEGER);",
    )
    .unwrap();
    for (project, state, added, removed, at) in [
        ("alpha", "accepted", 30, 5, MIDNIGHT + HOUR),
        ("alpha", "pending", 10, 2, MIDNIGHT + HOUR),
        ("alpha", "discarded", 999, 999, MIDNIGHT + HOUR),
        ("alpha", "accepted", 50, 50, MIDNIGHT - 3 * DAY),
    ] {
        db.execute(
            "INSERT INTO landing_change_sets VALUES (?1,?2,?3,?4,?5)",
            params![project, state, added, removed, at],
        )
        .unwrap();
    }
    record_observation(
        &db,
        &CredentialRef::new("claude-1").unwrap(),
        &Observation {
            plan: Some("max".into()),
            five_hour: Some(LimitWindow {
                used_percent: 20.0,
                resets_at_ms: Some(MIDNIGHT + 2 * HOUR),
            }),
            weekly: None,
        },
        LimitSource::Poll,
        MIDNIGHT,
    )
    .unwrap();

    let all = report(&db, None, 0, MIDNIGHT + DAY).unwrap();
    assert_eq!(
        all.total,
        Tokens {
            input: 148,
            output: 18,
            cache_read: 0,
            cache_write: 0
        }
    );
    assert_eq!(all.runs, 3);
    let keys = |rows: &[UsageRow]| rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&all.projects), vec!["alpha", "beta"]);
    assert_eq!(keys(&all.pools), vec!["pool-a", "pool-b"]);
    assert_eq!(keys(&all.accounts), vec!["claude-1", "claude-2"]);
    assert_eq!(keys(&all.agents), vec!["team-1", "team-2", "research-r1"]);
    assert_eq!(all.accounts[0].tokens.input, 107);
    assert_eq!(all.accounts[0].runs, 2);
    assert_eq!(all.agents[1].runs, 1);
    assert_eq!(all.agents[1].last_ms, MIDNIGHT + 23 * HOUR);
    assert_eq!(
        all.days
            .iter()
            .map(|d| (d.day_ms, d.tokens.input))
            .collect::<Vec<_>>(),
        vec![(MIDNIGHT - 2 * DAY, 100), (MIDNIGHT, 48)]
    );
    assert_eq!(
        all.lines,
        vec![LinesRow {
            project: "alpha".into(),
            change_sets: 3,
            added: 90,
            removed: 57,
            commits: 2,
            committed_added: 80,
            committed_removed: 55,
        }]
    );
    assert_eq!(all.limits.len(), 1);
    assert_eq!(all.limits[0].plan.as_deref(), Some("max"));

    let today = report(&db, Some(MIDNIGHT), 0, MIDNIGHT + DAY).unwrap();
    assert_eq!(today.total.input, 48);
    assert_eq!(keys(&today.agents), vec!["team-2", "research-r1"]);
    assert_eq!(today.lines[0].change_sets, 2);
    assert_eq!(today.lines[0].commits, 1);

    let east = report(&db, Some(MIDNIGHT), 120, MIDNIGHT + DAY).unwrap();
    assert_eq!(
        east.days
            .iter()
            .map(|d| (d.day_ms, d.tokens.input))
            .collect::<Vec<_>>(),
        vec![(MIDNIGHT - 2 * HOUR, 47), (MIDNIGHT + 22 * HOUR, 1)]
    );
    assert_eq!(
        report(&db, None, 15 * 60, MIDNIGHT).unwrap_err().0,
        "usage_request_invalid"
    );
    assert_eq!(
        report(&db, Some(MIDNIGHT + 1), 0, MIDNIGHT).unwrap_err().0,
        "usage_request_invalid"
    );
}
