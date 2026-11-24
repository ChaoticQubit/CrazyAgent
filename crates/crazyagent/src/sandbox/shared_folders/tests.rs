use super::*;
use kernel_sandbox::shared_folders::*;
#[cfg(unix)]
use std::path::PathBuf;
fn chosen(names: &[&str]) -> SharedFolders {
    SharedFolders {
        scope: SharedScope::Chosen,
        chosen: names.iter().map(|name| (*name).to_owned()).collect(),
        ..SharedFolders::default()
    }
}

#[cfg(unix)]
fn git(root: &Path, args: &[&str]) {
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Shared Test")
        .env("GIT_AUTHOR_EMAIL", "shared@example.invalid")
        .env("GIT_COMMITTER_NAME", "Shared Test")
        .env("GIT_COMMITTER_EMAIL", "shared@example.invalid")
        .current_dir(root);
    let status = kernel_processes::spawn_gate::status(&mut command);
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "git {args:?}: {status:?}"
    );
}

/// A repository with a tracked `src/main.rs` and `.gitignore`, an untracked `src/local.rs`,
/// a wholly untracked `program/` and ignored `build/`, an untracked `notes.txt`, and an agent
/// worktree folder under `.crazyagent/`.
#[cfg(unix)]
fn project() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    git(&root, &["init", "-q", "--template="]);
    git(&root, &["add", "src/main.rs", ".gitignore"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    std::fs::write(root.join("src").join("local.rs"), "\n").unwrap();
    std::fs::create_dir_all(root.join("program")).unwrap();
    std::fs::write(root.join("program").join("data.csv"), "a,b\n").unwrap();
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::write(root.join("build").join("out.bin"), "x").unwrap();
    std::fs::write(root.join("notes.txt"), "notes\n").unwrap();
    let agent = root.join(".crazyagent").join("worktrees").join("agent-1");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("work.txt"), "work\n").unwrap();
    (temp, root)
}

/// All with Read-only asks git nothing: a folder outside git still plans, with only the
/// shared-back secrets.
#[test]
fn all_with_read_only_plans_without_git_and_reveals_only_the_shared_back_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let shared = SharedFolders {
        secrets: vec!["config/.env".to_owned()],
        ..SharedFolders::default()
    };
    let plan = plan(&crate::sandbox::ACCESS_HOOKS, &root, &shared).unwrap();
    assert!(plan.kept_out.is_empty() && plan.writes.is_empty());
    assert_eq!(plan.revealed, vec![root.join("config").join(".env")]);
}

/// None keeps out every path git does not track, ignored or not, but never `.git` or the
/// engine folder that holds the agent's own worktree, and never a tracked path.
#[cfg(unix)]
#[test]
fn none_keeps_out_every_untracked_path_but_the_engine_folders() {
    let (_temp, root) = project();
    let shared = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let plan = plan(&crate::sandbox::ACCESS_HOOKS, &root, &shared).unwrap();
    for kept in ["program", "build", "notes.txt", "src/local.rs"] {
        assert!(plan.kept_out.contains(&root.join(kept)), "{kept}: {plan:?}");
    }
    assert!(
        plan.kept_out
            .iter()
            .all(|path| !path.starts_with(root.join(".crazyagent"))
                && !path.starts_with(root.join(".git"))
                && *path != root.join("src").join("main.rs")),
        "{plan:?}"
    );
    assert!(plan.writes.is_empty());
}

/// A project made before the projects home keeps the engine's worktrees and context in the
/// project folder itself: None never keeps those out, the picker never offers them, and
/// Read-write never opens a top-level `context` folder for writing in any layout.
#[cfg(unix)]
#[test]
fn engine_state_in_the_project_folder_is_never_kept_out_offered_or_opened_for_writes() {
    let (_temp, root) = project();
    std::fs::remove_dir_all(root.join(".crazyagent")).unwrap();
    let own = root.join("worktrees").join("agent-1");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("work.txt"), "work\n").unwrap();
    std::fs::create_dir_all(root.join("context")).unwrap();
    std::fs::write(root.join("context").join("facts.md"), "facts\n").unwrap();
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let kept = plan(&crate::sandbox::ACCESS_HOOKS, &root, &none).unwrap();
    assert!(kept.kept_out.contains(&root.join("program")), "{kept:?}");
    assert!(
        kept.kept_out
            .iter()
            .all(|path| !path.starts_with(root.join("worktrees"))
                && !path.starts_with(root.join("context"))),
        "{kept:?}"
    );
    let offered = local_folders(&crate::sandbox::ACCESS_HOOKS, &root);
    assert!(offered.contains(&"program".to_owned()), "{offered:?}");
    assert!(
        !offered.contains(&"worktrees".to_owned()) && !offered.contains(&"context".to_owned()),
        "{offered:?}"
    );
    let write = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..SharedFolders::default()
    };
    let opened = plan(&crate::sandbox::ACCESS_HOOKS, &root, &write).unwrap();
    assert!(opened.writes.contains(&root.join("program")), "{opened:?}");
    assert!(
        !opened.writes.contains(&root.join("context"))
            && !opened.writes.contains(&root.join("worktrees")),
        "{opened:?}"
    );

    let (_temp, modern) = project();
    std::fs::create_dir_all(modern.join("context")).unwrap();
    std::fs::write(modern.join("context").join("notes.md"), "mine\n").unwrap();
    let opened = plan(&crate::sandbox::ACCESS_HOOKS, &modern, &write).unwrap();
    assert!(
        !opened.writes.contains(&modern.join("context")),
        "a user's context folder stays readable but never writable: {opened:?}"
    );
    assert!(local_folders(&crate::sandbox::ACCESS_HOOKS, &modern).contains(&"context".to_owned()));
}

/// L4 r1 M1: Read-write never opens a hidden folder the owner's own tools run unconfined
/// (.venv, .vscode, .claude, .codex) or the engine's graphify-out, while node_modules, target
/// and venv open and the help says so (L4 r2 L1); graphify-out is never kept out or offered,
/// and the sandbox refuses a write grant for any of them.
#[cfg(unix)]
#[test]
fn read_write_keeps_hidden_and_engine_folders_read_only() {
    let (_temp, root) = project();
    for path in [
        ".venv/bin/python",
        ".vscode/tasks.json",
        ".claude/settings.json",
        ".codex/config.toml",
        "graphify-out/graph.json",
        "node_modules/left-pad/index.js",
        "target/debug/build.rs",
        "venv/bin/python",
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x\n").unwrap();
    }
    let write = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..SharedFolders::default()
    };
    let opened = plan(&crate::sandbox::ACCESS_HOOKS, &root, &write).unwrap();
    assert!(opened.writes.contains(&root.join("program")), "{opened:?}");
    for open in ["node_modules", "target", "venv"] {
        assert!(
            opened.writes.contains(&root.join(open)),
            "{open}: {opened:?}"
        );
    }
    let help = SharedAccess::ReadWrite.help();
    assert!(
        help.contains("Folders such as node_modules, target and venv stay writable, and your own tools run what is in them outside the sandbox."),
        "L4 r2 L1: the help says which program folders stay writable and who runs them: {help}"
    );
    assert!(!SharedAccess::ReadOnly.help().contains("node_modules"));
    for closed in [".venv", ".vscode", ".claude", ".codex", "graphify-out"] {
        assert!(
            !opened.writes.contains(&root.join(closed)),
            "{closed}: {opened:?}"
        );
        assert!(
            never_written(&crate::sandbox::ACCESS_HOOKS, &root, closed),
            "{closed}"
        );
    }
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let kept = plan(&crate::sandbox::ACCESS_HOOKS, &root, &none).unwrap();
    assert!(
        kept.kept_out
            .iter()
            .all(|path| !path.starts_with(root.join("graphify-out"))),
        "{kept:?}"
    );
    assert!(
        !local_folders(&crate::sandbox::ACCESS_HOOKS, &root).contains(&"graphify-out".to_owned())
    );
    let agent = tempfile::tempdir().unwrap();
    let agent_root = agent.path().canonicalize().unwrap();
    let home = agent_root.join("home");
    let tmp = agent_root.join("tmp");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&tmp).unwrap();
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &root.join(".crazyagent").join("worktrees").join("agent-1"),
        &home,
        &tmp,
        super::super::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(&root)
    .unwrap()
    .keep_project_secrets(&root, &[])
    .unwrap();
    for closed in [".venv", ".claude", "graphify-out"] {
        assert!(
            sandbox
                .clone()
                .allow_shared_write(&root.join(closed))
                .is_err(),
            "{closed}"
        );
    }
    assert!(sandbox.allow_shared_write(&root.join("program")).is_ok());
}

/// Chosen folders keeps out only the untracked paths under folders the owner did not pick.
#[cfg(unix)]
#[test]
fn chosen_folders_keeps_out_only_the_untracked_paths_of_folders_not_picked() {
    let (_temp, root) = project();
    let plan = plan(
        &crate::sandbox::ACCESS_HOOKS,
        &root,
        &chosen(&["program", "src"]),
    )
    .unwrap();
    assert!(plan.kept_out.contains(&root.join("build")), "{plan:?}");
    assert!(plan.kept_out.contains(&root.join("notes.txt")), "{plan:?}");
    assert!(!plan.kept_out.contains(&root.join("program")), "{plan:?}");
    assert!(
        !plan.kept_out.contains(&root.join("src").join("local.rs")),
        "{plan:?}"
    );
    assert!(plan.writes.is_empty(), "Read-only writes nothing: {plan:?}");
}

/// L4 r1 L1: Chosen folders and None keep open only the engine's names, the picked ones,
/// a picked one not made yet included, and the present ones git tracks something in; All
/// sets no such list.
#[cfg(unix)]
#[test]
fn chosen_and_none_keep_open_only_the_engine_picked_and_tracked_top_level_names() {
    let (_temp, root) = project();
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let open = plan(&crate::sandbox::ACCESS_HOOKS, &root, &none)
        .unwrap()
        .open_tops
        .unwrap();
    for name in [".git", ".crazyagent", "graphify-out", "src", ".gitignore"] {
        assert!(open.iter().any(|open| open == name), "{name}: {open:?}");
    }
    for name in ["program", "build", "notes.txt"] {
        assert!(!open.iter().any(|open| open == name), "{name}: {open:?}");
    }
    let open = plan(
        &crate::sandbox::ACCESS_HOOKS,
        &root,
        &chosen(&["program", "later"]),
    )
    .unwrap()
    .open_tops
    .unwrap();
    assert!(open.iter().any(|name| name == "program"), "{open:?}");
    assert!(open.iter().any(|name| name == "later"), "{open:?}");
    assert!(!open.iter().any(|name| name == "build"), "{open:?}");
    let all = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..SharedFolders::default()
    };
    assert_eq!(
        plan(&crate::sandbox::ACCESS_HOOKS, &root, &all)
            .unwrap()
            .open_tops,
        None
    );
}

/// The closed tops rule comes after the keep-outs, keeps the agent's own folder below the
/// project folder open, and the sandbox refuses open names out of shape or without the
/// project folder.
#[cfg(unix)]
#[test]
fn the_closed_tops_rule_keeps_the_own_folder_open_and_refuses_names_out_of_shape() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let project = base.join("project");
    let own = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    let home = base.join("home");
    let tmp = base.join("tmp");
    for folder in [&own, &home, &tmp] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let fresh = || {
        ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &own,
            &home,
            &tmp,
            super::super::SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_read(&project)
        .unwrap()
    };
    assert!(fresh().open_only_tops(&["src".into()]).is_err());
    for bad in ["a/b", "..", "", "x\"y", "back\\slash"] {
        let refused = fresh()
            .keep_project_secrets(&project, &[])
            .unwrap()
            .open_only_tops(&[bad.to_owned()]);
        assert!(refused.is_err(), "{bad:?}");
    }
    let profile = fresh()
        .keep_project_secrets(&project, &[])
        .unwrap()
        .keep_out(&project.join("notes.txt"))
        .unwrap()
        .open_only_tops(&[".crazyagent".into(), "src".into()])
        .unwrap()
        .profile();
    let rule = format!(
        "(require-not (regex #\"^{pattern}/(\\.crazyagent|src)$\"))\n    (require-not (regex #\"^{pattern}/(\\.crazyagent|src)/\"))\n    (require-not (subpath \"{}\"))))",
        own.display(),
        pattern = kernel_sandbox::profile::seatbelt::seatbelt_pattern(&project)
    );
    let closed = profile.find(&rule).expect(&profile);
    let kept = profile
        .find(&format!(
            "(subpath \"{}\")",
            project.join("notes.txt").display()
        ))
        .unwrap();
    assert!(kept < closed, "{profile}");
}

/// Read-write opens only shared top-level folders git does not track at all: a folder with
/// tracked files, a link and a folder not picked stay closed to writes.
#[cfg(unix)]
#[test]
fn read_write_opens_only_wholly_untracked_shared_top_level_folders() {
    let (_temp, root) = project();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
    let all = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..SharedFolders::default()
    };
    let all_plan = plan(&crate::sandbox::ACCESS_HOOKS, &root, &all).unwrap();
    assert!(
        all_plan.writes.contains(&root.join("program")),
        "{all_plan:?}"
    );
    assert!(
        all_plan.writes.contains(&root.join("build")),
        "{all_plan:?}"
    );
    assert!(!all_plan.writes.contains(&root.join("src")), "{all_plan:?}");
    assert!(
        !all_plan.writes.contains(&root.join("linked")),
        "{all_plan:?}"
    );
    assert!(
        !all_plan.writes.contains(&root.join("notes.txt")),
        "{all_plan:?}"
    );
    assert!(all_plan.kept_out.is_empty(), "{all_plan:?}");
    let picked = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..chosen(&["program"])
    };
    let plan = plan(&crate::sandbox::ACCESS_HOOKS, &root, &picked).unwrap();
    assert_eq!(plan.writes, vec![root.join("program")]);
}

/// A launch with more untracked paths to keep out than the bound, or whose paths git cannot
/// list, is refused, never run as All.
#[cfg(unix)]
#[test]
fn a_launch_past_the_keep_out_bound_or_without_git_is_refused() {
    let (_temp, root) = project();
    for index in 0..=LOCAL_PATHS_MAX {
        std::fs::write(root.join(format!("loose-{index}.txt")), "x").unwrap();
    }
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    assert_eq!(
        plan(&crate::sandbox::ACCESS_HOOKS, &root, &none),
        Err(TOO_MANY)
    );
    let outside = tempfile::tempdir().unwrap();
    let missing = outside.path().canonicalize().unwrap().join("gone");
    assert_eq!(
        plan(&crate::sandbox::ACCESS_HOOKS, &missing, &none),
        Err(UNLISTED)
    );
    let loose = outside.path().canonicalize().unwrap().join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::write(loose.join("notes.txt"), "x").unwrap();
    assert_eq!(
        plan(&crate::sandbox::ACCESS_HOOKS, &loose, &none),
        Err(UNLISTED),
        "a folder outside git"
    );
}

/// L4 r1 H1: a read-only run on an empty folder outside git, as instruction cleanup makes,
/// plans under Chosen folders and None with no git: nothing to keep out, and only the
/// engine's and picked names open, so anything made there later stays closed.
#[test]
fn an_empty_folder_plans_without_git_and_keeps_later_entries_closed() {
    let outside = tempfile::tempdir().unwrap();
    let empty = outside.path().canonicalize().unwrap();
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let planned = plan(&crate::sandbox::ACCESS_HOOKS, &empty, &none).unwrap();
    assert!(planned.kept_out.is_empty() && planned.writes.is_empty());
    let open = planned.open_tops.unwrap();
    assert!(
        open.iter().all(
            |name| engine_names(&crate::sandbox::ACCESS_HOOKS, &empty).contains(&name.as_str())
        ),
        "{open:?}"
    );
    let open = plan(&crate::sandbox::ACCESS_HOOKS, &empty, &chosen(&["program"]))
        .unwrap()
        .open_tops
        .unwrap();
    assert!(open.iter().any(|name| name == "program"), "{open:?}");
}

/// A wider choice than All with Read-only is refused off macOS before any grant.
#[test]
fn a_wider_choice_is_refused_off_macos() {
    let temp = tempfile::tempdir().unwrap();
    let root = kernel_sandbox::paths::canonical(temp.path()).unwrap();
    let home = root.join("home");
    let tmp = root.join("tmp");
    let project = root.join("project");
    for folder in [&home, &tmp, &project] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        super::super::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let result = apply(sandbox, &project, &none);
    if cfg!(target_os = "macos") {
        assert_ne!(result.err(), Some(NEEDS_MACOS));
    } else {
        assert_eq!(result.err(), Some(NEEDS_MACOS));
    }
}

/// A launch whose own folder is the project folder itself gets no exemption for it, so the
/// secret names stay closed there too; only `.git` is exempt.
#[cfg(unix)]
#[test]
fn a_launch_working_in_the_project_folder_itself_keeps_its_secrets_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let tmp = root.join("tmp");
    let project = root.join("project");
    for folder in [&home, &tmp, &project] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        super::super::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .keep_project_secrets(&project, &[])
    .unwrap();
    let profile = sandbox.profile();
    assert!(profile.contains(&format!(
        "(require-not (subpath \"{}/.git\"))",
        project.display()
    )));
    assert!(
        !profile.contains(&format!(
            "(require-not (subpath \"{}\"))",
            project.display()
        )),
        "{profile}"
    );
}

/// The Chosen folders picker offers the project's top-level real folders with plain names,
/// sorted, never `.git`, `.crazyagent` or a link.
#[cfg(unix)]
#[test]
fn the_picker_offers_top_level_real_folders_without_engine_folders_or_links() {
    let (_temp, root) = project();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
    assert_eq!(
        local_folders(&crate::sandbox::ACCESS_HOOKS, &root),
        vec!["build", "elsewhere", "program", "src"]
    );
}

/// DS-148 code F1: an engine name spelled with a code point HFS+ ignores in names, or in
/// another letter case, is never written and never a plain chosen name, since such a
/// volume opens it as the engine's own folder.
#[test]
fn an_engine_name_in_any_spelling_the_volume_opens_is_never_written_or_chosen() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    for name in [
        "\u{feff}.git",
        "graphify\u{200d}-out",
        "GRAPHIFY-OUT",
        "cont\u{200c}ext",
    ] {
        assert!(
            never_written(&crate::sandbox::ACCESS_HOOKS, root, name),
            "{name}"
        );
    }
    for name in [
        "\u{feff}.git",
        "\u{200c}.crazyagent",
        "graphify\u{feff}-out",
    ] {
        assert!(!plain_name(name), "{name}");
    }
}
