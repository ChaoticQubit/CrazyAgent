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
        project: Some(crate::protocol::ProjectId::new(project).unwrap()),
        pool: pool.map(|pool| CredentialRef::new(pool).unwrap()),
        account: Some(CredentialRef::new(account).unwrap()),
        agent: agent.into(),
        harness: "claude-code".into(),
    }
}

const LANDING: &str =
    "CREATE TABLE landing_change_sets (project TEXT, agent_kind TEXT, agent_id TEXT,
    state TEXT, added INTEGER, removed INTEGER, updated_ms INTEGER);";

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
    db.execute_batch(LANDING).unwrap();
    for (project, state, added, removed, at) in [
        ("alpha", "accepted", 30, 5, MIDNIGHT + HOUR),
        ("alpha", "pending", 10, 2, MIDNIGHT + HOUR),
        ("alpha", "discarded", 999, 999, MIDNIGHT + HOUR),
        ("alpha", "accepted", 50, 50, MIDNIGHT - 3 * DAY),
    ] {
        db.execute(
            "INSERT INTO landing_change_sets VALUES (?1,'sub','1',?2,?3,?4,?5)",
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

    let all = report(&db, None, None, 0, MIDNIGHT + DAY).unwrap();
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

    let today = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY).unwrap();
    assert_eq!(today.total.input, 48);
    assert_eq!(keys(&today.agents), vec!["team-2", "research-r1"]);
    assert_eq!(today.lines[0].change_sets, 2);
    assert_eq!(today.lines[0].commits, 1);

    let east = report(&db, None, Some(MIDNIGHT), 120, MIDNIGHT + DAY).unwrap();
    assert_eq!(
        east.days
            .iter()
            .map(|d| (d.day_ms, d.tokens.input))
            .collect::<Vec<_>>(),
        vec![(MIDNIGHT - 2 * HOUR, 47), (MIDNIGHT + 22 * HOUR, 1)]
    );
    assert_eq!(
        report(&db, None, None, 15 * 60, MIDNIGHT).unwrap_err().0,
        "usage_request_invalid"
    );
    assert_eq!(
        report(&db, None, Some(MIDNIGHT + 1), 0, MIDNIGHT)
            .unwrap_err()
            .0,
        "usage_request_invalid"
    );
}

#[test]
fn runs_tied_to_no_project_or_account_show_as_unattributed_rows() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    append(&home.path().join("projects/f/u.jsonl"), &line("u", 9, 2));
    let lost = Attribution {
        kind: RunKind::Research,
        project: None,
        pool: None,
        account: None,
        agent: "onboarding".into(),
        harness: "claude-code".into(),
    };
    scan_home(&db, home.path(), Format::Claude, &lost, MIDNIGHT).unwrap();
    let all = report(&db, None, None, 0, MIDNIGHT + HOUR).unwrap();
    assert_eq!(all.total.input, 9);
    assert_eq!(
        all.projects
            .iter()
            .map(|r| (r.key.as_str(), r.tokens.input))
            .collect::<Vec<_>>(),
        vec![("", 9)]
    );
    assert_eq!(
        all.accounts
            .iter()
            .map(|r| (r.key.as_str(), r.tokens.input))
            .collect::<Vec<_>>(),
        vec![("", 9)]
    );
    assert!(all.pools.is_empty());
    assert_eq!(all.agents[0].key, "onboarding");
}

#[test]
fn a_project_report_keeps_only_its_runs_and_names_lines_by_the_agent_that_made_them() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let homes = tempfile::tempdir().unwrap();
    for (project, agent, kind, input) in [
        ("alpha", "master-alpha", RunKind::Master, 40),
        ("alpha", "team-3", RunKind::Agent, 20),
        ("beta", "team-9", RunKind::Agent, 500),
    ] {
        let home = homes.path().join(agent);
        append(&home.join("projects/f/s.jsonl"), &line(agent, input, 1));
        scan_home(
            &db,
            &home,
            Format::Claude,
            &who(project, None, "claude-1", agent, kind),
            MIDNIGHT,
        )
        .unwrap();
    }
    db.execute_batch(LANDING).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT);
         INSERT INTO team_agents VALUES (3,'alpha','sub_agent'),(5,'alpha','master'),(9,'beta','sub_agent');",
    )
    .unwrap();
    for (project, kind, agent, state, added, removed) in [
        ("alpha", "sub", "3", "accepted", 10, 2),
        ("alpha", "sub", "3", "discarded", 99, 99),
        ("alpha", "sub", "5", "pending", 4, 1),
        ("alpha", "master", "session-1", "accepted", 7, 0),
        ("beta", "sub", "9", "pending", 100, 100),
    ] {
        db.execute(
            "INSERT INTO landing_change_sets VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![project, kind, agent, state, added, removed, MIDNIGHT],
        )
        .unwrap();
    }
    let alpha = crate::protocol::ProjectId::new("alpha").unwrap();
    let only = report(&db, Some(&alpha), None, 0, MIDNIGHT + HOUR).unwrap();
    assert_eq!(only.total.input, 60);
    assert_eq!(only.runs, 2);
    let keys = |rows: &[UsageRow]| rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&only.projects), vec!["alpha"]);
    assert_eq!(keys(&only.agents), vec!["master-alpha", "team-3"]);
    assert_eq!(only.lines.len(), 1);
    assert_eq!(only.lines[0].added, 21);
    assert_eq!(
        only.agent_lines,
        vec![
            AgentLinesRow {
                agent: "master-alpha".into(),
                change_sets: 2,
                added: 11,
                removed: 1,
                commits: 1,
                committed_added: 7,
                committed_removed: 0,
            },
            AgentLinesRow {
                agent: "team-3".into(),
                change_sets: 1,
                added: 10,
                removed: 2,
                commits: 1,
                committed_added: 10,
                committed_removed: 2,
            },
        ]
    );
    let all = report(&db, None, None, 0, MIDNIGHT + HOUR).unwrap();
    assert_eq!(all.total.input, 560);
    assert_eq!(all.agent_lines[0].agent, "team-9");
    assert_eq!(all.agent_lines.len(), 3);
}

#[test]
fn token_sums_past_the_integer_range_still_report_saturated() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    for (hour_ms, input) in [
        (MIDNIGHT, "9223372036854775807"),
        (MIDNIGHT + HOUR, "9223372036854775807"),
        (MIDNIGHT + 2 * HOUR, "1.8446744073709552e19"),
    ] {
        db.execute(
            &format!(
                "INSERT INTO usage_tokens VALUES
                 (?1,'agent','alpha','pool-a','claude-a','team-1','claude-code','s1',{input},1,0,0)"
            ),
            params![hour_ms],
        )
        .unwrap();
    }
    let report = report(&db, None, None, 0, MIDNIGHT + DAY).unwrap();
    assert_eq!(report.total.input, u64::MAX);
    assert_eq!(report.total.output, 3);
    assert_eq!(report.projects[0].tokens.input, u64::MAX);
    assert_eq!(report.agents[0].tokens.output, 3);
    assert_eq!(report.days[0].tokens.input, u64::MAX);
}
