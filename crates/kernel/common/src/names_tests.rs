use crate::names::{IDENTITY_REFUSED, is_dotted, is_snake, single_component, validate_identity};

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

#[test]
fn an_identity_is_one_safe_component_of_at_most_128_bytes() {
    let key = format!("plugin-key-{}", "a".repeat(64));
    for value in [
        "acct-work",
        "Work.Acme_1",
        "a:b",
        key.as_str(),
        &"x".repeat(128),
    ] {
        assert_eq!(validate_identity(value), Ok(()), "{value}");
    }
    for value in [
        "",
        "..",
        "a..b",
        "a/b",
        "a\\b",
        "a b",
        "trailing.",
        "nul",
        "com1.txt",
        "caf\u{e9}",
        &"x".repeat(129),
    ] {
        assert_eq!(validate_identity(value), Err(IDENTITY_REFUSED), "{value:?}");
    }
    assert!(single_component("a:b"));
    assert!(!single_component("."));
    assert!(!single_component("a/b"));
}
