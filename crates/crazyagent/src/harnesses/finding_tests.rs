use super::Search;
use super::finding::{
    self, PATH_AGENT_SPACE, PATH_INVALID, PATH_NOT_EXECUTABLE, PATH_NOT_FOUND, PATH_SHIM,
    TEMP_SKIPPED, UNKNOWN_HARNESS,
};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
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
        let space = finding::AgentSpace::open(&self.engine).unwrap();
        finding::check(id, path, &space, Some(&self.home)).map_err(|error| error.0)
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
    assert!(
        looked.contains(&"~/.volta/bin (shim, skipped)".to_owned()),
        "{looked:?}"
    );
    assert!(!looked.contains(&"~/.local/bin (shim, skipped)".to_owned()));
    let volta = case.home.join(".volta/bin");
    let on_path = Search::new(vec![volta], Some(case.home.clone()), Vec::new());
    let shown = finding::looked(&on_path, "codex");
    assert_eq!(
        shown
            .iter()
            .filter(|line| line.starts_with("~/.volta/bin"))
            .collect::<Vec<_>>(),
        ["~/.volta/bin (shim, skipped)"]
    );
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
    let open = case.file("open/node", 0o755);
    std::fs::set_permissions(
        open.parent().unwrap(),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let node = prefix.join("bin/node");
    std::fs::remove_file(&node).unwrap();
    std::os::unix::fs::symlink(&open, &node).unwrap();
    assert_eq!(super::node_prefix(&script), None);
    std::fs::remove_file(&node).unwrap();
    case.file("nvm/versions/node/v22.11.0/bin/node", 0o755);
    assert_eq!(super::node_prefix(&script), Some(prefix.clone()));
    std::fs::set_permissions(prefix.join("bin"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(super::node_prefix(&script), None);
}

#[cfg(target_os = "macos")]
#[test]
fn a_chosen_or_found_codex_where_agents_write_is_refused_as_agent_space() {
    let case = Case::new();
    let worktree = case.file(
        "projects/harbor/.crazyagent/worktrees/agent-3/bin/codex",
        0o755,
    );
    assert_eq!(case.check("codex", &worktree), Err(PATH_AGENT_SPACE.into()));
    let link = case.root.join("tools/codex");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&worktree, &link).unwrap();
    assert_eq!(case.check("codex", &link), Err(PATH_AGENT_SPACE.into()));
    let found = Search::new(
        vec![worktree.parent().unwrap().to_path_buf()],
        Some(case.home.clone()),
        Vec::new(),
    );
    let space = finding::AgentSpace::open(&case.engine).unwrap();
    assert_eq!(
        finding::pick("codex", &found, None, &space)
            .map(|picked| picked.map_err(|(path, error)| (path, error.0))),
        Some(Err((worktree.clone(), PATH_AGENT_SPACE.into())))
    );
    let project = case.file("projects/harbor/bin/codex", 0o755);
    let repository = case.file("repos/api/tools/codex", 0o755);
    let legacy = case.file("legacy/worktrees/agent-1/codex", 0o755);
    let leased = case.file("leases/task-9/codex", 0o755);
    let good = case.file("apps/codex", 0o755);
    let named = [&project, &repository, &legacy, &leased];
    for path in named.iter().copied().chain([&good]) {
        assert_eq!(case.check("codex", path), Ok(path.clone()));
    }
    let db = rusqlite::Connection::open(case.engine.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let text = |path: &Path| path.to_str().unwrap().to_owned();
    let setup =
        serde_json::json!({ "folder": text(&case.root.join("projects/harbor/.crazyagent")) });
    let declared = serde_json::json!({
        "project_id": "api",
        "spec": { "name": "Api", "repositories": [text(&case.root.join("repos/api"))] },
    });
    db.execute(
        "INSERT INTO project_setups(project, body) VALUES('harbor', ?1)",
        [setup.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO projects(id, body) VALUES('api', ?1)",
        [declared.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES(1,'old',?1,?2,'git')",
        [text(&case.root.join("legacy")), text(legacy.parent().unwrap())],
    )
    .unwrap();
    db.execute(
        "INSERT INTO worktree_leases VALUES('task-9','repo',?1,'{}')",
        [text(leased.parent().unwrap())],
    )
    .unwrap();
    for path in named {
        assert_eq!(
            case.check("codex", path),
            Err(PATH_AGENT_SPACE.into()),
            "{}",
            path.display()
        );
    }
    assert_eq!(case.check("codex", &good), Ok(good.clone()));
    let harbor = case.root.join("projects/harbor");
    let kept = db.unchecked_transaction().unwrap();
    for agent in 2..=(finding::AGENT_ROWS_MAX as i64 + 64) {
        let worktree = harbor.join(format!(".crazyagent/worktrees/agent-{agent}"));
        kept.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES(?1,'harbor',?2,?3,'git')",
            rusqlite::params![agent, text(&harbor), text(&worktree)],
        )
        .unwrap();
    }
    kept.commit().unwrap();
    assert_eq!(case.check("codex", &good), Ok(good.clone()));
    assert_eq!(case.check("codex", &legacy), Err(PATH_AGENT_SPACE.into()));
    finding::initialize(&db).unwrap();
    finding::set_chosen(&db, "codex", Some(&good)).unwrap();
    assert_eq!(
        finding::at_start(&case.engine, Some(&case.home)).codex,
        Some(good.clone())
    );
    db.execute(
        "INSERT INTO projects(id, body) VALUES('broken', 'not json')",
        [],
    )
    .unwrap();
    assert_eq!(
        finding::AgentSpace::open(&case.engine).map_err(|error| error.0),
        Err(finding::PATH_UNREADABLE.into())
    );
    assert_eq!(
        finding::at_start(&case.engine, Some(&case.home)).codex,
        None
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_node_prefix_in_an_agent_worktree_or_a_node_linked_into_one_gives_no_node_folder() {
    let case = Case::new();
    let agents = "projects/harbor/.crazyagent/worktrees/agent-3/nvm/versions/node/v22.11.0";
    let script = case.file(
        &format!("{agents}/lib/node_modules/@openai/codex/bin/codex.js"),
        0o755,
    );
    case.file(&format!("{agents}/bin/node"), 0o755);
    assert_eq!(super::node_prefix(&script), None);
    let owned = "nvm/versions/node/v22.11.0";
    let script = case.file(
        &format!("{owned}/lib/node_modules/@openai/codex/bin/codex.js"),
        0o755,
    );
    let planted = case.file("projects/harbor/.crazyagent/worktrees/agent-3/node", 0o755);
    let node = case.root.join(owned).join("bin/node");
    std::fs::create_dir_all(node.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&planted, &node).unwrap();
    assert_eq!(super::node_prefix(&script), None);
    std::fs::remove_file(&node).unwrap();
    case.file(&format!("{owned}/bin/node"), 0o755);
    assert_eq!(super::node_prefix(&script), Some(case.root.join(owned)));
}

#[cfg(target_os = "macos")]
#[test]
fn a_pin_takes_the_first_found_file_that_passes_the_chosen_file_checks_and_else_the_first_refusal()
{
    let case = Case::new();
    let volta = case.file("home/.volta/bin/codex", 0o755);
    let open = case.file("open/codex", 0o700);
    std::fs::set_permissions(
        open.parent().unwrap(),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let agent = case.file("engine/agent-1/bin/codex", 0o755);
    let good = case.file("tools/codex", 0o755);
    let search = |folders: &[&PathBuf]| {
        let folders = folders
            .iter()
            .map(|file| file.parent().unwrap().to_path_buf())
            .collect();
        Search::new(folders, Some(case.home.clone()), Vec::new())
    };
    let space = finding::AgentSpace::open(&case.engine).unwrap();
    let pick = |folders: &[&PathBuf]| {
        finding::pick("codex", &search(folders), None, &space)
            .map(|picked| picked.map_err(|(path, error)| (path, error.0)))
    };
    assert_eq!(
        pick(&[&volta]),
        Some(Err((volta.clone(), PATH_SHIM.into())))
    );
    assert_eq!(search(&[&volta]).find("codex"), None);
    assert_eq!(
        pick(&[&open]),
        Some(Err((open.clone(), finding::PATH_CHANGEABLE.into())))
    );
    assert_eq!(
        pick(&[&agent]),
        Some(Err((agent.clone(), PATH_AGENT_SPACE.into())))
    );
    assert_eq!(
        pick(&[&volta, &open, &agent]),
        Some(Err((volta.clone(), PATH_SHIM.into())))
    );
    assert_eq!(
        pick(&[&volta, &open, &agent, &good]),
        Some(Ok(good.clone()))
    );
    assert_eq!(pick(&[]), None);
    assert_eq!(
        finding::pick("aider", &search(&[&good]), None, &space),
        None
    );
}

#[test]
fn a_pipe_at_the_nvm_default_alias_never_holds_up_the_search() {
    let case = Case::new();
    let version = case.home.join(".nvm/versions/node/v22.11.0/bin");
    std::fs::create_dir_all(&version).unwrap();
    let alias = case.home.join(".nvm/alias/default");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&alias)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
    assert!(
        std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_fifo()
    );
    let (sent, searched) = std::sync::mpsc::channel();
    let home = case.home.clone();
    std::thread::spawn(move || {
        let search = Search::engine(None, &[], Some(home));
        sent.send(search.looked("codex")).unwrap();
    });
    let looked = searched
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the search waited on the pipe");
    assert!(looked.contains(&version), "{looked:?}");
}

#[test]
fn the_engine_search_finds_a_harness_in_the_engine_npm_prefix_after_the_owners_folders() {
    let case = Case::new();
    let bin = crate::setup::harness_prefix(&case.engine).join("bin");
    let bare = Search::engine(None, &[], None);
    let engine = bare.clone().with_engine(&case.engine);
    assert_eq!(
        engine.clone().with_engine(&case.engine),
        engine,
        "the folder is added once"
    );
    let relative = bin.strip_prefix(&case.root).unwrap().to_str().unwrap();
    let installed = case.file(&format!("{relative}/codex"), 0o755);
    assert_eq!(bare.find("codex"), None);
    assert_eq!(
        engine.find("codex"),
        Some(installed.clone()),
        "a prefix made after engine start is still looked in"
    );
    let owned = case.file("tools/codex", 0o755);
    let path = std::env::join_paths([owned.parent().unwrap()]).unwrap();
    let both = Search::engine_in(Some(&path), &[], None, &[]).with_engine(&case.engine);
    assert_eq!(both.find("codex"), Some(owned));
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        Search::engine(None, &[], None)
            .with_engine(&case.engine)
            .find("codex"),
        None,
        "a prefix others can change is left out"
    );
}

#[test]
fn an_owner_harness_in_a_home_folder_wins_over_the_one_the_engine_installed() {
    let case = Case::new();
    let bin = crate::setup::harness_prefix(&case.engine).join("bin");
    let relative = bin.strip_prefix(&case.root).unwrap().to_str().unwrap();
    let installed = case.file(&format!("{relative}/codex"), 0o755);
    let search = Search::engine(None, &[], Some(case.home.clone())).with_engine(&case.engine);
    assert_eq!(
        search.find("codex"),
        Some(installed.clone()),
        "the engine copy is the fallback"
    );
    let owned = case.file("home/.local/bin/codex", 0o755);
    assert_eq!(search.find("codex"), Some(owned.clone()));
    assert_eq!(search.candidates("codex"), [owned, installed]);
    let looked = search.looked("codex");
    let at = |folder: &Path| looked.iter().position(|seen| seen == folder).unwrap();
    assert!(at(&bin) > at(&case.home.join(".local/bin")));
}

#[test]
fn an_npm_harness_the_engine_installed_in_its_setup_folder_passes_and_other_engine_data_stays_refused()
 {
    let case = Case::new();
    let prefix = crate::setup::harness_prefix(&case.engine);
    let relative = prefix.strip_prefix(&case.root).unwrap().to_str().unwrap();
    let real = case.file(
        &format!("{relative}/lib/node_modules/@openai/codex/bin/codex.js"),
        0o755,
    );
    let entry = prefix.join("bin/codex");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &entry).unwrap();
    assert_eq!(case.check("codex", &entry), Ok(entry.clone()));
    assert_eq!(case.check("codex", &real), Ok(real));
    let agent = case.file("engine/team/agents/agent-7/home/codex", 0o755);
    assert_eq!(case.check("codex", &agent), Err(PATH_AGENT_SPACE.into()));
    let into = prefix.join("bin/claude");
    std::os::unix::fs::symlink(&agent, &into).unwrap();
    assert_eq!(
        case.check("claude-code", &into),
        Err(PATH_AGENT_SPACE.into())
    );
    let beside = case.file("engine/setup-other/codex", 0o755);
    assert_eq!(case.check("codex", &beside), Err(PATH_AGENT_SPACE.into()));
}

/// Writes a runnable file, its folders made first. Input: the file. Output: the file.
fn runnable(file: &Path) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file.to_path_buf()
}

/// V241-HARNESS-TEMP door 1 (DEC-157): the engine's search sets aside a PATH folder in a temp
/// folder and passes over a file in a kept folder whose real file lies in one, and where we
/// looked names each folder with the temp folder reason; an explicit search is unchanged.
#[test]
fn the_engine_search_sets_aside_temp_path_folders_and_names_them_where_it_looked() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let temps = vec![root.join("tmp")];
    let set_aside = root.join("tmp/bin");
    let kept = root.join("kept/bin");
    let codex = runnable(&set_aside.join("codex"));
    let real = runnable(&root.join("tmp/real/claude"));
    std::fs::create_dir_all(&kept).unwrap();
    std::os::unix::fs::symlink(&real, kept.join("claude")).unwrap();
    let path = std::env::join_paths([set_aside.clone(), kept.clone()]).unwrap();
    let search = Search::engine_in(Some(&path), &[], None, &temps);
    assert_eq!(search.find("codex"), None);
    assert!(search.candidates("codex").is_empty());
    assert_eq!(search.find("claude-code"), None);
    assert!(search.candidates("claude-code").is_empty());
    let codex_looked = finding::looked(&search, "codex");
    assert!(
        codex_looked.contains(&format!("{}{TEMP_SKIPPED}", set_aside.display())),
        "{codex_looked:?}"
    );
    let claude_looked = finding::looked(&search, "claude-code");
    assert!(
        claude_looked.contains(&format!("{}{TEMP_SKIPPED}", kept.display())),
        "{claude_looked:?}"
    );
    assert!(
        claude_looked
            .iter()
            .all(|line| !line.starts_with(set_aside.to_str().unwrap())),
        "a set-aside folder without the harness is not named: {claude_looked:?}"
    );
    let explicit = Search::new(vec![set_aside, kept.clone()], None, Vec::new());
    assert_eq!(explicit.find("codex"), Some(codex));
    assert_eq!(explicit.find("claude-code"), Some(kept.join("claude")));
}

/// V241-HARNESS-TEMP guard (DEC-157): a HOME place inside a temp folder, as a test HOME is,
/// is still looked in and gives its harness, also when the same folder is on PATH, and it is
/// never marked as skipped.
#[test]
fn a_home_place_inside_a_temp_folder_is_kept_and_not_marked_even_on_path() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let home = root.join("home");
    let codex = runnable(&home.join(".local/bin/codex"));
    let path = std::env::join_paths([home.join(".local/bin")]).unwrap();
    let search = Search::engine_in(Some(&path), &[], Some(home), &[root]);
    assert_eq!(search.find("codex"), Some(codex.clone()));
    assert_eq!(search.candidates("codex"), vec![codex]);
    let looked = finding::looked(&search, "codex");
    assert!(looked.contains(&"~/.local/bin".to_owned()), "{looked:?}");
    assert!(
        looked.iter().all(|line| !line.contains(TEMP_SKIPPED)),
        "{looked:?}"
    );
}

/// V241-HARNESS-TEMP r1 F1: a PATH folder in a temp folder that is also a HOME place keeps its
/// PATH position, so a test HOME's stand-in on PATH is found before the same harness in a later
/// PATH folder or a system folder such as Homebrew's, at the search and at the pin's entry.
#[test]
fn a_home_place_on_path_inside_a_temp_folder_keeps_its_path_position() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let home = root.join("home");
    let stand_in = runnable(&home.join(".local/bin/codex"));
    let later = runnable(&root.join("later/bin/codex"));
    let system = root.join("opt/homebrew/bin");
    runnable(&system.join("codex"));
    let path = std::env::join_paths([home.join(".local/bin"), root.join("later/bin")]).unwrap();
    let search = Search::engine_in(
        Some(&path),
        std::slice::from_ref(&system),
        Some(home.clone()),
        std::slice::from_ref(&home),
    );
    assert_eq!(search.find("codex"), Some(stand_in.clone()));
    let candidates = search.candidates("codex");
    assert_eq!(candidates.first(), Some(&stand_in), "{candidates:?}");
    assert!(candidates.contains(&later), "{candidates:?}");
    assert_eq!(
        crate::paths::harness_entry("codex", &search, Some(&path)),
        Some(stand_in)
    );
    let looked = finding::looked(&search, "codex");
    assert_eq!(
        looked.first().map(String::as_str),
        Some("~/.local/bin"),
        "{looked:?}"
    );
}

/// RV-DS241-61: a chosen harness file in the engine's own temp folder, which agents with the
/// owner's temp folder grant can write, is refused as a file agents can write, by its path as
/// given; the shared temp folders are already refused as folders other users can change.
#[cfg(target_os = "macos")]
#[test]
fn a_chosen_file_in_the_engines_own_temp_folder_is_refused_as_agent_space() {
    let case = Case::new();
    let owner_temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(case.root.starts_with(&owner_temp), "{:?}", case.root);
    let real = case.file("Applications/Codex/codex", 0o755);
    let space = finding::AgentSpace::open(&case.engine).unwrap();
    assert_eq!(
        finding::check("codex", &real, &space, Some(&case.home)).map_err(|error| error.0),
        Err(PATH_AGENT_SPACE.to_owned())
    );
}
