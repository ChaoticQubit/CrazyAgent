use crate::credentials::macos::owned_namespace;
use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{Keychain, SecretString, Store, reference};
use crate::engine::keychain::{agent_keychain_name, forget_record};
use crate::perf_kit::{Row, SAMPLES, time};
use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;

const FAST: usize = 1000;
const FILES: usize = 10;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";
const SERVICE: &str = "dev.crazyagent.e0123456789ab.perf";
const AGENT_KEYCHAIN: &str = "agent-0123456789abcdef.keychain-db";
const SECRET: &[u8] = b"ghp_owner-paste-0123456789abcdef0123";

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

/// Times the 2.4.4 path of every 2.5 macOS keychain row, under the 2.5 row names, on the
/// lib tests' own temporary keychains: `Keychain::new` on a namespace that keychain serves
/// stands for `in_home`, and `keychain_text` for `path`. Prints one PERF line per call and
/// asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn security_keychain_calls() {
    let mut scratch = TestKeychain::temporary();
    scratch.serve(NAMESPACE);
    let root = tempfile::tempdir().unwrap();
    let agent_home = std::fs::canonicalize(root.path()).unwrap().join("agent");
    std::fs::create_dir(&agent_home).unwrap();
    std::fs::set_permissions(&agent_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agent = TestKeychain::create_named(&agent_home, AGENT_KEYCHAIN);
    let store = Keychain::at(NAMESPACE, scratch.path()).unwrap();
    let secret = SecretString::new(SECRET.to_vec()).unwrap();
    let found = store.put(SERVICE, "token", &secret).unwrap();
    assert_eq!(store.get(&found).unwrap().expose().as_bytes(), SECRET);
    assert_eq!(
        Keychain::new(NAMESPACE).unwrap().keychain_text(),
        store.keychain_text()
    );
    assert!(agent_keychain_name(AGENT_KEYCHAIN) && owned_namespace(NAMESPACE).is_ok());
    forget_record(&agent_home, agent.path(), SERVICE).unwrap();
    time(&row("keychain.agent_keychain_name", FAST), SAMPLES, || {
        black_box(agent_keychain_name(black_box(AGENT_KEYCHAIN)));
    });
    time(&row("store.macos.owned_namespace", FAST), SAMPLES, || {
        black_box(owned_namespace(black_box(NAMESPACE)).unwrap());
    });
    time(&row("store.macos.Keychain.at", FILES), SAMPLES, || {
        black_box(Keychain::at(NAMESPACE, black_box(scratch.path())).unwrap());
    });
    time(&row("store.macos.Keychain.path", FAST), SAMPLES, || {
        black_box(black_box(&store).keychain_text());
    });
    time(&row("store.macos.Keychain.in_home", ONE), SAMPLES, || {
        black_box(Keychain::new(black_box(NAMESPACE)).unwrap());
    });
    time(&row("store.macos.Keychain.put", ONE), SAMPLES, || {
        black_box(store.put(SERVICE, "token", &secret).unwrap());
    });
    time(&row("store.macos.Keychain.get", ONE), SAMPLES, || {
        black_box(store.get(&found).unwrap());
    });
    let accounts: Vec<_> = (0..ONE_SHOT)
        .map(|at| store.put(SERVICE, &format!("gone-{at}"), &secret).unwrap())
        .collect();
    let mut next = accounts.iter();
    time(&row("store.macos.Keychain.delete", ONE), SAMPLES, || {
        store.delete(next.next().unwrap()).unwrap();
    });
    assert!(next.next().is_none());
    time(
        &row("store.macos.Keychain.git_credential_helper", FAST),
        SAMPLES,
        || {
            black_box(store.git_credential_helper(&found, "perf").unwrap());
        },
    );
    time(&row("keychain.forget_record", ONE), SAMPLES, || {
        forget_record(&agent_home, agent.path(), SERVICE).unwrap();
    });
    assert_eq!(reference(SERVICE, "token").unwrap(), found);
}
