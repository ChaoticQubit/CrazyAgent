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
    run(root, parse(&bytes)?, None)
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

#[test]
fn the_owner_reads_and_sets_the_pick_and_fallback_rules() {
    use crate::model_choice::rules::{FallbackRule, PickRule, Rules};
    let (_tmp, root) = engine();
    let read = |root: &Path| -> Rules {
        match send(root, json!({"model_choice": {"kind": "rules"}})).unwrap() {
            Reply::Rules { rules } => rules,
            other => panic!("not rules: {other:?}"),
        }
    };
    let first = read(&root);
    assert_eq!(
        (first.pick, first.fallback),
        (PickRule::Strict, FallbackRule::Notice)
    );
    let set = |root: &Path, rule: Value, version: u64| {
        send(
            root,
            json!({"model_choice": {"kind": "set_rule", "rule": rule, "version": version}}),
        )
        .unwrap()
    };
    let Reply::RuleSaved { rules: saved } = set(
        &root,
        json!({"rule": "fallback", "value": "stop"}),
        first.fallback_version,
    ) else {
        panic!("the fallback rule was not saved");
    };
    assert_eq!(
        (saved.pick, saved.fallback),
        (PickRule::Strict, FallbackRule::Stop)
    );
    let Reply::RuleStale { rules: current } = set(
        &root,
        json!({"rule": "fallback", "value": "silent"}),
        first.fallback_version,
    ) else {
        panic!("a stale rule set was saved");
    };
    assert_eq!(current, saved);
    assert_eq!(read(&root), saved);
    for bad in [
        json!({"model_choice": {"kind": "set_rule", "rule": {"rule": "pick", "value": "loose"}, "version": 0}}),
        json!({"model_choice": {"kind": "set_rule", "rule": {"rule": "speed", "value": "fast"}, "version": 0}}),
        json!({"model_choice": {"kind": "rules", "extra": 1}}),
    ] {
        assert_eq!(
            send(&root, bad).unwrap_err().0,
            "model_choice_request_invalid"
        );
    }
}

#[test]
fn the_largest_models_section_fits_one_owner_frame() {
    use crate::model_choice::{
        ALLOWED_MAX, CATALOG_MODELS_MAX, CUSTOM_MAX, Candidate, MODEL_BYTES_MAX, Origin,
    };
    let id = |n: usize| {
        let stem = format!("m{n}-");
        format!("{stem}{}", "x".repeat(MODEL_BYTES_MAX - stem.len()))
    };
    let full: Value = json!((0..ALLOWED_MAX).map(id).collect::<Vec<_>>());
    let rows = Slot::ALL
        .iter()
        .map(|slot| {
            let value = if *slot == Slot::Native {
                json!("harness")
            } else {
                full.clone()
            };
            (slot, value)
        })
        .map(|(slot, value)| Row {
            slot: *slot,
            here: Some(value.clone()),
            effective: value,
            source: Source::Project,
            version: u64::MAX,
            access: Access::ReadOnly {
                reason: crate::model_choice::NOT_SUPPORTED.into(),
            },
        })
        .collect();
    let candidates = (0..CATALOG_MODELS_MAX + CUSTOM_MAX)
        .map(|n| Candidate {
            id: id(n),
            origin: Origin::Verified,
        })
        .collect();
    let models = Models {
        harness: "claude-code".into(),
        rows,
        candidates,
        qualified: false,
        reason: Some("r".repeat(crate::model_choice::UNQUALIFIED_REASON_BYTES_MAX)),
        testable: true,
        live_note: Some(crate::model_choice::live::not_reported("claude-code")),
        providers: Vec::new(),
        live_retry: true,
    };
    for reply in [
        Reply::Values {
            models: models.clone(),
        },
        Reply::Added {
            added: Added::Pending,
            models,
        },
    ] {
        let frame = encode_frame(&reply).unwrap();
        assert!(frame.len() <= crate::protocol::FRAME_BYTES_MAX + 4);
    }
}

#[test]
fn only_a_models_view_open_reads_the_live_list() {
    let action = |body: Value| parse(&serde_json::to_vec(&body).unwrap()).unwrap();
    let open = action(
        json!({"model_choice": {"kind": "values", "harness": "codex", "project": null, "fresh": true}}),
    );
    assert_eq!(live_due(&open), Some("codex"));
    let again = action(
        json!({"model_choice": {"kind": "values", "harness": "codex", "project": null, "fresh": false}}),
    );
    assert_eq!(
        live_due(&again),
        None,
        "a re-read after a change starts no harness"
    );
    let old_client =
        action(json!({"model_choice": {"kind": "values", "harness": "codex", "project": null}}));
    assert_eq!(live_due(&old_client), None);
    let add =
        action(json!({"model_choice": {"kind": "add", "harness": "codex", "model": "gpt-x"}}));
    assert_eq!(live_due(&add), None);
    assert_eq!(
        live_due(&action(json!({"model_choice": {"kind": "rules"}}))),
        None
    );
}

#[test]
fn only_an_ask_again_reads_the_live_list_inside_its_backoff() {
    let action = |body: Value| parse(&serde_json::to_vec(&body).unwrap()).unwrap();
    let again = action(json!({"model_choice": {
        "kind": "values", "harness": "codex", "project": null, "fresh": true, "retry": true
    }}));
    assert_eq!(live_due(&again), Some("codex"));
    assert!(retried(&again));
    let open = action(
        json!({"model_choice": {"kind": "values", "harness": "codex", "project": null, "fresh": true}}),
    );
    assert!(!retried(&open), "a plain open keeps the backoff");
    assert!(!retried(&action(
        json!({"model_choice": {"kind": "rules"}})
    )));
}

#[test]
fn a_models_reply_says_whether_the_harness_list_can_be_read_again() {
    let (_tmp, root) = engine();
    let db = versioned::open(&root).unwrap();
    store::live_record(&db, "codex", Some(&["gpt-live".to_owned()])).unwrap();
    let shown = |harness: &str, executable: bool| {
        let body = json!({"model_choice": {"kind": "values", "harness": harness, "project": null}});
        match readable(send(&root, body).unwrap(), executable) {
            Reply::Values { models } => models,
            other => panic!("not values: {other:?}"),
        }
    };
    let installed = shown("codex", true);
    assert!(installed.live_retry, "a reader and an executable");
    assert_eq!(installed.live_note, None);
    let missing = shown("codex", false);
    assert!(!missing.live_retry, "no executable to read");
    assert_eq!(
        missing.live_note.as_deref(),
        Some("live list not reported by Codex"),
        "nothing can report the kept list now"
    );
    let claude = shown("claude-code", true);
    assert!(!claude.live_retry, "no list command");
    assert_eq!(
        claude.live_note.as_deref(),
        Some("live list not reported by Claude Code")
    );
    let reply = Reply::Added {
        added: Added::Pending,
        models: values(&root, None),
    };
    match readable(reply, true) {
        Reply::Added { models, .. } => assert!(models.live_retry),
        other => panic!("not added: {other:?}"),
    }
}

#[test]
fn the_models_reply_carries_the_live_note_and_a_reported_list() {
    let (_tmp, root) = engine();
    assert_eq!(
        values(&root, None).live_note.as_deref(),
        Some("live list not reported by Codex")
    );
    let db = versioned::open(&root).unwrap();
    store::live_record(&db, "codex", Some(&["gpt-live".to_owned()])).unwrap();
    let models = values(&root, None);
    assert_eq!(models.live_note, None);
    assert!(
        models
            .candidates
            .iter()
            .any(|candidate| candidate.id == "gpt-live"
                && candidate.origin == crate::model_choice::Origin::Catalog),
        "{models:?}"
    );
}

fn pool() -> CredentialRef {
    CredentialRef::new("pool-main").unwrap()
}

/// A launch that must not run: the checks before it refused the test.
fn never<'a>() -> Launch<'a> {
    Box::new(|_, _| panic!("a refused test spends no run"))
}

/// A launch that answers the given decision once, recording the pool it was handed.
fn answers<'a>(
    decision: (Tested, Option<String>),
    handed: &'a std::cell::RefCell<Option<CredentialRef>>,
) -> Launch<'a> {
    Box::new(move |pool, _| {
        *handed.borrow_mut() = Some(pool.clone());
        Ok(decision)
    })
}

fn origin(models: &Models, id: &str) -> crate::model_choice::Origin {
    models
        .candidates
        .iter()
        .find(|c| c.id == id)
        .unwrap()
        .origin
}

#[test]
fn a_test_request_names_the_harness_the_id_and_the_pool() {
    let bytes = serde_json::to_vec(&json!({"model_choice": {
        "kind": "test", "harness": "codex", "model": "my-model", "pool": "pool-main"
    }}))
    .unwrap();
    assert!(matches!(
        super::super::protocol(&bytes).unwrap(),
        super::super::Protocol::ModelChoice
    ));
    match parse(&bytes).unwrap() {
        Action::Test {
            harness,
            model,
            pool: picked,
        } => {
            assert_eq!((harness.as_str(), model.as_str()), ("codex", "my-model"));
            assert_eq!(picked, Some(pool()));
        }
        other => panic!("not a test: {other:?}"),
    }
}

#[test]
fn a_test_spends_no_run_without_a_supported_harness_a_pool_or_a_pending_id() {
    let (_tmp, root) = engine();
    let (state, _) = added(&root, "gpt-a");
    assert_eq!(state, Added::Pending);
    let refused = |harness: &str, model: &str, pool: Option<&CredentialRef>| {
        tested(&root, harness, model, pool, None, never())
            .unwrap_err()
            .0
    };
    assert_eq!(
        refused("opencode", "gpt-a", Some(&pool())),
        "model_choice_not_supported"
    );
    assert_eq!(refused("codex", "gpt-a", None), test_launch::POOL_NEEDED);
    assert_eq!(
        refused("codex", "never-added", Some(&pool())),
        "model_choice_custom_not_pending"
    );
    verified(&root, "gpt-a");
    assert_eq!(
        refused("codex", "gpt-a", Some(&pool())),
        "model_choice_custom_not_pending"
    );
}

#[test]
fn a_test_records_verified_and_failed_and_leaves_a_pending_id_pending_with_its_reason() {
    let (_tmp, root) = engine();
    let handed = std::cell::RefCell::new(None);
    for (id, decision, after) in [
        (
            "gpt-a",
            (Tested::Verified, None),
            crate::model_choice::Origin::Verified,
        ),
        (
            "gpt-b",
            (Tested::Failed, None),
            crate::model_choice::Origin::Failed,
        ),
        (
            "gpt-c",
            (
                Tested::Pending,
                Some("The test launch timed out.".to_owned()),
            ),
            crate::model_choice::Origin::Pending,
        ),
    ] {
        added(&root, id);
        let reply = tested(
            &root,
            "codex",
            id,
            Some(&pool()),
            None,
            answers(decision.clone(), &handed),
        )
        .unwrap();
        let Reply::Tested {
            tested: outcome,
            reason,
            models,
        } = reply
        else {
            panic!("not tested: {reply:?}");
        };
        assert_eq!((outcome, reason), decision, "{id}");
        assert_eq!(origin(&models, id), after, "{id}");
        assert_eq!(handed.borrow_mut().take(), Some(pool()));
    }
}

#[test]
fn a_test_whose_id_was_removed_while_it_ran_stays_undecided_and_a_busy_test_is_refused() {
    let (_tmp, root) = engine();
    added(&root, "gpt-a");
    let removing: Launch<'_> = Box::new(|_, _| {
        let db = versioned::open(&root).unwrap();
        assert!(store::custom_remove(&db, "codex", "gpt-a").unwrap());
        Ok((Tested::Verified, None))
    });
    let Reply::Tested {
        tested: outcome,
        reason,
        ..
    } = tested(&root, "codex", "gpt-a", Some(&pool()), None, removing).unwrap()
    else {
        panic!("not tested");
    };
    assert_eq!(outcome, Tested::Pending);
    assert!(reason.unwrap().contains("changed"));

    added(&root, "gpt-b");
    let busy: Launch<'_> = Box::new(|_, _| Err(test_launch::BUSY));
    assert_eq!(
        tested(&root, "codex", "gpt-b", Some(&pool()), None, busy)
            .unwrap_err()
            .0,
        test_launch::BUSY
    );
}

/// DEC-150 addendum 9e (leader 19:01): the owner adds an RCLI id naming one keyed provider,
/// RCLI's Models section offers those providers and a test launch, and the test hands the
/// launch the pool and the id's provider; a Codex test hands no provider.
#[test]
fn an_rcli_id_is_added_with_its_provider_and_its_test_runs_on_that_provider() {
    let (_tmp, root) = engine();
    let add = |provider: Option<&str>| {
        let mut body =
            json!({"model_choice": {"kind": "add", "harness": "rcli", "model": "my-model"}});
        if let Some(provider) = provider {
            body["model_choice"]["provider"] = json!(provider);
        }
        send(&root, body)
    };
    assert_eq!(add(None).unwrap_err().0, "model_choice_provider_needed");
    assert_eq!(
        add(Some("local")).unwrap_err().0,
        "model_choice_provider_unknown"
    );
    match add(Some("openrouter")).unwrap() {
        Reply::Added { added, models } => {
            assert_eq!(added, Added::Pending);
            assert!(models.testable);
            assert_eq!(
                models.providers,
                crate::model_choice::provider_choices("rcli")
            );
            assert!(!models.providers.is_empty());
        }
        other => panic!("not added: {other:?}"),
    }
    assert_eq!(
        send(
            &root,
            json!({"model_choice": {"kind": "add", "harness": "codex", "model": "gpt-a", "provider": "openai"}})
        )
        .unwrap_err()
        .0,
        "model_choice_provider_unknown"
    );
    let handed = std::cell::RefCell::new(None);
    let named = std::cell::RefCell::new(None);
    let launch: Launch<'_> = Box::new(|pool, provider| {
        *handed.borrow_mut() = Some(pool.clone());
        *named.borrow_mut() = provider.map(str::to_owned);
        Ok((Tested::Verified, None))
    });
    match tested(&root, "rcli", "my-model", Some(&pool()), None, launch).unwrap() {
        Reply::Tested { tested, reason, .. } => {
            assert_eq!((tested, reason), (Tested::Verified, None));
        }
        other => panic!("not tested: {other:?}"),
    }
    assert_eq!(handed.take(), Some(pool()));
    assert_eq!(named.take().as_deref(), Some("openrouter"));
    let db = versioned::open(&root).unwrap();
    assert_eq!(
        store::custom_provider(&db, "rcli", "my-model")
            .unwrap()
            .as_deref(),
        Some("openrouter")
    );
    let (state, _) = added(&root, "gpt-a");
    assert_eq!(state, Added::Pending);
    let codex: Launch<'_> = Box::new(|_, provider| {
        assert_eq!(provider, None, "a Codex id names no provider");
        Ok((Tested::Failed, None))
    });
    tested(&root, "codex", "gpt-a", Some(&pool()), None, codex).unwrap();
}

/// DEC-150 addendum 9e: an RCLI id added again under another provider while its test ran
/// is never recorded on a provider it was not tested on; it stays pending under the new
/// provider, saying it changed.
#[test]
fn an_rcli_id_added_again_under_another_provider_while_tested_stays_pending() {
    let (_tmp, root) = engine();
    let add = |provider: &str| {
        send(
            &root,
            json!({"model_choice": {"kind": "add", "harness": "rcli", "model": "my-model", "provider": provider}}),
        )
        .unwrap()
    };
    add("openrouter");
    let launch: Launch<'_> = Box::new(|_, provider| {
        assert_eq!(provider, Some("openrouter"));
        add("anthropic");
        Ok((Tested::Verified, None))
    });
    match tested(&root, "rcli", "my-model", Some(&pool()), None, launch).unwrap() {
        Reply::Tested { tested, reason, .. } => {
            assert_eq!(tested, Tested::Pending);
            assert_eq!(
                reason.as_deref(),
                Some("The id changed while it was tested; add it again to test it.")
            );
        }
        other => panic!("not tested: {other:?}"),
    }
    let db = versioned::open(&root).unwrap();
    assert_eq!(
        store::custom_provider(&db, "rcli", "my-model").unwrap(),
        None
    );
    assert_eq!(
        store::pending_provider(&db, "rcli", "my-model")
            .unwrap()
            .as_deref(),
        Some("anthropic")
    );
}
