use super::*;

#[test]
fn a_process_launched_the_ide_way_reads_its_capability_but_no_owner_key_or_engine_database() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let owner = data.join("o00000000");
    let ide = data.join(crate::ide::FOLDER);
    let mine = ide.join("p1");
    let other = ide.join("p2");
    let build = ide.join("builds").join("v1");
    let root = base.join("project");
    let (home, extensions) = (mine.join("server"), mine.join("extensions"));
    for path in [&owner, &home, &extensions, &other, &build, &root] {
        folder(path);
    }
    let files = [
        (owner.join("owner-desktop.cap"), "owner desktop key"),
        (owner.join("owner-tui.cap"), "owner tui key"),
        (owner.join("r00.cap"), "runner key"),
        (data.join("engine.db"), "engine database"),
        (data.join("engine.db-wal"), "engine database log"),
        (mine.join("capability"), "my capability"),
        (mine.join("token"), "my token"),
        (
            mine.join(crate::ide::server::LOG),
            "server output with the token",
        ),
        (extensions.join("extension.js"), "approved extension"),
        (other.join("capability"), "another capability"),
        (build.join("server-main.js"), "build file"),
        (root.join("README"), "project file"),
    ];
    for (path, text) in &files {
        fs::write(path, text).unwrap();
    }
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    let (capability, token) = (mine.join("capability"), mine.join("token"));
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[extensions.as_path()],
        readable: &[token.as_path(), capability.as_path()],
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
    assert_eq!(profile, mine.join(PROFILE));
    let read = |path: &Path| run(&profile, "/bin/cat", &[path.as_os_str()]);
    for (path, text) in &files {
        let allowed = [
            &capability,
            &token,
            &build.join("server-main.js"),
            &root.join("README"),
            &extensions.join("extension.js"),
        ]
        .contains(&path);
        let (ran, printed) = read(path.as_path());
        assert_eq!(ran, allowed, "{}", path.display());
        assert_eq!(printed == *text, allowed, "{}", path.display());
    }
    assert!(!run(&profile, "/bin/ls", &[data.as_os_str()]).0);
    assert!(!run(&profile, "/bin/ls", &[owner.as_os_str()]).0);
    let touch = |path: &Path| run(&profile, "/usr/bin/touch", &[path.as_os_str()]).0;
    assert!(touch(home.join("state").as_path()));
    assert!(!touch(extensions.join("state").as_path()));
    assert!(!touch(extensions.join("extension.js").as_path()));
    assert_eq!(
        fs::read_to_string(extensions.join("extension.js")).unwrap(),
        "approved extension"
    );
    assert!(touch(root.join("edited").as_path()));
    assert!(!touch(mine.join("planted").as_path()));
    assert!(!touch(data.join("planted").as_path()));
    assert!(!touch(owner.join("owner-desktop.cap").as_path()));
    assert!(!touch(mine.join(PROFILE).as_path()));
    assert_eq!(
        fs::read_to_string(owner.join("owner-desktop.cap")).unwrap(),
        "owner desktop key"
    );
    let socket = owner.join("owner.sock");
    let stray = owner.join("other.sock");
    let _listeners = [
        kernel_processes::spawn_gate::bind_unix_std(&socket).unwrap(),
        kernel_processes::spawn_gate::bind_unix_std(&stray).unwrap(),
    ];
    let connect = |path: &Path| {
        run(
            &profile,
            "/usr/bin/perl",
            &[
                std::ffi::OsStr::new("-MIO::Socket::UNIX"),
                std::ffi::OsStr::new("-e"),
                std::ffi::OsStr::new("IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 3"),
                path.as_os_str(),
            ],
        )
        .0
    };
    assert!(connect(socket.as_path()));
    assert!(!connect(stray.as_path()));
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// Whether any entry named `.git` in any ASCII letter case is at or below `top`, links not
/// followed. Input: the folder. Output: true when one is.
fn holds_git(top: &Path) -> bool {
    fs::read_dir(top).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_name().eq_ignore_ascii_case(".git")
                || (entry.file_type().is_ok_and(|kind| kind.is_dir()) && holds_git(&entry.path()))
        })
    })
}

/// SA vid 101: with the grants the server's launcher builds (no other writable folder, the
/// extensions folder read only, the Remote settings folder locked), the server data folder
/// takes no `.git` as a gitdir file, a symlink, a hard link or a folder renamed to `.Git`,
/// so none can be moved into the project, and the folder itself can be neither renamed nor
/// removed. A writable folder's own root is likewise pinned against a rename and `rmdir`.
#[test]
fn the_ide_server_data_folder_takes_no_git_in_any_form_and_keeps_its_root() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join(crate::ide::server::SERVER_DATA);
    let extensions = mine.join("extensions");
    let machine = crate::ide::settings::machine_folder(&home);
    for path in [&home, &extensions, &machine, &build, &root] {
        folder(path);
    }
    let token = mine.join("token");
    fs::write(&token, "token").unwrap();
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[extensions.as_path()],
        readable: &[token.as_path()],
        locked: &[machine.as_path()],
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
    let git = home.join(".git");
    assert!(!sh(
        &profile,
        r#"printf 'gitdir: /private/tmp/x\n' > "$1""#,
        &git
    ));
    assert!(!sh(&profile, r#"/bin/ln -s /private/tmp "$1""#, &git));
    assert!(!sh(
        &profile,
        r#": > "${1%/*}/plain" && /bin/ln "${1%/*}/plain" "$1""#,
        &git
    ));
    assert!(!sh(
        &profile,
        r#"/bin/mkdir "${1%/*}/aside" && /bin/mv "${1%/*}/aside" "$1""#,
        &home.join(".Git")
    ));
    assert!(!sh(&profile, r#"/bin/mkdir -p "$1""#, &home.join("r/.git")));
    let (moved, _) = run(
        &profile,
        "/bin/sh",
        &[
            OsStr::new("-c"),
            OsStr::new(r#"/bin/mkdir -p "$1/r/sub" && /bin/mv "$1/r" "$2/r""#),
            OsStr::new("sh"),
            home.as_os_str(),
            root.as_os_str(),
        ],
    );
    assert!(
        !holds_git(&home),
        "a .git was made in the server data folder"
    );
    assert!(
        !holds_git(&root),
        "a .git reached the project (moved: {moved})"
    );
    assert!(!renamed(&profile, &home, &root.join("home-moved")));
    assert!(!root.join("home-moved").exists());
    assert!(!sh(&profile, r#"/bin/chmod 777 "$1""#, &home));
    assert!(home.is_dir() && !home.is_symlink());
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(sh(&profile, r#": > "$1""#, &home.join("kept.txt")));

    let spare = mine.join("spare");
    folder(&spare);
    let grants = Grants {
        writable: &[spare.as_path()],
        ..grants
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
    assert!(!sh(&profile, r#"/bin/rmdir "$1""#, &spare));
    assert!(!renamed(&profile, &spare, &root.join("spare-moved")));
    assert!(spare.is_dir() && !root.join("spare-moved").exists());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// Whether git refused a folder as holding no repository it may use: git before 2.54 says
/// "not a git repository", and Apple git 2.54 stops at the `.git` the profile hides there,
/// "error reading '<folder>/.git'". Inputs: what git printed and the folder it ran in.
/// Output: true for either refusal.
fn no_repository(said: &str, dir: &Path) -> bool {
    let hidden = format!("error reading '{}'", dir.join(".git").display());
    said.contains("not a git repository") || said.contains(&hidden)
}

/// INT-1, IN1-1, IN1-F1 and INT1-R2-1 under the installed IDE profile, through the engine's
/// own git shim and git configuration. Agent folders under a `.crazyagent` project's agents
/// root, as an agent's own clone, a linked worktree whose admin folder it can write, a
/// `gitdir:` file and a bare-looking folder, each with a planted clean filter that runs when
/// git works there, are refused with the hooks switch Off and On, and the IDE cannot read a
/// clone's config. The owner's own tracked `worktrees` folder keeps the project's git. In
/// the Project section the IDE stages and commits, runs the project's hooks only when the
/// switch is On, never signs, and still cannot write the project's git config or hooks.
#[test]
fn ide_git_refuses_agent_folders_and_runs_project_hooks_only_when_on() {
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
    let marks = home.join("marks");
    for path in [&mine, &home, &build, &root, &marks, &root.join("tools")] {
        folder(path);
    }
    let ran = |name: &str| marks.join(name).exists();

    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    let sub = root.join("worktrees").join("sub");
    folder(&sub);
    fs::write(sub.join("own.txt"), "owner\n").unwrap();
    setup(&root, &["add", "a.txt", "worktrees/sub/own.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    planted(
        &root.join(".git").join("hooks").join("pre-commit"),
        &marks.join("project-hook"),
    );
    planted(
        &root.join("tools").join("sign.sh"),
        &marks.join("project-sign"),
    );
    setup(
        &root,
        &[
            "config",
            "gpg.program",
            root.join("tools/sign.sh").to_str().unwrap(),
        ],
    );
    setup(&root, &["config", "extensions.worktreeConfig", "true"]);
    for key in ["commit.gpgSign", "tag.gpgSign", "tag.forceSignAnnotated"] {
        setup(&root, &["config", key, "true"]);
    }

    let inner = root.join(".crazyagent").join("worktrees");
    folder(&inner);
    let clone = inner.join("agent-clone");
    setup(
        &base,
        &[
            "clone",
            "-q",
            root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let linked = inner.join("agent-linked");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-linked",
            linked.to_str().unwrap(),
        ],
    );
    let gitdir = inner.join("agent-gitdir");
    folder(&gitdir);
    setup(
        &gitdir,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            gitdir.join("meta").to_str().unwrap(),
        ],
    );
    let bare = inner.join("agent-bare");
    folder(&bare);
    setup(&bare, &["init", "-q", "--bare"]);
    setup(&bare, &["config", "core.bare", "false"]);
    setup(&bare, &["config", "core.worktree", bare.to_str().unwrap()]);
    let admin = root.join(".git").join("worktrees").join("agent-linked");
    let agents = [
        (&clone, clone.join(".git").join("config")),
        (&linked, admin.join("config.worktree")),
        (&gitdir, gitdir.join("meta").join("config")),
        (&bare, bare.join("config")),
    ];
    for (agent, config) in &agents {
        let name = agent.file_name().unwrap().to_str().unwrap();
        let filter = agent.join("filter.sh");
        planted(&filter, &marks.join(name));
        let mut text = fs::read_to_string(config).unwrap_or_default();
        text.push_str(&format!(
            "[filter \"evil\"]\n\tclean = {}\n\trequired = true\n",
            filter.display()
        ));
        fs::write(config, text).unwrap();
        fs::write(agent.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        fs::write(agent.join("notes.txt"), "planted\n").unwrap();
    }

    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let refuse_agents = |mode: &str| {
        for (agent, _) in &agents {
            let name = agent.file_name().unwrap().to_str().unwrap();
            for args in [&["status", "--porcelain"][..], &["add", "notes.txt"]] {
                let (worked, said) = ide_git(&profile, &shim, &home, &root, agent, args);
                assert!(!worked, "{mode}: git {args:?} worked in {name}: {said}");
                let refused = if *agent == &bare {
                    said.contains("cannot use bare repository")
                } else {
                    no_repository(&said, agent)
                };
                assert!(refused, "{mode}: {name}: {said}");
            }
            assert!(!ran(name), "{mode}: {name}'s filter ran");
        }
        let (read, printed) = run(
            &profile,
            "/bin/cat",
            &[clone.join(".git/config").as_os_str()],
        );
        assert!(
            !read && printed.is_empty(),
            "{mode}: the clone's config was read"
        );
        assert!(!sh(
            &profile,
            r#"printf x >> "$1""#,
            &root.join(".git/config")
        ));
        assert!(!sh(
            &profile,
            r#": > "$1""#,
            &root.join(".git/hooks/pre-commit")
        ));
    };

    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((&root, &home.join(crate::ide::git::PRIVATE_INDEX)))
        )
        .unwrap()
    );
    let (status, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["status", "--porcelain"],
    );
    assert!(
        status,
        "the Project section's git works under the profile: {said}"
    );
    let own = ide_git_output(
        &profile,
        &shim,
        &home,
        &root,
        &sub,
        &["rev-parse", "--show-toplevel"],
    );
    assert!(
        own.status.success(),
        "{}",
        String::from_utf8_lossy(&own.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap(),
        "the owner's own worktrees folder keeps the project's git"
    );
    fs::write(root.join("a.txt"), "off\n").unwrap();
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "a.txt"]);
    assert!(staged, "Off: the IDE stages in the Project section: {said}");
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "off"],
    );
    assert!(
        committed,
        "Off: the IDE commits in the Project section: {said}"
    );
    assert!(!ran("project-hook"), "Off: the project's hook never runs");
    let (tagged, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["tag", "-a", "-m", "off", "off"],
    );
    assert!(
        tagged,
        "Off: a project that signs every tag still tags: {said}"
    );
    assert!(!setup(&root, &["cat-file", "tag", "off"]).contains("BEGIN PGP"));
    let (signed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(
        !signed && said.contains("gpg failed to sign"),
        "Off: {said}"
    );
    assert!(!ran("project-sign"));
    refuse_agents("Off");

    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        Some(&root),
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    assert_ne!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((&root, &home.join(crate::ide::git::PRIVATE_INDEX)))
        )
        .unwrap()
    );
    fs::write(root.join("a.txt"), "on\n").unwrap();
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "a.txt"]);
    assert!(staged, "On: {said}");
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "on"],
    );
    assert!(committed, "On: {said}");
    assert!(
        ran("project-hook"),
        "On: the project's hook runs in the Project section"
    );
    let (signed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(!signed && said.contains("gpg failed to sign"), "On: {said}");
    assert!(!ran("project-sign"), "On: the IDE never signs");
    refuse_agents("On");
    assert_eq!(setup(&root, &["log", "--format=%s"]), "on\noff\nbase\n");
    for commit in ["HEAD", "HEAD~1"] {
        assert!(
            !setup(&root, &["cat-file", "commit", commit]).contains("gpgsig"),
            "IDE commits in a project that signs every commit go through unsigned"
        );
    }

    assert!(sh(
        &profile,
        r#""$1" < /dev/null"#,
        &root.join("tools/sign.sh")
    ));
    assert!(
        ran("project-sign"),
        "a program the IDE runs can record that it ran"
    );
    for (agent, _) in &agents {
        let name = agent.file_name().unwrap().to_str().unwrap();
        assert!(sh(
            &profile,
            r#""$1" < /dev/null"#,
            &agent.join("filter.sh")
        ));
        assert!(ran(name), "{name}'s filter can record under the profile");
        fs::remove_file(marks.join(name)).unwrap();
        let env: &[(&str, &str)] = if *agent == &bare {
            &[]
        } else {
            crate::ide::git::config_env()
        };
        let (added, _) = setup_git(agent, env, &["add", "notes.txt"]);
        assert!(
            added && ran(name),
            "{name}'s planted filter runs where git reaches it"
        );
    }
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// IN1-R1 under the installed IDE profile: in an older-layout project, whose agents sit at
/// `<project>/worktrees`, an agent folder's unreadable `.git` makes git in the Project
/// section see the agent's files as untracked, so Stage All would commit them past Accept.
/// Once the IDE start hides the agents root ([`crate::projects_home::exclude_agents`]),
/// Stage All through the IDE's git stages the owner's files only, and git in the agent's
/// folder stops at that root (INT1-R2-1).
#[test]
fn stage_all_in_an_older_layout_project_stages_no_agent_file() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join("worktrees").join("agent-3");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-3",
            agent.to_str().unwrap(),
        ],
    );
    fs::write(agent.join("work.txt"), "agent\n").unwrap();
    fs::write(root.join("notes.txt"), "owner\n").unwrap();
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root,
        "an older-layout project"
    );

    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);

    let before = ide_git_output(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["add", "-A", "--dry-run"],
    );
    let listed = String::from_utf8_lossy(&before.stdout).into_owned();
    assert!(
        listed.contains("worktrees/agent-3/work.txt"),
        "without the exclude, Stage All takes the agent's file: {listed}"
    );

    assert!(crate::projects_home::exclude_agents(&root).unwrap());
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "-A"]);
    assert!(staged, "{said}");
    assert_eq!(
        setup(&root, &["diff", "--cached", "--name-only"]),
        "notes.txt\n"
    );
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "owner"],
    );
    assert!(committed, "{said}");
    assert_eq!(
        setup(&root, &["show", "--name-only", "--format=", "HEAD"]),
        "notes.txt\n"
    );
    assert_eq!(setup(&agent, &["status", "--porcelain"]), "?? work.txt\n");
    let (inside, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &agent,
        &["status", "--porcelain"],
    );
    assert!(
        !inside && no_repository(&said, &agent),
        "the older layout's agents root stops the IDE's git: {said}"
    );
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// INT1-R3-1 under the installed IDE profile: an older-layout project keeps an agent's
/// linked worktree at `<project>/worktrees/agent-3` and then gains a `.crazyagent` folder by
/// hand. The profile still hides the agent's `.git`, so with the new layout's root alone git
/// in the agent's folder walks up and acts on the project; with the older root the engine
/// adds while it records the agent there, the IDE's git finds no repository, and the
/// Project section keeps its own.
#[test]
fn an_older_agent_keeps_its_git_stop_after_the_project_gains_a_crazyagent_folder() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join("worktrees").join("agent-3");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-3",
            agent.to_str().unwrap(),
        ],
    );
    folder(&root.join(".crazyagent"));
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root.join(".crazyagent"),
        "the project now reads as the newer layout"
    );

    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let top = |older_agents: bool, dir: &Path| {
        ide_git_recorded(
            &profile,
            &shim,
            &home,
            &root,
            older_agents,
            dir,
            &["rev-parse", "--show-toplevel"],
        )
    };

    let reached = top(false, &agent);
    if reached.status.success() {
        assert_eq!(
            String::from_utf8_lossy(&reached.stdout).trim(),
            root.to_str().unwrap(),
            "with the new layout's root alone the IDE's git in the agent's folder acts on the project"
        );
    } else {
        let said = String::from_utf8_lossy(&reached.stderr);
        assert!(
            no_repository(&said, &agent),
            "a git that stops at the hidden .git stops there: {said}"
        );
    }
    let stopped = top(true, &agent);
    let said = String::from_utf8_lossy(&stopped.stderr);
    assert!(
        !stopped.status.success() && no_repository(&said, &agent),
        "the older root stops the IDE's git in the agent's folder: {said}"
    );
    let own = top(true, &root);
    assert!(own.status.success());
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap()
    );
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// INT1R3-L1 under the installed IDE profile: an older-layout project ran a research job,
/// whose linked worktree stays at `<project>/worktrees/research-market-<job>-1` with no team
/// record, and then gains a `.crazyagent` folder by hand. The engine's own answer for the
/// older agents root keeps it in the ceiling, so the IDE's git in that folder finds no
/// repository instead of acting on the project's, and the Project section keeps its own.
#[test]
fn a_research_worktree_keeps_its_git_stop_after_the_project_gains_a_crazyagent_folder() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    let project = crate::protocol::ProjectId::new("older").unwrap();
    let job = crate::research::jobs::ResearchJobs::open(&data.join("engine.db"))
        .unwrap()
        .enqueue(project.as_str(), "market", "codex", false)
        .unwrap();
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let name = format!("research-market-{job}-1");
    let research = root.join("worktrees").join(&name);
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &name,
            research.to_str().unwrap(),
        ],
    );
    folder(&root.join(".crazyagent"));
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root.join(".crazyagent"),
        "the project now reads as the newer layout"
    );
    let older_agents = crate::ide::workspace::older_agents(&data, &project, &root).unwrap();
    assert!(
        older_agents,
        "the engine counts the research worktree left in the older agents root"
    );

    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let top = |dir: &Path| {
        ide_git_recorded(
            &profile,
            &shim,
            &home,
            &root,
            older_agents,
            dir,
            &["rev-parse", "--show-toplevel"],
        )
    };

    let stopped = top(&research);
    let said = String::from_utf8_lossy(&stopped.stderr);
    assert!(
        !stopped.status.success() && no_repository(&said, &research),
        "the IDE's git in the research worktree finds no repository: {said}"
    );
    let own = top(&root);
    assert!(own.status.success());
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap()
    );
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// IN1-FN1 under the installed IDE profile: the IDE cannot read an agent worktree's `.git`,
/// so git in the Project section sees the worktree as missing and a prune would drop its
/// registration. Through the IDE's git, `worktree prune` and `worktree remove` are refused
/// and a gc keeps the registration, while a dry-run prune by real git under the same
/// profile lists the worktree for removal.
#[test]
fn ide_git_never_prunes_an_agent_worktree() {
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
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join(".crazyagent").join("worktrees").join("agent-4");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-4",
            agent.to_str().unwrap(),
        ],
    );
    let admin = root.join(".git").join("worktrees").join("agent-4");
    let year = std::time::Duration::from_secs(365 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(admin.join("index"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - year)
        .unwrap();

    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let said = |output: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let registered = || admin.join("gitdir").is_file() && agent.join(".git").is_file();

    let dry = ide_git_output(
        &profile,
        Path::new(REAL_GIT),
        &home,
        &root,
        &root,
        &["worktree", "prune", "-n", "-v", "--expire=3.months.ago"],
    );
    assert!(
        said(&dry).contains("agent-4"),
        "the IDE sees the agent worktree as missing: {}",
        said(&dry)
    );
    for args in [
        &["worktree", "prune"][..],
        &["worktree", "prune", "-v", "--expire=now"],
        &[
            "worktree",
            "remove",
            "--force",
            "--force",
            agent.to_str().unwrap(),
        ],
    ] {
        let refused = ide_git_output(&profile, &shim, &home, &root, &root, args);
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            said(&refused).contains("the IDE does not run git worktree"),
            "{}",
            said(&refused)
        );
        assert!(registered(), "{args:?}");
    }
    let collected = ide_git_output(&profile, &shim, &home, &root, &root, &["gc", "-q"]);
    assert!(collected.status.success(), "{}", said(&collected));
    assert!(registered(), "a gc in the IDE keeps the agent worktree");
    assert_eq!(setup(&agent, &["status", "--porcelain"]), "");
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// FZ-7 under the installed IDE profile with the engine's own PATH, `/usr/bin` first: the
/// IDE's git never runs the `/usr/bin/git` xcrun shim, whose `xcrun_db` cache write in the
/// per-user temp folder the profile denies on every call. The status and the lookup of an
/// unset key that VS Code makes on every refresh print no such error, and the lookup still
/// exits 1 as git does, which VS Code logs as "Failed to execute git".
#[test]
fn ide_git_runs_no_xcrun_shim_under_the_profile_with_the_engines_path() {
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
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
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
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let ide = |args: &[&str]| {
        let mut command = server_git(&profile, &shim, &home, &root, false, "/usr/bin:/bin");
        command.args(args).current_dir(&root);
        kernel_processes::spawn_gate::output(&mut command).unwrap()
    };

    let status = ide(&["status", "--porcelain"]);
    let said = String::from_utf8_lossy(&status.stderr);
    assert!(status.status.success(), "{said}");
    assert!(
        !said.contains("xcrun_db"),
        "the IDE's status ran the xcrun shim: {said}"
    );
    let unset = ide(&["config", "--get", "--local", "core.virtualfilesystem"]);
    let said = String::from_utf8_lossy(&unset.stderr);
    assert_eq!(unset.status.code(), Some(1), "{said}");
    assert!(
        !said.contains("xcrun_db"),
        "the IDE's config lookup ran the xcrun shim: {said}"
    );
    let git = crate::research::runner::sandboxed_git();
    if git != Path::new("/usr/bin/git") {
        let text = fs::read_to_string(&shim).unwrap();
        for run in [
            format!("  exec '{}' \"$@\"\n", git.display()),
            format!("GIT_INDEX_FILE=\"$work\" '{}' \"$@\"\n", git.display()),
        ] {
            assert!(text.contains(&run), "the shim runs the engine's git: {run}");
        }
    }
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}
