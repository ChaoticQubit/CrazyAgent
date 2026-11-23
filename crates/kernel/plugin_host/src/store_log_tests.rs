use rusqlite::Connection;
use rusqlite::types::Value;

use crate::generated::permissions::BUNDLED_GRANTS;
use crate::store::initialize;
use crate::store_log::{Event, KINDS, REFUSED, SCHEMA, Source, apply};

/// The agent-quit plugin's id, a bundled code plugin.
const QUIT: &str = BUNDLED_GRANTS[0].plugin;

/// A key reference as the store names an entered key.
const REFERENCE: &str =
    "plugin-key-00000000000000000000000000000000000000000000000000000000000000c1";

/// A sha256 of 64 lowercase hex digits.
const SHA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

/// One valid event of every kind, in [`KINDS`] order. Output: the events.
fn every() -> Vec<Event> {
    vec![
        Event::Switched {
            plugin: "acme".into(),
            enabled: true,
        },
        Event::CodeSwitched {
            plugin: QUIT.into(),
            enabled: false,
        },
        Event::Trusted {
            plugin: "acme".into(),
            sha256: SHA.into(),
            hosts: vec!["api.acme.example".into(), "auth.acme.example".into()],
            trusted_ms: 7,
        },
        Event::Bound {
            plugin: "acme".into(),
            secret: "api_key".into(),
            source: Source::Key,
            reference: REFERENCE.into(),
        },
        Event::Unbound {
            plugin: "acme".into(),
            secret: "api_key".into(),
        },
        Event::Forgotten {
            plugin: "acme".into(),
        },
    ]
}

/// The refusal code of a result. Input: the result. Output: the code, none for Ok.
fn code<T>(result: Result<T, crate::Refused>) -> Option<&'static str> {
    result.err().map(|refused| refused.code)
}

/// A body with one field replaced. Inputs: the event and the field's new JSON value.
/// Output: the body.
fn with_field(event: &Event, field: &str, value: serde_json::Value) -> Vec<u8> {
    let mut body: serde_json::Value = serde_json::from_slice(&event.body().unwrap()).unwrap();
    body.as_object_mut().unwrap().insert(field.into(), value);
    serde_json::to_vec(&body).unwrap()
}

/// Every row of one store table, ordered by all its columns. Inputs: the store and the
/// table. Output: the rows.
fn rows(db: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
        .unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| (0..width).map(|at| row.get(at)).collect())
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A text cell. Input: the text. Output: the value.
fn text(value: &str) -> Value {
    Value::Text(value.into())
}

/// Each kind reads back from its own body, and a body outside its closed shape or its
/// bounds is refused, both when written and when read: a switch never names a code plugin
/// and a trust record never names a bundled one.
#[test]
fn every_store_event_decodes_only_its_closed_body() {
    let events = every();
    let kinds: Vec<&str> = events.iter().map(Event::kind).collect();
    assert_eq!(kinds, KINDS);
    for event in &events {
        let body = event.body().unwrap();
        assert_eq!(Event::decode(event.kind(), SCHEMA, &body).unwrap(), *event);
        assert_eq!(
            code(Event::decode(event.kind(), SCHEMA + 1, &body)),
            Some(REFUSED)
        );
        let extra = with_field(event, "note", serde_json::json!("x"));
        assert_eq!(
            code(Event::decode(event.kind(), SCHEMA, &extra)),
            Some(REFUSED),
            "{} refuses an unknown field",
            event.kind()
        );
    }
    assert_eq!(
        code(Event::decode("AppPluginRenamed", SCHEMA, b"{}")),
        Some(REFUSED)
    );

    let bound = &events[3];
    for (field, value) in [
        ("secret", "my api key"),
        ("secret", "sk-live-0123456789"),
        ("reference", "sk live 0123456789 abcdef"),
        ("reference", "../plugin-key-1"),
    ] {
        let body = with_field(bound, field, serde_json::json!(value));
        assert_eq!(
            code(Event::decode("AppPluginBound", SCHEMA, &body)),
            Some(REFUSED),
            "{field} {value}"
        );
    }
    let free_text = Event::Bound {
        plugin: "acme".into(),
        secret: "my api key".into(),
        source: Source::Key,
        reference: REFERENCE.into(),
    };
    assert_eq!(code(free_text.body()), Some(REFUSED));

    let trusted = &events[2];
    let long_sha = with_field(trusted, "sha256", serde_json::json!(format!("{SHA}0")));
    assert_eq!(
        code(Event::decode("AppPluginTrusted", SCHEMA, &long_sha)),
        Some(REFUSED)
    );
    let upper_sha = with_field(trusted, "sha256", serde_json::json!(SHA.to_uppercase()));
    assert_eq!(
        code(Event::decode("AppPluginTrusted", SCHEMA, &upper_sha)),
        Some(REFUSED)
    );
    let hosts: Vec<String> = (0..=crate::manifest::HOSTS_MAX)
        .map(|at| format!("h{at}.acme.example"))
        .collect();
    let too_many = with_field(trusted, "hosts", serde_json::json!(hosts));
    assert_eq!(
        code(Event::decode("AppPluginTrusted", SCHEMA, &too_many)),
        Some(REFUSED)
    );
    let long_host = "a".repeat(crate::manifest::HOST_BYTES_MAX + 1);
    let too_long = with_field(trusted, "hosts", serde_json::json!([long_host]));
    assert_eq!(
        code(Event::decode("AppPluginTrusted", SCHEMA, &too_long)),
        Some(REFUSED)
    );

    for name in [QUIT, "../acme", ""] {
        let switched = Event::Switched {
            plugin: name.into(),
            enabled: true,
        };
        assert_eq!(code(switched.body()), Some(REFUSED), "Switched {name:?}");
        let body = with_field(&events[0], "plugin", serde_json::json!(name));
        assert_eq!(
            code(Event::decode("AppPluginSwitched", SCHEMA, &body)),
            Some(REFUSED),
            "Switched {name:?}"
        );
    }
    let bundled = Event::Switched {
        plugin: "openai".into(),
        enabled: false,
    };
    assert!(
        bundled.body().is_ok(),
        "a bundled plugin switches by its name"
    );
    for name in ["openai", QUIT] {
        let trusted = Event::Trusted {
            plugin: name.into(),
            sha256: SHA.into(),
            hosts: Vec::new(),
            trusted_ms: 1,
        };
        assert_eq!(code(trusted.body()), Some(REFUSED), "Trusted {name}");
    }
    for name in ["acme", "openai"] {
        let switched = Event::CodeSwitched {
            plugin: name.into(),
            enabled: false,
        };
        assert_eq!(code(switched.body()), Some(REFUSED), "CodeSwitched {name}");
    }
}

/// Each kind writes the plugin store tables as the statement it replaces did: a code
/// plugin's switch under `code:<id>` and never a bare row, a forgotten plugin's rows gone
/// from all three tables and another plugin's rows kept.
#[test]
fn apply_writes_each_kind_as_the_old_statements_did() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    for event in every().into_iter().take(4) {
        apply(&db, &event).unwrap();
    }
    apply(
        &db,
        &Event::Switched {
            plugin: "near".into(),
            enabled: false,
        },
    )
    .unwrap();

    assert_eq!(
        rows(&db, "app_plugin_state"),
        [
            vec![text("acme"), Value::Integer(1)],
            vec![text(&format!("code:{QUIT}")), Value::Integer(0)],
            vec![text("near"), Value::Integer(0)],
        ]
    );
    assert_eq!(
        rows(&db, "app_plugin_trust"),
        [vec![
            text("acme"),
            text(SHA),
            text("api.acme.example\nauth.acme.example"),
            Value::Integer(7),
        ]]
    );
    assert_eq!(
        rows(&db, "app_plugin_secrets"),
        [vec![
            text("acme"),
            text("api_key"),
            text("key"),
            text(REFERENCE),
        ]]
    );

    apply(&db, &every()[4]).unwrap();
    assert!(rows(&db, "app_plugin_secrets").is_empty());

    apply(&db, &every()[3]).unwrap();
    apply(&db, &every()[5]).unwrap();
    assert!(rows(&db, "app_plugin_trust").is_empty());
    assert!(rows(&db, "app_plugin_secrets").is_empty());
    assert_eq!(
        rows(&db, "app_plugin_state"),
        [
            vec![text(&format!("code:{QUIT}")), Value::Integer(0)],
            vec![text("near"), Value::Integer(0)],
        ]
    );
}
