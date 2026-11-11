use super::*;

/// D6: New session from a project with no master session in view opens the master setup
/// form instead of refusing, and still refuses while the engine state is not ready.
#[test]
fn new_session_with_no_session_in_view_opens_the_master_setup() {
    let mut state = State::default();
    let project = ProjectId::new("no-session-yet").unwrap();
    state.select(None, Some(project.clone()), false).unwrap();
    assert_eq!(
        state.new_session(false),
        Err("Waiting for actual master state; Ctrl-R reconnects".to_owned())
    );
    assert!(!state.is_open());
    state.select(None, Some(project), true).unwrap();
    assert_eq!(state.new_session(false), Ok(()));
    assert!(state.is_open(), "the master setup form is open");
}

/// NT2-L1: leaving the project while a New session waits for the settings read says the
/// new session was not started, instead of leaving "Reading the current settings" up.
#[test]
fn leaving_the_project_while_a_new_session_waits_says_it_was_not_started() {
    let mut state = State::default();
    state
        .select(None, Some(ProjectId::new("first").unwrap()), true)
        .unwrap();
    state.renewal = Some(false);
    state
        .select(None, Some(ProjectId::new("second").unwrap()), true)
        .unwrap();
    assert_eq!(state.renewal, None);
    assert_eq!(
        state.notice,
        "New session not started. You left the project before the settings were read."
    );
}

/// One stored message of a live terminal turn, for the rows about the held messages.
/// Inputs: its sequence number and its text, if read. Output: the row.
fn stored_row(seq: u64, text: Option<&str>) -> SessionMessage {
    SessionMessage {
        seq,
        speaker: crate::master_sessions::MessageSpeaker::Owner,
        mode: crate::master_sessions::MasterMode::Live,
        harness: Some("claude-code".into()),
        text: text.map(str::to_owned),
        at_ms: 1_000 + seq,
    }
}

/// A run of stored rows with texts naming their sequence numbers. Input: the sequence
/// numbers. Output: the rows.
fn stored_rows(seqs: std::ops::RangeInclusive<u64>) -> Vec<SessionMessage> {
    seqs.map(|seq| stored_row(seq, Some(&format!("message {seq}"))))
        .collect()
}

#[test]
fn stored_messages_join_page_by_page_and_show_only_once_the_first_message_is_held() {
    let session = SessionId::new("session-1").unwrap();
    let mut held = Stored::default();
    assert!(held.whole(&session).is_empty());
    assert_eq!(held.wanted(), None);

    held.take(
        &session,
        StoredRead {
            newest: stored_rows(5..=6),
            newest_next: Some(5),
            older: None,
        },
    );
    assert!(
        held.whole(&session).is_empty(),
        "older messages are not held yet"
    );
    assert_eq!(held.wanted(), Some(5));

    held.take(
        &session,
        StoredRead {
            newest: stored_rows(5..=7),
            newest_next: Some(5),
            older: Some((5, stored_rows(3..=4), Some(3))),
        },
    );
    assert_eq!(held.wanted(), Some(3));
    assert!(held.whole(&session).is_empty());

    held.take(
        &session,
        StoredRead {
            newest: stored_rows(6..=7),
            newest_next: Some(6),
            older: Some((3, stored_rows(1..=2), None)),
        },
    );
    assert_eq!(held.wanted(), None);
    assert_eq!(held.whole(&session), stored_rows(1..=7).as_slice());
    let other = SessionId::new("session-2").unwrap();
    assert!(
        held.whole(&other).is_empty(),
        "one session's rows never show under another"
    );
}

#[test]
fn a_terminal_turn_s_owner_row_gains_its_words_and_a_stale_older_page_is_left_out() {
    let session = SessionId::new("session-1").unwrap();
    let mut held = Stored::default();
    held.take(
        &session,
        StoredRead {
            newest: vec![stored_row(1, Some("plan")), stored_row(2, None)],
            newest_next: None,
            older: None,
        },
    );
    assert_eq!(held.whole(&session)[1].text, None);
    held.take(
        &session,
        StoredRead {
            newest: vec![stored_row(2, Some("ship it")), stored_row(3, Some("done"))],
            newest_next: Some(2),
            older: Some((9, stored_rows(7..=8), Some(7))),
        },
    );
    let rows = held.whole(&session);
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [1, 2, 3],
        "a page asked below a cursor the client no longer holds adds nothing"
    );
    assert_eq!(rows[1].text.as_deref(), Some("ship it"));
}

#[test]
fn a_newest_page_that_does_not_reach_the_held_rows_or_is_of_another_session_starts_again() {
    let session = SessionId::new("session-1").unwrap();
    let mut held = Stored::default();
    held.take(
        &session,
        StoredRead {
            newest: stored_rows(1..=3),
            newest_next: None,
            older: None,
        },
    );
    held.take(
        &session,
        StoredRead {
            newest: stored_rows(70..=72),
            newest_next: Some(70),
            older: None,
        },
    );
    assert!(held.whole(&session).is_empty(), "rows 4 to 69 are not held");
    assert_eq!(held.wanted(), Some(70));

    let next = SessionId::new("session-2").unwrap();
    held.take(
        &next,
        StoredRead {
            newest: stored_rows(1..=2),
            newest_next: None,
            older: None,
        },
    );
    assert_eq!(held.whole(&next), stored_rows(1..=2).as_slice());
    assert!(held.whole(&session).is_empty());
}

#[test]
fn a_session_with_more_stored_messages_than_the_client_keeps_never_counts_as_whole() {
    let session = SessionId::new("session-1").unwrap();
    let mut held = Stored::default();
    let many = u64::try_from(MESSAGES_HELD_MAX).unwrap() + 10;
    held.take(
        &session,
        StoredRead {
            newest: stored_rows(1..=many),
            newest_next: None,
            older: None,
        },
    );
    assert!(held.whole(&session).is_empty());
    assert_eq!(
        held.wanted(),
        None,
        "the client stops asking for older pages"
    );
    assert_eq!(held.rows.len(), MESSAGES_HELD_MAX);
    assert_eq!(
        held.rows.last().map(|row| row.seq),
        Some(many),
        "the newest rows are the ones kept"
    );
}
