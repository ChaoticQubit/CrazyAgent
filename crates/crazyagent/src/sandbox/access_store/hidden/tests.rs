use super::*;
use kernel_sandbox::hidden_paths::*;
use kernel_sandbox::paths::canonical;
use std::collections::BTreeMap;
use std::path::Path;

fn project() -> tempfile::TempDir {
    #[cfg(unix)]
    let parent = std::fs::canonicalize("/tmp").unwrap();
    #[cfg(not(unix))]
    let parent = canonical(&std::env::temp_dir()).unwrap();
    tempfile::Builder::new()
        .prefix("ca-hidden-")
        .tempdir_in(parent)
        .unwrap()
}

/// The check for one folder, as a list of one. Inputs: the project root and the folder.
/// Output: what [`any_tracked`] answers.
fn git_tracks(project_root: &Path, folder: &Path) -> Result<bool, &'static str> {
    any_tracked(
        &crate::sandbox::ACCESS_HOOKS,
        project_root,
        &[folder.to_path_buf()],
    )
}

/// Runs the test's own git in `path` with no system or global config, as the owner makes
/// a repository: `/usr/bin/git` on Unix, the git on PATH on Windows. Inputs: the folder and
/// the arguments.
fn git_in(path: &Path, args: &[&str]) {
    let program = if cfg!(unix) { "/usr/bin/git" } else { "git" };
    let mut command = std::process::Command::new(program);
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", crate::github::NULL_DEVICE)
        .current_dir(path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = kernel_processes::spawn_gate::status(&mut command).unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository with `secret/key.txt` tracked and `scratch/tmp.txt` not. Input: the
/// canonical project folder. Output: the tracked and the untracked folder.
fn tracked_and_untracked(path: &Path) -> (PathBuf, PathBuf) {
    git_in(path, &["init", "-q"]);
    let tracked = path.join("secret");
    let untracked = path.join("scratch");
    std::fs::create_dir(&tracked).unwrap();
    std::fs::create_dir(&untracked).unwrap();
    std::fs::write(tracked.join("key.txt"), "k\n").unwrap();
    std::fs::write(untracked.join("tmp.txt"), "t\n").unwrap();
    git_in(path, &["add", "secret/key.txt"]);
    (tracked, untracked)
}

/// V243-WIN-HIDDEN-GIT: a `commondir` planted in `.git`, naming a common folder whose config
/// holds an unknown repository extension, redirects nothing. On Unix git's common folder is
/// pinned to `.git`, so the tracked folder is still found; on Windows the check before git
/// starts refuses a `.git` that points elsewhere, so the folder is unchecked.
#[test]
fn a_commondir_planted_in_the_git_folder_redirects_nothing() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let (tracked, _) = tracked_and_untracked(&path);
    let store = project();
    for name in ["objects", "refs"] {
        std::fs::create_dir(store.path().join(name)).unwrap();
    }
    std::fs::write(
        store.path().join("config"),
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tplanted = true\n",
    )
    .unwrap();
    let common = format!("{}\n", canonical(store.path()).unwrap().display());
    std::fs::write(path.join(".git").join("commondir"), common).unwrap();
    let answer = git_tracks(&path, &tracked);
    if cfg!(windows) {
        assert_eq!(answer, Err("hidden_path_unchecked"));
    } else {
        assert_eq!(answer, Ok(true));
    }
}

/// V243-WIN-HIDDEN-GIT: a tracked folder two levels down is found, and so is the folder
/// holding it; on Windows, where folder names ignore case, the folder typed in another case
/// is found too, since git is asked about its canonical spelling joined by `/`.
#[test]
fn a_tracked_folder_inside_another_is_found_in_any_case_spelling_windows_accepts() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    git_in(&path, &["init", "-q"]);
    let nested = path.join("conf").join("secret");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("key.txt"), "k\n").unwrap();
    git_in(&path, &["add", "conf/secret/key.txt"]);
    assert_eq!(git_tracks(&path, &nested), Ok(true));
    assert_eq!(git_tracks(&path, &path.join("conf")), Ok(true));
    if cfg!(windows) {
        let typed = path.join("CONF").join("Secret");
        assert_eq!(git_tracks(&path, &typed), Ok(true));
    }
}

/// #590 (rv-sec3 F1): a folder renamed on disk to another case without git, so the index
/// still holds its old case, is found tracked; a folder whose name reads as a pattern
/// (`[s]ecret`) matches only itself, so the tracked `secret` beside it stays out of its
/// answer.
#[test]
fn a_folder_the_index_tracks_under_another_case_is_found_and_a_pattern_name_is_literal() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let (tracked, _) = tracked_and_untracked(&path);
    let pattern = path.join("[s]ecret");
    std::fs::create_dir(&pattern).unwrap();
    std::fs::write(pattern.join("tmp.txt"), "t\n").unwrap();
    assert_eq!(git_tracks(&path, &pattern), Ok(false));
    let renamed = path.join("Secret");
    std::fs::rename(&tracked, &renamed).unwrap();
    assert_eq!(git_tracks(&path, &renamed), Ok(true));
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[renamed]),
        Err("hidden_path_tracked")
    );
}

/// #590 (leader 10:34): git's `icase` folds ASCII letters only, while Windows and macOS
/// folder names also fold other letters, so there a folder path with any non-ASCII
/// character in a git project is refused as unchecked by name; with no repository, and on
/// Linux, where names match byte for byte, it is checked as before.
#[test]
fn a_non_ascii_folder_name_in_a_git_project_is_unchecked_where_names_fold_case() {
    let plain = project();
    let plain_path = canonical(plain.path()).unwrap();
    let loose = plain_path.join("clé");
    std::fs::create_dir(&loose).unwrap();
    assert_eq!(git_tracks(&plain_path, &loose), Ok(false));
    let root = project();
    let path = canonical(root.path()).unwrap();
    let _ = tracked_and_untracked(&path);
    let folder = path.join("clé");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("tmp.txt"), "t\n").unwrap();
    let wanted = if cfg!(any(windows, target_os = "macos")) {
        Err("hidden_path_name_unchecked")
    } else {
        Ok(false)
    };
    assert_eq!(git_tracks(&path, &folder), wanted);
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[folder]),
        wanted.map(|_| ())
    );
}

/// V243-WIN-HIDDEN-GIT (leader 09:21): a saved list of several folders costs one git run,
/// wherever the tracked folder stands in it, and an empty list runs no git at all.
#[test]
fn a_list_of_folders_is_checked_with_one_git_run() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let (tracked, untracked) = tracked_and_untracked(&path);
    let mut folders = vec![untracked];
    for name in ["plain-a", "plain-b", "plain-c"] {
        let folder = path.join(name);
        std::fs::create_dir(&folder).unwrap();
        folders.push(folder);
    }
    let runs = crate::project_files::owner_git_runs(|| {
        assert_eq!(
            none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &folders),
            Ok(())
        );
    });
    assert_eq!(runs, 1, "four untracked folders");
    folders.push(tracked);
    let runs = crate::project_files::owner_git_runs(|| {
        assert_eq!(
            none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &folders),
            Err("hidden_path_tracked")
        );
    });
    assert_eq!(runs, 1, "the tracked folder last of five");
    let runs = crate::project_files::owner_git_runs(|| {
        assert_eq!(
            none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[]),
            Ok(())
        );
    });
    assert_eq!(runs, 0, "no folder");
}

/// V243-WIN-HIDDEN-GIT (leader 08:33, 09:21): times `none_tracked` per call, as each launch
/// gate makes it, on one untracked folder and on a full list of untracked folders, 20 calls
/// each, and prints the median and the slowest of each. The farm runs it quiet and under a
/// CPU load step; it asserts only the answers.
#[test]
#[ignore = "timing, run by the farm quiet and under a CPU load step"]
fn none_tracked_time_per_call() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let (tracked, untracked) = tracked_and_untracked(&path);
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[tracked]),
        Err("hidden_path_tracked")
    );
    let mut full = vec![untracked.clone()];
    for n in 1..HIDDEN_PATHS_MAX {
        let folder = path.join(format!("plain-{n}"));
        std::fs::create_dir(&folder).unwrap();
        full.push(folder);
    }
    let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.0;
    for folders in [std::slice::from_ref(&untracked), &full[..]] {
        let mut times: Vec<std::time::Duration> = (0..20)
            .map(|_| {
                let start = std::time::Instant::now();
                assert_eq!(
                    none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, folders),
                    Ok(())
                );
                start.elapsed()
            })
            .collect();
        times.sort();
        println!(
            "TIMING none_tracked folders={} calls=20 median_ms={:.1} max_ms={:.1}",
            folders.len(),
            ms(times[10]),
            ms(times[19])
        );
    }
}

#[test]
fn a_folder_git_tracks_is_refused_and_an_untracked_one_is_accepted() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let git = |args: &[&str]| git_in(&path, args);
    git(&["init", "-q"]);
    let tracked = path.join("secret");
    let untracked = path.join("scratch");
    std::fs::create_dir(&tracked).unwrap();
    std::fs::create_dir(&untracked).unwrap();
    std::fs::write(tracked.join("key.txt"), "k\n").unwrap();
    std::fs::write(untracked.join("tmp.txt"), "t\n").unwrap();
    git(&["add", "secret/key.txt"]);
    assert_eq!(git_tracks(&path, &tracked), Ok(true));
    assert_eq!(git_tracks(&path, &untracked), Ok(false));
    assert_eq!(
        none_tracked(
            &crate::sandbox::ACCESS_HOOKS,
            &path,
            &[tracked.clone(), untracked.clone()]
        ),
        Err("hidden_path_tracked")
    );
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[untracked]),
        Ok(())
    );
}

#[test]
fn a_folder_with_no_repository_is_not_tracked() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let folder = path.join("plain");
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(git_tracks(&path, &folder), Ok(false));
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[folder]),
        Ok(())
    );
}

#[cfg(unix)]
#[test]
fn a_hostile_fsmonitor_command_never_runs_during_the_check() {
    let root = project();
    let path = root.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("/usr/bin/git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        assert!(
            kernel_processes::spawn_gate::status(&mut command)
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    let marker = path.join("pwned");
    let folder = path.join("secret");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("key.txt"), "k\n").unwrap();
    git(&["add", "secret/key.txt"]);
    let hostile = format!("touch {}", marker.display());
    std::fs::write(
        path.join(".git").join("config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = {hostile}\n\tfsmonitorHookVersion = 1\n"
        ),
    )
    .unwrap();
    let _ = git_tracks(&path, &folder);
    assert!(!marker.exists(), "fsmonitor command ran: marker written");
}

#[test]
fn a_corrupt_index_fails_the_check_closed() {
    let root = project();
    let path = canonical(root.path()).unwrap();
    let git = |args: &[&str]| git_in(&path, args);
    git(&["init", "-q"]);
    let folder = path.join("secret");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("key.txt"), "k\n").unwrap();
    git(&["add", "secret/key.txt"]);
    std::fs::write(path.join(".git").join("index"), b"not a git index\n").unwrap();
    assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[folder]),
        Err("hidden_path_unchecked")
    );
}

#[test]
fn a_dot_git_file_fails_the_check_closed() {
    let root = project();
    let path = root.path().canonicalize().unwrap();
    let folder = path.join("secret");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("key.txt"), "k\n").unwrap();
    std::fs::write(path.join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[folder]),
        Err("hidden_path_unchecked")
    );
}

#[cfg(unix)]
#[test]
fn a_folder_with_more_than_64_kib_of_tracked_names_is_still_tracked() {
    let root = project();
    let path = root.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("/usr/bin/git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        assert!(
            kernel_processes::spawn_gate::status(&mut command)
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    let folder = path.join("bulk");
    std::fs::create_dir(&folder).unwrap();
    let mut total = 0usize;
    let mut index = 0usize;
    while total <= CHECK_BYTES_MAX as usize {
        let name = format!("{index:06}-{}.txt", "x".repeat(200));
        std::fs::write(folder.join(&name), "t\n").unwrap();
        total += "bulk/".len() + name.len() + 1;
        index += 1;
    }
    git(&["add", "bulk"]);
    assert_eq!(git_tracks(&path, &folder), Ok(true));
    assert_eq!(
        none_tracked(&crate::sandbox::ACCESS_HOOKS, &path, &[folder]),
        Err("hidden_path_tracked")
    );
}

#[test]
fn saved_folders_read_back_in_order_and_are_empty_without_a_table() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
    let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
    save(&connection, "app", &paths).unwrap();
    assert_eq!(load(&connection, "app").unwrap(), paths);
    save(&connection, "app", &[]).unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
}

#[test]
fn listed_agents_keep_their_restart_ask_until_they_leave_the_list() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(seers(&connection, "app").unwrap().is_empty());
    assert_eq!(ask_restart(&connection, "app").unwrap(), 0, "none listed");
    let both = BTreeSet::from([3, 4]);
    set_seers(&connection, "app", &both).unwrap();
    set_seers(&connection, "web", &BTreeSet::from([9])).unwrap();
    assert_eq!(seers(&connection, "app").unwrap(), both);
    assert!(restarts(&connection, "app").unwrap().is_empty());
    assert_eq!(ask_restart(&connection, "app").unwrap(), 2);
    assert_eq!(restarts(&connection, "app").unwrap(), both);
    assert!(
        restarts(&connection, "web").unwrap().is_empty(),
        "one project only"
    );
    set_seers(&connection, "app", &BTreeSet::from([4, 5])).unwrap();
    assert_eq!(seers(&connection, "app").unwrap(), BTreeSet::from([4, 5]));
    assert_eq!(restarts(&connection, "app").unwrap(), BTreeSet::from([4]));
    set_seers(&connection, "app", &BTreeSet::new()).unwrap();
    assert!(seers(&connection, "app").unwrap().is_empty());
    assert_eq!(seers(&connection, "web").unwrap(), BTreeSet::from([9]));
}

/// SA-VID14-R2-N2: every source file that loads a project's saved hidden folders was
/// checked against the run list. Those that start an engine run hold its guard for each
/// kind; the rest are team agents (SA-VID14-A), the master turn bundle whose guard sits in
/// master_runtime.rs, or reads that feed no sandbox. A new loader fails until classified.
#[test]
fn every_loader_of_saved_hidden_folders_holds_the_guard_or_is_classified() {
    let guarded: BTreeMap<&str, &[&str]> = BTreeMap::from([
        (
            "landing/lockfiles.rs",
            &["RunKind::Lockfiles", "listed.hid(&hidden)"][..],
        ),
        (
            "onboarding/service.rs",
            &[
                "RunKind::CodeGraph",
                "listed.hid(&hidden)",
                "std::mem::forget(listed)",
            ][..],
        ),
        (
            "research/runner.rs",
            &["RunKind::Research", "run.kind", "listed.hid(&hidden)"][..],
        ),
        (
            "engine/owner/transport/master_runtime.rs",
            &[
                "RunKind::Master",
                "listed.hid(master_turn_hides(&prepared))",
                "if active.host.settled()",
            ][..],
        ),
    ]);
    let classified = BTreeSet::from([
        "demo.rs",
        "project_overview/agent.rs",
        "project_overview/run.rs",
        "research/jobs.rs",
        "roadmap_status/run.rs",
        "storage.rs",
        "storage/master_session.rs",
        "team/runtime.rs",
    ]);
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut folders = vec![src.clone()];
    let mut loaders = BTreeSet::new();
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name() != Some("tests".as_ref()) {
                    folders.push(path);
                }
                continue;
            }
            let name = path.file_name().unwrap().to_str().unwrap();
            if path.extension() != Some("rs".as_ref())
                || name == "tests.rs"
                || name.ends_with("_tests.rs")
            {
                continue;
            }
            let relative = path
                .strip_prefix(&src)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            if relative != "sandbox/access_store/hidden.rs"
                && std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("access_store::hidden::load(")
            {
                loaders.insert(relative);
            }
        }
    }
    assert!(
        loaders.len() >= classified.len(),
        "the walk read the sources"
    );
    for loader in &loaders {
        assert!(
            guarded.contains_key(loader.as_str()) || classified.contains(loader.as_str()),
            "{loader} loads saved hidden folders: hold the run list guard or classify it"
        );
    }
    for (file, kinds) in &guarded {
        let source = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(source.contains("hidden_paths::starting("), "{file}");
        for kind in *kinds {
            assert!(source.contains(kind), "{file} lists {kind}");
        }
        assert!(
            source.matches("std::mem::forget(listed)").count()
                >= source.matches("std::mem::forget(lease)").count(),
            "{file}: every exit that keeps a lease because its processes may still run keeps its run named"
        );
    }
}

/// A project that never saved hidden folders has none, also once the table exists because
/// another project saved some. Input: an empty database. Output: the read of each project.
#[test]
fn hidden_paths_default_is_empty() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
    let other = [PathBuf::from("/tmp/other/vault")];
    save(&connection, "other", &other).unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
    assert_eq!(load(&connection, "other").unwrap(), other);
}
