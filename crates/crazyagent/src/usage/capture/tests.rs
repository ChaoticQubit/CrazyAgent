use super::*;
use std::io::Write;

const HOUR: i64 = 60 * 60 * 1000;
const NOW: i64 = 1_793_000_000_000;

fn tokens_of(usages: &[Usage]) -> Tokens {
    usages
        .iter()
        .fold(Tokens::default(), |sum, usage| sum.plus(usage.tokens))
}

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

/// A native lead's teammates keep their session records in their own home beside the
/// lead's (TPH-2 C), and those count with the lead's own. A teammates' home whose
/// `projects` is a link is not walked, so no teammate points the scan at another folder.
#[test]
fn a_native_lead_s_teammates_records_count_from_their_home_and_a_linked_top_folder_is_not_walked() {
    let (_root, db) = store();
    let agent = tempfile::tempdir().unwrap();
    let home = agent.path().join("home");
    append(
        &home.join("projects/-work-alpha/lead.jsonl"),
        &claude_assistant("lead1", 1, 2, 0, 0, "end_turn"),
    );
    append(
        &agent
            .path()
            .join("mates-home/projects/-work-alpha/mate.jsonl"),
        &claude_assistant("mate1", 10, 20, 0, 0, "end_turn"),
    );
    let who = attribution(RunKind::Master, Some("pool-a"));
    let added = scan_home(&db, &home, Format::Claude, &who, NOW).unwrap();
    assert_eq!(
        added,
        Tokens {
            input: 11,
            output: 22,
            cache_read: 0,
            cache_write: 0
        }
    );

    let other = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    append(
        &elsewhere.path().join("projects/-work-beta/other.jsonl"),
        &claude_assistant("other1", 1000, 1000, 0, 0, "end_turn"),
    );
    std::fs::create_dir_all(other.path().join("home")).unwrap();
    std::fs::create_dir_all(other.path().join("mates-home")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("projects"),
        other.path().join("mates-home").join("projects"),
    )
    .unwrap();
    assert_eq!(
        records(&other.path().join("home"), Format::Claude).0,
        Vec::<PathBuf>::new()
    );
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
                    }
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

#[test]
fn a_record_that_grows_while_it_is_read_counts_each_line_once_at_the_next_scan() {
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("projects/-work-alpha/s9.jsonl");
    append(&record, &claude_assistant("g1", 6, 2, 0, 0, "end_turn"));
    let mut progress = Progress::default();
    let opened = open_record(&record).unwrap();
    let length = opened.2;
    append(&record, &claude_assistant("g2", 3, 1, 0, 0, "end_turn"));
    let growth = tokens_of(&read_opened(opened, Format::Claude, &mut progress, NOW, "").0);
    assert_eq!(
        growth,
        Tokens {
            input: 6,
            output: 2,
            cache_read: 0,
            cache_write: 0
        }
    );
    assert_eq!(progress.offset, length);
    let growth = tokens_of(&read_record(&record, Format::Claude, &mut progress, NOW + 1).0);
    assert_eq!(
        growth,
        Tokens {
            input: 3,
            output: 1,
            cache_read: 0,
            cache_write: 0
        }
    );
    let grown = std::fs::metadata(&record).unwrap().len();
    assert_eq!(progress.offset, grown);

    let line = claude_assistant("g3", 5, 5, 0, 0, "end_turn");
    let (head, tail) = line.split_at(25);
    append(&record, head);
    let opened = open_record(&record).unwrap();
    append(&record, tail);
    let growth = tokens_of(&read_opened(opened, Format::Claude, &mut progress, NOW + 2, "").0);
    assert_eq!(growth, Tokens::default());
    assert_eq!(progress.offset, grown);
    let growth = tokens_of(&read_record(&record, Format::Claude, &mut progress, NOW + 3).0);
    assert_eq!((growth.input, growth.output), (5, 5));
    assert_eq!(progress.offset, std::fs::metadata(&record).unwrap().len());
    assert_eq!(
        tokens_of(&read_record(&record, Format::Claude, &mut progress, NOW + 4).0),
        Tokens::default()
    );
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

fn mkfifo(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let made =
        crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(path)).unwrap();
    assert!(made.success());
}

#[test]
fn a_record_that_is_a_fifo_is_skipped_without_waiting_for_a_writer() {
    let home = tempfile::tempdir().unwrap();
    let fifo = home.path().join("projects/-work-alpha/pipe.jsonl");
    mkfifo(&fifo);
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut progress = Progress::default();
        let found = read_record(&fifo, Format::Claude, &mut progress, NOW);
        let _ = sent.send((found, progress));
    });
    assert_eq!(
        read.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(((Vec::new(), None), Progress::default())),
        "a FIFO record is skipped without waiting for a writer"
    );
}

#[test]
fn a_record_swapped_for_a_fifo_never_stalls_the_scan_or_locks_the_store() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("projects/-work-alpha");
    let fresh = folder.join("a.jsonl");
    let record = folder.join("b.jsonl");
    let plain = home.path().join("plain");
    let fifo = home.path().join("fifo");
    append(&plain, &claude_assistant("b1", 1, 1, 0, 0, "end_turn"));
    mkfifo(&fifo);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let swapping = stop.clone();
    let swapper = std::thread::spawn(move || {
        while !swapping.load(Ordering::SeqCst) {
            for staged in [&plain, &fifo] {
                let _ = std::fs::rename(staged, &record);
                let _ = std::fs::rename(&record, staged);
            }
        }
    });
    let writing = stop.clone();
    let path = root.path().join("engine.db");
    let writer = std::thread::spawn(move || {
        let other = Connection::open(path).unwrap();
        other
            .busy_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let mut writes = 0_i64;
        while !writing.load(Ordering::SeqCst) {
            other
                .execute(
                    "INSERT INTO usage_refresh (singleton,requested_ms) VALUES (1,?1)
                     ON CONFLICT(singleton) DO UPDATE SET requested_ms=excluded.requested_ms",
                    params![writes],
                )
                .map_err(|error| error.to_string())?;
            writes += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok::<i64, String>(writes)
    });
    let (sent, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let who = attribution(RunKind::Agent, None);
        for scan in 0..300 {
            append(
                &fresh,
                &claude_assistant(&format!("a{scan}"), 1, 1, 0, 0, "end_turn"),
            );
            scan_home(&db, home.path(), Format::Claude, &who, NOW + scan).unwrap();
        }
        let _ = sent.send(());
    });
    let finished = done.recv_timeout(std::time::Duration::from_secs(30));
    stop.store(true, Ordering::SeqCst);
    swapper.join().unwrap();
    assert_eq!(finished, Ok(()), "every scan returns while records swap");
    assert!(writer.join().unwrap().unwrap() > 0);
}

#[test]
fn an_implausible_token_count_is_bounded_and_later_growth_never_overflows_its_row() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("projects/-work-alpha/s9.jsonl");
    append(
        &record,
        &claude_assistant("huge", i64::MAX as u64, u64::MAX, 0, 0, "end_turn"),
    );
    let who = attribution(RunKind::Agent, None);
    let first = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    assert_eq!(
        (first.input, first.output),
        (LINE_TOKENS_MAX, LINE_TOKENS_MAX)
    );
    append(&record, &claude_assistant("next", 7, 1, 0, 0, "end_turn"));
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + 1)
            .unwrap()
            .input,
        7
    );
    let flood = Tokens {
        input: u64::MAX,
        output: u64::MAX,
        cache_read: u64::MAX,
        cache_write: u64::MAX,
    };
    count_reported(&db, &who, flood, "s9", NOW + 2).unwrap();
    count_reported(&db, &who, flood, "s9", NOW + 3).unwrap();
    let stored: (i64, String, i64) = db
        .query_row(
            "SELECT input,typeof(input),cache_write FROM usage_tokens WHERE session='s9'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, (ROW_TOKENS_MAX, "integer".into(), ROW_TOKENS_MAX));
    let report = super::super::report::report(&db, None, None, 0, NOW + 4, &[]).unwrap();
    assert_eq!(report.total.input, ROW_TOKENS_MAX as u64);
}

fn codex_limits(used: f64, minutes: u64, resets_at_ms: i64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": null, "rate_limits": {
                "primary": {
                    "used_percent": used,
                    "window_minutes": minutes,
                    "resets_at": resets_at_ms / 1000
                }
            }}
        })
    )
}

#[test]
fn transcript_limits_stay_within_their_window_below_the_owner_and_a_fresh_poll() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home
        .path()
        .join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-f.jsonl");
    let mut who = attribution(RunKind::Agent, None);
    who.account = Some(CredentialRef::new("codex-a").unwrap());
    who.harness = "codex".into();
    let account = who.account.clone().unwrap();
    let stored = || super::super::limits(&db, &account).unwrap();
    let full_at = |at: i64| {
        stored()
            .and_then(|limits| limits.five_hour_at(at))
            .is_some_and(|window| window.used_percent >= 100.0)
    };
    let scan = |at: i64| scan_home(&db, home.path(), Format::Codex, &who, at).unwrap();
    append(&record, &codex_limits(100.0, 300, 32_503_679_999_000));
    scan(NOW);
    assert!(!full_at(NOW + 5 * HOUR), "a year-3000 reset is refused");
    append(&record, &codex_limits(100.0, 300, NOW + HOUR));
    scan(NOW + 1);
    assert!(full_at(NOW + 2));
    assert!(!full_at(NOW + HOUR));
    super::super::set_reset_override(
        &db,
        &account,
        super::super::WindowKind::FiveHour,
        Some(NOW + 60_000),
    )
    .unwrap();
    append(&record, &codex_limits(100.0, 300, NOW + 2 * HOUR));
    scan(NOW + 2);
    assert_eq!(
        stored().unwrap().five_hour_reset_override_ms,
        Some(NOW + 60_000)
    );
    assert!(!full_at(NOW + 60_000));
    let polled = Observation {
        plan: None,
        five_hour: Some(LimitWindow {
            used_percent: 20.0,
            resets_at_ms: Some(NOW + 4 * HOUR),
        }),
        weekly: None,
        provider: Some(crate::usage::poll::Provider::Codex),
    };
    record_observation(&db, &account, &polled, LimitSource::Poll, NOW + 3).unwrap();
    append(&record, &codex_limits(100.0, 300, NOW + HOUR));
    scan(NOW + 4);
    let kept = stored().unwrap();
    assert_eq!(kept.five_hour, polled.five_hour);
    assert_eq!(kept.source, Some(LimitSource::Poll));
    let stale = NOW + 3 + super::super::service::POLL_EVERY_MS;
    append(&record, &codex_limits(90.0, 300, stale + HOUR));
    scan(stale);
    let advisory = stored().unwrap();
    assert_eq!(advisory.five_hour.unwrap().used_percent, 90.0);
    assert_eq!(advisory.source, Some(LimitSource::Transcript));
}

fn listed(db: &Connection, sql: &str) -> Vec<String> {
    let mut statement = db.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_run_past_its_day_is_forgotten_with_its_records_once_its_home_is_gone() {
    let (_root, db) = store();
    let homes = tempfile::tempdir().unwrap();
    let gone = homes.path().join("run");
    let kept = homes.path().join("run-2");
    for (home, thread) in [(&gone, "a"), (&kept, "b")] {
        append(
            &home.join(format!(
                "sessions/2026/10/27/rollout-2026-10-27T03-00-00-{thread}.jsonl"
            )),
            &codex_count(400, 100, 20),
        );
        register_run(&db, home, &unattributed("codex"), NOW).unwrap();
    }
    let counted = scan_runs(&db, |_, _| None, NOW + 1).unwrap();
    assert_eq!(counted.input, 600);
    assert_eq!(
        listed(&db, "SELECT path FROM usage_files ORDER BY path").len(),
        2
    );
    let tokens = listed(&db, "SELECT session FROM usage_tokens ORDER BY session");
    std::fs::remove_dir_all(&gone).unwrap();

    let later = NOW + RUN_SCAN_MS + 1;
    assert!(scan_runs(&db, |_, _| None, later).unwrap().is_empty());
    assert_eq!(
        listed(&db, "SELECT home FROM usage_runs"),
        vec![kept.to_string_lossy().into_owned()],
        "a finished run whose home is gone is forgotten; one whose home stays is kept"
    );
    let files = listed(&db, "SELECT path FROM usage_files");
    assert_eq!(files.len(), 1);
    assert!(
        files[0].starts_with(&format!("{}/", kept.to_string_lossy())),
        "only the gone home's records are forgotten, not a sibling's sharing its name: {files:?}"
    );
    assert_eq!(
        listed(&db, "SELECT session FROM usage_tokens ORDER BY session"),
        tokens,
        "counted tokens stay"
    );

    register_run(&db, &kept, &unattributed("codex"), later).unwrap();
    assert!(
        scan_runs(&db, |_, _| None, later + 1).unwrap().is_empty(),
        "a kept home registered again never counts its records twice"
    );
}

/// One scan that sees limit windows in two Codex records keeps the reading of the most
/// recently changed one, not that of the older record read after it.
#[test]
fn one_scan_keeps_the_limits_of_the_most_recently_changed_codex_record() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let window = |used: f64| {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "token_count", "info": null, "rate_limits": {
                    "primary": {"used_percent": used, "window_minutes": 300, "resets_in_seconds": 600}
                }}
            })
        )
    };
    let older = home
        .path()
        .join("sessions/2026/10/26/rollout-2026-10-26T03-00-00-old.jsonl");
    let newer = home
        .path()
        .join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-new.jsonl");
    append(&older, &window(90.0));
    append(&newer, &window(10.0));
    for (path, secs) in [(&older, 1_793_000_000), (&newer, 1_793_000_600)] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }
    let mut who = attribution(RunKind::Research, None);
    who.account = Some(CredentialRef::new("codex-a").unwrap());
    who.harness = "codex".into();
    scan_home(&db, home.path(), Format::Codex, &who, NOW).unwrap();
    let stored = super::super::limits(&db, who.account.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.five_hour.map(|window| window.used_percent),
        Some(10.0),
        "the newer record's reading is kept"
    );
}

/// The record walk examines a bounded number of entries in one home in all, not per folder:
/// in a home an agent filled with 20 folders of 1,000 records, whose newest records are
/// spread over every folder, the scan stops before it has seen every folder.
#[test]
fn a_home_filled_with_records_is_walked_within_one_bound_in_all() {
    const FOLDERS: usize = 20;
    const FILES: usize = 1000;
    let home = tempfile::tempdir().unwrap();
    for folder in 0..FOLDERS {
        let dir = home.path().join("projects").join(format!("f{folder:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..FILES {
            let changed = 1_700_000_000 + u64::try_from(file * FOLDERS + folder).unwrap();
            std::fs::File::create(dir.join(format!("s{file:04}.jsonl")))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(changed))
                .unwrap();
        }
    }
    let (found, _, cut) = records(home.path(), Format::Claude);
    assert!(
        cut,
        "a walk stopped at the entry bound says the home was cut"
    );
    assert_eq!(found.len(), FILES_PER_SCAN_MAX);
    let folders: std::collections::BTreeSet<&Path> =
        found.iter().map(|path| path.parent().unwrap()).collect();
    assert!(
        folders.len() < FOLDERS,
        "the walk read all {FOLDERS} folders of 1,000 records"
    );
}

/// A session record whose stored progress names the same inode on another device, as a
/// reboot that renumbered the volume leaves it, keeps its offset and counts only the lines
/// appended since the last scan, while its stored pin moves to the live device. A record
/// removed and recreated on that renumbered pin, with a new inode, starts over.
#[test]
fn a_record_whose_volume_was_renumbered_keeps_its_place_and_counts_nothing_twice() {
    use std::os::unix::fs::MetadataExt;
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("projects/-work-alpha/s1.jsonl");
    let who = attribution(RunKind::Agent, None);
    let key = record.to_string_lossy().into_owned();
    let renumber = |db: &Connection| {
        let text: String = db
            .query_row(
                "SELECT progress FROM usage_files WHERE path=?1",
                params![key],
                |row| row.get(0),
            )
            .unwrap();
        let mut progress: Progress = serde_json::from_str(&text).unwrap();
        progress.identity.0 ^= 1;
        db.execute(
            "UPDATE usage_files SET progress=?2 WHERE path=?1",
            params![key, serde_json::to_string(&progress).unwrap()],
        )
        .unwrap();
    };
    let stored_identity = |db: &Connection| -> (u64, u64) {
        let text: String = db
            .query_row(
                "SELECT progress FROM usage_files WHERE path=?1",
                params![key],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Progress>(&text).unwrap().identity
    };

    append(&record, &claude_assistant("r1", 5, 2, 0, 0, "end_turn"));
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW)
            .unwrap()
            .input,
        5
    );

    renumber(&db);
    append(&record, &claude_assistant("r2", 3, 1, 0, 0, "end_turn"));
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + 1).unwrap(),
        Tokens {
            input: 3,
            output: 1,
            cache_read: 0,
            cache_write: 0,
        },
        "only the line appended since the last scan is counted, not the whole record again"
    );
    let live = std::fs::metadata(&record).unwrap();
    assert_eq!(
        stored_identity(&db),
        (live.dev(), live.ino()),
        "the pin moved to the live device"
    );

    let kept_offset = {
        let text: String = db
            .query_row(
                "SELECT progress FROM usage_files WHERE path=?1",
                params![key],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Progress>(&text).unwrap().offset
    };
    renumber(&db);
    std::fs::remove_file(&record).unwrap();
    append(&record, &claude_assistant("r3", 9, 4, 0, 0, "end_turn"));
    append(&record, &claude_assistant("r4", 2, 1, 0, 0, "end_turn"));
    append(&record, &claude_assistant("r5", 1, 1, 0, 0, "end_turn"));
    assert!(
        std::fs::metadata(&record).unwrap().len() >= kept_offset,
        "the recreated record is at least as long as the kept offset, so only the \
         inode check, not the length guard, can start it over"
    );
    assert_eq!(
        scan_home(&db, home.path(), Format::Claude, &who, NOW + 2)
            .unwrap()
            .input,
        12,
        "a record recreated with a new inode on the renumbered pin starts over"
    );
    let live = std::fs::metadata(&record).unwrap();
    assert_eq!(stored_identity(&db), (live.dev(), live.ino()));
}

/// A home past the entry bound still counts the record written last, however its agent dates
/// it, and its reading says it is partial until a scan reads the home whole. The walk orders
/// by the inode change time, which no agent can set, and enters the folder changed last
/// first. Here that record is the last entry of a folder of 1,100 records that a walk in
/// plain directory order reaches last, and the agent set both to an old modification time.
#[test]
fn a_home_past_the_entry_bound_counts_the_record_written_last_and_reads_as_partial() {
    const FOLDERS: usize = 20;
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let projects = home.path().join("projects");
    let at =
        |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + secs);
    for folder in 0..FOLDERS {
        let dir = projects.join(format!("f{folder:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..1100 {
            std::fs::File::create(dir.join(format!("s{file:04}.jsonl")))
                .unwrap()
                .set_modified(at(u64::try_from(file).unwrap()))
                .unwrap();
        }
    }
    let listed = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let folders = listed(&projects);
    let last = folders[0].clone();
    let live = listed(&last).pop().unwrap();
    append(&live, &claude_assistant("m1", 7, 3, 0, 0, "end_turn"));
    for path in [&live, &last] {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(at(0))
            .unwrap();
    }
    let who = attribution(RunKind::Agent, None);
    let partial = |db: &Connection| {
        let report = super::super::report::report(db, None, None, 0, NOW, &[]).unwrap();
        serde_json::to_value(report).unwrap()["partial_homes"].as_u64()
    };
    let added = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    let cut = partial(&db);
    for dir in &folders[1..16] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    scan_home(&db, home.path(), Format::Claude, &who, NOW + 1).unwrap();
    assert_eq!(
        ((added.input, added.output), cut, partial(&db)),
        ((7, 3), Some(1), Some(0)),
        "the record written last is counted, and the reading is partial only while the home is cut"
    );
}

/// MSL2B-R3-1: a home of more records than one scan reads, all well inside the walk's
/// bounds, reads as partial while a record the scan leaves out holds lines no scan counted,
/// however many scans pass, and only then. An agent rewriting its other records keeps its
/// live session record out of every scan; once a scan reads it, the records left out are
/// all counted and the reading is complete again, as it is for a home that grows past the
/// bound with records already counted.
#[test]
fn a_home_whose_scan_leaves_out_an_uncounted_record_reads_as_partial_until_it_is_read() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("projects").join("x");
    let live = folder.join("live.jsonl");
    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
    append(&live, &claude_assistant("m1", 7, 3, 0, 0, "end_turn"));
    pause();
    let others: Vec<PathBuf> = (0..FILES_PER_SCAN_MAX)
        .map(|index| folder.join(format!("o{index:03}.jsonl")))
        .collect();
    for other in &others {
        append(other, "\n");
    }
    let who = attribution(RunKind::Agent, None);
    let partial = |db: &Connection| {
        let report = super::super::report::report(db, None, None, 0, NOW, &[]).unwrap();
        serde_json::to_value(report).unwrap()["partial_homes"].as_u64()
    };
    let scan = |at: i64| {
        pause();
        let added = scan_home(&db, home.path(), Format::Claude, &who, at).unwrap();
        ((added.input, added.output), partial(&db))
    };

    assert_eq!(
        scan(NOW),
        ((0, 0), Some(1)),
        "the first scan leaves out the live record, which no scan read"
    );
    assert_eq!(
        scan(NOW + 1),
        ((0, 0), Some(1)),
        "a second scan that leaves it out unread still reads as partial"
    );
    append(&live, &claude_assistant("m2", 5, 2, 0, 0, "end_turn"));
    assert_eq!(
        scan(NOW + 2),
        ((12, 5), Some(0)),
        "the live record changed last is read whole, and the record left out was counted"
    );
    let grown = folder.join("grown.jsonl");
    append(&grown, "\n");
    assert_eq!(
        scan(NOW + 3),
        ((0, 0), Some(0)),
        "a home that grows past the bound with counted records reads as complete"
    );

    append(&live, &claude_assistant("m3", 1, 1, 0, 0, "end_turn"));
    pause();
    for other in others.iter().chain([&grown]) {
        append(other, "\n");
    }
    assert_eq!(
        scan(NOW + 4),
        ((0, 0), Some(1)),
        "an agent rewriting its other records keeps the changed live record out, and the reading says so"
    );
    assert_eq!(
        scan(NOW + 5),
        ((0, 0), Some(1)),
        "the next scan that leaves it out unread says so again"
    );
    std::fs::remove_file(&grown).unwrap();
    std::fs::remove_file(&others[0]).unwrap();
    assert_eq!(
        scan(NOW + 6),
        ((1, 1), Some(0)),
        "once the home fits one scan the live record is read and the reading is complete"
    );
}

/// MSL2B-R3-1: a home whose records were all counted, in which one scan leaves out a record
/// for the first time because one more record arrived, reads as complete: the record left
/// out was read when it was among the newest, so no usage is missing.
#[test]
fn a_home_that_grows_past_the_bound_with_counted_records_reads_as_complete() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("projects").join("x");
    for index in 0..FILES_PER_SCAN_MAX {
        append(
            &folder.join(format!("s{index:03}.jsonl")),
            &claude_assistant(&format!("m{index}"), 1, 1, 0, 0, "end_turn"),
        );
    }
    let who = attribution(RunKind::Agent, None);
    let partial = |db: &Connection| {
        let report = super::super::report::report(db, None, None, 0, NOW, &[]).unwrap();
        serde_json::to_value(report).unwrap()["partial_homes"].as_u64()
    };
    let read = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    assert_eq!(
        ((read.input, read.output), partial(&db)),
        ((256, 256), Some(0)),
        "a home of exactly the bound is read whole"
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    append(
        &folder.join("s256.jsonl"),
        &claude_assistant("m256", 1, 1, 0, 0, "end_turn"),
    );
    let grown = scan_home(&db, home.path(), Format::Claude, &who, NOW + 1).unwrap();
    assert_eq!(
        ((grown.input, grown.output), partial(&db)),
        ((1, 1), Some(0)),
        "the record left out was counted, so the reading is complete"
    );
}

/// CUT-L1: a record left out of a scan whose bytes past its counted lines hold no line a scan
/// could count reads as complete: a last line torn by a stopped agent, alone or after the
/// rest of a line longer than one read that the next read would skip (CUTT-L1).
#[test]
fn a_home_whose_left_out_records_end_in_a_torn_or_skipped_line_reads_as_complete() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("projects").join("x");
    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
    let torn_line = "{\"type\":\"assistant\",\"message\":{\"id\":\"t2\"";
    let long = folder.join("long.jsonl");
    append(&long, &claude_assistant("l1", 1, 1, 0, 0, "end_turn"));
    append(
        &long,
        &format!(
            "{{\"type\":\"assistant\",\"pad\":\"{}\"}}\n",
            "x".repeat(READ_BYTES_MAX + 10)
        ),
    );
    append(&long, torn_line);
    pause();
    let torn = folder.join("torn.jsonl");
    append(&torn, &claude_assistant("t1", 1, 1, 0, 0, "end_turn"));
    append(&torn, torn_line);
    pause();
    for index in 2..FILES_PER_SCAN_MAX {
        append(
            &folder.join(format!("s{index:03}.jsonl")),
            &claude_assistant(&format!("m{index}"), 1, 1, 0, 0, "end_turn"),
        );
    }
    let who = attribution(RunKind::Agent, None);
    let partial = |db: &Connection| {
        let report = super::super::report::report(db, None, None, 0, NOW, &[]).unwrap();
        serde_json::to_value(report).unwrap()["partial_homes"].as_u64()
    };
    let scan = |at: i64| {
        pause();
        let added = scan_home(&db, home.path(), Format::Claude, &who, at).unwrap();
        ((added.input, added.output), partial(&db))
    };

    assert_eq!(
        scan(NOW),
        ((256, 256), Some(0)),
        "a home of exactly the bound is read whole"
    );
    assert_eq!(
        scan(NOW + 1),
        ((0, 0), Some(0)),
        "the next read of the long record skips one read of its long line"
    );
    append(
        &folder.join("n1.jsonl"),
        &claude_assistant("n1", 1, 1, 0, 0, "end_turn"),
    );
    assert_eq!(
        scan(NOW + 2),
        ((1, 1), Some(0)),
        "the long record left out while skipping holds only the rest of its skipped line and a torn line, so the reading is complete"
    );
    append(
        &folder.join("n2.jsonl"),
        &claude_assistant("n2", 1, 1, 0, 0, "end_turn"),
    );
    assert_eq!(
        scan(NOW + 3),
        ((1, 1), Some(0)),
        "the record left out holds only a torn line past its counted lines, so the reading is complete"
    );
}

/// CUT-L1: one scan reads at most `READ_BYTES_MAX` bytes of the tails of the records it left
/// out; a home whose left-out tails pass that bound reads as partial rather than complete.
#[test]
fn a_home_whose_left_out_tails_pass_one_read_reads_as_partial() {
    let (_root, db) = store();
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("projects").join("x");
    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
    for name in ["a", "b"] {
        let record = folder.join(format!("{name}.jsonl"));
        append(&record, &claude_assistant(name, 1, 1, 0, 0, "end_turn"));
        append(&record, &"x".repeat(READ_BYTES_MAX / 2 + 1));
        pause();
    }
    for index in 2..FILES_PER_SCAN_MAX {
        append(
            &folder.join(format!("s{index:03}.jsonl")),
            &claude_assistant(&format!("m{index}"), 1, 1, 0, 0, "end_turn"),
        );
    }
    let who = attribution(RunKind::Agent, None);
    let partial = |db: &Connection| {
        let report = super::super::report::report(db, None, None, 0, NOW, &[]).unwrap();
        serde_json::to_value(report).unwrap()["partial_homes"].as_u64()
    };
    let read = scan_home(&db, home.path(), Format::Claude, &who, NOW).unwrap();
    assert_eq!(
        ((read.input, read.output), partial(&db)),
        ((256, 256), Some(0)),
        "a home of exactly the bound is read whole"
    );
    pause();
    for name in ["n1", "n2"] {
        append(
            &folder.join(format!("{name}.jsonl")),
            &claude_assistant(name, 1, 1, 0, 0, "end_turn"),
        );
    }
    let grown = scan_home(&db, home.path(), Format::Claude, &who, NOW + 1).unwrap();
    assert_eq!(
        ((grown.input, grown.output), partial(&db)),
        ((2, 2), Some(1)),
        "left-out tails past one read in all are not read through, so the reading stays partial"
    );
}

/// One Codex rollout line recording a tool call of the given item type, name and payload.
fn codex_call(kind: &str, name: &str, field: &str, value: serde_json::Value) -> String {
    let mut payload = serde_json::json!({"type": kind, "name": name});
    payload[field] = value;
    format!(
        "{}\n",
        serde_json::json!({"type": "response_item", "payload": payload})
    )
}

fn tool_counts(db: &Connection, agent: i64) -> Vec<(String, i64)> {
    let mut query = db
        .prepare("SELECT tool,count FROM coord_tool_use WHERE agent=?1 ORDER BY tool")
        .unwrap();
    query
        .query_map(params![agent], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn codex_rollout_tool_calls_count_once_per_line() {
    let (_root, db) = store();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
         VALUES ('alpha','sub','codex',1,'running',?1,?1,0)",
        params![NOW],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    let home = tempfile::tempdir().unwrap();
    let record = home
        .path()
        .join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-t.jsonl");
    let graphify = serde_json::json!({"cmd": "graphify query \"who owns pools\""}).to_string();
    let skill =
        serde_json::json!({"cmd": "sed -n 1,80p /h/.codex/skills/tdd/SKILL.md"}).to_string();
    let oversized =
        serde_json::json!({"cmd": format!("graphify {}", "x".repeat(ARGUMENTS_BYTES_MAX))})
            .to_string();
    append(
        &record,
        &codex_call(
            "function_call",
            "exec_command",
            "arguments",
            graphify.clone().into(),
        ),
    );
    append(
        &record,
        &codex_call("function_call", "exec_command", "arguments", skill.into()),
    );
    append(
        &record,
        &codex_call(
            "custom_tool_call",
            "apply_patch",
            "input",
            "*** Begin Patch".into(),
        ),
    );
    append(
        &record,
        &codex_call(
            "local_shell_call",
            "",
            "action",
            serde_json::json!({"type": "exec", "command": ["bash", "-lc", "ls"]}),
        ),
    );
    append(
        &record,
        &codex_call(
            "function_call",
            "mcp__context-mode__ctx_search",
            "arguments",
            "{}".into(),
        ),
    );
    append(
        &record,
        &codex_call(
            "function_call",
            "exec_command",
            "arguments",
            oversized.into(),
        ),
    );
    append(
        &record,
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\"}}\n",
    );
    append(&record, &codex_count(1000, 600, 50));
    let mut who = attribution(RunKind::Agent, None);
    who.harness = "codex".into();
    let counted = vec![
        ("Skill".to_owned(), 1),
        ("apply_patch".to_owned(), 1),
        ("ctx_search".to_owned(), 1),
        ("exec_command".to_owned(), 1),
        ("graphify".to_owned(), 1),
        ("local_shell_call".to_owned(), 1),
        ("skill:tdd".to_owned(), 1),
    ];

    scan_agent_home(&db, home.path(), Format::Codex, &who, agent, NOW).unwrap();
    assert_eq!(tool_counts(&db, agent), counted);

    scan_agent_home(&db, home.path(), Format::Codex, &who, agent, NOW + 1).unwrap();
    assert_eq!(
        tool_counts(&db, agent),
        counted,
        "a second scan of the same lines counts nothing again"
    );

    append(
        &record,
        &codex_call(
            "function_call",
            "exec_command",
            "arguments",
            graphify.into(),
        ),
    );
    scan_agent_home(&db, home.path(), Format::Codex, &who, agent, NOW + 2).unwrap();
    let graphify_count = tool_counts(&db, agent)
        .into_iter()
        .find(|(tool, _)| tool == "graphify")
        .map(|(_, count)| count);
    assert_eq!(graphify_count, Some(2), "only the new line counts");

    let other = tempfile::tempdir().unwrap();
    append(
        &other
            .path()
            .join("sessions/2026/10/27/rollout-2026-10-27T03-00-00-u.jsonl"),
        &codex_call(
            "function_call",
            "exec_command",
            "arguments",
            "{\"cmd\":\"graphify\"}".into(),
        ),
    );
    scan_home(&db, other.path(), Format::Codex, &who, NOW + 3).unwrap();
    assert_eq!(
        tool_counts(&db, agent)
            .iter()
            .map(|(_, count)| count)
            .sum::<i64>(),
        8,
        "a scan for no agent counts no tool calls"
    );
}

/// One Claude Code transcript line of an assistant message that calls one tool.
/// Inputs: the message id, the tool's name and its input. Output: the line.
fn claude_call(id: &str, name: &str, input: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": id,
                "role": "assistant",
                "content": [{"type": "tool_use", "id": format!("toolu_{id}"), "name": name, "input": input}],
                "usage": {"input_tokens": 10, "output_tokens": 2}
            }
        })
    )
}

/// A Claude Code agent launched without hooks (DEC-104 bare-hooks) has its tool calls
/// counted from its transcript, each call once however often the home is scanned, the way a
/// Codex agent's rollout is. One with hooks counts its own through them, so its transcript
/// counts tokens only.
#[test]
fn claude_transcript_tool_calls_count_only_for_an_agent_launched_without_hooks() {
    let (_root, db) = store();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let agent = |hookless: bool| {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
             VALUES ('alpha','sub','claude-code',1,'running',?1,?1,?2)",
            params![NOW, hookless],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let bare = agent(true);
    let hooked = agent(false);
    let lines = [
        claude_call(
            "m1",
            "Bash",
            serde_json::json!({"command": "graphify query \"pools\""}),
        ),
        claude_call("m2", "Skill", serde_json::json!({"skill": "tdd"})),
        claude_call(
            "m3",
            "Edit",
            serde_json::json!({"file_path": "/repo/a.rs", "old_string": "a", "new_string": "b"}),
        ),
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_m3\",\"content\":\"ok\"}]}}\n".to_owned(),
        claude_assistant("m4", 10, 2, 0, 0, "end_turn"),
    ];
    let homes = [bare, hooked].map(|agent| {
        let home = tempfile::tempdir().unwrap();
        let record = home.path().join("projects/-repo/s1.jsonl");
        for line in &lines {
            append(
                &record,
                &line.replace("\"id\":\"m", &format!("\"id\":\"{agent}-m")),
            );
        }
        (agent, home)
    });
    let who = attribution(RunKind::Agent, None);
    let scan = |at: i64| {
        homes.each_ref().map(|(agent, home)| {
            let added =
                scan_agent_home(&db, home.path(), Format::Claude, &who, *agent, at).unwrap();
            (added.input, added.output)
        })
    };

    assert_eq!(
        scan(NOW),
        [(40, 8); 2],
        "both transcripts count their tokens"
    );
    assert_eq!(scan(NOW + 1), [(0, 0); 2]);
    assert_eq!(
        tool_counts(&db, bare),
        vec![
            ("Edit".to_owned(), 1),
            ("Skill".to_owned(), 1),
            ("graphify".to_owned(), 1),
            ("skill:tdd".to_owned(), 1),
        ],
        "each call of the agent without hooks counts once"
    );
    assert_eq!(
        tool_counts(&db, hooked),
        Vec::new(),
        "the agent with hooks counts its calls through them"
    );
}

#[test]
fn one_harness_name_holds_claude_and_claude_code_rows() {
    let (_root, db) = store();
    let mut alias = attribution(RunKind::Agent, None);
    alias.harness = "claude".into();
    let named = attribution(RunKind::Agent, None);
    let mut unknown = attribution(RunKind::Agent, None);
    unknown.harness = "mystery-cli".into();
    let tokens = Tokens {
        input: 10,
        output: 5,
        ..Tokens::default()
    };
    count_reported(&db, &alias, tokens, "s1", NOW).unwrap();
    count_reported(&db, &named, tokens, "s1", NOW).unwrap();
    count_reported(&db, &unknown, tokens, "s1", NOW).unwrap();
    let rows: Vec<(String, i64)> = db
        .prepare("SELECT harness,input FROM usage_tokens ORDER BY harness")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("claude-code".to_owned(), 20),
            ("mystery-cli".to_owned(), 10)
        ]
    );
}
