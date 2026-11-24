use std::hint::black_box;
use std::time::Instant;

use rusqlite::Connection;

use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{Keychain, SecretString, Store};
use crate::paths::{Layout, Platform};
use crate::perf_kit::{Row, SAMPLES, line, line_ns, time};
use crate::protocol::Outcome;
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::sandbox::{LinuxSockets, LinuxStrict, LinuxSupervisor};
use crate::sandbox_settings::SandboxSettings;
use crate::service::Service;
use crate::storage::versioned;
use crate::update::Channel;

const LEAF: &str = "perf";
const SECRET_NAME: &str = "openai";
const PASTE: &[u8] = b"sk-stand-in-owner-paste";
const KEYCHAIN_SAMPLES: usize = 50;
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
const SANDBOX: [SandboxSettings; 2] = [
    SandboxSettings {
        linux_sockets: LinuxSockets::Refuse,
        linux_supervisor: LinuxSupervisor::Off,
        linux_strict: LinuxStrict::On,
    },
    SandboxSettings {
        linux_sockets: LinuxSockets::TcpOnly,
        linux_supervisor: LinuxSupervisor::On,
        linux_strict: LinuxStrict::Off,
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
fn data_root() -> (tempfile::TempDir, Layout) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap().join("data");
    crate::paths::private_folder(&root).unwrap();
    (folder, Layout::new(root).unwrap())
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

/// An in-memory store with every table the engine opens, so each statement meets the schema
/// size a running engine has. Output: the connection.
fn engine_store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    db
}

/// Times the engine's quit save of an owner command: one write of the quit settings row
/// in the request's transaction on an in-memory store with the engine's full schema, committed
/// after each call.
fn settings_save_quit() {
    let mut db = engine_store();
    let mut saved = 0_usize;
    let mut step = || {
        let tx = db.transaction().unwrap();
        let settings = QUIT[saved % QUIT.len()];
        saved += 1;
        let (outcome, ns) = timed(|| crate::quit_settings::save(&tx, &settings));
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

/// Times the engine's sandbox settings save of an owner command at the version just read,
/// committed after each call, and the owner read, on an in-memory store with the engine's
/// full schema.
fn sandbox_settings() {
    let mut db = engine_store();
    let mut saved = 0_usize;
    let mut step = || {
        let tx = db.transaction().unwrap();
        let version = versioned::current(&tx, &versioned::sandbox_settings_item())
            .unwrap()
            .version;
        let settings = SANDBOX[saved % SANDBOX.len()];
        saved += 1;
        let (outcome, ns) = timed(|| crate::sandbox_settings::save_from(&tx, &settings, version));
        assert!(matches!(outcome, Ok(Outcome::Applied)));
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
    assert_eq!(
        crate::sandbox_settings::load(&db).unwrap(),
        SANDBOX[(saved - 1) % SANDBOX.len()]
    );
    time(
        &row("provider.sandbox.settings", READ_BATCH),
        SAMPLES,
        || {
            let _ = black_box(crate::sandbox_settings::read(&db));
        },
    );
}

/// Times the credential store calls the engine made for a saved secret: put, get and
/// delete on a scratch keychain file in a temporary home, never the owner's.
fn secrets_calls() {
    let keychain = TestKeychain::temporary();
    let namespace = namespace();
    let store = Keychain::at(&namespace, keychain.path()).unwrap();
    let service = format!("{namespace}.{LEAF}");
    let mut put = Vec::with_capacity(KEYCHAIN_SAMPLES);
    let mut get = Vec::with_capacity(KEYCHAIN_SAMPLES);
    let mut delete = Vec::with_capacity(KEYCHAIN_SAMPLES);
    for index in 0..KEYCHAIN_SAMPLES + KEYCHAIN_SAMPLES / 10 {
        let secret = SecretString::new(PASTE.to_vec()).unwrap();
        let (saved, put_ns) = timed(|| store.put(&service, SECRET_NAME, &secret));
        let saved = saved.unwrap();
        let (found, get_ns) = timed(|| store.get(&saved));
        assert_eq!(found.unwrap().expose().as_bytes(), PASTE);
        let (gone, delete_ns) = timed(|| store.delete(&saved));
        gone.unwrap();
        if index >= KEYCHAIN_SAMPLES / 10 {
            put.push(put_ns);
            get.push(get_ns);
            delete.push(delete_ns);
        }
    }
    print(&row("provider.secrets.put_from_owner", 1), put);
    print(&row("provider.secrets.find", 1), get);
    print(&row("provider.secrets.forget", 1), delete);
}

/// Times the login service status and the update channel save on a private temporary
/// data folder, as the CLI runs them.
fn service_and_update() {
    let (_guard, layout) = data_root();
    let executable = std::env::current_exe().unwrap();
    let status = Service::current_user(Platform::CURRENT, executable, &layout)
        .unwrap()
        .status()
        .unwrap();
    assert!(!status.installed && !status.running);
    time(&row("provider.service.status", 1), SAMPLES, || {
        let executable = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .unwrap();
        let service = Service::current_user(Platform::CURRENT, executable, &layout);
        let _ = black_box(service.map(|service| service.status()));
    });
    let channels = [Channel::Dev, Channel::Stable];
    crate::update::save_channel(layout.root(), channels[0]).unwrap();
    let mut set = 0_usize;
    time(&row("provider.update.set_channel", 1), SAMPLES, || {
        set += 1;
        let _ = black_box(crate::update::save_channel(
            layout.root(),
            channels[set % channels.len()],
        ));
    });
}

/// Times the 2.4.4 job of each SDK capability call the 2.5 provider rows time on this
/// Mac: the engine's own call for the same work, with no plugin, gateway or SDK between.
/// net.request has no 2.4.4 job: 2.4.4 sent HTTP from each feature with no egress check.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn sdk_provider_calls() {
    settings_save_quit();
    sandbox_settings();
    secrets_calls();
    service_and_update();
}
