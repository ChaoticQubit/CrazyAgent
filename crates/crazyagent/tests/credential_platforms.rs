#![cfg(any(target_os = "linux", target_os = "windows"))]

use crazyagent::credentials::{
    GIT_HELPER_COMMAND, OsKeyring, SecretString, Store, parts, platform_store, reference,
};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "linux")]
const STORE_PREREQUISITE: &str = "UNAVAILABLE: a running Secret Service (gnome-keyring or KWallet) \
on the D-Bus session bus with an unlocked default collection";
#[cfg(target_os = "windows")]
const STORE_PREREQUISITE: &str = "UNAVAILABLE: Windows Credential Manager for the signed-in user";
const GIT_PREREQUISITE: &str = "UNAVAILABLE: git on PATH";
const ENGINE: &str = env!("CARGO_BIN_EXE_crazyagent");

struct Scratch {
    store: OsKeyring,
    namespace: String,
}

impl Scratch {
    fn new() -> Self {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let namespace = format!("dev.crazyagent.test-{suffix}");
        let store = OsKeyring::open(&namespace, Path::new(ENGINE)).expect(STORE_PREREQUISITE);
        Self { store, namespace }
    }

    fn service(&self, leaf: &str) -> String {
        format!("{}.{leaf}", self.namespace)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for (leaf, account) in [
            ("github", "token"),
            ("github", "other"),
            ("helper", "token"),
        ] {
            if let Ok(item) = reference(&self.service(leaf), account) {
                let _ = self.store.delete(&item);
            }
        }
    }
}

fn secret(text: &str) -> SecretString {
    SecretString::new(text.as_bytes().to_vec()).unwrap()
}

fn token() -> String {
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).unwrap();
    let tail: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("gho_test{tail}")
}

fn git_credential(helper: &str, operation: &str, request: &str, home: &Path) -> Output {
    let config = home.join("gitconfig");
    std::fs::write(&config, "").unwrap();
    let mut child = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .arg("-c")
        .arg(format!("credential.helper={helper}"))
        .arg("credential")
        .arg(operation)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect(GIT_PREREQUISITE);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn engine_helper(namespace: &str, item: &str, operation: &str) -> Output {
    Command::new(ENGINE)
        .args([
            GIT_HELPER_COMMAND,
            namespace,
            item,
            "x-access-token",
            operation,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn os_keyring_round_trip_saves_replaces_reads_and_deletes_a_real_item() {
    let scratch = Scratch::new();
    let first = token();
    let second = token();
    let service = scratch.service("github");
    let item = scratch
        .store
        .put(&service, "token", &secret(&first))
        .unwrap();
    assert_eq!(item.as_str(), format!("{service}:token"));
    assert!(!item.as_str().contains(&first));
    assert_eq!(parts(&item).unwrap(), (service.as_str(), "token"));
    assert_eq!(scratch.store.get(&item).unwrap().expose(), first);
    let replaced = scratch
        .store
        .put(&service, "token", &secret(&second))
        .unwrap();
    assert_eq!(replaced, item);
    assert_eq!(scratch.store.get(&item).unwrap().expose(), second);
    let shared = platform_store(&scratch.namespace).unwrap();
    assert_eq!(shared.get(&item).unwrap().expose(), second);
    let other = scratch
        .store
        .put(&service, "other", &secret(&first))
        .unwrap();
    assert_eq!(scratch.store.get(&other).unwrap().expose(), first);
    assert_eq!(scratch.store.get(&item).unwrap().expose(), second);
    scratch.store.delete(&item).unwrap();
    assert_eq!(
        scratch.store.get(&item).unwrap_err().0,
        "credential_missing"
    );
    scratch.store.delete(&item).unwrap();
    assert_eq!(scratch.store.get(&other).unwrap().expose(), first);
    scratch.store.delete(&other).unwrap();
    assert_eq!(
        scratch.store.get(&other).unwrap_err().0,
        "credential_missing"
    );
}

#[test]
fn os_keyring_never_touches_items_outside_its_own_namespace() {
    let scratch = Scratch::new();
    let value = secret(&token());
    let foreign_services = [
        "org.freedesktop.secrets".to_owned(),
        "dev.crazyagent".to_owned(),
        "dev.crazyagent.github".to_owned(),
        scratch.namespace.clone(),
        format!("{}x", scratch.namespace),
        format!("{}x.github", scratch.namespace),
    ];
    for service in foreign_services.iter().map(String::as_str) {
        let refused = scratch.store.put(service, "token", &value).unwrap_err();
        assert_eq!(refused.0, "credential_namespace_refused", "{service}");
        let foreign = reference(service, "token").unwrap();
        assert_eq!(
            scratch.store.get(&foreign).unwrap_err().0,
            "credential_namespace_refused"
        );
        assert_eq!(
            scratch.store.delete(&foreign).unwrap_err().0,
            "credential_namespace_refused"
        );
        assert_eq!(
            scratch
                .store
                .git_credential_helper(&foreign, "x-access-token")
                .unwrap_err()
                .0,
            "credential_namespace_refused"
        );
    }
    let owned = scratch.service("github");
    for (service, account) in [
        ("", "token"),
        ("Dev.Crazyagent", "token"),
        (owned.as_str(), "to ken"),
        (owned.as_str(), "to'ken"),
        (owned.as_str(), ".token"),
        (owned.as_str(), ""),
    ] {
        assert_eq!(
            scratch.store.put(service, account, &value).unwrap_err().0,
            "credential_name_invalid"
        );
    }
    let engine = Path::new(ENGINE);
    for namespace in ["dev", "dev.crazyagent.", "com.example.x"] {
        assert!(OsKeyring::open(namespace, engine).is_err(), "{namespace}");
    }
    assert_eq!(
        OsKeyring::open("dev.crazyagent.x'y", engine)
            .err()
            .unwrap()
            .0,
        "credential_name_invalid"
    );
}

#[test]
fn os_keyring_git_helper_answers_get_through_real_git_with_no_secret_in_its_text() {
    let scratch = Scratch::new();
    let value = token();
    let item = scratch
        .store
        .put(&scratch.service("helper"), "token", &secret(&value))
        .unwrap();
    let helper = scratch
        .store
        .git_credential_helper(&item, "x-access-token")
        .unwrap();
    assert!(helper.starts_with('!'));
    assert!(helper.contains(GIT_HELPER_COMMAND));
    assert!(!helper.contains(&value));
    let home = tempfile::tempdir().unwrap();
    let request = "protocol=https\nhost=github.com\n\n";
    let filled = git_credential(&helper, "fill", request, home.path());
    assert!(
        filled.status.success(),
        "{}",
        String::from_utf8_lossy(&filled.stderr)
    );
    let answer = String::from_utf8(filled.stdout).unwrap();
    let lines = answer.lines().collect::<Vec<_>>();
    assert!(lines.contains(&"username=x-access-token"), "{lines:?}");
    assert!(lines.contains(&format!("password={value}").as_str()));
    let other = token();
    let offered =
        format!("protocol=https\nhost=github.com\nusername=x-access-token\npassword={other}\n\n");
    for operation in ["approve", "reject"] {
        let quiet = git_credential(&helper, operation, &offered, home.path());
        assert!(quiet.status.success(), "{operation}");
        assert!(quiet.stdout.is_empty(), "{operation}");
        assert_eq!(scratch.store.get(&item).unwrap().expose(), value);
    }
    assert_eq!(
        scratch
            .store
            .git_credential_helper(&item, "x access")
            .unwrap_err()
            .0,
        "credential_name_invalid"
    );
    scratch.store.delete(&item).unwrap();
    let gone = git_credential(&helper, "fill", request, home.path());
    assert!(!gone.status.success());
    assert!(!String::from_utf8_lossy(&gone.stdout).contains("password="));
}

#[test]
fn engine_helper_command_refuses_items_outside_the_named_namespace() {
    let scratch = Scratch::new();
    let value = token();
    let item = scratch
        .store
        .put(&scratch.service("helper"), "token", &secret(&value))
        .unwrap();
    let answered = engine_helper(&scratch.namespace, item.as_str(), "get");
    assert!(answered.status.success());
    assert_eq!(
        String::from_utf8(answered.stdout).unwrap(),
        format!("username=x-access-token\npassword={value}\n")
    );
    for operation in ["store", "erase"] {
        let quiet = engine_helper(&scratch.namespace, item.as_str(), operation);
        assert!(quiet.status.success(), "{operation}");
        assert!(quiet.stdout.is_empty(), "{operation}");
    }
    let refused = engine_helper("dev.crazyagent.elsewhere", item.as_str(), "get");
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("credential_namespace_refused"));
    let shaped = engine_helper(&scratch.namespace, "no-colon", "get");
    assert!(!shaped.status.success());
    assert!(shaped.stdout.is_empty());
    assert_eq!(scratch.store.get(&item).unwrap().expose(), value);
}
