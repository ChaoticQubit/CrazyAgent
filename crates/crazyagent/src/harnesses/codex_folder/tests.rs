use super::*;
use std::path::PathBuf;

const QUIT: &str = "Folder access\r\n/some/folder\r\n\r\nConfig, hooks, and exec policies from untrusted folders stay disabled.\r\n\r\n\u{203a} 1. Open restricted\r\n  2. Quit\r\n\r\nenter continue \u{b7} esc quit\r\n";
const BACK: &str = "Folder access\r\n/some/folder\r\n\r\n\u{203a} 1. Open restricted\r\n  2. Back to Agent Command Center\r\n\r\nenter continue \u{b7} esc back\r\n";
const EXISTING: &str = "Folder access\r\n/some/folder\r\n\r\nThis existing task may retain settings and history.\r\n\r\n\u{203a} 1. Open existing task\r\n  2. Back to Agent Command Center\r\n\r\nenter continue \u{b7} esc back\r\n";

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("ca-codex-folder-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    (root, base)
}

fn folder(path: &Path) -> &Path {
    std::fs::create_dir_all(path).unwrap();
    path
}

fn line(folders: &[&Path], level: &str) -> String {
    let parts: Vec<String> = folders
        .iter()
        .map(|folder| format!("\"{}\"={{trust_level=\"{level}\"}}", folder.display()))
        .collect();
    format!("projects={{{}}}", parts.join(","))
}

#[test]
fn the_walk_stops_at_the_first_git_whether_file_or_folder() {
    let (_root, base) = scratch();
    let repo = base.join("top/repo");
    let sub = repo.join("a/b");
    folder(&sub);
    folder(&repo.join(".git"));
    folder(&base.join("top/.git"));
    assert_eq!(
        project_folders(&sub),
        [sub.as_path(), &repo.join("a"), repo.as_path()]
    );
    let linked = base.join("linked");
    folder(&linked);
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere\n").unwrap();
    assert_eq!(project_folders(&linked), [linked.as_path()]);
}

#[test]
fn a_broken_git_link_does_not_stop_the_walk() {
    let (_root, base) = scratch();
    let repo = base.join("top/repo");
    folder(&repo);
    folder(&base.join("top/.git"));
    std::os::unix::fs::symlink(base.join("missing"), repo.join(".git")).unwrap();
    assert_eq!(project_folders(&repo), [repo.as_path(), &base.join("top")]);
}

#[test]
fn with_no_git_the_walk_reaches_the_root() {
    let (_root, base) = scratch();
    let cwd = base.join("a");
    folder(&cwd);
    let folders = project_folders(&cwd);
    assert_eq!(folders.first(), Some(&cwd.as_path()));
    assert_eq!(folders.last(), Some(&Path::new("/")));
    assert!(folders.contains(&base.as_path()));
}

#[test]
fn a_codex_folder_on_the_walk_makes_every_walk_folder_untrusted() {
    let (_root, base) = scratch();
    let repo = base.join("repo");
    let sub = repo.join("sub");
    folder(&sub);
    folder(&repo.join(".git"));
    folder(&repo.join(".codex"));
    let wanted = line(&[&sub, &repo], "untrusted");
    assert_eq!(terminal_entry(&sub).unwrap(), wanted);
    assert_eq!(untrusted(&sub).unwrap(), wanted);
}

#[test]
fn a_codex_folder_in_the_launch_folder_counts_too() {
    let (_root, base) = scratch();
    let repo = base.join("repo");
    folder(&repo.join(".git"));
    folder(&repo.join(".codex"));
    assert_eq!(terminal_entry(&repo).unwrap(), line(&[&repo], "untrusted"));
}

#[test]
fn a_codex_folder_above_the_first_git_is_outside_the_walk() {
    let (_root, base) = scratch();
    let repo = base.join("top/repo");
    folder(&repo.join(".git"));
    folder(&base.join("top/.codex"));
    assert_eq!(terminal_entry(&repo).unwrap(), line(&[&repo], "trusted"));
}

#[test]
fn no_codex_folder_leaves_the_launch_folder_trusted_alone() {
    let (_root, base) = scratch();
    let repo = base.join("repo");
    let sub = repo.join("sub");
    folder(&sub);
    folder(&repo.join(".git"));
    assert_eq!(terminal_entry(&sub).unwrap(), line(&[&sub], "trusted"));
}

#[test]
fn a_codex_link_to_a_folder_counts_and_a_plain_file_does_not() {
    let (_root, base) = scratch();
    let linked = base.join("linked");
    folder(&linked.join(".git"));
    folder(&base.join("elsewhere"));
    std::os::unix::fs::symlink(base.join("elsewhere"), linked.join(".codex")).unwrap();
    assert_eq!(
        terminal_entry(&linked).unwrap(),
        line(&[&linked], "untrusted")
    );
    let plain = base.join("plain");
    folder(&plain.join(".git"));
    std::fs::write(plain.join(".codex"), "not a folder\n").unwrap();
    assert_eq!(terminal_entry(&plain).unwrap(), line(&[&plain], "trusted"));
}

#[test]
#[should_panic(expected = "folders.is_empty")]
fn an_entry_naming_no_folder_is_a_bug() {
    let _ = entry(&[], "untrusted");
}

#[test]
fn only_an_untrusted_setting_puts_a_launch_behind_the_screen() {
    let (_root, base) = scratch();
    let args = |setting: String| vec!["-c".to_owned(), setting, "resume".to_owned()];
    let repo = base.join("repo");
    folder(&repo.join(".git"));
    assert!(!behind_a_screen(&args(terminal_entry(&repo).unwrap())));
    assert!(behind_a_screen(&args(untrusted(&repo).unwrap())));
    let forged = base.join("x={trust_level=\"untrusted\"}}");
    folder(&forged.join(".git"));
    assert!(!behind_a_screen(&args(terminal_entry(&forged).unwrap())));
    assert!(!behind_a_screen(&[untrusted(&repo).unwrap()]));
    assert!(!behind_a_screen(&args("model=\"x\"".to_owned())));
}

#[test]
fn each_measured_folder_screen_reads_as_the_menu() {
    for screen in [QUIT, BACK, EXISTING] {
        assert_eq!(read_screen(screen.as_bytes()), Screen::Menu, "{screen:?}");
    }
    let drawn = "\x1b[1;1HFolder access\r\n\r\n\x1b[1;7m\u{203a} 1. Open restricted\x1b[0m      \r\n  2. Quit\r\n\r\nenter continue \u{b7} esc quit\r\n";
    assert_eq!(read_screen(drawn.as_bytes()), Screen::Menu);
}

#[test]
fn a_menu_that_differs_in_any_row_gets_no_key() {
    for screen in [
        QUIT.replace("\u{203a} 1. Open", "  1. Open"),
        QUIT.replace("  2. Quit", "\u{203a} 2. Quit"),
        QUIT.replace("Open restricted", "Open anyway"),
        QUIT.replace("Folder access", "Folder rights"),
        QUIT.replace("esc quit", "esc leave"),
        QUIT.replace("2. Quit", "2. Trust this folder"),
        BACK.replace("2. Back to Agent Command Center", "2. Quit"),
        EXISTING.replace("2. Back to Agent Command Center", "2. Quit"),
        format!("{QUIT}one row more\r\n"),
        "Trust this folder?\r\n\r\n\u{203a} 1. Trust and continue\r\n  2. Quit\r\n".to_owned(),
    ] {
        assert_eq!(read_screen(screen.as_bytes()), Screen::Other, "{screen:?}");
    }
}

#[test]
fn only_the_input_box_with_no_menu_reads_as_the_box() {
    let plain = "header\r\n\r\n\u{203a} Ask Codex to do anything\r\n\r\n  ? for shortcuts\r\n";
    assert_eq!(read_screen(plain.as_bytes()), Screen::InputBox);
    let over = "Folder access\r\n\u{203a} Ask Codex to do anything\r\n";
    assert_eq!(read_screen(over.as_bytes()), Screen::Other);
    let option = "\u{203a} 1. Open anything\r\n  2. Something\r\n";
    assert_eq!(read_screen(option.as_bytes()), Screen::Other);
    assert_eq!(read_screen(b"just text\r\n"), Screen::Other);
    assert_eq!(read_screen(b""), Screen::Other);
}
