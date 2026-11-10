use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs the system git in a folder with a clean configuration, plus the given variables, as
/// a process the IDE server starts would. Inputs: the folder, the extra variables and the
/// arguments. Output: what git printed and its status.
fn git(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .envs(env.iter().copied())
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let output = git(dir, &[], args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ide_git_never_enters_a_repository_planted_below_a_worktree() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let worktree = base.join("worktree");
    std::fs::create_dir(&worktree).unwrap();
    ok(&worktree, &["init", "-q"]);
    ok(&worktree, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let planted = worktree.join("planted");
    std::fs::create_dir(&planted).unwrap();
    ok(&planted, &["init", "-q"]);
    std::fs::write(planted.join("notes.txt"), "first\n").unwrap();
    ok(&planted, &["add", "notes.txt"]);
    ok(&planted, &["commit", "-q", "-m", "planted"]);
    std::fs::write(
        worktree.join(".gitmodules"),
        "[submodule \"planted\"]\n\tpath = planted\n\turl = ./planted\n",
    )
    .unwrap();
    ok(&worktree, &["add", ".gitmodules", "planted"]);
    ok(&worktree, &["commit", "-q", "-m", "gitlink"]);
    let marker = base.join("fsmonitor-ran");
    let hook = base.join("fsmonitor.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    ok(
        &planted,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    std::fs::write(planted.join("notes.txt"), "changed\n").unwrap();

    let guarded = git(&worktree, &CONFIG_ENV, &["status", "--porcelain"]);
    assert!(guarded.status.success());
    assert_eq!(String::from_utf8_lossy(&guarded.stdout), "");
    assert!(
        !marker.exists(),
        "the planted repository's fsmonitor never runs under the IDE's git configuration"
    );
    let guarded_diff = git(&worktree, &CONFIG_ENV, &["diff", "--stat"]);
    assert_eq!(String::from_utf8_lossy(&guarded_diff.stdout), "");
    assert!(!marker.exists());

    let open = git(&worktree, &[], &["status", "--porcelain"]);
    assert!(open.status.success());
    assert_eq!(String::from_utf8_lossy(&open.stdout), " M planted\n");
    assert!(
        marker.exists(),
        "without it, git enters the planted repository and runs its fsmonitor"
    );
}

#[test]
fn the_ide_git_settings_turn_off_nested_repository_and_submodule_detection() {
    assert_eq!(
        Value::Object(workspace()),
        serde_json::json!({
            "git.detectSubmodules": false,
            "git.autoRepositoryDetection": false,
        })
    );
    let count: usize = CONFIG_ENV[0].1.parse().unwrap();
    assert_eq!(CONFIG_ENV.len(), 1 + 2 * count);
    assert!(
        CONFIG_ENV
            .iter()
            .all(|(name, _)| name.starts_with("GIT_CONFIG_"))
    );
}

/// Writes an executable script that records that it ran by creating `marker`, then passes
/// its input through. Inputs: the script path and the marker. Output: none.
fn planted(script: &Path, marker: &Path) {
    std::fs::write(
        script,
        format!("#!/bin/sh\n: > '{}'\nexec cat\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn the_ide_git_configuration_runs_no_planted_program_and_refuses_a_bare_looking_folder() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    ok(&repo, &["init", "-q"]);
    ok(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let marks = base.join("marks");
    std::fs::create_dir(&marks).unwrap();
    let hooks = repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for name in ["pre-commit", "post-index-change"] {
        planted(&hooks.join(name), &marks.join(name));
    }
    planted(&base.join("fsmonitor.sh"), &marks.join("fsmonitor"));
    planted(&base.join("sign.sh"), &marks.join("sign"));
    ok(
        &repo,
        &[
            "config",
            "core.fsmonitor",
            base.join("fsmonitor.sh").to_str().unwrap(),
        ],
    );
    ok(
        &repo,
        &[
            "config",
            "gpg.program",
            base.join("sign.sh").to_str().unwrap(),
        ],
    );
    std::fs::write(repo.join("notes.txt"), "first\n").unwrap();
    let ran = |name: &str| marks.join(name).exists();

    let added = git(&repo, &CONFIG_ENV, &["add", "notes.txt"]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let committed = git(&repo, &CONFIG_ENV, &["commit", "-q", "-m", "guarded"]);
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let signed = git(
        &repo,
        &CONFIG_ENV,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(!signed.status.success());
    for name in ["pre-commit", "post-index-change", "fsmonitor", "sign"] {
        assert!(!ran(name), "{name} ran under the IDE's git configuration");
    }

    std::fs::write(repo.join("notes.txt"), "second\n").unwrap();
    assert!(git(&repo, &[], &["status", "--porcelain"]).status.success());
    assert!(git(&repo, &[], &["add", "notes.txt"]).status.success());
    assert!(
        git(&repo, &[], &["commit", "-q", "-m", "open"])
            .status
            .success()
    );
    assert!(
        !git(
            &repo,
            &[],
            &["commit", "-q", "-S", "--allow-empty", "-m", "signed"]
        )
        .status
        .success()
    );
    for name in ["pre-commit", "post-index-change", "fsmonitor", "sign"] {
        assert!(ran(name), "without it, git runs the planted {name}");
    }

    let bare = base.join("agent");
    std::fs::create_dir(&bare).unwrap();
    ok(&bare, &["init", "-q", "--bare"]);
    ok(&bare, &["config", "core.bare", "false"]);
    ok(&bare, &["config", "core.worktree", bare.to_str().unwrap()]);
    planted(
        &base.join("bare-fsmonitor.sh"),
        &marks.join("bare-fsmonitor"),
    );
    ok(
        &bare,
        &[
            "config",
            "core.fsmonitor",
            base.join("bare-fsmonitor.sh").to_str().unwrap(),
        ],
    );
    std::fs::write(bare.join("notes.txt"), "planted\n").unwrap();
    let guarded = git(&bare, &CONFIG_ENV, &["status", "--porcelain"]);
    assert!(!guarded.status.success());
    assert!(
        String::from_utf8_lossy(&guarded.stderr).contains("cannot use bare repository"),
        "{}",
        String::from_utf8_lossy(&guarded.stderr)
    );
    assert!(!ran("bare-fsmonitor"));
    assert!(git(&bare, &[], &["status", "--porcelain"]).status.success());
    assert!(
        ran("bare-fsmonitor"),
        "without it, git uses the bare-looking folder and runs its fsmonitor"
    );
}

/// FZ-7: Off and On, the shim's last line runs the absolute git the engine resolved, and
/// it keeps the PATH's git for the `/usr/bin/git` xcrun shim itself or for a path that a
/// single-quoted shell word cannot hold.
#[test]
fn the_shim_runs_the_resolved_git_and_keeps_the_path_git_otherwise() {
    let tools = Path::new("/Library/Developer/CommandLineTools/usr/bin/git");
    let exec = "exec '/Library/Developer/CommandLineTools/usr/bin/git' \"$@\"\n";
    let head = SHIM_TEXT.strip_suffix("exec git \"$@\"\n").unwrap();
    assert_eq!(
        shim_text(None, tools, None).unwrap(),
        format!("{head}{exec}")
    );
    let on = shim_text(Some(Path::new("/projects/alpha")), tools, None).unwrap();
    assert!(on.starts_with(head), "{on}");
    assert!(
        on.ends_with(&format!(
            "if [ \"$(pwd -P)\" = '/projects/alpha' ]; then\n  GIT_CONFIG_COUNT={PROJECT_HOOKS_COUNT}\n  export GIT_CONFIG_COUNT\nfi\n{exec}"
        )),
        "{on}"
    );
    let odd = std::ffi::OsStr::from_bytes(b"/odd/\xff/git");
    for kept in [
        Path::new("/usr/bin/git"),
        Path::new("/Applications/Xcode it's/git"),
        Path::new("/odd/line\nbreak/git"),
        Path::new(odd),
    ] {
        assert_eq!(shim_text(None, kept, None).unwrap(), SHIM_TEXT, "{kept:?}");
    }
}

/// The engine only ever hands the shim an absolute git.
#[test]
#[should_panic(expected = "the engine resolves git to an absolute path")]
fn the_shim_refuses_a_relative_git() {
    let _ = shim_text(None, Path::new("git"), None);
}

#[test]
fn the_shim_lets_git_run_hooks_in_the_project_folder_alone() {
    let path_git = Path::new(crate::research::runner::GIT);
    assert_eq!(shim_text(None, path_git, None).unwrap(), SHIM_TEXT);
    for refused in ["relative/project", "/project/it's", "/project/line\nbreak"] {
        assert!(
            shim_text(Some(Path::new(refused)), path_git, None).is_err(),
            "{refused:?}"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    let agent = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    std::fs::create_dir_all(&agent).unwrap();
    ok(&project, &["init", "-q"]);
    let on = base.join("on");
    let off = base.join("off");
    for (shim, text) in [
        (&on, shim_text(Some(&project), path_git, None).unwrap()),
        (&off, shim_text(None, path_git, None).unwrap()),
    ] {
        assert!(text.starts_with("#!/bin/sh\n") && text.ends_with("exec git \"$@\"\n"));
        std::fs::write(shim, text).unwrap();
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let hooks_path = |shim: &Path, dir: &Path| {
        let output = crate::spawn_gate::output(
            Command::new(shim)
                .current_dir(dir)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .args(["config", "--get", "core.hooksPath"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    let unset = (Some(1), String::new(), String::new());
    let forced = (Some(0), "/dev/null\n".to_owned(), String::new());
    assert_eq!(
        hooks_path(&on, &project),
        unset,
        "git ran and found the key unset"
    );
    assert_eq!(hooks_path(&on, &agent), forced);
    assert_eq!(hooks_path(&on, &project.join(".git")), forced);
    assert_eq!(hooks_path(&off, &project), forced);
}

/// CA-SEC-004 r2: a project's hook under the git the shim runs does not hold the relay's
/// secret, while the forwarder a network command goes to still does.
#[test]
fn the_shim_keeps_the_relay_secret_from_git_and_hands_it_to_the_forwarder() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    std::fs::create_dir_all(&project).unwrap();
    ok(&project, &["init", "-q"]);
    let seen = base.join("seen");
    let print = |target: &str| {
        format!("#!/bin/sh\nprintf '%s' \"${{CRAZYAGENT_IDE_GIT_SECRET-absent}}\"{target}\n")
    };
    let hook = project.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let forwarder = base.join("forwarder");
    let shim = base.join("shim");
    let path_git = Path::new(crate::research::runner::GIT);
    for (file, text) in [
        (&hook, print(&format!(" > '{}'", seen.display()))),
        (&forwarder, print("")),
        (&shim, shim_text(Some(&project), path_git, None).unwrap()),
    ] {
        std::fs::write(file, text).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let run = |args: &[&str]| {
        crate::spawn_gate::output(
            Command::new(&shim)
                .current_dir(&project)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &project)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .env("CRAZYAGENT_IDE_GIT_SECRET", "dummy-secret")
                .env("CRAZYAGENT_IDE_GIT_NODE", "/bin/sh")
                .env("CRAZYAGENT_IDE_GIT_FORWARD", &forwarder)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap()
    };
    let committed = run(&[
        "-c",
        "user.name=Owner",
        "-c",
        "user.email=owner@example.invalid",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "Plant beds",
    ]);
    assert!(committed.status.success(), "{committed:?}");
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "absent");
    let pushed = run(&["push", "origin", "main"]);
    assert!(pushed.status.success(), "{pushed:?}");
    assert_eq!(String::from_utf8_lossy(&pushed.stdout), "dummy-secret");
}

/// INT1-1, INT1-R2-1: a git that cannot read an agent folder's `.git` treats it as missing
/// and walks up to the project's repository. The ceiling is the agents root of the project's
/// own layout: git in an agent folder finds no repository, while the project folder and its
/// other folders keep theirs, a `.crazyagent` project's own top-level `worktrees` folder among
/// them; a folder holding a `:` or a relative one is refused.
#[test]
fn the_ceiling_stops_git_at_the_layouts_agents_root_and_keeps_the_project() {
    assert!(ceiling(Path::new("relative/project"), false).is_err());
    assert!(ceiling(Path::new("/projects/a:b"), true).is_err());
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let newer = base.join("newer");
    let older = base.join("older");
    let layouts = [
        (
            &newer,
            newer.join(".crazyagent").join("worktrees"),
            newer.join("worktrees").join("sub"),
        ),
        (&older, older.join("worktrees"), older.join("src")),
    ];
    for (project, agents, own) in &layouts {
        let agent = agents.join("agent-1");
        for folder in [&agent, own, &project.join("src")] {
            std::fs::create_dir_all(folder).unwrap();
        }
        ok(project, &["init", "-q"]);
        let value = ceiling(project, false).unwrap();
        assert_eq!(&value, agents.as_os_str());
        let top = |dir: &Path, bounded: bool| {
            let env: Vec<(&str, &str)> = if bounded {
                vec![(CEILING_VARIABLE, value.to_str().unwrap())]
            } else {
                Vec::new()
            };
            git(dir, &env, &["rev-parse", "--show-toplevel"])
        };
        let open = top(&agent, false);
        assert!(open.status.success());
        assert_eq!(
            String::from_utf8_lossy(&open.stdout).trim(),
            project.to_str().unwrap(),
            "without it, git in an agent folder acts on the project"
        );
        let bounded = top(&agent, true);
        assert!(!bounded.status.success(), "{}", agent.display());
        assert!(
            String::from_utf8_lossy(&bounded.stderr).contains("not a git repository"),
            "{}",
            String::from_utf8_lossy(&bounded.stderr)
        );
        for dir in [*project, own, &project.join("src")] {
            let bounded = top(dir, true);
            assert!(bounded.status.success(), "{}", dir.display());
            assert_eq!(
                String::from_utf8_lossy(&bounded.stdout).trim(),
                project.to_str().unwrap()
            );
        }
    }
}

/// INT1-R3-1: an older-layout project that gains a `.crazyagent` folder while the engine
/// still records an agent in `<project>/worktrees` keeps that root in the ceiling after the
/// layout's own, so git in the agent's folder, whose `.git` the IDE cannot read, still finds
/// no repository. Without such a record the ceiling is the layout's root alone, and an
/// older-layout project names its one root once.
#[test]
fn the_ceiling_keeps_the_older_agents_root_while_the_engine_records_agents_there() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("gained");
    let newer = project.join(".crazyagent").join("worktrees");
    let older = project.join("worktrees");
    let agent = older.join("agent-3");
    for folder in [&newer, &agent] {
        std::fs::create_dir_all(folder).unwrap();
    }
    ok(&project, &["init", "-q"]);
    assert_eq!(ceiling(&project, false).unwrap(), newer.as_os_str());
    let both = ceiling(&project, true).unwrap();
    assert_eq!(
        both.to_str().unwrap(),
        format!("{}:{}", newer.display(), older.display())
    );
    let top = |value: &std::ffi::OsStr| {
        git(
            &agent,
            &[(CEILING_VARIABLE, value.to_str().unwrap())],
            &["rev-parse", "--show-toplevel"],
        )
    };
    let reached = top(&ceiling(&project, false).unwrap());
    assert!(reached.status.success());
    assert_eq!(
        String::from_utf8_lossy(&reached.stdout).trim(),
        project.to_str().unwrap(),
        "the layout's root alone lets git in the older agent's folder act on the project"
    );
    let bounded = top(&both);
    assert!(!bounded.status.success());
    assert!(
        String::from_utf8_lossy(&bounded.stderr).contains("not a git repository"),
        "{}",
        String::from_utf8_lossy(&bounded.stderr)
    );
    let own = git(
        &project,
        &[(CEILING_VARIABLE, both.to_str().unwrap())],
        &["rev-parse", "--show-toplevel"],
    );
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        project.to_str().unwrap(),
        "the project folder keeps its repository"
    );

    let plain = base.join("plain");
    std::fs::create_dir_all(plain.join("worktrees")).unwrap();
    assert_eq!(
        ceiling(&plain, true).unwrap(),
        plain.join("worktrees").as_os_str()
    );
}

/// Makes a file look a year old. Input: the file. Output: none.
fn age(path: &Path) {
    let year = std::time::Duration::from_secs(365 * 24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - year)
        .unwrap();
}

/// IN1-FN1: the IDE cannot read an agent worktree's `.git`, so its git sees the worktree as
/// missing. Through the shim and the IDE's git configuration, `worktree prune` and
/// `worktree remove` are refused and a gc keeps the worktree's registration, while git
/// without them prunes it.
#[test]
fn the_ide_git_never_prunes_an_agent_worktree_it_sees_as_missing() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    std::fs::create_dir(&project).unwrap();
    ok(&project, &["init", "-q"]);
    ok(&project, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let agents = project.join(".crazyagent").join("worktrees");
    let (gone, kept) = (agents.join("agent-4"), agents.join("agent-5"));
    for (agent, branch) in [(&gone, "agent-4"), (&kept, "agent-5")] {
        ok(
            &project,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                agent.to_str().unwrap(),
            ],
        );
    }
    let admin = project.join(".git").join("worktrees");
    std::fs::remove_file(gone.join(".git")).unwrap();
    age(&admin.join("agent-4").join("index"));
    let shim = base.join("git");
    std::fs::write(&shim, SHIM_TEXT).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ide = |args: &[&str]| {
        crate::spawn_gate::output(
            Command::new(&shim)
                .current_dir(&project)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &project)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap()
    };
    let said = |output: &Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    let dry = git(
        &project,
        &[],
        &["worktree", "prune", "-n", "-v", "--expire=3.months.ago"],
    );
    assert!(said(&dry).contains("agent-4"), "{}", said(&dry));
    for args in [
        &["worktree", "prune"][..],
        &["worktree", "prune", "-v", "--expire=now"],
        &[
            "worktree",
            "remove",
            "--force",
            "--force",
            kept.to_str().unwrap(),
        ],
    ] {
        let refused = ide(args);
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            said(&refused).contains("the IDE does not run git worktree"),
            "{}",
            said(&refused)
        );
    }
    assert!(kept.join(".git").is_file());
    let listed = ide(&["worktree", "list"]);
    assert!(listed.status.success(), "{}", said(&listed));
    assert_eq!(
        String::from_utf8_lossy(&ide(&["config", "--get", "gc.worktreePruneExpire"]).stdout),
        "never\n"
    );
    let collected = ide(&["gc", "-q"]);
    assert!(collected.status.success(), "{}", said(&collected));
    assert!(admin.join("agent-4").join("gitdir").is_file());
    assert!(admin.join("agent-5").join("gitdir").is_file());

    let open = git(&project, &[], &["gc", "-q"]);
    assert!(open.status.success(), "{}", said(&open));
    assert!(
        !admin.join("agent-4").exists(),
        "without it, gc prunes the worktree it sees as missing"
    );
    assert!(admin.join("agent-5").join("gitdir").is_file());
}

/// IN1-FN1 after git's own leading options: `worktree prune` and `worktree remove` stay
/// refused when `-c`, `-C`, `--git-dir` or another option comes before the verb, through the
/// shim's head alone (a Full access server, or any folder) and through the project folder's
/// private index flow with a forwarder, so no agent worktree loses its unsaved work.
#[test]
fn the_ide_git_refuses_worktree_prune_and_remove_after_a_leading_option() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    let private = base.join("private");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&private).unwrap();
    ok(&project, &["init", "-q"]);
    ok(&project, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let agent = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-6");
    let place = agent.to_str().unwrap();
    ok(&project, &["worktree", "add", "-q", "-b", "agent-6", place]);
    let unsaved = agent.join("unsaved.txt");
    std::fs::write(&unsaved, "agent work\n").unwrap();
    let folder = project.to_str().unwrap();
    let forwarder = [
        ("CRAZYAGENT_IDE_GIT_NODE", "/usr/bin/true"),
        ("CRAZYAGENT_IDE_GIT_FORWARD", "/dev/null"),
        ("CRAZYAGENT_IDE_GIT_SECRET", "synthetic"),
    ];
    let head = base.join("head-git");
    let composed = base.join("composed-git");
    let flow = shim_text(None, Path::new("/usr/bin/git"), Some((&project, &private))).unwrap();
    for (shim, text) in [(&head, SHIM_TEXT), (&composed, flow.as_str())] {
        std::fs::write(shim, text).unwrap();
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let ide = |shim: &Path, args: &[&str]| {
        let output = crate::spawn_gate::output(
            Command::new(shim)
                .current_dir(&project)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &project)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .envs(forwarder)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), said)
    };

    let remove = ["worktree", "remove", "--force", "--force", place];
    for (shim, options) in [
        (&head, &["-c", "color.ui=never"][..]),
        (&head, &["-C", folder][..]),
        (&head, &["--git-dir", ".git"][..]),
        (&head, &["--git-dir=.git", "--literal-pathspecs"][..]),
        (&head, &["--work-tree", folder][..]),
        (&head, &["--namespace", "seed"][..]),
        (&head, &["--config-env", "color.ui=HOME"][..]),
        (&head, &["--attr-source", "HEAD"][..]),
        (&head, &["--shallow-file", "none"][..]),
        (&composed, &["-c", "color.ui=never"][..]),
        (&composed, &["-C", folder][..]),
        (&composed, &["--work-tree", folder][..]),
        (&composed, &["--namespace", "seed"][..]),
        (&composed, &["--config-env", "color.ui=HOME"][..]),
        (&composed, &["--attr-source", "HEAD"][..]),
        (&composed, &["--shallow-file", "none"][..]),
    ] {
        for verb in [&remove[..], &["worktree", "prune", "-v", "--expire=now"]] {
            let args = [options, verb].concat();
            let (ran, said) = ide(shim, &args);
            assert!(!ran, "{shim:?} {args:?}: {said}");
            assert!(
                said.contains("the IDE does not run git worktree"),
                "{shim:?} {args:?}: {said}"
            );
            assert!(unsaved.is_file(), "{shim:?} {args:?}");
        }
    }
    assert!(agent.join(".git").is_file());
    assert!(project.join(".git/worktrees/agent-6/gitdir").is_file());
    let (listed, said) = ide(&head, &["-c", "color.ui=never", "worktree", "list"]);
    assert!(listed, "{said}");
    assert!(said.contains(place), "{said}");
}

/// shim-wtremove r1 L1: a network command after git's own leading options never runs the
/// IDE's own git, which would fetch outside the engine at Full access, nor the forwarder,
/// which takes its words as given: the shim stops it with the plain line, in the head and
/// in the project folder's flow, while the same command with no option still goes to the
/// forwarder.
#[test]
fn a_network_command_after_a_leading_option_from_the_ide_stops_with_the_plain_line() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    let private = base.join("private");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&private).unwrap();
    ok(&project, &["init", "-q"]);
    let folder = project.to_str().unwrap();
    let head = base.join("head-git");
    let composed = base.join("composed-git");
    let flow = shim_text(None, Path::new("/usr/bin/git"), Some((&project, &private))).unwrap();
    for (shim, text) in [(&head, SHIM_TEXT), (&composed, flow.as_str())] {
        std::fs::write(shim, text).unwrap();
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let ide = |shim: &Path, args: &[&str]| {
        let output = crate::spawn_gate::output(
            Command::new(shim)
                .current_dir(&project)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &project)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .env("CRAZYAGENT_IDE_GIT_NODE", "/usr/bin/true")
                .env("CRAZYAGENT_IDE_GIT_FORWARD", "/dev/null")
                .env("CRAZYAGENT_IDE_GIT_SECRET", "synthetic")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), said)
    };

    for shim in [&head, &composed] {
        for verb in ["push", "pull", "fetch", "ls-remote"] {
            let (forwarded, said) = ide(shim, &[verb]);
            assert!(forwarded, "{shim:?} {verb}: {said}");
            assert!(said.is_empty(), "{shim:?} {verb}: {said}");
            for options in [
                &["-c", "color.ui=never"][..],
                &["-C", folder][..],
                &["--git-dir", ".git"][..],
                &["--no-pager"][..],
            ] {
                let args = [options, &[verb][..]].concat();
                let (ran, said) = ide(shim, &args);
                assert!(!ran, "{shim:?} {args:?}: {said}");
                assert!(
                    said.contains(&format!("CrazyAgent runs git {verb} in its engine")),
                    "{shim:?} {args:?}: {said}"
                );
            }
        }
    }
}

/// INT1-3: in a project that signs every commit and tag, git under the IDE's configuration
/// still commits and tags, unsigned and without running the signing program, as the hooks
/// setting's line says; git without it tries to sign and fails.
#[test]
fn the_ide_git_commits_and_tags_unsigned_in_a_project_that_signs_everything() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    ok(&repo, &["init", "-q"]);
    let marker = base.join("signed");
    planted(&base.join("sign.sh"), &marker);
    ok(
        &repo,
        &[
            "config",
            "gpg.program",
            base.join("sign.sh").to_str().unwrap(),
        ],
    );
    for key in ["commit.gpgSign", "tag.gpgSign", "tag.forceSignAnnotated"] {
        ok(&repo, &["config", key, "true"]);
    }

    let committed = git(
        &repo,
        &CONFIG_ENV,
        &["commit", "-q", "--allow-empty", "-m", "ide"],
    );
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let tagged = git(&repo, &CONFIG_ENV, &["tag", "-a", "-m", "ide", "ide"]);
    assert!(
        tagged.status.success(),
        "{}",
        String::from_utf8_lossy(&tagged.stderr)
    );
    assert!(!ok(&repo, &["cat-file", "commit", "HEAD"]).contains("gpgsig"));
    assert!(!ok(&repo, &["cat-file", "tag", "ide"]).contains("BEGIN PGP"));
    assert!(!marker.exists(), "the IDE never runs the signing program");

    let open = git(&repo, &[], &["commit", "-q", "--allow-empty", "-m", "open"]);
    assert!(!open.status.success());
    assert!(
        String::from_utf8_lossy(&open.stderr).contains("gpg failed to sign"),
        "{}",
        String::from_utf8_lossy(&open.stderr)
    );
    assert!(
        marker.exists(),
        "without it, git signs with the project's program"
    );
}

/// SECAUDIT-IDE-SERVER-INDEX: with a private index the shim runs git on a copy, asks the
/// engine only through its forwarder, and refuses a folder a shell word cannot hold.
#[test]
fn the_shim_works_on_a_private_copy_of_the_index() {
    let tools = Path::new("/Library/Developer/CommandLineTools/usr/bin/git");
    let (project, private) = (Path::new("/projects/alpha"), Path::new("/data/git-index"));
    let text = shim_text(None, tools, Some((project, private))).unwrap();
    for expected in [
        "GIT_INDEX_FILE=\"$work\" '/Library/Developer/CommandLineTools/usr/bin/git' \"$@\"",
        "crazyagent-index \"$$\"",
        "private='/data/git-index'",
        "if [ \"$(pwd -P)\" != '/projects/alpha' ]; then",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(text.ends_with("exit \"$step\"\n"), "{text}");
    for bad in [
        Path::new("relative"),
        Path::new("/it's"),
        Path::new("/a\nb"),
    ] {
        assert!(shim_text(None, tools, Some((bad, private))).is_err());
        assert!(shim_text(None, tools, Some((project, bad))).is_err());
    }
}

/// SECAUDIT-IDE-SERVER-INDEX: git and anything under it never hold the relay's secret, only
/// the forwarder does, and the shim exits with git's failure or else the engine step's.
#[test]
fn the_shim_keeps_the_secret_from_git_and_reports_both_failures() {
    let scratch = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(scratch.path()).unwrap();
    let (project, private) = (base.join("project"), base.join("private"));
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir(&private).unwrap();
    std::fs::write(project.join(".git").join("index"), "real").unwrap();
    let (saw_git, saw_forwarder) = (base.join("saw-git"), base.join("saw-forwarder"));
    let script = |name: &str, body: String| {
        let path = base.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    for (git_exit, step_exit, expected) in [(0, 0, 0), (0, 7, 7), (3, 7, 3)] {
        let fake = script(
            "fake-git",
            format!(
                "printf '%s' \"${{CRAZYAGENT_IDE_GIT_SECRET-absent}}\" > '{}'\nprintf changed >> \"$GIT_INDEX_FILE\"\nexit {git_exit}\n",
                saw_git.display()
            ),
        );
        let forwarder = script(
            "forward.sh",
            format!(
                "printf '%s' \"${{CRAZYAGENT_IDE_GIT_SECRET-absent}}\" > '{}'\nexit {step_exit}\n",
                saw_forwarder.display()
            ),
        );
        let shim = script(
            "shim",
            shim_text(None, &fake, Some((&project, &private)))
                .unwrap()
                .strip_prefix("#!/bin/sh\n")
                .unwrap()
                .to_owned(),
        );
        let output = crate::spawn_gate::output(
            Command::new(&shim)
                .arg("add")
                .current_dir(&project)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("CRAZYAGENT_IDE_GIT_NODE", "/bin/sh")
                .env("CRAZYAGENT_IDE_GIT_FORWARD", &forwarder)
                .env("CRAZYAGENT_IDE_GIT_SECRET", "row-secret")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        assert_eq!(std::fs::read_to_string(&saw_git).unwrap(), "absent");
        assert_eq!(
            std::fs::read_to_string(&saw_forwarder).unwrap(),
            "row-secret"
        );
        assert!(std::fs::read_dir(&private).unwrap().next().is_none());
    }
}
