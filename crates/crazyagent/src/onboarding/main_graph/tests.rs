use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn project(base: &Path) -> PathBuf {
    let root = base.join("project");
    fs::create_dir_all(root.join(CRAZYAGENT_DIRECTORY)).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    let graph = root.join("graphify-out");
    fs::create_dir_all(graph.join("wiki").join("deep")).unwrap();
    fs::write(graph.join("graph.json"), "{\"nodes\":1}").unwrap();
    fs::write(graph.join("GRAPH_REPORT.md"), "report\n").unwrap();
    fs::write(graph.join("wiki").join("deep").join("a.md"), "deep\n").unwrap();
    root
}

fn build(digit: char, built_ms: i64) -> Build {
    Build {
        tip: digit.to_string().repeat(40),
        built_ms,
    }
}

fn worktree(base: &Path, name: &str) -> PathBuf {
    let folder = base.join(name);
    fs::create_dir(&folder).unwrap();
    folder
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(path).unwrap()
}

/// An agent's copy holds every regular file and folder of the published graph as its own
/// files, never a link, and a link in the project's graph is neither followed nor copied,
/// so nothing outside `graphify-out` reaches the copy (DEC-123). The agent's change to its
/// copy leaves the project's graph as it was.
#[test]
fn a_copy_holds_the_files_and_never_follows_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = project(&base);
    fs::write(base.join("secret.txt"), "outside secret").unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside").join("b.md"), "outside folder").unwrap();
    symlink(
        base.join("secret.txt"),
        root.join("graphify-out").join("secret"),
    )
    .unwrap();
    symlink(base.join("outside"), root.join("graphify-out").join("out")).unwrap();

    assert!(publish(&root, &build('a', 1_000)).unwrap());
    let agent = worktree(&base, "agent");
    assert!(copy_into(&root, &agent).unwrap());

    let copy = agent.join("graphify-out");
    assert_eq!(read(copy.join("graph.json")), "{\"nodes\":1}");
    assert_eq!(read(copy.join("GRAPH_REPORT.md")), "report\n");
    assert_eq!(read(copy.join("wiki").join("deep").join("a.md")), "deep\n");
    for entry in ["graph.json", "GRAPH_REPORT.md", "wiki"] {
        let found = fs::symlink_metadata(copy.join(entry)).unwrap();
        assert!(
            !found.file_type().is_symlink(),
            "{entry} is the agent's own"
        );
    }
    for link in ["secret", "out"] {
        assert!(
            fs::symlink_metadata(copy.join(link)).is_err(),
            "{link} left out"
        );
    }

    fs::write(copy.join("graph.json"), "updated by the agent").unwrap();
    assert_eq!(
        read(root.join("graphify-out").join("graph.json")),
        "{\"nodes\":1}"
    );
    assert!(
        !copy_into(&root, &agent).unwrap(),
        "the agent's copy is kept"
    );
    assert_eq!(read(copy.join("graph.json")), "updated by the agent");
}

/// Each publish records the tip it was built at, agents copy the newest, and the older copy
/// and any leftover part folder are gone from the store (DEC-123).
#[test]
fn a_publish_records_its_tip_and_keeps_only_the_latest() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = project(&base);
    assert_eq!(latest(&root), None);

    assert!(publish(&root, &build('a', 1_000)).unwrap());
    assert_eq!(latest(&root), Some(build('a', 1_000)));
    let store = root.join(CRAZYAGENT_DIRECTORY).join("graph");
    fs::create_dir(store.join("g-1500.part")).unwrap();
    fs::write(root.join("graphify-out").join("graph.json"), "two").unwrap();
    assert!(publish(&root, &build('b', 2_000)).unwrap());

    assert_eq!(latest(&root), Some(build('b', 2_000)));
    assert_eq!(
        read(store.join("current")),
        format!("v1 {} 2000\n", "b".repeat(40))
    );
    let mut left: Vec<String> = fs::read_dir(&store)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["current", "g-2000", "lock"]);
    let agent = worktree(&base, "agent");
    assert!(copy_into(&root, &agent).unwrap());
    assert_eq!(read(agent.join("graphify-out").join("graph.json")), "two");
}

/// Publishing keeps the copies out of the project's git: `graphify-out/` and `.crazyagent/`
/// are in `.git/info/exclude` afterwards, once each however often it runs, so an imported
/// project whose exclude never named them still gets agent copies (leader 11:2x).
#[test]
fn a_publish_keeps_graphify_out_out_of_git() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = project(&base);
    let exclude = root.join(".git").join("info").join("exclude");
    assert!(!exclude.exists());

    assert!(publish(&root, &build('a', 1_000)).unwrap());
    assert!(publish(&root, &build('b', 2_000)).unwrap());

    let text = read(exclude);
    for line in crate::projects_home::EXCLUDES {
        assert_eq!(
            text.lines().filter(|found| *found == line).count(),
            1,
            "{text}"
        );
    }
}

/// No copy is made without a published graph, nor over what the agent made itself; a
/// project with no `.crazyagent` folder publishes nothing; the read-only links an older
/// engine placed (I3) give way to a copy.
#[test]
fn no_copy_is_made_without_a_published_graph() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = project(&base);
    let fresh = worktree(&base, "fresh");
    assert!(!copy_into(&root, &fresh).unwrap());
    assert!(!fresh.join("graphify-out").exists());

    let bare = base.join("bare");
    fs::create_dir_all(bare.join("graphify-out")).unwrap();
    fs::write(bare.join("graphify-out").join("graph.json"), "{}").unwrap();
    assert!(!publish(&bare, &build('a', 1_000)).unwrap());
    assert_eq!(latest(&bare), None);
    assert!(!bare.join("graph").exists());

    assert!(publish(&root, &build('a', 1_000)).unwrap());
    let owned = worktree(&base, "owned");
    fs::create_dir(owned.join("graphify-out")).unwrap();
    fs::write(owned.join("graphify-out").join("graph.json"), "mine").unwrap();
    symlink(
        root.join("graphify-out").join("wiki"),
        owned.join("graphify-out").join("wiki"),
    )
    .unwrap();
    assert!(!copy_into(&root, &owned).unwrap());
    assert_eq!(read(owned.join("graphify-out").join("graph.json")), "mine");

    let older = worktree(&base, "older");
    fs::create_dir(older.join("graphify-out")).unwrap();
    for entry in ["graph.json", "GRAPH_REPORT.md", "wiki"] {
        symlink(
            root.join("graphify-out").join(entry),
            older.join("graphify-out").join(entry),
        )
        .unwrap();
    }
    assert!(copy_into(&root, &older).unwrap());
    let graph = older.join("graphify-out").join("graph.json");
    assert!(fs::symlink_metadata(&graph).unwrap().file_type().is_file());
    assert_eq!(read(graph), "{\"nodes\":1}");
}

/// A graph is due when none is published or the main branch moved since its build; the
/// record reads back only as written (DEC-123).
#[test]
fn a_graph_is_due_when_missing_or_the_tip_moved() {
    let tip = "a".repeat(40);
    assert!(due(None, &tip));
    assert!(!due(Some(&build('a', 1_000)), &tip));
    assert!(due(Some(&build('b', 1_000)), &tip));

    let written = build('c', 42);
    assert_eq!(Build::parse(&written.record()), Some(written));
    for text in [
        format!("v2 {} 42\n", "c".repeat(40)),
        format!("v1 {} 42\n", "C".repeat(40)),
        format!("v1 {} 42\n", "c".repeat(39)),
        format!("v1 {} 0\n", "c".repeat(40)),
        format!("v1 {} 42 x\n", "c".repeat(40)),
        format!("v1 {} 42", "c".repeat(40)),
    ] {
        assert_eq!(Build::parse(&text), None, "{text:?}");
    }
    assert!(tip_valid(&"0f".repeat(32)));
}

/// Main graphs are built only while the code graph switch is on, and never on Linux
/// (DEC-094), whatever the switch says.
#[test]
fn no_graph_is_built_while_the_switch_is_off_or_on_linux() {
    assert!(wanted(Some(true), "macos"));
    assert!(!wanted(Some(false), "macos"));
    assert!(!wanted(None, "macos"));
    assert!(!wanted(Some(true), "linux"));
}
