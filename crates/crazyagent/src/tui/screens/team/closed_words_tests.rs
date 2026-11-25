use super::closed_words;

#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_a_terminal_with_no_tmux_says_what_the_desktop_says() {
    assert_eq!(
        closed_words("terminal_unavailable"),
        (
            "Terminals need tmux, and it isn't ready on this Mac.",
            "Run setup again from Settings.",
        )
    );
}

#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_terminal_with_no_tmux_says_to_install_it_with_the_package_manager() {
    assert_eq!(
        closed_words("terminal_unavailable"),
        (
            "Terminals need tmux, and it isn't ready on this computer.",
            "Install tmux with your package manager, then run setup again from Settings.",
        )
    );
}

#[cfg(target_os = "windows")]
#[test]
fn on_windows_a_terminal_with_no_tmux_keeps_the_generic_closed_words() {
    assert_eq!(
        closed_words("terminal_unavailable"),
        ("The terminal closed.", "Enter on the agent opens it again.")
    );
}
