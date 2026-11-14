#![cfg(unix)]

use crazyagent::projects_home::*;
use crazyagent::protocol::ProjectId;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    sources: PathBuf,
    engine: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let sources = root.join("sources");
    let engine = root.join("engine");
    for folder in [&home, &sources, &engine] {
        fs::create_dir(folder).unwrap();
    }
    Case {
        _temp: temp,
        root,
        home,
        sources,
        engine,
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn sample(folder: &Path) {
    fs::create_dir_all(folder.join("src").join("deep")).unwrap();
    fs::write(folder.join("README.md"), "hello\n").unwrap();
    fs::write(folder.join("src").join("main.rs"), vec![7u8; 200_000]).unwrap();
    fs::write(
        folder.join("src").join("deep").join("run.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    fs::set_permissions(
        folder.join("src").join("deep").join("run.sh"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    fs::set_permissions(folder.join("README.md"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(folder.join("src"), fs::Permissions::from_mode(0o750)).unwrap();
    symlink("../README.md", folder.join("src").join("readme-link")).unwrap();
    symlink("/etc/hosts", folder.join("outside-link")).unwrap();
    symlink("missing-target", folder.join("dangling")).unwrap();
}

fn git(folder: &Path, args: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(["-c", "init.defaultBranch=main"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn wait_for(watch: &HomeWatch, want: impl Fn(&[String]) -> bool) -> Duration {
    let start = Instant::now();
    let (mut generation, mut folders) = watch.folders();
    while !want(&folders) {
        assert!(start.elapsed() < Duration::from_secs(10), "{folders:?}");
        generation = watch.wait_changed(generation, Duration::from_millis(500));
        folders = watch.folders().1;
    }
    start.elapsed()
}

#[test]
fn a_scan_lists_plain_top_level_folders_with_git_and_setup_flags() {
    let case = case();
    fs::create_dir_all(case.home.join("alpha").join(".git")).unwrap();
    fs::create_dir_all(case.home.join("beta").join(CRAZYAGENT_DIRECTORY)).unwrap();
    fs::create_dir(case.home.join(".hidden")).unwrap();
    fs::create_dir(case.home.join(".crazyagent-adding-00")).unwrap();
    fs::write(case.home.join("notes.txt"), "x").unwrap();
    symlink(case.home.join("alpha"), case.home.join("link")).unwrap();

    let folders = scan(&case.home).unwrap();
    assert_eq!(folders, ["alpha", "beta"]);
    let entries = found(Some(case.home.as_path()), &folders, &[]);
    assert_eq!(
        entries,
        [
            FoundProject {
                name: "alpha".into(),
                path: text(&case.home.join("alpha")).into(),
                git: true,
                known: false,
                state: FoundState::Found,
                project_id: None,
            },
            FoundProject {
                name: "beta".into(),
                path: text(&case.home.join("beta")).into(),
                git: false,
                known: true,
                state: FoundState::Found,
                project_id: None,
            },
        ]
    );
    assert!(scan(&case.root.join("absent")).is_err());
}

#[test]
fn the_watch_sees_a_new_folder_and_a_removed_one_within_seconds() {
    let case = case();
    fs::create_dir(case.home.join("first")).unwrap();
    let watch = HomeWatch::start(&case.home).unwrap();
    assert_eq!(watch.home(), case.home.as_path());
    assert!(watch.watching());
    assert_eq!(watch.folders().1, ["first"]);

    fs::create_dir(case.home.join("dropped")).unwrap();
    let seen = wait_for(&watch, |folders| folders.contains(&"dropped".to_owned()));
    assert!(seen < Duration::from_secs(10), "{seen:?}");

    fs::create_dir(case.sources.join("finder")).unwrap();
    fs::rename(case.sources.join("finder"), case.home.join("finder")).unwrap();
    wait_for(&watch, |folders| folders.contains(&"finder".to_owned()));

    fs::remove_dir(case.home.join("dropped")).unwrap();
    wait_for(&watch, |folders| !folders.contains(&"dropped".to_owned()));
    fs::write(case.home.join("file.txt"), "x").unwrap();
    fs::create_dir(case.home.join(".quiet")).unwrap();
    fs::create_dir(case.home.join("last")).unwrap();
    wait_for(&watch, |folders| folders.contains(&"last".to_owned()));
    assert_eq!(watch.folders().1, ["finder", "first", "last"]);

    let (generation, _) = watch.folders();
    assert_eq!(
        watch.wait_changed(generation, Duration::from_millis(200)),
        generation
    );
}

#[test]
fn set_up_projects_show_as_ready_onboarding_or_missing() {
    let case = case();
    for name in ["ready", "onboarding", "fresh"] {
        fs::create_dir(case.home.join(name)).unwrap();
    }
    fs::create_dir(case.engine.join("elsewhere")).unwrap();
    let project = |id: &str, folder: PathBuf, onboarding: bool| Registered {
        project_id: ProjectId::new(id).unwrap(),
        name: format!("Project {id}"),
        repositories: vec![folder.clone()],
        folder,
        onboarding,
    };
    let registered = [
        project("ready", case.home.join("ready"), false),
        project("onboarding", case.home.join("onboarding"), true),
        project("gone", case.home.join("gone"), false),
        project("old", case.engine.join("vanished"), false),
        project("elsewhere", case.engine.join("elsewhere"), false),
    ];
    let folders = scan(&case.home).unwrap();
    let entries = found(Some(case.home.as_path()), &folders, &registered);
    let view: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.state,
                entry.project_id.as_ref().map(|id| id.as_str().to_owned()),
            )
        })
        .collect();
    assert_eq!(
        view,
        [
            ("Project gone", FoundState::Missing, Some("gone".to_owned())),
            ("Project old", FoundState::Missing, Some("old".to_owned())),
            ("fresh", FoundState::Found, None),
            (
                "onboarding",
                FoundState::Onboarding,
                Some("onboarding".to_owned())
            ),
            ("ready", FoundState::Ready, Some("ready".to_owned())),
        ]
    );
    let gone = entries
        .iter()
        .find(|entry| entry.name == "Project gone")
        .unwrap();
    assert_eq!(gone.path, text(&case.home.join("gone")));
    assert!(!gone.git && !gone.known);
    assert!(found(None, &[], &registered[..1]).is_empty());
}

#[test]
fn move_renames_into_the_home_and_copy_keeps_the_source_and_its_links() {
    let case = case();
    let moved = case.sources.join("moved");
    sample(&moved);
    let plan = plan_add(
        &case.home,
        &case.engine,
        text(&moved),
        AddMode::Move,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(plan.source(), moved.as_path());
    assert_eq!(plan.destination(), case.home.join("moved").as_path());
    plan.run().unwrap();
    assert!(!moved.exists());
    assert_eq!(
        fs::read(case.home.join("moved").join("src").join("main.rs")).unwrap(),
        vec![7u8; 200_000]
    );
    verify_added(&case.home, text(&moved), AddMode::Move).unwrap();

    let copied = case.sources.join("copied");
    sample(&copied);
    plan_add(
        &case.home,
        &case.engine,
        text(&copied),
        AddMode::Copy,
        &[],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    let copy = case.home.join("copied");
    verify_tree(&copied, &copy).unwrap();
    assert_eq!(
        fs::read_link(copy.join("src").join("readme-link")).unwrap(),
        Path::new("../README.md")
    );
    assert_eq!(
        fs::read_link(copy.join("outside-link")).unwrap(),
        Path::new("/etc/hosts")
    );
    assert!(
        fs::symlink_metadata(copy.join("dangling"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(mode(&copy.join("README.md")), 0o640);
    assert_eq!(mode(&copy.join("src")), 0o750);
    assert_eq!(mode(&copy.join("src").join("deep").join("run.sh")), 0o751);
    assert_eq!(names(&copied), names(&copy));
    verify_added(&case.home, text(&copied), AddMode::Copy).unwrap();
    assert_eq!(names(&case.home), ["copied", "moved"]);

    assert_eq!(
        verify_added(&case.home, text(&copied), AddMode::Move)
            .unwrap_err()
            .0,
        "home_add_unverified"
    );
    assert_eq!(
        verify_added(&case.home, text(&case.sources.join("never")), AddMode::Copy)
            .unwrap_err()
            .0,
        "home_add_unverified"
    );
}

#[test]
fn a_move_across_disks_checks_the_copy_before_deleting_the_source() {
    let case = case();
    let source = case.sources.join("project");
    sample(&source);
    fs::create_dir(source.join("locked")).unwrap();
    fs::write(source.join("locked").join("keep.txt"), "kept").unwrap();
    fs::set_permissions(source.join("locked"), fs::Permissions::from_mode(0o555)).unwrap();
    let reference = case.root.join("reference");
    copy_tree(&source, &reference).unwrap();
    verify_tree(&source, &reference).unwrap();

    let destination = case.home.join("project");
    move_by_copy(&source, &destination).unwrap();
    assert!(!source.exists());
    verify_tree(&reference, &destination).unwrap();
    assert_eq!(mode(&destination.join("locked")), 0o555);
    assert_eq!(names(&case.home), ["project"]);

    let changed = case.root.join("changed");
    copy_tree(&reference, &changed).unwrap();
    fs::write(changed.join("README.md"), "hellO\n").unwrap();
    fs::set_permissions(changed.join("README.md"), fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        verify_tree(&reference, &changed).unwrap_err().0,
        "home_copy_unverified"
    );
    let extra = case.root.join("extra");
    copy_tree(&reference, &extra).unwrap();
    fs::write(extra.join("added.txt"), "x").unwrap();
    assert_eq!(
        verify_tree(&reference, &extra).unwrap_err().0,
        "home_copy_unverified"
    );
    let relinked = case.root.join("relinked");
    copy_tree(&reference, &relinked).unwrap();
    fs::remove_file(relinked.join("outside-link")).unwrap();
    symlink("/etc/passwd", relinked.join("outside-link")).unwrap();
    assert_eq!(
        verify_tree(&reference, &relinked).unwrap_err().0,
        "home_copy_unverified"
    );
    let remoded = case.root.join("remoded");
    copy_tree(&reference, &remoded).unwrap();
    fs::set_permissions(remoded.join("README.md"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        verify_tree(&reference, &remoded).unwrap_err().0,
        "home_copy_unverified"
    );
    for folder in [
        &reference,
        &destination,
        &changed,
        &extra,
        &relinked,
        &remoded,
    ] {
        fs::set_permissions(folder.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn special_files_are_refused_and_nothing_is_left_behind() {
    let case = case();
    let source = case.sources.join("socketed");
    sample(&source);
    let _listener = crazyagent::spawn_gate::bind_unix_std(source.join("agent.sock")).unwrap();
    let before = names(&source);

    let refused = plan_add(
        &case.home,
        &case.engine,
        text(&source),
        AddMode::Copy,
        &[],
        &[],
    )
    .unwrap()
    .run()
    .unwrap_err();
    assert_eq!(refused.0, "home_copy_special_file");
    assert_eq!(names(&case.home), Vec::<String>::new());
    assert_eq!(names(&source), before);

    let refused = move_by_copy(&source, &case.home.join("socketed")).unwrap_err();
    assert_eq!(refused.0, "home_copy_special_file");
    assert_eq!(names(&case.home), Vec::<String>::new());
    assert_eq!(names(&source), before);
    assert!(plain_problem("home_copy_special_file").is_some());
}

#[test]
fn unsafe_homes_and_sources_are_refused_before_anything_changes() {
    let case = case();
    let owner = owner_home().unwrap();
    let file = case.root.join("file.txt");
    fs::write(&file, "x").unwrap();
    let link = case.root.join("link");
    symlink(&case.home, &link).unwrap();
    fs::create_dir(case.engine.join("inside")).unwrap();

    assert_eq!(check_home(&case.home, &case.engine).unwrap(), case.home);
    for (picked, code) in [
        (PathBuf::from("/"), "projects_home_unsafe"),
        (owner.clone(), "projects_home_unsafe"),
        (
            owner.parent().unwrap().to_path_buf(),
            "projects_home_unsafe",
        ),
        (case.engine.clone(), "projects_home_unsafe"),
        (case.engine.join("inside"), "projects_home_unsafe"),
        (case.root.clone(), "projects_home_unsafe"),
        (case.root.join("absent"), "projects_home_missing"),
        (file.clone(), "projects_home_not_folder"),
        (link.clone(), "projects_home_not_folder"),
    ] {
        assert_eq!(
            check_home(&picked, &case.engine).unwrap_err().0,
            code,
            "{}",
            picked.display()
        );
        assert!(plain_problem(code).is_some(), "{code}");
    }

    let source = case.sources.join("project");
    fs::create_dir(&source).unwrap();
    fs::create_dir(case.sources.join(".secret")).unwrap();
    fs::create_dir(case.home.join("taken")).unwrap();
    fs::create_dir(case.sources.join("taken")).unwrap();
    fs::create_dir(case.home.join("inner")).unwrap();
    symlink(&source, case.sources.join("pointer")).unwrap();
    for (picked, code) in [
        ("/".to_owned(), "home_source_unsafe"),
        (text(&owner).to_owned(), "home_source_unsafe"),
        (
            text(owner.parent().unwrap()).to_owned(),
            "home_source_unsafe",
        ),
        (text(&case.root).to_owned(), "home_source_unsafe"),
        (text(&case.engine).to_owned(), "home_source_unsafe"),
        (
            text(&case.engine.join("inside")).to_owned(),
            "home_source_unsafe",
        ),
        (text(&case.home).to_owned(), "home_source_in_home"),
        (
            text(&case.home.join("inner")).to_owned(),
            "home_source_in_home",
        ),
        (
            text(&case.sources.join(".secret")).to_owned(),
            "home_source_hidden",
        ),
        (
            text(&case.sources.join("taken")).to_owned(),
            "home_destination_exists",
        ),
        (
            text(&case.sources.join("absent")).to_owned(),
            "home_source_missing",
        ),
        (text(&file).to_owned(), "home_source_not_folder"),
        (
            text(&case.sources.join("pointer")).to_owned(),
            "home_source_not_folder",
        ),
        ("relative/project".to_owned(), "home_source_invalid"),
        (
            format!("{}/../sources/project", text(&case.home)),
            "home_source_invalid",
        ),
        (String::new(), "home_source_invalid"),
        (format!("{}\n", text(&source)), "home_source_invalid"),
    ] {
        for mode in [AddMode::Move, AddMode::Copy] {
            assert_eq!(
                plan_add(&case.home, &case.engine, &picked, mode, &[], &[])
                    .unwrap_err()
                    .0,
                code,
                "{picked}"
            );
        }
        assert!(plain_problem(code).is_some(), "{code}");
    }
    assert!(source.is_dir() && owner.is_dir() && case.engine.join("inside").is_dir());
    assert_eq!(names(&case.home), ["inner", "taken"]);
    plan_add(
        &case.home,
        &case.engine,
        text(&source),
        AddMode::Move,
        &[],
        &[],
    )
    .unwrap();
    assert!(source.is_dir());
}

#[test]
fn layout_is_created_once_and_each_git_exclude_line_is_written_exactly_once() {
    let case = case();
    let repo = case.home.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join(".git").join("info").join("exclude"), "*.log").unwrap();
    fs::create_dir(repo.join("graphify-out")).unwrap();
    fs::write(repo.join("graphify-out").join("graph.json"), "{}").unwrap();

    let root = prepare_layout(&repo).unwrap();
    assert_eq!(root, repo.join(CRAZYAGENT_DIRECTORY));
    fs::write(root.join("context").join("brief.md"), "brief").unwrap();
    assert_eq!(prepare_layout(&repo).unwrap(), root);
    assert!(exclude_from_git(&repo).unwrap());
    for name in LAYOUT {
        assert!(root.join(name).is_dir(), "{name}");
    }
    assert_eq!(
        fs::read_to_string(root.join("context").join("brief.md")).unwrap(),
        "brief"
    );
    let exclude = fs::read_to_string(repo.join(".git").join("info").join("exclude")).unwrap();
    assert_eq!(exclude, "*.log\n.crazyagent/\ngraphify-out/\n");
    for line in EXCLUDES {
        assert_eq!(
            exclude.lines().filter(|present| *present == line).count(),
            1
        );
    }
    assert_eq!(
        git(&repo, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );

    let plain = case.home.join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(prepare_layout(&plain).unwrap().is_dir());
    assert!(!exclude_from_git(&plain).unwrap());
    assert!(!plain.join(".git").exists());

    let linked = case.home.join("linked");
    fs::create_dir(&linked).unwrap();
    let elsewhere = case.root.join("elsewhere-git");
    fs::create_dir_all(elsewhere.join("info")).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", text(&elsewhere)),
    )
    .unwrap();
    assert!(!exclude_from_git(&linked).unwrap());
    assert!(!elsewhere.join("info").join("exclude").exists());

    let hijacked = case.home.join("hijacked");
    fs::create_dir_all(hijacked.join(".git").join("info")).unwrap();
    let target = case.root.join("target.txt");
    fs::write(&target, "untouched").unwrap();
    symlink(&target, hijacked.join(".git").join("info").join("exclude")).unwrap();
    assert_eq!(
        exclude_from_git(&hijacked).unwrap_err().0,
        "git_exclude_invalid"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");

    let redirected = case.home.join("redirected");
    fs::create_dir(&redirected).unwrap();
    symlink(&case.root, redirected.join(CRAZYAGENT_DIRECTORY)).unwrap();
    assert_eq!(
        prepare_layout(&redirected).unwrap_err().0,
        "crazyagent_folder_invalid"
    );
    assert!(!case.root.join("setup").exists());
    assert_eq!(
        prepare_layout(&case.home.join("absent")).unwrap_err().0,
        "project_folder_missing"
    );
}

/// IN1-R1: before the IDE starts, the agents root of either layout leaves git in the
/// project folder, so Stage All there never takes an agent's files past Accept, while the
/// owner's own files, and a top-level `worktrees` folder of a project keeping its state in
/// `.crazyagent`, stay visible.
#[test]
fn exclude_agents_hides_the_agents_root_of_either_layout_once() {
    let case = case();
    let older = case.home.join("older");
    let newer = case.home.join("newer");
    for (project, agents) in [
        (&older, older.join("worktrees")),
        (&newer, newer.join(CRAZYAGENT_DIRECTORY).join("worktrees")),
    ] {
        fs::create_dir_all(agents.join("agent-3")).unwrap();
        git(project, &["init", "-q"]);
        fs::write(agents.join("agent-3").join("work.txt"), "agent\n").unwrap();
        fs::write(project.join("notes.txt"), "owner\n").unwrap();
        assert!(
            git(project, &["status", "--porcelain", "--untracked-files=all"])
                .contains("agent-3/work.txt")
        );
    }
    fs::create_dir(newer.join("worktrees")).unwrap();
    fs::write(newer.join("worktrees").join("mine.txt"), "owner\n").unwrap();

    for _ in 0..2 {
        assert!(exclude_agents(&older).unwrap());
        assert!(exclude_agents(&newer).unwrap());
    }
    let exclude = |project: &Path| {
        fs::read_to_string(project.join(".git").join("info").join("exclude")).unwrap()
    };
    assert_eq!(
        exclude(&older)
            .lines()
            .filter(|line| *line == "/worktrees/")
            .count(),
        1
    );
    assert!(!exclude(&older).contains(".crazyagent/"));
    assert_eq!(
        exclude(&newer)
            .lines()
            .filter(|line| *line == ".crazyagent/")
            .count(),
        1
    );
    assert!(!exclude(&newer).contains("/worktrees/"));
    assert_eq!(
        git(&older, &["status", "--porcelain", "--untracked-files=all"]),
        "?? notes.txt\n"
    );
    assert_eq!(
        git(&newer, &["status", "--porcelain", "--untracked-files=all"]),
        "?? notes.txt\n?? worktrees/mine.txt\n"
    );
    git(&older, &["add", "-A"]);
    assert_eq!(
        git(&older, &["diff", "--cached", "--name-only"]),
        "notes.txt\n"
    );

    let plain = case.home.join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(!exclude_agents(&plain).unwrap());
    assert!(!plain.join(".git").exists());
}

#[test]
fn every_layout_preparation_keeps_an_existing_instructions_file_and_refuses_a_link_there() {
    let case = case();
    let repo = case.home.join("instructed");
    fs::create_dir(&repo).unwrap();
    let root = prepare_layout(&repo).unwrap();
    let instructions = root.join("instructions.md");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        "",
        "a new project gets an empty instructions file"
    );
    assert_eq!(
        fs::metadata(&instructions).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&instructions, "OWNER-PROJECT-RULE-85\n").unwrap();
    for _ in 0..2 {
        assert_eq!(prepare_layout(&repo).unwrap(), root);
        assert_eq!(
            fs::read_to_string(&instructions).unwrap(),
            "OWNER-PROJECT-RULE-85\n"
        );
    }
    let linked = case.home.join("linked-instructions");
    fs::create_dir_all(linked.join(CRAZYAGENT_DIRECTORY)).unwrap();
    let target = case.root.join("elsewhere.md");
    fs::write(&target, "untouched").unwrap();
    symlink(
        &target,
        linked.join(CRAZYAGENT_DIRECTORY).join("instructions.md"),
    )
    .unwrap();
    assert_eq!(
        prepare_layout(&linked).unwrap_err().0,
        "crazyagent_folder_invalid"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
}

#[test]
fn folder_size_adds_file_bytes_without_following_links() {
    let case = case();
    let folder = case.sources.join("sized");
    fs::create_dir_all(folder.join("a").join("b")).unwrap();
    fs::write(folder.join("ten.txt"), [1u8; 10]).unwrap();
    fs::write(folder.join("a").join("b").join("big.bin"), vec![2u8; 2000]).unwrap();
    fs::write(case.root.join("huge.bin"), vec![3u8; 50_000]).unwrap();
    symlink(
        case.root.join("huge.bin"),
        folder.join("a").join("huge-link"),
    )
    .unwrap();
    symlink(&case.root, folder.join("root-link")).unwrap();
    let later = Instant::now() + Duration::from_secs(30);
    let measured = folder_size(text(&folder), later).unwrap();
    assert_eq!(measured.bytes, 2010);
    assert_eq!(measured.placeholders, 0);

    let many = case.sources.join("many");
    fs::create_dir(&many).unwrap();
    for index in 0..1100 {
        fs::write(many.join(format!("{index}.txt")), "x").unwrap();
    }
    assert_eq!(folder_size(text(&many), later).unwrap().bytes, 1100);
    assert_eq!(
        folder_size(text(&many), Instant::now()).unwrap_err().0,
        "folder_size_timeout"
    );
    let owner = owner_home().unwrap();
    for (path, code) in [
        ("/".to_owned(), "folder_size_unsafe"),
        (text(&owner).to_owned(), "folder_size_unsafe"),
        (
            text(&case.root.join("absent")).to_owned(),
            "folder_size_unreadable",
        ),
        ("relative".to_owned(), "folder_size_path_invalid"),
    ] {
        assert_eq!(folder_size(&path, later).unwrap_err().0, code, "{path}");
        assert!(plain_problem(code).is_some(), "{code}");
    }
}

#[test]
fn moving_the_home_moves_every_project_folder_or_puts_them_all_back() {
    let case = case();
    for name in ["alpha", "beta"] {
        fs::create_dir(case.home.join(name)).unwrap();
        fs::write(case.home.join(name).join("main.rs"), name).unwrap();
    }
    fs::create_dir(case.home.join(".hidden")).unwrap();
    let next = case.root.join("next");
    fs::create_dir(&next).unwrap();

    let plan = plan_move(&case.home, &next, &case.engine).unwrap();
    assert_eq!(plan.destination(), next.as_path());
    assert_eq!(plan.folders(), ["alpha", "beta"]);
    plan.run().unwrap();
    assert_eq!(names(&next), ["alpha", "beta"]);
    assert_eq!(
        fs::read_to_string(next.join("beta").join("main.rs")).unwrap(),
        "beta"
    );
    assert_eq!(names(&case.home), [".hidden"]);

    let back = plan_move(&next, &case.home, &case.engine).unwrap();
    fs::create_dir(case.home.join("beta")).unwrap();
    assert_eq!(back.run().unwrap_err().0, "home_destination_exists");
    assert_eq!(names(&next), ["alpha", "beta"]);
    assert_eq!(names(&case.home), [".hidden", "beta"]);
    assert_eq!(
        plan_move(&next, &case.home, &case.engine).unwrap_err().0,
        "home_destination_exists"
    );

    let inner = next.join("alpha").join("inner");
    fs::create_dir(&inner).unwrap();
    for (picked, code) in [
        (next.clone(), "home_move_same"),
        (inner.clone(), "home_move_nested"),
        (case.root.clone(), "projects_home_unsafe"),
        (PathBuf::from("/"), "projects_home_unsafe"),
        (case.root.join("absent"), "projects_home_missing"),
    ] {
        assert_eq!(
            plan_move(&next, &picked, &case.engine).unwrap_err().0,
            code,
            "{}",
            picked.display()
        );
        assert!(plain_problem(code).is_some(), "{code}");
    }
    assert_eq!(names(&next), ["alpha", "beta"]);
}

#[test]
fn a_home_move_covers_every_folder_or_refuses_before_anything_moves() {
    let case = case();
    let all: Vec<String> = (0..=FOLDERS_MAX).map(|n| format!("f{n:04}")).collect();
    assert_eq!(all.len(), FOLDERS_MAX + 1);
    for name in &all {
        fs::create_dir(case.home.join(name)).unwrap();
    }
    let next = case.root.join("next");
    fs::create_dir(&next).unwrap();

    assert_eq!(
        plan_move(&case.home, &next, &case.engine).unwrap_err().0,
        "home_move_too_many"
    );
    let sentence = plain_problem("home_move_too_many").unwrap();
    assert!(sentence.contains(&FOLDERS_MAX.to_string()), "{sentence}");
    assert_eq!(names(&case.home), all);
    assert!(names(&next).is_empty());

    fs::remove_dir(case.home.join(&all[FOLDERS_MAX])).unwrap();
    let plan = plan_move(&case.home, &next, &case.engine).unwrap();
    assert_eq!(plan.folders(), &all[..FOLDERS_MAX]);
    plan.run().unwrap();
    assert_eq!(names(&next), &all[..FOLDERS_MAX]);
    assert!(names(&case.home).is_empty());
}

#[cfg(target_os = "macos")]
struct Volume {
    mount: PathBuf,
    _image: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl Volume {
    fn attach() -> Self {
        use std::os::unix::fs::MetadataExt;
        let image = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        let disk = image.path().join("second.dmg");
        let mount = image.path().join("mount");
        fs::create_dir(&mount).unwrap();
        for args in [
            vec![
                "create",
                "-quiet",
                "-size",
                "16m",
                "-fs",
                "APFS",
                "-volname",
                "ca-home-move",
                text(&disk),
            ],
            vec![
                "attach",
                "-quiet",
                "-nobrowse",
                "-mountpoint",
                text(&mount),
                text(&disk),
            ],
        ] {
            let status =
                crazyagent::spawn_gate::status(Command::new("/usr/bin/hdiutil").args(&args))
                    .unwrap();
            assert!(status.success(), "{args:?}");
        }
        let volume = Self {
            mount: mount.canonicalize().unwrap(),
            _image: image,
        };
        assert_ne!(
            fs::metadata(&volume.mount).unwrap().dev(),
            fs::metadata(volume._image.path()).unwrap().dev()
        );
        volume
    }
}

#[cfg(target_os = "macos")]
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(Command::new("/usr/bin/hdiutil").args([
            "detach",
            "-quiet",
            "-force",
            text(&self.mount),
        ]));
    }
}

#[cfg(target_os = "macos")]
fn tool(program: &str, args: &[&str], path: &Path) {
    let status =
        crazyagent::spawn_gate::status(Command::new(program).args(args).arg(path)).unwrap();
    assert!(status.success(), "{program} {args:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_home_move_across_disks_puts_back_a_folder_whose_source_could_not_be_deleted() {
    let volume = Volume::attach();
    for (case_name, code, left) in [
        ("deletion never starts", "home_move_failed", vec![]),
        ("deletion stops partway", "home_move_failed", vec![]),
        (
            "source cannot be refilled",
            "home_move_partial",
            vec!["beta"],
        ),
    ] {
        let case = case();
        for name in ["alpha", "gamma"] {
            sample(&case.home.join(name));
        }
        let beta = case.home.join("beta");
        sample(&beta.join("inner"));
        let reference = case.root.join("reference");
        copy_tree(&beta, &reference).unwrap();
        if case_name == "deletion never starts" {
            tool("/usr/bin/chflags", &["uchg"], &beta.join("inner"));
        } else {
            tool("/bin/chmod", &["+a", "everyone deny delete_child"], &beta);
        }
        if case_name == "source cannot be refilled" {
            tool(
                "/bin/chmod",
                &["+a", "everyone deny add_file,add_subdirectory"],
                &beta.join("inner"),
            );
        }
        let next = volume.mount.join(format!("next-{}", case_name.len()));
        fs::create_dir(&next).unwrap();

        let plan = plan_move(&case.home, &next, &case.engine).unwrap();
        assert_eq!(plan.folders(), ["alpha", "beta", "gamma"]);
        let failure = plan.run().unwrap_err().0;
        tool("/usr/bin/chflags", &["-R", "nouchg"], &beta);
        tool("/bin/chmod", &["-N"], &beta);
        tool("/bin/chmod", &["-N"], &beta.join("inner"));
        assert_eq!(failure, code, "{case_name}");
        assert_eq!(names(&case.home), ["alpha", "beta", "gamma"], "{case_name}");
        assert_eq!(names(&next), left, "{case_name}");
        if left.is_empty() {
            verify_tree(&reference, &beta).unwrap();
        } else {
            verify_tree(&reference, &next.join("beta")).unwrap();
            assert_eq!(names(&beta), ["inner"], "{case_name}");
            assert!(names(&beta.join("inner")).is_empty(), "{case_name}");
        }
        for name in ["alpha", "gamma"] {
            assert_eq!(
                fs::read_to_string(case.home.join(name).join("README.md")).unwrap(),
                "hello\n"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_home_move_across_disks_moves_a_project_holding_hard_links() {
    let volume = Volume::attach();
    let case = case();
    let app = case.home.join("app");
    sample(&app);
    fs::create_dir_all(app.join("target/debug/deps")).unwrap();
    fs::write(app.join("target/debug/deps/app-0f3e"), "binary\n").unwrap();
    fs::hard_link(
        app.join("target/debug/deps/app-0f3e"),
        app.join("target/debug/app"),
    )
    .unwrap();
    let next = volume.mount.join("next");
    fs::create_dir(&next).unwrap();

    let plan = plan_move(&case.home, &next, &case.engine).unwrap();
    assert_eq!(plan.folders(), ["app"]);
    plan.run().unwrap();
    assert!(names(&case.home).is_empty());
    assert_eq!(names(&next), ["app"]);
    assert_eq!(
        fs::read_to_string(next.join("app/target/debug/app")).unwrap(),
        "binary\n"
    );
}

fn repo_with_worktree(repo: &Path, worktree: &Path) {
    fs::create_dir_all(repo).unwrap();
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "README.md"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-q",
            "-m",
            "first",
        ],
    );
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            worktree.to_str().unwrap(),
        ],
    );
}

#[test]
fn a_move_that_would_break_worktrees_or_another_project_is_refused_and_copy_still_works() {
    let case = case();
    let repo = case.sources.join("repo");
    let worktree = case.sources.join("repo-side");
    repo_with_worktree(&repo, &worktree);
    let link = fs::read_to_string(worktree.join(".git")).unwrap();
    for source in [&repo, &worktree] {
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(source),
                AddMode::Move,
                &[],
                &[]
            )
            .unwrap_err()
            .0,
            "home_move_worktrees",
            "{}",
            source.display()
        );
    }
    assert!(plain_problem("home_move_worktrees").is_some());
    let copy = plan_add(
        &case.home,
        &case.engine,
        text(&repo),
        AddMode::Copy,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(copy.destination(), case.home.join("repo").as_path());

    let outer = case.sources.join("outer");
    let project = outer.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    let registered = [Registered {
        project_id: ProjectId::new("project").unwrap(),
        name: "project".into(),
        folder: project.clone(),
        repositories: vec![project.clone()],
        onboarding: false,
    }];
    for source in [&outer, &project, &project.join("src")] {
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(source),
                AddMode::Move,
                &[],
                &registered
            )
            .unwrap_err()
            .0,
            "home_move_project",
            "{}",
            source.display()
        );
    }
    assert!(plain_problem("home_move_project").is_some());
    plan_add(
        &case.home,
        &case.engine,
        text(&outer),
        AddMode::Copy,
        &[],
        &registered,
    )
    .unwrap();

    let plain = case.sources.join("plain");
    sample(&plain);
    plan_add(
        &case.home,
        &case.engine,
        text(&plain),
        AddMode::Move,
        &[],
        &registered,
    )
    .unwrap()
    .run()
    .unwrap();
    assert!(!plain.exists() && case.home.join("plain").join("README.md").is_file());

    assert!(repo.join(".git").is_dir() && project.is_dir());
    assert_eq!(fs::read_to_string(worktree.join(".git")).unwrap(), link);
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "side\n"
    );
    assert!(git(&repo, &["worktree", "list"]).contains("repo-side"));
    assert_eq!(names(&case.home), ["plain"]);
}

fn commit(repo: &Path, message: &str) {
    git(
        repo,
        &[
            "-c",
            "user.name=Owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn worktree_paths(repo: &Path) -> Vec<String> {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_copy_of_a_repo_with_linked_worktrees_is_standalone_and_git_in_it_never_reaches_the_source() {
    let case = case();
    let repo = case.sources.join("repo");
    let sibling = case.sources.join("repo-side");
    repo_with_worktree(&repo, &sibling);
    let nested = repo.join(".tool").join("worktrees").join("inner");
    let gone = case.sources.join("repo-gone");
    for (branch, path) in [("inner", &nested), ("gone", &gone)] {
        git(
            &repo,
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
    fs::remove_dir_all(&gone).unwrap();
    let library = case.sources.join("library");
    fs::create_dir(&library).unwrap();
    fs::write(library.join("lib.txt"), "lib\n").unwrap();
    git(&library, &["init", "-q"]);
    git(&library, &["add", "lib.txt"]);
    commit(&library, "lib");
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            library.to_str().unwrap(),
            "libs/library",
        ],
    );
    commit(&repo, "add library");
    fs::write(repo.join(".gitignore"), ".tool/\n").unwrap();
    git(&repo, &["config", "extensions.worktreeconfig", "true"]);
    for branch in ["main", "side", "inner"] {
        git(
            &repo,
            &[
                "config",
                &format!("branch.{branch}.vscode-merge-base"),
                "origin/main",
            ],
        );
    }
    fs::write(
        repo.join(".git/worktrees/repo-side/config.worktree"),
        "[codex]\n\tlocalEnvironmentConfigPath = __none__\n",
    )
    .unwrap();
    fs::write(repo.join("README.md"), "changed\n").unwrap();
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    let links = [&sibling, &nested].map(|tree| fs::read_to_string(tree.join(".git")).unwrap());
    assert_eq!(worktree_paths(&repo).len(), 4);
    assert_eq!(linked_worktrees(&repo), 3);
    assert!(git_links(&repo));
    assert!(git_links(&sibling) && git_links(&nested));
    assert!(!linked_checkout(&repo));
    assert!(linked_checkout(&sibling) && linked_checkout(&nested));

    plan_add(
        &case.home,
        &case.engine,
        text(&repo),
        AddMode::Copy,
        &[],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    let copy = case.home.join("repo");
    verify_added(&case.home, text(&repo), AddMode::Copy).unwrap();

    assert_eq!(worktree_paths(&copy), [text(&copy)]);
    assert_eq!(linked_worktrees(&copy), 0);
    assert!(!git_links(&copy));
    assert!(!copy.join(".git").join("worktrees").exists());
    assert!(!copy.join(".tool").join("worktrees").join("inner").exists());
    assert_eq!(
        fs::read_to_string(copy.join("README.md")).unwrap(),
        "changed\n"
    );
    for branch in ["main", "side", "inner", "gone"] {
        assert_eq!(
            git(&copy, &["branch", "--list", branch]).trim_start_matches(['*', ' ']),
            format!("{branch}\n")
        );
    }
    let common = git(
        &copy.join("libs").join("library"),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert!(
        Path::new(common.trim())
            .canonicalize()
            .unwrap()
            .starts_with(&copy),
        "{common}"
    );

    let checkout = case.root.join("copy-inner");
    git(&copy, &["worktree", "prune"]);
    git(&copy, &["branch", "-D", "side"]);
    git(
        &copy,
        &["worktree", "add", "-q", checkout.to_str().unwrap(), "inner"],
    );
    git(&copy, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(&copy, &["worktree", "prune"]);

    assert_eq!(git(&repo, &["worktree", "list", "--porcelain"]), listed);
    for (tree, link) in [&sibling, &nested].into_iter().zip(&links) {
        assert_eq!(&fs::read_to_string(tree.join(".git")).unwrap(), link);
    }
    assert_eq!(
        git(&sibling, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "side\n"
    );
    assert_eq!(
        git(&nested, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "inner\n"
    );
    assert!(git(&repo, &["branch", "--list", "side"]).contains("side"));
    assert!(
        repo.join(".git")
            .join("worktrees")
            .join("repo-gone")
            .is_dir()
    );
}

#[test]
fn a_linked_worktree_or_a_link_to_a_git_folder_is_not_copied() {
    let case = case();
    let repo = case.sources.join("repo");
    let sibling = case.sources.join("repo-side");
    repo_with_worktree(&repo, &sibling);
    let linked = case.sources.join("linked");
    fs::create_dir(&linked).unwrap();
    symlink(repo.join(".git"), linked.join(".git")).unwrap();
    for source in [&sibling, &linked] {
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(source),
                AddMode::Copy,
                &[],
                &[]
            )
            .unwrap_err()
            .0,
            "home_copy_linked_checkout",
            "{}",
            source.display()
        );
    }
    assert!(plain_problem("home_copy_linked_checkout").is_some());
    assert_eq!(linked_worktrees(&sibling), 0);
    assert!(git_links(&sibling) && git_links(&linked));
    assert!(linked_checkout(&sibling) && linked_checkout(&linked));
    assert_eq!(names(&case.home), Vec::<String>::new());
    assert_eq!(worktree_paths(&repo).len(), 2);
}

/// A folder on this Mac whose `.git` entry cannot be looked up counts as a linked checkout in
/// the early check as in the full plan: only a source that is itself an iCloud placeholder
/// leaves that lookup to the plan after its download.
#[test]
fn a_copy_of_a_folder_whose_git_entry_cannot_be_read_is_a_linked_checkout_in_either_check() {
    let case = case();
    let shut = case.sources.join("shut");
    fs::create_dir(&shut).unwrap();
    fs::write(shut.join("README.md"), "shut\n").unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o600)).unwrap();
    let copy = AddMode::Copy;
    let checked = check_add(&case.home, &case.engine, text(&shut), copy, &[], &[]);
    let planned = plan_add(&case.home, &case.engine, text(&shut), copy, &[], &[]);
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(checked.unwrap_err().0, "home_copy_linked_checkout", "early");
    assert_eq!(planned.unwrap_err().0, "home_copy_linked_checkout", "full");
    assert_eq!(names(&case.home), Vec::<String>::new());
}

const PAST_THE_USUAL_LEVELS: usize = 260;

fn deep(folder: &Path, levels: usize) -> PathBuf {
    let mut bottom = folder.to_path_buf();
    for _ in 0..levels {
        bottom.push("d");
    }
    fs::create_dir_all(&bottom).unwrap();
    fs::write(bottom.join("bottom.txt"), "deep\n").unwrap();
    bottom
}

fn git_fails(folder: &Path, args: &[&str]) -> bool {
    !crazyagent::spawn_gate::status(
        Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap()
    .success()
}

#[test]
fn a_move_anyway_past_linked_worktrees_needs_that_override_and_git_worktree_repair_mends_them() {
    let case = case();
    let repo = case.sources.join("repo");
    let worktree = case.sources.join("repo-side");
    repo_with_worktree(&repo, &worktree);
    for overrides in [&[][..], &[AddOverride::TooLarge][..]] {
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(&repo),
                AddMode::Move,
                overrides,
                &[]
            )
            .unwrap_err()
            .0,
            "home_move_worktrees",
            "{overrides:?}"
        );
    }
    assert_eq!(
        linked_worktree_paths(&repo, 16),
        std::slice::from_ref(&worktree)
    );
    assert_eq!(linked_worktree_paths(&repo, 0), Vec::<PathBuf>::new());
    assert_eq!(linked_worktree_paths(&worktree, 16), Vec::<PathBuf>::new());

    plan_add(
        &case.home,
        &case.engine,
        text(&repo),
        AddMode::Move,
        &[AddOverride::Worktrees],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    let moved = case.home.join("repo");
    assert!(!repo.exists() && moved.join(".git").is_dir());
    assert!(git_fails(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]));
    git(&moved, &["worktree", "repair", text(&worktree)]);
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "side\n"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
}

#[test]
fn a_copy_anyway_of_a_linked_checkout_needs_that_override_and_keeps_its_git_link() {
    let case = case();
    let repo = case.sources.join("repo");
    let sibling = case.sources.join("repo-side");
    repo_with_worktree(&repo, &sibling);
    for overrides in [&[][..], &[AddOverride::TooLarge][..]] {
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(&sibling),
                AddMode::Copy,
                overrides,
                &[]
            )
            .unwrap_err()
            .0,
            "home_copy_linked_checkout",
            "{overrides:?}"
        );
    }
    plan_add(
        &case.home,
        &case.engine,
        text(&sibling),
        AddMode::Copy,
        &[AddOverride::LinkedCheckout],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    let copy = case.home.join("repo-side");
    assert_eq!(
        fs::read_to_string(copy.join(".git")).unwrap(),
        fs::read_to_string(sibling.join(".git")).unwrap()
    );
    assert!(sibling.join(".git").is_file() && sibling.join("README.md").is_file());
    assert_eq!(worktree_paths(&repo).len(), 2);
}

#[test]
fn a_copy_anyway_past_the_usual_levels_needs_that_override_and_leaves_nothing_without_it() {
    let case = case();
    let source = case.sources.join("deep");
    let bottom = deep(&source, PAST_THE_USUAL_LEVELS);
    for overrides in [&[][..], &[AddOverride::LinkedCheckout][..]] {
        let refused = plan_add(
            &case.home,
            &case.engine,
            text(&source),
            AddMode::Copy,
            overrides,
            &[],
        )
        .unwrap()
        .run()
        .unwrap_err();
        assert_eq!(refused.0, "home_copy_too_large", "{overrides:?}");
        assert_eq!(names(&case.home), Vec::<String>::new());
    }
    plan_add(
        &case.home,
        &case.engine,
        text(&source),
        AddMode::Copy,
        &[AddOverride::TooLarge],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    let landed = case
        .home
        .join("deep")
        .join(bottom.strip_prefix(&source).unwrap())
        .join("bottom.txt");
    assert_eq!(fs::read_to_string(landed).unwrap(), "deep\n");
    assert!(bottom.join("bottom.txt").is_file());
}

#[test]
fn hard_stops_stay_whatever_the_owner_overrides() {
    let case = case();
    let every_move = [AddOverride::Worktrees, AddOverride::TooLarge];
    let every_copy = [AddOverride::LinkedCheckout, AddOverride::TooLarge];
    let outer = case.sources.join("outer");
    let project = outer.join("project");
    fs::create_dir_all(&project).unwrap();
    let registered = [Registered {
        project_id: ProjectId::new("project").unwrap(),
        name: "project".into(),
        folder: project.clone(),
        repositories: vec![project.clone()],
        onboarding: false,
    }];
    assert_eq!(
        plan_add(
            &case.home,
            &case.engine,
            text(&outer),
            AddMode::Move,
            &every_move,
            &registered
        )
        .unwrap_err()
        .0,
        "home_move_project"
    );
    let taken = case.sources.join("taken");
    sample(&taken);
    fs::create_dir(case.home.join("taken")).unwrap();
    let inside = case.home.join("inside");
    fs::create_dir(&inside).unwrap();
    let file = case.sources.join("file.txt");
    fs::write(&file, "x").unwrap();
    for (mode, overrides) in [
        (AddMode::Move, &every_move[..]),
        (AddMode::Copy, &every_copy[..]),
    ] {
        for (source, code) in [
            (&taken, "home_destination_exists"),
            (&inside, "home_source_in_home"),
            (&case.engine, "home_source_unsafe"),
            (&case.root, "home_source_unsafe"),
            (&file, "home_source_not_folder"),
        ] {
            assert_eq!(
                plan_add(&case.home, &case.engine, text(source), mode, overrides, &[])
                    .unwrap_err()
                    .0,
                code,
                "{mode:?} {}",
                source.display()
            );
        }
    }
    let socketed = case.sources.join("socketed");
    sample(&socketed);
    let _listener = crazyagent::spawn_gate::bind_unix_std(socketed.join("agent.sock")).unwrap();
    let refused = plan_add(
        &case.home,
        &case.engine,
        text(&socketed),
        AddMode::Copy,
        &every_copy,
        &[],
    )
    .unwrap()
    .run()
    .unwrap_err();
    assert_eq!(refused.0, "home_copy_special_file");
    assert_eq!(names(&case.home), ["inside", "taken"]);
    assert!(taken.join("README.md").is_file() && project.is_dir());
}

#[test]
fn an_override_the_mode_cannot_use_or_a_repeated_one_is_refused() {
    let case = case();
    let plain = case.sources.join("plain");
    sample(&plain);
    for (mode, overrides) in [
        (AddMode::Move, vec![AddOverride::LinkedCheckout]),
        (AddMode::Copy, vec![AddOverride::Worktrees]),
        (
            AddMode::Move,
            vec![AddOverride::TooLarge, AddOverride::TooLarge],
        ),
        (
            AddMode::Copy,
            vec![
                AddOverride::LinkedCheckout,
                AddOverride::TooLarge,
                AddOverride::LinkedCheckout,
                AddOverride::TooLarge,
            ],
        ),
    ] {
        let action = HomeAction::Added {
            source: text(&plain).into(),
            mode,
            overrides: overrides.clone(),
        };
        assert_eq!(
            action.validate().unwrap_err().0,
            "home_override_invalid",
            "{mode:?} {overrides:?}"
        );
        assert_eq!(
            plan_add(
                &case.home,
                &case.engine,
                text(&plain),
                mode,
                &overrides,
                &[]
            )
            .unwrap_err()
            .0,
            "home_override_invalid",
            "{mode:?} {overrides:?}"
        );
    }
    for (mode, overrides) in [
        (
            AddMode::Move,
            vec![AddOverride::Worktrees, AddOverride::TooLarge],
        ),
        (
            AddMode::Copy,
            vec![AddOverride::LinkedCheckout, AddOverride::TooLarge],
        ),
        (AddMode::Move, Vec::new()),
    ] {
        HomeAction::Added {
            source: text(&plain).into(),
            mode,
            overrides,
        }
        .validate()
        .unwrap();
    }
    assert!(plain_problem("home_override_invalid").is_some());

    let old: HomeAction =
        serde_json::from_str(r#"{"kind":"added","source":"/a","mode":"move"}"#).unwrap();
    assert_eq!(
        old,
        HomeAction::Added {
            source: "/a".into(),
            mode: AddMode::Move,
            overrides: Vec::new(),
        }
    );
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        r#"{"kind":"added","source":"/a","mode":"move"}"#
    );
    let asked: HomeAction = serde_json::from_str(
        r#"{"kind":"added","source":"/a","mode":"copy","overrides":["linked_checkout","too_large"]}"#,
    )
    .unwrap();
    assert_eq!(
        asked,
        HomeAction::Added {
            source: "/a".into(),
            mode: AddMode::Copy,
            overrides: vec![AddOverride::LinkedCheckout, AddOverride::TooLarge],
        }
    );
    assert!(
        serde_json::from_str::<HomeAction>(
            r#"{"kind":"added","source":"/a","mode":"move","overrides":["everything"]}"#
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_move_anyway_across_disks_past_the_usual_levels_copies_checks_then_removes() {
    let case = case();
    let volume = Volume::attach();
    let home = volume.mount.join("home");
    fs::create_dir(&home).unwrap();
    let source = case.sources.join("deep");
    let bottom = deep(&source, PAST_THE_USUAL_LEVELS);
    assert_eq!(same_volume(&source, &home), Some(false));
    assert_eq!(same_volume(&source, &case.home), Some(true));
    assert_eq!(same_volume(&case.root.join("missing"), &case.home), None);

    let refused = plan_add(&home, &case.engine, text(&source), AddMode::Move, &[], &[])
        .unwrap()
        .run()
        .unwrap_err();
    assert_eq!(refused.0, "home_copy_too_large");
    assert!(bottom.join("bottom.txt").is_file());
    assert_eq!(names(&home), Vec::<String>::new());

    plan_add(
        &home,
        &case.engine,
        text(&source),
        AddMode::Move,
        &[AddOverride::TooLarge],
        &[],
    )
    .unwrap()
    .run()
    .unwrap();
    assert!(!source.exists());
    let landed = home
        .join("deep")
        .join(bottom.strip_prefix(&source).unwrap())
        .join("bottom.txt");
    assert_eq!(fs::read_to_string(landed).unwrap(), "deep\n");
}

#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_file_keeps_a_move_across_disks_from_removing_the_source() {
    let case = case();
    let volume = Volume::attach();
    let home = volume.mount.join("home");
    fs::create_dir(&home).unwrap();
    let source = case.sources.join("guarded");
    sample(&source);
    let secret = source.join("src").join("secret.txt");
    fs::write(&secret, "kept\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
    let before = names(&source);
    for overrides in [&[][..], &[AddOverride::TooLarge][..]] {
        let refused = plan_add(
            &home,
            &case.engine,
            text(&source),
            AddMode::Move,
            overrides,
            &[],
        )
        .unwrap()
        .run()
        .unwrap_err();
        assert_eq!(refused.0, "home_permission_denied", "{overrides:?}");
        assert!(plain_problem(&refused.0).is_some());
        assert_eq!(names(&source), before);
        assert!(fs::symlink_metadata(&secret).is_ok());
        assert_eq!(names(&home), Vec::<String>::new());
    }
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read_to_string(&secret).unwrap(), "kept\n");
}

#[test]
fn a_folder_permissions_keep_from_moving_or_copying_is_refused_as_permission_denied() {
    let case = case();
    let parent = case.sources.join("locked-parent");
    let source = parent.join("shop");
    sample(&source);
    let secret = source.join("src").join("secret.txt");
    fs::write(&secret, "kept\n").unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let before = names(&source);
    let moved = plan_add(
        &case.home,
        &case.engine,
        text(&source),
        AddMode::Move,
        &[],
        &[],
    )
    .unwrap()
    .run()
    .unwrap_err();
    assert_eq!(moved.0, "home_permission_denied");
    assert_eq!(names(&source), before);
    assert_eq!(names(&case.home), Vec::<String>::new());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
    let copied = plan_add(
        &case.home,
        &case.engine,
        text(&source),
        AddMode::Copy,
        &[],
        &[],
    )
    .unwrap()
    .run()
    .unwrap_err();
    assert_eq!(copied.0, "home_permission_denied");
    assert_eq!(names(&source), before);
    assert_eq!(names(&case.home), Vec::<String>::new());
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read_to_string(&secret).unwrap(), "kept\n");
    for code in ["home_permission_denied", "home_icloud_not_downloaded"] {
        assert!(plain_problem(code).is_some(), "{code}");
    }
}

#[test]
fn each_git_link_a_move_breaks_is_told_apart() {
    let case = case();
    let plain = case.sources.join("plain");
    fs::create_dir(&plain).unwrap();
    let repo = case.sources.join("repo");
    let worktree = case.sources.join("repo-side");
    repo_with_worktree(&repo, &worktree);
    let submodule = case.sources.join("vendored");
    fs::create_dir(&submodule).unwrap();
    fs::create_dir_all(repo.join(".git/modules/vendored")).unwrap();
    fs::write(
        submodule.join(".git"),
        "gitdir: ../repo/.git/modules/vendored\n",
    )
    .unwrap();
    let named_worktrees = case.sources.join("worktrees-sub");
    fs::create_dir(&named_worktrees).unwrap();
    fs::create_dir_all(repo.join(".git/modules/worktrees/tools")).unwrap();
    fs::write(
        named_worktrees.join(".git"),
        "gitdir: ../repo/.git/modules/worktrees/tools\n",
    )
    .unwrap();
    let gone = case.sources.join("gone-sub");
    fs::create_dir(&gone).unwrap();
    fs::write(gone.join(".git"), "gitdir: ../repo/.git/modules/gone\n").unwrap();
    let elsewhere = case.sources.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join(".git"), "gitdir: /var/empty/git\n").unwrap();
    let locked = case.sources.join("locked");
    fs::create_dir_all(locked.join(".git")).unwrap();
    fs::set_permissions(locked.join(".git"), fs::Permissions::from_mode(0o000)).unwrap();
    let linked = case.sources.join("linked");
    fs::create_dir(&linked).unwrap();
    symlink(repo.join(".git"), linked.join(".git")).unwrap();
    let found = [
        (&plain, GitLink::None),
        (&repo, GitLink::Worktrees),
        (&worktree, GitLink::LinkedWorktree),
        (&submodule, GitLink::Submodule),
        (&named_worktrees, GitLink::Submodule),
        (&gone, GitLink::Unreadable),
        (&elsewhere, GitLink::Unreadable),
        (&locked, GitLink::Unreadable),
        (&linked, GitLink::Unreadable),
    ]
    .map(|(folder, want)| (folder.clone(), git_link(folder), want, git_links(folder)));
    fs::set_permissions(locked.join(".git"), fs::Permissions::from_mode(0o755)).unwrap();
    for (folder, got, want, links) in found {
        assert_eq!(got, want, "{}", folder.display());
        assert_eq!(links, want != GitLink::None, "{}", folder.display());
    }
}
