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
    let store = kernel_security::store::macos::Keychain::at(namespace, keychain.path()).unwrap();
    (Box::new(store), keychain)
}

/// Opens a scratch store that is never the owner's: the OS keyring under a random test
/// namespace, as `tests/credential_platforms.rs` does. Input: the namespace. Output: the
/// store and an empty guard.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn scratch(namespace: &str) -> (Box<dyn Store>, ()) {
    let engine = std::path::Path::new(env!("CARGO_BIN_EXE_crazyagent"));
    let store = kernel_security::store::os_keyring::OsKeyring::open(namespace, engine).unwrap();
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

/// A secrets provider that does nothing, so a timing holds only the gateway's call path.
struct NoopSecrets;

impl sdk::secrets::Provide for NoopSecrets {
    fn put_from_owner(
        &self,
        _: &'static str,
        _: String,
    ) -> Result<sdk::secrets::Secret, sdk::Refusal> {
        unreachable!("the row times secrets.find only")
    }

    fn find(
        &self,
        caller: &'static str,
        name: String,
    ) -> Result<Option<sdk::secrets::Secret>, sdk::Refusal> {
        std::hint::black_box((caller, name));
        Ok(Some(sdk::secrets::Secret::from_kernel(1)))
    }

    fn forget(&self, _: &'static str, _: sdk::secrets::Secret) -> Result<(), sdk::Refusal> {
        unreachable!("the row times secrets.find only")
    }
}

/// A settings provider and refusal sink the secrets row never reaches.
struct Unreached;

impl sdk::settings::Provide for Unreached {
    fn save_quit(
        &self,
        _: &'static str,
        _: sdk::settings::QuitSettings,
    ) -> Result<(), sdk::Refusal> {
        unreachable!("the row times secrets.find only")
    }
}

impl bus_gateway::Refusals for Unreached {
    fn refused(&self, plugin: &'static str, capability: &sdk::Capability, _: &sdk::Refusal) {
        panic!("the budget row was refused: {plugin} {}", capability.name);
    }
}

/// The OWNER Q4 budget for one gateway call's overhead p95, by the Mac condition named in
/// `OVERHEAD_MAC`: 1 us quiet, 5 us loaded. Output: the budget and the condition.
fn overhead_budget() -> (Duration, &'static str) {
    match std::env::var("OVERHEAD_MAC").as_deref() {
        Ok("quiet") => (Duration::from_micros(1), "quiet"),
        Ok("loaded") => (Duration::from_micros(5), "loaded"),
        other => panic!("set OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded, got {other:?}"),
    }
}

/// Times one batch of calls. Inputs: the batch size and the call. Output: time per call.
fn per_call(calls: u32, call: &mut impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..calls {
        call();
    }
    start.elapsed() / calls
}

/// Measures the gateway overhead of one `secrets.find` call the way A6 times the quit save:
/// the SDK call through a bound host minus the direct provider call, p95 over batches.
/// Run with `OVERHEAD_MAC=quiet|loaded cargo test --release -p crazyagent --test
/// security_budget -- --ignored --nocapture`.
#[test]
#[ignore = "budget row: run with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn gateway_secrets_call_p95() {
    use sdk::secrets::Provide;
    const BATCH: u32 = 1_000;
    let (budget, condition) = overhead_budget();
    let grants = bus_gateway::Grants::new(&[("budget", sdk::Tier::T1, &["secrets.find"])]).unwrap();
    let gateway = bus_gateway::Gateway::new(&grants, &Unreached, &NoopSecrets, &Unreached);
    let bound = gateway.bind("budget");
    let mut direct = || {
        std::hint::black_box(NoopSecrets.find("budget", "token".to_owned())).unwrap();
    };
    let mut through = || {
        std::hint::black_box(sdk::secrets::find(&bound, "token".to_owned())).unwrap();
    };
    let mut overhead = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let base = per_call(BATCH, &mut direct);
        overhead.push(per_call(BATCH, &mut through).saturating_sub(base));
    }
    overhead.sort();
    let p95 = percentile(&overhead, PERCENTILE);
    println!(
        "security_budget gateway.secrets_find condition={condition} samples={SAMPLES} p95_ns={}",
        p95.as_nanos()
    );
    assert!(p95 <= budget, "p95 {p95:?} over {budget:?} ({condition})");
}
