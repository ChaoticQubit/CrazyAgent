use super::*;
use crate::setup::pack::{PackGroup, PackKind};
use std::process::Command;

fn private() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn git_in(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn skill(folder: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n"),
    )
    .unwrap();
}

fn repository(dir: &Path) -> (String, String, String) {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "--quiet"]);
    skill(&repo.join("skills/notes"), "notes", "First version.");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "--quiet", "-m", "first"]);
    let first = git_in(&repo, &["rev-parse", "HEAD"]);
    skill(&repo.join("skills/notes"), "notes", "Second version.");
    git_in(&repo, &["commit", "--quiet", "-am", "second"]);
    let second = git_in(&repo, &["rev-parse", "HEAD"]);
    (format!("file://{}", repo.display()), first, second)
}

fn entry(id: &str, kind: PackKind, source: PackSource, install: &str) -> PackManifestEntry {
    PackManifestEntry {
        id: id.into(),
        kind,
        group: PackGroup::Workflow,
        source,
        license: "MIT".into(),
        on_by_default: true,
        install: install.into(),
    }
}

fn hidden(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

#[test]
fn a_pinned_commit_is_fetched_and_checked_out_exactly() {
    let work = tempfile::tempdir().unwrap();
    let setup = private();
    let (repo, first, second) = repository(work.path());
    assert_ne!(first, second);

    let checkout = fetch_git(setup.path(), &repo, &first).unwrap();
    assert!(checkout.starts_with(setup.path().join("pack")));
    let text = std::fs::read_to_string(checkout.join("skills/notes/SKILL.md")).unwrap();
    assert!(text.contains("First version."));
    assert_eq!(
        head(setup.path(), &checkout).as_deref(),
        Some(first.as_str())
    );
    assert_eq!(fetch_git(setup.path(), &repo, &first).unwrap(), checkout);

    let newer = fetch_git(setup.path(), &repo, &second).unwrap();
    assert_ne!(newer, checkout);
    let text = std::fs::read_to_string(newer.join("skills/notes/SKILL.md")).unwrap();
    assert!(text.contains("Second version."));
    assert!(hidden(&setup.path().join("pack")).is_empty());
}

#[test]
fn a_commit_the_source_does_not_have_is_refused_and_leaves_nothing() {
    let work = tempfile::tempdir().unwrap();
    let setup = private();
    let (repo, _, _) = repository(work.path());
    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        fetch_git(setup.path(), &repo, missing).unwrap_err(),
        InstallError("pack_commit_mismatch")
    );
    let pack = setup.path().join("pack");
    assert_eq!(std::fs::read_dir(&pack).unwrap().count(), 0);
}

#[test]
fn sources_outside_https_or_with_users_or_bad_pins_are_refused() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    assert!(validate_git("https://github.com/obra/superpowers", commit, None).is_ok());
    assert!(
        validate_git(
            "https://github.com/obra/superpowers",
            commit,
            Some("skills")
        )
        .is_ok()
    );
    for (repo, commit, path) in [
        ("http://github.com/obra/superpowers", commit, None),
        ("https://user@github.com/obra/superpowers", commit, None),
        ("https://github.com/obra/superpowers?x=1", commit, None),
        ("ssh://git@github.com/obra/superpowers", commit, None),
        ("ext::sh -c id", commit, None),
        ("https://github.com/obra/superpowers", "main", None),
        (
            "https://github.com/obra/superpowers",
            commit,
            Some("../outside"),
        ),
        ("https://github.com/obra/superpowers", commit, Some("/etc")),
    ] {
        assert_eq!(
            validate_git(repo, commit, path).unwrap_err(),
            InstallError("setup_pin_invalid"),
            "{repo} {commit} {path:?}"
        );
    }
}

#[test]
fn skill_folders_are_the_entry_folder_or_its_children_with_skill_md() {
    let work = tempfile::tempdir().unwrap();
    let single = work.path().join("single");
    skill(&single, "single", "One skill.");
    assert_eq!(skill_folders(&single).unwrap(), vec![single.clone()]);

    let many = work.path().join("many");
    skill(&many.join("beta"), "beta", "B.");
    skill(&many.join("alpha"), "alpha", "A.");
    skill(&many.join(".hidden"), "hidden", "H.");
    std::fs::create_dir_all(many.join("plain")).unwrap();
    assert_eq!(
        skill_folders(&many).unwrap(),
        vec![many.join("alpha"), many.join("beta")]
    );

    let empty = work.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        skill_folders(&empty).unwrap_err(),
        InstallError("pack_no_skills")
    );
}

#[test]
fn a_git_entry_folder_comes_from_its_pinned_checkout_and_bundled_needs_a_bundle() {
    let work = tempfile::tempdir().unwrap();
    let setup = private();
    let (repo, first, _) = repository(work.path());
    let git = entry(
        "notes",
        PackKind::Skill,
        PackSource::Git {
            repo,
            commit: first,
            path: Some("skills".into()),
        },
        "",
    );
    let folder = entry_folder(setup.path(), &git, None).unwrap();
    assert_eq!(skill_folders(&folder).unwrap(), vec![folder.join("notes")]);

    let bundled = entry(
        "mine",
        PackKind::Skill,
        PackSource::Bundled {
            path: "mine".into(),
        },
        "",
    );
    assert_eq!(
        entry_folder(setup.path(), &bundled, None).unwrap_err(),
        InstallError("pack_bundled_unavailable")
    );
    let bundle = work.path().join("bundle");
    skill(&bundle.join("mine"), "mine", "Mine.");
    assert_eq!(
        entry_folder(setup.path(), &bundled, Some(bundle.as_path())).unwrap(),
        bundle.join("mine")
    );
}

#[test]
fn a_server_entry_starts_its_program_without_a_shell() {
    let work = tempfile::tempdir().unwrap();
    let setup = private();
    let folder = work.path().join("server");
    std::fs::create_dir_all(folder.join("bin")).unwrap();
    let program = folder.join("bin/server");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PackSource::Bundled {
        path: "server".into(),
    };
    let server = entry(
        "Context Mode",
        PackKind::McpServer,
        source.clone(),
        "bin/server --stdio --root {dir}",
    );
    assert_eq!(server_name(&server).unwrap(), "context-mode");
    assert_eq!(
        server_spec(setup.path(), &server, &folder).unwrap(),
        ServerSpec::Stdio {
            command: program.display().to_string(),
            args: vec![
                "--stdio".into(),
                "--root".into(),
                folder.display().to_string()
            ],
        }
    );
    let missing = entry(
        "x",
        PackKind::McpServer,
        source.clone(),
        "crazyagent-no-such-program --stdio",
    );
    assert_eq!(
        server_spec(setup.path(), &missing, &folder).unwrap_err(),
        InstallError("pack_server_command_missing")
    );
    for line in ["", "../bin/server", "bin/../../server"] {
        let bad = entry("x", PackKind::McpServer, source.clone(), line);
        assert_eq!(
            server_spec(setup.path(), &bad, &folder).unwrap_err(),
            InstallError("pack_server_invalid"),
            "{line}"
        );
    }
}

fn shipped(id: &str) -> PackManifestEntry {
    crate::setup::pack::default_pack()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap()
}

fn programs(bin: &Path, names: &[&str]) {
    std::fs::create_dir_all(bin).unwrap();
    for name in names {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn the_shipped_mcp_entries_read_as_servers_without_a_shell() {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    let folder = work.path().join("entry");
    std::fs::create_dir_all(&folder).unwrap();
    programs(&bin, &["npx", "uv"]);
    let lookup = |name: &str| {
        let path = bin.join(name);
        runnable(&path).then_some(path)
    };
    let program = |name: &str| bin.join(name).display().to_string();

    let plan = server_plan(&shipped("context-mode"), &folder, &lookup).unwrap();
    assert_eq!(plan.tool, None);
    assert_eq!(
        plan.spec,
        ServerSpec::Stdio {
            command: program("npx"),
            args: vec!["-y".into(), "context-mode@1.0.169".into()],
        }
    );

    let plan = server_plan(&shipped("serena"), &folder, &lookup).unwrap();
    let ServerSpec::Stdio { command, args } = plan.spec else {
        panic!("serena runs as a command");
    };
    assert_eq!(command, program("uv"));
    assert_eq!(&args[..3], ["tool", "run", "--from"]);
    assert!(args[3].starts_with("git+https://github.com/oraios/serena@"));
    assert_eq!(
        &args[4..],
        ["serena", "start-mcp-server", "--project-from-cwd"]
    );

    let plan = server_plan(&shipped("linear"), &folder, &lookup).unwrap();
    assert_eq!(
        plan.spec,
        ServerSpec::Http {
            url: "https://mcp.linear.app/mcp".into()
        }
    );
    assert_eq!(server_name(&shipped("linear")).unwrap(), "linear");
}

#[test]
fn a_server_with_a_tool_step_finds_its_program_after_the_tool_is_installed() {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    let folder = work.path().join("entry");
    std::fs::create_dir_all(&folder).unwrap();
    programs(&bin, &["uv"]);
    let lookup = |name: &str| {
        let path = bin.join(name);
        runnable(&path).then_some(path)
    };

    let plan = server_plan(&shipped("headroom"), &folder, &lookup).unwrap();
    assert_eq!(plan.tool, Some(("headroom-ai".into(), "0.41.0".into())));
    assert_eq!(
        finish_plan(plan.clone(), &lookup).unwrap_err(),
        InstallError("pack_server_command_missing")
    );
    programs(&bin, &["headroom"]);
    assert_eq!(
        finish_plan(plan, &lookup).unwrap(),
        ServerSpec::Stdio {
            command: bin.join("headroom").display().to_string(),
            args: vec!["mcp".into(), "serve".into()],
        }
    );
}

#[test]
fn skill_folders_are_found_a_few_levels_down_but_not_inside_a_skill() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("repo");
    skill(&root.join("graphify/skill"), "graphify", "Graph.");
    skill(
        &root.join("graphify/skill/nested"),
        "nested",
        "Inside a skill.",
    );
    skill(&root.join("a/b/c/d"), "too-deep", "Four levels down.");
    assert_eq!(
        skill_folders(&root).unwrap(),
        vec![root.join("graphify/skill")]
    );
}
