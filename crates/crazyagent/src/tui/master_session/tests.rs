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
