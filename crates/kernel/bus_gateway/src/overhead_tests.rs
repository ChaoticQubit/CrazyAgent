use std::hint::black_box;
use std::time::Instant;

use crate::{Gateway, Grants, Refusals};
use sdk::settings::{self, LeftRunning, OnQuit, Provide, QuitSettings};
use sdk::{Capability, Refusal, Tier};

/// The timed batches per path.
const ROUNDS: usize = 1_000;

/// The calls in one timed batch.
const BATCH: u32 = 1_000;

/// The plugin id the row binds, granted the one permission the quit save needs.
const PLUGIN: &str = "overhead";

/// A settings provider that does nothing, so a timing holds only the call path.
struct Noop;

impl Provide for Noop {
    fn save_quit(&self, caller: &'static str, settings: QuitSettings) -> Result<(), Refusal> {
        black_box((caller, settings));
        Ok(())
    }
}

/// A secrets provider the row never reaches: it times only the quit save.
struct NoSecrets;

impl sdk::secrets::Provide for NoSecrets {
    fn put_from_owner(&self, _: &'static str, _: String) -> Result<sdk::secrets::Secret, Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn find(&self, _: &'static str, _: String) -> Result<Option<sdk::secrets::Secret>, Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn forget(&self, _: &'static str, _: sdk::secrets::Secret) -> Result<(), Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }
}

/// A refusal sink the row never reaches: the bound plugin holds its grant.
struct Unreached;

impl Refusals for Unreached {
    fn refused(&self, plugin: &'static str, capability: &Capability, _: &Refusal) {
        panic!("the overhead row was refused: {plugin} {}", capability.name);
    }
}

/// The OWNER Q4 budget for the overhead p95 in picoseconds, by the Mac's condition the
/// caller names in `OVERHEAD_MAC`: 1 us quiet, 5 us loaded. Output: the budget and the
/// condition; panics when the condition is not named.
fn budget() -> (u64, &'static str) {
    match std::env::var("OVERHEAD_MAC").as_deref() {
        Ok("quiet") => (1_000_000, "quiet"),
        Ok("loaded") => (5_000_000, "loaded"),
        other => panic!("set OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded, got {other:?}"),
    }
}

/// Times one batch of a call. Input: the call. Output: picoseconds per call.
fn batch(call: &mut impl FnMut()) -> u64 {
    let start = Instant::now();
    for _ in 0..BATCH {
        call();
    }
    let picos = start.elapsed().as_nanos().saturating_mul(1_000) / u128::from(BATCH);
    u64::try_from(picos).unwrap_or(u64::MAX)
}

/// The value at a percentile of sorted samples. Inputs: the samples and the percentile.
/// Output: the sample.
fn at(sorted: &[u64], percentile: usize) -> u64 {
    assert!(!sorted.is_empty() && percentile <= 100);
    assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
    sorted[(sorted.len() * percentile / 100).min(sorted.len() - 1)]
}

/// Prints one path's p50, p95 and p99 in nanoseconds per call. Inputs: its name and its
/// sorted samples in picoseconds. Output: its p95.
fn report(name: &str, sorted: &[u64]) -> u64 {
    let ns = |picos: u64| picos as f64 / 1_000.0;
    let (p50, p95, p99) = (at(sorted, 50), at(sorted, 95), at(sorted, 99));
    println!(
        "{name:<8} ns/call p50 {:.3} p95 {:.3} p99 {:.3}",
        ns(p50),
        ns(p95),
        ns(p99)
    );
    p95
}

/// Times the SDK call overhead against OWNER Q4, run on demand on a named Mac condition:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-bus-gateway --lib overhead_tests:: -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac). Prints p50, p95 and p99 ns per call for the
/// direct call, the SDK call and their difference; fails when the difference's p95 is over.
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn overhead_per_sdk_call() {
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &["settings.write"])]).unwrap();
    let provider = Noop;
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &provider, &NoSecrets, &refusals);
    let bound = gateway.bind(PLUGIN);
    let input = QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    };
    let mut direct_call = || black_box(provider.save_quit(PLUGIN, black_box(input))).unwrap();
    let mut sdk_call = || black_box(settings::save_quit(&bound, black_box(input))).unwrap();
    for _ in 0..ROUNDS / 10 {
        batch(&mut direct_call);
        batch(&mut sdk_call);
    }
    let mut direct = Vec::with_capacity(ROUNDS);
    let mut through = Vec::with_capacity(ROUNDS);
    let mut overhead = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let plain = batch(&mut direct_call);
        let bound_call = batch(&mut sdk_call);
        direct.push(plain);
        through.push(bound_call);
        overhead.push(bound_call.saturating_sub(plain));
    }
    for samples in [&mut direct, &mut through, &mut overhead] {
        samples.sort_unstable();
        assert_eq!(samples.len(), ROUNDS);
    }
    println!("overhead_per_sdk_call {condition}: {ROUNDS} rounds of {BATCH} calls");
    report("direct", &direct);
    report("sdk", &through);
    let p95 = report("overhead", &overhead);
    assert!(
        p95 <= budget,
        "SDK call overhead p95 {:.3} ns over the {condition} budget {:.3} ns",
        p95 as f64 / 1_000.0,
        budget as f64 / 1_000.0
    );
}
