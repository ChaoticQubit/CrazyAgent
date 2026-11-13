use super::*;
use crate::harness_settings::Scope;
use crate::model_choice::store;
use crate::model_choice::{Models, Slot, Written, assert_catalog, catalog as shipped_ids};
use rusqlite::Connection;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Instant;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

/// A private temporary folder holding an engine root with its database and one project.
/// Output: the folder, kept alive by the caller, the engine root and the connection.
fn engine() -> (tempfile::TempDir, PathBuf, Connection) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-live-models-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    let db = Connection::open(root.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, "alpha", &folder);
    (tmp, root, db)
}

/// Writes one executable stand-in harness script. Inputs: the path and the script.
/// Output: the tool the resolver would give for it.
fn stand_in(path: &Path, script: &str) -> crate::tools::Resolved {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::tools::Resolved {
        entry: path.to_path_buf(),
        program: path.to_path_buf(),
        kind: crate::tools::Kind::Native,
    }
}

fn values(db: &Connection) -> Models {
    store::values(
        db,
        "codex",
        &store::catalog(db, "codex").unwrap(),
        &Scope::App,
        None,
    )
    .unwrap()
}

#[test]
fn codex_lists_only_the_models_its_bundled_catalog_shows() {
    let output = json!({"models": [
        {"slug": "gpt-a", "visibility": "list", "base_instructions": "long text"},
        {"slug": "gpt-hidden", "visibility": "hide"},
        {"slug": "gpt-b", "visibility": "list", "priority": 2},
        {"slug": "gpt-a", "visibility": "list"},
        {"slug": "not an id", "visibility": "list"},
        {"display_name": "no slug", "visibility": "list"}
    ]})
    .to_string();
    assert_eq!(
        parse("codex", Shape::CodexCatalog, output.as_bytes()),
        Some(ids(&["gpt-a", "gpt-b"]))
    );
    assert_eq!(
        parse("codex", Shape::CodexCatalog, br#"{"models": []}"#),
        None,
        "no model is no list"
    );
    assert_eq!(parse("codex", Shape::CodexCatalog, b"{\"models\": ["), None);
}

#[test]
fn a_text_list_is_kept_only_when_every_line_is_one_model_id() {
    assert_eq!(
        parse(
            "opencode",
            Shape::Lines,
            b"anthropic/claude-sonnet-5-5\nopenai/gpt-6-sol\n\n"
        ),
        Some(ids(&["anthropic/claude-sonnet-5-5", "openai/gpt-6-sol"]))
    );
    for text in [
        &b"Usage: crush models [search]\n"[..],
        b"Please sign in first.\n",
        b"model-a\nnot signed in\n",
        b"",
        b"\n\n",
        b"\xff\xfe\n",
    ] {
        assert_eq!(parse("crush", Shape::Lines, text), None, "{text:?}");
    }
}

#[test]
fn pi_lists_provider_and_model_from_its_table() {
    let table = b"provider   model              context  max-out  thinking  images\n\
anthropic  claude-sonnet-5-5  1M       64K      yes       yes\n\
openai     gpt-6-sol          400K     128K     yes       yes\n";
    assert_eq!(
        parse("pi", Shape::PiTable, table),
        Some(ids(&["anthropic/claude-sonnet-5-5", "openai/gpt-6-sol"]))
    );
    assert_eq!(
        parse(
            "pi",
            Shape::PiTable,
            b"No models available. Set an API key with pi auth.\n"
        ),
        None
    );
}

#[test]
fn aider_lists_the_dashed_lines_under_its_banner() {
    let output = b"Aider v0.86.0\nModels which match \"\":\n- gpt-4o\n- anthropic/claude-3-5\n";
    assert_eq!(
        parse("aider", Shape::Dashed, output),
        Some(ids(&["gpt-4o", "anthropic/claude-3-5"]))
    );
    assert_eq!(
        parse(
            "aider",
            Shape::Dashed,
            b"Aider v0.86.0\nNo models match \"\".\n"
        ),
        None
    );
}

#[test]
fn json_ids_come_from_strings_or_an_id_field() {
    let array = json!([{"id": "a-1"}, {"model_id": "b-2"}, "c-3", {"modelId": "d-4"}, {"slug": "e-5"}, {"name": "no id"}, 7]);
    assert_eq!(
        parse("kiro", Shape::JsonIds, array.to_string().as_bytes()),
        Some(ids(&["a-1", "b-2", "c-3", "d-4", "e-5"]))
    );
    let object = json!({"models": [{"id": "f-6"}]});
    assert_eq!(
        parse("auggie", Shape::JsonIds, object.to_string().as_bytes()),
        Some(ids(&["f-6"]))
    );
    assert_eq!(parse("auggie", Shape::JsonIds, b"not json"), None);
    assert_eq!(parse("auggie", Shape::JsonIds, b"{\"models\": []}"), None);
}

#[test]
fn a_reported_list_is_distinct_and_cut_at_the_catalog_bound_so_it_never_trips_the_store() {
    let mut text = String::new();
    for n in 0..100 {
        text.push_str(&format!("model-{n}\nmodel-{n}\n"));
    }
    let read = parse("opencode", Shape::Lines, text.as_bytes()).unwrap();
    assert_eq!(read.len(), crate::model_choice::CATALOG_MODELS_MAX);
    assert_eq!(read[0], "model-0");
    assert_catalog("opencode", &read);
    let long = format!("{}\n", "m".repeat(crate::model_choice::MODEL_BYTES_MAX + 1));
    assert_eq!(parse("opencode", Shape::Lines, long.as_bytes()), None);
}

#[test]
fn the_live_list_follows_the_shipped_list_and_drops_nothing_shipped() {
    let shipped = ids(&["s-1", "s-2"]);
    assert_eq!(
        merged("codex", &shipped, &ids(&["l-1", "s-2", "l-2"])),
        ids(&["s-1", "s-2", "l-1", "l-2"])
    );
    assert_eq!(merged("codex", &shipped, &[]), shipped);
    let many: Vec<String> = (0..60).map(|n| format!("s-{n}")).collect();
    let more: Vec<String> = (0..10).map(|n| format!("l-{n}")).collect();
    let joined = merged("codex", &many, &more);
    assert_eq!(joined.len(), crate::model_choice::CATALOG_MODELS_MAX);
    assert_eq!(joined[..60], many[..]);
    assert_eq!(joined[60..], more[..4]);
    assert_catalog("codex", &joined);
}

#[test]
fn every_harness_with_a_list_command_has_a_reader_and_the_rest_say_so() {
    for harness in [
        "codex",
        "opencode",
        "openclaw",
        "crush",
        "cursor-agent",
        "antigravity",
        "gemini-cli",
        "pi",
        "aider",
        "kiro",
        "auggie",
    ] {
        assert!(reader(harness).is_some(), "{harness}");
    }
    for harness in ["claude-code", "rcli", "amp", "goose", "copilot", "kimi"] {
        assert!(reader(harness).is_none(), "{harness}");
    }
    assert_eq!(reader("claude"), None, "an alias names the same harness");
    assert_eq!(
        not_reported("claude-code"),
        "live list not reported by Claude Code"
    );
    assert_eq!(not_reported("codex"), "live list not reported by Codex");
}

#[test]
fn a_read_runs_the_resolved_harness_signed_out_in_a_private_home_it_then_removes() {
    let (_tmp, root, _db) = engine();
    let report = root.join("report");
    let codex = stand_in(
        &root.join("codex"),
        &format!(
            "#!/bin/sh\n{{ echo \"args=$*\"; echo \"home=$HOME\"; echo \"codex=$CODEX_HOME\"; echo \"claude=$CLAUDE_CONFIG_DIR\"; echo \"tmp=$TMPDIR\"; echo \"pwd=$(pwd -P)\"; echo \"path=$PATH\"; ls -ld \"$HOME\"; env | cut -d= -f1 | sort; }} > '{}'\necho '{{\"models\": [{{\"slug\": \"gpt-a\", \"visibility\": \"list\"}}]}}'\n",
            report.display()
        ),
    );
    assert_eq!(
        read_with("codex", &codex, &root, Duration::from_secs(60)),
        Some(ids(&["gpt-a"]))
    );
    let seen = std::fs::read_to_string(&report).unwrap();
    let field = |name: &str| -> String {
        seen.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{name} missing: {seen}"))
            .to_owned()
    };
    assert_eq!(field("args"), "debug models --bundled");
    let home = PathBuf::from(field("home"));
    assert!(
        home.starts_with(root.join("model-lists")),
        "the home is the read's own: {seen}"
    );
    for name in ["codex", "claude", "tmp", "pwd"] {
        assert_eq!(PathBuf::from(field(name)), home, "{name}: {seen}");
    }
    assert_eq!(field("path"), "/usr/bin:/bin");
    assert!(
        seen.lines().any(|line| line.starts_with("drwx------")),
        "the home is private: {seen}"
    );
    let allowed = [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "TMPDIR",
        "PATH",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ];
    let names: Vec<&str> = seen
        .lines()
        .filter(|line| !line.contains('=') && !line.starts_with('d'))
        .collect();
    assert!(
        names
            .iter()
            .all(|name| allowed.contains(name) || name.starts_with("__CF")),
        "only the read's own variables reach the harness: {names:?}"
    );
    assert!(!home.exists(), "the home is removed after the read");
}

#[test]
fn a_read_that_fails_runs_past_its_limit_or_has_no_reader_reports_nothing() {
    let (_tmp, root, _db) = engine();
    let failing = stand_in(&root.join("fails"), "#!/bin/sh\necho gpt-a\nexit 3\n");
    assert_eq!(read_with("opencode", &failing, &root, LIMIT), None);
    let hanging = stand_in(&root.join("hangs"), "#!/bin/sh\nexec /bin/sleep 30\n");
    let started = Instant::now();
    assert_eq!(
        read_with("opencode", &hanging, &root, Duration::from_millis(300)),
        None
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "killed at the limit"
    );
    let marker = root.join("ran");
    let claude = stand_in(
        &root.join("claude"),
        &format!("#!/bin/sh\ntouch '{}'\necho opus\n", marker.display()),
    );
    assert_eq!(read_with("claude-code", &claude, &root, LIMIT), None);
    assert!(
        !marker.exists(),
        "a harness with no reader is never started"
    );
    let homes = root.join("model-lists");
    assert!(
        !homes.exists() || std::fs::read_dir(&homes).unwrap().next().is_none(),
        "no private home is left behind"
    );
}

#[test]
fn a_saved_live_model_survives_a_shorter_read_a_failed_read_and_a_restart() {
    let (_tmp, root, db) = engine();
    let shipped = shipped_ids("codex");
    assert_eq!(store::catalog(&db, "codex").unwrap(), shipped);
    store::live_record(&db, "codex", Some(&ids(&["live-a", "live-b"]))).unwrap();
    let mut both = shipped.clone();
    both.extend(ids(&["live-a", "live-b"]));
    assert_eq!(store::catalog(&db, "codex").unwrap(), both);
    let models = values(&db);
    let master = models
        .rows
        .iter()
        .find(|row| row.slot == Slot::Master)
        .unwrap()
        .version;
    let written = store::set(
        &db,
        "codex",
        &store::catalog(&db, "codex").unwrap(),
        &Scope::App,
        Slot::Master,
        &json!("live-b"),
        master,
    )
    .unwrap();
    assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    store::live_record(&db, "codex", Some(&ids(&["live-c"]))).unwrap();
    let mut kept = shipped.clone();
    kept.extend(ids(&["live-c", "live-b"]));
    assert_eq!(
        store::catalog(&db, "codex").unwrap(),
        kept,
        "a shorter read drops live-a but keeps live-b, which the master slot names"
    );
    store::live_record(&db, "codex", None).unwrap();
    assert_eq!(
        store::catalog(&db, "codex").unwrap(),
        kept,
        "a failed read keeps the list"
    );
    drop(db);
    let db = Connection::open(root.join("engine.db")).unwrap();
    assert_eq!(
        store::catalog(&db, "codex").unwrap(),
        kept,
        "after a restart"
    );
    assert_eq!(
        store::launch_model(
            &db,
            "codex",
            &store::catalog(&db, "codex").unwrap(),
            None,
            Slot::Master
        )
        .unwrap()
        .as_deref(),
        Some("live-b"),
        "the picked live model still launches"
    );
}

#[test]
fn a_saved_live_model_past_the_bound_stays_when_a_new_id_comes_ahead_of_it() {
    let (_tmp, _root, db) = engine();
    let room = crate::model_choice::CATALOG_MODELS_MAX - shipped_ids("codex").len();
    let listed: Vec<String> = (0..room + 5).map(|n| format!("live-{n:03}")).collect();
    store::live_record(&db, "codex", Some(&listed)).unwrap();
    let last = store::catalog(&db, "codex")
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last, listed[room - 1]);
    let master = values(&db)
        .rows
        .iter()
        .find(|row| row.slot == Slot::Master)
        .unwrap()
        .version;
    let written = store::set(
        &db,
        "codex",
        &store::catalog(&db, "codex").unwrap(),
        &Scope::App,
        Slot::Master,
        &json!(last),
        master,
    )
    .unwrap();
    assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    let mut ahead = ids(&["live-new"]);
    ahead.extend(listed.iter().cloned());
    store::live_record(&db, "codex", Some(&ahead)).unwrap();
    let catalog = store::catalog(&db, "codex").unwrap();
    assert!(
        catalog.contains(&last),
        "the saved id stays though the new list pushes it past the bound: {catalog:?}"
    );
    assert!(catalog.contains(&"live-new".to_owned()));
    assert_catalog("codex", &catalog);
    assert_eq!(
        store::launch_model(&db, "codex", &catalog, None, Slot::Master)
            .unwrap()
            .as_deref(),
        Some(last.as_str())
    );
}

#[test]
fn the_section_says_live_list_not_reported_until_a_list_is_reported() {
    let (_tmp, _root, db) = engine();
    assert_eq!(
        values(&db).live_note.as_deref(),
        Some("live list not reported by Codex"),
        "never read"
    );
    store::live_record(&db, "codex", Some(&ids(&["live-a"]))).unwrap();
    assert_eq!(values(&db).live_note, None);
    store::live_record(&db, "codex", None).unwrap();
    assert_eq!(
        values(&db).live_note.as_deref(),
        Some("live list not reported by Codex"),
        "the last read failed"
    );
    let claude = store::values(
        &db,
        "claude-code",
        &store::catalog(&db, "claude-code").unwrap(),
        &Scope::App,
        None,
    )
    .unwrap();
    assert_eq!(
        claude.live_note.as_deref(),
        Some("live list not reported by Claude Code"),
        "a harness with no reader"
    );
    assert!(!claude.candidates.is_empty(), "the shipped list stays");
}
