use super::{TREE_BOUNDS, backlink_matches, identity, linked_metadata, tree_entries, write_tree};
use crazyagent_win_sandbox::private;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, base)
}

fn command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=w3", "-c", "user.email=w3@example.invalid"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = command(repo, args);
    command.stdin(Stdio::null());
    let done = crate::spawn_gate::output(&mut command).unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
    String::from_utf8(done.stdout)
        .unwrap()
        .trim_end()
        .to_owned()
}

fn fed(repo: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = command(repo, args);
    command.stdin(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let done = child.wait_with_output().unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
    String::from_utf8(done.stdout)
        .unwrap()
        .trim_end()
        .to_owned()
}

fn blob(repo: &Path, bytes: &[u8]) -> String {
    fed(repo, &["hash-object", "-w", "--stdin"], bytes)
}

fn tree(repo: &Path, entries: &[(&str, &str, String)]) -> String {
    let mut input = Vec::new();
    for (mode, name, oid) in entries {
        let kind = if *mode == "040000" { "tree" } else { "blob" };
        input.extend_from_slice(format!("{mode} {kind} {oid}\t{name}\n").as_bytes());
    }
    fed(repo, &["mktree"], &input)
}

fn repository(base: &Path) -> PathBuf {
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    repo
}

fn commit(repo: &Path, root: &str) -> String {
    git(repo, &["commit-tree", root, "-m", "w3-5b"])
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn code<T: std::fmt::Debug>(result: Result<T, crate::engine::EngineError>) -> String {
    result.unwrap_err().0
}

fn listed(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn null_list(path: &Path) {
    let script = format!(
        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
         public static class W3 {{ [DllImport(\"advapi32.dll\", CharSet = CharSet.Unicode)] \
         public static extern uint SetNamedSecurityInfoW(string n, int t, int i, IntPtr o, \
         IntPtr g, IntPtr d, IntPtr s); }}'; \
         [W3]::SetNamedSecurityInfoW('{}', 1, 0x80000004, [IntPtr]::Zero, [IntPtr]::Zero, \
         [IntPtr]::Zero, [IntPtr]::Zero)",
        path.display()
    );
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env_remove("PSModulePath")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let done = crate::spawn_gate::output(&mut command).unwrap();
    println!(
        "{} set: {} {:?} {:?}",
        path.display(),
        done.status,
        String::from_utf8_lossy(&done.stdout).trim(),
        String::from_utf8_lossy(&done.stderr).trim()
    );
    assert!(done.status.success());
    assert_eq!(String::from_utf8_lossy(&done.stdout).trim(), "0");
    let opened = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .access_mode(super::READ_CONTROL | super::FILE_READ_ATTRIBUTES)
            .custom_flags(super::FILE_FLAG_OPEN_REPARSE_POINT | super::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    };
    let facts = private::facts(&opened).unwrap();
    assert_eq!(facts.list, None, "the row's own precondition: a NULL list");
}

#[test]
fn a_tree_naming_an_entry_windows_cannot_name_again_is_refused_before_any_file() {
    let (_temp, base) = plain_temp();
    let repo = repository(&base);
    let bytes = blob(&repo, b"x\n");
    for name in [
        "aux", "NUL.txt", "con.json", "COM1", "lpt9.log", "a.", "a ", "a*b", "a?b", "a<b", "a>b",
        "a|b", "a\"b", "a\u{1}b",
    ] {
        let root = tree(&repo, &[("100644", name, bytes.clone())]);
        let head = commit(&repo, &root);
        assert_eq!(
            code(tree_entries(&repo, &head, &TREE_BOUNDS, deadline())),
            "unsupported_or_unsafe_tree_entry",
            "{name:?} at the top"
        );
        let nested = tree(&repo, &[("040000", "src", root)]);
        let head = commit(&repo, &nested);
        assert_eq!(
            code(tree_entries(&repo, &head, &TREE_BOUNDS, deadline())),
            "unsupported_or_unsafe_tree_entry",
            "src/{name:?}"
        );
    }
    let plain = tree(&repo, &[("100644", "auxiliary.txt", bytes.clone())]);
    let head = commit(&repo, &plain);
    let entries = tree_entries(&repo, &head, &TREE_BOUNDS, deadline()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, Path::new("auxiliary.txt"));
}

#[test]
fn a_tree_is_written_under_the_held_root_and_its_link_is_left_out() {
    let (_temp, base) = plain_temp();
    let repo = repository(&base);
    let readme = blob(&repo, b"# readme\n");
    let main = blob(&repo, b"fn main() {}\n");
    let deep = blob(&repo, b"deep\r\nbytes\n");
    let tool = blob(&repo, b"#!/bin/sh\n");
    let target = blob(&repo, b"README.md");
    let inner = tree(&repo, &[("100644", "x.txt", deep)]);
    let src = tree(
        &repo,
        &[("040000", "deep", inner), ("100644", "main.rs", main)],
    );
    let root = tree(
        &repo,
        &[
            ("100644", "README.md", readme),
            ("120000", "link", target),
            ("040000", "src", src),
            ("100755", "tool.sh", tool),
        ],
    );
    let head = commit(&repo, &root);
    let entries = tree_entries(&repo, &head, &TREE_BOUNDS, deadline()).unwrap();
    assert_eq!(entries.len(), 5);
    let folder = base.join("lease");
    let held = private::dir(&folder).unwrap();
    let left_out = write_tree(&repo, &held, &entries, deadline()).unwrap();
    assert_eq!(left_out, ["link"]);
    assert_eq!(listed(&folder), ["README.md", "src", "tool.sh"]);
    assert_eq!(
        std::fs::read(folder.join("README.md")).unwrap(),
        b"# readme\n"
    );
    assert_eq!(
        std::fs::read(folder.join("src").join("main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    assert_eq!(
        std::fs::read(folder.join("src").join("deep").join("x.txt")).unwrap(),
        b"deep\r\nbytes\n"
    );
    assert_eq!(
        std::fs::read(folder.join("tool.sh")).unwrap(),
        b"#!/bin/sh\n"
    );
    for made in ["README.md", "src", "tool.sh"] {
        let opened = if made == "src" {
            private::open_dir(&folder.join(made)).unwrap()
        } else {
            std::fs::File::open(folder.join(made)).unwrap()
        };
        private::check(&opened).unwrap();
    }
    assert!(std::fs::symlink_metadata(folder.join("link")).is_err());
}

#[test]
fn the_backlink_git_for_windows_writes_matches_the_lease_path() {
    let (_temp, base) = plain_temp();
    let repo = repository(&base);
    let readme = blob(&repo, b"# readme\n");
    let root = tree(&repo, &[("100644", "README.md", readme)]);
    let head = commit(&repo, &root);
    let lease = base.join("lease");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            "w3",
            lease.to_str().unwrap(),
            &head,
        ],
    );
    let admin = repo.join(".git").join("worktrees").join("lease");
    let written = std::fs::read(admin.join("gitdir")).unwrap();
    println!("gitdir bytes: {:?}", String::from_utf8_lossy(&written));
    println!(
        ".git bytes: {:?}",
        String::from_utf8_lossy(&std::fs::read(lease.join(".git")).unwrap())
    );
    assert!(backlink_matches(&written, &lease));
    assert!(!backlink_matches(&written, &base.join("other")));
    assert!(!backlink_matches(&written[..written.len() - 1], &lease));
    assert_eq!(linked_metadata(&repo, &lease).unwrap(), admin);
}

#[test]
fn a_folder_or_file_with_a_null_access_list_is_untrusted() {
    let (_temp, base) = plain_temp();
    let folder = base.join("open-folder");
    std::fs::create_dir(&folder).unwrap();
    let file = base.join("open-file");
    std::fs::write(&file, b"x").unwrap();
    null_list(&folder);
    null_list(&file);
    assert_eq!(
        code(identity(&folder, true)),
        "untrusted_filesystem_identity"
    );
    assert_eq!(
        code(identity(&file, false)),
        "untrusted_filesystem_identity"
    );
}
