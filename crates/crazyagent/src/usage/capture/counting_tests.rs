use super::*;
use std::io::Write;

const NOW: i64 = 1_793_000_000_000;
const MODEL: &str = "claude-opus-4-5";

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = super::super::open(root.path()).unwrap();
    (root, db)
}

fn who() -> Attribution {
    Attribution {
        kind: RunKind::Agent,
        project: Some(ProjectId::new("alpha").unwrap()),
        pool: None,
        account: Some(CredentialRef::new("acct").unwrap()),
        agent: "team-7".into(),
        harness: "claude-code".into(),
    }
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

fn transcript(home: &Path, name: &str) -> PathBuf {
    home.join("projects/-repo").join(format!("{name}.jsonl"))
}

fn line(id: &str, input: u64, output: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-11-09T02:00:00.000Z",
            "message": {
                "id": id,
                "model": MODEL,
                "stop_reason": "end_turn",
                "content": [],
                "usage": {"input_tokens": input, "output_tokens": output}
            }
        })
    )
}

fn scan_it(db: &Connection, home: &Path) -> Tokens {
    scan_home(db, home, Format::Claude, &who(), NOW).unwrap()
}

fn expected(input: u64, output: u64) -> Tokens {
    Tokens {
        input,
        output,
        ..Tokens::default()
    }
}

#[test]
fn repeated_lines_of_one_message_count_once_at_the_largest() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(100, 40));
}

#[test]
fn interleaved_lines_of_two_messages_count_each_once() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_b", 200, 20));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(300, 60));
}

#[test]
fn a_resumed_file_that_copies_a_message_does_not_count_it_again() {
    let (root, db) = store();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let resumed = transcript(root.path(), "s2");
    append(&resumed, &line("msg_a", 100, 10));
    append(&resumed, &line("msg_c", 7, 3));
    assert_eq!(scan_it(&db, root.path()), expected(7, 3));
}

#[test]
fn a_later_scan_adds_only_a_messages_growth() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    append(&file, &line("msg_b", 1, 1));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(1, 31));
}

#[test]
fn two_homes_holding_one_message_count_it_once() {
    let (root, db) = store();
    let other = tempfile::tempdir().unwrap();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    append(&transcript(other.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    assert_eq!(scan_it(&db, other.path()), expected(0, 0));
}

#[test]
fn a_line_without_an_id_counts_every_time() {
    let (root, db) = store();
    let text = format!(
        "{}\n",
        serde_json::json!({"type": "assistant", "message": {"usage": {"input_tokens": 5}}})
    );
    let file = transcript(root.path(), "s1");
    append(&file, &text);
    append(&file, &text);
    assert_eq!(scan_it(&db, root.path()), expected(10, 0));
}

#[test]
fn rows_carry_the_model_the_line_names() {
    let (root, db) = store();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    scan_it(&db, root.path());
    let model: String = db
        .query_row("SELECT model FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(model, MODEL);
}

#[test]
fn one_read_stops_at_its_bound_and_the_next_scan_goes_on() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    for index in 0..USAGES_PER_READ_MAX + 3 {
        append(&file, &line(&format!("msg_{index}"), 1, 0));
    }
    let first = scan_it(&db, root.path());
    assert_eq!(first, expected(USAGES_PER_READ_MAX as u64, 0));
    assert_eq!(scan_it(&db, root.path()), expected(3, 0));
}

fn counter(db: &Connection, table: &str) -> i64 {
    db.query_row(
        "SELECT rows FROM usage_row_counts WHERE name=?1",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn at_cap(db: &Connection, table: &str) {
    db.execute(
        "UPDATE usage_row_counts SET rows=?2 WHERE name=?1",
        params![table, TABLE_ROWS_MAX],
    )
    .unwrap();
}

#[test]
fn the_row_counters_follow_inserts_repeats_and_deletes() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_a", 100, 40));
    append(&file, &line("msg_b", 1, 1));
    scan_it(&db, root.path());
    assert_eq!(
        (counter(&db, "usage_messages"), counter(&db, "usage_tokens")),
        (2, 1)
    );
    db.execute("DELETE FROM usage_messages WHERE id='claude:msg_b'", [])
        .unwrap();
    assert_eq!(counter(&db, "usage_messages"), 1);
}

#[test]
fn counting_rows_again_takes_no_write_lock() {
    let (root, db) = store();
    let second = crate::usage::open(root.path()).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let counted = count_rows(&second);
    db.execute_batch("COMMIT").unwrap();
    assert!(counted.is_ok(), "{counted:?}");
}

#[test]
fn past_the_key_cap_a_new_message_counts_whole_and_stores_no_key() {
    let (root, db) = store();
    at_cap(&db, "usage_messages");
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let keys: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(keys, 0);
    append(&transcript(root.path(), "s2"), &line("msg_a", 100, 10));
    assert_eq!(
        scan_it(&db, root.path()),
        expected(100, 10),
        "with no key a repeat counts high"
    );
}

#[test]
fn past_sixteen_models_in_an_hour_a_session_files_under_no_model() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    for index in 0..17 {
        append(
            &file,
            &line(&format!("msg_{index}"), 1, 0).replace(MODEL, &format!("model-{index}")),
        );
    }
    assert_eq!(scan_it(&db, root.path()), expected(17, 0));
    let (rows, unnamed): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*),SUM(model='') FROM usage_tokens",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, unnamed), (17, 1));
}

#[test]
fn at_the_row_cap_growth_lands_in_the_hours_catch_all_row_and_no_token_is_lost() {
    let (root, db) = store();
    at_cap(&db, "usage_tokens");
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let row: (String, String, i64) = db
        .query_row("SELECT session,model,input FROM usage_tokens", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (String::new(), String::new(), 100));
}

#[test]
fn an_advisor_call_counts_once_beside_its_message() {
    let (root, db) = store();
    let advised = |output: u64| {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-11-09T02:00:00.000Z",
                "message": {"id": "msg_a", "model": MODEL, "usage": {
                    "input_tokens": 10, "output_tokens": 5,
                    "iterations": [
                        {"type": "message", "input_tokens": 10, "output_tokens": 5},
                        {"type": "advisor_message", "model": "claude-fable-5-1", "input_tokens": 70, "output_tokens": output}
                    ]
                }}
            })
        )
    };
    let file = transcript(root.path(), "s1");
    append(&file, &advised(20));
    append(&file, &advised(20));
    assert_eq!(scan_it(&db, root.path()), expected(80, 25));
    let rows: Vec<(String, i64)> = db
        .prepare("SELECT model,input FROM usage_tokens ORDER BY model")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![("claude-fable-5-1".to_owned(), 70), (MODEL.to_owned(), 10)]
    );
}

#[test]
fn a_rows_first_estimate_is_capped_like_its_later_ones() {
    let (_root, db) = store();
    let added = Added {
        tokens: expected(1, 0),
        cost_nanos: i64::MAX,
    };
    add_tokens(&db, NOW, &who(), "s1", MODEL, added).unwrap();
    let cost: i64 = db
        .query_row("SELECT cost_nanos FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cost, ROW_TOKENS_MAX);
}

const T0: &str = "2026-11-09T03:00:00.000Z";
const T0_1: &str = "2026-11-09T03:00:00.200Z";
const T0_2: &str = "2026-11-09T03:00:00.400Z";
const LATER: &str = "2026-11-09T03:01:00.000Z";
const BEFORE: &str = "2026-11-09T01:00:00.000Z";
const CODEX: &str = "gpt-5-codex";

fn rollout(home: &Path, id: &str) -> PathBuf {
    home.join("sessions/2026/11/09")
        .join(format!("rollout-2026-11-09T03-00-00-{id}.jsonl"))
}

fn meta(at: &str, id: &str, payload: serde_json::Value) -> String {
    let mut payload = payload;
    payload["id"] = serde_json::json!(id);
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "session_meta", "payload": payload})
    )
}

fn context(at: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "turn_context", "payload": {"model": CODEX}})
    )
}

fn usage(input: u64, cached: u64, output: u64) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "output_tokens": output,
        "reasoning_output_tokens": 0,
        "total_tokens": input + output
    })
}

fn counted(at: &str, total: (u64, u64, u64), last: (u64, u64, u64)) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "timestamp": at,
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": usage(total.0, total.1, total.2),
                "last_token_usage": usage(last.0, last.1, last.2)
            }, "rate_limits": null}
        })
    )
}

fn parent(home: &Path) {
    let file = rollout(home, "parent");
    append(&file, &meta(T0, "parent", serde_json::json!({})));
    append(&file, &context(T0));
    append(&file, &counted(T0_1, (100, 40, 10), (100, 40, 10)));
    append(&file, &counted(LATER, (300, 90, 30), (200, 50, 20)));
}

fn scan_codex(db: &Connection, home: &Path) -> Tokens {
    let mut who = who();
    who.harness = "codex".into();
    scan_home(db, home, Format::Codex, &who, NOW).unwrap()
}

fn plain(input: u64, cached: u64, output: u64) -> Tokens {
    Tokens {
        input,
        cache_read: cached,
        output,
        cache_write: 0,
    }
}

fn fork(home: &Path, payload: serde_json::Value, replay_at: &str) {
    let file = rollout(home, "fork");
    append(&file, &meta(T0, "fork", payload));
    append(&file, &counted(replay_at, (100, 40, 10), (100, 40, 10)));
    append(&file, &counted(T0_2, (300, 90, 30), (200, 50, 20)));
    append(&file, &counted(LATER, (350, 95, 35), (50, 5, 5)));
}

#[test]
fn a_fork_does_not_count_the_parents_calls_again() {
    let (root, db) = store();
    parent(root.path());
    fork(
        root.path(),
        serde_json::json!({"forked_from_id": "parent"}),
        T0_1,
    );
    assert_eq!(scan_codex(&db, root.path()), plain(255, 95, 35));
}

#[test]
fn a_fork_scanned_before_its_parent_counts_nothing_twice() {
    let (root, db) = store();
    fork(
        root.path(),
        serde_json::json!({"forked_from_id": "parent"}),
        T0_1,
    );
    let first = scan_codex(&db, root.path());
    parent(root.path());
    let second = scan_codex(&db, root.path());
    assert_eq!(first.plus(second), plain(255, 95, 35));
}

#[test]
fn a_spawned_rollout_with_the_parents_old_times_does_not_count_the_parent() {
    let (root, db) = store();
    parent(root.path());
    let spawned = serde_json::json!({"source": {"subagent": {"thread_spawn": {"parent_thread_id": "parent"}}}});
    fork(root.path(), spawned, BEFORE);
    assert_eq!(scan_codex(&db, root.path()), plain(255, 95, 35));
}

#[test]
fn a_resumed_rollout_of_one_thread_counts_its_copied_records_once() {
    let (root, db) = store();
    parent(root.path());
    assert_eq!(scan_codex(&db, root.path()), plain(210, 90, 30));
    let resumed = rollout(root.path(), "resumed");
    append(&resumed, &meta(LATER, "parent", serde_json::json!({})));
    append(&resumed, &counted(LATER, (300, 90, 30), (200, 50, 20)));
    assert_eq!(scan_codex(&db, root.path()), plain(0, 0, 0));
}

#[test]
fn a_response_record_and_its_token_count_twin_count_once() {
    let (root, db) = store();
    let file = rollout(root.path(), "records");
    append(&file, &meta(T0, "records", serde_json::json!({})));
    append(
        &file,
        &format!(
            "{}\n",
            serde_json::json!({
                "timestamp": T0_1,
                "type": "token_usage_record",
                "payload": {"response_id": "resp_1", "usage": usage(100, 40, 10)}
            })
        ),
    );
    append(&file, &counted(T0_2, (100, 40, 10), (100, 40, 10)));
    assert_eq!(scan_codex(&db, root.path()), plain(60, 40, 10));
}

#[test]
fn a_repeated_token_count_counts_once() {
    let (root, db) = store();
    let file = rollout(root.path(), "repeat");
    append(&file, &meta(T0, "repeat", serde_json::json!({})));
    let line = counted(T0_1, (100, 40, 10), (100, 40, 10));
    append(&file, &line);
    append(&file, &line);
    assert_eq!(scan_codex(&db, root.path()), plain(60, 40, 10));
}

#[test]
fn a_counter_restart_counts_the_new_calls() {
    let (root, db) = store();
    let file = rollout(root.path(), "restart");
    append(&file, &meta(T0, "restart", serde_json::json!({})));
    for (at, total) in [(T0_1, (1000, 0, 100)), (T0_2, (50, 0, 5))] {
        append(
            &file,
            &format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": at,
                    "type": "event_msg",
                    "payload": {"type": "token_count", "info": {"total_token_usage": usage(total.0, total.1, total.2)}}
                })
            ),
        );
    }
    assert_eq!(scan_codex(&db, root.path()), plain(1050, 0, 105));
}

#[test]
fn codex_rows_carry_the_turns_model() {
    let (root, db) = store();
    parent(root.path());
    scan_codex(&db, root.path());
    let model: String = db
        .query_row("SELECT DISTINCT model FROM usage_tokens", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(model, CODEX);
}

#[test]
fn a_file_stored_before_the_codex_state_existed_still_reads() {
    let text = r#"{"identity":[1,2],"offset":9,"skipping":false,"total":{"input":1,"output":1,"cache_read":0,"cache_write":0}}"#;
    let progress: Progress = serde_json::from_str(text).unwrap();
    assert_eq!(progress.codex, CodexState::default());

#[test]
fn a_lower_line_in_one_read_leaves_the_largest() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_low", 1, 40));
    append(&file, &line("msg_low", 1, 10));
    assert_eq!(scan_it(&db, root.path()), expected(1, 40));
}

#[test]
fn a_lower_line_on_a_later_scan_adds_nothing_and_neither_does_its_return() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_low", 1, 40));
    assert_eq!(scan_it(&db, root.path()), expected(1, 40));
    append(&file, &line("msg_low", 1, 10));
    assert_eq!(scan_it(&db, root.path()), expected(0, 0));
    append(&file, &line("msg_low", 1, 40));
    assert_eq!(scan_it(&db, root.path()), expected(0, 0));
}

#[test]
fn a_message_id_past_the_bound_stores_no_key_and_each_line_counts() {
    let (root, db) = store();
    let huge = "a".repeat(3 * 1024 * 1024);
    let file = transcript(root.path(), "s1");
    append(&file, &line(&huge, 5, 1));
    assert_eq!(scan_it(&db, root.path()), expected(5, 1));
    append(&file, &line(&huge, 5, 1));
    assert_eq!(scan_it(&db, root.path()), expected(5, 1));
    let keys: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(keys, 0);
}

#[test]
fn one_scan_stops_reading_files_at_its_bound_and_the_next_goes_on() {
    let (root, db) = store();
    for session in 0..3 {
        let lines: String = (0..USAGES_PER_READ_MAX)
            .map(|index| line(&format!("msg_{session}_{index}"), 1, 0))
            .collect();
        append(&transcript(root.path(), &format!("s{session}")), &lines);
    }
    assert_eq!(scan_it(&db, root.path()), expected(8192, 0));
    assert_eq!(scan_it(&db, root.path()), expected(4096, 0));
}
