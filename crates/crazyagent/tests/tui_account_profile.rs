use crazyagent::client_controller::OwnerController;
use crazyagent::config::{
    EffectiveConfig, ModelAccess, ModelAuthentication, ModelEndpoint, resolve_config,
};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    AccountProfileReceipt, AccountProfileRequest, ModelAccessCollection, ModelAccessRecord,
    OwnerClientKind, OwnerData, OwnerRead,
};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 1000\n[model_access]\naccounts = []\n";
const MODEL: &str = "local-model-acceptance";

const DRIVER: &str = r#"
if not __debug__:
    raise SystemExit('Account profile checks require Python assertions')
import json
import os
from pathlib import Path
import signal
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, sys.argv[1])
import tui_project_master as fixture

binary, root = sys.argv[2], Path(sys.argv[3])
endpoint, model = sys.argv[4:6]
database = root / 'engine.db'
raw = bytearray()
typed = bytearray()
steps = [b'\t\t1', b'a', b'\x1b[B' * 3, b'\r', b'Local acceptance', b'\r',
         endpoint.encode(), b'\r', model.encode(), b'\r', b'3', b'2', b'\r', b'k', b'\r']
step = 0
result = {'passed': False, 'authentication_started': False}

def write_new(name, data):
    assert len(data) <= 1048576
    fd = os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(data)

def interrupted(signum, frame):
    raise SystemExit('Account profile bounded interruption %d' % signum)

def expect(text):
    fixture.tui_flow.expect(master, text, child, limit=6)

def key(data):
    global step
    assert child.poll() is None
    assert step < len(steps) and data == steps[step], 'Unexpected setup keyboard step'
    step += 1
    typed.extend(data)
    os.write(master, data)

def counts():
    rows = fixture.creation.read_rows(database, 'SELECT '
        "(SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'),"
        '(SELECT COUNT(*) FROM native_auth_operations),'
        '(SELECT COUNT(*) FROM master_turns),'
        '(SELECT COUNT(*) FROM model_gateway_attempts),'
        '(SELECT COUNT(*) FROM projects)')
    assert len(rows) == 1 and len(rows[0]) == 5
    return rows[0]

def ordinary_screen():
    screen = fixture.conversation.screen(master)
    for field in ('Account reference', 'Connection ID', 'Credential/profile reference'):
        assert field not in screen, 'Ordinary setup exposed an internal routing field'
    assert 'AuthenticatedObserved' not in screen
    assert 'Ready' not in screen
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
        assert counts() == (0, 0, 0, 0, 0)
        original = (root / 'global.toml').read_bytes()
        key(b'a')
        expect('Sign in to a provider')
        expect('API key or compatible endpoint')
        expect('Local endpoint')
        ordinary_screen()
        key(b'\x1b[B' * 3)
        expect('> Local endpoint')
        key(b'\r')
        expect('Account label (optional)')
        key(b'Local acceptance')
        key(b'\r')
        expect('Endpoint URL')
        ordinary_screen()
        key(endpoint.encode())
        key(b'\r')
        expect('Enter saves account setup')
        ordinary_screen()
        assert counts() == (0, 0, 0, 0, 0)
        assert (root / 'global.toml').read_bytes() == original
        key(model.encode())
        key(b'\r')
        expect('Connections: 1 records on this page')
        assert counts() == (3, 0, 0, 0, 0)
        saved = (root / 'global.toml').read_bytes()
        assert saved != original
        key(b'3')
        expect('Pools: 1 records on this page')
        expect('Unassigned')
        key(b'2')
        expect('Connections: 1 records on this page')
        key(b'\r')
        expect('Provider: openai-compatible')
        expect('Auth method/reference: None')
        key(b'k')
        expect('Private profile setup')
        expect('No API key')
        expect(endpoint)
        expect(model)
        screen = ordinary_screen()
        assert 'API key (hidden)' not in screen
        key(b'\r')
        expect('ConfiguredOnly')
        ordinary_screen()
        assert step == len(steps) and bytes(typed) == b''.join(steps)
        assert counts() == (3, 0, 0, 0, 0)
        assert (root / 'global.toml').read_bytes() == saved
        result['typed_hex'] = bytes(typed).hex()
    result.update(tui_exit=child.poll(), supervisor_exit=child.supervisor.poll())
    assert result['tui_exit'] == 0 and result['supervisor_exit'] == 0
    try:
        os.killpg(result['tui_group'], 0)
    except ProcessLookupError:
        result['tui_group_absent'] = True
    else:
        raise AssertionError('Owned account profile TUI group remains after cleanup')
    assert counts() == (3, 0, 0, 0, 0)
    result['passed'] = True
    print('PTY PASS local-account-profile', flush=True)
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
    fn start(root: &Path, endpoint: &str) -> Self {
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
            .arg(endpoint)
            .arg(MODEL)
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
                    return Err("Account profile driver exceeded its output bound".into());
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                let output = fs::read_to_string(&self.output).unwrap();
                if status.success() && output.contains("PTY PASS local-account-profile") {
                    assert_eq!(output.matches("owned-group=").count(), 1);
                    assert_eq!(output.matches("reaped-group=").count(), 1);
                    return Ok(());
                }
                return Err(format!("Real account profile PTY failed: {status}"));
            }
            if Instant::now() >= deadline {
                return Err("Account profile driver exceeded 70 seconds".into());
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

fn catalog(root: &Path, endpoint: &str) -> EffectiveConfig {
    assert!(fs::metadata(root.join("global.toml")).unwrap().len() <= 1048576);
    let config = resolve_config(&root.join("global.toml"), &root.join("project.toml")).unwrap();
    let access = config.settings.model_access.as_ref().unwrap();
    assert_eq!(access.accounts.len(), 1);
    assert_eq!(access.connections.len(), 1);
    assert_eq!(access.pools.len(), 1);
    let connection = &access.connections[0];
    let pool = &access.pools[0];
    assert_eq!(connection.provider.as_str(), "openai-compatible");
    assert_eq!(connection.harness.as_str(), "rcli");
    assert_eq!(connection.protocol.as_str(), "openai-chat");
    assert_eq!(connection.model.id, MODEL);
    assert_eq!(connection.model.author, connection.provider);
    assert_eq!(
        connection.endpoint,
        ModelEndpoint::Http {
            url: endpoint.into()
        }
    );
    assert!(matches!(connection.auth, ModelAuthentication::None {}));
    assert_eq!(access.accounts, vec![connection.account_ref.clone()]);
    let identifiers: BTreeSet<_> = [&access.accounts[0], &connection.id, &pool.id]
        .into_iter()
        .collect();
    assert_eq!(identifiers.len(), 3);
    assert_eq!(pool.members, vec![connection.id.clone()]);
    assert!(pool.projects.is_empty());
    assert!(pool.roles.is_empty());
    assert!(!pool.fallback_allowed);
    assert_eq!(fs::read(root.join("project.toml")).unwrap(), b"");
    assert_eq!(fs::read_dir(root.join("home-empty")).unwrap().count(), 0);
    config
}

async fn owner_catalog(root: &Path, config: &EffectiveConfig) -> (String, String, String) {
    let access = config.settings.model_access.as_ref().unwrap();
    let mut owner =
        OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let mut observed_source = None;
    for (collection, expected) in [
        (
            ModelAccessCollection::Accounts,
            ModelAccessRecord::Account {
                account_ref: access.accounts[0].clone(),
            },
        ),
        (
            ModelAccessCollection::Connections,
            ModelAccessRecord::Connection {
                connection: access.connections[0].clone(),
            },
        ),
        (
            ModelAccessCollection::Pools,
            ModelAccessRecord::Pool {
                pool: access.pools[0].clone(),
            },
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
            source_hash,
            writable,
            items,
            next,
        } = reply.data
        else {
            panic!("Actual owner catalog page required");
        };
        assert_eq!(actual, collection);
        assert!(writable);
        assert!(next.is_none());
        assert_eq!(items, vec![expected]);
        if let Some(expected) = &observed_source {
            assert_eq!(&source_hash, expected);
        }
        observed_source = Some(source_hash);
    }
    (
        owner.identity().database_instance().to_owned(),
        owner.identity().engine_epoch().to_owned(),
        observed_source.unwrap(),
    )
}

fn profile_file(root: &Path, receipt: &AccountProfileReceipt, endpoint: &str) {
    let home = root
        .join("configured-sdk-homes")
        .join(format!(
            "{:x}",
            Sha256::digest(receipt.profile_ref.as_str().as_bytes())
        ));
    let path = home.join(".backboard/config.json");
    assert_eq!(fs::metadata(&home).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let bytes = fs::read(path).unwrap();
    assert!(!bytes.is_empty() && bytes.len() <= 65536);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        receipt.profile_config_sha256
    );
    let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({
            "model": { "provider": receipt.connection.provider.as_str(), "model": MODEL },
            "providers": [{
                "id": receipt.connection.provider.as_str(), "name": "Configured provider",
                "protocol": receipt.connection.protocol.as_str(), "baseUrl": endpoint,
                "auth": { "type": "none" }
            }]
        })
    );
}

fn profile_record(
    db: &Connection,
    config: &EffectiveConfig,
    identity: &(String, String, String),
) -> AccountProfileReceipt {
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM account_profile_operations",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let (owner, id, epoch, engine, request, phase, receipt): (
        String,
        String,
        i64,
        String,
        Vec<u8>,
        String,
        Vec<u8>,
    ) = db
        .query_row(
            "SELECT owner_id,request_id,owner_epoch,engine_epoch,request,phase,receipt \
             FROM account_profile_operations",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert!(!owner.is_empty() && epoch > 0);
    assert_eq!(engine, identity.1);
    assert_eq!(phase, "committed");
    assert!(request.len() <= 8192 && receipt.len() <= 65536);
    let request: AccountProfileRequest = serde_json::from_slice(&request).unwrap();
    let receipt: AccountProfileReceipt = serde_json::from_slice(&receipt).unwrap();
    let connection = &config.settings.model_access.as_ref().unwrap().connections[0];
    assert_eq!(request.request_id.as_str(), id);
    assert_eq!(request.scope.expected_database_instance, identity.0);
    assert_eq!(request.scope.expected_config_id, config.id);
    assert_eq!(request.scope.expected_source_hash, identity.2);
    assert_eq!(request.scope.connection, connection.id);
    assert_eq!(receipt.config_id, config.id);
    assert_eq!(receipt.source_hash, identity.2);
    assert_eq!(&receipt.connection, connection);
    assert_eq!(
        receipt.profile_ref.as_str(),
        format!(
            "configured-{:x}",
            Sha256::digest(serde_json::to_vec(connection).unwrap())
        )
    );
    assert!(!receipt.custody_id.is_empty() && receipt.profile_generation > 0);
    receipt
}

async fn verify(root: &Path, endpoint: &str) {
    let config = catalog(root, endpoint);
    let identity = owner_catalog(root, &config).await;
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("PRAGMA query_only=ON").unwrap();
    let receipt = profile_record(&db, &config, &identity);
    profile_file(root, &receipt, endpoint);
    let stored: String = db
        .query_row(
            "SELECT body FROM configs WHERE id=?1",
            [config.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<EffectiveConfig>(&stored).unwrap(),
        config
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    for table in [
        "projects",
        "native_auth_operations",
        "master_turns",
        "model_gateway_attempts",
        "conversation_entries",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn local_none_account_before_projects_configures_private_profile_and_unassigned_pool() {
    let root = tempfile::Builder::new()
        .prefix("ca-tap-")
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
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
    let mut driver = Driver::start(root.path(), &endpoint);
    let mut outcome = driver.finish().await;
    drop(driver);
    if outcome.is_ok() {
        let path = root.path().to_path_buf();
        let selected = endpoint.clone();
        if let Err(error) = tokio::spawn(async move { verify(&path, &selected).await }).await {
            outcome = Err(format!("Real account profile readback failed: {error}"));
        }
    }
    let cleanup = service.shutdown().await;
    let passive = listener.accept();
    let untouched = passive
        .as_ref()
        .err()
        .is_some_and(|error| error.kind() == ErrorKind::WouldBlock);
    if outcome.is_err() || cleanup.is_err() || !untouched {
        let retained = root.keep();
        panic!(
            concat!(
                "Account profile outcome={:?}, engine cleanup={:?}, endpoint untouched={}; ",
                "private evidence retained at {}"
            ),
            outcome,
            cleanup,
            untouched,
            retained.display()
        );
    }
}
