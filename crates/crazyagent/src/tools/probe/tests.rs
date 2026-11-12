use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

/// A scratch folder, its real path, so kept folders compare equal.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tools-probe-")
        .tempdir()
        .unwrap();
    let real = folder.path().canonicalize().unwrap();
    (folder, real)
}

/// Writes an executable file with the given text, its folders made first.
fn program(file: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file.to_path_buf()
}

fn joined(folders: &[PathBuf]) -> OsString {
    std::env::join_paths(folders).unwrap()
}

#[test]
fn a_timed_run_keeps_its_output_and_kills_its_whole_group_at_the_limit_or_at_exit() {
    let (_folder, root) = scratch();
    let echo = run_timed(Command::new("/bin/echo").arg("hi"), Duration::from_secs(5));
    assert_eq!(echo.as_deref(), Some(&b"hi\n"[..]));
    let failing = program(&root.join("fails"), "#!/bin/sh\necho out\nexit 3\n");
    assert_eq!(
        run_timed(&mut Command::new(&failing), Duration::from_secs(5)),
        None
    );
    let hanging = program(&root.join("hangs"), "#!/bin/sh\nexec /bin/sleep 30\n");
    let started = Instant::now();
    assert_eq!(
        run_timed(&mut Command::new(&hanging), Duration::from_millis(300)),
        None
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "killed at the limit"
    );
    let leaves_child = program(
        &root.join("leaves"),
        "#!/bin/sh\n/bin/sleep 30 &\necho done\n",
    );
    let started = Instant::now();
    assert_eq!(
        run_timed(&mut Command::new(&leaves_child), Duration::from_secs(10)).as_deref(),
        Some(&b"done\n"[..]),
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a child left holding the output is killed with the group"
    );
}

#[test]
fn the_login_shell_path_is_read_between_markers_and_startup_noise_is_ignored() {
    let output = format!("motd\n{BEGIN}\n/a/bin:/b/bin\n{END}\nbye\n");
    assert_eq!(
        between_markers(output.as_bytes()),
        Some(OsString::from("/a/bin:/b/bin"))
    );
    assert_eq!(between_markers(format!("{BEGIN}\n/a\n").as_bytes()), None);
    assert_eq!(between_markers(b"/a:/b\n"), None);
    assert_eq!(
        between_markers(format!("{BEGIN}\n/a\n/b\n{END}\n").as_bytes()),
        None,
        "one line only"
    );
}

#[test]
fn a_login_shell_run_gives_the_path_its_startup_files_set_and_a_hang_gives_none() {
    let (_folder, home) = scratch();
    let tools = home.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(
        home.join(".profile"),
        "echo noise\nPATH=\"$HOME/tools:$PATH\"\nexport PATH\n",
    )
    .unwrap();
    let path = login_path(Path::new("/bin/sh"), &home, "owner", SHELL_LIMIT)
        .expect("the shell printed its PATH");
    assert!(
        std::env::split_paths(&path).any(|folder| folder == tools),
        "{path:?}"
    );
    let hanging = program(&home.join("hangs"), "#!/bin/sh\nexec /bin/sleep 30\n");
    let started = Instant::now();
    assert_eq!(
        login_path(&hanging, &home, "owner", Duration::from_millis(300)),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn kept_folders_drop_relative_missing_repeated_shared_crazyagent_and_project_folders() {
    let (_folder, home) = scratch();
    let tools = home.join("tools");
    let project = home.join("projects/app");
    let shared = home.join("shared");
    let inside = home.join("x/.crazyagent/bin");
    for folder in [&tools, &project.join(".crazyagent"), &shared, &inside] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::create_dir_all(project.join("node_modules/.bin")).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let file = home.join("plain-file");
    std::fs::write(&file, "x").unwrap();
    let path = joined(&[
        tools.clone(),
        PathBuf::from("relative/bin"),
        home.join("missing"),
        tools.clone(),
        project.join("node_modules/.bin"),
        project.clone(),
        inside,
        shared,
        file,
    ]);
    assert_eq!(kept(&path, Some(&home)), [tools]);
    assert_eq!(kept(OsStr::new(""), Some(&home)), Vec::<PathBuf>::new());
    std::fs::create_dir_all(home.join(".crazyagent")).unwrap();
    assert_eq!(
        kept(&joined(&[home.join("tools")]), Some(&home)),
        [home.join("tools")],
        "a .crazyagent folder in HOME itself makes no project of HOME"
    );
    let many: Vec<PathBuf> = (0..FOLDERS_MAX + 10)
        .map(|index| {
            let folder = home.join(format!("many/{index}"));
            std::fs::create_dir_all(&folder).unwrap();
            folder
        })
        .collect();
    assert_eq!(kept(&joined(&many), Some(&home)).len(), FOLDERS_MAX);
}

#[test]
fn the_npm_prefix_bin_comes_from_the_found_npm_asked_with_a_cleared_environment() {
    assert_eq!(
        prefix_bin(b"/opt/homebrew\n"),
        Some(PathBuf::from("/opt/homebrew/bin"))
    );
    assert_eq!(prefix_bin(b"relative\n"), None);
    assert_eq!(prefix_bin(b""), None);
    assert_eq!(prefix_bin(b"/a\n/b\n"), None, "one path only");
    let (_folder, home) = scratch();
    let npm = program(
        &home.join("npm"),
        "#!/bin/sh\necho \"$HOME/p-$1-$2-${CARGO_PKG_NAME:-clear}\"\n",
    );
    let places = crate::tools::Places::new(Vec::new(), Some(home.clone()));
    let resolved = crate::tools::resolve_entry(&npm, &places).expect("a script npm resolves");
    assert_eq!(
        npm_bin(&resolved, &home, "owner", NPM_LIMIT),
        Some(home.join("p-prefix--g-clear/bin")),
        "asked for the global prefix, with no inherited variable"
    );
}

#[test]
fn the_probed_folders_put_the_found_npm_prefix_bin_first_then_the_login_folders() {
    let (_folder, home) = scratch();
    let tools = home.join("tools");
    let other = home.join("other");
    let global = home.join("global/bin");
    for folder in [&tools, &other, &global] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let npm = program(&tools.join("npm"), "#!/bin/sh\necho \"$HOME/global\"\n");
    let places = || crate::tools::Places::new(Vec::new(), Some(home.clone()));
    assert_eq!(
        probed(&[tools.clone(), other.clone()], places(), &home, "owner"),
        (vec![global, tools.clone(), other.clone()], true)
    );
    assert_eq!(
        probed(&[other.clone()], places(), &home, "owner"),
        (vec![other.clone()], true),
        "no npm found, the login folders alone, and nothing failed"
    );
    std::fs::write(&npm, "#!/bin/sh\necho \"$HOME/absent\"\n").unwrap();
    assert_eq!(
        probed(&[tools.clone(), other.clone()], places(), &home, "owner"),
        (vec![tools.clone(), other.clone()], true),
        "a prefix with no bin folder adds nothing"
    );
    std::fs::write(&npm, "#!/bin/sh\nexit 1\n").unwrap();
    assert_eq!(
        probed(&[tools.clone(), other.clone()], places(), &home, "owner"),
        (vec![tools, other], false),
        "an npm that failed makes the run one to retry"
    );
}

#[test]
fn the_folder_probes_run_again_after_a_failure_or_a_miss_once_the_backoff_passed_and_after_the_refresh_age()
 {
    let start = Instant::now();
    let tick = Duration::from_millis(1);
    let (a, b) = (PathBuf::from("/a/bin"), PathBuf::from("/b/bin"));
    let mut probes = Probes::new();
    assert!(probes.start(start, false), "the first call starts a run");
    assert!(
        !probes.start(start + RETRY_AFTER, true),
        "one run at a time"
    );
    probes.finish(vec![a.clone()], false, start);
    assert_eq!(probes.found, [a.clone()]);
    assert!(!probes.start(start + RETRY_AFTER - tick, false));
    let retried = start + RETRY_AFTER;
    assert!(
        probes.start(retried, false),
        "a failed run is retried after the backoff"
    );
    probes.finish(vec![b.clone()], false, retried);
    assert_eq!(
        probes.found,
        [b.clone(), a.clone()],
        "a failed run never drops what an earlier run found"
    );
    assert!(probes.start(retried + RETRY_AFTER, false));
    let complete = retried + RETRY_AFTER;
    probes.finish(vec![b.clone()], true, complete);
    assert_eq!(
        probes.found,
        [b.clone()],
        "a complete run replaces the folders"
    );
    assert!(
        !probes.start(complete + RETRY_AFTER, false),
        "a complete run is kept"
    );
    assert!(
        !probes.start(complete + RETRY_AFTER - tick, true),
        "a resolve that missed waits for the backoff"
    );
    assert!(
        probes.start(complete + RETRY_AFTER, true),
        "a resolve that missed probes again"
    );
    let missed = complete + RETRY_AFTER;
    probes.finish(vec![a.clone(), b.clone()], true, missed);
    assert_eq!(probes.found, [a, b], "a tool installed later is found");
    assert!(!probes.start(missed + REFRESH_AGE - tick, false));
    assert!(
        probes.start(missed + REFRESH_AGE, false),
        "a complete run is refreshed after the refresh age"
    );
}

#[test]
fn a_version_is_the_first_word_with_a_digit_and_a_program_reports_its_own() {
    assert_eq!(reported(b"tmux 3.4\n").as_deref(), Some("3.4"));
    assert_eq!(
        reported(b"codex-cli v0.164.0\n").as_deref(),
        Some("0.164.0")
    );
    assert_eq!(
        reported(b"uv 0.9.5 (0a1b2c3 2025-10-01)\n").as_deref(),
        Some("0.9.5")
    );
    assert_eq!(reported(b"no digits here\n"), None);
    assert_eq!(reported(b"x 1.0;rm\n"), None, "only version characters");
    assert_eq!(
        reported(format!("x 1{}\n", "0".repeat(80)).as_bytes()),
        None
    );
    let (_folder, root) = scratch();
    let tool = program(&root.join("tool"), "#!/bin/sh\necho \"tool v1.2.$#$1\"\n");
    assert_eq!(
        probe_version(&tool, VERSION_LIMIT).as_deref(),
        Some("1.2.1--version"),
        "asked with --version alone"
    );
    let tmux = program(&root.join("tmux"), "#!/bin/sh\necho \"tmux 3.$#$1\"\n");
    assert_eq!(
        probe_version(&tmux, VERSION_LIMIT).as_deref(),
        Some("3.1-V"),
        "tmux is asked with -V, which it answers, not --version"
    );
    let hanging = program(&root.join("hangs"), "#!/bin/sh\nexec /bin/sleep 30\n");
    let started = Instant::now();
    assert_eq!(probe_version(&hanging, Duration::from_millis(300)), None);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Asks for a program's version until its probe answered, for at most ten seconds.
fn answered(tool: &Path) -> Option<String> {
    let started = Instant::now();
    let mut found = None;
    while found.is_none() && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
        found = version(tool);
    }
    found
}

/// Sets a file's modification time without touching its contents.
fn modified_at(file: &Path, time: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn version_answers_at_once_then_from_its_cache_until_the_file_is_updated_in_place() {
    let (_folder, root) = scratch();
    let tool = program(&root.join("tool"), "#!/bin/sh\necho \"tool 4.5.6\"\n");
    let started = Instant::now();
    assert_eq!(version(&tool), None, "unknown until the probe ran");
    assert!(started.elapsed() < Duration::from_secs(1), "never waits");
    assert_eq!(answered(&tool).as_deref(), Some("4.5.6"));
    let modified = std::fs::metadata(&tool).unwrap().modified().unwrap();
    std::fs::write(&tool, "#!/bin/sh\necho \"tool 9.9.9\"\n").unwrap();
    modified_at(&tool, modified);
    assert_eq!(
        version(&tool).as_deref(),
        Some("4.5.6"),
        "the same file, size and time answer from the cache"
    );
    std::fs::write(&tool, "#!/bin/sh\necho \"tool 10.0.0\"\n").unwrap();
    modified_at(&tool, modified + Duration::from_secs(5));
    assert_eq!(
        version(&tool),
        None,
        "a file updated in place is probed again"
    );
    assert_eq!(answered(&tool).as_deref(), Some("10.0.0"));
    assert_eq!(version(Path::new("relative/tool")), None);
}

#[test]
fn a_version_is_probed_again_when_its_file_changes_or_after_the_backoff_when_it_gave_none() {
    let now = Instant::now();
    let tick = Duration::from_millis(1);
    let identity = Identity {
        device: 1,
        inode: 2,
        size: 3,
        modified: (4, 5),
    };
    let updated = Identity {
        modified: (6, 0),
        ..identity
    };
    let known = |version: Option<&str>, finished: Option<Instant>| Known {
        identity,
        version: version.map(str::to_owned),
        finished,
    };
    assert!(version_due(None, identity, now), "never probed");
    assert!(
        !version_due(Some(&known(None, None)), identity, now + RETRY_AFTER),
        "one probe at a time"
    );
    let answered = known(Some("1.0"), Some(now));
    assert!(
        !version_due(Some(&answered), identity, now + REFRESH_AGE),
        "a version stays while its file does"
    );
    assert!(
        version_due(Some(&answered), updated, now),
        "a file updated in place"
    );
    let gave_none = known(None, Some(now));
    assert!(!version_due(
        Some(&gave_none),
        identity,
        now + RETRY_AFTER - tick
    ));
    assert!(
        version_due(Some(&gave_none), identity, now + RETRY_AFTER),
        "a probe that gave none is retried after the backoff"
    );
}
