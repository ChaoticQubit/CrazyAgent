use super::*;
use std::process::Command;

/// An agent's own tool cache goes only through the hooks' tree removal: when that removal
/// refuses, removing the agent's caches fails with the hooks' code and the cache folder that
/// stays, and the folder stays.
#[test]
fn an_agents_own_cache_goes_only_through_the_hooks_removal() {
    fn unpin(_parent: std::os::fd::BorrowedFd<'_>, _name: &str) {}
    fn kept(
        _parent: std::os::fd::BorrowedFd<'_>,
        _name: &str,
        _otherwise: &'static str,
    ) -> Result<(), &'static str> {
        Err("kept by the stand-in")
    }
    fn priming(_folder: &Path, _uv: &Path) -> Command {
        Command::new("/usr/bin/false")
    }
    let hooks = ToolCacheHooks {
        unpin,
        remove_agent_entry: kept,
        priming_command: priming,
    };
    let runtime = tempfile::tempdir().unwrap();
    let key = key(
        ToolCacheScope::PerAgent,
        AgentAccess::Sandboxed,
        CacheShape::Worktree,
        "mate",
    )
    .unwrap();
    let cache = project_folder(runtime.path(), "project").join(key);
    std::fs::create_dir_all(&cache).unwrap();
    for folder in cache.ancestors().skip(1).take(2) {
        std::fs::set_permissions(folder, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
    }
    let refused = remove_agent(&hooks, runtime.path(), "project", "mate").unwrap_err();
    assert_eq!(refused.to_string(), "kept by the stand-in");
    assert_eq!(
        refused
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<EntryRefused>()),
        Some(&EntryRefused {
            code: "kept by the stand-in",
            folder: cache.clone(),
        })
    );
    assert!(cache.is_dir());
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

/// No two level and shape pairs share a cache, Full access gets none, and under One per agent each
/// agent gets its own, apart from every shared one.
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

/// A bucket named `.git` or over 64 bytes, or a 17th bucket, refuses the whole fresh cache, and up
/// to 16 plain buckets pass.
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

/// A link in place of a folder above the agent's cache, to a folder outside holding a cache
/// of the same name with a decoy in it, makes removing the agent's caches fail and removes
/// nothing there, whether the link stands for the tool cache folder or the project's.
#[test]
fn a_linked_folder_above_the_cache_removes_nothing_through_it() {
    use std::os::unix::fs::PermissionsExt;
    fn unpin(_parent: std::os::fd::BorrowedFd<'_>, _name: &str) {}
    fn priming(_folder: &Path, _uv: &Path) -> Command {
        Command::new("/usr/bin/false")
    }
    let hooks = ToolCacheHooks {
        unpin,
        remove_agent_entry: removed,
        priming_command: priming,
    };
    let key = key(
        ToolCacheScope::PerAgent,
        AgentAccess::Sandboxed,
        CacheShape::Worktree,
        "mate",
    )
    .unwrap();
    for linked_project in [false, true] {
        let runtime = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let owned = project_folder(outside.path(), "project");
        let decoy = owned.join(&key).join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        for folder in [outside.path().join(FOLDER), owned.clone(), owned.join(&key)] {
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        if linked_project {
            let tools = runtime.path().join(FOLDER);
            std::fs::create_dir(&tools).unwrap();
            std::fs::set_permissions(&tools, std::fs::Permissions::from_mode(0o700)).unwrap();
            let name = owned.file_name().unwrap();
            std::os::unix::fs::symlink(&owned, tools.join(name)).unwrap();
        } else {
            std::os::unix::fs::symlink(outside.path().join(FOLDER), runtime.path().join(FOLDER))
                .unwrap();
        }
        assert!(remove_agent(&hooks, runtime.path(), "project", "mate").is_err());
        assert!(decoy.is_dir(), "linked project folder: {linked_project}");
    }
}

/// A stand-in by-handle removal: empties the folder `name` of `parent` through handles,
/// never following a link, then removes it; an entry that is no folder, a link included,
/// is unlinked from `parent` as it is. Inputs: the open parent, the entry name and the
/// refusal code. Output: done, or the code.
fn removed(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &str,
    otherwise: &'static str,
) -> Result<(), &'static str> {
    let found =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| otherwise)?;
    if FileType::from_raw_mode(found.st_mode) != FileType::Directory {
        return rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(|_| otherwise);
    }
    let folder =
        rustix::fs::openat(parent, name, OPEN_FOLDER, Mode::empty()).map_err(|_| otherwise)?;
    let names: Vec<String> = rustix::fs::Dir::read_from(&folder)
        .map_err(|_| otherwise)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| entry != "." && entry != "..")
        .collect();
    for entry in &names {
        removed(folder.as_fd(), entry, otherwise)?;
    }
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|_| otherwise)
}

/// The project folder the swapping stand-in moves aside on its first call, and the folder
/// the link it leaves in its place names.
static SWAP: std::sync::Mutex<Option<(PathBuf, PathBuf)>> = std::sync::Mutex::new(None);

/// A project folder swapped for a link after the folders above its caches were checked,
/// once the first cache is gone, removes nothing through the link: the second cache goes
/// from the folder that was checked, now moved aside, and the decoy the link names stays.
#[test]
fn a_project_folder_swapped_for_a_link_after_its_check_removes_nothing_through_it() {
    use std::os::unix::fs::PermissionsExt;
    fn unpin(_parent: std::os::fd::BorrowedFd<'_>, _name: &str) {}
    fn swapping(
        parent: std::os::fd::BorrowedFd<'_>,
        name: &str,
        otherwise: &'static str,
    ) -> Result<(), &'static str> {
        removed(parent, name, otherwise)?;
        let swap = SWAP
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((project, outside)) = swap {
            std::fs::rename(&project, project.with_extension("aside")).unwrap();
            std::os::unix::fs::symlink(&outside, &project).unwrap();
        }
        Ok(())
    }
    fn priming(_folder: &Path, _uv: &Path) -> Command {
        Command::new("/usr/bin/false")
    }
    let hooks = ToolCacheHooks {
        unpin,
        remove_agent_entry: swapping,
        priming_command: priming,
    };
    let [own, mates] = ["mate", "mate-teammates"].map(|agent| {
        key(
            ToolCacheScope::PerAgent,
            AgentAccess::Sandboxed,
            CacheShape::Worktree,
            agent,
        )
        .unwrap()
    });
    let runtime = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let project = project_folder(runtime.path(), "project");
    for name in [&own, &mates] {
        std::fs::create_dir_all(project.join(name)).unwrap();
    }
    std::fs::create_dir(outside.path().join(&mates)).unwrap();
    for folder in [runtime.path().join(FOLDER), project.clone()] {
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    *SWAP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((project.clone(), outside.path().to_path_buf()));
    let removal = remove_agent(&hooks, runtime.path(), "project", "mate");
    assert!(removal.is_ok(), "{removal:?}");
    assert!(outside.path().join(&mates).is_dir());
    let aside = project.with_extension("aside");
    assert!(!aside.join(&own).exists());
    assert!(!aside.join(&mates).exists());
}

/// Opens a fresh folder the way `prepare` opens a project's folder. Output: the guard and
/// the open folder.
fn record_folder() -> (tempfile::TempDir, OwnedFd) {
    let folder = tempfile::tempdir().unwrap();
    let owned = rustix::fs::open(folder.path(), OPEN_FOLDER, Mode::empty()).unwrap();
    (folder, owned)
}

/// The names in a folder, sorted. Input: the folder. Output: the names.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort_unstable();
    names
}

/// A launch still writing through a staging file it opened cannot change the record another
/// launch published, since each writer stages in a file of its own.
#[test]
fn a_writer_still_holding_its_staging_file_cannot_change_the_record() {
    let (folder, owned) = record_folder();
    let mut held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(folder.path().join("k.primed.new"))
        .unwrap();
    write_record(&owned, "k.primed", "id 1", &["abc".to_owned()]).unwrap();
    held.write_all(b"zz\n").unwrap();
    assert_eq!(
        read_record(&owned, "k.primed"),
        Some(("id 1".to_owned(), vec!["abc".to_owned()]))
    );
}

/// Writing a record twice leaves only the record, holding the last write.
#[test]
fn a_written_record_leaves_no_staging_file() {
    let (folder, owned) = record_folder();
    write_record(&owned, "k.primed", "id 1", &["abc".to_owned()]).unwrap();
    write_record(&owned, "k.primed", "id 2", &["def".to_owned()]).unwrap();
    assert_eq!(names_in(folder.path()), ["k.primed"]);
    assert_eq!(
        read_record(&owned, "k.primed"),
        Some(("id 2".to_owned(), vec!["def".to_owned()]))
    );
}

/// A staging file left by a dead process that had this pid is removed once, the record is
/// written, and no staging file is left.
#[test]
fn a_staging_file_left_under_this_name_is_replaced() {
    let (folder, owned) = record_folder();
    std::fs::write(folder.path().join("k.primed.1.0.new"), b"left").unwrap();
    publish(&owned, "k.primed", "k.primed.1.0.new", b"id 1\nabc\n").unwrap();
    assert_eq!(names_in(folder.path()), ["k.primed"]);
    assert_eq!(
        read_record(&owned, "k.primed"),
        Some(("id 1".to_owned(), vec!["abc".to_owned()]))
    );
}

/// A record write removes the staging files of this record that another process left, and
/// keeps this process's own and every name that is not exactly a staging name of the record.
#[test]
fn a_record_write_sweeps_staging_files_another_process_left() {
    let (folder, owned) = record_folder();
    let own = std::process::id();
    let other = own.wrapping_add(1);
    assert_ne!(other, own);
    let own_side = format!("k.primed.{own}.999999999.new");
    let kept = [
        own_side.as_str(),
        "k.primed.x.0.new",
        "k.primed..0.new",
        "k.primed.1.new",
        "k.primed.1.0.new.bak",
        "other.primed.1.0.new",
    ];
    for name in kept {
        std::fs::write(folder.path().join(name), b"kept").unwrap();
    }
    std::fs::write(
        folder.path().join(format!("k.primed.{other}.0.new")),
        b"left",
    )
    .unwrap();
    write_record(&owned, "k.primed", "id 1", &["abc".to_owned()]).unwrap();
    let mut expected: Vec<String> = kept.iter().map(|name| (*name).to_owned()).collect();
    expected.push("k.primed".to_owned());
    expected.sort_unstable();
    assert_eq!(names_in(folder.path()), expected);
    assert_eq!(
        read_record(&owned, "k.primed"),
        Some(("id 1".to_owned(), vec!["abc".to_owned()]))
    );
}

std::thread_local! {
    /// The names the priming rows' removal stand-ins were asked for, on this row's thread.
    static ASKED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A removal stand-in that notes each name it is asked for, then removes it as
/// [`removed`] does. Inputs and output: as [`removed`].
fn noted(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &str,
    otherwise: &'static str,
) -> Result<(), &'static str> {
    ASKED.with(|asked| asked.borrow_mut().push(name.to_owned()));
    removed(parent, name, otherwise)
}

/// A removal stand-in that notes each name it is asked for and refuses every one, as a
/// folder too deep or still gaining entries is refused. Inputs: as [`removed`]. Output: the
/// refusal.
fn noted_and_kept(
    _parent: std::os::fd::BorrowedFd<'_>,
    name: &str,
    _otherwise: &'static str,
) -> Result<(), &'static str> {
    ASKED.with(|asked| asked.borrow_mut().push(name.to_owned()));
    Err("kept by the stand-in")
}

/// A removal stand-in that notes each name it is asked for, removes this process's own
/// priming folder as [`removed`] does and refuses any other. Inputs: as [`removed`].
/// Output: done for its own folder, else the refusal.
fn noted_and_kept_unless_own(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &str,
    otherwise: &'static str,
) -> Result<(), &'static str> {
    ASKED.with(|asked| asked.borrow_mut().push(name.to_owned()));
    if name.starts_with(&format!(".priming-{}-", std::process::id())) {
        removed(parent, name, otherwise)
    } else {
        Err("kept by the stand-in")
    }
}

/// A priming command that fails. Inputs: the priming folder and uv. Output: the command.
fn priming_fails(_folder: &Path, _uv: &Path) -> Command {
    Command::new("/usr/bin/false")
}

/// A priming command that ends well with an empty cache, so no bucket is found. Inputs: the
/// priming folder and uv. Output: the command.
fn priming_empty(folder: &Path, _uv: &Path) -> Command {
    let mut command = Command::new("/bin/mkdir");
    command.arg(folder.join("cache"));
    command
}

/// The hooks the priming rows use: the given removal and priming command. Inputs: the two.
/// Output: the hooks.
fn priming_hooks(
    remove_agent_entry: fn(
        std::os::fd::BorrowedFd<'_>,
        &str,
        &'static str,
    ) -> Result<(), &'static str>,
    priming_command: fn(&Path, &Path) -> Command,
) -> ToolCacheHooks {
    fn unpin(_parent: std::os::fd::BorrowedFd<'_>, _name: &str) {}
    ToolCacheHooks {
        unpin,
        remove_agent_entry,
        priming_command,
    }
}

/// A priming first removes, through the hooks, each priming folder another process left,
/// and keeps this process's own and every name that is not exactly a priming folder's.
#[test]
fn a_priming_sweeps_the_priming_folders_another_process_left() {
    let (folder, tools) = record_folder();
    let own = std::process::id();
    let other = own.wrapping_add(1);
    assert_ne!(other, own);
    let left = format!(".priming-{other}-1");
    std::fs::create_dir_all(folder.path().join(&left).join("home")).unwrap();
    std::fs::write(folder.path().join(&left).join("home").join("x"), b"x").unwrap();
    let own_folder = format!(".priming-{own}-2");
    let kept = [
        own_folder.as_str(),
        ".priming-x-1",
        ".priming-1",
        ".priming-1-1-1",
        "priming-1-1",
        "0123456789abcdef0123456789abcdef",
    ];
    for name in kept {
        std::fs::create_dir(folder.path().join(name)).unwrap();
    }
    let hooks = priming_hooks(noted, priming_fails);
    let primed = prime(&hooks, &tools, folder.path(), Path::new("/usr/bin/true"));
    assert_eq!(primed, Err("tool_cache_priming_failed"));
    let mut expected: Vec<String> = kept.iter().map(|name| (*name).to_owned()).collect();
    expected.sort_unstable();
    assert_eq!(names_in(folder.path()), expected);
    assert!(ASKED.with(|asked| asked.borrow().contains(&left)));
}

/// A priming folder another process left that the removal refuses stays, and the priming
/// goes on and finds its buckets.
#[test]
fn a_priming_goes_on_when_a_left_folder_cannot_be_removed() {
    let (folder, tools) = record_folder();
    let left = format!(".priming-{}-1", std::process::id().wrapping_add(1));
    std::fs::create_dir(folder.path().join(&left)).unwrap();
    let hooks = priming_hooks(noted_and_kept_unless_own, priming_empty);
    let primed = prime(&hooks, &tools, folder.path(), Path::new("/usr/bin/true"));
    assert!(ASKED.with(|asked| asked.borrow().contains(&left)));
    assert_eq!(names_in(folder.path()), [left]);
    assert_eq!(primed, Ok(Vec::new()));
}

/// A priming removes its own folder through the hooks: the folder is gone once the removal
/// takes it, and a removal that refuses fails the priming with its plain code.
#[test]
fn a_priming_removes_its_own_folder_through_the_hooks() {
    let (folder, tools) = record_folder();
    let hooks = priming_hooks(noted, priming_empty);
    let primed = prime(&hooks, &tools, folder.path(), Path::new("/usr/bin/true"));
    assert_eq!(primed, Ok(Vec::new()));
    assert_eq!(names_in(folder.path()), Vec::<String>::new());
    let own = format!(".priming-{}-", std::process::id());
    let asked = ASKED.with(|asked| asked.borrow().clone());
    assert_eq!(asked.len(), 1);
    assert!(asked[0].starts_with(&own));
    let (folder, tools) = record_folder();
    let hooks = priming_hooks(noted_and_kept, priming_empty);
    let primed = prime(&hooks, &tools, folder.path(), Path::new("/usr/bin/true"));
    assert_eq!(primed, Err("tool_cache_priming_failed"));
}

/// A rename that fails removes the writer's staging file and refuses the record.
#[test]
fn a_failed_rename_removes_the_staging_file() {
    let (folder, owned) = record_folder();
    std::fs::create_dir(folder.path().join("k.primed")).unwrap();
    std::fs::write(folder.path().join("k.primed").join("inside"), b"x").unwrap();
    assert_eq!(
        publish(&owned, "k.primed", "k.primed.1.0.new", b"id 1\n"),
        Err("tool_cache_record_unwritten")
    );
    assert_eq!(names_in(folder.path()), ["k.primed"]);
}
