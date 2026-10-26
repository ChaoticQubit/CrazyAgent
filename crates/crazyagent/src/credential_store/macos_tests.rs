use super::*;
use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{SecretString, Store, parts, reference};
use std::process::{Command, Stdio};

struct Scratch {
    store: Keychain,
    namespace: String,
    keychain: TestKeychain,
}

fn suffix() -> String {
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Scratch {
    fn new() -> Self {
        let namespace = format!("dev.crazyagent.test-{}", suffix());
        let mut keychain = TestKeychain::temporary();
        keychain.serve(&namespace);
        Self {
            store: Keychain::new(&namespace).unwrap(),
            namespace,
            keychain,
        }
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

#[test]
fn a_lib_test_store_lives_in_its_own_temporary_keychain() {
    let scratch = Scratch::new();
    assert_eq!(scratch.store.keychain, scratch.keychain.path());
    let value = token();
    let service = scratch.service("github");
    scratch
        .store
        .put(&service, "token", &secret(&value))
        .unwrap();
    let path = scratch.keychain.path().to_str().unwrap().to_owned();
    let found = scratch.keychain.security(
        &[
            "find-generic-password",
            "-s",
            &service,
            "-a",
            "token",
            "-w",
            &path,
        ],
        None,
    );
    assert_eq!(found.trim_end(), value);
}

#[test]
fn a_lib_test_store_with_no_temporary_keychain_is_refused() {
    let namespace = format!("dev.crazyagent.test-unserved-{}", suffix());
    let refused = std::panic::catch_unwind(|| Keychain::new(&namespace).map(drop));
    assert!(
        refused.is_err(),
        "{namespace} opened a store with no temporary keychain"
    );
}

#[test]
fn keychain_round_trip_saves_replaces_reads_and_deletes_a_real_item() {
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
    let other = scratch
        .store
        .put(&service, "other", &secret(&first))
        .unwrap();
    assert_eq!(scratch.store.get(&other).unwrap().expose(), first);
    assert_eq!(scratch.store.get(&item).unwrap().expose(), second);
    scratch.store.delete(&item).unwrap();
    let missing = scratch.store.get(&item).unwrap_err();
    assert_eq!(missing.0, "credential_missing");
    scratch.store.delete(&item).unwrap();
    assert_eq!(scratch.store.get(&other).unwrap().expose(), first);
    scratch.store.delete(&other).unwrap();
    assert_eq!(
        scratch.store.get(&other).unwrap_err().0,
        "credential_missing"
    );
}

#[test]
fn keychain_never_touches_items_outside_its_own_namespace() {
    let scratch = Scratch::new();
    let value = secret(&token());
    let foreign_services = [
        "com.apple.account".to_owned(),
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
    assert!(Keychain::new("dev").is_err());
    assert!(Keychain::new("com.apple.x").is_err());
    assert!(Keychain::new("dev.crazyagent.x'y").is_err());
}

#[test]
fn keychain_git_helper_answers_get_from_the_store_with_no_secret_in_its_text() {
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
    assert!(!helper.contains(&value));
    let home = tempfile::tempdir().unwrap();
    let run = |action: &str| {
        crate::spawn_gate::output(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("{} {action}", &helper[1..]))
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap()
    };
    let answered = run("get");
    assert!(answered.status.success());
    assert_eq!(
        String::from_utf8(answered.stdout).unwrap(),
        format!("username=x-access-token\npassword={value}\n")
    );
    for action in ["store", "erase"] {
        let quiet = run(action);
        assert!(quiet.status.success());
        assert!(quiet.stdout.is_empty());
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
    let gone = run("get");
    assert!(!gone.status.success());
    assert!(gone.stdout.is_empty());
}

#[test]
fn secrets_are_visible_ascii_bounded_and_redacted() {
    for bad in [
        Vec::new(),
        b"has space".to_vec(),
        b"line\n".to_vec(),
        vec![b'a'; crate::credentials::SECRET_BYTES_MAX + 1],
        "caf\u{e9}".as_bytes().to_vec(),
        vec![0x7f],
    ] {
        assert_eq!(
            SecretString::new(bad).unwrap_err().0,
            "credential_secret_invalid"
        );
    }
    let longest = SecretString::new(vec![b'z'; crate::credentials::SECRET_BYTES_MAX]).unwrap();
    assert_eq!(longest.expose().len(), crate::credentials::SECRET_BYTES_MAX);
    let shown = format!("{:?}", secret("gho_visible"));
    assert!(!shown.contains("gho_visible"));
    assert!(reference("dev.crazyagent.a", "b:c").is_err());
    assert!(parts(&crate::protocol::CredentialRef::new("no-colon").unwrap()).is_err());
    assert!(parts(&crate::protocol::CredentialRef::new("a:b:c").unwrap()).is_err());
}
