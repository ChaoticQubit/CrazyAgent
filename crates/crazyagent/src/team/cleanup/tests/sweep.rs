use super::*;

/// DEC-123, After N days: the sweep removes the worktree of each finished agent past its wait,
/// an agent with no finish time counting as past it, and keeps the rest, a check keeping one
/// never stopping the others; once the wait is over the next pass takes the one that waited.
#[test]
fn the_sweep_after_n_days_removes_only_agents_past_their_wait() {
    let case = Case::new();
    for id in 1..=4 {
        case.agent(id, "sub", "stopped");
    }
    case.stopped_at(1, Some(NOW - 3 * DAY_MS - 1));
    case.stopped_at(2, Some(NOW - 3 * DAY_MS + 60_000));
    case.stopped_at(3, None);
    case.stopped_at(4, Some(NOW - 10 * DAY_MS));
    write(&case.worktree(4).join("notes.txt"), b"never added\n");
    assert_eq!(
        case.sweep(CleanupMode::AfterDays, 3, 0, NOW),
        swept(&[1, 3], 0)
    );
    for id in [1, 3] {
        assert!(!case.worktree(id).exists() && !case.known(id), "agent {id}");
        assert!(case.history_kept(id), "agent {id} keeps its history");
    }
    assert!(case.kept(2), "agent 2 has not waited 3 days");
    assert!(case.kept(4), "a check keeps agent 4");
    assert_eq!(case.held(), [(4, "cleanup_worktree_dirty".to_owned())]);
    assert_eq!(
        case.sweep(CleanupMode::AfterDays, 3, 0, NOW + DAY_MS),
        swept(&[2], 0)
    );
    assert!(case.kept(4));
}

/// DEC-123, Once its work lands: the sweep removes a finished agent's worktree as soon as
/// nothing in it is unlanded, with no wait, and keeps one whose commits never landed.
#[test]
fn the_sweep_once_landed_removes_a_landed_agent_at_once() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    case.agent_commit(2, "unlanded.txt");
    case.agent(3, "sub", "stopped");
    write(&case.worktree(3).join("README"), b"landed\n");
    let landed = case.turn_end(3);
    case.accept(&landed);
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 30, 0, NOW),
        swept(&[1, 3], 0)
    );
    assert!(case.kept(2));
    assert_eq!(case.held(), [(2, "cleanup_commits_unlanded".to_owned())]);
    assert!(case.history_kept(1) && case.history_kept(3));
}

/// DEC-123, Only when I click Clean up: the sweep removes nothing, however long an agent
/// finished, and the button still does.
#[test]
fn the_sweep_set_to_manual_removes_nothing() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.stopped_at(1, Some(NOW - 400 * DAY_MS));
    case.agent(2, "sub", "stopped");
    case.stopped_at(2, None);
    assert_eq!(case.sweep(CleanupMode::Manual, 1, 0, NOW), swept(&[], 0));
    assert!(case.kept(1) && case.kept(2));
    assert_eq!(case.listed(), [2, 1]);
    assert_eq!(case.remove(&[1, 2]), removed(&[1, 2]));
}

/// A sweep pass removes at most `RECLAIMABLE_MAX` worktrees and says where it stopped, and
/// the next pass, starting there, takes the rest and starts over.
#[test]
fn a_sweep_pass_removes_at_most_its_bound_and_the_next_pass_takes_the_rest() {
    let case = Case::new();
    let last = i64::try_from(RECLAIMABLE_MAX).unwrap() + 1;
    for id in 1..=last {
        case.agent(id, "sub", "stopped");
    }
    let first = case.sweep(CleanupMode::OnceLanded, 1, 0, NOW);
    let bound: Vec<i64> = (1..last).collect();
    assert_eq!(first, swept(&bound, last - 1));
    assert!(case.kept(last));
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, first.after, NOW),
        swept(&[last], 0)
    );
    assert!(!case.worktree(last).exists());
}

/// A sweep pass reads at most `CANDIDATES_MAX` agents, so agents a check keeps fill a pass
/// without starving a newer one: the next pass starts after them and removes it.
#[test]
fn agents_a_check_keeps_never_hold_the_sweep_back_from_a_newer_one() {
    let case = Case::new();
    let held = i64::try_from(CANDIDATES_MAX).unwrap();
    for id in 1..=held {
        case.waiting_on_a_task(id);
    }
    case.agent(held + 1, "sub", "stopped");
    let first = case.sweep(CleanupMode::OnceLanded, 1, 0, NOW);
    assert_eq!(first, swept(&[], held));
    assert!(
        case.kept(held + 1),
        "the newer agent waits for the next pass"
    );
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, first.after, NOW),
        swept(&[held + 1], 0)
    );
    assert!(!case.worktree(held + 1).exists());
}

const SWEPT_IGNORED_HEAD: &str = "The worktree of ";
const SWEPT_IGNORED_MIDDLE: &str = " was removed; the files git ignores in it went with it: ";

/// Commits more ignore patterns in the project repository, so agent worktrees made after it
/// ignore them too. Inputs: the case and the pattern lines.
fn ignoring(case: &Case, patterns: &str) {
    let ignore = case.repo.join(".gitignore");
    let mut text = std::fs::read_to_string(&ignore).unwrap();
    text.push_str(patterns);
    write(&ignore, text.as_bytes());
    git_in(&case.home, &case.repo, &["add", ".gitignore"]);
    git_in(
        &case.home,
        &case.repo,
        &["commit", "-q", "-m", "Ignore more"],
    );
}

/// The engine's notes in the case project's team conversation, oldest first.
fn notes(case: &Case) -> Vec<String> {
    case.db
        .prepare("SELECT text FROM team_messages WHERE project=?1 AND state='note' ORDER BY id")
        .unwrap()
        .query_map([case.project.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The name the owner's lists give a finished agent. Inputs: the case and the agent.
fn label(case: &Case, id: i64) -> String {
    finished(&case.db, &case.project, NOW)
        .unwrap()
        .into_iter()
        .find(|agent| agent.id == id)
        .unwrap()
        .label
}

/// The sweep that removes a linked worktree and an own clone holding files git ignores
/// writes one note for each, naming a loose ignored file and a folder git ignores whole.
#[test]
fn a_swept_worktree_holding_ignored_files_is_noted_with_their_names() {
    let case = Case::new();
    ignoring(&case, ".env.local\n");
    case.agent(1, "sub", "stopped");
    case.clone_agent(2, "stopped");
    for id in [1, 2] {
        write(&case.worktree(id).join(".env.local"), b"TOKEN=stand-in\n");
        write(&case.worktree(id).join("target/debug/app"), b"built\n");
    }
    let names = [label(&case, 1), label(&case, 2)];
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1, 2], 0)
    );
    let noted: Vec<String> = names
        .iter()
        .map(|name| format!("{SWEPT_IGNORED_HEAD}{name}{SWEPT_IGNORED_MIDDLE}.env.local, target/."))
        .collect();
    assert_eq!(notes(&case), noted);
}

/// A swept worktree that holds no file git ignores gets no note.
#[test]
fn a_swept_worktree_with_no_ignored_file_is_not_noted() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1], 0)
    );
    assert_eq!(notes(&case), Vec::<String>::new());
}

/// A worktree a check keeps as dirty stays with its ignored files, and no note is written.
#[test]
fn a_kept_worktree_holding_ignored_files_is_not_noted() {
    let case = Case::new();
    ignoring(&case, ".env.local\n");
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join(".env.local"), b"TOKEN=stand-in\n");
    write(&case.worktree(1).join("notes.txt"), b"never added\n");
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[], 0)
    );
    assert!(case.kept(1) && case.worktree(1).join(".env.local").is_file());
    assert_eq!(notes(&case), Vec::<String>::new());
}

/// More ignored entries than a note names are counted: the note names as many as fit in
/// its bound and says how many more went.
#[test]
fn a_swept_worktree_holding_many_ignored_files_names_some_and_counts_the_rest() {
    let case = Case::new();
    ignoring(&case, "*.log\n");
    case.agent(1, "sub", "stopped");
    let count = crate::landing::DIRTY_FILES_MAX + 6;
    for at in 0..count {
        write(&case.worktree(1).join(format!("f{at:03}.log")), b"log\n");
    }
    let head = format!(
        "{SWEPT_IGNORED_HEAD}{}{SWEPT_IGNORED_MIDDLE}",
        label(&case, 1)
    );
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1], 0)
    );
    let noted = notes(&case);
    assert_eq!(noted.len(), 1);
    assert!(noted[0].len() <= crate::team::NOTE_BYTES_MAX);
    let named = noted[0].strip_prefix(&head).unwrap();
    let (names, more) = named.rsplit_once(" and ").unwrap();
    let shown = names.split(", ").count();
    assert!(
        shown > 0 && names.starts_with("f000.log, f001.log"),
        "{names}"
    );
    assert_eq!(more, format!("{} more.", count - shown));
}

/// An ignored file named with a control character is shown with it replaced, so the note
/// never carries the raw byte.
#[test]
fn a_swept_ignored_name_with_a_control_character_is_shown_replaced() {
    let case = Case::new();
    ignoring(&case, "*.log\n");
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("bad\u{1}name.log"), b"log\n");
    let name = label(&case, 1);
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1], 0)
    );
    assert_eq!(
        notes(&case),
        [format!(
            "{SWEPT_IGNORED_HEAD}{name}{SWEPT_IGNORED_MIDDLE}bad\u{fffd}name.log."
        )]
    );
}
