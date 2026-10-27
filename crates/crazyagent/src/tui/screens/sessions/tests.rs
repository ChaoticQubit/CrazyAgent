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
        SessionsReply::Read
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
        assert_eq!(&drawn[2..], rows);
        assert_eq!(
            help,
            vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Enter", "Resume"),
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
fn the_list_reads_follows_its_cursor_and_says_why_a_read_failed() {
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
    sessions.failed("Could not read the sessions.".into());
    assert_eq!(
        texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0),
        ["Could not read the sessions."]
    );
    assert_eq!(
        sessions.key(press(KeyCode::Char('r')), true, false),
        SessionsReply::Read
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
    sessions.failed("Could not refresh the sessions.".into());
    let drawn = texts(&sessions.view(76, 12, NOW, 0, &look).unwrap().0);
    assert_eq!(
        drawn[1],
        "Could not refresh the sessions. Press r to try again."
    );
    assert_eq!(drawn.len(), 5, "the rows already read stay");

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
        drawn[7].starts_with("> Parked  11:00         Session 49"),
        "{drawn:?}"
    );
    sessions.set(Vec::new());
    assert_eq!(
        texts(&sessions.view(76, 8, NOW, 0, &look).unwrap().0)[2],
        "No sessions yet."
    );
}
