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
/// CLI that prints the official sign-in line and then waits like a real login, and one
/// registered stand-in for the Codex CLI: by default one whose interpreter is missing, so
/// it never starts, or one that answers the app-server sign-in requests on stdio and then
/// ends as `codex-mode` in the root says, noting each login start and logout it gets in
/// `codex-requests`. The stand-ins live in their own folder outside the engine's data
/// folder, whose files the engine never starts. Dropping it kills the engine and every
/// login it started.
struct Case {
    engine: Option<Child>,
    root: tempfile::TempDir,
    bin: tempfile::TempDir,
    codex: &'static str,
    claude: &'static str,
    home: tempfile::TempDir,
    temp: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        Self::with_codex("codex-unstartable")
    }

    /// Starts the engine with the named Codex stand-in, `codex-unstartable` or
    /// `codex-app-server`. Input: the stand-in's file name. Output: the case.
    fn with_codex(codex: &'static str) -> Self {
        Self::started(codex, "claude-login", None)
    }

    /// Starts the engine with a scratch HOME, never the owner's, and PATH /usr/bin:/bin, and
    /// with Codex and Claude Code stand-ins whose first line asks `env` for
    /// `crazyagent-test-shell`, an interpreter that exists only in the scratch HOME's
    /// `.local/bin`, as nvm, fnm or volta keep node under HOME. Output: the case.
    fn with_interpreter_in_home() -> Self {
        let home = scratch("ca-nar-home-");
        let bin = home.path().join(".local").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("crazyagent-test-shell"),
            "#!/bin/sh\nexec /bin/sh \"$@\"\n",
        )
        .unwrap();
        private(&bin.join("crazyagent-test-shell"), 0o700);
        Self::started("codex-by-name", "claude-by-name", Some(home))
    }

    /// Starts the engine with a scratch HOME in this user's own temp folder, which no other
    /// user can change and which is never the engine's TMPDIR, whatever the runner's is, and a
    /// Codex stand-in whose first line asks `env` for
    /// `crazyagent-test-login-shell`, an interpreter only the login shell's PATH names: the
    /// scratch login files wait until the test opens [`Self::release_probe`], at most 9 s so
    /// a failed test still lets the shell end inside the probe's 10 s limit, then leave
    /// `login-probe-done` in that HOME and add its folder, so the first probe run is still
    /// going when the first sign-in starts.
    #[cfg(target_os = "macos")]
    fn with_interpreter_on_login_path() -> Self {
        let temp = crazyagent::spawn_gate::output(
            Command::new("/usr/bin/getconf")
                .arg("DARWIN_USER_TEMP_DIR")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let temp = std::fs::canonicalize(String::from_utf8(temp.stdout).unwrap().trim()).unwrap();
        let home = tempfile::Builder::new()
            .prefix("ca-nar-login-")
            .tempdir_in(temp)
            .unwrap();
        private(home.path(), 0o700);
        let bin = home.path().join("login-only").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("crazyagent-test-login-shell"),
            "#!/bin/sh\nexec /bin/sh \"$@\"\n",
        )
        .unwrap();
        private(&bin.join("crazyagent-test-login-shell"), 0o700);
        for login in [".zprofile", ".bash_profile", ".profile"] {
            std::fs::write(
                home.path().join(login),
                "i=0\nwhile [ ! -e \"$HOME/release-probe\" ] && [ \"$i\" -lt 180 ]; do /bin/sleep 0.05; i=$((i+1)); done\n: > \"$HOME/login-probe-done\"\nPATH=\"$HOME/login-only/bin:$PATH\"\nexport PATH\n",
            )
            .unwrap();
        }
        Self::started("codex-by-login-name", "claude-login", Some(home))
    }

    /// Writes every stand-in and starts the engine with the named ones and a scratch HOME,
    /// a fresh empty one unless given, never the owner's. Inputs: the Codex and Claude Code
    /// stand-ins' file names and the scratch HOME, if any. Output: the case.
    fn started(codex: &'static str, claude: &'static str, home: Option<tempfile::TempDir>) -> Self {
        let home = home.unwrap_or_else(|| scratch("ca-nar-home-"));
        let root = scratch("ca-nar-");
        let bin = scratch("ca-nar-bin-");
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
        stand_in(bin.path(), "claude-login", &login);
        stand_in(
            bin.path(),
            "claude-by-name",
            &login.replacen("#!/bin/sh", "#!/usr/bin/env crazyagent-test-shell", 1),
        );
        stand_in(
            bin.path(),
            "codex-unstartable",
            "#!/nonexistent/crazyagent-test-interpreter\n",
        );
        stand_in(
            bin.path(),
            "claude-unstartable",
            "#!/nonexistent/crazyagent-test-interpreter\n",
        );
        let server = format!(
            r#"#!/bin/sh
echo $$ >> '{root}/login-pids'
mode=$(/bin/cat '{root}/codex-mode')
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | /usr/bin/sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      [ "$mode" = slow ] && /bin/sleep 3
      printf '{{"id":%s,"result":{{"userAgent":"stand-in","codexHome":"%s","platformFamily":"unix","platformOs":"macos"}}}}\n' "$id" "$CODEX_HOME"
      ;;
    *'"method":"account/login/start"'*)
      echo login >> '{root}/codex-requests'
      printf '{{"id":%s,"result":{{"type":"chatgpt","loginId":"stand-in-login","authUrl":"https://auth.openai.com/authorize?state=fixture"}}}}\n' "$id"
      case "$mode" in
        exit) exit 0 ;;
        garbage) printf 'not a frame\n' ;;
      esac
      ;;
    *'"method":"account/login/cancel"'*)
      exit 0
      ;;
    *'"method":"account/logout"'*)
      echo logout >> '{root}/codex-requests'
      ;;
  esac
done
"#,
            root = root.path().display()
        );
        stand_in(bin.path(), "codex-app-server", &server);
        stand_in(
            bin.path(),
            "codex-by-name",
            &server.replacen("#!/bin/sh", "#!/usr/bin/env crazyagent-test-shell", 1),
        );
        stand_in(
            bin.path(),
            "codex-by-login-name",
            &server.replacen("#!/bin/sh", "#!/usr/bin/env crazyagent-test-login-shell", 1),
        );
        std::fs::write(root.path().join("codex-mode"), "wait\n").unwrap();
        let mut case = Self {
            engine: None,
            root,
            bin,
            codex,
            claude,
            home,
            temp: scratch("ca-nar-tmp-"),
        };
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_crazyagent"));
        command
            .arg("engine")
            .arg(self.root())
            .arg(self.root().join("global.toml"))
            .arg(self.root().join("project.toml"))
            .arg(socket)
            .arg("--writable-settings")
            .arg("--claude-executable")
            .arg(self.bin.path().join(self.claude))
            .arg("--codex-executable")
            .arg(self.bin.path().join(self.codex))
            .env("CRAZYAGENT_DEBUG_CUSTODIAN", "1")
            .env("HOME", self.home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", self.temp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
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

    /// Opens the gate the scratch login files of [`Self::with_interpreter_on_login_path`]
    /// wait on, so the login shell's probe goes on to its end. Input: the case. Output: none.
    #[cfg(target_os = "macos")]
    fn release_probe(&self) {
        std::fs::write(self.home.path().join("release-probe"), b"").unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root().join("engine.log")).unwrap_or_default()
    }

    /// Waits until the engine's log holds one line exactly, at most 10 s. Input: the line.
    /// Output: none; it fails the test with the log when the line never comes.
    #[cfg(target_os = "macos")]
    async fn logged(&self, line: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.log().lines().any(|logged| logged == line) {
            assert!(
                Instant::now() < deadline,
                "the engine never logged {line}: {}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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

    /// Reads the stored phase of one sign-in attempt, read only, or none once its row is gone.
    /// Input: the attempt id. Output: the phase column.
    fn attempt_phase(&self, operation: &RequestId) -> Option<String> {
        let db = rusqlite::Connection::open_with_flags(
            self.root().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(2)).unwrap();
        db.query_row(
            "SELECT phase FROM native_auth_operations WHERE id=?1",
            [operation.as_str()],
            |row| row.get(0),
        )
        .ok()
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

/// A private scratch folder under the real /tmp, removed when dropped. Input: its name's
/// prefix. Output: the folder.
fn scratch(prefix: &str) -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    private(folder.path(), 0o700);
    folder
}

/// Writes one stand-in program only its owner can run or change. Inputs: its folder, its
/// name and its text.
fn stand_in(folder: &Path, name: &str, text: &str) {
    std::fs::write(folder.join(name), text).unwrap();
    private(&folder.join(name), 0o700);
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

async fn subscription(client: &OwnerClient, provider: Provider, name: &str) -> CredentialRef {
    client
        .add_account(
            AccountDraft {
                provider,
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

async fn start(
    client: &OwnerClient,
    scope: &NativeAuthScope,
    method: NativeAuthMethod,
) -> Result<NativeAuthFact, String> {
    client
        .start_native_auth(NativeAuthStart {
            request_id: next("start"),
            scope: scope.clone(),
            method,
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

/// Whether a sign-in has settled, read as both clients read it: Uncertain is still moving,
/// since a killed custodian's lost channel can show it before the engine reaps the custodian
/// and settles the attempt, and an attempt that stays Uncertain fails at the settle bound.
/// Input: the phase. Output: the answer.
fn ended(phase: NativeAuthPhase) -> bool {
    !matches!(
        phase,
        NativeAuthPhase::Starting
            | NativeAuthPhase::AwaitingOwner
            | NativeAuthPhase::CheckingAccount
            | NativeAuthPhase::Cancelling
            | NativeAuthPhase::Uncertain
    )
}

async fn waiting(client: &OwnerClient, scope: &NativeAuthScope) -> NativeAuthFact {
    waiting_with(client, scope, NativeAuthMethod::ClaudeOfficial).await
}

/// Starts a sign-in with the given method and waits until it shows its challenge to the
/// owner. Inputs: the owner client, the scope and the method. Output: the waiting fact.
async fn waiting_with(
    client: &OwnerClient,
    scope: &NativeAuthScope,
    method: NativeAuthMethod,
) -> NativeAuthFact {
    let begun = start(client, scope, method).await.unwrap();
    let shown = settle(client, scope, &begun, shown).await;
    assert_eq!(shown.phase, NativeAuthPhase::AwaitingOwner);
    shown
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_waiting_claude_sign_in_settles_cancelled_and_frees_the_account() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Claude, "Work Claude").await;
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
async fn removing_an_account_while_its_sign_in_runs_leaves_other_accounts_free_to_sign_in() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let removed = subscription(&client, Provider::Claude, "Work Claude").await;
    let other = subscription(&client, Provider::Claude, "Home Claude").await;
    let (scope, _) = scope(&client, &database, &removed).await;
    let begun = waiting(&client, &scope).await;
    let login = case.logins();
    assert_eq!(login.len(), 1);
    client.remove_account(removed).await.unwrap();
    rustix::process::kill_process(login[0], Signal::KILL).unwrap();
    gone(login[0]).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut phase = case.attempt_phase(&begun.operation_id);
    while matches!(
        phase.as_deref(),
        Some("starting" | "awaiting_owner" | "checking_account" | "cancelling")
    ) {
        assert!(
            Instant::now() < deadline,
            "the removed account's sign-in stayed {phase:?}: {}",
            case.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        phase = case.attempt_phase(&begun.operation_id);
    }
    assert_ne!(
        phase.as_deref(),
        Some("uncertain"),
        "the removed account's sign-in ended uncertain, which no account can clear: {}",
        case.log()
    );
    case.no_fence().await;
    let (elsewhere, _) = self::scope(&client, &database, &other).await;
    let started = start(&client, &elsewhere, NativeAuthMethod::ClaudeOfficial).await;
    assert_eq!(
        started.as_ref().err().map(String::as_str),
        None,
        "a sign-in on another account starts once the removed account's sign-in ended: {}",
        case.log()
    );
    let proceeds = settle(&client, &elsewhere, &started.unwrap(), shown).await;
    assert_eq!(proceeds.phase, NativeAuthPhase::AwaitingOwner);
    cancel(&client, &elsewhere, &proceeds).await;
    settle(&client, &elsewhere, &proceeds, ended).await;
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_custodian_frees_its_account_in_the_same_engine_run() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Claude, "Work Claude").await;
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
    let first = subscription(&client, Provider::Claude, "Work Claude").await;
    let other = subscription(&client, Provider::Claude, "Home Claude").await;
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
        start(&client, &elsewhere, NativeAuthMethod::ClaudeOfficial)
            .await
            .unwrap_err(),
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

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claude_sign_in_that_fails_before_its_login_starts_settles_failed_and_frees_the_account()
{
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Claude, "Work Claude").await;
    let (scope, profile) = scope(&client, &database, &account).await;
    let first = waiting(&client, &scope).await;
    cancel(&client, &scope, &first).await;
    settle(&client, &scope, &first, ended).await;
    case.no_fence().await;
    let library = case
        .root()
        .join("native-profiles")
        .join(&profile)
        .join("home")
        .join("Library");
    let kept = case.root().join("kept-library");
    std::fs::rename(&library, &kept).unwrap();
    std::os::unix::fs::symlink(&kept, &library).unwrap();
    let begun = start(&client, &scope, NativeAuthMethod::ClaudeOfficial)
        .await
        .unwrap();
    let failed = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (failed.phase, failed.code.as_deref()),
        (
            NativeAuthPhase::Failed,
            Some("native_auth_claude_keychain_unavailable")
        ),
        "{}",
        case.log()
    );
    assert_eq!(case.logins().len(), 1);
    case.no_fence().await;
    std::fs::remove_file(&library).unwrap();
    std::fs::rename(&kept, &library).unwrap();
    let again = waiting(&client, &scope).await;
    cancel(&client, &scope, &again).await;
    let cancelled = settle(&client, &scope, &again, ended).await;
    assert_eq!(cancelled.phase, NativeAuthPhase::Cancelled);
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_codex_sign_in_whose_harness_cannot_start_settles_unavailable_and_frees_the_account() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    for _ in 0..2 {
        let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
            .await
            .unwrap();
        let settled = settle(&client, &scope, &begun, ended).await;
        assert_eq!(
            (settled.phase, settled.code.as_deref()),
            (
                NativeAuthPhase::Unavailable,
                Some("native_auth_host_unavailable")
            ),
            "{}",
            case.log()
        );
        case.no_fence().await;
    }
}

/// A Claude Code sign-in whose harness the engine cannot resolve settles Unavailable with the
/// plain code and frees the account, twice in a row, as a Codex one does: the custodian is
/// handed no harness and ends the action without effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claude_sign_in_whose_harness_cannot_start_settles_unavailable_and_frees_the_account() {
    let case = Case::started("codex-unstartable", "claude-unstartable", None);
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Claude, "Work Claude").await;
    let (scope, _) = scope(&client, &database, &account).await;
    for _ in 0..2 {
        let begun = start(&client, &scope, NativeAuthMethod::ClaudeOfficial)
            .await
            .unwrap();
        let settled = settle(&client, &scope, &begun, ended).await;
        assert_eq!(
            (settled.phase, settled.code.as_deref()),
            (
                NativeAuthPhase::Unavailable,
                Some("native_auth_host_unavailable")
            ),
            "{}",
            case.log()
        );
        assert!(
            case.log()
                .lines()
                .any(|line| line == "native_auth_harness_unresolved harness=claude"),
            "{}",
            case.log()
        );
        case.no_fence().await;
    }
}

/// A harness whose first line asks `env` for an interpreter found only in a folder under
/// the engine's HOME, as nvm, fnm or volta keep node, signs in: the engine resolves and pins
/// the words, and the custodian, which has no HOME and no PATH, starts them as handed. Codex
/// and Claude Code both reach the owner's step, then cancel and free the account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_whose_interpreter_is_found_only_under_home_signs_in() {
    let case = Case::with_interpreter_in_home();
    let (client, database) = owner(case.root()).await;
    for (provider, name, method) in [
        (
            Provider::Chatgpt,
            "Work ChatGPT",
            NativeAuthMethod::CodexBrowser,
        ),
        (
            Provider::Claude,
            "Work Claude",
            NativeAuthMethod::ClaudeOfficial,
        ),
    ] {
        let account = subscription(&client, provider, name).await;
        let (scope, _) = scope(&client, &database, &account).await;
        let begun = start(&client, &scope, method).await.unwrap();
        let challenge = settle(&client, &scope, &begun, shown).await;
        assert_eq!(
            (challenge.phase, challenge.code.as_deref()),
            (NativeAuthPhase::AwaitingOwner, None),
            "{name}: {}",
            case.log()
        );
        cancel(&client, &scope, &challenge).await;
        let finished = settle(&client, &scope, &challenge, ended).await;
        assert_eq!(
            finished.phase,
            NativeAuthPhase::Cancelled,
            "{name}: {}",
            case.log()
        );
        case.no_fence().await;
    }
}

/// The first sign-in right after the engine starts, as in onboarding, finds a harness whose
/// interpreter only the login shell's PATH names: the engine's first probe run is still
/// going, so the engine waits for it, resolves once more and starts the harness, never
/// settling Unavailable while the answer is on its way.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_sign_in_after_start_waits_for_the_login_path_to_find_its_interpreter() {
    let case = Case::with_interpreter_on_login_path();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
        .await
        .unwrap();
    case.logged("native_auth_harness_waiting harness=codex")
        .await;
    case.release_probe();
    let challenge = settle(&client, &scope, &begun, shown).await;
    assert_eq!(
        (challenge.phase, challenge.code.as_deref()),
        (NativeAuthPhase::AwaitingOwner, None),
        "{}",
        case.log()
    );
    cancel(&client, &scope, &challenge).await;
    let finished = settle(&client, &scope, &challenge, ended).await;
    assert_eq!(finished.phase, NativeAuthPhase::Cancelled, "{}", case.log());
    case.no_fence().await;
}

/// Cancelling the first sign-in while the engine waits for its first probe run settles
/// Cancelled and frees the account: the engine has logged that it waits and the login
/// shell's probe has not ended yet when the cancel is sent, so the cancel lands inside the
/// wait, and the engine never goes on to call the harness unresolved.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_sign_in_while_the_engine_waits_for_its_first_probe_run_settles_cancelled() {
    let case = Case::with_interpreter_on_login_path();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
        .await
        .unwrap();
    case.logged("native_auth_harness_waiting harness=codex")
        .await;
    assert_eq!(
        read(&client, &scope, &begun).await.phase,
        NativeAuthPhase::Starting
    );
    assert!(
        !case.home.path().join("login-probe-done").exists(),
        "the probe ended before the cancel: {}",
        case.log()
    );
    cancel(&client, &scope, &begun).await;
    case.release_probe();
    let finished = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (finished.phase, finished.code.as_deref()),
        (NativeAuthPhase::Cancelled, None),
        "{}",
        case.log()
    );
    case.no_fence().await;
    assert!(
        !case.log().contains("native_auth_harness_unresolved"),
        "{}",
        case.log()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_codex_sign_in_whose_harness_ends_before_confirming_settles_cancelled() {
    let case = Case::with_codex("codex-app-server");
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    for _ in 0..2 {
        let begun = waiting_with(&client, &scope, NativeAuthMethod::CodexBrowser).await;
        cancel(&client, &scope, &begun).await;
        let finished = settle(&client, &scope, &begun, ended).await;
        assert_eq!(
            (finished.phase, finished.code.as_deref()),
            (NativeAuthPhase::Cancelled, None),
            "{}",
            case.log()
        );
        case.no_fence().await;
    }
    assert!(
        !case.log().contains("native_auth_dispatch_failed"),
        "{}",
        case.log()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_codex_sign_in_whose_harness_fails_after_starting_settles_failed_with_its_code() {
    let case = Case::with_codex("codex-app-server");
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    for (mode, code) in [
        ("exit", "native_auth_codex_exited_before_observation"),
        ("garbage", "native_auth_codex_protocol_invalid"),
    ] {
        std::fs::write(case.root().join("codex-mode"), format!("{mode}\n")).unwrap();
        let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
            .await
            .unwrap();
        let settled = settle(&client, &scope, &begun, ended).await;
        assert_eq!(
            (settled.phase, settled.code.as_deref()),
            (NativeAuthPhase::Failed, Some(code)),
            "{mode}: {}",
            case.log()
        );
        case.no_fence().await;
    }
    assert!(
        !case.log().contains("native_auth_dispatch_failed"),
        "{}",
        case.log()
    );
    std::fs::write(case.root().join("codex-mode"), "wait\n").unwrap();
    let again = waiting_with(&client, &scope, NativeAuthMethod::CodexBrowser).await;
    cancel(&client, &scope, &again).await;
    settle(&client, &scope, &again, ended).await;
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_codex_logout_whose_harness_never_answers_settles_failed_at_its_deadline() {
    let case = Case::with_codex("codex-app-server");
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = client
        .logout_native_auth(NativeAuthLogout {
            request_id: next("logout"),
            scope: scope.clone(),
        })
        .await
        .unwrap()
        .fact;
    let settled = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (settled.phase, settled.code.as_deref()),
        (NativeAuthPhase::Failed, Some("native_auth_codex_deadline")),
        "{}",
        case.log()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("codex-requests")).unwrap(),
        "logout\n"
    );
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_codex_sign_in_before_its_harness_starts_the_login_settles_cancelled() {
    let case = Case::with_codex("codex-app-server");
    std::fs::write(case.root().join("codex-mode"), "slow\n").unwrap();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
        .await
        .unwrap();
    assert_eq!(begun.phase, NativeAuthPhase::Starting);
    let spawned = Instant::now() + Duration::from_secs(10);
    while case.logins().is_empty() {
        assert!(Instant::now() < spawned, "the Codex stand-in never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        read(&client, &scope, &begun).await.phase,
        NativeAuthPhase::Starting
    );
    cancel(&client, &scope, &begun).await;
    let finished = settle(&client, &scope, &begun, ended).await;
    assert_eq!(
        (finished.phase, finished.code.as_deref()),
        (NativeAuthPhase::Cancelled, None),
        "{}",
        case.log()
    );
    assert!(
        !case.root().join("codex-requests").exists(),
        "the login started before the cancel"
    );
    case.no_fence().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_codex_sign_in_before_its_harness_is_spawned_settles_cancelled() {
    let case = Case::with_codex("codex-app-server");
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Chatgpt, "Work ChatGPT").await;
    let (scope, _) = scope(&client, &database, &account).await;
    let begun = start(&client, &scope, NativeAuthMethod::CodexBrowser)
        .await
        .unwrap();
    assert_eq!(begun.phase, NativeAuthPhase::Starting);
    cancel(&client, &scope, &begun).await;
    let finished = settle(&client, &scope, &begun, ended).await;
    assert!(
        case.logins().is_empty(),
        "the Codex stand-in started before the cancel"
    );
    assert_eq!(
        (finished.phase, finished.code.as_deref()),
        (NativeAuthPhase::Cancelled, None),
        "{}",
        case.log()
    );
    case.no_fence().await;
}

/// SECAUDIT account-removal-leaves-native-managed-profile-credentials: removing a signed-in
/// native account leaves no sign-in in its profile, with no restart, and its slot stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_signed_in_account_leaves_no_sign_in_in_its_profile() {
    let case = Case::new();
    let (client, database) = owner(case.root()).await;
    let account = subscription(&client, Provider::Claude, "dummy-managed").await;
    let (scope, profile) = scope(&client, &database, &account).await;
    let begun = waiting(&client, &scope).await;
    cancel(&client, &scope, &begun).await;
    settle(&client, &scope, &begun, ended).await;
    case.no_fence().await;
    let slot = case.root().join("native-profiles").join(&profile);
    let placeholder = slot.join("home").join("auth.json");
    std::fs::write(&placeholder, "dummy").unwrap();
    std::fs::set_permissions(&placeholder, std::fs::Permissions::from_mode(0o600)).unwrap();
    client.remove_account(account).await.unwrap();
    assert!(!placeholder.exists(), "the sign-in is still there");
    assert!(slot.join("identity.json").exists(), "the slot stays");
}
