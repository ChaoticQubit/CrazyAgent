use super::*;

const NOW: i64 = 10 * DAY_MS + 39_600_000;

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn session(
    id: &str,
    live: bool,
    started_ms: Option<i64>,
    line: &str,
    agents: &[&str],
    waiting: u32,
) -> SessionRow {
    SessionRow {
        id: id.into(),
        live,
        started_ms,
        line: line.into(),
        agents: agents.iter().map(|name| (*name).to_owned()).collect(),
        waiting,
        count: 0,
        ran: None,
        first: None,
    }
}

/// One stored message for the rows about an opened session. Inputs: its sequence number,
/// who said it, the master mode, the stored harness and its text, if any. Output: the row.
fn stored(
    seq: u64,
    speaker: MessageSpeaker,
    mode: MasterMode,
    harness: &str,
    text: Option<&str>,
) -> SessionMessage {
    SessionMessage {
        seq,
        speaker,
        mode,
        harness: Some(harness.into()),
        text: text.map(str::to_owned),
        at_ms: 1_000,
    }
}

fn listed() -> Vec<SessionRow> {
    vec![
        session(
            "s3",
            true,
            Some(10 * DAY_MS + 38_280_000),
            "Plan the release notes page",
            &["ada"],
            0,
        ),
        session(
            "s2",
            false,
            Some(NOW - 26 * 3_600_000),
            "Fix the login redirect\nThen add a test.",
            &["ada", "lin", "kai"],
            2,
        ),
        session("s1", false, None, "", &[], 0),
    ]
}

fn opened() -> Sessions {
    let mut sessions = Sessions::default();
    assert_eq!(
        sessions.key(press(KeyCode::Char('l')), true, false),
        SessionsReply::Handled
    );
    sessions.set(listed());
    sessions
}

fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

#[test]
fn the_list_and_header_draw_every_session_at_80_and_120_columns() {
    let look = Look::plain();
    let sessions = opened();
    for (width, header, rows) in [
        (
            76,
            "Session  started 10:38   2 earlier               2 results waiting elsewhere",
            [
                "> Live    10:38         Plan the release notes p\u{2026}  ada",
                "  Parked  26 h ago      Fix the login redirect     ada, lin, kai   2 waiting",
                "  Parked  earlier       No messages yet            No agents",
            ],
        ),
        (
            116,
            "Session  started 10:38   2 earlier                                                       2 results waiting elsewhere",
            [
                "> Live    10:38         Plan the release notes page                              ada",
                "  Parked  26 h ago      Fix the login redirect                                   ada, lin, kai             2 waiting",
                "  Parked  earlier       No messages yet                                          No agents",
            ],
        ),
    ] {
        let head = sessions.header(width, NOW, 0, &look).unwrap();
        assert_eq!(head.width(), width);
        assert_eq!(head.text().trim_end(), header);
        let (body, help) = sessions.view(width, 12, NOW, 0, &look).unwrap();
        assert!(body.iter().all(|row| row.width() <= width));
        let drawn = texts(&body);
        assert_eq!(drawn[0], "Sessions  3");
        assert_eq!(drawn[1], "");
        assert_eq!(&drawn[2..5], rows);
        assert_eq!(
            &drawn[5..],
            ["", "  No messages yet"],
            "the chosen session's count shows under the list"
        );
        assert_eq!(
            help,
            vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Enter", "Resume"),
                ("m", "Messages"),
                ("d", "Delete"),
                ("n", "New session"),
                ("Esc", "Back")
            ]
        );
    }
    let mut alone = Sessions::default();
    alone.set(vec![session(
        "s3",
        true,
        Some(NOW - 3 * DAY_MS),
        "Only one",
        &[],
        0,
    )]);
    assert_eq!(
        alone.header(76, NOW, 0, &look).unwrap().text().trim_end(),
        "Session  started 3 days ago"
    );
    assert!(
        Sessions::default().header(76, NOW, 0, &look).is_none(),
        "no row before the engine lists one"
    );
    assert_eq!(
        Sessions::conversation_help(),
        vec![("n", "New session"), ("l", "Sessions")]
    );
}

#[test]
fn n_starts_a_new_session_and_asks_first_while_a_turn_runs() {
    let mut sessions = opened();
    sessions.key(press(KeyCode::Esc), true, false);
    assert!(!sessions.showing());
    assert_eq!(
        sessions.key(press(KeyCode::Char('n')), true, false),
        SessionsReply::Act(SessionsAction::New {
            cancel_running: false
        })
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('n')), true, true),
        SessionsReply::Handled
    );
    assert!(sessions.showing());
    let (body, help) = sessions.view(76, 12, NOW, 0, &Look::plain()).unwrap();
    assert_eq!(
        texts(&body),
        ["A turn is running. Stop it and start a new session?"]
    );
    assert_eq!(
        help,
        vec![("y", "Stop the turn and switch"), ("n", "Keep it running")]
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('l')), true, true),
        SessionsReply::Handled,
        "the question keeps every key"
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('y')), true, true),
        SessionsReply::Act(SessionsAction::New {
            cancel_running: true
        })
    );
    assert!(!sessions.showing());
    sessions.key(press(KeyCode::Char('n')), true, true);
    assert_eq!(
        sessions.key(press(KeyCode::Char('n')), true, true),
        SessionsReply::Handled
    );
    assert!(!sessions.showing(), "n keeps the turn running");
    assert_eq!(
        sessions.key(press(KeyCode::Char('n')), false, false),
        SessionsReply::Ignored,
        "not before the master is ready"
    );
    assert_eq!(
        sessions.key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            true,
            false
        ),
        SessionsReply::Ignored
    );
}

#[test]
fn enter_resumes_the_chosen_session_and_asks_first_while_a_turn_runs() {
    let mut sessions = opened();
    assert_eq!(
        sessions.key(press(KeyCode::Enter), true, false),
        SessionsReply::Handled
    );
    assert!(
        !sessions.showing(),
        "Enter on the live session goes back to it"
    );

    sessions.key(press(KeyCode::Char('l')), true, false);
    sessions.key(press(KeyCode::Down), true, false);
    assert_eq!(
        sessions.key(press(KeyCode::Enter), true, false),
        SessionsReply::Act(SessionsAction::Resume {
            session: "s2".into(),
            cancel_running: false
        })
    );
    assert!(!sessions.showing());

    sessions.key(press(KeyCode::Char('l')), true, true);
    assert_eq!(
        sessions.key(press(KeyCode::Enter), true, true),
        SessionsReply::Handled
    );
    let (body, help) = sessions.view(76, 8, NOW, 0, &Look::plain()).unwrap();
    let drawn = texts(&body);
    assert!(body.len() <= 8);
    assert_eq!(
        drawn.last().unwrap(),
        "A turn is running. Stop it and resume the session from 26 h ago?"
    );
    assert!(
        drawn
            .iter()
            .any(|line| line.starts_with("> Parked  26 h ago")),
        "{drawn:?}"
    );
    assert_eq!(help[0], ("y", "Stop the turn and switch"));
    assert_eq!(
        sessions.key(press(KeyCode::Esc), true, true),
        SessionsReply::Handled
    );
    assert!(
        sessions.showing(),
        "Esc keeps the list open after the question"
    );
    sessions.key(press(KeyCode::Enter), true, true);
    assert_eq!(
        sessions.key(press(KeyCode::Char('y')), true, true),
        SessionsReply::Act(SessionsAction::Resume {
            session: "s2".into(),
            cancel_running: true
        })
    );
    assert!(!sessions.showing());
}

#[test]
fn the_list_waits_for_its_read_follows_its_cursor_and_keeps_every_key() {
    let look = Look::plain();
    let mut sessions = Sessions::default();
    sessions.key(press(KeyCode::Char('l')), true, false);
    assert_eq!(
        texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0),
        ["Reading sessions..."]
    );
    assert_eq!(
        sessions.key(press(KeyCode::Enter), true, false),
        SessionsReply::Handled,
        "nothing to resume yet"
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('e')), true, false),
        SessionsReply::Handled,
        "the open list keeps the conversation's keys"
    );

    sessions.set(listed());
    sessions.key(press(KeyCode::Down), true, false);
    sessions.key(press(KeyCode::Down), true, false);
    sessions.key(press(KeyCode::Down), true, false);
    let mut moved = listed();
    moved.rotate_left(1);
    sessions.set(moved);
    let drawn = texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0);
    assert!(
        drawn[3].starts_with("> Parked  earlier"),
        "the cursor stays on its session: {drawn:?}"
    );
    assert_eq!(drawn.len(), 7, "three sessions and the chosen one's detail");

    let many: Vec<SessionRow> = (0..50)
        .map(|index| {
            session(
                &format!("s{index}"),
                index == 0,
                Some(NOW),
                &format!("Session {index}"),
                &[],
                0,
            )
        })
        .collect();
    sessions.set(many);
    for _ in 0..60 {
        sessions.key(press(KeyCode::Down), true, false);
    }
    let drawn = texts(&sessions.view(76, 8, NOW, 0, &look).unwrap().0);
    assert_eq!(drawn.len(), 8);
    assert!(
        drawn[5].starts_with("> Parked  11:00         Session 49"),
        "{drawn:?}"
    );
    assert_eq!(&drawn[6..], ["", "  No messages yet"]);
    sessions.set(Vec::new());
    assert_eq!(
        texts(&sessions.view(76, 8, NOW, 0, &look).unwrap().0)[2],
        "No sessions yet."
    );
}

#[test]
fn a_switch_the_engine_refused_for_a_running_turn_asks_the_owner_again() {
    let mut sessions = opened();
    let refused = MasterOwnerAction::Resume {
        project_id: crate::protocol::ProjectId::new("notes-cli").unwrap(),
        session_id: crate::protocol::SessionId::new("s2").unwrap(),
        cancel_running: false,
    };
    assert!(sessions.turn_was_running(&refused));
    let (body, _) = sessions.view(76, 12, NOW, 0, &Look::plain()).unwrap();
    assert_eq!(
        texts(&body).last().unwrap(),
        "A turn is running. Stop it and resume the session from 26 h ago?"
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('y')), true, true),
        SessionsReply::Act(SessionsAction::Resume {
            session: "s2".into(),
            cancel_running: true
        })
    );
}

#[test]
fn d_asks_before_deleting_a_session_that_is_not_live_and_n_keeps_it() {
    let content = 80 - 4;
    let mut sessions = opened();
    assert_eq!(
        sessions.key(press(KeyCode::Char('d')), true, false),
        SessionsReply::Handled
    );
    let (body, _) = sessions.view(content, 12, NOW, 0, &Look::plain()).unwrap();
    assert!(
        !texts(&body)
            .iter()
            .any(|line| line.starts_with("Delete the session")),
        "the live session is never offered for Delete"
    );
    sessions.key(press(KeyCode::Down), true, false);
    assert_eq!(
        sessions.key(press(KeyCode::Char('d')), true, false),
        SessionsReply::Handled
    );
    let (body, help) = sessions.view(content, 12, NOW, 0, &Look::plain()).unwrap();
    assert!(body.iter().all(|row| row.width() <= content));
    let drawn = texts(&body);
    let asked = drawn
        .iter()
        .position(|line| line.starts_with("Delete the session"))
        .unwrap_or_else(|| panic!("{drawn:?}"));
    assert!(
        asked + 1 < drawn.len(),
        "the question wraps in the 76 cells an 80-column master view gives it"
    );
    assert_eq!(
        drawn[asked..].join(" "),
        "Delete the session from 26 h ago? Its messages and its transcript are removed for good.",
        "the whole question is drawn"
    );
    assert_eq!(help, vec![("y", "Delete it"), ("n", "Keep it")]);
    assert_eq!(
        sessions.key(press(KeyCode::Char('n')), true, false),
        SessionsReply::Handled
    );
    let (body, _) = sessions.view(content, 12, NOW, 0, &Look::plain()).unwrap();
    assert!(
        !texts(&body)
            .iter()
            .any(|line| line.starts_with("Delete the session")),
        "n keeps the session"
    );
    assert!(sessions.showing(), "the list stays open after n");

    sessions.key(press(KeyCode::Char('d')), true, false);
    assert_eq!(
        sessions.key(press(KeyCode::Char('y')), true, false),
        SessionsReply::Act(SessionsAction::Delete {
            session: "s2".into()
        })
    );
}

#[test]
fn the_chosen_session_shows_its_count_the_master_that_ran_it_last_and_its_first_and_newest_line() {
    use crate::protocol::{Cursor, SessionId};
    let summary = |id: &str, phase, turns, first: Option<&str>, last: Option<&str>, ran| {
        let (mode, harness): (Option<MasterMode>, Option<&str>) = ran;
        MasterSessionSummary {
            session_id: SessionId::new(id).unwrap(),
            phase,
            created: Cursor::new(3),
            last_activity: Cursor::new(5),
            started_ms: None,
            active_ms: None,
            turns,
            first_line: first.map(str::to_owned),
            last_line: last.map(str::to_owned),
            agents: Vec::new(),
            results_waiting: 0,
            mode,
            harness: harness.map(str::to_owned),
        }
    };
    let rows = rows_of(&[
        summary(
            "live",
            MasterSessionPhase::Configured,
            15,
            Some("Plan the launch"),
            Some("Launch shipped"),
            (Some(MasterMode::Live), Some("claude-code")),
        ),
        summary(
            "old",
            MasterSessionPhase::Parked,
            1,
            Some("Fix the login"),
            Some("Fix the login"),
            (Some(MasterMode::OneTurn), Some("rcli")),
        ),
        summary(
            "new",
            MasterSessionPhase::Parked,
            0,
            None,
            None,
            (None, None),
        ),
    ]);
    assert_eq!(
        (
            rows[0].count,
            rows[0].ran.as_deref(),
            rows[0].first.as_deref()
        ),
        (15, Some("Claude Code"), Some("Plan the launch"))
    );
    assert_eq!(rows[0].line, "Launch shipped");
    assert_eq!(
        (
            rows[1].count,
            rows[1].ran.as_deref(),
            rows[1].first.as_deref()
        ),
        (1, Some("One-turn master (RCLI)"), None),
        "one line is shown once"
    );
    assert_eq!((rows[2].count, rows[2].ran.as_deref()), (0, None));

    let look = Look::plain();
    let mut sessions = Sessions::default();
    sessions.key(press(KeyCode::Char('l')), true, false);
    sessions.set(rows);
    let drawn = texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0);
    assert_eq!(
        &drawn[5..],
        [
            "",
            "  15 messages, last run by Claude Code",
            "  First: Plan the launch",
            "  Last: Launch shipped",
        ]
    );
    sessions.key(press(KeyCode::Down), true, false);
    let drawn = texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0);
    assert_eq!(
        &drawn[5..],
        ["", "  1 message, last run by One-turn master (RCLI)"]
    );
    let tight = texts(&sessions.view(76, 4, NOW, 0, &look).unwrap().0);
    assert_eq!(
        tight.len(),
        4,
        "a short list keeps its rows for the sessions"
    );
    assert!(tight[2].starts_with("  Live"), "{tight:?}");

    assert_eq!(ran_by(MasterMode::Live, Some("codex")), "Codex");
    assert_eq!(ran_by(MasterMode::Live, Some("claude")), "Claude Code");
    assert_eq!(ran_by(MasterMode::Live, None), "Live master");
    assert_eq!(
        ran_by(MasterMode::OneTurn, Some("claude")),
        "One-turn master (Claude)"
    );
    assert_eq!(
        ran_by(MasterMode::OneTurn, Some("codex")),
        "One-turn master (Codex)"
    );
    assert_eq!(ran_by(MasterMode::OneTurn, None), "One-turn master");
}

#[test]
fn m_opens_the_chosen_session_s_stored_messages_and_esc_goes_back_to_the_list() {
    use MasterMode::{Live, OneTurn};
    use MessageSpeaker::{Engine, Master, Owner, Switch};
    let look = Look::plain();
    let mut sessions = opened();
    assert_eq!(sessions.reading(), None);
    sessions.key(press(KeyCode::Down), true, false);
    assert_eq!(
        sessions.key(press(KeyCode::Char('m')), true, false),
        SessionsReply::Handled
    );
    assert_eq!(
        sessions.reading(),
        Some("s2"),
        "the host reads this session"
    );
    assert!(sessions.showing());
    let (body, help) = sessions.view(76, 12, NOW, 0, &look).unwrap();
    assert_eq!(
        texts(&body),
        [
            "Messages  session from 26 h ago, No messages yet",
            "",
            "Reading the messages..."
        ]
    );
    assert_eq!(help, vec![("\u{2191}\u{2193}", "Scroll"), ("Esc", "Back")]);

    let switched = "Switched to Claude Code. The conversation continues.";
    let rows = [
        stored(1, Owner, OneTurn, "rcli", Some("Plan the launch.")),
        stored(2, Master, OneTurn, "rcli", Some("Launch planned.")),
        stored(3, Switch, Live, "claude-code", Some(switched)),
        stored(4, Owner, Live, "claude-code", None),
        stored(5, Master, Live, "claude-code", Some("Launch shipped.")),
        stored(6, Engine, Live, "claude-code", Some("A result came in.")),
        stored(7, Master, Live, "claude-code", None),
    ];
    sessions.set_messages(Some((&rows[..], true)));
    let (body, _) = sessions.view(76, 20, NOW, 0, &look).unwrap();
    assert!(body.iter().all(|row| row.width() <= 76));
    assert_eq!(
        &texts(&body)[2..],
        [
            "You",
            "  Plan the launch.",
            "One-turn master (RCLI)",
            "  Launch planned.",
            switched,
            TYPED_IN_TERMINAL,
            "Claude Code",
            "  Launch shipped.",
            "CrazyAgent",
            "  A result came in.",
        ]
    );

    let (short, _) = sessions.view(76, 5, NOW, 0, &look).unwrap();
    assert_eq!(
        &texts(&short)[2..],
        ["  Launch shipped.", "CrazyAgent", "  A result came in."],
        "the newest messages show first"
    );
    sessions.key(press(KeyCode::Up), true, false);
    sessions.key(press(KeyCode::Up), true, false);
    let (up, _) = sessions.view(76, 5, NOW, 0, &look).unwrap();
    assert_eq!(
        &texts(&up)[2..],
        [TYPED_IN_TERMINAL, "Claude Code", "  Launch shipped."]
    );
    for _ in 0..40 {
        sessions.key(press(KeyCode::PageUp), true, false);
    }
    let (top, _) = sessions.view(76, 5, NOW, 0, &look).unwrap();
    assert_eq!(
        &texts(&top)[2..],
        ["You", "  Plan the launch.", "One-turn master (RCLI)"],
        "scrolling stops at the first message"
    );

    sessions.set_messages(Some((&rows[5..], false)));
    let (part, _) = sessions.view(76, 20, NOW, 0, &look).unwrap();
    assert_eq!(texts(&part)[2], "Reading earlier messages...");
    sessions.set_messages(Some((&rows[..0], true)));
    let (none, _) = sessions.view(76, 20, NOW, 0, &look).unwrap();
    assert_eq!(texts(&none)[2], "No stored messages in this session.");

    assert_eq!(
        sessions.key(press(KeyCode::Char('d')), true, false),
        SessionsReply::Handled,
        "no list key acts while the messages show"
    );
    assert_eq!(
        sessions.key(press(KeyCode::Esc), true, false),
        SessionsReply::Handled
    );
    assert_eq!(sessions.reading(), None);
    let (list, _) = sessions.view(76, 12, NOW, 0, &look).unwrap();
    assert_eq!(texts(&list)[0], "Sessions  3", "Esc goes back to the list");

    sessions.key(press(KeyCode::Char('m')), true, false);
    assert_eq!(sessions.reading(), Some("s2"));
    sessions.set(vec![session("s3", true, Some(NOW), "Only one", &[], 0)]);
    assert_eq!(
        sessions.reading(),
        None,
        "a session the engine no longer lists is closed"
    );
}

#[test]
fn an_opened_session_draws_stored_text_without_terminal_escapes_and_inside_its_width() {
    use MasterMode::Live;
    use MessageSpeaker::{Master, Owner, Switch};
    let look = Look::plain();
    let mut sessions = opened();
    sessions.key(press(KeyCode::Down), true, false);
    sessions.key(press(KeyCode::Char('m')), true, false);
    let hostile = "\u{1b}[2J\u{1b}[31mRed\u{1b}[0m\u{7}\u{0} text\r\n\u{1b}]52;c;cGF5bG9hZA==\u{7}clip\u{9b}31m\u{202e}evil\ttab";
    let hidden = "a\u{200b}".repeat(60);
    let rows = [
        stored(1, Owner, Live, "claude-code", Some(hostile)),
        stored(2, Master, Live, "claude-code", Some(hostile)),
        stored(3, Switch, Live, "claude-code", Some(hostile)),
        stored(4, Owner, Live, "claude-code", Some(hidden.as_str())),
    ];
    sessions.set_messages(Some((&rows[..], true)));
    let (body, _) = sessions.view(76, 40, NOW, 0, &look).unwrap();
    assert_eq!(
        &texts(&body)[2..11],
        [
            "You",
            "  Red text",
            "  clip31m\\u202eevil tab",
            "Claude Code",
            "  Red text",
            "  clip31m\\u202eevil tab",
            "Red text",
            "clip31m\\u202eevil tab",
            "You",
        ]
    );
    for row in &body {
        assert!(
            row.text().chars().all(|character| !character.is_control()),
            "{:?}",
            row.text()
        );
    }
    assert!(
        body.iter().all(|row| row.width() <= 76),
        "hidden characters are shown as escapes before the text is wrapped, so no line outgrows the view"
    );
    let shown: String = texts(&body)[11..].concat().replace(' ', "");
    assert_eq!(
        shown,
        "a\\u200b".repeat(60),
        "every hidden character shows as an escape"
    );
}
