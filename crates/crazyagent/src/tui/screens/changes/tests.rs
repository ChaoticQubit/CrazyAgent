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
    assert!(text(&view).contains("Accept these changes?"));
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
