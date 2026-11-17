use super::*;

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

/// W10b: each record has an identity of its own that a rename keeps, as an open reads it on
/// every platform, so a record swapped in at a listed path is never taken for the one listed
/// (recount rows `a_file_swapped_after_its_listing_is_refused` and
/// `a_session_with_one_file_not_read_to_its_end_keeps_its_old_numbers` on Windows).
#[test]
fn each_record_has_its_own_identity_and_a_rename_keeps_it() {
    let home = tempfile::tempdir().unwrap();
    let first = home.path().join("a.jsonl");
    let second = home.path().join("b.jsonl");
    std::fs::write(&first, "{}\n").unwrap();
    std::fs::write(&second, "{}\n").unwrap();
    let one = open_record(&first).unwrap().1;
    assert_ne!(one, open_record(&second).unwrap().1);
    let moved = home.path().join("c.jsonl");
    std::fs::rename(&first, &moved).unwrap();
    assert_eq!(open_record(&moved).unwrap().1, one);
}
