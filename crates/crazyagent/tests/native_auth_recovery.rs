#![cfg(unix)]

use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::config::ModelAuthentication;
use crazyagent::engine::owner::*;
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use rustix::process::{Pid, Signal};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// One real engine process on a private root, with a registered stand-in for the Claude
/// CLI that prints the official sign-in line and then waits like a real login.
/// Dropping it kills the engine and every login it started.
struct Case {
    engine: Option<Child>,
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-nar-")
            .tempdir_in("/private/tmp")
            .unwrap();
        private(root.path(), 0o700);
        std::fs::write(
            root.path().join("global.toml"),
            "version = 1\ntimeout_ms = 5000\n",
        )
        .unwrap();
        private(&root.path().join("global.toml"), 0o600);
        let login = format!(
            "#!/bin/sh\necho $$ >> '{}'\nprintf '%s\\n' \"If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=fixture\"\nexec /bin/sleep 120\n",
            root.path().join("login-pids").display()
        );
        std::fs::write(root.path().join("claude-login"), login).unwrap();
        private(&root.path().join("claude-login"), 0o700);
        let mut case = Self { engine: None, root };
        case.launch("first.sock");
        case
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn launch(&mut self, socket: &str) {
        assert!(self.engine.is_none());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root().join("engine.log"))
            .unwrap();
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(self.root())
                .arg(self.root().join("global.toml"))
                .arg(self.root().join("project.toml"))
                .arg(socket)
                .arg("--writable-settings")
                .arg("--claude-executable")
                .arg(self.root().join("claude-login"))
                .env("CRAZYAGENT_DEBUG_CUSTODIAN", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(log)),
        )
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        self.engine = Some(child);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = send.send(read);
        });
        let line = receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert_eq!(line.trim(), "ready", "{}", self.log());
    }

    fn kill_engine(&mut self) {
        let mut engine = self.engine.take().unwrap();
        engine.kill().unwrap();
        engine.wait().unwrap();
    }

    fn engine_pid(&self) -> u32 {
        self.engine.as_ref().unwrap().id()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root().join("engine.log")).unwrap_or_default()
    }

    fn logins(&self) -> Vec<Pid> {
        std::fs::read_to_string(self.root().join("login-pids"))
            .unwrap_or_default()
            .lines()
            .map(|line| Pid::from_raw(line.trim().parse().unwrap()).unwrap())
            .collect()
    }

    fn custodian(&self) -> Pid {
        let listing = crazyagent::spawn_gate::output(
            Command::new("/bin/ps")
                .args(["-A", "-o", "pid=,ppid=,command="])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let engine = self.engine_pid().to_string();
        let found: Vec<Pid> = String::from_utf8(listing.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?;
                let parent = fields.next()?;
                let command = fields.collect::<Vec<_>>().join(" ");
                (parent == engine && command.contains("native-auth-custodian"))
                    .then(|| Pid::from_raw(pid.parse().unwrap()).unwrap())
            })
            .collect();
        assert_eq!(found.len(), 1, "one custodian expected: {found:?}");
        found[0]
    }

    fn fences(&self) -> i64 {
        let db = rusqlite::Connection::open_with_flags(
            self.root().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(2)).unwrap();
        db.query_row("SELECT COUNT(*) FROM native_auth_fences", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    async fn no_fence(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.fences() != 0 {
            assert!(
                Instant::now() < deadline,
                "the sign-in fence was never released: {}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn hold(&self, profile: &str) -> std::fs::File {
        let manifest = std::fs::File::open(
            self.root()
                .join("native-profiles")
                .join(profile)
                .join("identity.json"),
        )
        .unwrap();
        rustix::fs::flock(
            &manifest,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();
        manifest
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if let Some(mut engine) = self.engine.take() {
            let _ = engine.kill();
            let _ = engine.wait();
        }
        for login in self.logins() {
            let _ = rustix::process::kill_process(login, Signal::KILL);
        }
    }
}

fn private(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn next(kind: &str) -> RequestId {
    RequestId::new(format!(
        "{kind}-{}",
        REQUESTS.fetch_add(1, Ordering::SeqCst)
    ))
    .unwrap()
}

fn alive(pid: Pid) -> bool {
    rustix::process::test_kill_process(pid).is_ok()
}

async fn gone(pid: Pid) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < deadline, "process {pid:?} never exited");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn owner(root: &Path) -> (OwnerClient, String) {
    let (client, identity) =
        OwnerClient::discover(root, OwnerClientKind::Tui, Duration::from_secs(20))
            .await
            .unwrap();
    (client, identity.database_instance().to_owned())
}

async fn subscription(client: &OwnerClient, name: &str) -> CredentialRef {
    client
        .add_account(
            AccountDraft {
                provider: Provider::Claude,
                method: SignInMethod::Subscription,
                name: name.into(),
                url: None,
                model: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

/// Reads the sign-in scope of one account and the folder name of its native profile.
/// Inputs: the owner client, the database instance and the account. Output: both.
async fn scope(
    client: &OwnerClient,
    database: &str,
    account: &CredentialRef,
) -> (NativeAuthScope, String) {
    let read = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: next("record"),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Connections,
                    id: account.clone(),
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = read.response else {
        panic!("connection read required")
    };
    let OwnerData::ModelAccessRecord {
        source_hash,
        record: ModelAccessRecord::Connection { connection },
        ..
    } = state.data
    else {
        panic!("a subscription account is a connection")
    };
    let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        panic!("a subscription account uses a managed profile")
    };
    let profile = format!("{:x}", Sha256::digest(profile_ref.as_str().as_bytes()));
    (
        NativeAuthScope {
            expected_database_instance: database.to_owned(),
            connection: account.clone(),
            expected_config_id: state.config_id,
            expected_source_hash: source_hash,
        },
        profile,
    )
}

async fn start(client: &OwnerClient, scope: &NativeAuthScope) -> Result<NativeAuthFact, String> {
    client
        .start_native_auth(NativeAuthStart {
            request_id: next("start"),
            scope: scope.clone(),
            method: NativeAuthMethod::ClaudeOfficial,
        })
        .await
        .map(|reply| reply.fact)
        .map_err(|error| error.0)
}

async fn cancel(client: &OwnerClient, scope: &NativeAuthScope, fact: &NativeAuthFact) {
    client
        .cancel_native_auth(NativeAuthCancel {
            request_id: next("cancel"),
            scope: scope.clone(),
            operation_id: fact.operation_id.clone(),
        })
        .await
        .unwrap();
}

async fn read(
    client: &OwnerClient,
    scope: &NativeAuthScope,
    fact: &NativeAuthFact,
) -> NativeAuthFact {
    client
        .read_native_auth(NativeAuthQuery {
            request_id: next("read"),
            scope: scope.clone(),
            operation_id: fact.operation_id.clone(),
        })
        .await
        .unwrap()
        .fact
}

async fn settle(
    client: &OwnerClient,
    scope: &NativeAuthScope,
    fact: &NativeAuthFact,
    done: fn(NativeAuthPhase) -> bool,
) -> NativeAuthFact {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let observed = read(client, scope, fact).await;
        if done(observed.phase) {
            return observed;
        }
        assert!(
            Instant::now() < deadline,
            "sign-in stayed {:?}",
            observed.phase
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn shown(phase: NativeAuthPhase) -> bool {
    phase != NativeAuthPhase::Starting
}

fn ended(phase: NativeAuthPhase) -> bool {
    !matches!(
        phase,
        NativeAuthPhase::Starting
            | NativeAuthPhase::AwaitingOwner
            | NativeAuthPhase::CheckingAccount
            | NativeAuthPhase::Cancelling
    )
}

async fn waiting(client: &OwnerClient, scope: &NativeAuthScope) -> NativeAuthFact {
    let begun = start(client, scope).await.unwrap();
    let shown = settle(client, scope, &begun, shown).await;
    assert_eq!(shown.phase, NativeAuthPhase::AwaitingOwner);
    shown
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_waiting_claude_sign_in_settles_cancelled_and_frees_the_account() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, "Work Claude").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = waiting(&client, &scope).await;
    let login = case.logins();
    assert_eq!(login.len(), 1);
    assert!(alive(login[0]));
    cancel(&client, &scope, &begun).await;
    let finished = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (finished.phase, finished.code.as_deref()),
        (NativeAuthPhase::Cancelled, None),
        "{}",
        case.log()
    );
    case.no_fence().await;
    gone(login[0]).await;
    assert!(
        !case.log().contains("native_auth_dispatch_failed"),
        "{}",
        case.log()
    );
    waiting(&client, &scope).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_custodian_frees_its_account_in_the_same_engine_run() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, "Work Claude").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = waiting(&client, &scope).await;
    let custodian = case.custodian();
    rustix::process::kill_process(custodian, Signal::KILL).unwrap();
    let finished = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (finished.phase, finished.code.as_deref()),
        (NativeAuthPhase::Failed, Some("native_auth_interrupted")),
        "{}",
        case.log()
    );
    case.no_fence().await;
    let again = waiting(&client, &scope).await;
    assert_ne!(again.operation_id, begun.operation_id);
    cancel(&client, &scope, &again).await;
    settle(&client, &scope, &again, ended).await;
    case.no_fence().await;
}

/// Starts a sign-in on the first of two subscription accounts, waits until it shows the
/// sign-in page, then kills the engine and waits for the orphaned custodian to exit.
/// Input: the case. Output: both accounts, the first one's profile folder and its fact.
async fn interrupted_by_a_killed_engine(
    case: &mut Case,
) -> (CredentialRef, CredentialRef, String, NativeAuthFact) {
    let (client, database) = owner(case.root()).await;
    let first = subscription(&client, "Work Claude").await;
    let other = subscription(&client, "Home Claude").await;
    let (scope, profile) = scope(&client, &database, &first).await;
    let interrupted = waiting(&client, &scope).await;
    let custodian = case.custodian();
    drop(client);
    case.kill_engine();
    gone(custodian).await;
    assert_eq!(case.fences(), 1);
    (first, other, profile, interrupted)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_engine_frees_every_account_when_it_starts_again() {
    let mut case = Case::new();
    let (first, other, _, interrupted) = interrupted_by_a_killed_engine(&mut case).await;
    case.launch("second.sock");
    assert_eq!(case.fences(), 0, "{}", case.log());
    let (client, database) = owner(case.root()).await;
    let (scope, _) = self::scope(&client, &database, &first).await;
    let (elsewhere, _) = self::scope(&client, &database, &other).await;
    let settled = read(&client, &scope, &interrupted).await;
    assert_eq!(
        (settled.phase, settled.code.as_deref()),
        (NativeAuthPhase::Failed, Some("native_auth_interrupted"))
    );
    let proceeds = waiting(&client, &elsewhere).await;
    cancel(&client, &elsewhere, &proceeds).await;
    settle(&client, &elsewhere, &proceeds, ended).await;
    case.no_fence().await;
    let again = waiting(&client, &scope).await;
    cancel(&client, &scope, &again).await;
    settle(&client, &scope, &again, ended).await;
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_old_sign_in_stays_fenced_while_its_profile_is_held_and_clears_once_free() {
    let mut case = Case::new();
    let (first, other, profile, interrupted) = interrupted_by_a_killed_engine(&mut case).await;
    let held = case.hold(&profile);
    case.launch("second.sock");
    let (client, database) = owner(case.root()).await;
    let (scope, _) = self::scope(&client, &database, &first).await;
    let (elsewhere, _) = self::scope(&client, &database, &other).await;
    assert_eq!(
        start(&client, &elsewhere).await.unwrap_err(),
        "native_auth_recovery_required"
    );
    assert_eq!(case.fences(), 1);
    assert_eq!(
        read(&client, &scope, &interrupted).await.phase,
        NativeAuthPhase::Uncertain
    );
    drop(held);
    let proceeds = waiting(&client, &elsewhere).await;
    let settled = read(&client, &scope, &interrupted).await;
    assert_eq!(
        (settled.phase, settled.code.as_deref()),
        (NativeAuthPhase::Failed, Some("native_auth_interrupted"))
    );
    cancel(&client, &elsewhere, &proceeds).await;
    settle(&client, &elsewhere, &proceeds, ended).await;
    case.no_fence().await;
}
