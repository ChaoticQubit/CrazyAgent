use super::*;
use crate::app_plugins::store;
use std::os::unix::fs::PermissionsExt;

const NEAR: &str = "schema: 1
kind: embedding
name: near
title: Near
version: 1
http:
  url: http://127.0.0.1:9000/embed
  hosts: [127.0.0.1]
  body:
    input: \"{{inputs}}\"
  vectors: embeddings.*
  dimensions: 128
";

struct Case {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    db: Connection,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    Case {
        _temp: temp,
        root,
        db: Connection::open_in_memory().unwrap(),
    }
}

fn option<'a>(search: &'a SearchView, name: &str) -> Option<&'a SearchOption> {
    search.options.iter().find(|option| option.name == name)
}

fn saved(result: Result<Result<SearchView, u64>, EngineError>) -> SearchView {
    result.unwrap().expect("saved")
}

fn plugin(name: &str) -> SearchChoice {
    SearchChoice::Plugin { name: name.into() }
}

#[test]
fn search_starts_on_the_local_plugin_and_lists_only_enabled_plugins() {
    let case = case();
    let search = view(&case.db, &case.root).unwrap();
    assert_eq!((search.version, &search.choice), (0, &plugin("local")));
    let local = option(&search, "local").unwrap();
    assert!(local.sends.is_empty());
    assert!(matches!(
        local.runs,
        Some(crate::app_plugins::RunsView::Local { .. })
    ));
    assert!(option(&search, "openai").is_none());
    assert!(option(&search, "ollama").is_none());
    assert_eq!(search.model, crate::semantic::local::state(&case.root));
    if !matches!(search.model, ModelState::Ready { .. }) {
        assert!(!local.usable);
        assert!(local.reason.is_some());
    }
    assert!(chosen(&case.db, &case.root).is_some_and(|loaded| loaded.manifest.name == "local"));
}

#[test]
fn a_save_needs_the_current_version_and_full_text_stops_vectors_at_once() {
    let case = case();
    let search = saved(set(&case.db, &case.root, &SearchChoice::FullText, None, 0));
    assert_eq!(
        (search.version, &search.choice),
        (1, &SearchChoice::FullText)
    );
    assert!(chosen(&case.db, &case.root).is_none());
    assert_eq!(
        set(&case.db, &case.root, &plugin("local"), None, 0).unwrap(),
        Err(1)
    );
    let search = saved(set(&case.db, &case.root, &plugin("local"), None, 1));
    assert_eq!((search.version, &search.choice), (2, &plugin("local")));
}

#[test]
fn an_http_plugin_needs_a_confirm_naming_its_host_and_lists_what_leaves_the_mac() {
    let case = case();
    store::set_enabled(&case.db, &case.root, "ollama", true).unwrap();
    let search = view(&case.db, &case.root).unwrap();
    let ollama = option(&search, "ollama").unwrap();
    assert!(ollama.usable);
    assert_eq!(ollama.sends, SENDS);
    assert_eq!(
        set(&case.db, &case.root, &plugin("ollama"), None, 0)
            .unwrap_err()
            .0,
        "search_confirm_required"
    );
    assert_eq!(
        set(
            &case.db,
            &case.root,
            &plugin("ollama"),
            Some("api.openai.com"),
            0
        )
        .unwrap_err()
        .0,
        "search_confirm_required"
    );
    assert_eq!(view(&case.db, &case.root).unwrap().choice, plugin("local"));
    let search = saved(set(
        &case.db,
        &case.root,
        &plugin("ollama"),
        Some("127.0.0.1"),
        0,
    ));
    assert_eq!(search.choice, plugin("ollama"));
    assert!(chosen(&case.db, &case.root).is_some_and(|loaded| loaded.manifest.name == "ollama"));
}

#[test]
fn a_plugin_that_cannot_run_is_not_chosen() {
    let case = case();
    for (name, host) in [
        ("openai", "api.openai.com"),
        ("voyage", "api.voyageai.com"),
        ("missing", "x.example"),
    ] {
        assert_eq!(
            set(&case.db, &case.root, &plugin(name), Some(host), 0)
                .unwrap_err()
                .0,
            "search_choice_unavailable",
            "{name}"
        );
    }
    store::set_enabled(&case.db, &case.root, "openai", true).unwrap();
    let search = view(&case.db, &case.root).unwrap();
    let openai = option(&search, "openai").unwrap();
    assert!(!openai.usable);
    assert_eq!(
        openai.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("app_plugin_needs_key", None, None)
                .message
                .as_str()
        )
    );
    assert_eq!(
        set(
            &case.db,
            &case.root,
            &plugin("openai"),
            Some("api.openai.com"),
            0
        )
        .unwrap_err()
        .0,
        "search_choice_unavailable"
    );
}

#[test]
fn turning_the_chosen_plugin_off_stops_it_and_keeps_it_listed_with_the_reason() {
    let case = case();
    store::set_enabled(&case.db, &case.root, "ollama", true).unwrap();
    saved(set(
        &case.db,
        &case.root,
        &plugin("ollama"),
        Some("127.0.0.1"),
        0,
    ));
    store::set_enabled(&case.db, &case.root, "ollama", false).unwrap();
    assert!(chosen(&case.db, &case.root).is_none());
    let search = view(&case.db, &case.root).unwrap();
    assert_eq!(search.choice, plugin("ollama"));
    let ollama = option(&search, "ollama").unwrap();
    assert!(!ollama.usable);
    assert_eq!(
        ollama.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("app_plugin_off", None, None)
                .message
                .as_str()
        )
    );
}

#[test]
fn a_chosen_plugin_moved_to_another_host_stops_until_confirmed_again() {
    let case = case();
    let sha = crate::semantic::sha256_hex(NEAR.as_bytes());
    store::add(&case.db, &case.root, NEAR, &sha, false, 1).unwrap();
    saved(set(
        &case.db,
        &case.root,
        &plugin("near"),
        Some("127.0.0.1"),
        0,
    ));
    assert!(chosen(&case.db, &case.root).is_some());
    let moved = NEAR
        .replace("http://127.0.0.1:9000/embed", "http://localhost:9000/embed")
        .replace("[127.0.0.1]", "[localhost]");
    let moved_sha = crate::semantic::sha256_hex(moved.as_bytes());
    store::add(&case.db, &case.root, &moved, &moved_sha, true, 2).unwrap();
    assert!(chosen(&case.db, &case.root).is_none());
    let search = view(&case.db, &case.root).unwrap();
    let near = option(&search, "near").unwrap();
    assert!(!near.usable);
    assert_eq!(
        near.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("search_confirm_required", None, None)
                .message
                .as_str()
        )
    );
    saved(set(
        &case.db,
        &case.root,
        &plugin("near"),
        Some("localhost"),
        1,
    ));
    assert!(chosen(&case.db, &case.root).is_some());
}

#[test]
fn a_chosen_plugin_that_is_gone_or_refused_stays_listed_with_its_reason() {
    let case = case();
    let sha = crate::semantic::sha256_hex(NEAR.as_bytes());
    store::add(&case.db, &case.root, NEAR, &sha, false, 1).unwrap();
    saved(set(
        &case.db,
        &case.root,
        &plugin("near"),
        Some("127.0.0.1"),
        0,
    ));
    let path = case.root.join(crate::app_plugins::FOLDER).join("near.yaml");
    std::fs::write(&path, "a: 1\n\tb: 2\n").unwrap();
    assert!(chosen(&case.db, &case.root).is_none());
    let search = view(&case.db, &case.root).unwrap();
    assert_eq!(search.choice, plugin("near"));
    let near = option(&search, "near").unwrap();
    assert_eq!((near.usable, &near.runs), (false, &None));
    assert_eq!(
        near.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("app_plugin_refused", None, None)
                .message
                .as_str()
        )
    );
    std::fs::remove_file(&path).unwrap();
    let search = view(&case.db, &case.root).unwrap();
    let near = option(&search, "near").unwrap();
    assert!(!near.usable);
    assert_eq!(
        near.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("app_plugin_not_found", None, None)
                .message
                .as_str()
        )
    );
}

#[test]
fn a_confirm_belongs_to_the_reviewed_file_so_a_same_name_replacement_needs_a_new_one() {
    let case = case();
    let sha = crate::semantic::sha256_hex(NEAR.as_bytes());
    store::add(&case.db, &case.root, NEAR, &sha, false, 1).unwrap();
    saved(set(
        &case.db,
        &case.root,
        &plugin("near"),
        Some("127.0.0.1"),
        0,
    ));
    let path = case.root.join(crate::app_plugins::FOLDER).join("near.yaml");
    let other = NEAR.replace("title: Near", "title: Near again");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, &other).unwrap();
    let other_sha = crate::semantic::sha256_hex(other.as_bytes());
    store::trust_file(&case.db, &case.root, "near", &other_sha, 2).unwrap();
    assert!(chosen(&case.db, &case.root).is_none());
    let search = view(&case.db, &case.root).unwrap();
    let near = option(&search, "near").unwrap();
    assert_eq!(
        near.reason.as_deref(),
        Some(
            crate::app_plugins::Problem::new("search_confirm_required", None, None)
                .message
                .as_str()
        )
    );
    saved(set(
        &case.db,
        &case.root,
        &plugin("near"),
        Some("127.0.0.1"),
        1,
    ));
    assert!(chosen(&case.db, &case.root).is_some());
    assert!(view(&case.db, &case.root).unwrap().note.is_none());
}
