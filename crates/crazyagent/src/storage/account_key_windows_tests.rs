use super::{
    ACCOUNT_KEYS, KEY_BYTES_MAX, PLUGIN_KEYS, key_saved_in, read_key_file, read_key_in,
    remove_key_file, remove_key_in, replace_key_file, replace_key_in, store_key_file,
};
use crate::harnesses::launch::low_probe::{Try, low_probe, probe};
use crate::protocol::CredentialRef;
use crate::sandbox::windows::WindowsSandbox;
use crate::sandbox::{ProjectSandbox, Sandbox};
use crazyagent_win_sandbox::{dacl, private};
use kernel_sandbox::net::SandboxNetwork;
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
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
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

/// Makes a file symbolic link at `link` to `target`, falling back to `mklink` when the std
/// call is refused. Inputs: the link and target paths.
fn file_link(link: &Path, target: &Path) {
    if std::os::windows::fs::symlink_file(target, link).is_err() {
        run(
            "cmd",
            &[
                "/C",
                "mklink",
                link.to_str().unwrap(),
                target.to_str().unwrap(),
            ],
        );
    }
}

fn plugin_saved(root: &Path, name: &str) -> String {
    read_key_in(root, PLUGIN_KEYS, &reference(name))
        .unwrap()
        .unwrap()
        .expose()
        .to_owned()
}

/// A plugin key is saved into its own labelled private folder by a replace of a key that
/// was never saved, read back, replaced with no staged copy left, and removed, after which a
/// second remove is fine; an empty key and one past the bound are refused with nothing
/// written, and the account key folder is never made.
#[test]
fn windows_a_plugin_key_is_saved_read_replaced_and_removed_in_its_own_folder() {
    let (_temp, root) = engine_root();
    let keys = root.join(PLUGIN_KEYS);
    assert!(
        read_key_in(&root, PLUGIN_KEYS, &reference(NAME))
            .unwrap()
            .is_none()
    );
    replace_key_in(
        &root,
        PLUGIN_KEYS,
        &reference(NAME),
        b"sk-plugin-first".to_vec(),
    )
    .unwrap();
    assert_eq!(plugin_saved(&root, NAME), "sk-plugin-first");
    assert_secret_file(&keys.join(NAME));
    replace_key_in(
        &root,
        PLUGIN_KEYS,
        &reference(NAME),
        b"sk-plugin-second".to_vec(),
    )
    .unwrap();
    assert_eq!(plugin_saved(&root, NAME), "sk-plugin-second");
    assert_secret_file(&keys.join(NAME));
    assert_eq!(names(&keys), [NAME]);
    let too_long = vec![b'k'; usize::try_from(KEY_BYTES_MAX).unwrap() + 1];
    for key in [Vec::new(), too_long] {
        assert_eq!(
            replace_key_in(&root, PLUGIN_KEYS, &reference(OTHER), key)
                .unwrap_err()
                .0,
            "account_key_store_unavailable"
        );
    }
    assert_eq!(names(&keys), [NAME]);
    remove_key_in(&root, PLUGIN_KEYS, NAME).unwrap();
    assert!(
        read_key_in(&root, PLUGIN_KEYS, &reference(NAME))
            .unwrap()
            .is_none()
    );
    assert!(names(&keys).is_empty());
    remove_key_in(&root, PLUGIN_KEYS, NAME).unwrap();
    assert!(!root.join(ACCOUNT_KEYS).exists());
}

/// Whether a key is saved is answered without reading it: true for a saved key in either
/// folder, false for a missing folder, a missing key, a file link and a folder in its place.
#[test]
fn windows_key_saved_in_sees_a_saved_key_and_not_a_link_or_a_missing_one() {
    let (temp, root) = engine_root();
    for folder in [PLUGIN_KEYS, ACCOUNT_KEYS] {
        assert!(
            !key_saved_in(&root, folder, &reference(NAME)).unwrap(),
            "{folder}"
        );
        replace_key_in(&root, folder, &reference(NAME), b"sk-saved-key".to_vec()).unwrap();
        assert!(
            key_saved_in(&root, folder, &reference(NAME)).unwrap(),
            "{folder}"
        );
        assert!(
            !key_saved_in(&root, folder, &reference(OTHER)).unwrap(),
            "{folder}"
        );
    }
    let target = temp.path().join("target.txt");
    std::fs::write(&target, "sk-elsewhere").unwrap();
    file_link(&root.join(PLUGIN_KEYS).join(OTHER), &target);
    assert!(!key_saved_in(&root, PLUGIN_KEYS, &reference(OTHER)).unwrap());
    std::fs::create_dir(root.join(ACCOUNT_KEYS).join(OTHER)).unwrap();
    assert!(!key_saved_in(&root, ACCOUNT_KEYS, &reference(OTHER)).unwrap());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "sk-elsewhere");
}

/// The start sweep deletes the staged key copies a crash left in both key folders and keeps
/// every saved key; an engine root with no key folder sweeps clean.
#[test]
fn windows_the_start_sweep_deletes_staged_key_copies_and_keeps_keys() {
    let (_empty_temp, empty) = engine_root();
    crate::app_plugins::sweep_staged(&empty);
    let (_temp, root) = engine_root();
    for folder in [PLUGIN_KEYS, ACCOUNT_KEYS] {
        replace_key_in(&root, folder, &reference(NAME), b"sk-kept-key".to_vec()).unwrap();
        for staged in [".replace-0000000000000001", ".replace-crashed"] {
            std::fs::write(root.join(folder).join(staged), b"sk-staged-copy").unwrap();
        }
    }
    crate::app_plugins::sweep_staged(&root);
    for folder in [PLUGIN_KEYS, ACCOUNT_KEYS] {
        assert_eq!(names(&root.join(folder)), [NAME], "{folder}");
        assert_eq!(
            read_key_in(&root, folder, &reference(NAME))
                .unwrap()
                .unwrap()
                .expose(),
            "sk-kept-key"
        );
    }
}

/// A plugin key folder that is a junction is refused as unsafe by the read, the check, the
/// save and the delete, and the folder it points to is left as it was.
#[test]
fn windows_a_plugin_key_folder_that_is_a_junction_is_refused() {
    let (temp, root) = engine_root();
    let elsewhere = temp.path().join("elsewhere");
    drop(private::dir(&elsewhere).unwrap());
    std::fs::write(elsewhere.join(NAME), b"sk-elsewhere").unwrap();
    let link = root.join(PLUGIN_KEYS);
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
    let unsafe_code = "account_key_store_unsafe";
    assert_eq!(
        read_key_in(&root, PLUGIN_KEYS, &reference(NAME))
            .unwrap_err()
            .0,
        unsafe_code
    );
    assert_eq!(
        key_saved_in(&root, PLUGIN_KEYS, &reference(NAME))
            .unwrap_err()
            .0,
        unsafe_code
    );
    assert_eq!(
        replace_key_in(&root, PLUGIN_KEYS, &reference(OTHER), b"sk-new".to_vec())
            .unwrap_err()
            .0,
        unsafe_code
    );
    assert_eq!(
        remove_key_in(&root, PLUGIN_KEYS, NAME).unwrap_err().0,
        unsafe_code
    );
    assert_eq!(names(&elsewhere), [NAME]);
    assert_eq!(
        std::fs::read(elsewhere.join(NAME)).unwrap(),
        b"sk-elsewhere"
    );
}

/// A process of the same user at low integrity cannot read a saved or replaced plugin key,
/// while it reads an unlabelled file beside the engine root and the engine reads the key.
#[test]
fn windows_a_low_integrity_process_cannot_read_a_plugin_key() {
    let (temp, root) = engine_root();
    let plain = temp.path().join("plain.txt");
    std::fs::write(&plain, "plain").unwrap();
    let key = root.join(PLUGIN_KEYS).join(NAME);
    replace_key_in(
        &root,
        PLUGIN_KEYS,
        &reference(NAME),
        b"sk-plugin-low".to_vec(),
    )
    .unwrap();
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
    replace_key_in(
        &root,
        PLUGIN_KEYS,
        &reference(NAME),
        b"sk-plugin-low-2".to_vec(),
    )
    .unwrap();
    let seen = probe(&low, &[(Try::Read, &key)]);
    assert!(
        seen.contains(&format!("DENIED {}", key.display())),
        "{seen}"
    );
    assert_eq!(plugin_saved(&root, NAME), "sk-plugin-low-2");
}

/// The start sweep deletes only plugin key files no binding names: a named plugin key, a
/// file that is no plugin key, and every key in the account key folder stay, and with no
/// readable binding table nothing is deleted.
#[test]
fn windows_the_start_sweep_deletes_unnamed_plugin_keys_and_keeps_named_ones() {
    use crate::app_plugins::{KEYS, sweep_at_start};
    use kernel_plugin_host::store::{KeyFolder, KeyRef};
    let (_temp, root) = engine_root();
    let key = |name: &str| KeyRef::new(name).unwrap();
    let save = |folder: KeyFolder, name: &str| {
        (KEYS.replace)(&root, folder, &key(name), b"sk-sweep-key-01".to_vec()).unwrap();
    };
    let saved = |folder: KeyFolder, name: &str| (KEYS.saved)(&root, folder, &key(name)).unwrap();
    save(KeyFolder::Plugin, "plugin-key-named");
    save(KeyFolder::Plugin, "plugin-key-orphan");
    save(KeyFolder::Plugin, "acct-other");
    save(KeyFolder::Account, "plugin-key-account");
    let bare = rusqlite::Connection::open_in_memory().unwrap();
    sweep_at_start(&bare, &root);
    assert!(saved(KeyFolder::Plugin, "plugin-key-orphan"));
    let db = rusqlite::Connection::open_in_memory().unwrap();
    crate::app_plugins::initialize(&db).unwrap();
    db.execute(
        "INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
         VALUES ('acme', 'api_key', 'key', 'plugin-key-named')",
        [],
    )
    .unwrap();
    sweep_at_start(&db, &root);
    assert!(saved(KeyFolder::Plugin, "plugin-key-named"));
    assert!(!saved(KeyFolder::Plugin, "plugin-key-orphan"));
    assert!(saved(KeyFolder::Plugin, "acct-other"));
    assert!(saved(KeyFolder::Account, "plugin-key-account"));
}

/// The plugin host's key store is the engine's own on Windows: a plugin key goes into
/// `plugin-keys` as a labelled secret file, reads back, survives the staged sweep and is
/// deleted, and an account key saved by the account store can be bound.
#[test]
fn windows_the_engine_key_store_backs_the_plugin_host() {
    use crate::app_plugins::{KEYS, sweep_staged};
    use kernel_plugin_host::store::{KeyFolder, KeyRef};
    let (_temp, root) = engine_root();
    let reference_of = KeyRef::new(&format!("plugin-key-{}", (KEYS.new_ref)().unwrap())).unwrap();
    assert!(!(KEYS.saved)(&root, KeyFolder::Plugin, &reference_of).unwrap());
    (KEYS.replace)(
        &root,
        KeyFolder::Plugin,
        &reference_of,
        b"sk-adapter-key-01".to_vec(),
    )
    .unwrap();
    assert!((KEYS.saved)(&root, KeyFolder::Plugin, &reference_of).unwrap());
    assert_secret_file(&root.join(PLUGIN_KEYS).join(reference_of.as_str()));
    let value = (KEYS.read)(&root, KeyFolder::Plugin, &reference_of)
        .unwrap()
        .unwrap();
    assert_eq!(value.expose(), "sk-adapter-key-01");
    sweep_staged(&root);
    assert!((KEYS.saved)(&root, KeyFolder::Plugin, &reference_of).unwrap());
    (KEYS.remove)(&root, KeyFolder::Plugin, &reference_of).unwrap();
    assert!(!(KEYS.saved)(&root, KeyFolder::Plugin, &reference_of).unwrap());
    store_key_file(&root, &reference(NAME), b"sk-ant-account").unwrap();
    assert!((KEYS.saved)(&root, KeyFolder::Account, &KeyRef::new(NAME).unwrap()).unwrap());
}
