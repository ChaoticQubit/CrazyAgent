#![cfg(unix)]

use crazyagent::accounts::{AccountDraft, AccountView, Provider, SignInMethod};
use crazyagent::client_controller::OwnerController;
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::owner::{
    AccountProfileOutcome, AccountProfileRequest, ModelAccessCollection, ModelAccessRecord,
    NativeAuthScope, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead,
    OwnerResponse, OwnerResult,
};
use crazyagent::master_session::{MasterOwnerAction, MasterTurnPhase, MasterTurnView};
use crazyagent::paths::Harnesses;
use crazyagent::protocol::{ConfigId, CredentialRef, ProjectId, ProjectSpec, RequestId, SessionId};
use crazyagent::usage::capture::Tokens;
use crazyagent::usage::report::{UsageReport, UsageRow};
use crazyagent::usage::service::{SCAN_EVERY, master_agent};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PROJECT: &str = "rcli-usage";
const POOL: &str = "rcli-usage-pool";
const MODEL: &str = "qwen2.5:0.5b";
const OLLAMA: &str = "127.0.0.1:11434";
const ATTACH_TIME: Duration = Duration::from_secs(20);
const READY_TIME: Duration = Duration::from_secs(20);
const TURN_TIME: Duration = Duration::from_secs(240);
const TAGS_BYTES_MAX: u64 = 1 << 20;
const PROBE_FIRST: (u64, u64, u64) = (50, 0, 5);
const PROBE: [(u64, u64, u64); 2] = [(100, 0, 10), (200, 0, 20)];
const STDERR_TAIL_BYTES: usize = 4096;
const COUNTED_WHEN: &str =
    "the engine counts usage only while its team runtime runs, which needs one registered harness";

/// One `crazyagent engine` process on the test's own root, started with the RCLI host the
/// test names, so the master runs through that host and never the owner's engine, and with
/// a Codex or Claude Code executable, so its team runtime and usage upkeep run.
struct Engine {
    child: Child,
    stdout: PathBuf,
}

impl Engine {
    /// Starts the engine on a root with an explicit RCLI host and team harness and waits for
    /// `ready`. Inputs: the root, the RCLI bundle and the harness flag with its executable.
    /// Output: the running engine.
    fn start(root: &Path, rcli: &Path, (flag, harness): (&str, &Path)) -> Self {
        let stdout = root.join("engine.stdout");
        let child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(root)
                .arg(root.join("global.toml"))
                .arg(root.join("project.toml"))
                .arg("engine.sock")
                .arg("--writable-settings")
                .args([OsStr::new("--rcli-executable"), rcli.as_os_str()])
                .args([OsStr::new(flag), harness.as_os_str()])
                .stdin(Stdio::null())
                .stdout(File::create(&stdout).unwrap())
                .stderr(File::create(root.join("engine.stderr")).unwrap()),
        )
        .unwrap();
        let mut engine = Self { child, stdout };
        let deadline = Instant::now() + READY_TIME;
        while std::fs::read(&engine.stdout).unwrap() != b"ready\n" {
            assert!(
                engine.child.try_wait().unwrap().is_none(),
                "engine exited before ready"
            );
            assert!(Instant::now() < deadline, "engine readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        engine
    }

    /// Stops the engine with SIGTERM as the desktop app does and waits for it to exit.
    fn stop(&mut self) {
        let terminated = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &self.child.id().to_string(),
        ]))
        .unwrap();
        assert!(terminated.success());
        let deadline = Instant::now() + READY_TIME;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "engine shutdown deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Keeps the engine root on disk when the test fails, so its logs and store can be read.
struct Retained(Option<tempfile::TempDir>);

impl Drop for Retained {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(root) = self.0.take()
        {
            eprintln!("rcli usage fixture retained at {}", root.keep().display());
        }
    }
}

/// The usage lines one agent row counted, summed per attribution, as the engine stored them.
#[derive(Debug, PartialEq, Eq)]
struct Recorded {
    kind: String,
    project: String,
    pool: String,
    account: String,
    harness: String,
    tokens: Tokens,
}

/// The verified RCLI bundle the test runs, from `CRAZYAGENT_RCLI`. Output: its path; a
/// missing or relative path fails the test as UNAVAILABLE.
fn rcli_bundle() -> PathBuf {
    let bundle = std::env::var_os("CRAZYAGENT_RCLI")
        .map(PathBuf::from)
        .expect("UNAVAILABLE: set CRAZYAGENT_RCLI to the verified crazyagent-rcli bundle");
    assert!(
        bundle.is_absolute() && bundle.is_file(),
        "UNAVAILABLE: {bundle:?}"
    );
    bundle
}

/// The harness the engine registers so its usage upkeep runs: Codex, else Claude Code,
/// found on PATH the way the engine's default start finds them, since the positional start
/// this test uses looks up nothing. Output: the engine flag and the resolved executable;
/// neither on PATH fails the test as UNAVAILABLE.
fn scanning_harness() -> (&'static str, PathBuf) {
    let found = crazyagent::paths::resolve_harnesses(
        Harnesses::default(),
        std::env::var_os("PATH").as_deref(),
        None,
    );
    let (flag, executable) = match (found.codex, found.claude) {
        (Some(codex), _) => ("--codex-executable", codex),
        (None, Some(claude)) => ("--claude-executable", claude),
        (None, None) => panic!("UNAVAILABLE: Codex or Claude Code on PATH ({COUNTED_WHEN})"),
    };
    assert!(executable.is_absolute(), "{executable:?}");
    (flag, executable)
}

/// Reads the local Ollama's model list. Output: the raw reply, at most [`TAGS_BYTES_MAX`].
async fn ollama_tags() -> std::io::Result<String> {
    let mut stream = crazyagent::spawn_gate::connect_tcp(OLLAMA).await?;
    stream
        .write_all(b"GET /api/tags HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .await?;
    let mut reply = Vec::new();
    stream.take(TAGS_BYTES_MAX).read_to_end(&mut reply).await?;
    Ok(String::from_utf8_lossy(&reply).into_owned())
}

/// Fails the test as UNAVAILABLE when the local account's model is not served.
async fn local_model_served() {
    let tags = ollama_tags().await.unwrap_or_else(|error| {
        panic!("UNAVAILABLE: the local account needs Ollama at {OLLAMA}: {error}")
    });
    assert!(
        tags.contains(&format!("\"{MODEL}\"")),
        "UNAVAILABLE: Ollama at {OLLAMA} does not serve {MODEL}"
    );
}

fn git_repository(path: &Path) -> String {
    std::fs::create_dir(path).unwrap();
    std::fs::write(path.join("README.md"), "# usage\n").unwrap();
    for arguments in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &[
            "-c",
            "user.name=owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-qm",
            "init",
        ],
    ] {
        let status =
            crazyagent::spawn_gate::status(Command::new("git").args(arguments).current_dir(path))
                .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }
    path.to_str().unwrap().to_owned()
}

async fn apply(controller: &mut OwnerController, generation: &mut u64, mutation: OwnerMutation) {
    controller.read(OwnerRead::Capabilities).await.unwrap();
    *generation += 1;
    controller.begin_mutation(mutation, *generation).unwrap();
    let response = controller.send_pending().await.unwrap();
    assert!(
        matches!(&response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "{response:?}"
    );
}

async fn source(controller: &mut OwnerController) -> (ConfigId, String) {
    let state = controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
        panic!("model access source required")
    };
    (state.config_id, source_hash)
}

/// Creates the project, gives its master a pool holding only the local account and sets
/// up that account's profile, all through the owner protocol. Inputs: the owner client and
/// its engine identity, the TUI controller, its generation and the account.
async fn project_on_local_account(
    (owner, database_instance): (&OwnerClient, &str),
    controller: &mut OwnerController,
    generation: &mut u64,
    (root, account): (&Path, &AccountView),
) {
    let project = ProjectId::new(PROJECT).unwrap();
    apply(
        controller,
        generation,
        OwnerMutation::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "RCLI usage".into(),
                repositories: vec![git_repository(&root.join("repository"))],
            },
        },
    )
    .await;
    let (config_id, source_hash) = source(controller).await;
    apply(
        controller,
        generation,
        OwnerMutation::ModelAccess {
            expected_config_id: config_id,
            expected_source_hash: source_hash,
            edit: ModelAccessEdit::CreatePool {
                pool: AccountPool {
                    id: CredentialRef::new(POOL).unwrap(),
                    name: None,
                    members: vec![account.id.clone()],
                    projects: vec![CredentialRef::new(project.as_str()).unwrap()],
                    roles: vec![CredentialRef::new("project_leader").unwrap()],
                    fallback_allowed: false,
                    provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
                },
            },
        },
    )
    .await;
    let record = controller
        .read(OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: account.id.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessRecord {
        source_hash,
        record: ModelAccessRecord::Connection { .. },
        ..
    } = record.data
    else {
        panic!("the pooled account must be a config connection")
    };
    let configured = owner
        .configure_account_profile(
            AccountProfileRequest {
                request_id: RequestId::new("configure-local").unwrap(),
                scope: NativeAuthScope {
                    expected_database_instance: database_instance.to_owned(),
                    connection: account.id.clone(),
                    expected_config_id: record.config_id,
                    expected_source_hash: source_hash,
                },
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            configured.outcome,
            AccountProfileOutcome::ConfiguredOnly { .. }
        ),
        "{configured:?}"
    );
}

async fn finished_turn(
    controller: &mut OwnerController,
    project_id: &ProjectId,
    session_id: &SessionId,
) -> MasterTurnView {
    let deadline = Instant::now() + TURN_TIME;
    loop {
        let state = controller
            .read(OwnerRead::ProjectMasterTurns {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: None,
                limit: 16,
            })
            .await
            .unwrap();
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("master turns required")
        };
        if let Some(turn) = page.turns.into_iter().find(|turn| {
            matches!(
                turn.phase,
                MasterTurnPhase::Succeeded
                    | MasterTurnPhase::Failed
                    | MasterTurnPhase::Cancelled
                    | MasterTurnPhase::Uncertain
                    | MasterTurnPhase::Blocked
            )
        }) {
            return turn;
        }
        assert!(Instant::now() < deadline, "master turn deadline");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Opens the project's master as the TUI does and runs one owner turn to its end.
/// Inputs: the TUI controller, its generation and the project. Output: the finished turn.
async fn master_turn(
    controller: &mut OwnerController,
    generation: &mut u64,
    project: &ProjectId,
) -> MasterTurnView {
    let open = crazyagent::tui::default_master_open(controller, project)
        .await
        .unwrap();
    apply(
        controller,
        generation,
        OwnerMutation::ProjectMaster { action: open },
    )
    .await;
    let state = controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        panic!("open master session required")
    };
    apply(
        controller,
        generation,
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Submit {
                project_id: project.clone(),
                session_id: session.session_id.clone(),
                text: "Say hi in five words.".into(),
            },
        },
    )
    .await;
    finished_turn(controller, project, &session.session_id).await
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn token_count((input, cached, output): (u64, u64, u64)) -> String {
    serde_json::json!({
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {"total_token_usage": {
                "input_tokens": input,
                "cached_input_tokens": cached,
                "output_tokens": output,
                "total_tokens": input + output,
            }},
        },
    })
    .to_string()
}

fn counted((input, cached, output): (u64, u64, u64)) -> Tokens {
    Tokens {
        input: input - cached,
        output,
        cache_read: cached,
        cache_write: 0,
    }
}

/// Records a stopped Codex sub-agent of the project in the engine's store, the agent whose
/// record the test grows to see each scan finish. Input: the engine root. Output: its id.
fn add_probe(root: &Path) -> i64 {
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    crazyagent::team::initialize(&connection).unwrap();
    let now = now_ms();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms, stopped_ms)
             VALUES (?1, 'sub', 'codex', 0, 'stopped', ?2, ?2, ?2)",
            rusqlite::params![PROJECT, now],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn codex_rollout(root: &Path, agent: i64) -> PathBuf {
    let home = crazyagent::team::runtime::agent_home(&root.join("team"), agent);
    let day = home.join("sessions").join("2026").join("10").join("28");
    for folder in [home.as_path(), day.as_path()] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    day.join("rollout-2026-10-28T07-30-00-0199.jsonl")
}

fn append(path: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

/// Reads the usage lines the engine stored for one agent row, summed per attribution.
/// Inputs: the engine root and the row key. Output: one entry per attribution.
fn recorded(root: &Path, agent: &str) -> Vec<Recorded> {
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT kind,project,pool,account,harness,SUM(input),SUM(output),SUM(cache_read),SUM(cache_write)
             FROM usage_tokens WHERE agent=?1
             GROUP BY kind,project,pool,account,harness ORDER BY kind,project,pool,account,harness",
        )
        .unwrap();
    let total = |row: &rusqlite::Row<'_>, at: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(row.get::<_, i64>(at)?).unwrap())
    };
    statement
        .query_map([agent], |row| {
            Ok(Recorded {
                kind: row.get(0)?,
                project: row.get(1)?,
                pool: row.get(2)?,
                account: row.get(3)?,
                harness: row.get(4)?,
                tokens: Tokens {
                    input: total(row, 5)?,
                    output: total(row, 6)?,
                    cache_read: total(row, 7)?,
                    cache_write: total(row, 8)?,
                },
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn row<'a>(rows: &'a [UsageRow], key: &str) -> Option<&'a UsageRow> {
    rows.iter().find(|row| row.key == key)
}

async fn report_until(
    owner: &OwnerController,
    project: &ProjectId,
    what: &str,
    done: impl Fn(&UsageReport) -> bool,
) -> UsageReport {
    let start = Instant::now();
    loop {
        let report = owner
            .usage_report(Some(project.clone()), None, 0)
            .await
            .unwrap();
        if done(&report) {
            return report;
        }
        assert!(
            start.elapsed() < SCAN_EVERY * 6,
            "{what} within {:?} ({COUNTED_WHEN}): {report:?}",
            SCAN_EVERY * 6
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Proves the engine's usage upkeep scans before the test reads any count from it: the probe
/// agent's first total reaches the page within six scan periods. A failure carries the end
/// of the engine's stderr, where a failed upkeep start says why usage is not counted.
/// Inputs: the engine root, the reader, the project, and the probe record and row key.
async fn upkeep_runs(
    root: &Path,
    owner: &OwnerController,
    project: &ProjectId,
    (record, key): (&Path, &str),
) {
    append(record, &token_count(PROBE_FIRST));
    let start = Instant::now();
    loop {
        let report = owner
            .usage_report(Some(project.clone()), None, 0)
            .await
            .unwrap();
        if row(&report.agents, key).is_some_and(|row| row.tokens == counted(PROBE_FIRST)) {
            return;
        }
        if start.elapsed() >= SCAN_EVERY * 6 {
            let stderr = std::fs::read(root.join("engine.stderr")).unwrap_or_default();
            let tail = &stderr[stderr.len().saturating_sub(STDERR_TAIL_BYTES)..];
            panic!(
                "the usage upkeep never scanned the probe record within {:?} ({COUNTED_WHEN}); engine stderr ends: {}",
                SCAN_EVERY * 6,
                String::from_utf8_lossy(tail)
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Appends each probe total to the probe agent's record and waits until the page counts it.
/// Each count after the first proves the scan before it finished over every other record.
/// Inputs: the owner client, the project, the probe record and row key, and the totals.
/// Output: the report that counted the last total.
async fn rescanned(
    owner: &OwnerController,
    project: &ProjectId,
    (record, key): (&Path, &str),
    totals: &[(u64, u64, u64)],
) -> UsageReport {
    let mut report = None;
    for &total in totals {
        append(record, &token_count(total));
        report = Some(
            report_until(
                owner,
                project,
                "the next scan counts the probe record",
                |report| row(&report.agents, key).is_some_and(|row| row.tokens == counted(total)),
            )
            .await,
        );
    }
    report.unwrap()
}

/// V2 acceptance, usage shows RCLI tokens. A project master on a local account runs one
/// real turn through CrazyAgent's own pinned RCLI host against the local Ollama model; the
/// usage page then shows that turn's reported tokens under the project's master row, stored
/// against the RCLI harness, the pool and the account, never as not reported, and the
/// desktop and TUI readers show the same row. A probe Codex record first proves the usage
/// upkeep scans; two later scans leave the master row unchanged.
///
/// Needs `CRAZYAGENT_RCLI` naming the verified `crazyagent-rcli` bundle, Ollama serving
/// `qwen2.5:0.5b` at 127.0.0.1:11434 and Codex or Claude Code on PATH, which the engine is
/// given explicitly; without one the test fails as UNAVAILABLE. The engine runs on its own
/// root with no provider key and never reads the owner's profile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_usage_page_shows_an_rcli_master_turn_under_rcli_counted_once() {
    let bundle = rcli_bundle();
    let (flag, harness) = scanning_harness();
    local_model_served().await;
    let fixture = tempfile::Builder::new()
        .prefix("ca-av2-rcli-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = std::fs::canonicalize(fixture.path()).unwrap();
    let _retained = Retained(Some(fixture));
    let global = root.join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut engine = Engine::start(&root, &bundle, (flag, &harness));
    let (owner, identity) = OwnerClient::discover(&root, OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap();
    let local = owner
        .add_account(
            AccountDraft {
                provider: Provider::Local,
                method: SignInMethod::Url,
                name: "Laptop".into(),
                url: None,
                model: Some(MODEL.into()),
            },
            None,
        )
        .await
        .unwrap();
    let mut tui = OwnerController::attach(&root, OwnerClientKind::Tui, ATTACH_TIME)
        .await
        .unwrap();
    let mut generation = 0;
    project_on_local_account(
        (&owner, identity.database_instance()),
        &mut tui,
        &mut generation,
        (&root, &local),
    )
    .await;
    drop(owner);
    let project = ProjectId::new(PROJECT).unwrap();
    let turn = master_turn(&mut tui, &mut generation, &project).await;
    assert_eq!(
        turn.phase,
        MasterTurnPhase::Succeeded,
        "the RCLI master turn: {:?}",
        turn.reason
    );
    let reply = turn.reply.expect("a real model reply");
    assert!(!reply.text.trim().is_empty(), "empty reply");

    let key = master_agent(Some(&project));
    let desk = OwnerController::attach(&root, OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap();
    let probe = add_probe(&root);
    let probe_key = format!("team-{probe}");
    let record = codex_rollout(&root, probe);
    upkeep_runs(&root, &desk, &project, (&record, &probe_key)).await;
    let report = report_until(
        &desk,
        &project,
        "the page shows the RCLI master turn's tokens",
        |report| row(&report.agents, &key).is_some_and(|row| !row.tokens.is_empty()),
    )
    .await;
    let master = row(&report.agents, &key).unwrap().clone();
    assert!(
        master.tokens.input > 0 && master.tokens.output > 0,
        "the RCLI host reported the turn's input and output: {master:?}"
    );
    assert_eq!(master.runs, 1, "one turn of one master session");
    assert_eq!(master.not_reported_by, None, "RCLI tokens are counted");
    let rcli_name = crazyagent::harnesses::spec("rcli").unwrap().name;
    assert!(
        !report.not_reported_by.iter().any(|name| name == rcli_name),
        "the page never names RCLI as not reporting: {:?}",
        report.not_reported_by
    );
    assert_eq!(
        report.total,
        master.tokens.plus(counted(PROBE_FIRST)),
        "the turn and the probe's first total are the only usage"
    );
    assert_eq!(
        row(&report.pools, POOL).map(|row| row.tokens),
        Some(master.tokens),
        "the pool's row holds the turn"
    );
    assert_eq!(
        row(&report.accounts, local.id.as_str()).map(|row| row.tokens),
        Some(master.tokens),
        "the local account's row holds the turn"
    );
    let stored = [Recorded {
        kind: "master".into(),
        project: PROJECT.into(),
        pool: POOL.into(),
        account: local.id.as_str().into(),
        harness: "rcli".into(),
        tokens: master.tokens,
    }];
    assert_eq!(
        recorded(&root, &key),
        stored,
        "the engine stored the turn once, against the RCLI harness"
    );
    assert_eq!(
        tui.usage_report(Some(project.clone()), None, 0)
            .await
            .unwrap()
            .agents,
        report.agents,
        "the TUI reads the same rows as the desktop"
    );
    let everywhere = tui.usage_report(None, None, 0).await.unwrap();
    assert_eq!(
        row(&everywhere.agents, &key),
        Some(&master),
        "the TUI's usage page over every project shows the same master row"
    );

    let rescan = rescanned(&desk, &project, (&record, &probe_key), &PROBE).await;
    assert_eq!(
        row(&rescan.agents, &key),
        Some(&master),
        "two later scans add nothing to the RCLI master row"
    );
    assert_eq!(
        rescan.total,
        master.tokens.plus(counted(PROBE[1])),
        "the total grows only by the probe record"
    );
    assert_eq!(
        recorded(&root, &key),
        stored,
        "the scans store no second count of the turn"
    );
    drop(desk);
    drop(tui);
    engine.stop();
}
