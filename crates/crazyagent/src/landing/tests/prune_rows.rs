use super::*;

/// A change set restored and discarded again after a prune pass selected it, but before the
/// pass held its repository, keeps its new restore window: pruning it then deletes nothing
/// and a later restore inside that window still works.
#[test]
fn a_set_discarded_again_after_the_prune_selection_keeps_its_new_window() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    let pass = NOW_MS + 7 * DAY_MS;
    let due = store::discarded_due(&case.db, pass).unwrap();
    assert!(
        due.iter().any(|found| found.id == id),
        "the pass selects the set"
    );
    restore(&case.db, &id, pass - 1).unwrap();
    discard(&case.db, &id, &head, pass).unwrap();
    let again = store::load(&case.db, &id).unwrap();
    assert_eq!(again.restorable_until_ms, Some(pass + 7 * DAY_MS));
    assert!(again.restorable_until_ms.is_some_and(|until| until > pass));

    let pruned = crate::landing::prune::prune_one(&case.db, &id, pass);

    assert_eq!(pruned, Ok(false));
    let reference = format!("refs/crazyagent/discarded/{}", id.as_str());
    assert_eq!(case.owner(&["rev-parse", &reference]).trim(), head);
    assert!(!store::load(&case.db, &id).unwrap().pruned);
    restore(&case.db, &id, pass + 1).unwrap();
}

/// A discarded set whose repository cannot be read when its window ends is not marked
/// pruned, so the next pass deletes its ref once the repository reads again.
#[test]
fn a_prune_that_cannot_read_the_repository_is_tried_again() {
    let case = Case::new();
    if rustix::process::geteuid().is_root() {
        return;
    }
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    let pass = NOW_MS + 7 * DAY_MS;
    std::fs::set_permissions(&case.repo, std::fs::Permissions::from_mode(0o000)).unwrap();

    let first = prune(&case.db, pass);
    let marked = store::load(&case.db, &id).map(|record| record.pruned);
    std::fs::set_permissions(&case.repo, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(first, Ok(0));
    assert_eq!(marked, Ok(false));
    assert_eq!(prune(&case.db, pass), Ok(1));
    assert_eq!(case.owner(&["for-each-ref", "refs/crazyagent/"]), "");
    assert!(store::load(&case.db, &id).unwrap().pruned);
}

/// A discarded set whose repository folder was replaced by a file is gone like a removed
/// repository: the pass marks it pruned once instead of refusing it on every pass.
#[test]
fn a_set_whose_repository_became_a_file_is_marked_pruned() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    std::fs::remove_dir_all(&case.repo).unwrap();
    write(&case.repo, "not a repository\n");

    assert_eq!(prune(&case.db, NOW_MS + 7 * DAY_MS), Ok(1));
    assert!(store::load(&case.db, &id).unwrap().pruned);
    assert_eq!(prune(&case.db, NOW_MS + 8 * DAY_MS), Ok(0));
}
