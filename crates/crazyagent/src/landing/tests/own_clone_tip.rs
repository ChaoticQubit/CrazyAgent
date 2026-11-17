use super::*;

/// An agent in its own clone already on the tip hears nothing (DEC-147.6).
#[test]
fn an_own_clone_agent_on_the_tip_is_told_nothing() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    assert!(!tell_main_moved(
        &case.db,
        &case.workspace.project,
        CLONE_AGENT
    ));
    assert!(engine_notes(&case).is_empty());
}
