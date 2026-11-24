use super::*;

const NO_FORWARDER_LINE: &str = "carries git index changes through the IDE's Source Control";

/// SECAUDIT-IDE-SERVER-INDEX and SECAUDIT-GIT-STATE-FILES under the installed `server.sb`:
/// no door reaches the real index or its lock, by the plain name or a case variant, and
/// nothing can be planted under the folders that carry a program or another repository,
/// nor a checkout link in a worktrees folder (SECAUDIT-WORKTREE-ADMIN-PLANT). The private
/// index folder stays writable and each tool used by a door works elsewhere.
#[test]
fn the_server_reaches_the_real_index_and_the_program_carrying_files_by_no_door() {
    let s = server();
    fs::write(s.root.join("CaseProbe"), "").unwrap();
    assert!(
        s.root.join("caseprobe").exists(),
        "needs a case-insensitive volume"
    );
    fs::remove_file(s.root.join("CaseProbe")).unwrap();
    let (index, lock) = (s.index(), s.root.join(".git/index.lock"));
    let spare = s.root.join("spare");
    fs::write(&spare, "raw\n").unwrap();
    let before = fs::read(&index).unwrap();
    let from = spare.display();
    let swap = format!(
        "/usr/bin/perl -e 'my ($first, $second) = @ARGV; exit(syscall(488, -2, $first, -2, $second, 2) == 0 ? 0 : 1)' '{from}' \"$1\""
    );
    let doors = [
        ("write", r#"printf x >> "$1""#.to_owned(), index.clone()),
        ("create the lock", r#": > "$1""#.to_owned(), lock.clone()),
        (
            "rename over",
            format!(r#"/bin/mv '{from}' "$1""#),
            index.clone(),
        ),
        (
            "hard link the lock",
            format!(r#"/bin/ln '{from}' "$1""#),
            lock.clone(),
        ),
        (
            "hard link over",
            format!(r#"/bin/ln -f '{from}' "$1""#),
            index.clone(),
        ),
        (
            "clone the lock",
            format!(r#"/bin/cp -c '{from}' "$1""#),
            lock.clone(),
        ),
        ("swap", swap.clone(), index.clone()),
        ("unlink", r#"/bin/rm "$1""#.to_owned(), index.clone()),
        (
            "case variant of the folder",
            r#"printf x >> "$1""#.to_owned(),
            s.root.join(".GIT/index"),
        ),
        (
            "case variant of the file",
            r#"printf x >> "$1""#.to_owned(),
            s.root.join(".git/INDEX"),
        ),
        (
            "case variant of the lock",
            r#": > "$1""#.to_owned(),
            s.root.join(".GIT/INDEX.LOCK"),
        ),
        (
            "move the git folder aside",
            r#"/bin/mv "$1" "$1.aside""#.to_owned(),
            s.root.join(".git"),
        ),
    ];
    for (door, script, target) in &doors {
        assert!(!sh(&s.profile, script, target), "{door} ran");
        assert_eq!(fs::read(&index).unwrap(), before, "{door}");
        assert!(!lock.exists(), "{door}");
    }
    for below in [
        ".git/rebase-merge/git-rebase-todo",
        ".git/rebase-apply/next",
        ".git/remotes/origin",
        ".git/branches/origin",
        ".git/modules/x/hooks/post-checkout",
        ".git/modules/x/config",
        ".git/sequencer/opts",
        ".GIT/REBASE-MERGE/git-rebase-todo",
        ".git/Remotes/origin",
        ".git/Sequencer/opts",
        "worktrees/planted/.git",
    ] {
        let target = s.root.join(below);
        let ran = sh(
            &s.profile,
            r#"/bin/mkdir -p "$(/usr/bin/dirname "$1")" && printf x > "$1""#,
            &target,
        );
        assert!(!ran, "{below} was planted");
        assert!(!target.exists(), "{below}");
    }
    for (name, script) in [
        ("hard link", format!(r#"/bin/ln '{from}' "$1""#)),
        ("clone", format!(r#"/bin/cp -c '{from}' "$1""#)),
    ] {
        let target = s.root.join(format!("control {name}"));
        assert!(sh(&s.profile, &script, &target), "control {name}");
        assert!(target.exists(), "control {name}");
    }
    let target = s.root.join("control swap");
    fs::write(&target, "other\n").unwrap();
    assert!(sh(&s.profile, &swap, &target), "control swap");
    assert_eq!(fs::read_to_string(&target).unwrap(), "raw\n");
    let target = s.root.join("control rename");
    assert!(sh(
        &s.profile,
        &format!(r#"/bin/mv '{from}' "$1""#),
        &target
    ));
    let private = s.home.join(crate::ide::git::PRIVATE_INDEX).join("written");
    assert!(sh(&s.profile, r#"printf x > "$1""#, &private));
    fs::remove_file(private).unwrap();
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A git the shim does not run, as an extension or anything else in the server can start,
/// cannot stage: it fails on the denied lock and the real index is byte-identical.
#[test]
fn a_git_outside_the_shim_cannot_stage() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    let before = fs::read(s.index()).unwrap();
    let real = Path::new(crate::research::runner::TOOLS_GIT);
    let mut command = server_git(&s.profile, real, &s.home, &s.root, false, "/usr/bin:/bin");
    command.args(["add", "a.txt"]).current_dir(&s.root);
    let output = kernel_processes::spawn_gate::output(&mut command).unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("index.lock"),
        "{output:?}"
    );
    assert_eq!(fs::read(s.index()).unwrap(), before);
    assert!(!s.root.join(".git/index.lock").exists());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// Source Control keeps working through the private index, with the real shim under the
/// installed profile and the real engine step: stage, unstage, a partial stage, commit,
/// checkout and a conflicted merge with its three stages, each read back by the owner's own
/// git outside the sandbox. No private file outlives a command.
#[test]
fn source_control_works_through_the_private_index() {
    let s = server();
    let staged = |args: &[&str]| {
        let (worked, said) = s.git(args);
        assert!(worked, "git {args:?}: {said}");
        assert!(s.no_private_files(), "git {args:?} left a private file");
    };

    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    staged(&["add", "a.txt"]);
    assert_eq!(s.status(), "M  a.txt\n");
    staged(&["reset", "-q", "HEAD", "--", "a.txt"]);
    assert_eq!(s.status(), " M a.txt\n");

    fs::write(s.root.join("partial"), "part\n").unwrap();
    let oid = setup(&s.root, &["hash-object", "-w", "partial"]);
    fs::remove_file(s.root.join("partial")).unwrap();
    staged(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{},a.txt", oid.trim()),
    ]);
    assert_eq!(s.status(), "MM a.txt\n");

    staged(&["commit", "-q", "-m", "ide"]);
    assert_eq!(s.status(), " M a.txt\n");
    assert_eq!(
        setup(&s.root, &["show", "HEAD:a.txt"]),
        "part\n",
        "the commit holds the partial stage"
    );
    staged(&["checkout", "-q", "--", "a.txt"]);
    assert_eq!(s.status(), "");
    assert_eq!(fs::read_to_string(s.root.join("a.txt")).unwrap(), "part\n");

    setup(&s.root, &["switch", "-q", "-c", "other", "HEAD~1"]);
    fs::write(s.root.join("a.txt"), "theirs\n").unwrap();
    setup(&s.root, &["commit", "-q", "-am", "theirs"]);
    setup(&s.root, &["switch", "-q", "main"]);
    let (merged, said) = s.git(&["merge", "other"]);
    assert!(!merged, "the merge conflicts: {said}");
    assert!(s.no_private_files());
    assert_eq!(s.status(), "UU a.txt\n");
    assert_eq!(
        setup(&s.root, &["ls-files", "-u"]).lines().count(),
        3,
        "all three stages reached the real index"
    );

    fs::write(s.root.join("a.txt"), "resolved\n").unwrap();
    staged(&["add", "a.txt"]);
    staged(&["commit", "-q", "--no-edit"]);
    assert_eq!(s.status(), "");
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A new checkout keeps its own index: `git worktree add` runs a child `reset --hard` and
/// `git clone` its checkout, and on the private index both would lose it, and a carry would
/// then unstage the project's files. Both run directly. The server's own profile refuses a
/// checkout at its `.git`; under a profile with no such limit the checkout gets its own
/// index. In both the project's real index is untouched and no private file is made.
#[test]
fn a_worktree_and_a_clone_through_the_shim_keep_their_own_index() {
    let s = server();
    fs::write(s.root.join("staged"), "staged\n").unwrap();
    setup(&s.root, &["add", "staged"]);
    let before = fs::read(s.index()).unwrap();
    let untouched = |args: &[&str]| {
        assert!(s.no_private_files(), "git {args:?} made a private file");
        assert_eq!(fs::read(s.index()).unwrap(), before, "git {args:?}");
    };
    for args in [
        &["worktree", "add", "-q", "refused", "-b", "refused", "HEAD"][..],
        &["clone", "-q", ".", "refused-clone"][..],
    ] {
        let (worked, said) = s.git(args);
        assert!(!worked, "git {args:?}: {said}");
        assert!(said.contains("Operation not permitted"), "{said}");
        untouched(args);
    }

    let open = s.home.join("open.sb");
    fs::write(&open, "(version 1)(allow default)\n").unwrap();
    let checked = |args: &[&str], folder: &str, index: &str| {
        let (worked, said) = ide_git(&open, &s.shim, &s.home, &s.root, &s.root, args);
        assert!(worked, "git {args:?}: {said}");
        untouched(args);
        assert!(s.root.join(index).exists(), "git {args:?} left no index");
        assert_eq!(setup(&s.root.join(folder), &["status", "--porcelain"]), "");
    };
    checked(
        &["worktree", "add", "-q", "wtree", "-b", "other", "HEAD"],
        "wtree",
        ".git/worktrees/wtree/index",
    );
    checked(
        &["clone", "-q", ".", "cloned"],
        "cloned",
        "cloned/.git/index",
    );
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A read-only command runs on the real index, so Source Control sees what is staged, and
/// makes no private copy.
#[test]
fn a_read_only_git_sees_the_real_index_and_copies_nothing() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    setup(&s.root, &["add", "a.txt"]);
    let output = ide_git_output(
        &s.profile,
        &s.shim,
        &s.home,
        &s.root,
        &s.root,
        &["status", "--porcelain"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "M  a.txt\n");
    assert!(s.no_private_files());
    assert!(!s.home.join(FORWARD_REQUEST).exists());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A git command that cannot reach the engine fails before git runs, with a plain line:
/// the shim with no forwarder, and a task's `git add` with the shim first on its path, as
/// the IDE shell and tasks run it. Read-only commands still work there.
#[test]
fn a_git_that_cannot_reach_the_engine_fails_before_it_runs() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    let before = fs::read(s.index()).unwrap();
    let held = |output: &std::process::Output| {
        assert_eq!(output.status.code(), Some(128), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(NO_FORWARDER_LINE),
            "{output:?}"
        );
        assert_eq!(fs::read(s.index()).unwrap(), before);
        assert!(s.no_private_files());
    };
    let mut direct = server_git(
        &s.profile,
        &s.shim,
        &s.home,
        &s.root,
        false,
        "/usr/bin:/bin",
    );
    for name in FORWARDER_VARIABLES {
        direct.env_remove(name);
    }
    direct.args(["add", "a.txt"]).current_dir(&s.root);
    held(&kernel_processes::spawn_gate::output(&mut direct).unwrap());
    held(&s.task("git add a.txt"));
    let read = s.task("git branch --show-current");
    assert!(read.status.success(), "{read:?}");
    assert_eq!(String::from_utf8_lossy(&read.stdout), "main\n");
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A rebase from the IDE stops with a plain line and leaves no state folder, since the
/// sandbox denies it; a merge still runs.
#[test]
fn a_rebase_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    let (ran, said) = s.git(&["rebase", "main"]);
    assert!(!ran);
    assert!(
        said.contains("does not run git rebase from the IDE"),
        "{said}"
    );
    assert!(!s.root.join(".git/rebase-merge").exists());
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// SECAUDIT ide-git-commondir-case-variant-config-redirect: IDE-sandboxed code cannot plant a
/// case-variant `commondir` in the project's git folder.
#[test]
fn ide_sandboxed_code_cannot_plant_a_case_variant_commondir() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    fs::write(base.join("CaseProbe"), "").unwrap();
    assert!(
        base.join("caseprobe").exists(),
        "needs a case-insensitive volume"
    );
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    setup(&root, &["init", "-q"]);
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let profile = install(
        &mine,
        &grants,
        PORT,
        closed(),
        &[],
        AgentFilesRule::default(),
    )
    .unwrap();
    let wrote = sh(&profile, "printf x > \"$1\"", &root.join(".git/COMMONDIR"));
    let planted: Vec<String> = fs::read_dir(root.join(".git"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.eq_ignore_ascii_case("commondir"))
        .collect();
    assert!(!wrote, "the write ran: {planted:?}");
    assert!(planted.is_empty(), "{planted:?}");
}

/// An intent-to-add entry has no form the carry can bring across, so the shim stops every
/// spelling of it in the project folder with a plain line before git runs, and nothing is
/// staged (SECAUDIT-14-L1).
#[test]
fn an_intent_to_add_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    fs::write(s.root.join("new.txt"), "eight by").unwrap();
    let spellings: [&[&str]; 6] = [
        &["add", "-N", "new.txt"],
        &["add", "--intent-to-add", "new.txt"],
        &["add", "-vN", "new.txt"],
        &["stage", "--inten", "new.txt"],
        &["reset", "-N"],
        &["apply", "-N", "none.patch"],
    ];
    for args in spellings {
        let (ran, said) = s.git(args);
        assert!(!ran, "{args:?}");
        assert!(
            said.contains("does not mark files as intent to add from the IDE"),
            "{args:?}: {said}"
        );
    }
    assert!(s.no_private_files());
    let (ran, said) = s.git(&["add", "--", "-N"]);
    assert!(
        !said.contains("intent to add"),
        "a path after -- is a path: {said}"
    );
    assert!(!ran, "git finds no file named -N: {said}");
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// git takes any unique prefix of a long option, so every prefix of `--intent-to-add` that
/// reset reads as it stops from the IDE in the project folder with the plain line
/// (SECAUDIT-14-L1 r1 L1).
#[test]
fn a_reset_with_a_prefix_of_intent_to_add_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    for prefix in ["--i", "--in", "--int", "--inte"] {
        let (ran, said) = s.git(&["reset", prefix]);
        assert!(!ran, "{prefix}");
        assert!(
            said.contains("does not mark files as intent to add from the IDE"),
            "{prefix}: {said}"
        );
    }
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// apply reads `--int` and `--inte` as `--intent-to-add`, so both stop from the IDE in the
/// project folder with the plain line, while `--index` is still apply's own option
/// (SECAUDIT-14-L1 r1 L1).
#[test]
fn an_apply_with_a_prefix_of_intent_to_add_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    for prefix in ["--int", "--inte"] {
        let (ran, said) = s.git(&["apply", prefix, "none.patch"]);
        assert!(!ran, "{prefix}");
        assert!(
            said.contains("does not mark files as intent to add from the IDE"),
            "{prefix}: {said}"
        );
    }
    let (ran, said) = s.git(&["apply", "--index", "none.patch"]);
    assert!(!said.contains("intent to add"), "--index: {said}");
    assert!(!ran, "git finds no none.patch: {said}");
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A git option before the verb does not hide an intent-to-add from the shim: it stops from
/// the IDE in the project folder with the plain line, and nothing is staged
/// (SECAUDIT-14-L1 r1 L1).
#[test]
fn an_intent_to_add_after_a_global_option_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    fs::write(s.root.join("new.txt"), "eight by").unwrap();
    let spellings: [&[&str]; 5] = [
        &["-c", "color.ui=never", "add", "-N", "new.txt"],
        &["-C", ".", "add", "-N", "new.txt"],
        &["--literal-pathspecs", "add", "-N", "new.txt"],
        &["--git-dir", ".git", "add", "-N", "new.txt"],
        &["--work-tree", ".", "add", "-N", "new.txt"],
    ];
    for args in spellings {
        let (ran, said) = s.git(args);
        assert!(!ran, "{args:?}");
        assert!(
            said.contains("does not mark files as intent to add from the IDE"),
            "{args:?}: {said}"
        );
    }
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// A git option before the verb does not hide a rebase from the shim: it stops from the IDE
/// with the plain line and leaves no state folder (SECAUDIT-14-L1 r1 L1).
#[test]
fn a_rebase_after_a_global_option_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    let spellings: [&[&str]; 2] = [
        &["-c", "color.ui=never", "rebase", "main"],
        &["-C", ".", "rebase", "main"],
    ];
    for args in spellings {
        let (ran, said) = s.git(args);
        assert!(!ran, "{args:?}");
        assert!(
            said.contains("does not run git rebase from the IDE"),
            "{args:?}: {said}"
        );
    }
    assert!(!s.root.join(".git/rebase-merge").exists());
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// The server's data folder and its other writable folders are granted after the base
/// profile, so the base denies there hold only when given again after that grant (sandbox-git
/// r1 note 4): no `.git` at any depth in any letter case, no launch file or folder of an
/// agent home, and no mode change of a granted folder itself, nor any flag or ACL in the
/// data folder, which the engine walks and rewrites before each start, so none keeps the
/// owner's IDE from starting (rv-sa241 b8 L1). Everything else there stays writable, npm's
/// config too, since no launch resets the data folder and the agent config denies are an
/// agent home's alone (RV-DS241-188).
#[test]
fn the_ide_data_folders_keep_the_git_launch_file_and_root_denies_after_their_grant() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let work = mine.join("work");
    for path in [&home, &work, &build, &root] {
        folder(path);
    }
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[work.as_path()],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let profile = install(
        &mine,
        &grants,
        PORT,
        closed(),
        &[],
        AgentFilesRule::default(),
    )
    .unwrap();
    let make = r#"/bin/mkdir -p "$1""#;
    let write = r#": > "$1""#;
    for granted in [&home, &work] {
        assert!(
            !sh(&profile, make, &granted.join(".git")),
            "{} takes no .git",
            granted.display()
        );
        assert!(!sh(&profile, make, &granted.join("deep/.GIT")));
        assert!(!sh(&profile, r#"/bin/chmod 777 "$1""#, granted));
        assert!(sh(&profile, make, &granted.join("deep/kept")));
        assert!(sh(&profile, write, &granted.join("kept.txt")));
    }
    for launch in ["settings.json", "CLAUDE.md", "Config.TOML"] {
        assert!(!sh(&profile, write, &home.join(launch)), "{launch}");
    }
    assert!(!sh(&profile, make, &home.join("agents")));
    assert!(sh(&profile, make, &home.join("data/User/globalStorage")));
    assert!(sh(&profile, write, &home.join("data/User/settings.json")));
    assert!(sh(&profile, write, &home.join(".npmrc")));
    let flagged = r#"/usr/bin/chflags uchg "$1" && /usr/bin/chflags nouchg "$1""#;
    assert!(!sh(&profile, flagged, &home.join("kept.txt")));
    let listed = r#"/bin/chmod +a 'everyone deny read' "$1" && /bin/chmod -N "$1""#;
    assert!(!sh(&profile, listed, &home.join("kept.txt")));
    for granted in [&home, &work] {
        assert!(!granted.join(".git").exists() && !granted.join("deep/.GIT").exists());
        assert_eq!(
            fs::metadata(granted).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(!home.join("settings.json").exists() && !home.join("agents").exists());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}
