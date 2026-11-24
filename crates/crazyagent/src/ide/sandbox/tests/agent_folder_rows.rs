use super::*;

/// One linked agent and its teammate under a [`Server`]'s agents root, as the team runtime
/// makes them, each holding a committed file and an untracked folder with a file in it, an
/// owner's own `worktrees` folder beside them, and an owner file in the project.
struct AgentFolders {
    agents: PathBuf,
    agent: PathBuf,
    teammate: PathBuf,
    owner_worktrees: PathBuf,
    owner_file: PathBuf,
}

/// Makes the [`AgentFolders`] of a server with real git, outside every sandbox. Input: the
/// server. Output: the folders.
fn agent_folders(s: &Server) -> AgentFolders {
    let agents = s.root.join(".crazyagent").join("worktrees");
    let agent = agents.join("agent-4");
    let teammate = agent.join(crate::agent_git::TEAMMATES_FOLDER).join("t2");
    setup(
        &s.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-4",
            agent.to_str().unwrap(),
        ],
    );
    setup(
        &agent,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-4--t2",
            teammate.to_str().unwrap(),
        ],
    );
    for checkout in [&agent, &teammate] {
        fs::create_dir_all(checkout.join("work")).unwrap();
        fs::write(checkout.join("work").join("notes.txt"), "agent\n").unwrap();
    }
    let owner_worktrees = s.root.join("worktrees").join("mine");
    fs::create_dir_all(&owner_worktrees).unwrap();
    fs::write(owner_worktrees.join("own.txt"), "owner\n").unwrap();
    let owner_file = s.root.join("own.txt");
    fs::write(&owner_file, "owner\n").unwrap();
    AgentFolders {
        agents,
        agent,
        teammate,
        owner_worktrees,
        owner_file,
    }
}

/// What each IDE change did in agent folders under one agent folders choice: a delete, a
/// folder delete, a rename, a folder rename, moving the agent folder and the agents root
/// aside, a delete through a case variant, an in-place edit, a new file, and the save
/// "Edit anyway" lets the owner make, written in place and written beside and renamed over.
/// Each is true when the change landed.
#[derive(Debug, PartialEq, Eq)]
struct Changes {
    delete: bool,
    delete_folder: bool,
    rename: bool,
    rename_folder: bool,
    move_agent: bool,
    move_agents_root: bool,
    delete_case_variant: bool,
    edit_in_place: bool,
    create: bool,
    edit_anyway_in_place: bool,
    edit_anyway_renamed_over: bool,
}

/// Makes every change of [`Changes`] under the profile of a fresh server holding `choice`,
/// checks that what a refused change aimed at is still there with its content, and that the
/// owner's own files outside the agents root stay changeable. A folder at
/// `<project>/worktrees/<name>` is where an older project keeps an agent's checkout, so the
/// base profile never removes or moves it at any choice; only its files change. Input: the
/// choice. Output: what landed.
fn changes_under(choice: IdeAgentFiles) -> Changes {
    let s = server_with(AgentFilesRule {
        choice,
        older_agents: false,
    });
    let f = agent_folders(&s);
    let teammate_file = f.teammate.join("a.txt");
    let agent_file = f.agent.join("a.txt");
    let teammate_notes = f.teammate.join("work").join("notes.txt");
    let agent_work = f.agent.join("work");
    let content = |path: &Path| fs::read_to_string(path).unwrap_or_default();
    let delete = sh(&s.profile, "rm -f \"$1\"", &teammate_file);
    assert_eq!(delete, !teammate_file.exists());
    let delete_folder = sh(&s.profile, "rm -rf \"$1\"", &f.teammate.join("work"));
    assert_eq!(delete_folder, !teammate_notes.exists());
    let rename = sh(&s.profile, "mv \"$1\" \"$1.moved\"", &agent_file);
    assert_eq!(rename, !agent_file.exists());
    let rename_folder = sh(&s.profile, "mv \"$1\" \"$1-aside\"", &agent_work);
    assert_eq!(rename_folder, !agent_work.exists());
    let variant = PathBuf::from(f.agent.to_str().unwrap().replace(
        ".crazyagent/worktrees/agent-4",
        ".CRAZYAGENT/WorkTrees/AGENT-4",
    ));
    let delete_case_variant =
        !rename_folder && sh(&s.profile, "rm -f \"$1/work/notes.txt\"", &variant);
    assert_eq!(
        delete_case_variant,
        !rename_folder && !agent_work.join("notes.txt").exists()
    );
    let readme = f.teammate.join("README.md");
    fs::write(&readme, "agent\n").unwrap();
    let edit_in_place = sh(&s.profile, "printf edited > \"$1\"", &readme);
    assert_eq!(edit_in_place, content(&readme) == "edited");
    let created = f.teammate.join("new.txt");
    let create = sh(&s.profile, "printf new > \"$1\"", &created);
    assert_eq!(create, created.exists());
    let unlocked = f.teammate.join("unlocked.txt");
    fs::write(&unlocked, "agent\n").unwrap();
    let edit_anyway_in_place = sh(&s.profile, "printf owner > \"$1\"", &unlocked);
    assert_eq!(edit_anyway_in_place, content(&unlocked) == "owner");
    let edit_anyway_renamed_over = sh(
        &s.profile,
        "printf saved > \"$1.tmp\" && mv -f \"$1.tmp\" \"$1\"",
        &unlocked,
    );
    assert_eq!(edit_anyway_renamed_over, content(&unlocked) == "saved");
    let move_agent = sh(&s.profile, "mv \"$1\" \"$1-aside\"", &f.agent);
    assert_eq!(move_agent, !f.agent.exists());
    let move_agents_root = !move_agent && sh(&s.profile, "mv \"$1\" \"$1-aside\"", &f.agents);
    assert_eq!(move_agents_root, !move_agent && !f.agents.exists());
    assert!(sh(&s.profile, "printf changed > \"$1\"", &f.owner_file));
    assert_eq!(content(&f.owner_file), "changed");
    assert!(sh(&s.profile, "rm -f \"$1/own.txt\"", &f.owner_worktrees));
    assert!(!f.owner_worktrees.join("own.txt").exists());
    assert!(!sh(&s.profile, "rmdir \"$1\"", &f.owner_worktrees));
    assert!(f.owner_worktrees.is_dir());
    assert!(sh(&s.profile, "rm -f \"$1\"", &f.owner_file));
    assert!(!f.owner_file.exists());
    Changes {
        delete,
        delete_folder,
        rename,
        rename_folder,
        move_agent,
        move_agents_root,
        delete_case_variant,
        edit_in_place,
        create,
        edit_anyway_in_place,
        edit_anyway_renamed_over,
    }
}

/// DEC-124, Allow all changes: the IDE changes agent folders as before the setting, and the
/// rule adds nothing to its profile. As before, the base profile never moves an agent's
/// checkout or the agents root aside, at any choice, while files and folders inside land.
#[test]
fn allow_all_changes_lets_the_ide_delete_rename_and_edit_in_agent_folders() {
    assert_eq!(AgentFilesRule::default().choice, IdeAgentFiles::AllowAll);
    for older_agents in [false, true] {
        let rule = AgentFilesRule {
            choice: IdeAgentFiles::AllowAll,
            older_agents,
        };
        assert_eq!(rule.seatbelt(Path::new("/tmp/project")), "");
    }
    assert_eq!(
        changes_under(IdeAgentFiles::AllowAll),
        Changes {
            delete: true,
            delete_folder: true,
            rename: true,
            rename_folder: true,
            move_agent: false,
            move_agents_root: false,
            delete_case_variant: false,
            edit_in_place: true,
            create: true,
            edit_anyway_in_place: true,
            edit_anyway_renamed_over: true,
        }
    );
}

/// DEC-124, No delete or rename: nothing in an agent folder, a teammate's included, is
/// removed or renamed, not through a case variant, not by moving the agent folder or the
/// agents root aside, while edits in place, new files and the in-place save "Edit anyway"
/// allows still land. A save written beside and renamed over is refused, since the rename
/// replaces the file (sandbox-exec probe, agent-files/l1-probe, DEC-124 r1 L1).
#[test]
fn no_delete_or_rename_keeps_every_agent_file_and_folder_while_edits_land() {
    assert_eq!(
        changes_under(IdeAgentFiles::NoDeleteOrRename),
        Changes {
            delete: false,
            delete_folder: false,
            rename: false,
            rename_folder: false,
            move_agent: false,
            move_agents_root: false,
            delete_case_variant: false,
            edit_in_place: true,
            create: true,
            edit_anyway_in_place: true,
            edit_anyway_renamed_over: false,
        }
    );
}

/// DEC-124, Read only: no write of any kind lands in an agent folder, a teammate's included:
/// no delete, rename, move aside, edit, new file or "Edit anyway" save.
#[test]
fn read_only_refuses_every_write_in_agent_folders() {
    assert_eq!(
        changes_under(IdeAgentFiles::ReadOnly),
        Changes {
            delete: false,
            delete_folder: false,
            rename: false,
            rename_folder: false,
            move_agent: false,
            move_agents_root: false,
            delete_case_variant: false,
            edit_in_place: false,
            create: false,
            edit_anyway_in_place: false,
            edit_anyway_renamed_over: false,
        }
    );
}

/// IDE-SANDBOX-AGENT-FOLDERS (a) with DEC-124: the installed profile keeps the agent git
/// admin deny at every agent folders choice, with that choice's own rule right after it.
#[test]
fn every_agent_folders_choice_keeps_the_agent_admin_deny() {
    for choice in IdeAgentFiles::ALL {
        let rule = AgentFilesRule {
            choice,
            older_agents: false,
        };
        let s = server_with(rule);
        let text = fs::read_to_string(&s.profile).unwrap();
        let admin = agent_admin(&s.root);
        assert!(!admin.is_empty());
        assert!(
            text.contains(&format!("{admin}{}", rule.seatbelt(&s.root))),
            "{choice:?}"
        );
    }
}

/// DEC-124 r1 rv-code L1: a `.CRAZYAGENT` folder made before the first agent, which the
/// engine's agent then lands in on a case-insensitive volume, is never moved aside under a
/// protecting choice, so the agent's files stay under the rule; under Read only the IDE cannot
/// make it at all. Under No delete or rename the base `.crazyagent` write deny may refuse the
/// IDE's own mkdir of the variant, so the row notes that result and makes the folder outside
/// the sandbox when it was refused (DEC124-R2-L1).
#[test]
fn a_case_variant_crazyagent_folder_made_before_the_first_agent_stays_in_place() {
    for (choice, ide_makes) in [
        (IdeAgentFiles::NoDeleteOrRename, None),
        (IdeAgentFiles::ReadOnly, Some(false)),
    ] {
        let s = server_with(AgentFilesRule {
            choice,
            older_agents: false,
        });
        let variant = s.root.join(".CRAZYAGENT");
        let made = sh(&s.profile, "mkdir \"$1\"", &variant);
        eprintln!("{choice:?}: the IDE's own mkdir of .CRAZYAGENT made it: {made}");
        match ide_makes {
            Some(expected) => assert_eq!(made, expected, "{choice:?}"),
            None if !made => fs::create_dir(&variant).unwrap(),
            None => {}
        }
        let agent = s.root.join(".crazyagent").join("worktrees").join("agent-4");
        setup(
            &s.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent-4",
                agent.to_str().unwrap(),
            ],
        );
        let file = agent.join("a.txt");
        assert!(file.exists());
        assert!(
            !sh(&s.profile, "mv \"$1\" \"$1-aside\"", &variant),
            "{choice:?}"
        );
        assert!(!sh(&s.profile, "rm -f \"$1\"", &file), "{choice:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "base\n");
    }
}

/// In a project with a `.crazyagent` folder, the older agents root `<project>/worktrees` is
/// held only while engine-made worktrees stay there; until then it may be the owner's own
/// folder, which stays changeable. A project without one always holds it
/// (`ide::workspace::agent_files_rule`, DEC-124 r1 M1).
#[test]
fn the_older_agents_root_is_held_only_while_agents_stay_there() {
    for (older_agents, held) in [(false, false), (true, true)] {
        let s = server_with(AgentFilesRule {
            choice: IdeAgentFiles::NoDeleteOrRename,
            older_agents,
        });
        fs::create_dir_all(s.root.join(".crazyagent")).unwrap();
        let agent = s.root.join("worktrees").join("agent-1");
        fs::create_dir_all(&agent).unwrap();
        let file = agent.join("a.txt");
        fs::write(&file, "agent\n").unwrap();
        assert_eq!(sh(&s.profile, "rm -f \"$1\"", &file), !held);
        assert_eq!(file.exists(), held);
    }
}

/// IDE-SANDBOX-AGENT-FOLDERS (a) under the installed `server.sb`: the IDE writes nothing
/// under the project's `.git/worktrees`, in any letter case, nor below an agent folder's
/// `.git`, whatever makes the write: a shell, real git by path, or git through the shim,
/// an alias included, a Kelvin sign or long s spelling of `worktrees` included, and no
/// hard link of an admin file into the project carries a write to it. Agents' admin files
/// keep their bytes and nothing is planted there. `branch -m` of an agent's branch stops
/// at the agent's admin `HEAD`, which keeps its bytes, but the branch ref itself is
/// renamed: the row pins that partial rename (IDE-ADMIN-BRANCH-RENAME). Status,
/// `worktree list`, a commit at the project folder and a gc, with agent reflogs and
/// without, still work.
#[test]
fn the_ide_writes_nothing_under_an_agent_s_git_admin_folders() {
    let s = server();
    let agents = s.root.join(".crazyagent").join("worktrees");
    let admins = s.root.join(".git").join("worktrees");
    let agent = agents.join("agent-4");
    let teammate = agent.join(crate::agent_git::TEAMMATES_FOLDER).join("t2");
    let renamed = agents.join("agent-6");
    for (dir, branch, path) in [
        (&s.root, "agent-4", &agent),
        (&agent, "agent-4--t2", &teammate),
        (&s.root, "agent-6", &renamed),
    ] {
        setup(
            dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
    }
    let clone = agents.join("agent-5");
    setup(&s.root, &["clone", "-q", ".", clone.to_str().unwrap()]);
    let clone_teammate = clone.join(crate::agent_git::TEAMMATES_FOLDER).join("t1");
    setup(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-5--t1",
            clone_teammate.to_str().unwrap(),
        ],
    );
    let names = ["agent-4", "t2", "agent-6"];
    let mut heads: Vec<PathBuf> = names
        .iter()
        .flat_map(|name| ["HEAD", "index"].map(|file| admins.join(name).join(file)))
        .collect();
    heads.push(clone.join(".git").join("HEAD"));
    heads.push(clone.join(".git").join("worktrees").join("t1").join("HEAD"));
    let logs: Vec<PathBuf> = names
        .iter()
        .map(|name| admins.join(name).join("logs").join("HEAD"))
        .collect();
    let read = |files: &[PathBuf]| -> Vec<Vec<u8>> {
        files.iter().map(|file| fs::read(file).unwrap()).collect()
    };
    let collect = |step: &str| {
        let (worked, said) = s.git(&["gc", "-q"]);
        assert!(worked, "gc {step}: {said}");
    };

    let (worked, said) = s.git(&["status", "--porcelain"]);
    assert!(worked, "{said}");
    let listed = ide_git_output(
        &s.profile,
        &s.shim,
        &s.home,
        &s.root,
        &s.root,
        &["worktree", "list", "--porcelain"],
    );
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("agent-4--t2"));
    fs::write(s.root.join("b.txt"), "ide\n").unwrap();
    for args in [&["add", "b.txt"][..], &["commit", "-q", "-m", "ide"]] {
        let (worked, said) = s.git(args);
        assert!(worked, "{args:?}: {said}");
    }
    assert_eq!(setup(&s.root, &["log", "-1", "--format=%s"]), "ide\n");
    assert!(logs.iter().all(|file| file.is_file()));
    collect("with agent reflogs");
    let before = read(&heads);
    let logged = read(&logs);

    for file in heads.iter().chain(&logs) {
        assert!(!sh(&s.profile, "rm -f \"$1\"", file), "{}", file.display());
        assert!(
            !sh(&s.profile, "printf x > \"$1\"", file),
            "{}",
            file.display()
        );
    }
    for planted in [
        admins.join("planted"),
        s.root.join(".GIT/Worktrees/planted"),
    ] {
        assert!(
            !sh(&s.profile, "mkdir \"$1\"", &planted),
            "{}",
            planted.display()
        );
    }
    assert!(!admins.join("planted").exists());
    let lock = admins.join("agent-4").join("locked");
    assert!(!sh(&s.profile, ": > \"$1\"", &lock));
    let linked =
        r#"/bin/ln "$1" "${1%/.git/*}/h" && printf 'ref: refs/heads/x\n' > "${1%/.git/*}/h""#;
    assert!(!sh(
        &s.profile,
        linked,
        &admins.join("agent-4").join("HEAD")
    ));
    assert!(!s.root.join("h").exists());
    for spelling in ["wor\u{212a}trees", "worktree\u{17f}"] {
        let head = s
            .root
            .join(".git")
            .join(spelling)
            .join("agent-4")
            .join("HEAD");
        assert!(
            !sh(&s.profile, "printf x > \"$1\"", &head),
            "{}",
            head.display()
        );
    }
    let commit = setup(&s.root, &["rev-parse", "agent-4"]);
    let (moved, said) = ide_git(
        &s.profile,
        Path::new(REAL_GIT),
        &s.home,
        &s.root,
        &s.root,
        &[
            "update-ref",
            "--no-deref",
            "worktrees/agent-4/HEAD",
            commit.trim(),
        ],
    );
    assert!(!moved, "{said}");
    let (locked, said) = s.git(&["worktree", "lock", agent.to_str().unwrap()]);
    assert!(!locked, "{said}");
    assert!(!lock.exists());
    assert_eq!(read(&heads), before);
    assert_eq!(read(&logs), logged);

    for file in &logs {
        fs::remove_file(file).unwrap();
    }
    collect("without agent reflogs");
    let renamed_from = setup(
        &s.root,
        &["rev-parse", "--verify", "-q", "refs/heads/agent-6"],
    );
    let (moved, said) = s.git(&["branch", "-m", "agent-6", "agent-x"]);
    assert!(!moved, "{said}");
    assert!(said.contains("HEAD is not updated"), "{said}");
    let (kept, _) = setup_git(
        &s.root,
        &[],
        &["rev-parse", "--verify", "-q", "refs/heads/agent-6"],
    );
    assert!(!kept, "IDE-ADMIN-BRANCH-RENAME: the ref agent-6 moved");
    assert_eq!(
        setup(
            &s.root,
            &["rev-parse", "--verify", "-q", "refs/heads/agent-x"]
        ),
        renamed_from,
        "IDE-ADMIN-BRANCH-RENAME: agent-x holds agent-6's commit"
    );
    let (removed, said) = s.git(&[
        "-c",
        "alias.x=worktree remove --force --force",
        "x",
        teammate.to_str().unwrap(),
    ]);
    assert!(!removed, "{said}");
    assert_eq!(read(&heads), before);
    assert!(s.no_private_files());
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// IDE-SANDBOX-AGENT-FOLDERS (a) r1 M1 under the installed `server.sb`: in a project with
/// no `.git/worktrees` yet, the IDE plants no admin root under a spelling APFS folds to it
/// with the Kelvin sign for `k` or the long s for `s`, nor through the project folder
/// spelled with a long s, so a later `git worktree add` by the engine never puts agents'
/// admin folders under a name the deny misses.
#[test]
fn the_ide_plants_no_admin_root_under_a_kelvin_or_long_s_spelling() {
    let s = server();
    let git = s.root.join(".git");
    assert!(!git.join("worktrees").exists());
    let variant = PathBuf::from(
        s.root
            .to_str()
            .unwrap()
            .replacen("ca-ides-", "ca-ide\u{17f}-", 1),
    );
    assert_ne!(variant, s.root);
    for planted in [
        git.join("wor\u{212a}trees"),
        git.join("worktree\u{17f}"),
        git.join("wor\u{212a}tree\u{17f}"),
        variant.join(".git").join("worktrees"),
    ] {
        assert!(
            !sh(&s.profile, "/bin/mkdir \"$1\"", &planted),
            "{}",
            planted.display()
        );
        assert!(!git.join("worktrees").exists(), "{}", planted.display());
    }
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}

/// SA vid 202 and the ligature half of vid 11 under the installed `server.sb`: the IDE
/// makes no `.git/config.worktree` and no `.crazyagent/instructions.md` under a spelling
/// the case-insensitive volume folds onto them through one code point for two letters
/// (U+FB01 for `fi`, U+FB05 and U+FB06 for `st`), so the owner's own git never reads a
/// planted `core.fsmonitor` and no later agent reads planted instructions. Nor under a
/// spelling holding a code point HFS+ ignores in names (U+200C, U+FEFF), which that volume
/// opens as the plain name, and it reads no project file whose path holds one. A project
/// file named with such a fold of letters no rule protects stays writable and readable, as
/// does one whose name holds other letters beside such a code point, as Persian names do.
#[test]
fn the_ide_plants_no_git_config_or_instructions_under_a_ligature_or_hfs_ignored_spelling() {
    let s = server();
    let git = s.root.join(".git");
    let state = s.root.join(".crazyagent");
    folder(&state);
    let write = r#"printf '[core]\n\tfsmonitor = /usr/bin/true\n' > "$1""#;
    for planted in [
        git.join("con\u{fb01}g.worktree"),
        git.join("CON\u{fb01}G.WORKTREE"),
        state.join("in\u{fb05}ructions.md"),
        state.join("IN\u{fb06}RUCTIONS.MD"),
        git.join("con\u{200c}fig.worktree"),
        state.join("\u{feff}instructions.md"),
    ] {
        assert!(!sh(&s.profile, write, &planted), "{}", planted.display());
        assert!(!planted.exists(), "{}", planted.display());
    }
    assert!(!git.join("config.worktree").exists());
    assert!(!state.join("instructions.md").exists());
    let hidden = s.root.join("no\u{200c}tes.md");
    fs::write(&hidden, "owner\n").unwrap();
    assert!(!sh(&s.profile, r#"/bin/cat "$1""#, &hidden));
    for kept in [
        s.root.join("Gro\u{df}.md"),
        s.root.join("o\u{fb03}ce.md"),
        s.root
            .join("\u{646}\u{627}\u{645}\u{200c}\u{647}\u{627}.md"),
    ] {
        assert!(sh(&s.profile, write, &kept), "{}", kept.display());
        assert!(
            sh(&s.profile, r#"/bin/cat "$1""#, &kept),
            "{}",
            kept.display()
        );
    }
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &s.home).unwrap();
}
