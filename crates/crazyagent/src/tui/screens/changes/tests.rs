use super::*;
use crate::landing::{ChangeAgent, ChangeDiff};

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

fn project() -> ProjectId {
    ProjectId::new("project-1").unwrap()
}

fn id(digit: char) -> ChangeSetId {
    ChangeSetId::new(digit.to_string().repeat(32)).unwrap()
}

fn set(digit: char, state: ChangeState) -> ChangeSetView {
    ChangeSetView {
        id: id(digit),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-1".into(),
            label: "Agent 1".into(),
        },
        state,
        base: "b".repeat(40),
        head: digit.to_string().repeat(40),
        files: 1,
        added: 1,
        removed: 0,
        last_outcome: TurnOutcome::Succeeded,
        checked: false,
        created_ms: 1,
        updated_ms: 2,
        restorable_until_ms: None,
        conflicts: Vec::new(),
        message: "Add a file".into(),
        waiting: false,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn text(view: &ProjectChanges) -> String {
    let (rows, _, _) = view.rows(100, 40, NOW, &Look::plain());
    rows.iter().map(|row| row.text() + "\n").collect()
}

fn reply(view: &mut ProjectChanges, result: Result<ChangesView, String>) -> ChangesQuery {
    let query = view.next_query(NOW).expect("a request is due");
    view.apply(&ChangesEvent {
        query: query.clone(),
        result,
    });
    query
}

fn listed(sets: Vec<ChangeSetView>) -> ProjectChanges {
    let mut view = ProjectChanges::new(project());
    let query = reply(&mut view, Ok(ChangesView::List(sets)));
    assert_eq!(query, ChangesQuery::List { project: project() });
    view
}

#[test]
fn changes_unavailable_shows_not_ready_yet_and_stops_polling_until_the_tab_opens_again() {
    let mut view = ProjectChanges::new(project());
    reply(&mut view, Err("changes_unavailable".into()));
    let shown = text(&view);
    assert!(shown.contains("Not ready yet."), "{shown}");
    assert!(
        shown.contains("Landing changes isn't ready in this engine yet."),
        "{shown}"
    );
    assert!(!view.key(key(KeyCode::Char('a')), NOW));
    assert_eq!(view.next_query(NOW + 10 * LIST_POLL_MS), None);
    view.refresh();
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::List { project: project() })
    );
}

#[test]
fn a_pending_change_set_shows_its_agent_size_and_that_crazyagent_did_not_check_it() {
    let view = listed(vec![set('1', ChangeState::Pending)]);
    let shown = text(&view);
    for want in [
        "Sub-agent Agent 1",
        "Ready to review",
        "1 file, 1 line added, 0 removed",
        "Not checked by CrazyAgent. Any test results in it are reported by the agent.",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
    let (_, _, help) = view.rows(100, 40, NOW, &Look::plain());
    assert!(help.contains(&("a", "Accept")) && help.contains(&("d", "Discard")));
    assert!(!help.iter().any(|(key, _)| *key == "u" || *key == "r"));
}

#[test]
fn an_empty_project_says_where_agent_changes_will_appear() {
    let view = listed(Vec::new());
    assert!(text(&view).contains("No changes yet."));
}

#[test]
fn the_diff_shows_files_marks_and_plain_text_then_accept_lands_the_reviewed_head() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    assert!(view.key(key(KeyCode::Enter), NOW));
    let page = ChangeDiff {
        files: vec![ChangedFile {
            path: "run.sh".into(),
            old_path: None,
            status: FileStatus::Modified,
            kind: FileKind::Text,
            marks: vec![FileMark::Executable],
        }],
        text: "@@ -1 +1,2 @@\n echo one\n+echo\ttwo\n+\u{1b}[31mred\u{202e}".into(),
        next: None,
        truncated: true,
    };
    let query = reply(&mut view, Ok(ChangesView::Diff(page)));
    assert_eq!(
        query,
        ChangesQuery::Diff {
            id: id('1'),
            page: 0
        }
    );
    let shown = text(&view);
    for want in [
        "Modified run.sh",
        "! Can run as a program",
        "+echo    two",
        "Part of this diff is not shown",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{202e}'),
        "{shown}"
    );

    assert!(view.key(key(KeyCode::Char('a')), NOW));
    assert!(text(&view).contains("Accept the changes of Sub-agent Agent 1?"));
    assert!(view.key(key(KeyCode::Enter), NOW));
    let landed = AcceptOutcome::Landed {
        commit: "abcdef0123456789abcdef0123456789abcdef01".into(),
        branch: "main".into(),
    };
    let query = reply(&mut view, Ok(ChangesView::Accepted(landed)));
    assert_eq!(
        query,
        ChangesQuery::Accept {
            id: id('1'),
            expected_head: "1".repeat(40),
            message: "Add a file".into(),
        }
    );
    let shown = text(&view);
    assert!(shown.contains("Accepted as abcdef0 on main."), "{shown}");
    assert!(view.diff.is_none());
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::List { project: project() })
    );
}

#[test]
fn a_refused_accept_says_why_in_plain_words_and_reads_the_list_again() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    view.key(key(KeyCode::Char('a')), NOW);
    view.key(key(KeyCode::Esc), NOW);
    assert_eq!(view.next_query(NOW), None);
    view.key(key(KeyCode::Char('a')), NOW);
    view.key(key(KeyCode::Enter), NOW);
    reply(&mut view, Err("changes_owner_dirty".into()));
    assert!(text(&view).contains(
        "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again."
    ));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::List { project: project() })
    );
    for code in [
        "changes_owner_dirty",
        "changes_detached_head",
        "changes_git_busy",
        "changes_conflict",
        "changes_stale",
        "changes_branch_moved",
        "changes_git_unsupported",
        "changes_restore_expired",
        "changes_agent_has_pending",
        "changes_agent_busy",
        "changes_not_found",
        "changes_owner_identity_missing",
        "changes_message_invalid",
        "changes_unavailable",
    ] {
        assert!(
            crate::engine::owner::CHANGES_CODES.contains(&code),
            "{code}"
        );
        let sentence = refusal(code);
        assert!(!sentence.contains('_'), "{code} reads as {sentence}");
    }
}

#[test]
fn an_update_a_master_cannot_take_says_why_in_plain_words() {
    for (code, sentence) in [
        (
            "changes_master_tools_off",
            "This master has tools off, so it can't edit files to update these changes. Turn tools on for the master, or discard.",
        ),
        (
            "changes_master_closed",
            "The master conversation these changes came from is closed, so it can't update them. Discard them, or start a new conversation.",
        ),
        (
            "changes_master_not_started",
            "Your branch is merged into the master's copy, but the master couldn't start resolving it yet. It starts after the master's next turn ends.",
        ),
    ] {
        let mut conflict = set('1', ChangeState::Conflict);
        conflict.agent.kind = AgentKind::Master;
        let mut view = listed(vec![conflict]);
        assert!(view.key(key(KeyCode::Char('u')), NOW));
        let query = reply(&mut view, Err(code.into()));
        assert_eq!(query, ChangesQuery::Update { id: id('1') });
        assert!(
            crate::engine::owner::CHANGES_CODES.contains(&code),
            "{code}"
        );
        assert!(text(&view).contains(sentence), "{code}: {}", text(&view));
        assert_eq!(refusal(code), sentence);
    }
}

#[test]
fn an_update_the_team_refuses_says_why_in_the_desktop_words() {
    for (code, sentence) in [
        (
            "team_agent_not_found",
            "The agent that made these changes is no longer on the team, so it can't update them. Discard them instead.",
        ),
        (
            "team_conversation_full",
            "Too many of your messages are waiting for the master. Send more once it has answered.",
        ),
        ("team_task_invalid", "The task is empty or too long."),
    ] {
        let mut view = listed(vec![set('1', ChangeState::Conflict)]);
        assert!(view.key(key(KeyCode::Char('u')), NOW));
        let query = reply(&mut view, Err(code.into()));
        assert_eq!(query, ChangesQuery::Update { id: id('1') });
        assert!(
            crate::engine::owner::CHANGES_CODES.contains(&code),
            "{code}"
        );
        let shown = text(&view);
        assert!(shown.contains(sentence), "{code}: {shown}");
        assert!(!shown.contains("That didn't work"), "{code}: {shown}");
        assert_eq!(refusal(code), sentence);
    }
}

#[test]
fn a_conflicting_accept_names_the_files_and_offers_ask_agent_to_update() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    view.key(key(KeyCode::Char('a')), NOW);
    view.key(key(KeyCode::Enter), NOW);
    let files: Vec<String> = (1..=7).map(|n| format!("f{n}.txt")).collect();
    reply(
        &mut view,
        Ok(ChangesView::Accepted(AcceptOutcome::Conflict { files })),
    );
    assert!(text(&view).contains(
        "These files also changed on your branch: f1.txt, f2.txt, f3.txt, f4.txt, f5.txt and 2 more. Ask the agent to update, or discard."
    ));
    let mut conflict = set('1', ChangeState::Conflict);
    conflict.conflicts = vec!["README".into()];
    reply(&mut view, Ok(ChangesView::List(vec![conflict])));
    assert!(text(&view).contains("Conflicts with your branch"));
    assert!(view.key(key(KeyCode::Char('u')), NOW));
    let query = reply(&mut view, Ok(ChangesView::Updating));
    assert_eq!(query, ChangesQuery::Update { id: id('1') });
    assert!(text(&view).contains(
        "Your branch is merged into the agent's copy. The list shows when the agent picks it up."
    ));
}

#[test]
fn a_discarded_change_set_can_be_restored_only_inside_its_window() {
    let mut open = set('1', ChangeState::Discarded);
    open.restorable_until_ms = Some(u64::try_from(NOW + 3 * DAY).unwrap());
    let mut gone = set('2', ChangeState::Discarded);
    gone.restorable_until_ms = Some(u64::try_from(NOW - 1).unwrap());
    let mut view = listed(vec![open, gone]);
    assert!(text(&view).contains("You can restore these changes for the next 3 days."));
    assert!(!view.key(key(KeyCode::Char('a')), NOW));
    assert!(view.key(key(KeyCode::Char('r')), NOW));
    let query = reply(&mut view, Ok(ChangesView::Restored));
    assert_eq!(query, ChangesQuery::Restore { id: id('1') });
    assert!(text(&view).contains("Restored. The changes are ready to review again."));
    reply(&mut view, Ok(ChangesView::List(Vec::new())));
    let mut view = listed(vec![set('2', ChangeState::Discarded)]);
    view.key(key(KeyCode::Down), NOW);
    assert!(text(&view).contains("These changes can no longer be restored."));
    assert!(!view.key(key(KeyCode::Char('r')), NOW));
}

#[test]
fn an_action_waits_for_the_read_in_flight_and_a_lost_action_is_not_resent() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    let poll = view.next_query(NOW + LIST_POLL_MS).unwrap();
    view.key(key(KeyCode::Char('d')), NOW);
    view.key(key(KeyCode::Char('y')), NOW);
    assert!(view.busy());
    assert_eq!(view.next_query(NOW), None);
    view.apply(&ChangesEvent {
        query: ChangesQuery::Restore { id: id('9') },
        result: Ok(ChangesView::Restored),
    });
    view.apply(&ChangesEvent {
        query: poll,
        result: Ok(ChangesView::List(vec![set('1', ChangeState::Pending)])),
    });
    let discard = view.next_query(NOW).unwrap();
    assert_eq!(
        discard,
        ChangesQuery::Discard {
            id: id('1'),
            expected_head: "1".repeat(40)
        }
    );
    view.forget_reads();
    assert!(!view.busy());
    assert!(text(&view).contains("Check the list before trying again."));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::List { project: project() })
    );
}

#[test]
fn the_changes_tab_glyph_has_an_ascii_fallback() {
    let ascii = Look {
        ascii: true,
        ..Look::plain()
    };
    let glyph = super::super::project::Tab::Changes.glyph();
    assert_eq!(ascii.glyph(glyph), "~");
    assert_eq!(Look::plain().glyph(glyph), "\u{2213}");
}

#[test]
fn accept_opens_the_proposed_commit_message_for_editing_and_sends_the_edit() {
    let mut proposed = set('1', ChangeState::Pending);
    proposed.message = "Add a main\n\n- Add the fourth line".into();
    let mut view = listed(vec![proposed]);
    assert!(!view.editing());
    assert!(view.key(key(KeyCode::Char('a')), NOW));
    assert!(view.editing());
    let (rows, cursor, help) = view.rows(100, 40, NOW, &Look::plain());
    let shown: Vec<String> = rows.iter().map(|row| row.text()).collect();
    let line = shown
        .iter()
        .position(|row| row.trim_end() == "  - Add the fourth line")
        .expect("the proposed message is shown");
    assert_eq!(shown[line - 2].trim_end(), "  Add a main");
    assert_eq!(cursor, Some((2 + "- Add the fourth line".len(), line)));
    assert!(help.contains(&("Enter", "Accept")) && help.contains(&("Esc", "Keep reviewing")));

    view.key(
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        NOW,
    );
    assert!(text(&view).contains("Write a commit message to accept."));
    assert!(view.key(key(KeyCode::Enter), NOW));
    assert_eq!(view.next_query(NOW), None);
    for typed in "Owner title".chars() {
        assert!(view.key(key(KeyCode::Char(typed)), NOW));
    }
    view.key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        NOW,
    );
    view.key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        NOW,
    );
    assert!(view.paste("y n ? why\tit changed"));
    assert!(view.key(key(KeyCode::Enter), NOW));
    assert!(!view.editing());
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Accept {
            id: id('1'),
            expected_head: "1".repeat(40),
            message: "Owner title\n\ny n ? why it changed".into(),
        })
    );
    assert!(!view.paste("ignored"));
}

#[test]
fn an_answer_acts_on_the_change_set_it_was_asked_for_even_after_the_list_reorders() {
    let mut view = listed(vec![
        set('1', ChangeState::Pending),
        set('2', ChangeState::Pending),
    ]);
    assert!(view.key(key(KeyCode::Char('a')), NOW));
    let poll = view.next_query(NOW + LIST_POLL_MS).unwrap();
    view.apply(&ChangesEvent {
        query: poll,
        result: Ok(ChangesView::List(vec![
            set('2', ChangeState::Pending),
            set('1', ChangeState::Pending),
        ])),
    });
    assert!(view.editing());
    assert!(view.key(key(KeyCode::Enter), NOW));
    assert_eq!(
        view.next_query(NOW + LIST_POLL_MS),
        Some(ChangesQuery::Accept {
            id: id('1'),
            expected_head: "1".repeat(40),
            message: "Add a file".into(),
        })
    );
    let poll = ChangesQuery::List { project: project() };
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    view.key(key(KeyCode::Char('d')), NOW);
    assert_eq!(view.next_query(NOW + LIST_POLL_MS), Some(poll.clone()));
    view.apply(&ChangesEvent {
        query: poll,
        result: Ok(ChangesView::List(vec![set('2', ChangeState::Pending)])),
    });
    assert!(!view.key(key(KeyCode::Char('y')), NOW));
    assert_eq!(view.next_query(NOW), None);
}

#[test]
fn a_set_merged_but_not_handed_to_its_agent_says_so_and_can_be_asked_again_or_discarded() {
    let mut waiting = set('1', ChangeState::Updating);
    waiting.waiting = true;
    let mut view = listed(vec![waiting]);
    let shown = text(&view);
    for want in [
        "Updated, waiting for the agent",
        "Your branch is merged into the agent's copy, but the agent wasn't handed the work",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
    assert!(!shown.contains("Agent is updating"), "{shown}");
    let (_, _, help) = view.rows(100, 40, NOW, &Look::plain());
    assert!(help.contains(&("u", "Ask agent to update")) && help.contains(&("d", "Discard")));
    assert!(!help.iter().any(|(key, _)| *key == "a"));
    assert!(view.key(key(KeyCode::Char('u')), NOW));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Update { id: id('1') })
    );
    assert!(refusal("team_agent_not_found").contains("no longer on the team"));

    let resolving = listed(vec![set('1', ChangeState::Updating)]);
    let (_, _, help) = resolving.rows(100, 40, NOW, &Look::plain());
    assert!(!help.iter().any(|(key, _)| ["a", "d", "u"].contains(key)));
    assert!(text(&resolving).contains("Agent is updating"));
}

/// A change set whose agent label carries its digit, so the list rows tell sets apart.
/// Inputs: the digit and the state. Output: the change set.
fn named(digit: char, state: ChangeState) -> ChangeSetView {
    let mut set = set(digit, state);
    set.agent.label = format!("Agent {digit}");
    set
}

/// Answers the poll due after the interval with the sets in a new order.
/// Inputs: the view and the sets. Output: none.
fn reordered(view: &mut ProjectChanges, sets: Vec<ChangeSetView>) {
    let poll = view.next_query(NOW + LIST_POLL_MS).unwrap();
    assert_eq!(poll, ChangesQuery::List { project: project() });
    view.apply(&ChangesEvent {
        query: poll,
        result: Ok(ChangesView::List(sets)),
    });
}

#[test]
fn the_highlight_and_each_action_stay_on_the_chosen_set_when_a_read_reorders_the_list() {
    let mut view = listed(vec![
        named('1', ChangeState::Pending),
        named('2', ChangeState::Pending),
        named('3', ChangeState::Pending),
    ]);
    assert!(view.key(key(KeyCode::Down), NOW));
    assert!(text(&view).contains("> Sub-agent Agent 2"));
    reordered(
        &mut view,
        vec![
            named('3', ChangeState::Pending),
            named('1', ChangeState::Pending),
            named('2', ChangeState::Pending),
        ],
    );
    assert!(
        text(&view).contains("> Sub-agent Agent 2"),
        "the highlight follows the chosen set, not its old place"
    );
    assert!(view.key(key(KeyCode::Char('d')), NOW));
    assert!(view.key(key(KeyCode::Char('y')), NOW));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Discard {
            id: id('2'),
            expected_head: "2".repeat(40),
        })
    );

    let mut view = listed(vec![
        named('1', ChangeState::Pending),
        named('3', ChangeState::Conflict),
    ]);
    assert!(view.key(key(KeyCode::Down), NOW));
    reordered(
        &mut view,
        vec![
            named('3', ChangeState::Conflict),
            named('1', ChangeState::Pending),
        ],
    );
    assert!(text(&view).contains("> Sub-agent Agent 3"));
    assert!(view.key(key(KeyCode::Char('u')), NOW));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Update { id: id('3') })
    );

    let mut view = listed(vec![
        named('1', ChangeState::Pending),
        named('2', ChangeState::Pending),
    ]);
    assert!(view.key(key(KeyCode::Down), NOW));
    assert!(view.key(key(KeyCode::Char('d')), NOW));
    reordered(
        &mut view,
        vec![
            named('2', ChangeState::Pending),
            named('1', ChangeState::Pending),
        ],
    );
    assert!(
        text(&view).contains("> Sub-agent Agent 2"),
        "an open question keeps the highlight and details on its own set"
    );
    assert!(view.key(key(KeyCode::Char('y')), NOW));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Discard {
            id: id('2'),
            expected_head: "2".repeat(40),
        })
    );
}

/// The whole view text on a screen tall enough that no file row is cut. Input: the view.
/// Output: the text.
fn tall(view: &ProjectChanges) -> String {
    let (rows, _, _) = view.rows(100, 80, NOW, &Look::plain());
    rows.iter().map(|row| row.text() + "\n").collect()
}

/// One changed file of a diff page. Input: its path, status, kind and marks. Output: the file.
fn changed(path: &str, status: FileStatus, kind: FileKind, marks: Vec<FileMark>) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        old_path: None,
        status,
        kind,
        marks,
    }
}

/// Opens the diff of one pending set with these files. Input: the files. Output: the view.
fn diff_of(files: Vec<ChangedFile>) -> ProjectChanges {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    assert!(view.key(key(KeyCode::Enter), NOW));
    let page = ChangeDiff {
        files,
        text: "@@ -0,0 +1 @@\n+x".into(),
        next: None,
        truncated: true,
    };
    reply(&mut view, Ok(ChangesView::Diff(page)));
    view
}

#[test]
fn a_marked_file_past_the_preview_limit_is_named_with_its_marks_before_plain_files() {
    let mut files: Vec<ChangedFile> = (0..FILES_SHOWN)
        .map(|n| {
            changed(
                &format!(".github/workflow-{n:02}.yml"),
                FileStatus::Modified,
                FileKind::Text,
                Vec::new(),
            )
        })
        .collect();
    files.push(changed(
        "zz/hook.sh",
        FileStatus::Added,
        FileKind::Text,
        vec![FileMark::Executable],
    ));
    files.push(changed(
        "zz/link",
        FileStatus::Added,
        FileKind::Symlink,
        vec![FileMark::Symlink],
    ));
    let shown = tall(&diff_of(files));
    for want in [
        "Added zz/hook.sh",
        "! Can run as a program",
        "Added zz/link",
        "! Symbolic link: points to another path",
        "and 2 more files",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
    assert!(
        shown.find("zz/link").unwrap() < shown.find(".github/workflow-00.yml").unwrap(),
        "marked files come first\n{shown}"
    );
    assert!(!shown.contains("need checking"), "{shown}");

    let many: Vec<ChangedFile> = (0..FILES_SHOWN + 2)
        .map(|n| {
            changed(
                &format!("bin/tool-{n:02}"),
                FileStatus::Added,
                FileKind::Text,
                vec![FileMark::Executable],
            )
        })
        .chain([changed(
            "README.md",
            FileStatus::Modified,
            FileKind::Text,
            Vec::new(),
        )])
        .collect();
    let listed = listed_files(&many);
    assert_eq!(
        (
            listed.shown.len(),
            listed.marked_hidden,
            listed.plain_hidden
        ),
        (FILES_SHOWN, 2, 1)
    );
    let shown = tall(&diff_of(many));
    for want in [
        "2 more files need checking before you accept.",
        "and 1 more files",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
}

#[test]
fn a_set_that_leaves_the_list_takes_the_selection_with_it_and_no_key_acts_on_its_neighbour() {
    let mut view = listed(vec![
        named('1', ChangeState::Conflict),
        named('2', ChangeState::Conflict),
        named('3', ChangeState::Conflict),
    ]);
    assert!(view.key(key(KeyCode::Down), NOW));
    assert!(text(&view).contains("> Sub-agent Agent 2"));
    reordered(
        &mut view,
        vec![
            named('1', ChangeState::Conflict),
            named('3', ChangeState::Conflict),
        ],
    );
    let shown = text(&view);
    assert!(
        !shown.contains("> Sub-agent"),
        "no set takes over the highlight\n{shown}"
    );
    assert!(
        shown.contains("The change set you picked left the list. Pick one with"),
        "{shown}"
    );
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char('d'),
        KeyCode::Char('u'),
        KeyCode::Enter,
    ] {
        view.key(key(code), NOW);
        assert_eq!(view.asking, None, "{code:?} asks nothing");
        assert!(view.diff.is_none(), "{code:?} opens nothing");
        assert_eq!(view.next_query(NOW), None, "{code:?} sends nothing");
    }
    let (_, _, help) = view.rows(100, 40, NOW, &Look::plain());
    assert!(
        !help
            .iter()
            .any(|(key, _)| ["a", "d", "u", "r"].contains(key)),
        "{help:?}"
    );

    assert!(view.key(key(KeyCode::Down), NOW));
    assert!(
        text(&view).contains("> Sub-agent Agent 3"),
        "the first move picks the set now in that place"
    );
    assert!(view.key(key(KeyCode::Char('u')), NOW));
    assert_eq!(
        view.next_query(NOW),
        Some(ChangesQuery::Update { id: id('3') })
    );
}

#[test]
fn accept_and_discard_name_the_set_they_ask_about() {
    let mut view = listed(vec![
        named('1', ChangeState::Pending),
        named('2', ChangeState::Pending),
    ]);
    assert!(view.key(key(KeyCode::Down), NOW));
    assert!(view.key(key(KeyCode::Char('d')), NOW));
    let shown = text(&view);
    for want in [
        "Discard the changes of Sub-agent Agent 2?",
        "1 file, 1 line added, 0 removed, at 2222222.",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
    assert!(view.key(key(KeyCode::Char('n')), NOW));
    assert!(view.key(key(KeyCode::Char('a')), NOW));
    let shown = text(&view);
    for want in [
        "Accept the changes of Sub-agent Agent 2?",
        "1 file, 1 line added, 0 removed, at 2222222.",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
}

#[test]
fn marks_on_file_pages_past_the_text_limit_still_load_and_show() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    assert!(view.key(key(KeyCode::Enter), NOW));
    let padded: String = (0..DIFF_LINES_MAX + 5)
        .map(|n| format!("+line {n}\n"))
        .collect();
    let first = ChangeDiff {
        files: vec![changed(
            "notes.txt",
            FileStatus::Modified,
            FileKind::Text,
            Vec::new(),
        )],
        text: padded,
        next: Some(1),
        truncated: false,
    };
    reply(&mut view, Ok(ChangesView::Diff(first)));
    assert_eq!(view.diff.as_ref().unwrap().lines.len(), DIFF_LINES_MAX);
    let second = ChangeDiff {
        files: vec![changed(
            "zz/hook.sh",
            FileStatus::Added,
            FileKind::Text,
            vec![FileMark::Executable],
        )],
        text: "+more\n".into(),
        next: Some(2),
        truncated: false,
    };
    let query = reply(&mut view, Ok(ChangesView::Diff(second)));
    assert_eq!(
        query,
        ChangesQuery::Diff {
            id: id('1'),
            page: 1
        },
        "the file list keeps loading past the text limit"
    );
    let third = ChangeDiff {
        files: Vec::new(),
        text: "+last\n".into(),
        next: Some(3),
        truncated: false,
    };
    let query = reply(&mut view, Ok(ChangesView::Diff(third)));
    assert_eq!(
        query,
        ChangesQuery::Diff {
            id: id('1'),
            page: 2
        }
    );
    assert_eq!(
        view.next_query(NOW),
        None,
        "once every file is known, no text past the limit is read"
    );
    let diff = view.diff.as_ref().unwrap();
    assert_eq!(diff.lines.len(), DIFF_LINES_MAX);
    assert!(diff.truncated);
    let shown = tall(&view);
    for want in [
        "Added zz/hook.sh",
        "! Can run as a program",
        "Part of this diff is not shown",
    ] {
        assert!(shown.contains(want), "{want} missing in\n{shown}");
    }
}

#[test]
fn a_file_list_past_its_bound_is_cut_and_says_the_rest_is_not_shown() {
    let mut view = listed(vec![set('1', ChangeState::Pending)]);
    assert!(view.key(key(KeyCode::Enter), NOW));
    let files: Vec<ChangedFile> = (0..=FILES_MAX)
        .map(|n| {
            changed(
                &format!("gen/file-{n:05}.txt"),
                FileStatus::Added,
                FileKind::Text,
                Vec::new(),
            )
        })
        .collect();
    let page = ChangeDiff {
        files,
        text: "+x\n".into(),
        next: None,
        truncated: false,
    };
    reply(&mut view, Ok(ChangesView::Diff(page)));
    let diff = view.diff.as_ref().unwrap();
    assert_eq!(diff.files.len(), FILES_MAX);
    assert!(diff.files_cut && diff.files_done);
    assert_eq!(view.next_query(NOW), None);
    let shown = tall(&view);
    assert!(
        shown.contains(&format!(
            "Only the first {FILES_MAX} changed files are listed."
        )),
        "{shown}"
    );
}
