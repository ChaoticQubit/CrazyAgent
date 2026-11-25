use super::closed_words;

#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_terminal_with_no_tmux_says_to_install_it_with_the_package_manager() {
    assert_eq!(
        closed_words("terminal_unavailable"),
        (
            "Terminals need tmux.",
            "Install it with your package manager, then run setup.",
        )
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn off_linux_a_terminal_with_no_tmux_keeps_the_generic_closed_words() {
    assert_eq!(
        closed_words("terminal_unavailable"),
        ("The terminal closed.", "Enter on the agent opens it again.")
    );
}
