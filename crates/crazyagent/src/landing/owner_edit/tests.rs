use super::*;
use crate::ide::workspace::tests::Case;
use crate::landing::{ChangeSetId, FileMark};

fn now() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    u64::try_from(elapsed.as_millis()).unwrap()
}

/// The marks of every file in a change set's first diff page, by path.
fn marks(case: &Case, id: &ChangeSetId) -> Vec<(String, Vec<FileMark>)> {
    let db = crate::landing::open(&case.root).unwrap();
    let head = crate::landing::ide::view(&db, &case.project, id)
        .unwrap()
        .head;
    crate::landing::diff(&db, id, &head, 0)
        .unwrap()
        .files
        .into_iter()
        .map(|file| (file.path, file.marks))
        .collect()
}

#[test]
fn owner_edit_in_a_running_agent_worktree_is_recorded_in_its_change_set() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    std::fs::write(agent.worktree.join("README"), "one\nedited by the owner\n").unwrap();
    let db = crate::landing::open(&case.root).unwrap();
    record(&db, &agent.worktree, "README", now()).unwrap();
    let first = case.change(&agent);
    assert_eq!(
        marks(&case, &first),
        vec![
            ("README".to_owned(), vec![FileMark::OwnerEdited]),
            ("notes.txt".to_owned(), vec![]),
        ]
    );
    let head = crate::landing::list(&db, &case.project)
        .unwrap()
        .into_iter()
        .find(|set| set.id == first)
        .unwrap()
        .head;
    std::thread::sleep(std::time::Duration::from_millis(5));
    crate::landing::discard(&db, &first, &head, now()).unwrap();
    assert_eq!(
        marks(&case, &first),
        vec![
            ("README".to_owned(), vec![FileMark::OwnerEdited]),
            ("notes.txt".to_owned(), vec![]),
        ]
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    std::fs::write(agent.worktree.join("README"), "one\nagain by the agent\n").unwrap();
    let second = case.change(&agent);
    assert_ne!(second, first);
    assert!(
        marks(&case, &second)
            .iter()
            .all(|(_, marks)| !marks.contains(&FileMark::OwnerEdited))
    );
}

#[test]
fn owner_edit_paths_must_stay_inside_the_worktree() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    let db = crate::landing::open(&case.root).unwrap();
    for path in [
        "",
        "/etc/passwd",
        "../outside",
        "src/../../outside",
        "./README",
        "src\0name",
    ] {
        assert_eq!(
            record(&db, &agent.worktree, path, now()).unwrap_err().0,
            "changes_path_invalid",
            "{path:?}"
        );
    }
    assert_eq!(
        record(&db, &agent.worktree, &"a".repeat(PATH_BYTES_MAX + 1), now())
            .unwrap_err()
            .0,
        "changes_path_invalid"
    );
    assert_eq!(
        record(&db, Path::new("relative/worktree"), "README", now())
            .unwrap_err()
            .0,
        "changes_path_invalid"
    );
    record(&db, &agent.worktree, "src/lib.rs", now()).unwrap();
    record(&db, &agent.worktree, "src/lib.rs", now()).unwrap();
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM landing_owner_edits WHERE worktree=?1",
            [agent.worktree.to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn owner_edits_stop_at_their_bound_per_worktree() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    let db = crate::landing::open(&case.root).unwrap();
    for index in 0..PATHS_PER_WORKTREE_MAX {
        record(&db, &agent.worktree, &format!("file-{index}"), now()).unwrap();
    }
    assert_eq!(
        record(&db, &agent.worktree, "one-more", now())
            .unwrap_err()
            .0,
        "changes_owner_edits_full"
    );
    record(&db, &agent.worktree, "file-0", now()).unwrap();
}

#[test]
fn owner_edit_marks_the_change_set_its_turn_end_records_not_one_closed_before() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    std::fs::write(agent.worktree.join("README"), "one\nby the agent\n").unwrap();
    let first = case.change(&agent);
    let db = crate::landing::open(&case.root).unwrap();
    std::fs::write(agent.worktree.join("README"), "one\nby the owner\n").unwrap();
    record(&db, &agent.worktree, "README", now()).unwrap();
    let head = crate::landing::list(&db, &case.project)
        .unwrap()
        .into_iter()
        .find(|set| set.id == first)
        .unwrap()
        .head;
    crate::landing::discard(&db, &first, &head, now()).unwrap();
    let second = case.change(&agent);
    assert_ne!(second, first);
    assert_eq!(
        marks(&case, &first),
        vec![
            ("README".to_owned(), vec![]),
            ("notes.txt".to_owned(), vec![])
        ]
    );
    assert_eq!(
        marks(&case, &second),
        vec![
            ("README".to_owned(), vec![FileMark::OwnerEdited]),
            ("notes.txt".to_owned(), vec![])
        ]
    );
}
