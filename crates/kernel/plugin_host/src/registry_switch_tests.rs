use super::*;

#[test]
fn bundled_plugins_turn_on_and_off_and_unknown_names_are_refused() {
    let case = case();
    set_enabled(&case.db, &case.engine(), "ollama", Switch::On).unwrap();
    set_enabled(&case.db, &case.engine(), "voyage", off()).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(plugin(&listing, "ollama").enabled);
    assert!(!plugin(&listing, "voyage").enabled);
    set_enabled(&case.db, &case.engine(), "ollama", off()).unwrap();
    assert!(!plugin(&list(&case.db, &case.engine()).unwrap(), "ollama").enabled);
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "nope", Switch::On)),
        "app_plugin_not_found"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "ollama")),
        "app_plugin_off"
    );
    assert_eq!(
        code(trust_file(
            &case.db,
            &case.engine(),
            "openai",
            &"0".repeat(64),
            1
        )),
        "app_plugin_bundled"
    );
}

#[test]
fn a_ready_plugin_loads_with_its_manifest_and_a_needs_key_or_off_one_is_refused() {
    let case = case();
    trusted(&case, NEAR);
    add_text(&case, ACME, false, 2).unwrap();
    set_enabled(&case.db, &case.engine(), "ollama", Switch::On).unwrap();
    let ollama = ready(&case.db, &case.engine(), "ollama").unwrap();
    assert_eq!(
        (ollama.manifest.name.as_str(), ollama.origin),
        ("ollama", Origin::Bundled)
    );
    assert!(ollama.bindings.is_empty());
    let near = ready(&case.db, &case.engine(), "near").unwrap();
    assert_eq!(
        (near.manifest.name.as_str(), near.origin),
        ("near", Origin::User)
    );
    assert!(near.bindings.is_empty());
    assert_eq!(
        code(ready(&case.db, &case.engine(), "acme")),
        "app_plugin_needs_key"
    );
    set_enabled(&case.db, &case.engine(), "near", off()).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "nope", Switch::On)),
        "app_plugin_not_found"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "missing")),
        "app_plugin_not_found"
    );
}

/// Turning a plugin off writes the off state first: the next call is refused with
/// `app_plugin_off`, while a run already holding the plugin keeps it, is not stopped under
/// the default setting, and stays listed until it ends.
#[test]
fn turning_off_refuses_the_next_call_and_lets_a_running_one_finish() {
    let case = case();
    trusted(&case, NEAR);
    let usage = Usage::default();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (loaded, lease) = hold(
        &case.db,
        &case.engine(),
        &usage,
        "near",
        holder("r1"),
        counted(&calls),
    )
    .unwrap();
    assert_eq!(loaded.manifest.name, "near");
    set_enabled(
        &case.db,
        &case.engine(),
        "near",
        Switch::Off {
            usage: &usage,
            when_off: TurnOff::RefuseNewCalls,
            essential_unlocked: false,
        },
    )
    .unwrap();
    assert_eq!(
        code(hold(
            &case.db,
            &case.engine(),
            &usage,
            "near",
            holder("r2"),
            counted(&calls)
        )),
        "app_plugin_off"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    assert_eq!(usage.users("near"), [holder("r1")]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(lease);
    assert!(usage.users("near").is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// With `stop_runs`, turning a plugin off stops each run holding it once, after the off
/// state is written; turning it off again stops none, and other plugins' runs go on.
#[test]
fn stop_runs_calls_each_holders_stop_once() {
    let case = case();
    trusted(&case, NEAR);
    add_text(
        &case,
        &NEAR.replace("plugin = \"near\"", "plugin = \"far\""),
        false,
        2,
    )
    .unwrap();
    let usage = Usage::default();
    let near_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let far_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held = |name: &str, run: &str, calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
        hold(
            &case.db,
            &case.engine(),
            &usage,
            name,
            holder(run),
            counted(calls),
        )
        .unwrap()
        .1
    };
    let _first = held("near", "r1", &near_calls);
    let _second = held("near", "r2", &near_calls);
    let _far = held("far", "r3", &far_calls);
    let stop_runs = Switch::Off {
        usage: &usage,
        when_off: TurnOff::StopRuns,
        essential_unlocked: false,
    };
    set_enabled(&case.db, &case.engine(), "near", stop_runs).unwrap();
    assert_eq!(near_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    set_enabled(&case.db, &case.engine(), "near", stop_runs).unwrap();
    assert_eq!(near_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(far_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(usage.users("far"), [holder("r3")]);
}

/// A trusted plugin whose plugin.toml changed is refused with `app_plugin_changed` from the
/// next call, and that change gives one notice naming the trusted and the current SHA-256:
/// a second call for the same change gives none, a further change gives a new one, and
/// approving it runs the plugin again.
#[test]
fn a_changed_plugin_notifies_once_per_change() {
    let case = case();
    trusted(&case, NEAR);
    let usage = Usage::default();
    let call = || {
        hold(
            &case.db,
            &case.engine(),
            &usage,
            "near",
            holder("r1"),
            Box::new(|| {}),
        )
    };
    drop(call().unwrap());
    assert!(usage.take_notices().is_empty());
    let path = case.root.join(FOLDER).join("near").join(MANIFEST_FILE);
    let first = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(&path, &first).unwrap();
    assert_eq!(code(call()), "app_plugin_changed");
    assert_eq!(code(call()), "app_plugin_changed");
    let notice = |current: &str| ChangeNotice {
        plugin: "near".to_owned(),
        trusted_sha256: sha(NEAR),
        current_sha256: sha(current),
    };
    assert_eq!(usage.take_notices(), [notice(&first)]);
    assert_eq!(code(call()), "app_plugin_changed");
    assert!(usage.take_notices().is_empty());
    let second = NEAR.replace("dimensions = 128", "dimensions = 512");
    std::fs::write(&path, &second).unwrap();
    assert_eq!(code(call()), "app_plugin_changed");
    assert_eq!(usage.take_notices(), [notice(&second)]);
    trust_file(&case.db, &case.engine(), "near", &sha(&second), 3).unwrap();
    drop(call().unwrap());
    assert!(usage.take_notices().is_empty());
    assert!(usage.users("near").is_empty());
}

/// Turning a bundled code plugin off with the setting `unlocked`. Input: whether the
/// essential lock is lifted. Output: the switch.
fn code_off(unlocked: bool) -> Switch<'static> {
    static IDLE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();
    Switch::Off {
        usage: IDLE.get_or_init(Usage::default),
        when_off: TurnOff::RefuseNewCalls,
        essential_unlocked: unlocked,
    }
}

/// An essential bundled code plugin cannot be turned off while the lock holds: the switch
/// is refused with `app_plugin_essential` and no off state is written.
#[test]
fn an_essential_plugin_cannot_be_turned_off_while_locked() {
    let case = case();
    assert_eq!(
        code(set_enabled(
            &case.db,
            &case.engine(),
            "agent-quit",
            code_off(false)
        )),
        "app_plugin_essential"
    );
    assert!(off_code_plugins(&case.db).unwrap().is_empty());
    assert!(cells(&case.db, "app_plugin_state").is_empty());
}

/// With the lock lifted an essential bundled code plugin turns off, is listed in the off
/// set under its plugin id, turns back on, and never shows as an app plugin.
#[test]
fn an_unlocked_essential_plugin_turns_off_and_back_on() {
    let case = case();
    set_enabled(&case.db, &case.engine(), "agent-quit", code_off(true)).unwrap();
    assert_eq!(
        off_code_plugins(&case.db)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["agent-quit"]
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|view| view.name != "agent-quit"));
    set_enabled(&case.db, &case.engine(), "agent-quit", Switch::On).unwrap();
    assert!(off_code_plugins(&case.db).unwrap().is_empty());
}

/// A switch row an older version left under a bundled code plugin's id, an unreviewed
/// drop-in turned off with no trust record, never turns that code plugin off: the off set
/// stays empty, the grants list it as on, and the next registry call drops the row. The
/// owner's own switch-off with the lock lifted still lands in the off set.
#[test]
fn an_old_drop_in_off_row_does_not_turn_off_a_code_plugin() {
    let case = case();
    case.db
        .execute(
            "INSERT INTO app_plugin_state (name, enabled) VALUES ('agent-quit', 0)",
            [],
        )
        .unwrap();
    let left = |case: &Case| -> i64 {
        case.db
            .query_row(
                "SELECT COUNT(*) FROM app_plugin_state WHERE name='agent-quit'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(left(&case), 1);
    assert!(off_code_plugins(&case.db).unwrap().is_empty());
    let grants = crate::grants::list(&case.db, &case.engine()).unwrap();
    let quit = grants
        .iter()
        .find(|grant| grant.plugin == "agent-quit")
        .unwrap();
    assert_eq!(quit.state, crate::grants::GrantState::On);
    list(&case.db, &case.engine()).unwrap();
    assert_eq!(left(&case), 0);
    assert!(off_code_plugins(&case.db).unwrap().is_empty());

    set_enabled(&case.db, &case.engine(), "agent-quit", code_off(true)).unwrap();
    assert_eq!(
        off_code_plugins(&case.db)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["agent-quit"]
    );
    assert_eq!(left(&case), 0, "the owner's switch is not a bare row");
    list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        off_code_plugins(&case.db).unwrap().len(),
        1,
        "the next registry call keeps the owner's switch"
    );
}

/// A user plugin named like a bundled code plugin is refused, so it never shares that
/// plugin's on and off state: adding it is `app_plugin_name_taken`, and one dropped into
/// the folder lists with that problem.
#[test]
fn a_user_plugin_named_like_a_code_plugin_is_refused() {
    let case = case();
    let named = ACME.replace("plugin = \"acme\"", "plugin = \"agent-quit\"");
    assert_ne!(named, ACME);
    assert_eq!(
        code(add_text(&case, &named, false, 1)),
        "app_plugin_name_taken"
    );
    drop_in(&case, "agent-quit", &named);
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        refused_code(plugin(&listing, "agent-quit")),
        "app_plugin_name_taken"
    );
    assert!(off_code_plugins(&case.db).unwrap().is_empty());
}
