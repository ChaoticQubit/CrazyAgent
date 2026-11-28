use std::hint::black_box;
use std::time::Instant;

use bus_gateway::{AllOn, Gateway, Grants, Refusals};
use sdk::settings::{self, LeftRunning, OnQuit, Provide, QuitSettings};
use sdk::{Capability, Refusal, Tier};

use crate::{Bound, Providers};

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

/// A net provider the row never reaches: it times only the quit save.
struct NoNet;

/// The editor provider of the overhead rows, which never call it.
struct NoEditor;

impl sdk::generated::editor::Provide for NoEditor {
    fn spawn(
        &self,
        _: &'static str,
        _: sdk::generated::editor::EditorLaunch,
    ) -> Result<u32, Refusal> {
        unreachable!("the overhead rows call no editor capability")
    }
}

/// The process provider of the overhead rows, which never call it.
struct NoProcess;

impl sdk::process::Provide for NoProcess {
    fn install(
        &self,
        _: &'static str,
        _: sdk::process::InstallRecipe,
    ) -> Result<sdk::process::InstallResult, Refusal> {
        unreachable!("the overhead rows call no process capability")
    }
}

/// The projects read provider of the overhead rows, which never call it.
struct NoProjectRows;

impl sdk::generated::project_rows::Provide for NoProjectRows {
    fn page(
        &self,
        _: &'static str,
        _: Option<String>,
        _: u16,
    ) -> Result<sdk::generated::project_rows::RowsPage, Refusal> {
        unreachable!("the overhead rows call no projects read")
    }
}

impl sdk::net::Provide for NoNet {
    fn request(&self, _: &'static str, _: sdk::net::Request) -> Result<sdk::net::Reply, Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }
}

/// A service provider the row never reaches: it times only the quit save.
struct NoService;

impl sdk::generated::service::Provide for NoService {
    fn status(&self, _: &'static str) -> Result<sdk::generated::service::ServiceState, Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn repair(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn install(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn uninstall(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }
}

/// A sandbox provider the row never reaches: it times only the quit save.
struct NoSandbox;

impl sdk::generated::sandbox::Provide for NoSandbox {
    fn save_settings(
        &self,
        _: &'static str,
        _: sdk::generated::sandbox::SandboxSettings,
        _: u64,
    ) -> Result<(), Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }

    fn settings(
        &self,
        _: &'static str,
    ) -> Result<sdk::generated::sandbox::StoredSandboxSettings, Refusal> {
        unreachable!("the overhead row calls only settings.save_quit")
    }
}

/// An update provider the row never reaches: it times only the quit save.
struct NoUpdate;

impl sdk::generated::update::Provide for NoUpdate {
    fn set_channel(
        &self,
        _: &'static str,
        _: sdk::generated::update::UpdateChannel,
    ) -> Result<(), Refusal> {
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

/// The budget for the overhead p95 in picoseconds, by the Mac's condition the
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

/// Times the SDK call overhead through the generated binding and the gateway core against
/// its budget, run on demand on a named Mac condition:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-sdk-host --lib overhead_tests:: -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac). Prints p50, p95 and p99 ns per call for the
/// direct call, the SDK call and their difference; fails when the difference's p95 is over.
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn overhead_per_sdk_call() {
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &["settings.write"])]).unwrap();
    let provider = Noop;
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        editor: &NoEditor,
        net: &NoNet,
        process: &NoProcess,
        project_rows: &NoProjectRows,
        sandbox: &NoSandbox,
        secrets: &NoSecrets,
        settings: &provider,
        service: &NoService,
        update: &NoUpdate,
    };
    let bound = Bound::new(gateway.bind(PLUGIN), &providers);
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

/// A secrets provider that finds every name at once, so a timing holds only the call path.
struct Found;

impl sdk::secrets::Provide for Found {
    fn put_from_owner(&self, _: &'static str, _: String) -> Result<sdk::secrets::Secret, Refusal> {
        unreachable!("the secrets row times secrets.find only")
    }

    fn find(
        &self,
        caller: &'static str,
        name: String,
    ) -> Result<Option<sdk::secrets::Secret>, Refusal> {
        black_box((caller, name));
        Ok(Some(sdk::secrets::Secret::from_kernel(1)))
    }

    fn forget(&self, _: &'static str, _: sdk::secrets::Secret) -> Result<(), Refusal> {
        unreachable!("the secrets row times secrets.find only")
    }
}

/// Times one `secrets.find` call's overhead through the generated binding and the gateway
/// core against its budget, the way the quit save row does: the SDK call through a bound
/// host minus the direct provider call. Run:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-sdk-host --lib overhead_tests::gateway_secrets_call_p95 -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac).
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn gateway_secrets_call_p95() {
    use sdk::secrets::Provide;
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T1, &["secrets.find"])]).unwrap();
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        editor: &NoEditor,
        net: &NoNet,
        process: &NoProcess,
        project_rows: &NoProjectRows,
        sandbox: &NoSandbox,
        secrets: &Found,
        settings: &Noop,
        service: &NoService,
        update: &NoUpdate,
    };
    let bound = Bound::new(gateway.bind(PLUGIN), &providers);
    let mut direct_call = || {
        black_box(Found.find(PLUGIN, black_box("token".to_owned()))).unwrap();
    };
    let mut sdk_call = || {
        black_box(sdk::secrets::find(&bound, black_box("token".to_owned()))).unwrap();
    };
    let mut overhead = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let plain = batch(&mut direct_call);
        overhead.push(batch(&mut sdk_call).saturating_sub(plain));
    }
    overhead.sort_unstable();
    let p95 = report("secrets", &overhead);
    println!("gateway.secrets_find condition={condition} p95_ps={p95}");
    assert!(p95 <= budget, "p95 {p95} ps over {budget} ps ({condition})");
}

/// An update provider that saves nothing, so a timing holds only the call path.
struct Saved;

impl sdk::generated::update::Provide for Saved {
    fn set_channel(
        &self,
        caller: &'static str,
        channel: sdk::generated::update::UpdateChannel,
    ) -> Result<(), Refusal> {
        black_box((caller, channel));
        Ok(())
    }
}

/// Times one `update.set_channel` call's overhead through the generated binding and the
/// gateway core against its budget, the way the secrets row does. Run:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-sdk-host --lib overhead_tests::gateway_update_set_channel_p95 -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac).
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn gateway_update_set_channel_p95() {
    use sdk::generated::update::{self, Provide, UpdateChannel};
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &["update.write"])]).unwrap();
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        editor: &NoEditor,
        net: &NoNet,
        process: &NoProcess,
        project_rows: &NoProjectRows,
        sandbox: &NoSandbox,
        secrets: &NoSecrets,
        settings: &Noop,
        service: &NoService,
        update: &Saved,
    };
    let bound = Bound::new(gateway.bind(PLUGIN), &providers);
    let mut direct_call =
        || black_box(Saved.set_channel(PLUGIN, black_box(UpdateChannel::Dev))).unwrap();
    let mut sdk_call =
        || black_box(update::set_channel(&bound, black_box(UpdateChannel::Dev))).unwrap();
    let mut overhead = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let plain = batch(&mut direct_call);
        overhead.push(batch(&mut sdk_call).saturating_sub(plain));
    }
    overhead.sort_unstable();
    let p95 = report("update", &overhead);
    println!("gateway.update_set_channel condition={condition} p95_ps={p95}");
    assert!(p95 <= budget, "p95 {p95} ps over {budget} ps ({condition})");
}

/// A service provider that reports a stand-in state at once, so a timing holds only the
/// call path.
struct Reported;

impl sdk::generated::service::Provide for Reported {
    fn status(
        &self,
        caller: &'static str,
    ) -> Result<sdk::generated::service::ServiceState, Refusal> {
        black_box(caller);
        Ok(sdk::generated::service::ServiceState {
            installed: true,
            running: true,
            current: true,
        })
    }

    fn repair(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the service row times service.status only")
    }

    fn install(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the service row times service.status only")
    }

    fn uninstall(&self, _: &'static str) -> Result<(), Refusal> {
        unreachable!("the service row times service.status only")
    }
}

/// Times one `service.status` call's overhead, a call with no inputs and a named output,
/// through the generated binding and the gateway core against its budget, the way the
/// secrets row does. Run:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-sdk-host --lib overhead_tests::gateway_service_status_p95 -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac).
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn gateway_service_status_p95() {
    use sdk::generated::service::{self, Provide};
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &["service.read"])]).unwrap();
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        editor: &NoEditor,
        net: &NoNet,
        process: &NoProcess,
        project_rows: &NoProjectRows,
        sandbox: &NoSandbox,
        secrets: &NoSecrets,
        settings: &Noop,
        service: &Reported,
        update: &NoUpdate,
    };
    let bound = Bound::new(gateway.bind(PLUGIN), &providers);
    let mut direct_call = || {
        black_box(Reported.status(PLUGIN)).unwrap();
    };
    let mut sdk_call = || {
        black_box(service::status(&bound)).unwrap();
    };
    let mut overhead = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let plain = batch(&mut direct_call);
        overhead.push(batch(&mut sdk_call).saturating_sub(plain));
    }
    overhead.sort_unstable();
    let p95 = report("service", &overhead);
    println!("gateway.service_status condition={condition} p95_ps={p95}");
    assert!(p95 <= budget, "p95 {p95} ps over {budget} ps ({condition})");
}

/// A sandbox provider that answers a save at once, so a timing holds only the call path.
struct SavedSandbox;

impl sdk::generated::sandbox::Provide for SavedSandbox {
    fn save_settings(
        &self,
        caller: &'static str,
        settings: sdk::generated::sandbox::SandboxSettings,
        version: u64,
    ) -> Result<(), Refusal> {
        black_box((caller, settings, version));
        Ok(())
    }

    fn settings(
        &self,
        _: &'static str,
    ) -> Result<sdk::generated::sandbox::StoredSandboxSettings, Refusal> {
        unreachable!("the sandbox row times sandbox.save_settings only")
    }
}

/// Times one `sandbox.save_settings` call's overhead through the generated binding and the
/// gateway core against its budget, the way the secrets row does. Run:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-sdk-host --lib overhead_tests::gateway_sandbox_save_settings_p95 -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac).
#[test]
#[ignore = "timing row, run on demand with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn gateway_sandbox_save_settings_p95() {
    use sdk::generated::sandbox::{
        self, LinuxSockets, LinuxStrict, LinuxSupervisor, Provide, SandboxSettings,
    };
    let (budget, condition) = budget();
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &["sandbox.settings_write"])]).unwrap();
    let refusals = Unreached;
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        editor: &NoEditor,
        net: &NoNet,
        process: &NoProcess,
        project_rows: &NoProjectRows,
        sandbox: &SavedSandbox,
        secrets: &NoSecrets,
        settings: &Noop,
        service: &NoService,
        update: &NoUpdate,
    };
    let bound = Bound::new(gateway.bind(PLUGIN), &providers);
    let settings = SandboxSettings {
        linux_sockets: LinuxSockets::Refuse,
        linux_supervisor: LinuxSupervisor::Off,
        linux_strict: LinuxStrict::On,
    };
    let mut direct_call = || {
        black_box(SavedSandbox.save_settings(PLUGIN, black_box(settings), black_box(3))).unwrap();
    };
    let mut sdk_call = || {
        black_box(sandbox::save_settings(
            &bound,
            black_box(settings),
            black_box(3),
        ))
        .unwrap();
    };
    let mut overhead = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let plain = batch(&mut direct_call);
        overhead.push(batch(&mut sdk_call).saturating_sub(plain));
    }
    overhead.sort_unstable();
    let p95 = report("sandbox", &overhead);
    println!("gateway.sandbox_save_settings condition={condition} p95_ps={p95}");
    assert!(p95 <= budget, "p95 {p95} ps over {budget} ps ({condition})");
}
