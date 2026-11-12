use super::*;

/// Shell prompt and list marks are cut from the front of a line, word by word, and nowhere
/// else; a word that only starts with a mark stays (F22).
#[test]
fn prompt_and_list_marks_are_cut_from_the_front_of_a_line_only() {
    assert_eq!(
        unprompted("\u{276F} uv run academy platform sync"),
        "uv run academy platform sync"
    );
    assert_eq!(unprompted("  $ make check"), "make check");
    assert_eq!(unprompted("> > - fix the parser"), "fix the parser");
    assert_eq!(unprompted("#123 fix the parser"), "#123 fix the parser");
    assert_eq!(unprompted("fix a > b in $HOME"), "fix a > b in $HOME");
    assert_eq!(unprompted("\u{276F}"), "");
    assert_eq!(unprompted(""), "");
}

/// A summary names a set by its first line of at least two words, with marks and local
/// paths cut, so a heading or a bare "Done." names nothing (F22).
#[test]
fn a_summary_subject_is_its_first_line_of_two_words_or_more() {
    let roots: [&Path; 0] = [];
    assert_eq!(
        summary_subject(
            "## Summary\n\n- Added two lines to README in /Users/ada/notes/x.md\nDone.",
            &roots
        ),
        "Added two lines to README in x.md"
    );
    assert_eq!(summary_subject("Done.\nSummary", &roots), "");
    assert_eq!(summary_subject("", &roots), "");
    let long = format!("Fixed {}", "word ".repeat(40));
    assert!(summary_subject(&long, &roots).chars().count() <= 72);
}

/// A summary goes first and every commit subject the proposed message held is listed under
/// it; without a summary the proposed message stays as it was (F22).
#[test]
fn a_summarized_message_lists_every_commit_subject_under_the_summary() {
    assert_eq!(
        summarized(
            "Added two lines",
            "Add the fifth line\n\n- Add the fourth line\n- and more commits"
        ),
        "Added two lines\n\n- Add the fifth line\n- Add the fourth line\n- and more commits"
    );
    assert_eq!(summarized("Added two lines", ""), "Added two lines");
    assert_eq!(
        summarized("", "Add the fifth line\n\n- Add the fourth line"),
        "Add the fifth line\n\n- Add the fourth line"
    );
}

/// OA a6a13c9: a set the agent made no commit for goes by the owner's pasted prompt line
/// without its shell prompt mark, and lands with that subject (F22).
#[test]
fn a_pasted_prompt_mark_is_cut_from_the_subject_of_a_set_without_agent_commits() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case
        .turn_with(
            TurnOutcome::Succeeded,
            "\u{276F} uv run academy platform sync",
        )
        .unwrap();
    assert_eq!(case.view(&id).message, "uv run academy platform sync");
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), "uv run academy platform sync");
}

/// The agent's own summary names its change set and the commit subjects are listed below
/// it, in the Changes view and in the landed commit; a later turn with no usable summary
/// clears it, so the set goes by its newest commit subject again (F22).
#[test]
fn an_agent_summary_names_the_set_and_lists_its_commit_subjects() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.edit("README", "one\ntwo\nthree\nfour\nfive\n");
    case.agent_commit("Add the fifth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let proposed = "Add the fifth line\n\n- Add the fourth line";
    assert_eq!(case.view(&id).message, proposed);

    record_summary(&case.db, &case.workspace, &id, "Done.").unwrap();
    assert_eq!(case.view(&id).message, proposed);

    let summary = format!(
        "## Summary\n\u{276F} Added two lines to README in {}/README\nAll checks written.",
        case.workspace.worktree.display()
    );
    record_summary(&case.db, &case.workspace, &id, &summary).unwrap();
    let message =
        "Added two lines to README in README\n\n- Add the fifth line\n- Add the fourth line";
    assert_eq!(case.view(&id).message, message);
    assert_eq!(
        store::load(&case.db, &id).unwrap().summary,
        "Added two lines to README in README"
    );
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), message);
}

/// OA CS#035688c8: what landing did is kept apart from how the agent's turn went. A set whose
/// turn failed reads refused after an Accept refused for a dirty file it changes, then landed
/// once it lands, while its turn still reads failed, and both lines say so (F23).
#[test]
fn the_landing_result_is_kept_apart_from_the_turn_result() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Failed).unwrap();
    assert_eq!(case.view(&id).landing_result, LandingResult::None);
    write(&case.repo.join("README"), "one\ntwo\nthree\nowner\n");
    assert_eq!(refused(case.accept(&id)), "changes_owner_dirty");
    let view = case.view(&id);
    assert_eq!(view.landing_result, LandingResult::Refused);
    assert_eq!(
        turn_line(&view),
        Some("The agent's last turn failed. These are the changes it made before it stopped.")
    );
    assert_eq!(
        landing_line(&view),
        Some("Landing: the last Accept was refused, so nothing landed yet.")
    );
    case.owner(&["checkout", "--", "README"]);
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let view = case.view(&id);
    assert_eq!(view.last_outcome, TurnOutcome::Failed);
    assert_eq!(view.landing_result, LandingResult::Landed);
    assert_eq!(
        turn_line(&view),
        Some("Last turn: failed. Landing: landed. How the turn ended does not change what landed.")
    );
    assert_eq!(landing_line(&view), None);
    assert_eq!(
        store::load(&case.db, &id).unwrap().landing_result,
        LandingResult::Landed
    );
}

/// An Accept that conflicts keeps conflict as the landing result, and a stale head that
/// tried nothing leaves it as it was (F23).
#[test]
fn a_conflicting_accept_keeps_conflict_and_a_stale_one_changes_nothing() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nthree\nmine\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    assert_eq!(case.view(&id).landing_result, LandingResult::Conflict);
    let stale = accept(
        &case.db,
        &case.home,
        &id,
        &"0".repeat(40),
        None,
        crate::activity::AcceptPlace::Changes,
    );
    assert_eq!(refused(stale), "changes_stale");
    assert_eq!(case.view(&id).landing_result, LandingResult::Conflict);
}

/// F24: a removed project's open change set is left alone by every poll, so a trial project
/// the owner removed stops failing on its sets, and once the project is back a poll judges
/// the set again and closes it as landed outside the app.
#[test]
fn a_poll_leaves_the_sets_of_a_removed_project_alone() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY, body TEXT NOT NULL);
             INSERT INTO projects(id, body) VALUES ('another', '{}');",
        )
        .unwrap();
    case.owner(&[
        "merge",
        "--no-ff",
        "-q",
        "-m",
        "Merge the agent work",
        &head,
    ]);
    observe(&case.db, None).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    case.db
        .execute(
            "INSERT INTO projects(id, body) VALUES (?1, '{}')",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    observe(&case.db, None).unwrap();
    case.landed_outside(&id, &head);
}

/// F24: the same log line is written once per engine run, however often a poll meets it.
#[test]
fn a_line_logged_once_is_kept_once() {
    let line = format!(
        "landing_lead_gone project=polish-{} lead=1 worker=2",
        NOW_MS
    );
    log_once(&line);
    log_once(&line);
    let kept = LOGGED_ONCE.lock().unwrap();
    assert_eq!(kept.iter().filter(|kept| **kept == line).count(), 1);
    assert!(kept.len() <= LOGGED_ONCE_MAX);
}

/// F21: an owner change to a file the change set does not touch no longer blocks its Accept;
/// the set lands and the owner's change stays in the checkout, uncommitted, and nothing is
/// listed as in the way.
#[test]
fn an_owner_change_to_a_file_the_set_does_not_touch_lands_and_stays_uncommitted() {
    let case = Case::new();
    case.owner_commit("notes.txt", "base\n", "Notes");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join("notes.txt"), "owner edit\n");
    write(&case.repo.join("scratch.txt"), "mine\n");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap(),
        OwnerDirty::default()
    );
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.tip(), commit);
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("notes.txt")).unwrap(),
        "owner edit\n"
    );
    assert_eq!(
        case.owner(&["status", "--porcelain"]),
        " M notes.txt\n?? scratch.txt\n"
    );
    assert_eq!(case.view(&id).state, ChangeState::Accepted);
}

/// F21: an owner change to a file the change set changes is listed, committed first with the
/// owner's message and nothing else, and the next Accept lands on top of it.
#[test]
fn an_owner_change_to_a_file_the_set_changes_is_listed_and_committed_first() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join("README"), "zero\none\ntwo\nthree\n");
    write(&case.repo.join("scratch.txt"), "mine\n");
    let before = case.tip();
    assert_eq!(refused(case.accept(&id)), "changes_owner_dirty");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap(),
        OwnerDirty {
            files: vec!["README".to_owned()],
            untracked: Vec::new(),
        }
    );

    let (commit, branch) = commit_owner_files(
        &case.db,
        &case.home,
        &id,
        &["README".to_owned()],
        "Add the top line",
        None,
    )
    .unwrap();
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%s", &commit]).trim(),
        format!("{before}|Add the top line")
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "?? scratch.txt\n");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap(),
        OwnerDirty::default()
    );

    let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &landed]).trim(),
        commit
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "zero\none\ntwo\nthree\nfour\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "?? scratch.txt\n");
}

/// F21: anything staged in the owner's index still blocks an Accept, because the move back
/// after a failed checkout step needs a clean index; the staged file is listed and can be
/// committed first like any other.
#[test]
fn a_staged_owner_file_blocks_the_accept_until_it_is_committed_first() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join("staged.txt"), "staged\n");
    case.owner(&["add", "staged.txt"]);
    assert_eq!(refused(case.accept(&id)), "changes_owner_dirty");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap().files,
        ["staged.txt"]
    );
    commit_owner_files(
        &case.db,
        &case.home,
        &id,
        &["staged.txt".to_owned()],
        "Add the staged file",
        None,
    )
    .unwrap();
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
}

/// F21: a commit first names only files the engine itself finds in the way, with a usable
/// message; anything else is refused and commits nothing.
#[test]
fn a_commit_first_of_a_file_not_in_the_way_or_without_a_message_commits_nothing() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join("README"), "zero\none\ntwo\nthree\n");
    write(&case.repo.join("scratch.txt"), "mine\n");
    let before = case.tip();
    let commit = |files: &[&str], message: &str| {
        let files: Vec<String> = files.iter().map(|file| (*file).to_owned()).collect();
        refused(commit_owner_files(
            &case.db, &case.home, &id, &files, message, None,
        ))
    };
    assert_eq!(commit(&["scratch.txt"], "Add"), "changes_stale");
    assert_eq!(commit(&["README", "scratch.txt"], "Add"), "changes_stale");
    assert_eq!(commit(&["README"], "  \n"), "changes_message_invalid");
    assert_eq!(commit(&[], "Add"), "changes_request_invalid");
    assert_eq!(case.tip(), before);
    assert_eq!(
        case.owner(&["status", "--porcelain"]),
        " M README\n?? scratch.txt\n"
    );
}

/// S3 r1 F1: a file the owner staged in part is committed as staged, not as the working copy;
/// the owner's index keeps the staged version and the unstaged lines stay unstaged.
#[test]
fn a_partly_staged_file_is_committed_as_staged_and_keeps_its_unstaged_lines() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join("README"), "zero\none\ntwo\nthree\n");
    case.owner(&["add", "README"]);
    write(
        &case.repo.join("README"),
        "zero\none\ntwo\nthree\ndebug on\n",
    );
    assert_eq!(owner_dirty(&case.db, &id, None).unwrap().files, ["README"]);
    let (commit, _) = commit_owner_files(
        &case.db,
        &case.home,
        &id,
        &["README".to_owned()],
        "Add the top line",
        None,
    )
    .unwrap();
    assert_eq!(
        case.owner(&["show", &format!("{commit}:README")]),
        "zero\none\ntwo\nthree\n"
    );
    assert_eq!(case.owner(&["show", ":README"]), "zero\none\ntwo\nthree\n");
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "zero\none\ntwo\nthree\ndebug on\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), " M README\n");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap().files,
        ["README"],
        "the unstaged lines are still in the way, for the owner to decide"
    );
}

/// S3 r1 F1: a file the owner stopped tracking with git rm --cached is committed as removed,
/// never added back; it stays on disk, untracked, and the Accept then lands.
#[test]
fn a_file_untracked_with_rm_cached_is_committed_as_removed_and_never_added_back() {
    let case = Case::new();
    case.owner_commit("notes.txt", "secret\n", "Notes");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner(&["rm", "-q", "--cached", "notes.txt"]);
    assert_eq!(refused(case.accept(&id)), "changes_owner_dirty");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap().files,
        ["notes.txt"]
    );
    let (commit, _) = commit_owner_files(
        &case.db,
        &case.home,
        &id,
        &["notes.txt".to_owned()],
        "Stop tracking the notes",
        None,
    )
    .unwrap();
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\n"
    );
    assert_eq!(case.owner(&["ls-files"]), "README\n");
    assert_eq!(
        std::fs::read_to_string(case.repo.join("notes.txt")).unwrap(),
        "secret\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "?? notes.txt\n");
    let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &landed]),
        "README\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "?? notes.txt\n");
}

/// S3 r1 security M1: an untracked owner file where the set adds one, such as a .env with
/// secrets, is listed apart and never committed: commit-first refuses it and nothing moves.
#[test]
fn an_untracked_owner_file_where_the_set_adds_one_is_never_committed() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.edit(".env", "AGENT=1\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&case.repo.join(".env"), "SECRET=owner\n");
    let before = case.tip();
    assert_eq!(refused(case.accept(&id)), "changes_owner_dirty");
    assert_eq!(
        owner_dirty(&case.db, &id, None).unwrap(),
        OwnerDirty {
            files: Vec::new(),
            untracked: vec![".env".to_owned()],
        }
    );
    let commit = commit_owner_files(
        &case.db,
        &case.home,
        &id,
        &[".env".to_owned()],
        "Save my changes",
        None,
    );
    assert_eq!(refused(commit), "changes_owner_untracked");
    assert_eq!(case.tip(), before);
    assert_eq!(case.owner(&["ls-files"]), "README\n");
    assert_eq!(
        std::fs::read_to_string(case.repo.join(".env")).unwrap(),
        "SECRET=owner\n"
    );
    std::fs::remove_file(case.repo.join(".env")).unwrap();
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
}
