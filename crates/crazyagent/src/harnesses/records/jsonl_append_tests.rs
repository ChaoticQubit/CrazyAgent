use super::*;
use crate::harnesses::records::json_files::tests::{Home, NEW, SESSION_ID, STANDIN_PATH, pattern};
use crate::harnesses::transcript::{EVENTS_PER_READ_MAX, LINE_BYTES_MAX, READ_BYTES_MAX};
use serde_json::json;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

const DEEP_PATH: &str = "{home}/agent/sessions/**/*_{session}.jsonl";

fn record_below() -> String {
    format!("standin/records/{SESSION_ID}.jsonl")
}

fn standin(home: &Home) -> JsonlAppend {
    JsonlAppend::new(pattern(home, STANDIN_PATH).unwrap())
}

/// Appends text to a record as a harness does. Inputs: the record and the text. Output:
/// none.
fn append(record: &Path, text: &str) {
    let mut file = File::options().append(true).open(record).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn nothing() -> Vec<Value> {
    Vec::new()
}

#[test]
fn the_record_is_found_once_written_and_read_line_by_line_as_it_grows() {
    let home = Home::new();
    let mut reader = standin(&home);
    assert_eq!(reader.read().unwrap(), nothing());
    assert_eq!(reader.record(), None);
    let record = home.write(&record_below(), "{\"turn\":1}\n", NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 1})]);
    assert_eq!(reader.record(), Some(record.clone()));
    append(&record, "{\"turn\":2}\n{\"turn\":3}\n");
    let grown = vec![json!({"turn": 2}), json!({"turn": 3})];
    assert_eq!(reader.read().unwrap(), grown);
    assert_eq!(reader.read().unwrap(), nothing());
}

#[test]
fn the_newest_session_file_under_a_deep_record_path_is_the_one_read() {
    let home = Home::new();
    let mut reader = JsonlAppend::new(pattern(&home, DEEP_PATH).unwrap());
    let older = format!("agent/sessions/a/1_{SESSION_ID}.jsonl");
    home.write(&older, "{\"older\":1}\n", Duration::from_secs(60));
    let newest = format!("agent/sessions/b/2_{SESSION_ID}.jsonl");
    let newest = home.write(&newest, "{\"newest\":1}\n", NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"newest": 1})]);
    assert_eq!(reader.record(), Some(newest));
}

#[test]
fn a_record_left_before_the_start_is_never_read_and_a_later_one_is_whatever_its_age() {
    let home = Home::new();
    let earlier = format!("agent/sessions/a/1_{SESSION_ID}.jsonl");
    home.write(&earlier, "{\"earlier\":1}\n", NEW);
    let mut reader = JsonlAppend::new(pattern(&home, DEEP_PATH).unwrap());
    assert_eq!(reader.read().unwrap(), nothing());
    let later = format!("agent/sessions/b/2_{SESSION_ID}.jsonl");
    let later = home.write(&later, "{\"later\":1}\n", Duration::from_secs(60));
    assert_eq!(reader.read().unwrap(), vec![json!({"later": 1})]);
    assert_eq!(reader.record(), Some(later));
}

#[test]
fn a_half_written_last_line_waits_for_its_newline() {
    let home = Home::new();
    let mut reader = standin(&home);
    let record = home.write(&record_below(), "{\"a\":1}\n{\"b\":", NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"a": 1})]);
    assert_eq!(reader.read().unwrap(), nothing());
    append(&record, "2}\n");
    assert_eq!(reader.read().unwrap(), vec![json!({"b": 2})]);
}

#[test]
fn a_line_that_is_no_json_object_is_passed_over() {
    let home = Home::new();
    let mut reader = standin(&home);
    let lines = "[1]\nnot json\n\"text\"\n\n{\"c\":3}\n";
    home.write(&record_below(), lines, NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"c": 3})]);
}

#[test]
fn a_record_replaced_at_its_place_is_followed_from_its_end_with_nothing_replayed() {
    let home = Home::new();
    let mut reader = standin(&home);
    let record = home.write(&record_below(), "{\"turn\":1}\n", NEW);
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 1})]);
    let rewritten = "{\"turn\":1}\n{\"turn\":2}\n";
    let swap = home.write("standin/records/swap", rewritten, NEW);
    std::fs::rename(swap, &record).unwrap();
    assert_eq!(reader.read().unwrap(), nothing());
    append(&record, "{\"turn\":3}\n");
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 3})]);
    assert_eq!(reader.record(), Some(record));
}

#[test]
fn a_record_cut_shorter_than_what_was_read_is_followed_from_its_end() {
    let home = Home::new();
    let mut reader = standin(&home);
    let record = home.write(&record_below(), "{\"turn\":1}\n{\"turn\":2}\n", NEW);
    assert_eq!(reader.read().unwrap().len(), 2);
    let cut = File::options().write(true).open(&record).unwrap();
    cut.set_len(5).unwrap();
    assert_eq!(reader.read().unwrap(), nothing());
    append(&record, "\n{\"turn\":3}\n");
    assert_eq!(reader.read().unwrap(), vec![json!({"turn": 3})]);
}

#[test]
fn a_line_over_the_line_bound_is_passed_over_to_its_newline() {
    let home = Home::new();
    let mut reader = standin(&home);
    let text = "x".repeat(LINE_BYTES_MAX + READ_BYTES_MAX + 1);
    let record = home.write(&record_below(), &format!("{{\"long\":\"{text}\"}}\n"), NEW);
    append(&record, "{\"after\":1}\n");
    let mut objects = Vec::new();
    for _ in 0..LINE_BYTES_MAX / READ_BYTES_MAX + 4 {
        objects.extend(reader.read().unwrap());
    }
    assert_eq!(objects, vec![json!({"after": 1})]);
}

#[test]
fn one_read_returns_at_most_the_object_bound_and_the_rest_come_next() {
    let home = Home::new();
    let mut reader = standin(&home);
    let lines = "{}\n".repeat(EVENTS_PER_READ_MAX + 5);
    home.write(&record_below(), &lines, NEW);
    assert_eq!(reader.read().unwrap().len(), EVENTS_PER_READ_MAX);
    assert_eq!(reader.read().unwrap().len(), 5);
}

#[test]
fn a_resumed_reader_reads_on_from_the_position_an_earlier_reader_reached() {
    let home = Home::new();
    let mut first = standin(&home);
    let record = home.write(&record_below(), "{\"turn\":1}\n{\"turn\":2", NEW);
    assert_eq!(first.read().unwrap(), vec![json!({"turn": 1})]);
    assert_eq!(first.position(), 11);
    append(&record, "}\n{\"turn\":3}\n");
    let pattern = pattern(&home, STANDIN_PATH).unwrap();
    let mut resumed = JsonlAppend::resumed(pattern, first.position());
    let rest = vec![json!({"turn": 2}), json!({"turn": 3})];
    assert_eq!(resumed.read().unwrap(), rest);
    let length = std::fs::metadata(&record).unwrap().len();
    assert_eq!(resumed.position(), length);
}

#[test]
fn a_reader_skipped_to_the_end_reads_only_lines_written_after() {
    let home = Home::new();
    let record = home.write(&record_below(), "{\"turn\":1}\n", NEW);
    let pattern = pattern(&home, STANDIN_PATH).unwrap();
    let mut adopted = JsonlAppend::resumed(pattern, 0);
    adopted.skip_to_end().unwrap();
    assert_eq!(adopted.record(), Some(record.clone()));
    assert_eq!(adopted.read().unwrap(), nothing());
    append(&record, "{\"turn\":2}\n");
    assert_eq!(adopted.read().unwrap(), vec![json!({"turn": 2})]);
}
