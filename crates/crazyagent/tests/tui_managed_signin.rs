#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelAccess, ModelAuthentication, ModelEndpoint, resolve_config};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerClientKind, OwnerData, OwnerRead,
};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 1000\n[model_access]\naccounts = []\n";

const DRIVER: &str = r#"
if not __debug__:
    raise SystemExit('Managed sign-in checks require Python assertions')
import json
import os
from pathlib import Path
import signal
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, sys.argv[1])
import tui_project_master as fixture

binary, root = sys.argv[2], Path(sys.argv[3])
database = root / 'engine.db'
raw = bytearray()
typed = bytearray()
result = {'passed': False, 'authentication_started': False}

def write_new(name, data):
    assert len(data) <= 1048576
    fd = os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(data)

def interrupted(signum, frame):
    raise SystemExit('Managed sign-in bounded interruption %d' % signum)

def expect(text):
    fixture.tui_flow.expect(master, text, child, limit=6)

def key(data):
    assert child.poll() is None
    typed.extend(data)
    assert bytes(typed) in (b'\t\t1', b'\t\t1a', b'\t\t1a\r', b'\t\t1a\r\r')
    os.write(master, data)

def counts():
    rows = fixture.creation.read_rows(database, 'SELECT '
        "(SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'),"
        '(SELECT COUNT(*) FROM native_auth_operations),'
        '(SELECT COUNT(*) FROM master_turns),'
        '(SELECT COUNT(*) FROM model_gateway_attempts)')
    assert len(rows) == 1 and len(rows[0]) == 4
    return rows[0]

def managed_screen():
    screen = fixture.conversation.screen(master)
    for field in ('Account reference', 'Connection ID', 'Harness', 'Protocol',
                  'Endpoint', 'Credential/profile reference'):
        assert field not in screen, 'Managed setup exposed manual field: ' + field
    assert 'AuthenticatedObserved' not in screen
    assert 'Claude official sign-in' not in screen
    return screen

for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
    signal.signal(handled, interrupted)
signal.setitimer(signal.ITIMER_REAL, 60)
try:
    with fixture.terminal(binary, root / 'e.sock') as (child, master):
        raw = fixture.tui_flow.outputs[master]
        result.update(tui_pid=child.pid, supervisor_pid=child.supervisor.pid,
                      tui_group=os.getpgid(child.pid))
        key(b'\t\t1')
        expect('Accounts: 0 records on this page')
        expect('Writable: host-granted metadata sources')
        assert counts() == (0, 0, 0, 0)
        original = (root / 'global.toml').read_bytes()
        key(b'a')
        expect('Sign in to a provider')
        expect('Codex')
        managed_screen()
        assert counts() == (0, 0, 0, 0)
        assert (root / 'global.toml').read_bytes() == original
        key(b'\r')
        expect('Account label (optional)')
        managed_screen()
        assert counts() == (0, 0, 0, 0)
        assert (root / 'global.toml').read_bytes() == original
        key(b'\r')
        expect('Codex browser sign-in')
        expect('Codex device-code sign-in')
        managed_screen()
        assert counts() == (3, 0, 0, 0)
        assert (root / 'global.toml').read_bytes() != original
        assert bytes(typed) == b'\t\t1a\r\r'
        result['typed_hex'] = bytes(typed).hex()
        write_new('chooser.txt', fixture.conversation.screen(master).encode())
    result.update(tui_exit=child.poll(), supervisor_exit=child.supervisor.poll())
    assert result['tui_exit'] == 0 and result['supervisor_exit'] == 0
    try:
        os.killpg(result['tui_group'], 0)
    except ProcessLookupError:
        result['tui_group_absent'] = True
    else:
        raise AssertionError('Owned managed sign-in TUI group remains after cleanup')
    assert counts() == (3, 0, 0, 0)
    result['passed'] = True
    print('PTY PASS managed-signin1023', flush=True)
finally:
    signal.setitimer(signal.ITIMER_REAL, 0)
    result['raw_bytes'] = len(raw)
    result['raw_truncated'] = len(raw) > 1048576
    write_new('terminal.raw', bytes(raw[:1048576]))
    payload = (json.dumps(result, sort_keys=True) + '\n').encode()
    assert len(payload) <= 8192
    write_new('ui-result.json', payload)
    assert not result['raw_truncated']
"#;

fn private_file(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

struct Driver {
    child: Child,
    output: PathBuf,
    error: PathBuf,
}

impl Driver {
    fn start(root: &Path) -> Self {
        let output = root.join("driver.stdout");
        let error = root.join("driver.stderr");
        private_file(&output, b"");
        private_file(&error, b"");
        let logs = |path| fs::OpenOptions::new().append(true).open(path).unwrap();
        let child = Command::new("/usr/bin/python3")
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", root.join("home-empty"))
            .env("TERM", "xterm-256color")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .args(["-B", "-c", DRIVER])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qualification"
            ))
            .arg(env!("CARGO_BIN_EXE_crazyagent"))
            .arg(root)
            .stdin(Stdio::null())
            .stdout(logs(&output))
            .stderr(logs(&error))
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(child.id() > 1);
        Self {
            child,
            output,
            error,
        }
    }

    async fn finish(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(70);
        loop {
            for path in [&self.output, &self.error] {
                if fs::metadata(path).unwrap().len() > 1048576 {
                    return Err("Managed sign-in driver exceeded its output bound".into());
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                let output = fs::read_to_string(&self.output).unwrap();
                if status.success() && output.contains("PTY PASS managed-signin1023") {
                    assert_eq!(output.matches("owned-group=").count(), 1);
                    assert_eq!(output.matches("reaped-group=").count(), 1);
                    return Ok(());
                }
                return Err(format!("Real managed sign-in PTY failed: {status}"));
            }
            if Instant::now() >= deadline {
                return Err("Managed sign-in driver exceeded 70 seconds".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn signal_group(group: u32, signal: &str) {
    assert!(group > 1 && matches!(signal, "-TERM" | "-KILL"));
    let _ = Command::new("/bin/kill")
        .args([signal, "--", &format!("-{group}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Drop for Driver {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            signal_group(self.child.id(), "-TERM");
            let deadline = Instant::now() + Duration::from_secs(8);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            signal_group(self.child.id(), "-KILL");
            let _ = self.child.wait();
        }
        if let Ok(output) = fs::read_to_string(&self.output) {
            for group in output
                .lines()
                .filter_map(|line| line.strip_prefix("owned-group="))
            {
                if let Ok(group) = group.parse::<u32>()
                    && !output
                        .lines()
                        .any(|line| line == format!("reaped-group={group}"))
                {
                    signal_group(group, "-TERM");
                    signal_group(group, "-KILL");
                }
            }
        }
    }
}

async fn verify_catalog(root: &Path) {
    assert!(fs::metadata(root.join("global.toml")).unwrap().len() <= 1048576);
    let config = resolve_config(&root.join("global.toml"), &root.join("project.toml")).unwrap();
    let catalog = config.settings.model_access.as_ref().unwrap();
    assert_eq!(catalog.accounts.len(), 1);
    assert_eq!(catalog.connections.len(), 1);
    assert_eq!(catalog.pools.len(), 1);
    let connection = &catalog.connections[0];
    let pool = &catalog.pools[0];
    assert_eq!(connection.harness.as_str(), "codex");
    assert_eq!(connection.protocol.as_str(), "native");
    assert!(matches!(connection.endpoint, ModelEndpoint::Native {}));
    assert!(!connection.provider.as_str().is_empty());
    assert!(!connection.model.id.is_empty());
    assert!(!connection.model.author.as_str().is_empty());
    assert_eq!(catalog.accounts, vec![connection.account_ref.clone()]);
    let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        panic!("Generated connection must retain a managed native profile reference");
    };
    let identifiers: BTreeSet<_> = [&catalog.accounts[0], &connection.id, profile_ref, &pool.id]
        .into_iter()
        .collect();
    assert_eq!(identifiers.len(), 4);
    assert_eq!(pool.members, vec![connection.id.clone()]);
    assert!(!pool.fallback_allowed);
    let mut owner = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .unwrap();
    for (collection, expected) in [
        (
            ModelAccessCollection::Accounts,
            ModelAccessRecord::Account {
                account_ref: catalog.accounts[0].clone(),
            },
        ),
        (
            ModelAccessCollection::Connections,
            ModelAccessRecord::Connection {
                connection: connection.clone(),
            },
        ),
        (
            ModelAccessCollection::Pools,
            ModelAccessRecord::Pool { pool: pool.clone() },
        ),
    ] {
        let reply = owner
            .read(OwnerRead::ModelAccessPage {
                collection,
                cursor: None,
                limit: 16,
            })
            .await
            .unwrap();
        assert_eq!(reply.config_id, config.id);
        let OwnerData::ModelAccessPage {
            collection: actual,
            writable,
            items,
            next,
            ..
        } = reply.data
        else {
            panic!("Actual owner catalog page required");
        };
        assert_eq!(actual, collection);
        assert!(writable);
        assert!(next.is_none());
        assert_eq!(items, vec![expected]);
    }
    drop(owner);
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("PRAGMA query_only=ON").unwrap();
    let commits: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(commits, 3);
    let stored: String = db
        .query_row(
            "SELECT body FROM configs WHERE id=?1",
            [config.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let stored: crazyagent::config::EffectiveConfig = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored, config);
    for table in [
        "native_auth_operations",
        "master_turns",
        "model_gateway_attempts",
        "conversation_entries",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(fs::read(root.join("project.toml")).unwrap(), b"");
    assert_eq!(fs::read_dir(root.join("home-empty")).unwrap().count(), 0);
}

#[tokio::test]
async fn managed_signin_blank_label_persists_generated_catalog_before_auth_method_choice() {
    let root = tempfile::Builder::new()
        .prefix("ca-ms1023-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&root.path().join("global.toml"), SETTINGS.as_bytes());
    private_file(&root.path().join("project.toml"), b"");
    fs::create_dir(root.path().join("home-empty")).unwrap();
    fs::set_permissions(
        root.path().join("home-empty"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let (_stop, stopped) = watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &root.path().join("global.toml"),
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let mut owner =
        OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let initial = owner
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage {
        collection: ModelAccessCollection::Accounts,
        writable: true,
        items,
        next: None,
        ..
    } = initial.data
    else {
        panic!("Actual writable initial Accounts read required before terminal input");
    };
    assert!(items.is_empty());
    assert_eq!(
        resolve_config(
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
        )
        .unwrap()
        .settings
        .model_access,
        Some(ModelAccess::default())
    );
    drop(owner);
    let mut driver = Driver::start(root.path());
    let mut outcome = driver.finish().await;
    drop(driver);
    if outcome.is_ok() {
        let path = root.path().to_path_buf();
        if let Err(error) = tokio::spawn(async move { verify_catalog(&path).await }).await {
            outcome = Err(format!(
                "Real managed sign-in catalog readback failed: {error}"
            ));
        }
    }
    let cleanup = service.shutdown().await;
    if outcome.is_err() || cleanup.is_err() {
        let retained = root.keep();
        panic!(
            "Managed sign-in outcome={outcome:?}, engine cleanup={cleanup:?}; private evidence retained at {}",
            retained.display()
        );
    }
}
