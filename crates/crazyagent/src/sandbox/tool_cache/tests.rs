use super::*;
use std::os::unix::fs::PermissionsExt;

/// A canonical engine runtime folder and a stand-in uv: a script that makes the cache the
/// way uv 0.13 does, a `sdists-v10` bucket holding an empty `.git` file beside a
/// `wheels-v6` folder without one, after running `extra` first.
struct Runtime {
    _temp: tempfile::TempDir,
    runtime: PathBuf,
    uv: PathBuf,
}

impl Runtime {
    fn new(extra: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let runtime = root.join("runtime");
        let tools = root.join("tools");
        for folder in [&runtime, &tools] {
            std::fs::create_dir(folder).unwrap();
        }
        let uv = tools.join("uv");
        std::fs::write(
            &uv,
            format!(
                "#!/bin/sh\n{extra}\nmkdir -p \"$UV_CACHE_DIR/sdists-v10\" \"$UV_CACHE_DIR/wheels-v6\" && : > \"$UV_CACHE_DIR/sdists-v10/.git\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _temp: temp,
            runtime,
            uv,
        }
    }

    fn prepare(&self) -> Result<Prepared, &'static str> {
        prepare(
            &self.runtime,
            "project-1",
            "sandboxed-worktree",
            Some(&self.uv),
        )
    }

    fn priming_left(&self) -> usize {
        std::fs::read_dir(self.runtime.join(FOLDER))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".priming-")
            })
            .count()
    }
}

/// The marker path of the stand-in's bucket in a prepared cache.
fn marker_of(prepared: &Prepared) -> PathBuf {
    prepared.root.join(UV).join("sdists-v10").join(MARKER)
}

#[test]
fn a_folder_name_from_uv_is_lowercase_letters_digits_and_dashes_only() {
    for name in ["sdists-v10", "wheels-v6", "a", &"a".repeat(64)] {
        assert!(plain_name(name), "{name}");
    }
    for name in [
        "",
        ".git",
        ".GIT",
        ".Git",
        "..",
        ".",
        "Upper",
        "a/b",
        "a.b",
        "a b",
        &"a".repeat(65),
    ] {
        assert!(!plain_name(name), "{name}");
    }
}

/// T8 C2 (R2): no two level and shape pairs share a cache, Full access gets none, and under
/// One per agent each agent gets its own, apart from every shared one.
#[test]
fn shared_caches_differ_by_level_and_shape_and_full_access_gets_none() {
    let mut keys = std::collections::BTreeSet::new();
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        for shape in CacheShape::ALL {
            assert!(keys.insert(key(ToolCacheScope::Shared, access, shape, "35").unwrap()));
        }
        for agent in ["35", "52", "master"] {
            let own = key(
                ToolCacheScope::PerAgent,
                access,
                CacheShape::Worktree,
                agent,
            )
            .unwrap();
            assert_eq!(
                own,
                key(ToolCacheScope::PerAgent, access, CacheShape::Clone, agent).unwrap()
            );
            keys.insert(own);
        }
    }
    assert_eq!(keys.len(), 2 * CacheShape::ALL.len() + 6);
    for scope in [ToolCacheScope::Shared, ToolCacheScope::PerAgent] {
        assert_eq!(
            key(scope, AgentAccess::Full, CacheShape::ProjectRoot, "35"),
            None
        );
    }
}

/// T8 C3 (R3): the priming process starts uv under the priming profile from an environment
/// holding exactly the listed names, in the priming folder.
#[test]
fn the_priming_process_holds_only_the_listed_environment() {
    let folder = Path::new("/private/var/crazyagent-test/tool-cache/.priming-1-2");
    let uv = Path::new("/opt/homebrew/Cellar/uv/0.13.5/bin/uv");
    let command = priming_command(folder, uv);
    assert_eq!(
        command.get_program(),
        OsStr::new(crate::sandbox::SANDBOX_EXEC)
    );
    let args: Vec<&OsStr> = command.get_args().collect();
    assert_eq!(args[0], "-p");
    assert_eq!(
        &args[2..],
        [uv.as_os_str(), "tool".as_ref(), "list".as_ref()]
    );
    let mut names: Vec<String> = command
        .get_envs()
        .map(|(name, value)| {
            assert!(value.is_some());
            name.to_string_lossy().into_owned()
        })
        .collect();
    names.sort();
    let mut listed: Vec<String> = PRIMING_ENV.iter().map(|name| (*name).to_owned()).collect();
    listed.sort();
    assert_eq!(names, listed);
    assert_eq!(command.get_current_dir(), Some(folder));
}

/// T8: priming leaves a `.git` link to exactly `/dev/null` in each bucket uv marked and no
/// other bucket, an empty pnpm store, and no priming folder.
#[test]
fn priming_makes_a_dev_null_link_in_each_marked_bucket_and_leaves_no_priming_folder() {
    let runtime = Runtime::new("");
    let prepared = runtime.prepare().unwrap();
    assert!(prepared.uv && prepared.primed);
    assert_eq!(
        prepared.root,
        project_folder(&runtime.runtime, "project-1").join("sandboxed-worktree")
    );
    assert_eq!(
        std::fs::read_link(marker_of(&prepared)).unwrap(),
        Path::new(MARKER_TARGET)
    );
    assert!(!prepared.root.join(UV).join("wheels-v6").exists());
    assert!(prepared.root.join(PNPM).is_dir());
    assert_eq!(runtime.priming_left(), 0);
    for folder in [
        runtime.runtime.join(FOLDER),
        project_folder(&runtime.runtime, "project-1"),
        prepared.root.clone(),
    ] {
        let mode = std::fs::symlink_metadata(&folder)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{}", folder.display());
    }
}

/// T8 C4 (R6): a second launch with the same uv starts no priming process, and still finds
/// the marker, remaking one an agent's move of its bucket took away.
#[test]
fn a_second_launch_with_the_same_uv_does_not_prime_and_remakes_a_moved_marker() {
    let runtime = Runtime::new("");
    let first = runtime.prepare().unwrap();
    let again = runtime.prepare().unwrap();
    assert!(again.uv && !again.primed);
    let bucket = first.root.join(UV).join("sdists-v10");
    std::fs::rename(&bucket, first.root.join(UV).join("moved")).unwrap();
    assert!(!runtime.prepare().unwrap().primed);
    assert_eq!(
        std::fs::read_link(marker_of(&first)).unwrap(),
        Path::new(MARKER_TARGET)
    );
    assert_eq!(
        std::fs::read_link(first.root.join(UV).join("moved").join(MARKER)).unwrap(),
        Path::new(MARKER_TARGET)
    );
}

/// T8 r1 L1: launches racing on one Shared key after an agent moved a bucket away all get
/// uv's marker: one makes the link and the others find it, so none ends with
/// `tool_cache_marker_unmade` and runs uv with no cache.
#[test]
fn launches_racing_on_one_key_all_get_the_marker() {
    const ROUNDS: usize = 20;
    const LAUNCHES: usize = 8;
    let runtime = Runtime::new("");
    let bucket = runtime.prepare().unwrap().root.join(UV).join("sdists-v10");
    for round in 0..ROUNDS {
        std::fs::rename(&bucket, bucket.with_file_name(format!("moved-{round}"))).unwrap();
        let start = std::sync::Barrier::new(LAUNCHES);
        let ended: Vec<Result<Prepared, &'static str>> = std::thread::scope(|scope| {
            let launches: Vec<_> = (0..LAUNCHES)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        runtime.prepare()
                    })
                })
                .collect();
            launches
                .into_iter()
                .map(|launch| launch.join().unwrap())
                .collect()
        });
        assert!(ended.iter().all(Result::is_ok), "round {round}: {ended:?}");
        assert_eq!(
            std::fs::read_link(bucket.join(MARKER)).unwrap(),
            Path::new(MARKER_TARGET)
        );
    }
}

/// T8 C4: a uv binary that changed since the record primes again.
#[test]
fn a_changed_uv_primes_again() {
    let runtime = Runtime::new("");
    assert!(runtime.prepare().unwrap().primed);
    let mut script = std::fs::read_to_string(&runtime.uv).unwrap();
    script.push_str("true\n");
    std::fs::write(&runtime.uv, script).unwrap();
    assert!(runtime.prepare().unwrap().primed);
}

/// T8: a file, a folder or a link to anything but `/dev/null` at a marker fails closed.
#[test]
fn anything_but_a_dev_null_link_at_a_marker_fails_closed() {
    for planted in ["file", "folder", "link"] {
        let runtime = Runtime::new("");
        let marker = marker_of(&runtime.prepare().unwrap());
        std::fs::remove_file(&marker).unwrap();
        match planted {
            "file" => std::fs::write(&marker, "").unwrap(),
            "folder" => std::fs::create_dir(&marker).unwrap(),
            _ => std::os::unix::fs::symlink("/etc", &marker).unwrap(),
        }
        assert_eq!(
            runtime.prepare(),
            Err("tool_cache_marker_unexpected"),
            "{planted}"
        );
    }
}

/// T8 C3 (R5): a link in place of the engine's own folders, the tool cache folder, the
/// project's folder or the cache root, fails closed, and nothing is made through it.
#[test]
fn a_link_in_place_of_an_engine_cache_folder_fails_closed() {
    for at in ["tool-cache", "project", "root"] {
        let runtime = Runtime::new("");
        let prepared = runtime.prepare().unwrap();
        let elsewhere = runtime.runtime.parent().unwrap().join("elsewhere");
        let swapped = match at {
            "tool-cache" => runtime.runtime.join(FOLDER),
            "project" => project_folder(&runtime.runtime, "project-1"),
            _ => prepared.root.clone(),
        };
        std::fs::rename(&swapped, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &swapped).unwrap();
        assert_eq!(runtime.prepare(), Err("tool_cache_link"), "{at}");
    }
}

/// T8 r1 L2: in the cache an agent writes, a link left where `uv` or a bucket was, a file
/// left where `pnpm-store` was and a `uv` folder closed to its owner do not keep every
/// later launch at the key from a cache: the next launch unlinks what was left without
/// following it, opens the closed folder again and makes the rest anew, and the link's
/// target gets nothing.
#[test]
fn a_link_file_or_closed_folder_an_agent_left_in_its_cache_is_repaired_at_the_next_launch() {
    let runtime = Runtime::new("");
    let prepared = runtime.prepare().unwrap();
    let elsewhere = runtime.runtime.parent().unwrap().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let uv = prepared.root.join(UV);
    let pnpm = prepared.root.join(PNPM);
    std::fs::rename(&uv, prepared.root.join("moved-uv")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &uv).unwrap();
    std::fs::rename(&pnpm, prepared.root.join("moved-pnpm")).unwrap();
    std::fs::write(&pnpm, "left").unwrap();
    assert_eq!(
        runtime.prepare().map(|again| (again.root, again.uv)),
        Ok((prepared.root.clone(), true)),
        "uv link and pnpm file"
    );
    let bucket = uv.join("sdists-v10");
    std::fs::rename(&bucket, uv.join("moved-bucket")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &bucket).unwrap();
    assert_eq!(
        runtime.prepare().map(|again| (again.root, again.uv)),
        Ok((prepared.root.clone(), true)),
        "bucket link"
    );
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        runtime.prepare().map(|again| (again.root, again.uv)),
        Ok((prepared.root.clone(), true)),
        "closed uv folder"
    );
    for folder in [&uv, &pnpm, &bucket] {
        let found = std::fs::symlink_metadata(folder).unwrap();
        assert!(found.is_dir(), "{} is a folder again", folder.display());
    }
    assert_eq!(
        std::fs::read_link(marker_of(&prepared)).unwrap(),
        Path::new(MARKER_TARGET)
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// T8 C3: a folder of the engine's own that others may enter fails closed.
#[test]
fn an_engine_cache_folder_others_may_enter_fails_closed() {
    let runtime = Runtime::new("");
    let prepared = runtime.prepare().unwrap();
    std::fs::set_permissions(&prepared.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(runtime.prepare(), Err("tool_cache_folder_refused"));
}

/// T8 C3 (R3): a bucket named `.git` or over 64 bytes, or a 17th bucket, refuses the whole
/// fresh cache, and up to 16 plain buckets pass.
#[test]
fn a_bucket_name_or_count_out_of_bounds_refuses_the_cache() {
    let mark = |cache: &Path, name: &str| {
        std::fs::create_dir_all(cache.join(name)).unwrap();
        std::fs::write(cache.join(name).join(MARKER), "").unwrap();
    };
    for name in [".git", ".GIT", &"a".repeat(65), "Upper", "a.b"] {
        let temp = tempfile::tempdir().unwrap();
        mark(temp.path(), "sdists-v10");
        mark(temp.path(), name);
        assert_eq!(
            buckets_in(temp.path()),
            Err("tool_cache_bucket_refused"),
            "{name}"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    for index in 0..BUCKETS_MAX {
        mark(temp.path(), &format!("bucket-{index}"));
    }
    std::fs::create_dir(temp.path().join("unmarked")).unwrap();
    std::fs::write(temp.path().join("CACHEDIR.TAG"), "tag").unwrap();
    assert_eq!(buckets_in(temp.path()).unwrap().len(), BUCKETS_MAX);
    mark(temp.path(), "bucket-16");
    assert_eq!(buckets_in(temp.path()), Err("tool_cache_bucket_refused"));
}

/// T8 C3: a uv that fails, or that names a bad bucket, gives no cache and leaves no priming
/// folder; a launch with no uv gets the cache for pnpm alone.
#[test]
fn a_failed_priming_gives_no_cache_and_no_uv_gives_a_pnpm_cache() {
    for extra in [
        "exit 1",
        "mkdir -p \"$UV_CACHE_DIR/.GIT\" && : > \"$UV_CACHE_DIR/.GIT/.git\"",
    ] {
        let runtime = Runtime::new(extra);
        assert!(runtime.prepare().is_err(), "{extra}");
        assert_eq!(runtime.priming_left(), 0, "{extra}");
    }
    let runtime = Runtime::new("");
    let prepared = prepare(&runtime.runtime, "project-1", "sandboxed-root", None).unwrap();
    assert!(!prepared.uv && !prepared.primed);
    assert!(prepared.root.join(PNPM).is_dir());
    assert!(!prepared.root.join(UV).join("sdists-v10").exists());
}

/// T8 C3: the priming process reaches nothing outside its folder: a uv that tries to write
/// beside the engine's runtime fails, and nothing appears there.
#[test]
fn the_priming_process_writes_nothing_outside_its_folder() {
    let probe = "echo x > \"$HOME/../../../escaped\" && exit 0";
    let runtime = Runtime::new(probe);
    let _ = runtime.prepare();
    assert!(!runtime.runtime.join(FOLDER).join("escaped").exists());
    assert!(!runtime.runtime.join("escaped").exists());
}
