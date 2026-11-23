use crate::names::{is_dotted, is_snake};

#[test]
fn a_snake_name_is_a_lowercase_letter_then_lowercase_letters_digits_and_underscores() {
    for name in ["a", "settings", "agent_quit", "v2", "a_1_b"] {
        assert!(is_snake(name), "{name}");
    }
    for name in [
        "",
        "Settings",
        "9a",
        "_a",
        "agent-quit",
        "a.b",
        "a b",
        "caf\u{e9}",
    ] {
        assert!(!is_snake(name), "{name}");
    }
}

#[test]
fn a_dotted_name_is_two_or_more_snake_names_joined_by_dots() {
    for name in ["settings.write", "net.http.get", "agent_quit.read"] {
        assert!(is_dotted(name), "{name}");
    }
    for name in [
        "settings",
        "",
        ".",
        "settings.",
        ".write",
        "settings..write",
        "Settings.write",
        "net.http-get",
    ] {
        assert!(!is_dotted(name), "{name}");
    }
}
