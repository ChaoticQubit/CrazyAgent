use super::*;

/// Every code the App plugins protocol lists reaches the window as itself; a lost
/// controller reads as not connected, and any other code is an unknown outcome that keeps
/// the engine code behind it.
#[test]
fn listed_app_plugin_codes_pass_through_and_others_read_unknown() {
    for code in APP_PLUGINS_CODES {
        assert_eq!(app_plugin_error(EngineError((*code).into())).code, *code);
    }
    assert_eq!(
        app_plugin_error(EngineError("controller_disconnected".into())).code,
        "not_connected"
    );
    let unlisted = app_plugin_error(EngineError("app_plugin_store_torn".into()));
    assert_eq!(unlisted.code, "app_plugin_outcome_unknown");
    assert_eq!(unlisted.cause.as_deref(), Some("app_plugin_store_torn"));
}

/// The bridge hands the engine only an absolute folder path within the bound and reads
/// nothing in it; an empty, relative or oversized path is refused.
#[test]
fn only_an_absolute_folder_within_the_bound_is_handed_on() {
    let absolute = if cfg!(windows) {
        r"C:\Users\owner\acme"
    } else {
        "/Users/owner/acme"
    };
    assert_eq!(
        picked_folder(absolute.into()).unwrap(),
        PathBuf::from(absolute)
    );
    let long = format!("{absolute}{}", "a".repeat(FOLDER_BYTES_MAX));
    for refused in [String::new(), "acme".into(), "./acme".into(), long] {
        assert_eq!(
            picked_folder(refused.clone()).unwrap_err().code,
            "app_plugin_request_invalid",
            "{refused}"
        );
    }
}

/// An empty typed key is refused before anything is sent, and a key with a line break is
/// refused by the engine's secret type.
#[test]
fn an_empty_or_broken_key_is_refused_before_it_is_sent() {
    for refused in ["", "sk-one\nsk-two"] {
        assert_eq!(
            entered_key(refused.into()).err().map(|error| error.code),
            Some("app_plugin_key_invalid")
        );
    }
    assert!(entered_key("sk-typed-key-0001".into()).is_ok());
}
