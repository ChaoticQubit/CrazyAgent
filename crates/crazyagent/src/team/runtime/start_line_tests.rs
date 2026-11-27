use super::*;

/// A start refused with a code no sentence covers names only that code.
#[test]
fn a_start_refused_with_an_unknown_code_names_only_the_code() {
    assert_eq!(
        start_problem(&LaunchError("launch_deadline".into(), None)),
        "The agent could not start. (launch_deadline)"
    );
}

/// A start the terminal host refused for a tmux too old tells the owner to run setup, with
/// the code kept after the sentence, and a terminal code with no sentence of its own keeps
/// today's line.
#[test]
fn a_start_refused_by_an_old_tmux_says_to_run_setup_and_keeps_its_code() {
    assert_eq!(
        start_problem(&LaunchError("terminal_tmux_unsupported".into(), None)),
        "The agent could not start. The installed tmux is too old for agent terminals. Run \
         setup to update it. (terminal_tmux_unsupported)"
    );
    assert_eq!(
        start_problem(&LaunchError("terminal_made_up".into(), None)),
        "The agent could not start. (terminal_made_up)"
    );
}
