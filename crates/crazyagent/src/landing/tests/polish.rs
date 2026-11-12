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
