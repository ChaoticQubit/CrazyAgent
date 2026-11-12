use super::*;
use crate::harness_settings::{Access, Source};
use serde_json::{Value, json};

const PROJECT: &str = "alpha";

/// An engine root whose database holds the authority tables and one set-up project.
/// Output: the temporary folder and the engine root.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-model-choice-")
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
/// client's frame body. Inputs: the engine root and the body. Output: the reply, or the
/// refusal.
fn send(root: &Path, body: Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(
        super::super::protocol(&bytes).unwrap(),
        super::super::Protocol::ModelChoice
    ));
    run(root, parse(&bytes)?)
}

fn values(root: &Path, project: Option<&str>) -> Models {
    match send(
        root,
        json!({"model_choice": {"kind": "values", "harness": "codex", "project": project}}),
    )
    .unwrap()
    {
        Reply::Values { models } => models,
        other => panic!("not values: {other:?}"),
    }
}

fn row(models: &Models, slot: Slot) -> Row {
    models
        .rows
        .iter()
        .find(|row| row.slot == slot)
        .cloned()
        .unwrap()
}

fn added(root: &Path, model: &str) -> (Added, Models) {
    match send(
        root,
        json!({"model_choice": {"kind": "add", "harness": "codex", "model": model}}),
    )
    .unwrap()
    {
        Reply::Added { added, models } => (added, models),
        other => panic!("not added: {other:?}"),
    }
}

/// Passes the test launch of one pending owner id, as the engine's check does.
fn verified(root: &Path, model: &str) {
    let db = versioned::open(root).unwrap();
    store::custom_checked(&db, "codex", model, true).unwrap();
}

#[test]
fn the_owner_adds_a_model_ticks_it_and_overrides_it_in_one_project() {
    let (_tmp, root) = engine();
    let (state, models) = added(&root, "gpt-a");
    assert_eq!(state, Added::Pending);
    let candidate = models
        .candidates
        .iter()
        .find(|candidate| candidate.id == "gpt-a")
        .unwrap();
    assert_eq!(candidate.origin, crate::model_choice::Origin::Pending);
    let refused = send(
        &root,
        json!({"model_choice": {"kind": "set", "harness": "codex", "project": null,
            "slot": "agent", "value": "gpt-a", "version": 0}}),
    )
    .unwrap_err();
    assert_eq!(refused.0, "model_choice_value_invalid");
    verified(&root, "gpt-a");
    assert_eq!(added(&root, "gpt-a").0, Added::Verified);
    added(&root, "gpt-b");
    verified(&root, "gpt-b");

    for (slot, value) in [
        ("allowed", json!(["gpt-a", "gpt-b"])),
        ("agent", json!("gpt-a")),
    ] {
        let reply = send(
            &root,
            json!({"model_choice": {"kind": "set", "harness": "codex", "project": null,
                "slot": slot, "value": value, "version": 0}}),
        )
        .unwrap();
        assert!(matches!(reply, Reply::Saved { .. }), "{reply:?}");
    }
    let reply = send(
        &root,
        json!({"model_choice": {"kind": "set", "harness": "codex", "project": PROJECT,
            "slot": "agent", "value": "gpt-b", "version": 0}}),
    )
    .unwrap();
    let Reply::Saved { row: saved } = reply else {
        panic!("not saved: {reply:?}");
    };
    assert_eq!(saved.effective, json!("gpt-b"));
    assert_eq!(saved.source, Source::Project);

    let app = values(&root, None);
    assert_eq!(row(&app, Slot::Agent).effective, json!("gpt-a"));
    assert_eq!(
        row(&app, Slot::Allowed).effective,
        json!(["gpt-a", "gpt-b"])
    );
    let mine = values(&root, Some(PROJECT));
    assert_eq!(row(&mine, Slot::Agent).effective, json!("gpt-b"));
    assert_eq!(row(&mine, Slot::Allowed).source, Source::App);
    assert_eq!(row(&mine, Slot::Master).access, Access::Editable);

    let stale = send(
        &root,
        json!({"model_choice": {"kind": "reset", "harness": "codex", "project": PROJECT,
            "slot": "agent", "version": 0}}),
    )
    .unwrap();
    assert!(matches!(stale, Reply::Stale { .. }), "{stale:?}");
    let reset = send(
        &root,
        json!({"model_choice": {"kind": "reset", "harness": "codex", "project": PROJECT,
            "slot": "agent", "version": 1}}),
    )
    .unwrap();
    let Reply::Saved { row: back } = reset else {
        panic!("not saved: {reset:?}");
    };
    assert_eq!(back.effective, json!("gpt-a"));
    assert_eq!(back.source, Source::App);

    match send(
        &root,
        json!({"model_choice": {"kind": "remove", "harness": "codex", "model": "gpt-b"}}),
    )
    .unwrap()
    {
        Reply::Removed { removed, models } => {
            assert!(removed);
            assert_eq!(row(&models, Slot::Allowed).effective, json!(["gpt-a"]));
        }
        other => panic!("not removed: {other:?}"),
    }
}

#[test]
fn a_request_out_of_shape_or_bounds_is_refused() {
    let (_tmp, root) = engine();
    for body in [
        json!({"model_choice": {"kind": "values", "harness": "codex", "project": null, "extra": 1}}),
        json!({"model_choice": {"kind": "set", "harness": "codex", "project": null,
            "slot": "everything", "value": "gpt-a", "version": 0}}),
        json!({"model_choice": {"kind": "drop", "harness": "codex"}}),
        json!({"model_choice": {}}),
    ] {
        assert_eq!(
            send(&root, body.clone()).unwrap_err().0,
            "model_choice_request_invalid",
            "{body}"
        );
    }
    let long = "m".repeat(REQUEST_BYTES_MAX);
    assert_eq!(
        send(
            &root,
            json!({"model_choice": {"kind": "add", "harness": "codex", "model": long}})
        )
        .unwrap_err()
        .0,
        "model_choice_request_bounds"
    );
    assert_eq!(
        send(
            &root,
            json!({"model_choice": {"kind": "values", "harness": "nope", "project": null}})
        )
        .unwrap_err()
        .0,
        "model_choice_harness_unknown"
    );
    assert_eq!(
        send(
            &root,
            json!({"model_choice": {"kind": "values", "harness": "codex", "project": "beta"}})
        )
        .unwrap_err()
        .0,
        "model_choice_project_unknown"
    );
    assert_eq!(
        send(
            &root,
            json!({"model_choice": {"kind": "add", "harness": "codex", "model": "-c x"}})
        )
        .unwrap_err()
        .0,
        "model_choice_value_invalid"
    );
}

#[test]
fn every_code_the_store_returns_is_a_protocol_code() {
    for code in crate::model_choice::MODEL_CHOICE_CODES {
        assert!(MODEL_CHOICE_PROTOCOL_CODES.contains(code), "{code}");
    }
    let refused = refusal("model_choice_unavailable").unwrap();
    let reply: Reply = serde_json::from_slice(&refused[4..]).unwrap();
    assert!(matches!(reply, Reply::Rejected { code } if code == "model_choice_unavailable"));
}
