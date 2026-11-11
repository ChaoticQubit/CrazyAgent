#![cfg(unix)]

use crazyagent::project_setup::{Platform, ResearchKind, StoredSetup, TeamPlan, list_context};
use crazyagent::protocol::ProjectId;
use crazyagent_desktop::bridge::open::{context_file, context_item, link_allowed, project_folder};
use std::os::unix::fs::symlink;

#[test]
fn only_plain_web_links_open_in_the_browser() {
    assert_eq!(
        link_allowed(" https://example.com/report?q=1 ").unwrap(),
        "https://example.com/report?q=1"
    );
    assert_eq!(
        link_allowed("http://127.0.0.1:8080/").unwrap(),
        "http://127.0.0.1:8080/"
    );
    for refused in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "ftp://example.com/a",
        "x-apple.systempreferences:com.apple.preference",
        "https://user:secret@example.com/",
        "https://exa mple.com/",
        "https://example.com/\n",
        "-a Calculator",
        "",
    ] {
        assert_eq!(
            link_allowed(refused).unwrap_err().code,
            "link_not_allowed",
            "{refused:?}"
        );
    }
    let long = format!("https://example.com/{}", "a".repeat(2048));
    assert_eq!(link_allowed(&long).unwrap_err().code, "link_not_allowed");
}

#[test]
fn a_project_folder_must_be_an_existing_absolute_folder() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("harbor-notes");
    std::fs::create_dir(&folder).unwrap();
    let canonical = folder.canonicalize().unwrap();
    assert_eq!(project_folder(folder.to_str().unwrap()).unwrap(), canonical);
    assert_eq!(
        project_folder("relative/folder").unwrap_err().code,
        "project_folder_missing"
    );
    let missing = root.path().join("gone");
    assert_eq!(
        project_folder(missing.to_str().unwrap()).unwrap_err().code,
        "project_folder_missing"
    );
    let file = root.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(
        project_folder(file.to_str().unwrap()).unwrap_err().code,
        "project_folder_missing"
    );
}

#[test]
fn context_files_open_only_inside_the_project_folder_and_only_as_documents() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("project");
    std::fs::create_dir_all(folder.join("research")).unwrap();
    std::fs::write(folder.join("research/market.md"), "# Market").unwrap();
    std::fs::write(folder.join("brief.PDF"), "%PDF").unwrap();
    std::fs::write(folder.join("run.command"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.path().join("outside.md"), "secret").unwrap();
    symlink(root.path().join("outside.md"), folder.join("escape.md")).unwrap();
    let folder = folder.canonicalize().unwrap();

    let opened = context_file(&folder, "research/market.md").unwrap();
    assert_eq!(opened, folder.join("research/market.md"));
    assert!(context_file(&folder, "brief.PDF").is_ok());

    for refused in [
        "../outside.md",
        "research/../../outside.md",
        "/etc/hosts",
        "escape.md",
        "run.command",
        "research",
        "research/missing.md",
        "",
    ] {
        assert_eq!(
            context_file(&folder, refused).unwrap_err().code,
            "context_item_not_openable",
            "{refused:?}"
        );
    }
}

fn setup_at(folder: &std::path::Path) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new("notes-cli").unwrap(),
        name: "Notes CLI".into(),
        about: "Terminal-first notes.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Cli],
        research: vec![ResearchKind::CompetitorResearch],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1_793_000_000_000,
        revision: 1,
    }
}

/// Open in default app on each context file the engine listed: a project made in the
/// projects home keeps its context in `.crazyagent/context` and an older project keeps it
/// in the project folder, and both open the listed file.
#[test]
fn every_listed_context_file_opens_from_the_folder_the_engine_listed_it_in() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let made = home.join("notes-cli");
    let older = home.join("older-app");
    for state in [made.join(".crazyagent"), older.clone()] {
        std::fs::create_dir_all(state.join("context/research")).unwrap();
        std::fs::write(state.join("context/brief.md"), "# Brief").unwrap();
        std::fs::write(state.join("context/research/market.md"), "# Market").unwrap();
    }

    for (project, state) in [(&made, made.join(".crazyagent")), (&older, older.clone())] {
        let setup = setup_at(project);
        let listed = list_context(&setup).unwrap();
        let paths: Vec<&str> = listed.items.iter().map(|item| item.path.as_str()).collect();
        assert_eq!(paths, ["context/brief.md", "context/research/market.md"]);
        let opened: Vec<_> = listed
            .items
            .iter()
            .map(|item| context_item(&setup, &item.path).map_err(|error| error.code))
            .collect();
        assert_eq!(
            opened,
            vec![
                Ok(state.join("context/brief.md")),
                Ok(state.join("context/research/market.md")),
            ],
            "{project:?}"
        );
    }
}

/// Open in default app on a file linked from the repository: the engine lists it as
/// `repo/<path>` and it opens below the project folder itself, not below `.crazyagent`. A
/// link is not followed, and a path that leaves the folder, is missing, is empty or is no
/// document type is refused.
#[test]
fn a_linked_repo_file_opens_from_the_project_folder_and_nothing_else_does() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let project = home.join("notes-cli");
    std::fs::create_dir_all(project.join(".crazyagent/context")).unwrap();
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(project.join("docs/plan.md"), "# Plan").unwrap();
    std::fs::write(project.join("docs/plan"), "# Plan").unwrap();
    std::fs::write(home.join("outside.md"), "outside").unwrap();
    symlink(home.join("outside.md"), project.join("docs/link.md")).unwrap();
    let setup = setup_at(&project);
    assert_eq!(setup.state_folder(), project.join(".crazyagent"));
    assert_eq!(
        context_item(&setup, "repo/docs/plan.md").unwrap(),
        project.join("docs/plan.md")
    );
    for refused in [
        "repo/docs/link.md",
        "repo/../outside.md",
        "repo/docs/missing.md",
        "repo/docs/plan",
        "repo/",
    ] {
        assert_eq!(
            context_item(&setup, refused).unwrap_err().code,
            "context_item_not_openable",
            "{refused}"
        );
    }
}
