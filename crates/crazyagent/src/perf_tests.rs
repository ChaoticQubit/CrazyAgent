use std::hint::black_box;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use bus_gateway::{AllOn, Gateway, Grants, Refusals};
use kernel_security::error::StoreError;
use kernel_security::net::capture::CaptureRules;
use kernel_security::net::{Egress, RawReply, Requests, Sealed};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store};
use sdk::generated::{net, sandbox, secrets, service, update};
use sdk::{Capability, Refusal, Tier};
use sdk_host::{Bound, Providers};

use crate::keychain_support::TemporaryKeychain;
use crate::perf_kit::{Row, SAMPLES, line, line_ns, time};
use crate::protocol::Outcome;
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::sdk_processes::{ServiceProvider, UpdateProvider};
use crate::sdk_sandbox::SandboxProvider;
use crate::security_caps::{Absent, NetCaps, SecretsCaps};
use crate::settings_log::tests::moved;

const PLUGIN: &str = "perf";
const LEAF: &str = "perf";
const PERMISSIONS: [&str; 8] = [
    "net.request",
    "sandbox.settings_read",
    "sandbox.settings_write",
    "secrets.find",
    "secrets.forget",
    "secrets.put",
    "service.read",
    "update.write",
];
const SECRET_NAME: &str = "openai";
const HOST: &str = "api.github.com";
const URL: &str = "https://api.github.com/user";
const PASTE: &[u8] = b"sk-stand-in-owner-paste";
const BEARER: &[u8] = b"gho_stand-in-held-token";
const KEYCHAIN_SAMPLES: usize = 50;
const NET_BATCH: usize = 10;
const READ_BATCH: usize = 100;
const QUIT: [QuitSettings; 2] = [
    QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    },
    QuitSettings {
        on_quit: OnQuit::KeepRunning,
        left_running: LeftRunning::AfterHours,
        hours: 8,
    },
];
const SANDBOX: [sandbox::SandboxSettings; 2] = [
    sandbox::SandboxSettings {
        linux_sockets: sandbox::LinuxSockets::Refuse,
        linux_supervisor: sandbox::LinuxSupervisor::Off,
        linux_strict: sandbox::LinuxStrict::On,
    },
    sandbox::SandboxSettings {
        linux_sockets: sandbox::LinuxSockets::TcpOnly,
        linux_supervisor: sandbox::LinuxSupervisor::On,
        linux_strict: sandbox::LinuxStrict::Off,
    },
];

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "sdk",
        name,
        batch,
    }
}

/// A refusal sink the rows never reach: the bound plugin holds every grant it calls.
struct Unreached;

impl Refusals for Unreached {
    fn refused(&self, plugin: &'static str, capability: &Capability, _: &Refusal) {
        panic!("perf row refused: {plugin} {}", capability.name);
    }
}

/// A store the net row never opens: a held bearer needs no store call.
struct Unopened;

impl Store for Unopened {
    fn put(&self, _: &str, _: &str, _: &SecretString) -> Result<SecretRef, StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn get(&self, _: &SecretRef) -> Result<SecretString, StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn delete(&self, _: &SecretRef) -> Result<(), StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("a held bearer needs no store call")
    }
}

/// Egress that allows every origin, resolves to one public address and answers at once,
/// so the net row holds the kernel request path and no network.
struct Immediate;

impl Egress for Immediate {
    fn allows(&self, _: &'static str, _: &str, _: &str, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, StoreError> {
        Ok(vec![IpAddr::from([140, 82, 112, 4])])
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        black_box(sealed.config().len());
        Ok(RawReply {
            status: 200,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        })
    }
}

/// The providers of one row: the given ones, every other namespace absent.
fn providers<'p>(
    net: &'p dyn net::Provide,
    sandbox: &'p dyn sandbox::Provide,
    secrets: &'p dyn secrets::Provide,
    service: &'p dyn service::Provide,
    update: &'p dyn update::Provide,
) -> Providers<'p> {
    Providers {
        editor: &Absent,
        net,
        process: &Absent,
        project_rows: &Absent,
        sandbox,
        secrets,
        service,
        settings: &Absent,
        update,
    }
}

/// Runs one call and the time it took. Input: the call. Output: its result and the ns.
fn timed<T>(call: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let found = black_box(call());
    let ns = u64::try_from(started.elapsed().as_nanos()).expect("sample fits u64");
    (found, ns)
}

/// Prints the PERF line of one row from unsorted per-call samples. Inputs: the row and
/// the samples.
fn print(row: &Row, mut samples: Vec<u64>) {
    samples.sort_unstable();
    println!("{}", line(row, &samples));
}

/// Prints the PERF line of one row in nanoseconds at any median, for a row whose builds
/// fall on either side of 10 us. Inputs: the row and the unsorted per-call samples.
fn print_ns(row: &Row, mut samples: Vec<u64>) {
    samples.sort_unstable();
    println!("{}", line_ns(row, &samples));
}

/// A data folder made private as the engine makes it. Output: the guard and its layout.
fn data_root() -> (tempfile::TempDir, crate::paths::Layout) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap().join("data");
    crate::paths::private_folder(&root).unwrap();
    (folder, crate::paths::Layout::new(root).unwrap())
}

/// A store namespace for this run only, so a keychain item never meets the owner's.
fn namespace() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mixed = u64::from(std::process::id()) ^ u64::try_from(nanos & 0xffff_ffff_ffff).unwrap();
    format!("dev.crazyagent.e{:012x}", mixed & 0xffff_ffff_ffff)
}

/// An in-memory store with every table the engine opens and the test host's owner tables,
/// so each statement meets the schema size a running engine has. Output: the connection.
fn engine_store() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::state_log::fence::seed(&db);
    db
}

/// Times the quit save the engine runs: the bundled agent-quit plugin through the
/// generated host, the gateway and the settings provider, writing one event in the
/// request's transaction on an in-memory store with the engine's full schema, committed after
/// each call.
fn settings_save_quit() {
    let mut db = engine_store();
    let mut saved = 0_usize;
    let mut step = || {
        let tx = db.transaction().unwrap();
        let revision = u64::try_from(moved(&tx).0).unwrap();
        let settings = QUIT[saved % QUIT.len()];
        saved += 1;
        let (outcome, ns) = timed(|| crate::sdk_slice::save_quit(&tx, revision, &settings));
        assert!(matches!(outcome, Ok(Outcome::Applied)));
        tx.commit().unwrap();
        ns
    };
    for _ in 0..SAMPLES / 10 {
        step();
    }
    print_ns(
        &row("provider.settings.save_quit", 1),
        (0..SAMPLES).map(|_| step()).collect(),
    );
}

/// Times the two sandbox settings calls through the generated host and the gateway to
/// the engine's provider on an in-memory store with the engine's full schema: a save at the
/// version just read, committed after each call, and a read.
fn sandbox_settings(grants: &Grants) {
    let mut db = engine_store();
    let mut saved = 0_usize;
    let mut step = || {
        let tx = db.transaction().unwrap();
        let settings = SANDBOX[saved % SANDBOX.len()];
        saved += 1;
        let ns = {
            let provider = SandboxProvider {
                tx: &tx,
                revision: u64::try_from(moved(&tx).0).unwrap(),
            };
            let all = providers(&Absent, &provider, &Absent, &Absent, &Absent);
            let gateway = Gateway::new(grants, &Unreached, &AllOn);
            let host = Bound::new(gateway.bind(PLUGIN), &all);
            let version = sandbox::settings(&host).unwrap().version;
            let (outcome, ns) = timed(|| sandbox::save_settings(&host, settings, version));
            assert_eq!(outcome, Ok(()));
            ns
        };
        tx.commit().unwrap();
        ns
    };
    for _ in 0..SAMPLES / 10 {
        step();
    }
    print(
        &row("provider.sandbox.save_settings", 1),
        (0..SAMPLES).map(|_| step()).collect(),
    );
    let tx = db.transaction().unwrap();
    let provider = SandboxProvider {
        tx: &tx,
        revision: u64::try_from(moved(&tx).0).unwrap(),
    };
    let all = providers(&Absent, &provider, &Absent, &Absent, &Absent);
    let gateway = Gateway::new(grants, &Unreached, &AllOn);
    let host = Bound::new(gateway.bind(PLUGIN), &all);
    let stored = sandbox::settings(&host).unwrap();
    assert_eq!(stored.settings, SANDBOX[(saved - 1) % SANDBOX.len()]);
    time(
        &row("provider.sandbox.settings", READ_BATCH),
        SAMPLES,
        || {
            let _ = black_box(sandbox::settings(&host));
        },
    );
}

/// Times the three secrets calls through the generated host and the gateway to the
/// engine's provider on a scratch keychain file in a temporary home, never the owner's:
/// a save of a stand-in owner paste, a find of that name and a forget of its handle.
fn secrets_calls(grants: &Grants) {
    let keychain = TemporaryKeychain::create();
    let namespace = namespace();
    let store = kernel_security::store::macos::Keychain::at(&namespace, keychain.path()).unwrap();
    let scopes = Scopes::new(&[(PLUGIN, LEAF)]).unwrap();
    let handles = Mutex::new(Handles::new());
    let mut put = Vec::with_capacity(KEYCHAIN_SAMPLES);
    let mut find = Vec::with_capacity(KEYCHAIN_SAMPLES);
    let mut forget = Vec::with_capacity(KEYCHAIN_SAMPLES);
    for index in 0..KEYCHAIN_SAMPLES + KEYCHAIN_SAMPLES / 10 {
        let entry = OwnerEntry {
            plugin: PLUGIN,
            hosts: vec![HOST.to_owned()],
            value: SecretString::new(PASTE.to_vec()).unwrap(),
        };
        let kernel = Secrets::for_request(&store, &namespace, &scopes, &handles, Some(entry));
        let caps = SecretsCaps(kernel);
        let all = providers(&Absent, &Absent, &caps, &Absent, &Absent);
        let gateway = Gateway::new(grants, &Unreached, &AllOn);
        let host = Bound::new(gateway.bind(PLUGIN), &all);
        let (saved, put_ns) = timed(|| secrets::put_from_owner(&host, SECRET_NAME.to_owned()));
        let saved = saved.unwrap_or_else(|_| panic!("put_from_owner refused"));
        let (found, find_ns) = timed(|| secrets::find(&host, SECRET_NAME.to_owned()));
        assert!(found == Ok(Some(saved)));
        let (forgot, forget_ns) = timed(|| secrets::forget(&host, saved));
        assert_eq!(forgot, Ok(()));
        if index >= KEYCHAIN_SAMPLES / 10 {
            put.push(put_ns);
            find.push(find_ns);
            forget.push(forget_ns);
        }
    }
    print(&row("provider.secrets.put_from_owner", 1), put);
    print(&row("provider.secrets.find", 1), find);
    print(&row("provider.secrets.forget", 1), forget);
}

/// Times net.request through the generated host and the gateway to the engine's
/// provider and the kernel request path, with a held bearer and egress that answers at
/// once, so nothing goes on the network.
fn net_request(grants: &Grants) {
    let namespace = namespace();
    let scopes = Scopes::new(&[(PLUGIN, LEAF)]).unwrap();
    let handles = Mutex::new(Handles::new());
    let kernel = Secrets::for_request(&Unopened, &namespace, &scopes, &handles, None);
    let bearer = kernel
        .hold(
            PLUGIN,
            SecretString::bearer(BEARER.to_vec()).unwrap(),
            &[HOST.to_owned()],
        )
        .unwrap();
    let rules = CaptureRules::new(&[]).unwrap();
    let caps = NetCaps(Requests::new(&kernel, &Immediate, &rules));
    let all = providers(&caps, &Absent, &Absent, &Absent, &Absent);
    let gateway = Gateway::new(grants, &Unreached, &AllOn);
    let host = Bound::new(gateway.bind(PLUGIN), &all);
    let request = || net::Request {
        method: net::Method::Get,
        url: URL.to_owned(),
        headers: vec![net::Field {
            name: "accept".to_owned(),
            value: "application/json".to_owned(),
        }],
        bearer: Some(secrets::Secret::from_kernel(bearer)),
        form: Vec::new(),
        secret_form: Vec::new(),
        json: None,
        reply_headers: Vec::new(),
        capture: Vec::new(),
    };
    let reply = net::request(&host, request()).unwrap_or_else(|_| panic!("request refused"));
    assert_eq!(
        (reply.status, reply.body.as_slice()),
        (200, b"{}".as_slice())
    );
    time(&row("provider.net.request", NET_BATCH), SAMPLES, || {
        let _ = black_box(net::request(&host, request()));
    });
}

/// Times service.status and update.set_channel through the generated host and the
/// gateway to the engine's providers on a private temporary data folder.
fn service_and_update(grants: &Grants) {
    let (_guard, layout) = data_root();
    let service = ServiceProvider { layout: &layout };
    let update = UpdateProvider { layout: &layout };
    let all = providers(&Absent, &Absent, &Absent, &service, &update);
    let gateway = Gateway::new(grants, &Unreached, &AllOn);
    let host = Bound::new(gateway.bind(PLUGIN), &all);
    let status = service::status(&host).unwrap();
    assert!(!status.installed && !status.running);
    time(&row("provider.service.status", 1), SAMPLES, || {
        let _ = black_box(service::status(&host));
    });
    let channels = [update::UpdateChannel::Dev, update::UpdateChannel::Stable];
    assert_eq!(update::set_channel(&host, channels[0]), Ok(()));
    let mut set = 0_usize;
    time(&row("provider.update.set_channel", 1), SAMPLES, || {
        set += 1;
        let _ = black_box(update::set_channel(&host, channels[set % channels.len()]));
    });
}

/// Times the SDK capability calls whose engine provider runs on this Mac with no side
/// effect on the owner: each through the generated host and the gateway to the real
/// provider, on an in-memory store, a scratch keychain, egress that answers at once or a
/// temporary data folder. Each answer is checked once before its row is timed.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn sdk_provider_calls() {
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &PERMISSIONS)]).unwrap();
    settings_save_quit();
    sandbox_settings(&grants);
    secrets_calls(&grants);
    net_request(&grants);
    service_and_update(&grants);
}
