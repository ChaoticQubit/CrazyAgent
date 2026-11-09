use super::*;
use crate::protocol::CredentialRef;
use crate::usage::capture::{Attribution, Format, RunKind, register_run, scan_home, scan_runs};
use crate::usage::{LimitSource, LimitWindow, Observation, record_failure, record_observation};
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
    let claude_1 = CredentialRef::new("claude-1").unwrap();
    for account in [&claude_1, &CredentialRef::new("claude-gone").unwrap()] {
        record_observation(
            &db,
            account,
            &Observation {
                plan: Some("max".into()),
                five_hour: Some(LimitWindow {
                    used_percent: 20.0,
                    resets_at_ms: Some(MIDNIGHT + 2 * HOUR),
                }),
                weekly: None,
                provider: Some(crate::usage::poll::Provider::Claude),
            },
            LimitSource::Poll,
            MIDNIGHT,
        )
        .unwrap();
    }

    let all = report(
        &db,
        None,
        None,
        0,
        MIDNIGHT + DAY,
        std::slice::from_ref(&claude_1),
    )
    .unwrap();
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
    assert_eq!(all.limits[0].account, claude_1);
    assert_eq!(all.limits[0].plan.as_deref(), Some("max"));

    let today = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(today.total.input, 48);
    assert_eq!(keys(&today.agents), vec!["team-2", "research-r1"]);
    assert_eq!(today.lines[0].change_sets, 2);
    assert_eq!(today.lines[0].commits, 1);

    let east = report(&db, None, Some(MIDNIGHT), 120, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(
        east.days
            .iter()
            .map(|d| (d.day_ms, d.tokens.input))
            .collect::<Vec<_>>(),
        vec![(MIDNIGHT - 2 * HOUR, 47), (MIDNIGHT + 22 * HOUR, 1)]
    );
    assert_eq!(
        report(&db, None, None, 15 * 60, MIDNIGHT, &[])
            .unwrap_err()
            .0,
        "usage_request_invalid"
    );
    assert_eq!(
        report(&db, None, Some(MIDNIGHT + 1), 0, MIDNIGHT, &[])
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
    let all = report(&db, None, None, 0, MIDNIGHT + HOUR, &[]).unwrap();
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
    let only = report(&db, Some(&alpha), None, 0, MIDNIGHT + HOUR, &[]).unwrap();
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
    let all = report(&db, None, None, 0, MIDNIGHT + HOUR, &[]).unwrap();
    assert_eq!(all.total.input, 560);
    assert_eq!(all.agent_lines[0].agent, "team-9");
    assert_eq!(all.agent_lines.len(), 3);
}

fn cached_line(id: &str, input: u64, output: u64, read: u64, written: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "message": {"id": id, "usage": {
                "input_tokens": input, "output_tokens": output,
                "cache_read_input_tokens": read, "cache_creation_input_tokens": written
            }}
        })
    )
}

/// The headline counts new input, cache writes and output once per message and never the
/// cache re-reads (DEC-117 Q6), and rows rank by it: an agent that mostly re-read its
/// cache, as Ken did on HackerBlog (428,000 headline, 17.67 M re-read), ranks below one
/// with a larger headline, while all four counts stay in the report.
#[test]
fn rows_count_and_rank_by_the_headline_and_never_by_cache_reads() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let homes = tempfile::tempdir().unwrap();
    let ken = homes.path().join("ken");
    let ada = homes.path().join("ada");
    let record = ken.join("projects/f/k.jsonl");
    for _ in 0..3 {
        append(&record, &cached_line("k1", 100, 70_000, 182_114, 150_000));
    }
    append(&record, &cached_line("k2", 94, 69_484, 17_482_981, 138_322));
    let at = MIDNIGHT + HOUR;
    scan_home(
        &db,
        &ken,
        Format::Claude,
        &who("alpha", None, "claude-1", "team-ken", RunKind::Agent),
        at,
    )
    .unwrap();
    append(&ada.join("projects/f/a.jsonl"), &line("a1", 500_000, 0));
    scan_home(
        &db,
        &ada,
        Format::Claude,
        &who("alpha", None, "claude-2", "team-ada", RunKind::Agent),
        at,
    )
    .unwrap();
    let report = report(&db, None, None, 0, at + HOUR, &[]).unwrap();
    let keys: Vec<&str> = report.agents.iter().map(|row| row.key.as_str()).collect();
    assert_eq!(keys, ["team-ada", "team-ken"]);
    let ken = &report.agents[1].tokens;
    assert_eq!(
        *ken,
        Tokens {
            input: 194,
            output: 139_484,
            cache_read: 17_665_095,
            cache_write: 288_322,
        }
    );
    assert_eq!(ken.headline(), 428_000);
    assert_eq!(report.agents[0].tokens.headline(), 500_000);
    assert_eq!(report.total.headline(), 928_000);
    assert_eq!(report.total.cache_read, 17_665_095);
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
    let report = report(&db, None, None, 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(report.total.input, u64::MAX);
    assert_eq!(report.total.output, 3);
    assert_eq!(report.projects[0].tokens.input, u64::MAX);
    assert_eq!(report.agents[0].tokens.output, 3);
    assert_eq!(report.days[0].tokens.input, u64::MAX);
}

fn readerless(project: &str, agent: &str, harness: &str, kind: RunKind) -> Attribution {
    Attribution {
        harness: harness.into(),
        ..who(project, None, "key-1", agent, kind)
    }
}

fn flagged(rows: &[UsageRow]) -> Vec<(&str, u64, Option<&str>)> {
    rows.iter()
        .map(|row| {
            (
                row.key.as_str(),
                row.tokens.input,
                row.not_reported_by.as_deref(),
            )
        })
        .collect()
}

#[test]
fn harness_without_reader_reports_not_reported_never_zero() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    assert!(Format::of("opencode").is_none() && Format::of("gemini-cli").is_none());
    append(&home.path().join("projects/f/a.jsonl"), &line("a", 70, 7));
    scan_home(
        &db,
        home.path(),
        Format::Claude,
        &who(
            "alpha",
            Some("pool-a"),
            "claude-1",
            "team-1",
            RunKind::Agent,
        ),
        MIDNIGHT + HOUR,
    )
    .unwrap();
    let opencode = readerless("alpha", "team-4", "opencode", RunKind::Agent);
    for at in [MIDNIGHT + HOUR, MIDNIGHT + HOUR + 5, MIDNIGHT + 3 * HOUR] {
        mark_unreported(&db, &opencode, at).unwrap();
    }
    let gemini = readerless("alpha", "team-1", "gemini-cli", RunKind::Agent);
    mark_unreported(&db, &gemini, MIDNIGHT + 2 * HOUR).unwrap();
    let beta = readerless("beta", "master-beta", "gemini-cli", RunKind::Master);
    mark_unreported(&db, &beta, MIDNIGHT + 4 * HOUR).unwrap();
    let old = readerless("alpha", "team-5", "opencode", RunKind::Agent);
    mark_unreported(&db, &old, MIDNIGHT - 3 * DAY).unwrap();
    let hours: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usage_unreported WHERE agent='team-4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hours, 2, "one row per hour an agent ran");

    let all = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(all.total.input, 70, "unreported runs add no tokens");
    assert_eq!(all.not_reported_by, ["Gemini CLI", "OpenCode"]);
    assert_eq!(
        flagged(&all.agents),
        [
            ("team-1", 70, Some("Gemini CLI")),
            ("master-beta", 0, Some("Gemini CLI")),
            ("team-4", 0, Some("OpenCode")),
        ],
        "a counted agent keeps its tokens; a readerless one is flagged, never a bare zero"
    );
    let four = all.agents.iter().find(|row| row.key == "team-4").unwrap();
    assert_eq!(four.last_ms, MIDNIGHT + 3 * HOUR);
    assert!(
        !all.agents.iter().any(|row| row.key == "team-5"),
        "outside the range"
    );
    assert_eq!(
        flagged(&all.projects),
        [
            ("alpha", 70, Some("OpenCode")),
            ("beta", 0, Some("Gemini CLI"))
        ]
    );
    assert!(all.pools.iter().all(|row| row.not_reported_by.is_none()));
    assert!(all.accounts.iter().all(|row| row.not_reported_by.is_none()));

    let only = crate::protocol::ProjectId::new("beta").unwrap();
    let beta_report = report(&db, Some(&only), Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(beta_report.total, Tokens::default());
    assert_eq!(beta_report.not_reported_by, ["Gemini CLI"]);
    assert_eq!(
        flagged(&beta_report.agents),
        [("master-beta", 0, Some("Gemini CLI"))]
    );
    let earlier = report(&db, None, Some(MIDNIGHT - 4 * DAY), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert!(earlier.agents.iter().any(|row| row.key == "team-5"));

    let counted = who("alpha", None, "claude-1", "team-2", RunKind::Agent);
    let empty = readerless("alpha", "", "opencode", RunKind::Agent);
    let unnamed = readerless("alpha", "team-6", "", RunKind::Agent);
    for refused in [&counted, &empty, &unnamed] {
        assert_eq!(
            mark_unreported(&db, refused, MIDNIGHT).unwrap_err().0,
            "usage_attribution_invalid"
        );
    }
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_unreported", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 5, "a refused mark writes nothing");
}

#[test]
fn rcli_counted_from_host_frames_is_reported_and_never_marked_unreported() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    assert!(Format::of("rcli").is_none() && reports_usage("rcli"));
    assert!(reports_usage("claude-code") && reports_usage("codex"));
    assert!(!reports_usage("opencode") && !reports_usage(""));
    let rcli = readerless("alpha", "master-alpha", "rcli", RunKind::Master);
    crate::usage::capture::count_reported(
        &db,
        &rcli,
        Tokens {
            input: 300,
            output: 40,
            cache_read: 0,
            cache_write: 0,
        },
        "session-1",
        MIDNIGHT + HOUR,
    )
    .unwrap();
    assert_eq!(
        mark_unreported(&db, &rcli, MIDNIGHT + HOUR).unwrap_err().0,
        "usage_attribution_invalid"
    );
    let opencode = readerless("alpha", "team-4", "opencode", RunKind::Agent);
    mark_unreported(&db, &opencode, MIDNIGHT + HOUR).unwrap();

    let all = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(all.total.input, 300);
    assert_eq!(all.not_reported_by, ["OpenCode"]);
    assert_eq!(
        flagged(&all.agents),
        [("master-alpha", 300, None), ("team-4", 0, Some("OpenCode"))],
        "a counted RCLI master is never labelled not reported; a readerless agent still is"
    );
}

#[test]
fn a_report_from_an_engine_without_step_health_reads_as_healthy() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    crate::usage::health::record_panic(&db, crate::usage::health::UsageStep::Poll, 7).unwrap();
    let report = report(&db, None, None, 0, MIDNIGHT, &[]).unwrap();
    assert_eq!(report.health.len(), 1);
    let mut older = serde_json::to_value(&report).unwrap();
    older.as_object_mut().unwrap().remove("health");
    let read: UsageReport = serde_json::from_value(older).unwrap();
    assert!(read.health.is_empty());
    assert_eq!(
        serde_json::to_value(&report.health).unwrap(),
        serde_json::json!([{"step": "poll", "panics": 1, "last_ms": 7, "recovered_ms": null}])
    );
}

#[test]
fn a_flagged_key_outside_the_top_rows_keeps_its_full_count() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let listed = ROWS_MAX as u64 + 1;
    for index in 1..=listed {
        let project = format!("p-{index:02}");
        let agent = format!("team-{index}");
        crate::usage::capture::count_reported(
            &db,
            &who(&project, None, "claude-1", &agent, RunKind::Agent),
            Tokens {
                input: 1000 - index,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            &format!("s-{index}"),
            MIDNIGHT + HOUR,
        )
        .unwrap();
    }
    let last = format!("team-{listed}");
    let last_project = format!("p-{listed:02}");
    let trimmed = readerless(&last_project, &last, "opencode", RunKind::Agent);
    mark_unreported(&db, &trimmed, MIDNIGHT + 2 * HOUR + 5).unwrap();
    let readerless_only = readerless("p-01", "team-99", "opencode", RunKind::Agent);
    mark_unreported(&db, &readerless_only, MIDNIGHT + HOUR + 5).unwrap();

    let all = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(all.agents.len(), ROWS_MAX + 2);
    assert!(
        all.agents[..ROWS_MAX]
            .iter()
            .all(|row| row.not_reported_by.is_none())
    );
    assert_eq!(
        flagged(&all.agents[ROWS_MAX..]),
        [
            (last.as_str(), 1000 - listed, Some("OpenCode")),
            ("team-99", 0, Some("OpenCode")),
        ],
        "the trimmed agent shows the tokens it counted, not a bare not reported"
    );
    let row = &all.agents[ROWS_MAX];
    assert_eq!(row.runs, 1);
    assert_eq!(row.last_ms, MIDNIGHT + 2 * HOUR + 5);

    assert_eq!(all.projects.len(), ROWS_MAX + 1);
    assert_eq!(
        flagged(&all.projects[..1]),
        [("p-01", 999, Some("OpenCode"))],
        "a listed project is flagged in place"
    );
    assert_eq!(
        flagged(&all.projects[ROWS_MAX..]),
        [(last_project.as_str(), 1000 - listed, Some("OpenCode"))],
        "a trimmed project keeps its full count too"
    );

    let only = crate::protocol::ProjectId::new(last_project.as_str()).unwrap();
    let alone = report(&db, Some(&only), Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert_eq!(
        flagged(&alone.agents),
        [(last.as_str(), 1000 - listed, Some("OpenCode"))]
    );
}

#[test]
fn trimmed_flagged_keys_are_counted_together_and_an_unattributed_agent_mark_stays_out() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let counted = ROWS_MAX as u64 + 3;
    for index in 1..=counted {
        crate::usage::capture::count_reported(
            &db,
            &who(
                "p-1",
                None,
                "claude-1",
                &format!("team-{index}"),
                RunKind::Agent,
            ),
            Tokens {
                input: 1000 - index,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            &format!("s-{index}"),
            MIDNIGHT + HOUR,
        )
        .unwrap();
    }
    let first = format!("team-{}", ROWS_MAX + 1);
    let second = format!("team-{}", ROWS_MAX + 2);
    let marks = [
        (first.as_str(), "opencode", MIDNIGHT + HOUR + 5),
        (first.as_str(), "gemini-cli", MIDNIGHT + 4 * HOUR),
        (second.as_str(), "opencode", MIDNIGHT + 3 * HOUR),
        ("team-99", "opencode", MIDNIGHT + 2 * HOUR),
    ];
    for (agent, harness, at) in marks {
        mark_unreported(&db, &readerless("p-1", agent, harness, RunKind::Agent), at).unwrap();
    }
    db.execute(
        "INSERT INTO usage_unreported(hour_ms,kind,project,agent,harness,last_ms)
         VALUES (?1,'agent','p-1','','opencode',?2)",
        params![MIDNIGHT + 5 * HOUR, MIDNIGHT + 5 * HOUR + 1],
    )
    .unwrap();

    let all = report(&db, None, Some(MIDNIGHT), 0, MIDNIGHT + DAY, &[]).unwrap();
    assert!(
        !all.agents.iter().any(|row| row.key.is_empty()),
        "the agents list leaves the unattributed key out, marked or not"
    );
    assert_eq!(all.agents.len(), ROWS_MAX + 3);
    assert_eq!(
        flagged(&all.agents[ROWS_MAX..]),
        [
            (
                first.as_str(),
                1000 - (ROWS_MAX as u64 + 1),
                Some("Gemini CLI")
            ),
            (
                second.as_str(),
                1000 - (ROWS_MAX as u64 + 2),
                Some("OpenCode")
            ),
            ("team-99", 0, Some("OpenCode")),
        ],
        "each trimmed key read in the one grouped count keeps its own tokens, once, under its newest harness"
    );
    let row = &all.agents[ROWS_MAX];
    assert_eq!((row.runs, row.last_ms), (1, MIDNIGHT + 4 * HOUR));
    assert!(
        !all.agents
            .iter()
            .any(|row| row.key == format!("team-{counted}")),
        "a trimmed key without a mark stays trimmed"
    );
    assert_eq!(
        flagged(&all.projects),
        [(
            "p-1",
            (1..=counted).map(|index| 1000 - index).sum::<u64>(),
            Some("OpenCode")
        )],
        "the project column keeps its unattributed rule and its place"
    );
}

/// How many accounts, runs and days `fill_past_one_frame` writes: past every cap.
pub(crate) const WIDE_ROWS: usize = 70;

/// Names one row `fill_past_one_frame` writes: the prefix and index padded to the
/// 128-byte identity limit. Inputs: the prefix and the index. Output: the name.
pub(crate) fn wide(prefix: &str, index: usize) -> String {
    format!("{:x<128}", format!("{prefix}-{index:03}-"))
}

/// Fills a usage store past what one reply frame holds: `WIDE_ROWS` saved accounts' limits
/// with every field at its widest, and one run on each of `WIDE_ROWS` days with its own
/// 128-byte project, pool, account and agent, the newest run spending the most tokens.
/// Inputs: the store and the time of the newest run.
pub(crate) fn fill_past_one_frame(db: &Connection, now_ms: i64) {
    let percent = 100.0_f64 / 3.0;
    crate::storage::accounts::initialize(db).unwrap();
    for index in 0..WIDE_ROWS {
        save_account(db, &wide("account", index));
        db.execute(
            "INSERT INTO account_limits (account,plan,five_hour_percent,five_hour_resets_ms,
                weekly_percent,weekly_resets_ms,five_hour_reset_override_ms,
                weekly_reset_override_ms,source,observed_ms,checked_ms,failure,
                five_hour_polled_percent,five_hour_polled_resets_ms,weekly_polled_percent,
                weekly_polled_resets_ms)
             VALUES (?1,?2,?3,?4,?3,?4,?4,?4,'transcript',?5,?5,?6,?3,?4,?3,?4)",
            params![
                wide("account", index),
                "p".repeat(crate::usage::PLAN_BYTES_MAX),
                percent,
                crate::usage::RESET_MS_MAX,
                now_ms,
                "f".repeat(crate::usage::FAILURE_BYTES_MAX),
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO usage_tokens VALUES
             (?1,'agent',?2,?3,?4,?5,'claude-code','s',?6,?6,?6,?6)",
            params![
                now_ms - index as i64 * DAY,
                wide("project", index),
                wide("pool", index),
                wide("account", index),
                wide("agent", index),
                spent(index),
            ],
        )
        .unwrap();
    }
}

/// Saves one API key account in the accounts table as Settings > Accounts stores it.
/// Inputs: the engine store and the account id.
fn save_account(db: &Connection, id: &str) {
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let connection = crate::config::ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("local-account"),
        endpoint: crate::config::ModelEndpoint::Http {
            url: "http://127.0.0.1:8000/v1".into(),
        },
        model: crate::config::ModelReference {
            id: "m".into(),
            author: reference("openai-compatible"),
        },
        auth: crate::config::ModelAuthentication::ApiKey {
            credential_ref: reference("wide-key"),
        },
    };
    let details = crate::accounts::AccountDetails {
        name: "Wide".into(),
        provider: crate::accounts::Provider::OpenaiCompatible,
    };
    let body =
        serde_json::to_vec(&serde_json::json!({"connection": connection, "details": details}))
            .unwrap();
    db.execute(
        "INSERT INTO accounts(id, body) VALUES (?1, ?2)",
        params![id, body],
    )
    .unwrap();
}

/// Tokens the run at `index` spent: near the integer range, falling with the index.
/// Input: the index. Output: the tokens.
fn spent(index: usize) -> i64 {
    i64::MAX / 4 - index as i64 * 1_000_000_000_000
}

#[test]
fn an_offset_beyond_fourteen_hours_is_refused_even_at_the_integer_edges() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    for offset in [i32::MIN, i32::MIN + 1, -841, 841, i32::MAX] {
        assert_eq!(
            report(&db, None, None, offset, MIDNIGHT, &[])
                .unwrap_err()
                .0,
            "usage_request_invalid",
            "offset {offset}"
        );
    }
    for offset in [-840, 0, 840] {
        assert!(
            report(&db, None, None, offset, MIDNIGHT, &[]).is_ok(),
            "offset {offset}"
        );
    }
}

#[test]
fn a_report_past_one_reply_frame_keeps_its_largest_rows_and_newest_days_and_fits() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    db.execute_batch(LANDING).unwrap();
    let now = MIDNIGHT + DAY;
    fill_past_one_frame(&db, now);
    for index in 0..WIDE_ROWS {
        db.execute(
            "INSERT INTO landing_change_sets VALUES (?1,'sub',?2,'accepted',?3,?3,?4)",
            params![wide("project", index), index.to_string(), spent(index), now],
        )
        .unwrap();
    }

    let report = report(&db, None, None, 0, now, &[]).unwrap();
    let encoded = serde_json::to_vec(&report).unwrap();
    assert!(encoded.len() <= REPORT_BYTES_MAX, "{} bytes", encoded.len());
    assert!(
        crate::protocol::encode_frame(&serde_json::json!({ "kind": "report", "report": &report }))
            .is_ok(),
        "the reply that carries it fits one frame"
    );
    assert!(
        !report.limits.is_empty() && report.limits.len() < LIMITS_MAX,
        "the widest report is trimmed to fit: {} limits",
        report.limits.len()
    );
    assert_eq!(report.limits[0].account.as_str(), wide("account", 0));
    for rows in [
        &report.projects,
        &report.pools,
        &report.accounts,
        &report.agents,
    ] {
        assert!(!rows.is_empty() && rows.len() < ROWS_MAX, "{}", rows.len());
    }
    assert_eq!(
        report.projects[0].key,
        wide("project", 0),
        "the largest project stays first"
    );
    assert_eq!(report.lines[0].project, wide("project", 0));
    assert!(!report.days.is_empty() && report.days.len() < DAYS_MAX);
    assert_eq!(
        report.days.last().unwrap().day_ms,
        now,
        "the newest day stays"
    );
}

#[test]
fn a_window_past_its_reset_reads_as_not_read_when_the_last_read_failed() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let now = MIDNIGHT + 2 * HOUR;
    let passed = LimitWindow {
        used_percent: 40.0,
        resets_at_ms: Some(MIDNIGHT + HOUR),
    };
    let ahead = LimitWindow {
        used_percent: 60.0,
        resets_at_ms: Some(now + DAY),
    };
    let held: Vec<CredentialRef> = ["failed", "healthy", "mixed"]
        .iter()
        .map(|id| CredentialRef::new(*id).unwrap())
        .collect();
    for (account, five_hour) in held.iter().zip([passed, passed, ahead]) {
        record_observation(
            &db,
            account,
            &Observation {
                plan: None,
                five_hour: Some(five_hour),
                weekly: Some(if account.as_str() == "mixed" {
                    passed
                } else {
                    ahead
                }),
                provider: Some(crate::usage::poll::Provider::Claude),
            },
            LimitSource::Poll,
            MIDNIGHT,
        )
        .unwrap();
    }
    record_failure(&db, &held[0], "usage_unreachable", MIDNIGHT).unwrap();
    record_failure(&db, &held[2], "usage_unreachable", MIDNIGHT).unwrap();
    let shown = report(&db, None, None, 0, now, &held).unwrap().limits;
    let row = |id: &str| shown.iter().find(|row| row.account.as_str() == id).unwrap();
    assert_eq!(row("failed").five_hour, None);
    assert_eq!(row("failed").weekly, Some(ahead));
    assert_eq!(row("healthy").five_hour, Some(passed));
    assert_eq!(row("mixed").five_hour, Some(ahead));
    assert_eq!(row("mixed").weekly, None);
}
