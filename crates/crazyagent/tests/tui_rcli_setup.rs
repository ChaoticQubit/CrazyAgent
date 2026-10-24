#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerRequest, OwnerResponse, OwnerResult,
};
use crazyagent::master_session::{MasterSessionPhase, RcliProfileRegistrationMetadata};
use crazyagent::protocol::{Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, watch};

static UI_SLOT: Mutex<()> = Mutex::const_new(());

const CATALOG: &str = r#"
version = 1
timeout_ms = 1000
allowed_destinations = ["sandbox953"]
[routing]
approved_accounts = ["api953", "local953"]
global_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
project_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
task_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
[model_access]
accounts = ["api953", "local953"]
[[model_access.connections]]
id = "a-api953"
provider = "api-provider953"
harness = "rcli"
protocol = "openai-chat"
account_ref = "api953"
endpoint = { kind="http", url="https://api953.example.invalid/v1" }
model = { id="api-model953", author="owner953" }
auth = { method="api_key", credential_ref="key-reference953" }
[[model_access.connections]]
id = "b-local953"
provider = "local-provider953"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local953"
endpoint = { kind="http", url="http://127.0.0.1:9/v1" }
model = { id="local-model953", author="owner953" }
auth = { method="none" }
[[model_access.pools]]
id = "pool953"
members = ["b-local953"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
"#;

const DRIVER: &str = r#"
if not __debug__:
    raise SystemExit('RCLI UI checks require Python assertions')
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import sys
import time
sys.dont_write_bytecode = True
sys.path.insert(0, sys.argv[1])
import tui_project_master as fixture

binary, root, mode = sys.argv[2], Path(sys.argv[3]), sys.argv[4]
assert mode in ('intent', 'registered', 'reconcile')
database = root / 'engine.db'
draft = 'Retained owner draft 953 日本'
raw = bytearray()
result = {'passed': False, 'mode': mode, 'sign_in_qualified': False}

def write_new(name, data):
    assert len(data) <= 1048576
    fd = os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(data)

def interrupted(signum, frame):
    raise SystemExit('RCLI UI bounded interruption %d' % signum)

def screen():
    return fixture.conversation.screen(master)

def expect(text):
    fixture.tui_flow.expect(master, text, child, limit=6)

def key(data):
    assert child.poll() is None
    os.write(master, data)

def counts():
    rows = fixture.creation.read_rows(database, 'SELECT '
        '(SELECT COUNT(*) FROM configured_master_profiles),'
        '(SELECT COUNT(*) FROM native_auth_operations),'
        '(SELECT COUNT(*) FROM master_turns),'
        '(SELECT COUNT(*) FROM model_gateway_attempts)')
    assert len(rows) == 1 and len(rows[0]) == 4
    return rows[0]

def settings():
    key(b'\t\t2')
    expect('Connections:')
    expect("Connections: 2 records on this page")
    key(b'\r')
    expect('Account: api953 | Model: api-model953')

def home_editor(path, retained=False):
    key(b'h')
    expect('SDK home')
    if retained:
        expect(str(path))
    else:
        key(str(path).encode())
    assert counts() == (0, 0, 0, 0)

def retain_home(path):
    home_editor(path)
    key(b'\r')
    expect('Home selected')
    assert counts() == (0, 0, 0, 0)
    assert 'Registered configured profile' not in screen()

def local_connection():
    expect('Connections:')
    key(b'\x1b[B\r')
    expect('Account: local953 | Model: local-model953')

def conversation():
    expect('Model-access fields are above; conversation draft retained')
    key(b'\x1b[Z')
    expect('Projects | n: New project')
    key(b'\x1b[Z')
    expect('Project owner conversation')
    fixture.conversation.draft_contains(master, child, draft)

def opened(count):
    fixture.setup(master, child, 'RCLI')
    expect('pool953')
    expect('local953')
    expect('sandbox953')
    expect('Enter Open | Tab/Shift-Tab fields')
    key(b'\r')
    session = fixture.wait_session(database, child, master, count, 'rcli', 'configured')
    assert session['selection']['route']['account_ref'] == 'local953'
    assert session['selection']['target_profile_ref'] == 'sandbox953'
    assert session['current_turn'] is None
    assert counts() == (0, 0, 0, 0)
    fixture.conversation.draft_contains(master, child, draft)
    return session

def confirmation(session):
    fixture.setup(master, child, 'RCLI')
    expect('Owner catalog revision')
    expect('h Register SDK home')
    key(b'h')
    expect('Register configured SDK home')
    expect(str(root / 'home-local'))
    expect(session['session_id'])
    expect('b-local953')
    expect('sandbox953')
    assert counts() == (0, 0, 0, 0)

def registered():
    expect('Registered configured profile')
    assert counts() == (1, 0, 0, 0)
    assert 'Ready' not in screen() and 'AuthenticatedObserved' not in screen()

for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
    signal.signal(handled, interrupted)
signal.setitimer(signal.ITIMER_REAL, 60)
try:
    with fixture.terminal(binary, root / 'e.sock') as (child, master):
        raw = fixture.tui_flow.outputs[master]
        result.update(tui_pid=child.pid, supervisor_pid=child.supervisor.pid,
                      tui_group=os.getpgid(child.pid))
        fixture.conversation.project_input(master, child)
        key(draft.encode())
        fixture.conversation.draft_contains(master, child, draft)
        settings()
        if mode == 'intent':
            home_editor(root / 'home-api')
            key(b'\x1b')
            expect('SDK home input retained')
            home_editor(root / 'home-api', retained=True)
            key(b'\r')
            expect('Home selected')
            local_connection()
            home_editor(root / 'home-local')
            assert str(root / 'home-api') not in screen()
            key(b'\x1b')
            expect('SDK home input retained')
            home_editor(root / 'home-local', retained=True)
            key(b'\r')
            expect('Home selected')
            conversation()
            assert counts() == (0, 0, 0, 0)
            assert fixture.sessions(database) == []
        else:
            local_connection()
            retain_home(root / 'home-local')
            conversation()
            session = opened(1)
            if mode == 'reconcile':
                key(fixture.F4)
                fixture.wait_session(database, child, master, 1, 'rcli', 'closed')
                fixture.setup(master, child, 'RCLI')
                expect('Owner catalog revision')
                key(b'h')
                expect('SDK home registration disabled')
                assert counts() == (0, 0, 0, 0)
                fixture.close_setup(master, child, draft)
                session = opened(2)
            confirmation(session)
            if mode == 'registered':
                key(b'\r')
                registered()
                fixture.conversation.draft_contains(master, child, draft)
                result['session_id'] = session['session_id']
            else:
                with sqlite3.connect(str(database), timeout=0.25) as blocker:
                    blocker.execute('BEGIN IMMEDIATE')
                    key(b'\r')
                    expect('Owner outcome unknown')
                    pending = re.search(r'Owner operation\s+([0-9a-f]{64})\s+unresolved', screen())
                    assert pending is not None, 'Original pending registration identity absent'
                    result['operation_id'] = pending.group(1)
                    assert counts() == (0, 0, 0, 0)
                    key(b'\x1b')
                    fixture.conversation.project_input(master, child)
                    fixture.conversation.draft_contains(master, child, draft)
                    key(b'-newer953')
                    fixture.conversation.draft_contains(master, child, draft + '-newer953')
                    blocker.rollback()
                key(b'\x12')
                expect('Original owner operation not recorded at actual barrier')
                key(b'\x12')
                registered()
                fixture.conversation.draft_contains(master, child, draft + '-newer953')
                result['session_id'] = session['session_id']
        assert fixture.conversation.actual_rows(database, 'p') == []
        assert not (root / 'home-local/.backboard/keys.json').exists()
    result.update(tui_exit=child.poll(), supervisor_exit=child.supervisor.poll())
    assert result['tui_exit'] == 0 and result['supervisor_exit'] == 0
    try:
        os.killpg(result['tui_group'], 0)
    except ProcessLookupError:
        result['tui_group_absent'] = True
    else:
        raise AssertionError('Owned RCLI TUI group remains after helper cleanup')
    result['passed'] = True
    print('PTY PASS rcli953 ' + mode, flush=True)
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

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Driver {
    child: Child,
    output: PathBuf,
    error: PathBuf,
}

impl Driver {
    fn start(root: &Path, mode: &str) -> Self {
        let output = root.join("driver.stdout");
        let error = root.join("driver.stderr");
        private_file(&output, b"");
        private_file(&error, b"");
        let logs = |path| fs::OpenOptions::new().append(true).open(path).unwrap();
        let child = crazyagent::spawn_gate::spawn(
            Command::new("/usr/bin/python3")
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
                .arg(mode)
                .stdin(Stdio::null())
                .stdout(logs(&output))
                .stderr(logs(&error))
                .process_group(0),
        )
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
                    return Err("Owned RCLI UI driver exceeded its output bound".into());
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                let output = fs::read_to_string(&self.output).unwrap();
                if status.success() && output.contains("PTY PASS rcli953 ") {
                    assert_eq!(
                        output.matches("owned-group=").count(),
                        output.matches("reaped-group=").count()
                    );
                    return Ok(());
                }
                return Err(format!("Real RCLI UI driver failed: {status}"));
            }
            if Instant::now() >= deadline {
                return Err("Owned RCLI UI driver exceeded 70 seconds".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn signal_group(group: u32, signal: &str) {
    assert!(group > 1 && matches!(signal, "-TERM" | "-KILL"));
    let _ = crazyagent::spawn_gate::status(
        Command::new("/bin/kill")
            .args([signal, "--", &format!("-{group}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
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

fn observer(root: &Path) -> Connection {
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("PRAGMA query_only=ON").unwrap();
    db
}

async fn verify_registration(root: &Path, mode: &str) {
    let result: Value =
        serde_json::from_slice(&fs::read(root.join("ui-result.json")).unwrap()).unwrap();
    assert_eq!(result["passed"], true);
    assert_eq!(result["sign_in_qualified"], false);
    let db = observer(root);
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
    assert_eq!(
        fs::read(root.join("global.toml")).unwrap(),
        CATALOG.as_bytes()
    );
    let profiles: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM configured_master_profiles",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if mode == "intent" {
        assert_eq!(profiles, 0);
        return;
    }
    assert_eq!(profiles, 1);
    let records: Vec<(String, String, Vec<u8>)> = db
        .prepare(
            "SELECT operation_id,lower(hex(request_hash)),result FROM owner_operations \
         WHERE origin='native_owner' ORDER BY revision_after LIMIT 16",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let registrations: Vec<_> = records
        .into_iter()
        .filter_map(|(id, hash, body)| {
            match serde_json::from_slice::<OwnerResult>(&body).unwrap() {
                OwnerResult::RcliProfileRegistered { registration } => {
                    Some((id, hash, registration))
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(registrations.len(), 1);
    let (operation, hash, metadata) = &registrations[0];
    assert!(metadata.generation > 0);
    assert_eq!(metadata.project_id.as_str(), "p");
    assert_eq!(
        metadata.session_id.as_str(),
        result["session_id"].as_str().unwrap()
    );
    assert_eq!(metadata.profile_ref.as_str(), "sandbox953");
    assert_eq!(
        serde_json::to_value(metadata)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        5
    );
    if mode == "reconcile" {
        assert_eq!(operation, result["operation_id"].as_str().unwrap());
    }
    verify_actual_receipt(root, operation, hash, metadata).await;
}

async fn verify_actual_receipt(
    root: &Path,
    operation: &str,
    hash: &str,
    metadata: &RcliProfileRegistrationMetadata,
) {
    let mut controller =
        OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let view = controller
        .read(OwnerRead::ProjectMaster {
            project_id: ProjectId::new("p").unwrap(),
        })
        .await
        .unwrap();
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = view.data
    else {
        panic!("Actual registered master is missing");
    };
    let session = *session;
    assert_eq!(session.session_id, metadata.session_id);
    assert_eq!(session.phase, MasterSessionPhase::Configured);
    assert!(session.current_turn.is_none());
    let (client, _) = OwnerClient::discover(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .unwrap();
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("rcli953-observe-original-receipt").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Reconcile {
                operation_id: RequestId::new(operation).unwrap(),
                expected_hash: Some(hash.into()),
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Committed { operation: actual } = reply.response else {
        panic!("Actual original registration receipt is missing");
    };
    assert_eq!(actual.provenance.operation_id.as_str(), operation);
    assert_eq!(actual.provenance.request_hash, hash);
    assert!(
        matches!(actual.result, OwnerResult::RcliProfileRegistered { registration }
        if registration.as_ref() == metadata)
    );
}

async fn exercise(mode: &str) {
    let _slot = UI_SLOT.lock().await;
    let root = tempfile::Builder::new()
        .prefix("ca-r953-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&root.path().join("global.toml"), CATALOG.as_bytes());
    private_file(&root.path().join("project.toml"), b"");
    for directory in ["home-empty", "home-api", "home-local"] {
        private_dir(&root.path().join(directory));
    }
    private_dir(&root.path().join("home-local/.backboard"));
    private_file(
        &root.path().join("home-local/.backboard/config.json"),
        &serde_json::to_vec(&serde_json::json!({
            "model": {"provider": "local-provider953", "model": "local-model953"},
            "providers": [{"id": "local-provider953", "name": "Local 953",
                "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:9/v1",
                "auth": {"type": "none"}}]
        }))
        .unwrap(),
    );
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
    owner
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert!(repository.join(".git").exists());
    owner
        .begin_mutation(
            OwnerMutation::CreateProject {
                project_id: ProjectId::new("p").unwrap(),
                spec: ProjectSpec {
                    name: "RCLI setup 953".into(),
                    repositories: vec![repository.to_str().unwrap().into()],
                },
            },
            1,
        )
        .unwrap();
    assert!(matches!(
        owner.send_pending().await.unwrap(),
        OwnerResponse::Applied { .. }
    ));
    drop(owner);
    let mut driver = Driver::start(root.path(), mode);
    let outcome = driver.finish().await;
    drop(driver);
    if outcome.is_ok() {
        verify_registration(root.path(), mode).await;
    }
    service.shutdown().await.unwrap();
    if let Err(reason) = outcome {
        let retained = root.keep();
        panic!(
            "{reason}; private evidence retained at {}",
            retained.display()
        );
    }
}

#[tokio::test]
async fn rcli_ui_retains_explicit_api_key_and_local_home_intent_without_authentication() {
    exercise("intent").await;
}

#[tokio::test]
async fn rcli_ui_registers_only_after_actual_repository_master_open_and_owner_confirmation() {
    exercise("registered").await;
}

#[tokio::test]
async fn rcli_ui_rejects_closed_session_and_reconciles_original_request_preserving_newer_draft() {
    exercise("reconcile").await;
}
