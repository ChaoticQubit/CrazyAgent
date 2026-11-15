use super::*;
use crate::harnesses::launch::failure;
use crate::harnesses::manifest;
use crate::harnesses::records::json_files::tests::{Home, NEW, SESSION_ID};
use crate::harnesses::transcript::EVENTS_PER_READ_MAX;
use serde_json::json;

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-json-rewrite.yaml");

fn record_below() -> String {
    format!("standin/records/{SESSION_ID}.json")
}

fn standin_pattern(home: &Home) -> Pattern {
    let standin = manifest::parse(STANDIN.as_bytes()).unwrap();
    Pattern::read(&standin, &home.path(), SESSION_ID).unwrap()
}

/// Writes the record anew as the stand-in does: the whole list into a scratch file beside
/// it, then moved over it, so each rewrite is a new file. Inputs: the home and the list.
/// Output: the record's full path.
fn rewrite(home: &Home, list: &Value) -> PathBuf {
    let record = home.path().join(record_below());
    let scratch = home.write("standin/records/scratch", &list.to_string(), NEW);
    std::fs::rename(scratch, &record).unwrap();
    record
}

fn nothing() -> Vec<Value> {
    Vec::new()
}

#[test]
fn each_rewrite_moved_over_the_record_returns_the_entries_past_the_position() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    assert_eq!(reader.read().unwrap(), nothing());
    let record = rewrite(&home, &json!([{"turn": 1}]));
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 1})]);
    assert_eq!(reader.record(), Some(record));
    assert_eq!(reader.position(), 1);
    rewrite(&home, &json!([{"turn": 1}, {"turn": 2}, {"turn": 3}]));
    let grown = vec![json!({"turn": 2}), json!({"turn": 3})];
    assert_eq!(reader.read().unwrap(), grown);
    assert_eq!(reader.position(), 3);
    assert_eq!(reader.read().unwrap(), nothing());
}

#[test]
fn an_entry_already_returned_and_changed_since_is_not_returned_again() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    rewrite(&home, &json!([{"turn": 1, "text": "a"}]));
    let first = vec![json!({"turn": 1, "text": "a"})];
    assert_eq!(reader.read().unwrap(), first);
    rewrite(&home, &json!([{"turn": 1, "text": "ab"}, {"turn": 2}]));
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 2})]);
}

#[test]
fn a_shorter_list_moves_the_position_back_and_returns_nothing() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    rewrite(&home, &json!([{"turn": 1}, {"turn": 2}, {"turn": 3}]));
    assert_eq!(reader.read().unwrap().len(), 3);
    rewrite(&home, &json!([{"turn": 1}]));
    assert_eq!(reader.read().unwrap(), nothing());
    assert_eq!(reader.position(), 1);
    rewrite(&home, &json!([{"turn": 1}, {"turn": 4}]));
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 4})]);
}

#[test]
fn a_record_written_in_place_and_caught_half_written_is_no_news_until_whole() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    let record = home.write(&record_below(), "[{\"turn\":1}]", NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 1})]);
    std::fs::write(&record, "[{\"turn\":1},{\"tu").unwrap();
    assert_eq!(reader.read().unwrap(), nothing());
    assert_eq!(reader.position(), 1);
    std::fs::write(&record, "[{\"turn\":1},{\"turn\":2}]").unwrap();
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 2})]);
}

#[test]
fn a_record_left_before_the_start_is_passed_over_until_it_is_written_anew() {
    let home = Home::new();
    home.write(&record_below(), "[{\"earlier\":1}]", NEW);
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    assert_eq!(reader.read().unwrap(), nothing());
    assert_eq!(reader.record(), None);
    rewrite(&home, &json!([{"earlier": 1}, {"later": 1}]));
    let whole = vec![json!({"earlier": 1}), json!({"later": 1})];
    assert_eq!(reader.read().unwrap(), whole);
}

#[test]
fn entries_that_are_no_json_object_are_passed_over_and_counted() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    rewrite(&home, &json!([1, "text", null, {"turn": 1}]));
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 1})]);
    assert_eq!(reader.position(), 4);
}

#[test]
fn a_record_that_is_no_json_list_answers_a_plain_code() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    home.write(&record_below(), "{\"turns\":[]}", NEW);
    let unavailable = Err(failure("launch_transcript_unavailable"));
    assert_eq!(reader.read(), unavailable);
}

#[test]
fn one_read_returns_at_most_the_object_bound_and_the_rest_come_next() {
    let home = Home::new();
    let mut reader = JsonRewrite::new(standin_pattern(&home));
    let turns = 0..EVENTS_PER_READ_MAX + 5;
    let many: Vec<Value> = turns.map(|turn| json!({"turn": turn})).collect();
    rewrite(&home, &Value::Array(many));
    assert_eq!(reader.read().unwrap().len(), EVENTS_PER_READ_MAX);
    assert_eq!(reader.read().unwrap().len(), 5);
    assert_eq!(reader.position(), EVENTS_PER_READ_MAX + 5);
}

#[test]
fn a_resumed_reader_reads_on_from_its_position_and_a_skipped_one_from_the_end() {
    let home = Home::new();
    rewrite(&home, &json!([{"turn": 1}, {"turn": 2}]));
    let mut resumed = JsonRewrite::resumed(standin_pattern(&home), 1);
    assert_eq!(resumed.read().unwrap(), vec![json!({"turn": 2})]);
    let mut adopted = JsonRewrite::resumed(standin_pattern(&home), 0);
    adopted.skip_to_end().unwrap();
    assert_eq!(adopted.position(), 2);
    assert_eq!(adopted.read().unwrap(), nothing());
    rewrite(&home, &json!([{"turn": 1}, {"turn": 2}, {"turn": 3}]));
    assert_eq!(adopted.read().unwrap(), vec![json!({"turn": 3})]);
}
