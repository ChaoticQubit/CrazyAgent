use super::*;
use std::io::Write;

const HOUR: i64 = 60 * 60 * 1000;
const NOW: i64 = 1_793_000_000_000;

fn attribution(kind: RunKind, pool: Option<&str>) -> Attribution {
    Attribution {
        kind,
        project: Some(ProjectId::new("alpha").unwrap()),
        pool: pool.map(|pool| CredentialRef::new(pool).unwrap()),
        account: Some(CredentialRef::new("claude-main").unwrap()),
        agent: "team-7".into(),
        harness: "claude-code".into(),
    }
}

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = super::super::open(root.path()).unwrap();
    (root, db)
}

fn append(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn claude_assistant(
    id: &str,
    input: u64,
    output: u64,
    read: u64,
    write: u64,
    stop: &str,
) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "sessionId": "s1",
            "message": {
                "id": id,
                "role": "assistant",
                "content": [{"type": "text", "text": "working"}],
                "stop_reason": stop,
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": read,
                    "cache_creation_input_tokens": write
                }
            }
        })
    )
}

type Row = (
    i64,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
);

fn rows(db: &Connection) -> Vec<Row> {
    let mut statement = db
        .prepare(
            "SELECT hour_ms,kind,pool,account,agent,session,input,output,cache_read,cache_write
             FROM usage_tokens ORDER BY session, hour_ms",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn claude_messages_count_once_at_their_largest_usage_and_never_twice() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("projects/-work-alpha/s1.jsonl");
    let user = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n";
    append(&record, user);
    append(&record, &claude_assistant("m1", 10, 2, 100, 50, "tool_use"));
    append(&record, &claude_assistant("m1", 10, 7, 100, 50, "tool_use"));
    append(&record, &claude_assistant("m1", 10, 7, 100, 50, "tool_use"));
    append(&record, &claude_assistant("m2", 4, 30, 160, 0, "end_turn"));
    append(&record, "not json\n");
    let who = attribution(RunKind::Agent, None);
    let added = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    let expected = Tokens {
        input: 14,
        output: 37,
        cache_read: 260,
        cache_write: 50,
    };
    assert_eq!(added, expected);
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + 1).unwrap(),
        Tokens::default()
    );
    let line = claude_assistant("m3", 1, 1, 1, 1, "end_turn");
    let (head, tail) = line.split_at(20);
    append(&record, head);
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + 2).unwrap(),
        Tokens::default()
    );
    append(&record, tail);
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + HOUR).unwrap(),
        Tokens {
            input: 1,
            output: 1,
            cache_read: 1,
            cache_write: 1
        }
    );
    let hour = NOW - NOW.rem_euclid(HOUR);
    assert_eq!(
        rows(&db),
        vec![
            (
                hour,
                "agent".into(),
                "".into(),
                "claude-main".into(),
                "team-7".into(),
                "s1".into(),
                14,
                37,
                260,
                50
            ),
            (
                hour + HOUR,
                "agent".into(),
                "".into(),
                "claude-main".into(),
                "team-7".into(),
                "s1".into(),
                1,
                1,
                1,
                1
            ),
        ]
    );
}

#[test]
fn claude_sub_agent_records_count_and_a_linked_record_is_never_followed() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    append(
        &home
            .path()
            .join("projects/-work-alpha/s2/subagents/agent-a1.jsonl"),
        &claude_assistant("sub1", 3, 4, 5, 6, "end_turn"),
    );
    let outside = elsewhere.path().join("other.jsonl");
    append(
        &outside,
        &claude_assistant("x1", 1000, 1000, 0, 0, "end_turn"),
    );
    std::os::unix::fs::symlink(
        &outside,
        home.path().join("projects/-work-alpha/linked.jsonl"),
    )
    .unwrap();
    let who = attribution(RunKind::Master, Some("pool-a"));
    let added = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    assert_eq!(
        added,
        Tokens {
            input: 3,
            output: 4,
            cache_read: 5,
            cache_write: 6
        }
    );
    let stored = rows(&db);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].1, "master");
    assert_eq!(stored[0].2, "pool-a");
    assert_eq!(stored[0].5, "agent-a1");
}

fn codex_count(input: u64, cached: u64, output: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "timestamp": "2026-10-27T03:00:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                        "reasoning_output_tokens": 0,
                        "total_tokens": input + output
                    },
                    "last_token_usage": {"input_tokens": 1, "output_tokens": 1}
                },
                "rate_limits": null
            }
        })
    )
}

#[test]
fn codex_totals_count_their_growth_with_cached_input_apart_and_limits_are_kept() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home
        .path()
        .join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-abc.jsonl");
    append(
        &record,
        "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/work/alpha\"}}\n",
    );
    append(&record, &codex_count(1000, 600, 50));
    append(&record, &codex_count(1000, 600, 50));
    append(&record, &codex_count(2500, 1800, 120));
    let limits = serde_json::json!({
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": null,
            "rate_limits": {
                "plan_type": "pro",
                "primary": {"used_percent": 12.5, "window_minutes": 300, "resets_at": 1_793_003_600},
                "secondary": {"used_percent": 140.0, "window_minutes": 10080, "resets_in_seconds": 7200}
            }
        }
    });
    append(&record, &format!("{limits}\n"));
    let mut who = attribution(RunKind::Research, None);
    who.account = Some(CredentialRef::new("codex-a").unwrap());
    who.harness = "codex".into();
    let added = scan_home(&db, home.path(), Format::Codex, &who, NOW).unwrap();
    assert_eq!(
        added,
        Tokens {
            input: 700,
            output: 120,
            cache_read: 1800,
            cache_write: 0
        }
    );
    let stored = super::super::limits(&db, who.account.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.plan.as_deref(), Some("pro"));
    assert_eq!(
        stored.five_hour,
        Some(LimitWindow {
            used_percent: 12.5,
            resets_at_ms: Some(1_793_003_600_000)
        })
    );
    assert_eq!(
        stored.weekly,
        Some(LimitWindow {
            used_percent: 100.0,
            resets_at_ms: Some(NOW + 7_200_000)
        })
    );
    assert_eq!(stored.source, Some(LimitSource::Transcript));
    assert_eq!(stored.checked_ms, None);
    let other = home.path().join("sessions/2026/10/27/notes.jsonl");
    append(&other, &codex_count(9, 0, 9));
    assert_eq!(
        scan_home(&db, home.path(), Format::Codex, &who, NOW + 1).unwrap(),
        Tokens::default()
    );
}

#[test]
fn a_replaced_or_shortened_record_starts_over_and_an_overlong_line_is_skipped() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("projects/-work-alpha/s3.jsonl");
    append(&record, &claude_assistant("a1", 5, 5, 0, 0, "end_turn"));
    let who = attribution(RunKind::Agent, None);
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW)
            .unwrap()
            .input,
        5
    );
    std::fs::remove_file(&record).unwrap();
    append(&record, &claude_assistant("b1", 8, 1, 0, 0, "end_turn"));
    append(&record, &claude_assistant("b2", 2, 1, 0, 0, "end_turn"));
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW)
            .unwrap()
            .input,
        10
    );
    std::fs::write(&record, claude_assistant("c1", 3, 1, 0, 0, "end_turn")).unwrap();
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW)
            .unwrap()
            .input,
        3
    );
    let long = format!(
        "{{\"type\":\"assistant\",\"pad\":\"{}\"}}\n",
        "x".repeat(READ_BYTES_MAX + 10)
    );
    append(&record, &long);
    append(&record, &claude_assistant("c2", 4, 1, 0, 0, "end_turn"));
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap(),
        Tokens::default()
    );
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW)
            .unwrap()
            .input,
        4
    );
    let mut nameless = who.clone();
    nameless.agent = String::new();
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &nameless, NOW)
            .unwrap_err()
            .0,
        "usage_attribution_invalid"
    );
    assert_eq!(Format::of("claude-code"), Some(Format::Claude));
    assert_eq!(Format::of("codex"), Some(Format::Codex));
    assert_eq!(Format::of("aider"), None);
}

fn unattributed(harness: &str) -> Attribution {
    Attribution {
        kind: RunKind::Research,
        project: None,
        pool: None,
        account: None,
        agent: "onboarding".into(),
        harness: harness.into(),
    }
}

fn groups(db: &Connection) -> Vec<(String, String, String, String, i64)> {
    let mut statement = db
        .prepare(
            "SELECT project,pool,account,agent,SUM(input) FROM usage_tokens
             GROUP BY project,pool,account,agent ORDER BY agent,project",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_run_tied_to_no_project_or_account_counts_unattributed_and_keeps_no_limits() {
    let (_root, db) = store();
    let homes = tempfile::tempdir().unwrap();
    let endpoint = homes.path().join("endpoint");
    let record = endpoint.join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-e.jsonl");
    append(&record, &codex_count(400, 100, 20));
    let limits = serde_json::json!({
        "type": "event_msg",
        "payload": {"type": "token_count", "info": null, "rate_limits": {
            "primary": {"used_percent": 40.0, "window_minutes": 300, "resets_in_seconds": 60}
        }}
    });
    append(&record, &format!("{limits}\n"));
    register_run(&db, &endpoint, &unattributed("codex"), NOW).unwrap();
    let renamed = homes.path().join("renamed");
    append(
        &renamed.join("projects/f/r.jsonl"),
        &claude_assistant("m1", 7, 1, 0, 0, "end_turn"),
    );
    db.execute(
        "INSERT INTO usage_runs (home,kind,project,account,agent,harness,started_ms)
         VALUES (?1,'research','not a project id','not an account!','research-r','claude-code',?2)",
        params![renamed.to_string_lossy(), NOW],
    )
    .unwrap();
    let added = scan_runs(
        &db,
        |project, account| {
            panic!(
                "no pool lookup without both: {project} {}",
                account.as_str()
            )
        },
        NOW + 1,
    )
    .unwrap();
    assert_eq!(added.input, 307);
    assert_eq!(
        groups(&db),
        vec![
            (
                String::new(),
                String::new(),
                String::new(),
                "onboarding".into(),
                300
            ),
            (
                String::new(),
                String::new(),
                String::new(),
                "research-r".into(),
                7
            ),
        ]
    );
    assert!(super::super::all_limits(&db).unwrap().is_empty());
}

#[test]
fn a_finished_turn_counts_its_reported_usage_or_its_records_once_and_forgets_its_home() {
    let (_root, db) = store();
    let homes = tempfile::tempdir().unwrap();
    let mut master = attribution(RunKind::Master, Some("pool-a"));
    master.agent = "master-alpha".into();
    let claude = homes.path().join("claude-turn");
    std::fs::create_dir_all(&claude).unwrap();
    let reported = Tokens {
        input: 12,
        output: 30,
        cache_read: 900,
        cache_write: 40,
    };
    assert_eq!(
        count_turn(
            &db,
            &claude,
            Format::Claude,
            &master,
            reported,
            "turn-1",
            NOW
        )
        .unwrap(),
        reported
    );
    let codex = homes.path().join("codex-turn");
    let record = codex.join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-m.jsonl");
    append(&record, &codex_count(1000, 600, 50));
    master.harness = "codex".into();
    let counted = count_turn(
        &db,
        &codex,
        Format::Codex,
        &master,
        Tokens::default(),
        "turn-2",
        NOW,
    )
    .unwrap();
    assert_eq!(counted.input, 400);
    assert_eq!(counted.cache_read, 600);
    let sessions: Vec<(String, i64, i64)> = db
        .prepare("SELECT session,input,cache_read FROM usage_tokens WHERE agent='master-alpha' ORDER BY session")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        sessions,
        vec![
            ("rollout-2026-10-27T03-00-00-m".into(), 400, 600),
            ("turn-1".into(), 12, 900),
        ]
    );
    let progress: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(progress, 0);
    assert_eq!(
        count_turn(
            &db,
            &claude,
            Format::Claude,
            &master,
            Tokens::default(),
            "turn-3",
            NOW
        )
        .unwrap(),
        Tokens::default()
    );
}

#[test]
fn reported_turn_usage_adds_up_under_its_agent_and_a_bad_session_name_is_refused() {
    let (_root, db) = store();
    let mut piped = attribution(RunKind::Agent, Some("pool-a"));
    piped.agent = "team-4".into();
    let turn = Tokens {
        input: 5,
        output: 7,
        cache_read: 100,
        cache_write: 3,
    };
    count_reported(&db, &piped, turn, "headless", NOW).unwrap();
    count_reported(&db, &piped, turn, "headless", NOW + 1000).unwrap();
    count_reported(&db, &piped, Tokens::default(), "headless", NOW).unwrap();
    let rows: Vec<(String, String, i64, i64, i64, i64)> = db
        .prepare("SELECT agent,session,input,output,cache_read,cache_write FROM usage_tokens")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![("team-4".into(), "headless".into(), 10, 14, 200, 6)]
    );
    for session in ["", &"s".repeat(129)] {
        assert_eq!(
            count_reported(&db, &piped, turn, session, NOW)
                .unwrap_err()
                .0,
            "usage_attribution_invalid"
        );
    }
}
