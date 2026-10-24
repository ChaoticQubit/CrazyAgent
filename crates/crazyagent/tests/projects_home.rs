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
    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
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
    let plan = plan_add(&case.home, &case.engine, text(&moved), AddMode::Move).unwrap();
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
    plan_add(&case.home, &case.engine, text(&copied), AddMode::Copy)
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

    let refused = plan_add(&case.home, &case.engine, text(&source), AddMode::Copy)
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
                plan_add(&case.home, &case.engine, &picked, mode)
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
    plan_add(&case.home, &case.engine, text(&source), AddMode::Move).unwrap();
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
    assert_eq!(folder_size(text(&folder), later).unwrap(), 2010);

    let many = case.sources.join("many");
    fs::create_dir(&many).unwrap();
    for index in 0..1100 {
        fs::write(many.join(format!("{index}.txt")), "x").unwrap();
    }
    assert_eq!(folder_size(text(&many), later).unwrap(), 1100);
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
