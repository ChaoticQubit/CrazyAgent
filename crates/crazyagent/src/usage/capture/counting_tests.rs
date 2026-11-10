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

/// A row's estimate stops at `ROW_TOKENS_MAX` when a later add would pass it: at the hour's
/// upsert, and when a removed project's rows add into the rows kept under removed projects.
/// The usage report reads the capped rows back (COUNT-6-F2).
#[test]
fn a_rows_estimate_stops_at_the_cap_when_an_add_or_a_move_would_pass_it() {
    let (_root, db) = store();
    let near = Added {
        tokens: expected(1, 0),
        cost_nanos: ROW_TOKENS_MAX - 1,
    };
    let costs = |db: &Connection| -> Vec<(String, i64)> {
        db.prepare("SELECT project,cost_nanos FROM usage_tokens ORDER BY project")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    add_tokens(&db, NOW, &who(), "s1", MODEL, near).unwrap();
    add_tokens(&db, NOW, &who(), "s1", MODEL, near).unwrap();
    assert_eq!(
        costs(&db),
        vec![("alpha".to_owned(), ROW_TOKENS_MAX)],
        "the hour's upsert stops at the cap"
    );
    super::super::forget_project(&db, "alpha").unwrap();
    add_tokens(&db, NOW, &who(), "s1", MODEL, near).unwrap();
    super::super::forget_project(&db, "alpha").unwrap();
    assert_eq!(
        costs(&db),
        vec![(super::super::REMOVED_PROJECTS.to_owned(), ROW_TOKENS_MAX)],
        "the move into removed projects stops at the cap"
    );
    let report = super::super::report::report(&db, None, None, 0, NOW + 1, &[]).unwrap();
    assert_eq!(report.total.input, 3);
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
}

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

fn token_count(at: &str, info: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "timestamp": at,
            "type": "event_msg",
            "payload": {"type": "token_count", "info": info, "rate_limits": null}
        })
    )
}

fn started(home: &Path, id: &str) -> PathBuf {
    let file = rollout(home, id);
    append(&file, &meta(T0, id, serde_json::json!({})));
    file
}

fn forked(home: &Path, id: &str) -> PathBuf {
    let file = rollout(home, id);
    append(
        &file,
        &meta(T0, id, serde_json::json!({"forked_from_id": "parent"})),
    );
    file
}

#[test]
fn a_first_event_counts_its_last_usage_not_its_running_total() {
    let (root, db) = store();
    let file = started(root.path(), "first");
    append(&file, &counted(T0_1, (1000, 0, 0), (50, 0, 0)));
    assert_eq!(scan_codex(&db, root.path()), plain(50, 0, 0));
}

#[test]
fn a_last_line_then_a_line_with_no_last_counts_only_the_difference() {
    let (root, db) = store();
    let file = started(root.path(), "mixed");
    append(&file, &counted(T0_1, (100, 40, 10), (100, 40, 10)));
    append(
        &file,
        &token_count(
            T0_2,
            serde_json::json!({"total_token_usage": usage(150, 60, 15)}),
        ),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(90, 60, 15));
}

#[test]
fn two_different_lines_with_only_a_last_usage_both_count() {
    let (root, db) = store();
    let file = started(root.path(), "last-only");
    for (at, input) in [(T0_1, 10), (T0_2, 20)] {
        append(
            &file,
            &token_count(
                at,
                serde_json::json!({"last_token_usage": usage(input, 0, 1)}),
            ),
        );
    }
    assert_eq!(scan_codex(&db, root.path()), plain(30, 0, 2));
}

#[test]
fn a_repeated_line_with_only_a_last_usage_counts_once() {
    let (root, db) = store();
    let file = started(root.path(), "repeat-last");
    let line = token_count(
        T0_1,
        serde_json::json!({"last_token_usage": usage(10, 0, 1)}),
    );
    append(&file, &line);
    append(&file, &line);
    assert_eq!(scan_codex(&db, root.path()), plain(10, 0, 1));
}

#[test]
fn a_total_that_leaves_out_a_field_keeps_that_fields_running_value() {
    let (root, db) = store();
    let file = started(root.path(), "omitted");
    let totals = [
        (T0_1, usage(100, 40, 10)),
        (
            T0_2,
            serde_json::json!({"input_tokens": 150, "output_tokens": 15, "total_tokens": 165}),
        ),
        (LATER, usage(200, 60, 20)),
    ];
    for (at, total) in totals {
        append(
            &file,
            &token_count(at, serde_json::json!({"total_token_usage": total})),
        );
    }
    assert_eq!(scan_codex(&db, root.path()), plain(140, 60, 20));
}

#[test]
fn a_usage_record_with_no_valid_counter_leaves_the_token_counts_counted() {
    let (root, db) = store();
    let file = started(root.path(), "bad-record");
    let bad = [
        serde_json::json!({}),
        serde_json::json!({"input_tokens": "x", "output_tokens": 5}),
    ];
    for record in bad {
        append(
            &file,
            &format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": T0_1,
                    "type": "token_usage_record",
                    "payload": {"response_id": "resp_1", "usage": record}
                })
            ),
        );
    }
    append(&file, &counted(T0_2, (100, 40, 10), (100, 40, 10)));
    assert_eq!(scan_codex(&db, root.path()), plain(60, 40, 10));
}

#[test]
fn a_real_call_two_seconds_after_the_copies_counts() {
    let (root, db) = store();
    let file = forked(root.path(), "gap");
    append(&file, &counted(T0_1, (100, 40, 10), (100, 40, 10)));
    append(
        &file,
        &counted("2026-11-09T03:00:02.200Z", (150, 50, 15), (50, 10, 5)),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(40, 10, 5));
}

#[test]
fn a_steady_burst_of_copies_ends_five_seconds_after_the_fork() {
    let (root, db) = store();
    let file = forked(root.path(), "window");
    let copies = [
        "2026-11-09T03:00:00.900Z",
        "2026-11-09T03:00:01.800Z",
        "2026-11-09T03:00:02.700Z",
        "2026-11-09T03:00:03.600Z",
        "2026-11-09T03:00:04.500Z",
    ];
    for (index, at) in copies.iter().enumerate() {
        let calls = index as u64 + 1;
        append(&file, &counted(at, (10 * calls, 0, calls), (10, 0, 1)));
    }
    append(
        &file,
        &counted("2026-11-09T03:00:05.400Z", (60, 0, 6), (10, 0, 1)),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(10, 0, 1));
}

#[test]
fn a_copied_line_without_totals_does_not_hide_the_same_real_call() {
    let (root, db) = store();
    let file = forked(root.path(), "copy");
    let line = |at: &str| token_count(at, serde_json::json!({"last_token_usage": usage(10, 0, 1)}));
    append(&file, &line(T0_1));
    append(&file, &line(LATER));
    assert_eq!(scan_codex(&db, root.path()), plain(10, 0, 1));
}

#[test]
fn a_rollout_named_only_by_its_session_id_is_one_thread_across_files() {
    let (root, db) = store();
    for id in ["a", "b"] {
        let file = rollout(root.path(), id);
        append(
            &file,
            &format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": T0,
                    "type": "session_meta",
                    "payload": {"session_id": "thread-9"}
                })
            ),
        );
        append(&file, &counted(T0_1, (100, 40, 10), (100, 40, 10)));
    }
    assert_eq!(scan_codex(&db, root.path()), plain(60, 40, 10));
}

#[test]
fn session_meta_names_the_rollouts_model() {
    let (root, db) = store();
    let file = rollout(root.path(), "named");
    append(
        &file,
        &meta(T0, "named", serde_json::json!({"model": CODEX})),
    );
    append(&file, &counted(T0_1, (100, 40, 10), (100, 40, 10)));
    scan_codex(&db, root.path());
    let model: String = db
        .query_row("SELECT model FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(model, CODEX);
}

#[test]
fn a_restart_with_no_previous_total_is_read_field_by_field() {
    let (root, db) = store();
    let file = started(root.path(), "restart");
    append(
        &file,
        &token_count(
            T0_1,
            serde_json::json!({
                "total_token_usage": {"input_tokens": 1000, "output_tokens": 100},
                "last_token_usage": usage(100, 0, 10)
            }),
        ),
    );
    append(
        &file,
        &token_count(
            T0_2,
            serde_json::json!({"total_token_usage": usage(50, 0, 5)}),
        ),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(150, 0, 15));
}

#[test]
fn a_running_total_past_the_line_cap_keeps_growing() {
    let (root, db) = store();
    let file = started(root.path(), "huge");
    for (at, input) in [(T0_1, 2_000_000_000), (T0_2, 2_000_000_100)] {
        append(
            &file,
            &token_count(
                at,
                serde_json::json!({"total_token_usage": usage(input, 0, 0)}),
            ),
        );
    }
    assert_eq!(scan_codex(&db, root.path()), plain(1_000_000_100, 0, 0));
}

#[test]
fn a_cache_write_is_carved_out_of_the_uncached_input() {
    let (root, db) = store();
    let file = started(root.path(), "write");
    let call = serde_json::json!({
        "input_tokens": 100,
        "cached_input_tokens": 40,
        "cache_write_input_tokens": 10,
        "output_tokens": 5,
        "total_tokens": 105
    });
    append(
        &file,
        &token_count(
            T0_1,
            serde_json::json!({"total_token_usage": call, "last_token_usage": call}),
        ),
    );
    let expected = Tokens {
        input: 50,
        output: 5,
        cache_read: 40,
        cache_write: 10,
    };
    assert_eq!(scan_codex(&db, root.path()), expected);
}

#[test]
fn a_thread_setting_turns_fast_on_and_off() {
    let mut progress = Progress::default();
    let mut fast_after = |tier: &str, calls: u64| {
        let setting = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "thread_settings_applied",
                "thread_settings": {"service_tier": tier}
            }
        });
        codex_line(&setting, &mut progress, NOW, "thread");
        let line: Value =
            serde_json::from_str(&counted(T0_1, (calls, 0, 1), (calls, 0, 1))).unwrap();
        codex_line(&line, &mut progress, NOW, "thread")
            .0
            .unwrap()
            .fast
    };
    assert!(fast_after("priority", 10));
    assert!(!fast_after("default", 20));
}

#[test]
fn a_total_an_older_build_stored_still_counts_only_the_growth() {
    let (root, db) = store();
    let file = started(root.path(), "older");
    append(
        &file,
        &token_count(
            T0_1,
            serde_json::json!({"total_token_usage": usage(100, 40, 10)}),
        ),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(60, 40, 10));
    db.execute(
        r#"UPDATE usage_files SET progress=json_set(json_remove(progress,'$.codex'),'$.total',
            json('{"input":60,"output":10,"cache_read":40,"cache_write":0}'))"#,
        [],
    )
    .unwrap();
    append(
        &file,
        &token_count(
            T0_2,
            serde_json::json!({"total_token_usage": usage(150, 50, 15)}),
        ),
    );
    assert_eq!(scan_codex(&db, root.path()), plain(40, 10, 5));
}

const SONNET: &str = "claude-sonnet-4-5";
const FAST_MODEL: &str = "claude-opus-5-5";

fn priced_line(id: &str, model: &str, usage: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-11-09T02:00:00.000Z",
            "message": {
                "id": id,
                "model": model,
                "stop_reason": "end_turn",
                "content": [],
                "usage": usage
            }
        })
    )
}

fn spent(db: &Connection) -> i64 {
    db.query_row("SELECT SUM(cost_nanos) FROM usage_tokens", [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn estimate(model: &str, charged: super::super::prices::Charged) -> i64 {
    super::super::prices::table()
        .cost_nanos(model, charged)
        .unwrap()
}

fn cost_of_one(model: &str, usage: serde_json::Value) -> i64 {
    let (root, db) = store();
    append(
        &transcript(root.path(), "s1"),
        &priced_line("msg_a", model, usage),
    );
    scan_it(&db, root.path());
    spent(&db)
}

#[test]
fn a_counted_message_lands_with_its_estimated_cost() {
    let (root, db) = store();
    append(&transcript(root.path(), "s1"), &line("msg_a", 1000, 0));
    scan_it(&db, root.path());
    let cost: i64 = db
        .query_row("SELECT cost_nanos FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    let priced = super::super::prices::table().cost_nanos(
        MODEL,
        super::super::prices::Charged {
            tokens: expected(1000, 0),
            prompt_tokens: 1000,
            ..Default::default()
        },
    );
    assert_eq!(Some(cost), priced);
    assert!(cost > 0);
}

#[test]
fn a_repeated_line_is_charged_only_for_its_growth() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 1000, 10));
    append(&file, &line("msg_b", 1, 1));
    scan_it(&db, root.path());
    let first = spent(&db);
    append(&file, &line("msg_a", 1000, 10));
    scan_it(&db, root.path());
    assert_eq!(first, spent(&db));
}

#[test]
fn two_messages_in_one_read_add_their_costs() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 1000, 0));
    append(&file, &line("msg_b", 3000, 0));
    scan_it(&db, root.path());
    let priced = |input: u64| {
        estimate(
            MODEL,
            super::super::prices::Charged {
                tokens: expected(input, 0),
                prompt_tokens: input,
                ..Default::default()
            },
        )
    };
    assert_eq!(spent(&db), priced(1000) + priced(3000));
}

#[test]
fn an_unknown_model_keeps_its_tokens_and_has_no_cost() {
    let (root, db) = store();
    let text = line("msg_a", 1000, 0).replace(MODEL, "local-llama");
    append(&transcript(root.path(), "s1"), &text);
    scan_it(&db, root.path());
    let row: (String, i64, i64) = db
        .query_row(
            "SELECT model,input,cost_nanos FROM usage_tokens",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("local-llama".to_owned(), 1000, 0));
}

#[test]
fn a_message_charges_its_one_hour_cache_write() {
    let cost = cost_of_one(
        SONNET,
        serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 1,
            "cache_creation_input_tokens": 2000,
            "cache_creation": {
                "ephemeral_1h_input_tokens": 1500,
                "ephemeral_5m_input_tokens": 500
            }
        }),
    );
    let priced = estimate(
        SONNET,
        super::super::prices::Charged {
            tokens: Tokens {
                output: 1,
                cache_write: 2000,
                ..Tokens::default()
            },
            prompt_tokens: 2000,
            one_hour: 1500,
            ..Default::default()
        },
    );
    assert_eq!(cost, priced);
}

#[test]
fn a_message_charges_its_web_searches() {
    let cost = cost_of_one(
        SONNET,
        serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 1,
            "server_tool_use": {"web_search_requests": 3}
        }),
    );
    let priced = estimate(
        SONNET,
        super::super::prices::Charged {
            tokens: expected(10, 1),
            prompt_tokens: 10,
            web_searches: 3,
            ..Default::default()
        },
    );
    assert_eq!(cost, priced);
}

#[test]
fn a_repeated_message_charges_its_web_searches_once() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    let text = priced_line(
        "msg_a",
        SONNET,
        serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 1,
            "server_tool_use": {"web_search_requests": 3}
        }),
    );
    append(&file, &text);
    scan_it(&db, root.path());
    let first = spent(&db);
    append(&file, &text);
    scan_it(&db, root.path());
    assert_eq!(first, spent(&db));
}

#[test]
fn a_fast_message_charges_the_fast_rate() {
    let cost = cost_of_one(
        FAST_MODEL,
        serde_json::json!({"input_tokens": 1000, "output_tokens": 1, "speed": "fast"}),
    );
    let priced = estimate(
        FAST_MODEL,
        super::super::prices::Charged {
            tokens: expected(1000, 1),
            prompt_tokens: 1000,
            fast: true,
            ..Default::default()
        },
    );
    assert_eq!(cost, priced);
}

#[test]
fn a_codex_write_with_no_published_rate_costs_the_calls_input_rate() {
    let cost_of = |input: u64, write: u64, output: u64| {
        let (root, db) = store();
        let file = rollout(root.path(), "unpublished");
        append(
            &file,
            &meta(T0, "unpublished", serde_json::json!({"model": "gpt-5.4"})),
        );
        let call = serde_json::json!({
            "input_tokens": input,
            "cached_input_tokens": 0,
            "cache_write_input_tokens": write,
            "output_tokens": output,
            "total_tokens": input + output
        });
        append(
            &file,
            &token_count(
                T0_1,
                serde_json::json!({"total_token_usage": call, "last_token_usage": call}),
            ),
        );
        scan_codex(&db, root.path());
        spent(&db)
    };
    assert_eq!(
        [cost_of(1_000, 100, 10), cost_of(300_000, 10_000, 1_000)],
        [2_650_000, 1_522_500_000]
    );
}

#[test]
fn a_repeat_charges_the_one_hour_rate_only_on_one_hour_growth() {
    let written = |total: u64, one_hour: u64| {
        serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 1,
            "cache_creation_input_tokens": total,
            "cache_creation": {
                "ephemeral_1h_input_tokens": one_hour,
                "ephemeral_5m_input_tokens": total - one_hour
            }
        })
    };
    let repeated = |first: (u64, u64), second: (u64, u64)| {
        let (root, db) = store();
        let file = transcript(root.path(), "s1");
        append(
            &file,
            &priced_line("msg_a", SONNET, written(first.0, first.1)),
        );
        scan_it(&db, root.path());
        append(
            &file,
            &priced_line("msg_a", SONNET, written(second.0, second.1)),
        );
        scan_it(&db, root.path());
        spent(&db)
    };
    let charge = |output: u64, cache_write: u64, one_hour: u64, prompt_tokens: u64| {
        estimate(
            SONNET,
            super::super::prices::Charged {
                tokens: Tokens {
                    output,
                    cache_write,
                    ..Tokens::default()
                },
                prompt_tokens,
                one_hour,
                ..Default::default()
            },
        )
    };
    assert_eq!(
        [
            repeated((1000, 800), (1200, 800)),
            repeated((1000, 1000), (2000, 2000)),
        ],
        [
            charge(1, 1000, 800, 1000) + charge(0, 200, 0, 1200),
            charge(1, 1000, 1000, 1000) + charge(0, 1000, 1000, 2000),
        ]
    );
}

#[test]
fn the_whole_calls_prompt_picks_the_tier() {
    let priced = |tokens: Tokens, prompt_tokens: u64| {
        estimate(
            SONNET,
            super::super::prices::Charged {
                tokens,
                prompt_tokens,
                ..Default::default()
            },
        )
    };
    assert_eq!(
        [
            cost_of_one(
                SONNET,
                serde_json::json!({
                    "input_tokens": 10,
                    "output_tokens": 1,
                    "cache_read_input_tokens": 250_000
                }),
            ),
            cost_of_one(
                SONNET,
                serde_json::json!({
                    "input_tokens": 10,
                    "output_tokens": 1,
                    "cache_creation_input_tokens": 250_000
                }),
            ),
        ],
        [
            priced(
                Tokens {
                    input: 10,
                    output: 1,
                    cache_read: 250_000,
                    ..Tokens::default()
                },
                250_010
            ),
            priced(
                Tokens {
                    input: 10,
                    output: 1,
                    cache_write: 250_000,
                    ..Tokens::default()
                },
                250_010
            ),
        ]
    );
}

#[test]
fn growth_on_a_repeated_message_keeps_the_whole_calls_tier() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(
        &file,
        &priced_line(
            "msg_a",
            SONNET,
            serde_json::json!({"input_tokens": 250_000, "output_tokens": 10}),
        ),
    );
    scan_it(&db, root.path());
    append(
        &file,
        &priced_line(
            "msg_a",
            SONNET,
            serde_json::json!({"input_tokens": 250_000, "output_tokens": 30}),
        ),
    );
    scan_it(&db, root.path());
    let priced = |tokens: Tokens| {
        estimate(
            SONNET,
            super::super::prices::Charged {
                tokens,
                prompt_tokens: 250_000,
                ..Default::default()
            },
        )
    };
    assert_eq!(
        spent(&db),
        priced(expected(250_000, 10)) + priced(expected(0, 20))
    );
}

#[test]
fn a_web_search_that_arrives_on_a_repeat_is_still_charged() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(
        &file,
        &priced_line(
            "msg_a",
            SONNET,
            serde_json::json!({"input_tokens": 10, "output_tokens": 1}),
        ),
    );
    scan_it(&db, root.path());
    append(
        &file,
        &priced_line(
            "msg_a",
            SONNET,
            serde_json::json!({
                "input_tokens": 10,
                "output_tokens": 1,
                "server_tool_use": {"web_search_requests": 2}
            }),
        ),
    );
    scan_it(&db, root.path());
    let first = estimate(
        SONNET,
        super::super::prices::Charged {
            tokens: expected(10, 1),
            prompt_tokens: 10,
            ..Default::default()
        },
    );
    let searches = estimate(
        SONNET,
        super::super::prices::Charged {
            prompt_tokens: 10,
            web_searches: 2,
            ..Default::default()
        },
    );
    assert_eq!(spent(&db), first + searches);
}

const PAST_REPLAY: &str = "2026-11-09T03:02:00.000Z";

fn user(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type": "user", "timestamp": "2026-11-09T02:00:00.000Z", "message": {"content": text}})
    )
}

fn calls(id: &str, stop: Option<&str>, tools: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-11-09T02:00:01.000Z",
            "message": {
                "id": id, "model": MODEL, "stop_reason": stop,
                "usage": {"input_tokens": 1, "output_tokens": 1},
                "content": tools
            }
        })
    )
}

fn edit(file: &str) -> serde_json::Value {
    serde_json::json!([{"type": "tool_use", "name": "Edit", "input": {"file_path": file}}])
}

fn shell(command: &str) -> serde_json::Value {
    serde_json::json!([{"type": "tool_use", "name": "Bash", "input": {"command": command}}])
}

fn activity_rows(db: &Connection) -> Vec<(String, i64, i64, i64)> {
    db.prepare(
        "SELECT activity,turns,edit_turns,first_try_turns FROM usage_activity ORDER BY activity",
    )
    .unwrap()
    .query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn names(db: &Connection, sql: &str) -> Vec<String> {
    db.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn an_edit_verified_and_edited_again_is_not_first_try() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add a page"));
    append(&file, &calls("m1", None, edit("/repo/a.rs")));
    append(&file, &calls("m2", None, shell("cargo test")));
    append(&file, &calls("m3", Some("end_turn"), edit("/repo/a.rs")));
    append(&file, &user("fix the crash"));
    append(&file, &calls("m4", Some("end_turn"), edit("/repo/b.rs")));
    scan_it(&db, root.path());
    assert_eq!(
        activity_rows(&db),
        vec![
            ("debugging".to_owned(), 1, 1, 1),
            ("feature".to_owned(), 1, 1, 0),
        ]
    );
}

#[test]
fn a_lookup_between_two_edits_is_still_first_try() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add a page"));
    append(&file, &calls("m1", None, edit("/repo/a.rs")));
    append(&file, &calls("m2", None, shell("git status")));
    append(&file, &calls("m3", Some("end_turn"), edit("/repo/a.rs")));
    scan_it(&db, root.path());
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
}

#[test]
fn a_tool_result_or_a_meta_line_does_not_start_a_turn() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add a page"));
    append(&file, &calls("m1", None, edit("/repo/a.rs")));
    let result = serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}});
    let note = serde_json::json!({"type": "user", "isMeta": true, "message": {"content": "fix the crash"}});
    append(&file, &format!("{result}\n{note}\n"));
    append(&file, &calls("m2", Some("end_turn"), edit("/repo/b.rs")));
    scan_it(&db, root.path());
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
}

#[test]
fn a_turn_split_across_two_scans_counts_once() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add a page"));
    append(&file, &calls("m1", None, edit("/repo/a.rs")));
    scan_it(&db, root.path());
    assert_eq!(activity_rows(&db), vec![]);
    append(&file, &calls("m2", Some("end_turn"), edit("/repo/b.rs")));
    scan_it(&db, root.path());
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
}

#[test]
fn a_saved_turn_holds_no_message_or_path() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add the-secret-words"));
    append(&file, &calls("m1", None, edit("/repo/secret-file.rs")));
    scan_it(&db, root.path());
    let progress: String = db
        .query_row("SELECT progress FROM usage_files", [], |row| row.get(0))
        .unwrap();
    assert!(progress.contains("\"started\":true"), "{progress}");
    assert!(!progress.contains("the-secret-words"), "{progress}");
    assert!(!progress.contains("secret-file"), "{progress}");
}

fn tools_read(db: &Connection, file: &Path, format: Format) -> usize {
    let mut progress = Progress::default();
    let mut tools = Vec::new();
    let mut seen = Seen::new(db, "usage_messages");
    read_record_with(
        file,
        format,
        &mut progress,
        NOW,
        Some(&mut tools),
        Some(&mut seen),
    );
    tools.len()
}

#[test]
fn a_resumed_copy_files_its_turns_and_tools_once() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let first = transcript(root.path(), "s1");
    append(&first, &user("add a page"));
    append(&first, &calls("m1", Some("end_turn"), edit("/repo/a.rs")));
    scan_it(&db, root.path());
    let copy = transcript(root.path(), "s2");
    append(&copy, &user("add a page"));
    append(&copy, &calls("m1", Some("end_turn"), edit("/repo/a.rs")));
    assert_eq!(
        tools_read(&db, &copy, Format::Claude),
        0,
        "a copied turn's tools are not new calls"
    );
    append(&copy, &user("fix the crash"));
    append(&copy, &calls("m2", Some("end_turn"), edit("/repo/b.rs")));
    assert_eq!(
        tools_read(&db, &copy, Format::Claude),
        1,
        "the new turn's call is"
    );
    scan_it(&db, root.path());
    assert_eq!(
        activity_rows(&db),
        vec![
            ("debugging".to_owned(), 1, 1, 1),
            ("feature".to_owned(), 1, 1, 1)
        ]
    );
}

#[test]
fn an_original_and_its_copy_in_one_scan_file_one_turn() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    for name in ["s1", "s2"] {
        let file = transcript(root.path(), name);
        append(&file, &user("add a page"));
        append(&file, &calls("m1", Some("end_turn"), edit("/repo/a.rs")));
    }
    scan_it(&db, root.path());
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
}

fn codex_user(at: &str, text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "event_msg", "payload": {"type": "user_message", "message": text}})
    )
}

fn codex_call(at: &str, name: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "response_item", "payload": {"type": "function_call", "name": name, "arguments": "{}"}})
    )
}

fn codex_done(at: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_complete"}})
    )
}

#[test]
fn a_forks_replayed_turn_files_nothing() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = rollout(root.path(), "fork");
    append(
        &file,
        &meta(
            LATER,
            "fork",
            serde_json::json!({"forked_from_id": "parent"}),
        ),
    );
    append(&file, &codex_user(T0_1, "add a page"));
    append(&file, &codex_call(T0_1, "apply_patch"));
    append(&file, &codex_done(T0_1));
    append(&file, &codex_user(PAST_REPLAY, "fix the crash"));
    append(&file, &codex_call(PAST_REPLAY, "apply_patch"));
    append(&file, &codex_done(PAST_REPLAY));
    scan_codex(&db, root.path());
    assert_eq!(activity_rows(&db), vec![("debugging".to_owned(), 1, 1, 1)]);
}

#[test]
fn a_codex_turn_files_under_its_model() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = rollout(root.path(), "one");
    append(&file, &meta(T0, "one", serde_json::json!({})));
    append(&file, &codex_user(T0_1, "add a page"));
    append(&file, &context(T0_1));
    append(&file, &codex_call(T0_2, "apply_patch"));
    append(&file, &codex_done(T0_2));
    scan_codex(&db, root.path());
    assert_eq!(
        names(&db, "SELECT model FROM usage_activity"),
        vec![CODEX.to_owned()]
    );
}

#[test]
fn past_its_caps_a_turn_is_left_out_of_the_chart_only() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    append(&file, &user("add a page"));
    append(&file, &calls("m1", Some("end_turn"), edit("/repo/a.rs")));
    assert_eq!(scan_it(&db, root.path()), expected(1, 1));
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
    at_cap(&db, "usage_activity");
    append(&file, &user("fix the crash"));
    append(&file, &calls("m2", Some("end_turn"), edit("/repo/b.rs")));
    assert_eq!(scan_it(&db, root.path()), expected(1, 1));
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 1, 1, 1)]);
}

#[test]
fn past_sixteen_models_in_an_hour_a_turn_files_under_no_model() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    for index in 0..17 {
        append(&file, &user("add a page"));
        let turn = calls(&format!("m{index}"), Some("end_turn"), edit("/repo/a.rs"));
        append(&file, &turn.replace(MODEL, &format!("model-{index}")));
    }
    scan_it(&db, root.path());
    let models = names(&db, "SELECT model FROM usage_activity ORDER BY model");
    assert_eq!(models.len(), 17, "{models:?}");
    assert_eq!(models[0], "", "{models:?}");
}

#[test]
fn a_forgotten_projects_turns_stay_in_the_chart_under_removed_projects() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = transcript(root.path(), "s1");
    for id in ["m1", "m2"] {
        append(&file, &user("add a page"));
        append(&file, &calls(id, Some("end_turn"), edit("/repo/a.rs")));
        scan_it(&db, root.path());
        super::super::forget_project(&db, "alpha").unwrap();
    }
    assert_eq!(
        names(&db, "SELECT project FROM usage_activity"),
        vec![super::super::REMOVED_PROJECTS.to_owned()]
    );
    assert_eq!(activity_rows(&db), vec![("feature".to_owned(), 2, 2, 2)]);
}

fn codex_tool(at: &str, name: &str, arguments: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "response_item", "payload": {"type": "function_call", "name": name, "arguments": arguments.to_string()}})
    )
}

fn codex_local_shell(at: &str, command: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "response_item", "payload": {"type": "local_shell_call", "action": {"type": "exec", "command": command}}})
    )
}

fn codex_patch(at: &str, head: &str, file: &str) -> String {
    let patch = format!("*** Begin Patch\n*** {head} File: {file}\n@@\n+x\n*** End Patch\n");
    format!(
        "{}\n",
        serde_json::json!({"timestamp": at, "type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "input": patch}})
    )
}

fn edits_around(between: &str) -> Vec<(String, i64, i64, i64)> {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = rollout(root.path(), "one");
    append(&file, &meta(T0, "one", serde_json::json!({})));
    append(&file, &codex_user(T0_1, "add a page"));
    append(&file, &codex_call(T0_1, "apply_patch"));
    append(&file, between);
    append(&file, &codex_call(T0_2, "apply_patch"));
    append(&file, &codex_done(T0_2));
    scan_codex(&db, root.path());
    activity_rows(&db)
}

fn retried() -> Vec<(String, i64, i64, i64)> {
    vec![("feature".to_owned(), 1, 1, 0)]
}

#[test]
fn a_codex_turn_takes_a_turn_context_written_before_its_message() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = rollout(root.path(), "one");
    append(&file, &meta(T0, "one", serde_json::json!({})));
    append(&file, &context(T0_1));
    append(&file, &codex_user(T0_1, "add a page"));
    append(&file, &codex_call(T0_2, "apply_patch"));
    append(&file, &codex_done(T0_2));
    scan_codex(&db, root.path());
    assert_eq!(
        names(&db, "SELECT model FROM usage_activity"),
        vec![CODEX.to_owned()]
    );
}

#[test]
fn a_codex_shell_call_with_an_array_command_verifies_an_edit() {
    let arguments = serde_json::json!({"command": ["bash", "-lc", "cargo test"]});
    assert_eq!(
        edits_around(&codex_tool(T0_1, "shell", arguments)),
        retried()
    );
}

#[test]
fn a_codex_shell_command_call_verifies_an_edit() {
    let arguments = serde_json::json!({"command": "cargo test"});
    assert_eq!(
        edits_around(&codex_tool(T0_1, "shell_command", arguments)),
        retried()
    );
}

#[test]
fn a_codex_container_exec_call_verifies_an_edit() {
    let arguments = serde_json::json!({"command": ["cargo", "test"]});
    assert_eq!(
        edits_around(&codex_tool(T0_1, "container.exec", arguments)),
        retried()
    );
}

#[test]
fn a_codex_local_shell_call_verifies_an_edit() {
    let command = serde_json::json!(["bash", "-lc", "cargo test"]);
    assert_eq!(edits_around(&codex_local_shell(T0_1, command)), retried());
}

#[test]
fn a_lookup_wrapped_in_a_codex_shell_is_still_first_try() {
    let lookups = [
        codex_tool(
            T0_1,
            "shell",
            serde_json::json!({"command": ["bash", "-lc", "git status"]}),
        ),
        codex_tool(
            T0_1,
            "shell",
            serde_json::json!({"command": ["/bin/zsh", "-lc", "rg foo"]}),
        ),
        codex_local_shell(T0_1, serde_json::json!(["bash", "-lc", "cat a.rs"])),
    ];
    for between in lookups {
        assert_eq!(
            edits_around(&between),
            vec![("feature".to_owned(), 1, 1, 1)],
            "{between}"
        );
    }
}

#[test]
fn a_codex_patch_files_by_its_first_path_and_stores_only_a_hash() {
    let (root, db) = store();
    super::super::recount::begin(&db, &[], 0).unwrap();
    let file = rollout(root.path(), "one");
    let test = serde_json::json!({"cmd": "cargo test"});
    append(&file, &meta(T0, "one", serde_json::json!({})));
    append(&file, &codex_user(T0_1, "add a page"));
    append(&file, &codex_patch(T0_1, "Update", "src/secret-a.rs"));
    scan_codex(&db, root.path());
    let progress: String = db
        .query_row("SELECT progress FROM usage_files", [], |row| row.get(0))
        .unwrap();
    assert!(progress.contains("\"edits\":[["), "{progress}");
    assert!(!progress.contains("secret-a"), "{progress}");
    append(&file, &codex_tool(T0_1, "exec_command", test.clone()));
    append(&file, &codex_patch(T0_1, "Add", "src/secret-b.rs"));
    append(&file, &codex_done(T0_1));
    append(&file, &codex_user(T0_2, "fix the crash"));
    append(&file, &codex_patch(T0_2, "Update", "src/a.rs"));
    append(&file, &codex_tool(T0_2, "exec_command", test));
    let patch = "*** Begin Patch\n*** Update File: src/a.rs\n*** End Patch\n";
    append(
        &file,
        &codex_tool(T0_2, "apply_patch", serde_json::json!({"input": patch})),
    );
    append(&file, &codex_done(T0_2));
    scan_codex(&db, root.path());
    assert_eq!(
        activity_rows(&db),
        vec![
            ("debugging".to_owned(), 1, 1, 0),
            ("feature".to_owned(), 1, 1, 1),
        ]
    );
}

#[test]
fn a_forks_replayed_tool_call_is_not_a_new_call() {
    let (root, db) = store();
    let file = rollout(root.path(), "fork");
    append(
        &file,
        &meta(
            LATER,
            "fork",
            serde_json::json!({"forked_from_id": "parent"}),
        ),
    );
    append(&file, &codex_call(T0_1, "apply_patch"));
    assert_eq!(
        tools_read(&db, &file, Format::Codex),
        0,
        "a replayed call outside any turn is not new"
    );
    append(&file, &codex_user(PAST_REPLAY, "fix the crash"));
    append(&file, &codex_call(PAST_REPLAY, "apply_patch"));
    assert_eq!(
        tools_read(&db, &file, Format::Codex),
        1,
        "the call after the replay is"
    );
}
