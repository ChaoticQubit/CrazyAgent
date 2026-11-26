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
        "openai:work",
        "ab:c",
        key.as_str(),
        &"x".repeat(128),
    ] {
        assert_eq!(validate_identity(value), Ok(()), "{value}");
    }
    for value in [
        "",
        "a:b",
        "Z:x",
        "c:",
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
    assert!(single_component("ab:c"));
    assert!(!single_component("."));
    assert!(!single_component("a/b"));
}

#[test]
fn a_store_name_is_lowercase_and_bounded() {
    use crate::names::{STORE_NAME_BYTES_MAX, is_store_name};
    for good in [
        "token",
        "a",
        "github.token",
        "a-b_c.d1",
        "a.",
        "nul",
        "com1.token",
        "x.-_y",
        "a.b.c",
        "0",
        "_",
        &"t".repeat(STORE_NAME_BYTES_MAX),
    ] {
        assert!(is_store_name(good), "{good:?}");
    }
    for bad in [
        "",
        "Token",
        ".token",
        "-token",
        "a..b",
        "a..",
        "ab..",
        "..",
        ".",
        "-",
        "a b",
        "a:b",
        "a/b",
        "a\0b",
        "a\u{7f}b",
        "caf\u{e9}",
        "CON",
        &"t".repeat(STORE_NAME_BYTES_MAX + 1),
        &format!("{}..", "t".repeat(STORE_NAME_BYTES_MAX - 2)),
    ] {
        assert!(!is_store_name(bad), "{bad:?}");
    }
}

#[test]
fn a_store_account_is_a_store_name_without_a_trailing_dot() {
    use crate::names::is_store_account;
    for good in ["token", "nul", "con", "nul.token", "a.b"] {
        assert!(is_store_account(good), "{good:?}");
    }
    for bad in ["a.", "token.", "", "Token", "a b"] {
        assert!(!is_store_account(bad), "{bad:?}");
    }
}

#[test]
fn the_project_state_folder_and_the_null_device_have_one_kernel_home() {
    assert_eq!(crate::names::CRAZYAGENT_DIRECTORY, ".crazyagent");
    #[cfg(not(windows))]
    assert_eq!(crate::names::NULL_DEVICE, "/dev/null");
    #[cfg(windows)]
    assert_eq!(crate::names::NULL_DEVICE, "NUL");
}
