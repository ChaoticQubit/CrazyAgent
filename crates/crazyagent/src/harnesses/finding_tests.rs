use super::Search;
use super::finding::{
    self, PATH_AGENT_SPACE, PATH_INVALID, PATH_NOT_EXECUTABLE, PATH_NOT_FOUND, PATH_SHIM,
    UNKNOWN_HARNESS,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    engine: PathBuf,
}

impl Case {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let engine = root.join("engine");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&engine).unwrap();
        Self {
            _temp: temp,
            root,
            home,
            engine,
        }
    }

    fn file(&self, relative: &str, mode: u32) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn check(&self, id: &str, path: &Path) -> Result<PathBuf, String> {
        finding::check(id, path, &self.engine, Some(&self.home)).map_err(|error| error.0)
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_chosen_codex_file_that_passes_every_check_is_kept_with_its_link() {
    let case = Case::new();
    let real = case.file("Applications/Codex/codex-aarch64-apple-darwin", 0o755);
    let link = case.root.join("bin/codex");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(case.check("codex", &real), Ok(real.clone()));
    assert_eq!(case.check("codex", &link), Ok(link.clone()));
    assert_eq!(case.check("claude-code", &real), Ok(real));
}

#[test]
fn a_chosen_file_that_is_not_executable_is_refused() {
    let case = Case::new();
    let plain = case.file("tools/codex", 0o644);
    assert_eq!(case.check("codex", &plain), Err(PATH_NOT_EXECUTABLE.into()));
    let group_only = case.file("tools/codex-group", 0o610);
    assert_eq!(
        case.check("codex", &group_only),
        Err(PATH_NOT_EXECUTABLE.into())
    );
    let folder = case.root.join("tools");
    assert_eq!(
        case.check("codex", &folder),
        Err(PATH_NOT_EXECUTABLE.into())
    );
}

#[test]
fn a_chosen_path_that_is_relative_missing_too_long_or_odd_is_refused() {
    let case = Case::new();
    assert_eq!(
        case.check("codex", Path::new("bin/codex")),
        Err(PATH_INVALID.into())
    );
    assert_eq!(
        case.check("codex", &case.root.join("nowhere/codex")),
        Err(PATH_NOT_FOUND.into())
    );
    let long = PathBuf::from(format!("/{}", "a".repeat(finding::PATH_BYTES_MAX)));
    assert_eq!(case.check("codex", &long), Err(PATH_INVALID.into()));
    assert_eq!(
        case.check("codex", &case.root.join("bin/co\ndex")),
        Err(PATH_INVALID.into())
    );
    assert_eq!(
        case.check("codex", Path::new("/")),
        Err(PATH_INVALID.into())
    );
    let real = case.file("bin/codex", 0o755);
    assert_eq!(case.check("aider", &real), Err(UNKNOWN_HARNESS.into()));
    assert_eq!(case.check("claude", &real), Err(UNKNOWN_HARNESS.into()));
}

#[test]
fn a_chosen_version_manager_shim_is_refused() {
    let case = Case::new();
    let volta = case.file("home/.volta/bin/codex", 0o755);
    assert_eq!(case.check("codex", &volta), Err(PATH_SHIM.into()));
    let mise = case.file("home/.local/share/mise/shims/codex", 0o755);
    assert_eq!(case.check("codex", &mise), Err(PATH_SHIM.into()));
    let shim = case.file("tools/volta-shim", 0o755);
    let link = case.root.join("tools/codex");
    std::os::unix::fs::symlink(&shim, &link).unwrap();
    assert_eq!(case.check("codex", &link), Err(PATH_SHIM.into()));
}

#[test]
fn a_chosen_file_inside_the_engine_folder_or_an_agent_temp_folder_is_refused() {
    let case = Case::new();
    let agent = case.file("engine/team/agents/agent-7/home/codex", 0o755);
    assert_eq!(case.check("codex", &agent), Err(PATH_AGENT_SPACE.into()));
    let outside = case.file("tools/real-codex", 0o755);
    let into = case.root.join("engine/codex-link");
    std::os::unix::fs::symlink(&outside, &into).unwrap();
    assert_eq!(case.check("codex", &into), Err(PATH_AGENT_SPACE.into()));
    let pointing = case.root.join("tools/codex");
    std::os::unix::fs::symlink(&agent, &pointing).unwrap();
    assert_eq!(case.check("codex", &pointing), Err(PATH_AGENT_SPACE.into()));
    let tmp = if cfg!(target_os = "macos") {
        "/private/tmp/ca-0000000000000000/codex"
    } else {
        "/tmp/ca-0000000000000000/codex"
    };
    assert!(matches!(
        case.check("codex", Path::new(tmp)),
        Err(code) if code == PATH_NOT_FOUND || code == PATH_AGENT_SPACE
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn a_chosen_file_in_a_folder_other_users_can_change_is_refused() {
    let case = Case::new();
    let shared = case.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let codex = case.file("shared/codex", 0o755);
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        case.check("codex", &codex),
        Err(finding::PATH_CHANGEABLE.into())
    );
}

#[test]
fn the_chosen_file_is_stored_read_and_cleared_per_harness() {
    let case = Case::new();
    let db = rusqlite::Connection::open(case.engine.join("engine.db")).unwrap();
    finding::initialize(&db).unwrap();
    assert_eq!(finding::chosen(&db, "codex").unwrap(), None);
    let codex = case.file("tools/codex", 0o755);
    finding::set_chosen(&db, "codex", Some(&codex)).unwrap();
    assert_eq!(finding::chosen(&db, "codex").unwrap(), Some(codex.clone()));
    assert_eq!(finding::chosen(&db, "claude-code").unwrap(), None);
    let other = case.file("tools/codex-2", 0o755);
    finding::set_chosen(&db, "codex", Some(&other)).unwrap();
    assert_eq!(finding::chosen(&db, "codex").unwrap(), Some(other));
    finding::set_chosen(&db, "codex", None).unwrap();
    assert_eq!(finding::chosen(&db, "codex").unwrap(), None);
    assert_eq!(
        finding::set_chosen(&db, "aider", Some(&codex))
            .unwrap_err()
            .0,
        UNKNOWN_HARNESS
    );
    assert!(
        db.execute(
            "INSERT INTO harness_chosen_paths(id,path) VALUES('aider','/x/aider')",
            []
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn engine_start_reads_a_chosen_file_that_still_passes_and_drops_one_that_does_not() {
    let case = Case::new();
    let empty = finding::at_start(&case.engine, Some(&case.home));
    assert_eq!(empty, crate::paths::Harnesses::default());
    let db = rusqlite::Connection::open(case.engine.join("engine.db")).unwrap();
    let none = finding::at_start(&case.engine, Some(&case.home));
    assert_eq!(none, crate::paths::Harnesses::default());
    finding::initialize(&db).unwrap();
    let codex = case.file("tools/codex", 0o755);
    let claude = case.file("tools/claude", 0o755);
    finding::set_chosen(&db, "codex", Some(&codex)).unwrap();
    finding::set_chosen(&db, "claude-code", Some(&claude)).unwrap();
    let both = finding::at_start(&case.engine, Some(&case.home));
    assert_eq!(both.codex, Some(codex));
    assert_eq!(both.claude, Some(claude.clone()));
    assert_eq!(both.rcli, None);
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o644)).unwrap();
    let dropped = finding::at_start(&case.engine, Some(&case.home));
    assert_eq!(dropped.claude, None);
}

#[test]
fn where_we_looked_names_home_as_tilde_and_stays_bounded() {
    let case = Case::new();
    let many: Vec<PathBuf> = (0..200)
        .map(|index| case.root.join(format!("path-{index}")))
        .collect();
    let search = Search::new(many, Some(case.home.clone()), Vec::new());
    let looked = finding::looked(&search, "codex");
    assert!(looked.len() <= finding::LOOKED_MAX);
    assert!(
        looked
            .iter()
            .all(|line| line.len() <= finding::LOOKED_BYTES_MAX)
    );
    let search = Search::new(Vec::new(), Some(case.home.clone()), Vec::new());
    let looked = finding::looked(&search, "codex");
    assert!(looked.contains(&"~/.local/bin".to_owned()), "{looked:?}");
    assert!(looked.contains(&"~/.volta/bin".to_owned()), "{looked:?}");
    assert!(
        looked
            .iter()
            .all(|line| !line.contains(case.home.to_str().unwrap()))
    );
    assert!(finding::looked(&search, "claude").is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn the_node_folder_of_a_node_script_harness_is_its_npm_prefix_bin_holding_node() {
    let case = Case::new();
    let script = case.file(
        "nvm/versions/node/v22.11.0/lib/node_modules/@openai/codex/bin/codex.js",
        0o755,
    );
    let prefix = case.root.join("nvm/versions/node/v22.11.0");
    assert_eq!(super::node_prefix(&script), None);
    case.file("nvm/versions/node/v22.11.0/bin/node", 0o755);
    assert_eq!(super::node_prefix(&script), Some(prefix.clone()));
    let native = case.file("tools/codex", 0o755);
    assert_eq!(super::node_prefix(&native), None);
    std::fs::set_permissions(prefix.join("bin"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(super::node_prefix(&script), None);
}
