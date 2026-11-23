use super::{ACCOUNT_KEYS, read_key_file, remove_key_file, replace_key_file, store_key_file};
use crate::harnesses::launch::low_probe::{Try, low_probe, probe};
use crate::protocol::CredentialRef;
use crate::sandbox::ProjectSandbox;
use crate::sandbox::windows::WindowsSandbox;
use crazyagent_win_sandbox::{dacl, private};
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::profile::command::Sandbox;
use kernel_sandbox::windows::HELPER_NAME;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const NAME: &str = "anthropic-key-0123456789abcdef0123";
const OTHER: &str = "openai-key-0123456789abcdef0123";

fn reference(name: &str) -> CredentialRef {
    CredentialRef::new(name).unwrap()
}

/// A temp engine root made as a W1 private folder, as the engine's data folder is.
/// Output: the temp guard and the engine root path inside it.
fn engine_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("engine");
    drop(private::dir(&root).unwrap());
    (temp, root)
}

/// The names in one folder, sorted. Input: the folder. Output: the names.
fn names(folder: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    found
}

fn saved(root: &Path, name: &str) -> String {
    read_key_file(root, &reference(name))
        .unwrap()
        .unwrap()
        .expose()
        .to_owned()
}

/// Asserts the key file passes W1's check and carries the secret file's no-read-up label on
/// itself, as `private::secret_file` makes it. Input: the key path.
fn assert_secret_file(path: &Path) {
    let file = private::read_only(path).unwrap();
    private::check(&file).unwrap();
    let facts = private::facts(&file).unwrap();
    assert_eq!(facts.links, 1);
    assert!(!facts.reparse);
    let label = facts.label.expect("a stored key carries a label");
    assert_eq!(
        dacl::label(&label).unwrap(),
        dacl::label(&private::secret_file_label()).unwrap()
    );
}

fn run(program: &str, arguments: &[&str]) {
    let status = kernel_processes::spawn_gate::status(
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "{program} {arguments:?}");
}

/// K1 row 1: a key is stored as a labelled W1 secret file, read back, refused a second store,
/// replaced with no staged file left, and removed, after which a second remove is fine.
#[test]
fn a_key_is_stored_read_replaced_and_removed_as_a_labelled_secret_file() {
    let (_temp, root) = engine_root();
    let keys = root.join(ACCOUNT_KEYS);
    assert!(read_key_file(&root, &reference(NAME)).unwrap().is_none());
    store_key_file(&root, &reference(NAME), b"sk-ant-first").unwrap();
    assert_eq!(saved(&root, NAME), "sk-ant-first");
    assert_secret_file(&keys.join(NAME));
    assert_eq!(
        store_key_file(&root, &reference(NAME), b"sk-ant-second")
            .unwrap_err()
            .0,
        "account_key_store_unavailable"
    );
    assert_eq!(saved(&root, NAME), "sk-ant-first");
    replace_key_file(&root, &reference(NAME), b"sk-ant-second").unwrap();
    assert_eq!(saved(&root, NAME), "sk-ant-second");
    assert_secret_file(&keys.join(NAME));
    assert_eq!(names(&keys), [NAME]);
    remove_key_file(&root, NAME).unwrap();
    assert!(read_key_file(&root, &reference(NAME)).unwrap().is_none());
    assert!(names(&keys).is_empty());
    remove_key_file(&root, NAME).unwrap();
}

/// K1 row 2: a process of the same user at low integrity cannot read a stored or replaced
/// key, while it reads an unlabelled file beside the engine root and the engine reads the key.
#[test]
fn a_low_integrity_process_cannot_read_a_stored_key() {
    let (temp, root) = engine_root();
    let plain = temp.path().join("plain.txt");
    std::fs::write(&plain, "plain").unwrap();
    let key = root.join(ACCOUNT_KEYS).join(NAME);
    store_key_file(&root, &reference(NAME), b"sk-ant-low").unwrap();
    let low = low_probe(temp.path());
    let seen = probe(&low, &[(Try::Read, &plain), (Try::Read, &key)]);
    assert!(
        seen.contains(&format!("READ {}", plain.display())),
        "{seen}"
    );
    assert!(
        seen.contains(&format!("DENIED {}", key.display())),
        "{seen}"
    );
    assert_eq!(saved(&root, NAME), "sk-ant-low");
    replace_key_file(&root, &reference(NAME), b"sk-ant-low-replaced").unwrap();
    let seen = probe(&low, &[(Try::Read, &key)]);
    assert!(
        seen.contains(&format!("DENIED {}", key.display())),
        "{seen}"
    );
    assert_eq!(saved(&root, NAME), "sk-ant-low-replaced");
}

/// The boundary helper the build puts beside the test binary's folder. Output: its path.
fn helper() -> PathBuf {
    let tests = std::env::current_exe().unwrap();
    let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
    assert!(
        helper.is_file(),
        "build crazyagent-win-boundary first: {}",
        helper.display()
    );
    helper
}

/// Whether the agent's box reads `path` with `type`, output discarded. The path goes unquoted,
/// as std quotes an argument holding a space and escapes an inner quote as `\"`, which cmd
/// does not read, so a path with a space is refused here. Inputs: the installed sandbox, the
/// project folder and the path. Output: whether `type` succeeded.
fn boxed_read(installed: &WindowsSandbox, project: &Path, path: &Path) -> bool {
    let named = path.to_str().unwrap();
    assert!(!named.contains(' '), "{named}");
    assert!(!named.contains('"'), "{named}");
    let shell = PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    let mut command = Command::new(shell);
    command
        .arg("/C")
        .arg(format!("type {named}"))
        .current_dir(project);
    kernel_processes::spawn_gate::status(
        Sandbox::wrap(installed, &command)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap()
    .success()
}

/// K1 row 3: an agent started through the real boundary helper cannot read a key stored or
/// replaced in an engine root inside its own granted project, while it reads a plain file
/// beside the key folder there.
#[test]
fn an_app_container_agent_cannot_read_a_key_inside_its_project() {
    let temp = tempfile::tempdir().unwrap();
    let base = kernel_sandbox::paths::canonical(temp.path()).unwrap();
    let project = base.join("project");
    let home = base.join("agent").join("home");
    let tmp = base.join("agent").join("tmp");
    let profiles = base.join("profiles");
    let grants = base.join("grants");
    for folder in [&project, &home, &tmp, &profiles, &grants] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let root = project.join("engine");
    std::fs::create_dir(&root).unwrap();
    let plain = root.join("plain.txt");
    std::fs::write(&plain, "plain").unwrap();
    store_key_file(&root, &reference(NAME), b"sk-ant-box").unwrap();
    let sandbox =
        ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
    let installed =
        WindowsSandbox::install(sandbox, &helper(), &profiles.join("agent.sb"), &grants).unwrap();
    let key = root.join(ACCOUNT_KEYS).join(NAME);
    assert!(
        boxed_read(&installed, &project, &plain),
        "the box reads its project"
    );
    assert!(
        !boxed_read(&installed, &project, &key),
        "the box reads a stored key"
    );
    replace_key_file(&root, &reference(NAME), b"sk-ant-box-replaced").unwrap();
    assert!(
        !boxed_read(&installed, &project, &key),
        "the box reads a replaced key"
    );
    assert_eq!(saved(&root, NAME), "sk-ant-box-replaced");
}

/// K1 row 4: a stored key given a second name by a hard link, a stored key given an Everyone
/// entry, and a key folder that is a junction are each refused as unsafe, and nothing is
/// written through the junction.
#[test]
fn a_swapped_key_file_or_key_folder_is_refused() {
    let (_linked_temp, linked) = engine_root();
    let keys = linked.join(ACCOUNT_KEYS);
    store_key_file(&linked, &reference(NAME), b"sk-ant-linked").unwrap();
    std::fs::hard_link(keys.join(NAME), keys.join(OTHER)).unwrap();
    for name in [NAME, OTHER] {
        assert_eq!(
            read_key_file(&linked, &reference(name)).unwrap_err().0,
            "account_key_store_unsafe",
            "{name}"
        );
    }
    let (_open_temp, open) = engine_root();
    store_key_file(&open, &reference(NAME), b"sk-ant-open").unwrap();
    let file = open.join(ACCOUNT_KEYS).join(NAME);
    run(
        "icacls",
        &[file.to_str().unwrap(), "/grant", "*S-1-1-0:(R)"],
    );
    assert_eq!(
        read_key_file(&open, &reference(NAME)).unwrap_err().0,
        "account_key_store_unsafe"
    );
    let (junction_temp, junction) = engine_root();
    let elsewhere = junction_temp.path().join("elsewhere");
    drop(private::dir(&elsewhere).unwrap());
    let link = junction.join(ACCOUNT_KEYS);
    run(
        "cmd",
        &[
            "/C",
            "mklink",
            "/J",
            link.to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    assert_eq!(
        store_key_file(&junction, &reference(NAME), b"sk-ant-junction")
            .unwrap_err()
            .0,
        "account_key_store_unsafe"
    );
    assert_eq!(
        read_key_file(&junction, &reference(NAME)).unwrap_err().0,
        "account_key_store_unsafe"
    );
    assert!(names(&elsewhere).is_empty());
}

/// K1 row 5: a replace while a reader holds the key fails cleanly: the old key stays and no
/// staged file is left. The outcome is asserted, not an OS error.
#[test]
fn a_replace_while_the_key_is_read_fails_and_keeps_the_old_key() {
    let (_temp, root) = engine_root();
    let keys = root.join(ACCOUNT_KEYS);
    store_key_file(&root, &reference(NAME), b"sk-ant-held").unwrap();
    let held = private::read_only(&keys.join(NAME)).unwrap();
    assert_eq!(
        replace_key_file(&root, &reference(NAME), b"sk-ant-new")
            .unwrap_err()
            .0,
        "account_key_store_unavailable"
    );
    drop(held);
    assert_eq!(saved(&root, NAME), "sk-ant-held");
    assert_eq!(names(&keys), [NAME]);
}

/// K1 row 6 (V244-WIN-KEY-NAME-ALIASES): a name Windows reads as another file is refused by
/// the store, the reader and the remover. A trailing dot or space and device names never
/// become a reference (protocol.rs `single_component`, 686953b5d); a colon names a stream.
#[test]
fn a_key_name_windows_reads_as_another_file_is_refused() {
    for alias in ["key.", "key ", "NUL", "CON", "COM1", "LPT1", "NUL.txt"] {
        assert!(CredentialRef::new(alias).is_err(), "{alias}");
    }
    let (_temp, root) = engine_root();
    let stream = "anthropic-key:stream";
    assert_eq!(
        store_key_file(&root, &reference(stream), b"sk-ant-stream")
            .unwrap_err()
            .0,
        "account_key_reference_invalid"
    );
    assert_eq!(
        read_key_file(&root, &reference(stream)).unwrap_err().0,
        "account_key_reference_invalid"
    );
    assert_eq!(
        remove_key_file(&root, stream).unwrap_err().0,
        "account_key_reference_invalid"
    );
    assert!(!root.join(ACCOUNT_KEYS).exists());
}
