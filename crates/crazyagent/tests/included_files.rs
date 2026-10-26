use crazyagent::spawn_gate;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// Runs git in `root` and returns its standard output, failing the test when git
/// cannot run there. Inputs: the work tree and the arguments. Output: the bytes.
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = spawn_gate::output(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Splits git's NUL-separated path list. Input: the bytes. Output: the paths.
fn paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).expect("utf-8 path"))
        .collect()
}

/// Lexically resolves `path` against `base` without touching the disk, so a
/// file that is missing still gets a name. Inputs: both paths. Output: the
/// repository-relative path with forward slashes.
fn resolve(base: &Path, path: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in base.join(path).components() {
        match component {
            Component::ParentDir => {
                assert!(parts.pop().is_some(), "{path} leaves the repository");
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => panic!("{path} is not relative"),
        }
    }
    parts.join("/")
}

/// Lists every file an `include_str!` or `include_bytes!` path literal names in
/// the tracked or unignored Rust sources of the work tree at `root`. Input: the
/// work tree. Output: the sorted repository paths, which must not be empty.
fn included(root: &Path) -> Vec<String> {
    let sources = paths(&git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ],
    ));
    let mut named = Vec::new();
    for source in &sources {
        let text = std::fs::read_to_string(root.join(source)).expect("readable source");
        let base = Path::new(source).parent().unwrap_or(Path::new(""));
        for name in ["include_str", "include_bytes"] {
            let call = format!("{name}!(");
            for (start, _) in text.match_indices(&call) {
                let rest = text[start + call.len()..].trim_start();
                let Some(literal) = rest.strip_prefix('"') else {
                    continue;
                };
                let end = literal.find('"').expect("closed path literal");
                named.push(resolve(base, &literal[..end]));
            }
        }
    }
    named.sort();
    named.dedup();
    assert!(!named.is_empty(), "no included file found under {root:?}");
    named
}

/// Picks the files in `named` that git does not track or that an ignore rule
/// matches, so a new file of that kind would be left out of commits. Inputs:
/// the work tree and repository paths. Output: the offending paths, sorted.
fn unshipped(root: &Path, named: &[String]) -> Vec<String> {
    let tracked = paths(&git(root, &["ls-files", "-z", "--cached"]));
    let mut offending: Vec<String> = named
        .iter()
        .filter(|path| !tracked.contains(path))
        .cloned()
        .collect();
    let mut ignored = spawn_gate::spawn(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["check-ignore", "--no-index", "--stdin", "-z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .expect("git runs");
    {
        use std::io::Write;
        let mut input = ignored.stdin.take().expect("stdin");
        for path in named {
            input.write_all(path.as_bytes()).expect("write");
            input.write_all(&[0]).expect("write");
        }
    }
    let output = ignored.wait_with_output().expect("git check-ignore");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "git check-ignore failed"
    );
    offending.extend(paths(&output.stdout));
    offending.sort();
    offending.dedup();
    offending
}

#[test]
fn every_file_compiled_in_with_include_is_tracked_and_not_ignored() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root =
        String::from_utf8(git(&manifest, &["rev-parse", "--show-toplevel"])).expect("utf-8 root");
    let root = Path::new(root.trim_end());
    let named = included(root);
    assert!(
        named.iter().any(|path| path.starts_with("crates/crazyagent/assets/pack/")
            && path.ends_with(".md")),
        "the pack skill files are not among {named:?}"
    );
    let offending = unshipped(root, &named);
    assert!(
        offending.is_empty(),
        "compiled-in files that a commit would leave out: {offending:?}"
    );
}

#[test]
fn the_check_names_an_untracked_or_ignored_included_file() {
    let folder = tempfile::tempdir().expect("temp folder");
    let root = folder.path();
    git(root, &["init", "-q"]);
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::create_dir_all(root.join("assets/pack")).expect("assets");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub const A: &[u8] = include_bytes!(\n    \"../assets/pack/new.md\"\n);\npub const B: &str = include_str!(\"../assets/pack/kept.txt\");\npub const C: &str = include_str!(\"../assets/pack/old.md\");\n",
    )
    .expect("source");
    std::fs::write(root.join("assets/pack/new.md"), "new").expect("new");
    std::fs::write(root.join("assets/pack/kept.txt"), "kept").expect("kept");
    std::fs::write(root.join("assets/pack/old.md"), "old").expect("old");
    git(
        root,
        &[
            "add",
            "src/lib.rs",
            "assets/pack/kept.txt",
            "assets/pack/old.md",
        ],
    );
    std::fs::write(root.join(".gitignore"), "*.md\n").expect("ignore");
    let named = included(root);
    assert_eq!(
        named,
        [
            "assets/pack/kept.txt",
            "assets/pack/new.md",
            "assets/pack/old.md"
        ]
    );
    assert_eq!(
        unshipped(root, &named),
        ["assets/pack/new.md", "assets/pack/old.md"]
    );
    std::fs::write(root.join(".gitignore"), "*.md\n!assets/pack/**/*.md\n").expect("ignore");
    assert_eq!(unshipped(root, &named), ["assets/pack/new.md"]);
    git(root, &["add", "assets/pack/new.md"]);
    assert!(unshipped(root, &named).is_empty());
}
