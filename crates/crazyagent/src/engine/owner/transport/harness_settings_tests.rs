use super::*;
use crate::harness_settings::tests::{fixture, fixture_json};
use crate::harness_settings::{Access, Catalog, Scope, Source, catalog, store};
use serde_json::{Value, json};

const PROJECT: &str = "alpha";
const QUALIFIED: Option<&str> = Some("0.50.0");
const SANDBOX_REASON: &str = "CrazyAgent runs every agent in its own sandbox.";

/// An engine root whose database holds the authority tables and one set-up project.
/// Output: the temporary folder and the engine root.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-harness-settings-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    (tmp, root)
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body, against the fixture Codex catalog. Inputs: the engine root, the
/// installed Codex version and the body. Output: the reply, or the refusal.
fn send(root: &Path, installed: Option<&str>, body: Value) -> Result<Reply, EngineError> {
    send_with(root, &fixture("codex"), installed, body)
}

/// Sends one request as `send` does, against another catalog. Inputs: the engine root,
/// the catalog, the installed version and the body. Output: the reply, or the refusal.
fn send_with(
    root: &Path,
    catalog: &Catalog,
    installed: Option<&str>,
    body: Value,
) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(
        protocol(&bytes).unwrap(),
        Protocol::HarnessSettings
    ));
    run(root, catalog, installed, parse(&bytes)?)
}

/// Reads every page of one scope's rows against a catalog. Inputs: the engine root, the
/// catalog, the installed version and the project, none for the app. Output: the rows.
fn every_value(
    root: &Path,
    catalog: &Catalog,
    installed: Option<&str>,
    project: Option<&str>,
) -> Vec<Setting> {
    let mut rows = Vec::new();
    let mut after: Option<u32> = None;
    loop {
        let body = json!({"harness_settings": {"kind": "values", "harness": "codex",
            "project": project, "after": after}});
        match send_with(root, catalog, installed, body).unwrap() {
            Reply::Values { page } => {
                assert!(!page.settings.is_empty());
                rows.extend(page.settings);
                match page.next {
                    Some(next) => after = Some(next),
                    None => return rows,
                }
            }
            other => panic!("expected values, got {other:?}"),
        }
    }
}

fn set_body(project: Option<&str>, key: &str, value: Value, version: u64) -> Value {
    json!({"harness_settings": {"kind": "set", "harness": "codex", "project": project,
        "key": key, "value": value, "version": version}})
}

fn reset_body(project: Option<&str>, key: &str, version: u64) -> Value {
    json!({"harness_settings": {"kind": "reset", "harness": "codex", "project": project,
        "key": key, "version": version}})
}

fn values(root: &Path, installed: Option<&str>, project: Option<&str>) -> Vec<Setting> {
    let body = json!({"harness_settings": {"kind": "values", "harness": "codex",
        "project": project, "after": null}});
    match send(root, installed, body).unwrap() {
        Reply::Values { page } => {
            assert_eq!(page.next, None);
            assert_eq!(page.installed.as_deref(), installed);
            page.settings
        }
        other => panic!("expected values, got {other:?}"),
    }
}

fn row<'a>(settings: &'a [Setting], key: &str) -> &'a Setting {
    settings
        .iter()
        .find(|setting| setting.key == key)
        .unwrap_or_else(|| panic!("no row for {key}"))
}

fn saved(reply: Result<Reply, EngineError>) -> Setting {
    match reply.unwrap() {
        Reply::Saved { setting } => setting,
        other => panic!("expected saved, got {other:?}"),
    }
}

fn stale(reply: Result<Reply, EngineError>) -> Setting {
    match reply.unwrap() {
        Reply::Stale { setting } => setting,
        other => panic!("expected stale, got {other:?}"),
    }
}

fn refused(reply: Result<Reply, EngineError>) -> (String, Setting) {
    match reply.unwrap() {
        Reply::Refused { code, setting } => (code, setting),
        other => panic!("expected refused, got {other:?}"),
    }
}

fn stored(root: &Path, scope: &Scope) -> Vec<(String, Value)> {
    let db = crate::storage::versioned::open(root).unwrap();
    store::stored(&db, &fixture("codex"), scope)
        .unwrap()
        .into_iter()
        .collect()
}

fn alpha() -> Scope {
    Scope::Project(ProjectId::new(PROJECT).unwrap())
}

#[test]
fn a_set_on_a_locked_key_is_refused_with_its_reason() {
    let (_tmp, root) = engine();
    for project in [None, Some(PROJECT)] {
        for installed in [QUALIFIED, Some("0.49.0"), None] {
            let (code, setting) = refused(send(
                &root,
                installed,
                set_body(project, "sandbox_mode", json!("danger-full-access"), 0),
            ));
            assert_eq!(code, "harness_settings_key_locked");
            assert_eq!(
                setting.access,
                Access::Locked {
                    reason: SANDBOX_REASON.into()
                }
            );
            assert_eq!(setting.effective, Value::Null);
            assert_eq!(setting.source, Source::Engine);
            assert_eq!(setting.here, None);
            assert_eq!(setting.version, 0);
            let (code, _) = refused(send(
                &root,
                installed,
                reset_body(project, "approval_policy", 0),
            ));
            assert_eq!(code, "harness_settings_key_locked");
        }
    }
    let shown = values(&root, QUALIFIED, Some(PROJECT));
    let sandbox = row(&shown, "sandbox_mode");
    assert_eq!(
        sandbox.access,
        Access::Locked {
            reason: SANDBOX_REASON.into()
        }
    );
    assert_eq!(sandbox.effective, Value::Null);
    assert_eq!(
        row(&shown, "approval_policy").access,
        Access::Locked {
            reason: "The engine decides what an agent may run.".into()
        }
    );
    assert!(stored(&root, &Scope::App).is_empty());
    assert!(stored(&root, &alpha()).is_empty());
}

#[test]
fn project_override_beats_app_default_and_reset_falls_back() {
    let (_tmp, root) = engine();
    let key = "model_reasoning_effort";
    let fresh = row(&values(&root, QUALIFIED, None), key).clone();
    assert_eq!(
        (fresh.here, fresh.effective, fresh.source, fresh.version),
        (None, json!("medium"), Source::Default, 0)
    );
    assert_eq!(fresh.access, Access::Editable);

    let app = saved(send(
        &root,
        QUALIFIED,
        set_body(None, key, json!("high"), 0),
    ));
    assert_eq!(
        (app.here, app.effective, app.source, app.version),
        (Some(json!("high")), json!("high"), Source::App, 1)
    );
    let inherited = row(&values(&root, QUALIFIED, Some(PROJECT)), key).clone();
    assert_eq!(
        (inherited.here, inherited.effective, inherited.source),
        (None, json!("high"), Source::App)
    );
    assert_eq!(inherited.version, 0);

    let project = saved(send(
        &root,
        QUALIFIED,
        set_body(Some(PROJECT), key, json!("low"), 0),
    ));
    assert_eq!(
        (
            project.here,
            project.effective,
            project.source,
            project.version
        ),
        (Some(json!("low")), json!("low"), Source::Project, 1)
    );
    let app_page = row(&values(&root, QUALIFIED, None), key).clone();
    assert_eq!(
        (app_page.effective, app_page.source),
        (json!("high"), Source::App)
    );
    assert_eq!(stored(&root, &alpha()), [(key.to_owned(), json!("low"))]);
    let locked = [
        ("project_doc_max_bytes".to_owned(), json!(0), Source::Engine),
        ("sandbox_mode".to_owned(), Value::Null, Source::Engine),
        ("approval_policy".to_owned(), json!("never"), Source::Engine),
    ];
    let launch = |installed: Option<&str>, project: Option<&ProjectId>| {
        let db = crate::storage::versioned::open(&root).unwrap();
        store::resolved(&db, &fixture("codex"), installed, project)
            .unwrap()
            .into_iter()
            .map(|resolved| (resolved.key, resolved.value, resolved.source))
            .collect::<Vec<_>>()
    };
    let alpha_id = ProjectId::new(PROJECT).unwrap();
    let mut expected = vec![(key.to_owned(), json!("low"), Source::Project)];
    expected.extend(locked.clone());
    assert_eq!(launch(QUALIFIED, Some(&alpha_id)), expected);
    let mut expected = vec![(key.to_owned(), json!("high"), Source::App)];
    expected.extend(locked.clone());
    assert_eq!(launch(QUALIFIED, None), expected);
    assert_eq!(launch(Some("0.49.0"), Some(&alpha_id)), locked);

    let back = saved(send(&root, QUALIFIED, reset_body(Some(PROJECT), key, 1)));
    assert_eq!(
        (back.here, back.effective, back.source, back.version),
        (None, json!("high"), Source::App, 2)
    );
    assert!(stored(&root, &alpha()).is_empty());
    let default = saved(send(&root, QUALIFIED, reset_body(None, key, 1)));
    assert_eq!(
        (
            default.here,
            default.effective,
            default.source,
            default.version
        ),
        (None, json!("medium"), Source::Default, 2)
    );
    let project_page = row(&values(&root, QUALIFIED, Some(PROJECT)), key).clone();
    assert_eq!(
        (
            project_page.effective,
            project_page.source,
            project_page.version
        ),
        (json!("medium"), Source::Default, 2)
    );
    assert!(stored(&root, &Scope::App).is_empty());
}

#[test]
fn an_unknown_or_unqualified_key_is_read_only() {
    let (_tmp, root) = engine();
    saved(send(
        &root,
        QUALIFIED,
        set_body(None, "hide_agent_reasoning", json!(true), 0),
    ));
    let db = crate::storage::versioned::open(&root).unwrap();
    db.execute(
        "INSERT INTO harness_settings(scope, project, harness, key, value_json, updated_ms)
         VALUES ('app', '', 'codex', 'retired_key', 'true', 1)",
        [],
    )
    .unwrap();
    drop(db);

    let shown = values(&root, QUALIFIED, None);
    let retired = row(&shown, "retired_key");
    let not_qualified = Access::ReadOnly {
        reason: "Not qualified for Codex 0.50.0".into(),
    };
    assert_eq!(retired.access, not_qualified);
    assert_eq!(retired.here, Some(json!(true)));
    assert_eq!(retired.effective, Value::Null);
    assert_eq!(retired.source, Source::NotApplied);
    assert_eq!(shown.last().unwrap().key, "retired_key");
    for reply in [
        send(
            &root,
            QUALIFIED,
            set_body(None, "retired_key", json!(false), 0),
        ),
        send(&root, QUALIFIED, reset_body(None, "never_listed", 0)),
        send(
            &root,
            QUALIFIED,
            set_body(None, "never_listed", json!(1), 0),
        ),
    ] {
        let (code, setting) = refused(reply);
        assert_eq!(code, "harness_settings_key_not_qualified");
        assert_eq!(setting.access, not_qualified);
    }

    for (installed, reason) in [
        (Some("0.49.0"), "Not qualified for Codex 0.49.0"),
        (None, "Not qualified for Codex (version unknown)"),
    ] {
        let read_only = Access::ReadOnly {
            reason: reason.into(),
        };
        let shown = values(&root, installed, None);
        for setting in &shown {
            if fixture("codex")
                .key(&setting.key)
                .is_some_and(|key| key.locked.is_some())
            {
                assert!(matches!(setting.access, Access::Locked { .. }));
            } else {
                assert_eq!(setting.access, read_only, "{}", setting.key);
            }
        }
        let hidden = row(&shown, "hide_agent_reasoning");
        assert_eq!(hidden.here, Some(json!(true)));
        assert_eq!(
            (&hidden.effective, &hidden.source),
            (&json!(false), &Source::Default)
        );
        let (code, setting) = refused(send(
            &root,
            installed,
            set_body(None, "hide_agent_reasoning", json!(false), 1),
        ));
        assert_eq!(code, "harness_settings_key_not_qualified");
        assert_eq!(setting.access, read_only);
        let (code, _) = refused(send(
            &root,
            installed,
            reset_body(Some(PROJECT), "model_reasoning_effort", 0),
        ));
        assert_eq!(code, "harness_settings_key_not_qualified");
        let body = json!({"harness_settings": {"kind": "catalog", "harness": "codex",
            "after": null}});
        match send(&root, installed, body).unwrap() {
            Reply::Catalog { page } => {
                assert!(!page.qualified);
                assert_eq!(page.installed.as_deref(), installed);
                assert_eq!(page.keys.len(), 6);
            }
            other => panic!("expected the catalog, got {other:?}"),
        }
    }
    assert_eq!(
        stored(&root, &Scope::App),
        [
            ("hide_agent_reasoning".to_owned(), json!(true)),
            ("retired_key".to_owned(), json!(true))
        ]
    );

    let (code, setting) = refused(send(
        &root,
        QUALIFIED,
        set_body(Some(PROJECT), "tui.status_label", json!("x"), 0),
    ));
    assert_eq!(code, "harness_settings_key_app_only");
    assert!(matches!(setting.access, Access::ReadOnly { .. }));
    saved(send(
        &root,
        QUALIFIED,
        set_body(None, "tui.status_label", json!("x"), 0),
    ));
}

#[test]
fn a_value_an_older_catalog_allowed_shows_with_a_reset_and_is_never_applied() {
    let (_tmp, root) = engine();
    for body in [
        set_body(None, "model_reasoning_effort", json!("minimal"), 0),
        set_body(Some(PROJECT), "model_reasoning_effort", json!("low"), 0),
        set_body(None, "tui.status_label", json!("abcdefghij"), 0),
        set_body(None, "hide_agent_reasoning", json!(true), 0),
    ] {
        saved(send(&root, QUALIFIED, body));
    }
    let mut later = fixture_json();
    later["keys"][1]["options"] = json!(["medium", "high"]);
    later["keys"][3]["max_bytes"] = json!(4);
    let later = catalog::parse(&serde_json::to_vec(&later).unwrap()).unwrap();

    let app = every_value(&root, &later, QUALIFIED, None);
    for (key, here, effective, source) in [
        (
            "model_reasoning_effort",
            json!("minimal"),
            json!("medium"),
            Source::Default,
        ),
        (
            "tui.status_label",
            json!("abcdefghij"),
            json!(""),
            Source::Default,
        ),
        (
            "hide_agent_reasoning",
            json!(true),
            json!(true),
            Source::App,
        ),
    ] {
        let shown = row(&app, key);
        assert_eq!(shown.access, Access::Editable, "{key}");
        assert_eq!(shown.here.as_ref(), Some(&here), "{key}");
        assert_eq!(
            (&shown.effective, shown.source),
            (&effective, source),
            "{key}"
        );
        assert_eq!(shown.version, 1, "{key}");
    }
    let project = every_value(&root, &later, QUALIFIED, Some(PROJECT));
    let effort = row(&project, "model_reasoning_effort");
    assert_eq!(effort.access, Access::Editable);
    assert_eq!(effort.here, Some(json!("low")));
    assert_eq!(
        (&effort.effective, effort.source),
        (&json!("medium"), Source::Default)
    );

    let db = crate::storage::versioned::open(&root).unwrap();
    let applied = |catalog: &Catalog| -> Vec<(String, Value, Source)> {
        store::resolved(
            &db,
            catalog,
            QUALIFIED,
            Some(&ProjectId::new(PROJECT).unwrap()),
        )
        .unwrap()
        .into_iter()
        .filter(|value| value.source != Source::Engine)
        .map(|value| (value.key, value.value, value.source))
        .collect()
    };
    assert_eq!(
        applied(&fixture("codex")),
        [
            ("hide_agent_reasoning".to_owned(), json!(true), Source::App),
            (
                "model_reasoning_effort".to_owned(),
                json!("low"),
                Source::Project
            ),
            (
                "tui.status_label".to_owned(),
                json!("abcdefghij"),
                Source::App
            ),
        ]
    );
    assert_eq!(
        applied(&later),
        [("hide_agent_reasoning".to_owned(), json!(true), Source::App)]
    );

    let app_effort = saved(send_with(
        &root,
        &later,
        QUALIFIED,
        set_body(None, "model_reasoning_effort", json!("high"), 1),
    ));
    assert_eq!(app_effort.here, Some(json!("high")));
    let effort = row(
        &every_value(&root, &later, QUALIFIED, Some(PROJECT)),
        "model_reasoning_effort",
    )
    .clone();
    assert_eq!(effort.here, Some(json!("low")));
    assert_eq!(
        (&effort.effective, effort.source),
        (&json!("high"), Source::App)
    );
    assert_eq!(
        applied(&later),
        [
            ("hide_agent_reasoning".to_owned(), json!(true), Source::App),
            (
                "model_reasoning_effort".to_owned(),
                json!("high"),
                Source::App
            ),
        ]
    );
    let reset = saved(send_with(
        &root,
        &later,
        QUALIFIED,
        reset_body(Some(PROJECT), "model_reasoning_effort", 1),
    ));
    assert_eq!(reset.here, None);
    assert_eq!(
        (&reset.effective, reset.source, reset.version),
        (&json!("high"), Source::App, 2)
    );
    let label = saved(send_with(
        &root,
        &later,
        QUALIFIED,
        reset_body(None, "tui.status_label", 1),
    ));
    assert_eq!((label.here, label.effective), (None, json!("")));
    assert!(stored(&root, &alpha()).is_empty());
}

#[test]
fn a_key_the_catalog_stopped_listing_can_be_reset_and_never_fills_the_page() {
    let (_tmp, root) = engine();
    for body in [
        set_body(None, "hide_agent_reasoning", json!(true), 0),
        set_body(None, "tui.status_label", json!("x"), 0),
    ] {
        saved(send(&root, QUALIFIED, body));
    }
    let unlisted = store::UNLISTED_MAX + 40;
    let mut db = crate::storage::versioned::open(&root).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..unlisted {
        tx.execute(
            "INSERT INTO harness_settings(scope, project, harness, key, value_json, updated_ms)
             VALUES ('app', '', 'codex', ?1, 'true', 1)",
            [format!("a{index:03}")],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO harness_settings(scope, project, harness, key, value_json, updated_ms)
         VALUES ('project', ?1, 'codex', 'b_retired', '7', 1)",
        [PROJECT],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);

    let codex = fixture("codex");
    let shown = every_value(&root, &codex, QUALIFIED, None);
    assert_eq!(shown.len(), codex.keys.len() + store::UNLISTED_MAX);
    let listed: Vec<&str> = shown[..codex.keys.len()]
        .iter()
        .map(|setting| setting.key.as_str())
        .collect();
    let catalog_keys: Vec<&str> = codex.keys.iter().map(|key| key.key.as_str()).collect();
    assert_eq!(listed, catalog_keys);
    for (index, setting) in shown[codex.keys.len()..].iter().enumerate() {
        assert_eq!(setting.key, format!("a{index:03}"));
        assert_eq!(setting.here, Some(json!(true)));
        assert_eq!(setting.source, Source::NotApplied);
        assert!(matches!(setting.access, Access::ReadOnly { .. }));
    }
    let hidden = row(&shown, "hide_agent_reasoning");
    assert_eq!(
        (&hidden.effective, hidden.source),
        (&json!(true), Source::App)
    );
    assert_eq!(row(&shown, "tui.status_label").here, Some(json!("x")));
    let db = crate::storage::versioned::open(&root).unwrap();
    let applied: Vec<String> = store::resolved(&db, &codex, QUALIFIED, None)
        .unwrap()
        .into_iter()
        .filter(|value| value.source == Source::App)
        .map(|value| value.key)
        .collect();
    assert_eq!(applied, ["hide_agent_reasoning", "tui.status_label"]);
    drop(db);

    let first = saved(send(&root, QUALIFIED, reset_body(None, "a000", 0)));
    assert_eq!((first.here, first.version), (None, 1));
    assert_eq!(first.source, Source::NotApplied);
    let unqualified = saved(send(&root, None, reset_body(None, "a001", 0)));
    assert_eq!(unqualified.here, None);
    let next = format!("a{:03}", store::UNLISTED_MAX);
    let shown = every_value(&root, &codex, QUALIFIED, None);
    assert_eq!(shown.len(), codex.keys.len() + store::UNLISTED_MAX);
    assert_eq!(row(&shown, &next).here, Some(json!(true)));
    assert!(shown.iter().all(|setting| setting.key != "a000"));

    let project = saved(send(
        &root,
        QUALIFIED,
        reset_body(Some(PROJECT), "b_retired", 0),
    ));
    assert_eq!(project.here, None);
    assert!(stored(&root, &alpha()).is_empty());
    for reply in [
        send(&root, QUALIFIED, reset_body(None, "a000", 1)),
        send(&root, QUALIFIED, reset_body(Some(PROJECT), "a002", 0)),
        send(&root, QUALIFIED, set_body(None, "a003", json!(false), 0)),
    ] {
        let (code, _) = refused(reply);
        assert_eq!(code, "harness_settings_key_not_qualified");
    }
    assert_eq!(
        stale(send(&root, QUALIFIED, reset_body(None, "a004", 3))).here,
        Some(json!(true))
    );
}

#[test]
fn a_value_of_the_wrong_type_or_over_its_bound_is_refused() {
    let (_tmp, root) = engine();
    let mut document = fixture_json();
    document["keys"][2]["locked"] = Value::Null;
    let open = catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    let send =
        |root: &Path, installed: Option<&str>, body: Value| send_with(root, &open, installed, body);
    for (project, key, value) in [
        (None, "hide_agent_reasoning", json!("yes")),
        (Some(PROJECT), "hide_agent_reasoning", json!(1)),
        (None, "hide_agent_reasoning", Value::Null),
        (None, "model_reasoning_effort", json!("extreme")),
        (Some(PROJECT), "model_reasoning_effort", json!(true)),
        (None, "project_doc_max_bytes", json!(-1)),
        (None, "project_doc_max_bytes", json!(1_048_577)),
        (Some(PROJECT), "project_doc_max_bytes", json!(1.5)),
        (None, "project_doc_max_bytes", json!("10")),
        (None, "tui.status_label", json!("x".repeat(33))),
        (None, "tui.status_label", json!("two\nlines")),
        (None, "tui.status_label", json!("right\u{202e}left")),
        (None, "hide_agent_reasoning", json!([true])),
    ] {
        let error = send(&root, QUALIFIED, set_body(project, key, value.clone(), 0)).unwrap_err();
        assert_eq!(error.0, "harness_settings_value_invalid", "{key} {value}");
    }
    assert!(stored(&root, &Scope::App).is_empty());
    assert!(stored(&root, &alpha()).is_empty());
    assert!(
        values(&root, QUALIFIED, None)
            .iter()
            .all(|setting| setting.version == 0)
    );
    saved(send(
        &root,
        QUALIFIED,
        set_body(None, "project_doc_max_bytes", json!(1_048_576), 0),
    ));
    saved(send(
        &root,
        QUALIFIED,
        set_body(None, "tui.status_label", json!("x".repeat(32)), 0),
    ));
    for (body, code) in [
        (
            set_body(None, "bad key", json!(true), 0),
            "harness_settings_key_invalid",
        ),
        (
            set_body(
                Some("missing-project"),
                "hide_agent_reasoning",
                json!(true),
                0,
            ),
            "harness_settings_project_unknown",
        ),
    ] {
        assert_eq!(send(&root, QUALIFIED, body).unwrap_err().0, code);
    }
}

#[test]
fn a_stale_set_is_refused() {
    let (_tmp, root) = engine();
    let key = "hide_agent_reasoning";
    assert_eq!(
        saved(send(&root, QUALIFIED, set_body(None, key, json!(true), 0))).version,
        1
    );
    for body in [
        set_body(None, key, json!(false), 0),
        reset_body(None, key, 0),
        set_body(None, key, json!(false), 2),
    ] {
        let current = stale(send(&root, QUALIFIED, body));
        assert_eq!((current.here, current.version), (Some(json!(true)), 1));
    }
    assert_eq!(
        saved(send(&root, QUALIFIED, set_body(None, key, json!(false), 1))).version,
        2
    );
    assert_eq!(
        saved(send(&root, QUALIFIED, reset_body(None, key, 2))).version,
        3
    );
    let after_reset = stale(send(&root, QUALIFIED, set_body(None, key, json!(true), 2)));
    assert_eq!((after_reset.here, after_reset.version), (None, 3));
    let other = stale(send(
        &root,
        QUALIFIED,
        set_body(Some(PROJECT), key, json!(true), 3),
    ));
    assert_eq!((other.here, other.version), (None, 0));
    assert!(stored(&root, &Scope::App).is_empty());
}

#[test]
fn harness_settings_codes_cover_every_refusal() {
    let transport = include_str!("harness_settings.rs");
    let (head, listed) = transport
        .split_once("pub const HARNESS_SETTINGS_CODES")
        .unwrap();
    let (_, tail) = listed.split_once("];").unwrap();
    let sources = [
        head,
        tail,
        include_str!("../../../harness_settings.rs"),
        include_str!("../../../harness_settings/catalog.rs"),
        include_str!("../../../harness_settings/store.rs"),
        include_str!("../../../harness_settings/installed.rs"),
    ];
    let scanned: Vec<&str> = sources
        .iter()
        .flat_map(|source| source.split("\"harness_settings_").skip(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    let found: Vec<&str> = scanned
        .iter()
        .copied()
        .filter(|rest| {
            rest.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
        .collect();
    assert_eq!(
        scanned.len() - found.len(),
        1,
        "only the tests module path is not a code: {scanned:?}"
    );
    assert!(found.len() >= 15, "{found:?}");
    for code in &found {
        let code = format!("harness_settings_{code}");
        assert!(
            HARNESS_SETTINGS_CODES.contains(&code.as_str()),
            "{code} is not in HARNESS_SETTINGS_CODES"
        );
    }
    for code in HARNESS_SETTINGS_CODES {
        if let Some(rest) = code.strip_prefix("harness_settings_") {
            assert!(found.contains(&rest), "{code} is never answered");
        }
    }
    let versioned = include_str!("../../../storage/versioned.rs");
    for code in versioned.split("failure(\"").skip(1) {
        let code = code.split('"').next().unwrap();
        assert!(HARNESS_SETTINGS_CODES.contains(&code), "{code}");
    }
    let mut sorted = HARNESS_SETTINGS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, HARNESS_SETTINGS_CODES);

    let (_tmp, root) = engine();
    let oversized = json!({"harness_settings": {"kind": "set", "harness": "codex",
        "project": null, "key": "tui.status_label",
        "value": "x".repeat(REQUEST_BYTES_MAX), "version": 0}});
    assert_eq!(
        send(&root, QUALIFIED, oversized).unwrap_err().0,
        "harness_settings_request_bounds"
    );
    let extra = json!({"harness_settings": {"kind": "values", "harness": "codex",
        "project": null, "after": null, "force": true}});
    assert_eq!(
        send(&root, QUALIFIED, extra).unwrap_err().0,
        "harness_settings_request_invalid"
    );
    let frame = refusal("harness_settings_unavailable").unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&frame[4..]).unwrap(),
        json!({"kind": "rejected", "code": "harness_settings_unavailable"})
    );
}
