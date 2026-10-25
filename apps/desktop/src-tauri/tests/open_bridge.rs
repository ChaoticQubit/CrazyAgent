#![cfg(unix)]

use crazyagent_desktop::bridge::open::{context_file, link_allowed, project_folder};
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
