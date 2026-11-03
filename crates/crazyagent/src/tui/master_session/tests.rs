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
