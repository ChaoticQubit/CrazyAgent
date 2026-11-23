#[cfg(target_os = "macos")]
mod keychain_support;

use kernel_security::secret::SecretString;
use kernel_security::store::{Store, reference};
use std::time::{Duration, Instant};

const SAMPLES: usize = 200;
const SAMPLES_MIN: usize = 100;
const PERCENTILE: usize = 95;
const _: () = assert!(SAMPLES >= SAMPLES_MIN && PERCENTILE < 100);

/// Opens a scratch store that is never the owner's: a temporary keychain file in a
/// temporary home (`tests/keychain_support/mod.rs`, deleted on drop). Input: the
/// namespace. Output: the store and the guard that removes the keychain.
#[cfg(target_os = "macos")]
fn scratch(namespace: &str) -> (Box<dyn Store>, keychain_support::TemporaryKeychain) {
    let keychain = keychain_support::TemporaryKeychain::create();
    let store = crazyagent::credentials::Keychain::at(namespace, keychain.path()).unwrap();
    (Box::new(store), keychain)
}

/// Opens a scratch store that is never the owner's: the OS keyring under a random test
/// namespace, as `tests/credential_platforms.rs` does. Input: the namespace. Output: the
/// store and an empty guard.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn scratch(namespace: &str) -> (Box<dyn Store>, ()) {
    let engine = std::path::Path::new(env!("CARGO_BIN_EXE_crazyagent"));
    let store = crazyagent::credentials::OsKeyring::open(namespace, engine).unwrap();
    (Box::new(store), ())
}

/// The sample at the given percentile of an ascending list. Inputs: the sorted samples
/// (at least `SAMPLES_MIN`) and the percentile below 100. Output: that sample.
fn percentile(sorted: &[Duration], rank: usize) -> Duration {
    assert!(sorted.len() >= SAMPLES_MIN);
    assert!(rank < 100);
    assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
    sorted[sorted.len() * rank / 100]
}

/// Measures p95 of put, get and delete on a scratch store, printing one line per call.
/// Inputs: none. Output: the printed numbers; the row fails only when a call errors, never
/// on a number (the gate is read from the printed lines).
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
#[ignore = "budget row: run with --ignored --nocapture on a quiet and a loaded Mac"]
fn store_calls_p95() {
    let mut random = [0_u8; 6];
    getrandom::fill(&mut random).unwrap();
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let namespace = format!("dev.crazyagent.test-{suffix}");
    let (store, _guard) = scratch(&namespace);
    let service = format!("{namespace}.budget");
    let mut put = Vec::with_capacity(SAMPLES);
    let mut get = Vec::with_capacity(SAMPLES);
    let mut delete = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let account = format!("a{index}");
        let secret = SecretString::new(b"budget-secret".to_vec()).unwrap();
        let started = Instant::now();
        let found = store.put(&service, &account, &secret).unwrap();
        put.push(started.elapsed());
        let started = Instant::now();
        let read = store.get(&found).unwrap();
        get.push(started.elapsed());
        assert_eq!(read.expose(), "budget-secret");
        let started = Instant::now();
        store.delete(&found).unwrap();
        delete.push(started.elapsed());
        assert_eq!(found, reference(&service, &account).unwrap());
    }
    assert!(store.get(&reference(&service, "a0").unwrap()).is_err());
    for (name, mut samples) in [("put", put), ("get", get), ("delete", delete)] {
        assert_eq!(samples.len(), SAMPLES);
        samples.sort();
        let p95 = percentile(&samples, PERCENTILE);
        println!(
            "security_budget store.{name} samples={SAMPLES} p95_us={}",
            p95.as_micros()
        );
    }
}
