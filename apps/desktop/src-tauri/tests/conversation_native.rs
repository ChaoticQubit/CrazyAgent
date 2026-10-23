#![cfg(feature = "native-webdriver")]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::conversation::{ConversationContent, ConversationMutation};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerControl, OwnerData, OwnerDecision, OwnerMutation, OwnerRead,
    OwnerResponse, OwnerResult,
};
use crazyagent::protocol::*;
use crazyagent::verification::{BaselineDraft, BaselineOrigin, CheckKind, CheckSpec, Risk};
use serde_json::{Value, json};
use std::error::Error;
use std::fs::{File, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn Error>>;

struct OwnedProcess {
    child: Child,
    pid: u32,
}

impl OwnedProcess {
    fn signal(&self, signal: &str) -> bool {
        ProcessCommand::new("/bin/kill")
            .args([signal, &format!("-{}", self.pid)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn absent(&self) -> bool {
        !self.signal("-0")
    }

    fn finish(&mut self) -> bool {
        self.signal("-CONT");
        if !self.absent() {
            self.signal("-TERM");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let _ = self.child.try_wait();
            if self.absent() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.signal("-KILL");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let _ = self.child.try_wait();
            if self.absent() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !self.finish() {
            eprintln!("owned native test group cleanup deadline");
        }
    }
}

fn baseline() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("alpha").unwrap(),
        origin: BaselineOrigin::HumanAuthored,
        requirements: "Explicit native pending baseline".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"actual conversation behavior".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"original test preservation".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

async fn seed(root: &Path) -> TestResult<OwnerController> {
    let (ordinary, mut observer) = OwnerController::attach_configured(
        Client::new(root.join("e.sock"), Duration::from_secs(2))?,
        OwnerClientKind::Tui,
    )
    .await?;
    for id in ["alpha", "beta", "constructor"] {
        let revision = observer.read(OwnerRead::Capabilities).await?.revision;
        let reply = ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("native-conversation")?,
                request_id: RequestId::new(id)?,
                expected_revision: revision,
                command: Command::CreateProject {
                    project_id: ProjectId::new(id)?,
                    spec: ProjectSpec {
                        name: id.into(),
                        repositories: vec![],
                    },
                },
            })
            .await?;
        if reply.outcome != Outcome::Applied {
            return Err("real native fixture project creation failed".into());
        }
    }
    observer.read(OwnerRead::Capabilities).await?;
    observer.begin_mutation(OwnerMutation::ProposeBaseline { draft: baseline() }, 1)?;
    if !matches!(observer.send_pending().await?, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
    {
        return Err("actual native baseline proposal failed".into());
    }
    Ok(observer)
}

async fn observe(observer: &mut OwnerController) -> TestResult<Value> {
    let mut result = serde_json::Map::new();
    for id in ["alpha", "beta"] {
        let mut cursor = None;
        let mut messages = Vec::new();
        let mut rows = 0;
        loop {
            let state = observer
                .read(OwnerRead::ConversationHistory {
                    project_id: ProjectId::new(id)?,
                    cursor,
                    limit: 32,
                })
                .await?;
            let OwnerData::ConversationHistory { page } = state.data else {
                return Err("actual native history required".into());
            };
            rows += page.entries.len();
            if rows > 1024 {
                return Err("native observer row bound".into());
            }
            messages.extend(
                page.entries
                    .into_iter()
                    .filter(|e| e.content == ConversationContent::Message)
                    .map(|e| e.text),
            );
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let state = observer
            .read(OwnerRead::CurrentBrief {
                project_id: ProjectId::new(id)?,
            })
            .await?;
        let OwnerData::CurrentBrief { brief, .. } = state.data else {
            return Err("actual native brief required".into());
        };
        let brief = brief.map(|entry| match entry.content {
            ConversationContent::Brief { version } => {
                json!({"version": version.to_string(), "text": entry.text})
            }
            _ => Value::Null,
        });
        result.insert(
            id.into(),
            json!({"messages": messages, "brief": brief, "history_rows": rows}),
        );
    }
    let id = baseline().identity()?;
    let state = observer
        .read(OwnerRead::Baseline {
            baseline_id: id.clone(),
        })
        .await?;
    let OwnerData::Baseline {
        approved, revoked, ..
    } = state.data
    else {
        return Err("actual baseline observation required".into());
    };
    result.insert(
        "baseline".into(),
        json!({"id": id.as_str(), "approved": approved, "revoked": revoked}),
    );
    Ok(Value::Object(result))
}

fn group(command: &mut ProcessCommand, evidence: &Path, name: &str) -> TestResult<OwnedProcess> {
    let child = command
        .stdin(Stdio::null())
        .stdout(File::create_new(evidence.join(format!("{name}.stdout")))?)
        .stderr(File::create_new(evidence.join(format!("{name}.stderr")))?)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    Ok(OwnedProcess { child, pid })
}

fn check_backend_logs(evidence: &Path) -> TestResult<()> {
    let owner = std::fs::symlink_metadata(evidence)?;
    if !owner.is_dir() || owner.permissions().mode() & 0o077 != 0 {
        return Err("private owned native evidence directory required".into());
    }
    let directory = evidence.join("webdriver");
    let metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || metadata.uid() != owner.uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("private owned native backend directory required".into());
    }
    let mut count = 0_u8;
    let mut bytes = 0_u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        count += 1;
        if count > 16 {
            return Err("native backend log entry bound".into());
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or("native backend log name required")?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !name.starts_with("wdio")
            || !name.ends_with(".log")
            || name.len() > 128
            || !metadata.is_file()
            || metadata.uid() != owner.uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("private regular owned native backend log required".into());
        }
        bytes = bytes
            .checked_add(metadata.len())
            .ok_or("native backend log size overflow")?;
        if bytes > 4 * 1024 * 1024 {
            return Err("native backend log byte bound".into());
        }
    }
    Ok(())
}

async fn window_case(
    root: &Path,
    evidence: &Path,
    service: Option<&EngineService>,
    cli: &mut Option<OwnedProcess>,
) -> TestResult<Value> {
    let mut observer = seed(root).await?;
    let app_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("desktop root missing")?
        .to_path_buf();
    let app = std::env::var("CRAZYAGENT_DESKTOP_TEST_APP")?;
    let node = std::env::var("CRAZYAGENT_NODE")?;
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let number = port.local_addr()?.port();
    drop(port);
    std::fs::create_dir(evidence.join("home"))?;
    std::fs::create_dir(evidence.join("tmp"))?;
    let mode = if service.is_some() {
        "authority"
    } else {
        "uncertainty"
    };
    let mut command = ProcessCommand::new(&node);
    command
        .current_dir(&app_root)
        .env_clear()
        .envs([
            (
                "PATH",
                "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            ),
            ("HOME", evidence.join("home").to_string_lossy().into_owned()),
            (
                "TMPDIR",
                evidence.join("tmp").to_string_lossy().into_owned(),
            ),
            ("LANG", "en_US.UTF-8".to_owned()),
            ("NO_COLOR", "1".to_owned()),
            (
                "CRAZYAGENT_DESKTOP_SOCKET",
                root.join("e.sock").to_string_lossy().into_owned(),
            ),
            ("CRAZYAGENT_DESKTOP_TEST_APP", app.clone()),
            ("CRAZYAGENT_DESKTOP_TEST_PORT", number.to_string()),
            (
                "CRAZYAGENT_DESKTOP_TEST_EVIDENCE",
                evidence.to_string_lossy().into_owned(),
            ),
            ("CRAZYAGENT_CONVERSATION_CASE", mode.to_owned()),
        ])
        .arg(app_root.join("node_modules/@wdio/cli/bin/wdio.js"))
        .args(["run", "test/native/conversation-full.wdio.conf.mjs"]);
    let mut driver = group(&mut command, evidence, "webdriver")?;
    let driver_pid = driver.pid;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut next = 1;
    let mut actions = Vec::new();
    let outcome: TestResult<_> = async {
        loop {
            if let Some(status) = driver.child.try_wait()? {
                return Ok(status.code());
            }
            if Instant::now() >= deadline {
                return Err("native full test deadline".into());
            }
            for name in ["webdriver.stdout", "webdriver.stderr"] {
                if std::fs::metadata(evidence.join(name))?.len() > 4 * 1024 * 1024 {
                    return Err("native output bound".into());
                }
            }
            check_backend_logs(evidence)?;
            let path = evidence.join(format!("request-{next}.json"));
            if path.exists() {
                if next > 64 || std::fs::metadata(&path)?.len() > 4096 {
                    return Err("native request bound".into());
                }
                let request: Value = serde_json::from_slice(&std::fs::read(path)?)?;
                let action = request["action"].as_str().ok_or("native action missing")?;
                let response = match action {
                    "snapshot" => observe(&mut observer).await?,
                    "concurrent_brief" => {
                        let state = observer
                            .read(OwnerRead::CurrentBrief {
                                project_id: ProjectId::new("alpha")?,
                            })
                            .await?;
                        let OwnerData::CurrentBrief { brief, .. } = state.data else {
                            return Err("actual fixture brief required".into());
                        };
                        let expected_version = match brief.map(|e| e.content) {
                            Some(ConversationContent::Brief { version }) => version,
                            None => 0,
                            _ => return Err("unexpected brief kind".into()),
                        };
                        observer.begin_mutation(
                            OwnerMutation::Conversation {
                                mutation: ConversationMutation::ReplaceBrief {
                                    project_id: ProjectId::new("alpha")?,
                                    expected_version,
                                    text: "independent version".into(),
                                },
                            },
                            next,
                        )?;
                        let receipt = observer.send_pending().await?;
                        let OwnerResponse::Applied { operation } = receipt else {
                            return Err("actual concurrent receipt required".into());
                        };
                        if operation.result != OwnerResult::Applied {
                            return Err("actual concurrent effect required".into());
                        }
                        observe(&mut observer).await?
                    }
                    "revoke" | "reactivate" => {
                        let host = service
                            .ok_or("host control unavailable in CLI fixture")?
                            .owner_runtime()
                            .ok_or("actual host runtime missing")?;
                        let epoch = host.epoch(OwnerClientKind::Desktop)?;
                        let reply = host
                            .control(OwnerControl {
                                operation_id: RequestId::new(format!("native-control-{next}"))?,
                                target: OwnerClientKind::Desktop,
                                expected_epoch: epoch,
                                decision: if action == "revoke" {
                                    OwnerDecision::Revoke
                                } else {
                                    OwnerDecision::Reactivate
                                },
                            })
                            .await?;
                        json!({"active": reply.active, "epoch": reply.owner_epoch.to_string()})
                    }
                    "pause" | "resume" => {
                        let owned = cli.as_mut().ok_or("owned CLI fixture required")?;
                        if !owned.signal(if action == "pause" { "-STOP" } else { "-CONT" }) {
                            return Err("owned engine signal failed".into());
                        }
                        let deadline = Instant::now() + Duration::from_secs(2);
                        loop {
                            let status = ProcessCommand::new("/bin/ps")
                                .args(["-o", "state=", "-p", &owned.pid.to_string()])
                                .output()?;
                            let stopped = String::from_utf8(status.stdout)?.trim().starts_with('T');
                            if stopped == (action == "pause") {
                                break;
                            }
                            if Instant::now() >= deadline {
                                return Err(
                                    "actual owned child stop/resume observation deadline".into()
                                );
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        json!({"stopped": action == "pause", "owned_pid": owned.pid})
                    }
                    _ => return Err("unrecognized native fixture action".into()),
                };
                actions.push(
                    json!({"sequence": next, "action": action, "actual_observation": response}),
                );
                let temporary = evidence.join(format!("reply-{next}.pending"));
                std::fs::write(&temporary, serde_json::to_vec(&response)?)?;
                std::fs::rename(temporary, evidence.join(format!("reply-{next}.json")))?;
                next += 1;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    .await;
    let cleaned = driver.finish();
    let backend_logs = check_backend_logs(evidence);
    let report = json!({"driver_pid": driver_pid, "driver_group_absent": driver.absent(), "owned_cleanup_ok": cleaned, "app_path": app, "mode": mode, "actions": actions, "exit": outcome.as_ref().ok(), "error": outcome.as_ref().err().map(ToString::to_string), "backend_log_bounds_error": backend_logs.as_ref().err().map(ToString::to_string)});
    std::fs::write(
        evidence.join("completion.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    if !cleaned {
        return Err("owned native driver group cleanup failed".into());
    }
    if outcome? != Some(0) {
        return Err(format!("native workflow failed; evidence {}", evidence.display()).into());
    }
    backend_logs?;
    Ok(report)
}

fn evidence(mode: &str) -> TestResult<PathBuf> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.local-state/desktop-conversation-execution/native-full");
    std::fs::create_dir_all(&directory)?;
    let unique = tempfile::Builder::new()
        .prefix(mode)
        .tempdir_in(directory)?;
    std::fs::set_permissions(unique.path(), Permissions::from_mode(0o700))?;
    Ok(unique.keep())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_window_conversation_version_conflict_drafts_and_revocation() {
    let root = tempfile::Builder::new()
        .prefix("ca-dn-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), Permissions::from_mode(0o700)).unwrap();
    let evidence = evidence("authority-").unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(210),
        window_case(root.path(), &evidence, Some(&service), &mut None),
    )
    .await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "native EngineService cleanup failed"
    );
    assert!(
        matches!(outcome, Ok(Ok(_))),
        "actual native authority workflow failed: {outcome:?}; {}",
        evidence.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_window_queued_late_delivery_preserves_identical_and_other_project_drafts() {
    let root = tempfile::Builder::new()
        .prefix("ca-dq-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), Permissions::from_mode(0o700)).unwrap();
    let evidence = evidence("uncertainty-").unwrap();
    let cli_path = std::env::var("CRAZYAGENT_TEST_CLI").unwrap();
    let mut command = ProcessCommand::new(cli_path);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .arg("engine")
        .arg(root.path())
        .arg(root.path().join("g"))
        .arg(root.path().join("p"))
        .arg("e.sock");
    let mut cli = Some(group(&mut command, &evidence, "engine").unwrap());
    let outcome: TestResult<_> = async {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if OwnerController::attach_configured(
                Client::new(root.path().join("e.sock"), Duration::from_millis(200))?,
                OwnerClientKind::Tui,
            )
            .await
            .is_ok()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("actual CLI readiness deadline".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        window_case(root.path(), &evidence, None, &mut cli).await
    }
    .await;
    let owned = cli.as_mut().unwrap();
    let cleaned = owned.finish();
    std::fs::write(
        evidence.join("engine-cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid": owned.pid, "group_absent": owned.absent(), "cleanup_ok": cleaned}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(cleaned, "actual CLI fixture cleanup failed");
    assert!(
        outcome.is_ok(),
        "actual native queued delivery failed: {outcome:?}; {}",
        evidence.display()
    );
}

fn project_private_directory(path: &Path) -> TestResult<()> {
    std::fs::create_dir(path)?;
    std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
    assert_eq!(
        std::fs::symlink_metadata(path)?.permissions().mode() & 0o777,
        0o700
    );
    Ok(())
}

fn project_write(path: &Path, value: &Value) -> TestResult<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > 1024 * 1024 {
        return Err("project evidence JSON byte bound".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    assert_eq!(file.metadata()?.len(), bytes.len() as u64);
    assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

fn project_no_effects(root: &Path, specs: &[ProjectSpec]) -> TestResult<()> {
    for path in specs
        .iter()
        .flat_map(|spec| spec.repositories.iter().map(PathBuf::from))
        .chain(
            [
                "g",
                "p",
                "worktrees",
                "repositories",
                "tasks",
                "baselines",
                "runs",
            ]
            .map(|name| root.join(name)),
        )
    {
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(format!("unexpected creation effect at {}", path.display()).into()),
        }
    }
    Ok(())
}

async fn project_observation(ordinary: &Client, owner: &mut OwnerController) -> TestResult<Value> {
    use crazyagent::engine::owner::OwnerView;

    let reply = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("native-project-readback")?,
            request_id: RequestId::new("native-project-snapshot")?,
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await?;
    let Outcome::Snapshot {
        mut projects,
        tasks,
    } = reply.outcome
    else {
        return Err("actual bound ordinary Snapshot required".into());
    };
    if !tasks.is_empty() || projects.len() > 2 {
        return Err("project creation must retain zero tasks and at most two projects".into());
    }
    let baselines = owner.read_page(OwnerView::Baselines, None, Some(1)).await?;
    let OwnerData::Page { items, next } = baselines.data else {
        return Err("actual baseline page required".into());
    };
    if !items.is_empty() || next.is_some() {
        return Err("project creation and first brief must retain zero baselines".into());
    }
    projects.sort_by(|left, right| left.project_id.as_str().cmp(right.project_id.as_str()));
    let mut rows = Vec::new();
    for project in projects {
        let current = owner
            .read(OwnerRead::CurrentBrief {
                project_id: project.project_id.clone(),
            })
            .await?;
        let OwnerData::CurrentBrief { brief, .. } = current.data else {
            return Err("actual current project brief required".into());
        };
        let brief = match brief {
            Some(entry) => match entry.content {
                ConversationContent::Brief { version } => {
                    json!({"version": version.to_string(), "text": entry.text})
                }
                _ => return Err("current brief must have actual brief content".into()),
            },
            None => Value::Null,
        };
        let history = owner
            .read(OwnerRead::ConversationHistory {
                project_id: project.project_id.clone(),
                cursor: None,
                limit: 2,
            })
            .await?;
        let OwnerData::ConversationHistory { page } = history.data else {
            return Err("actual project history required".into());
        };
        if page.next.is_some() || page.entries.len() > 1 {
            return Err("only one explicit first brief is allowed in actual history".into());
        }
        rows.push(json!({"id": project.project_id.as_str(), "spec": project.spec, "brief": brief, "history": page.entries}));
    }
    Ok(json!({"projects": rows, "tasks": tasks, "baselines": items}))
}

fn project_cli(argv: &[std::ffi::OsString], phase: &Path) -> TestResult<OwnedProcess> {
    if argv.len() != 6 || !Path::new(&argv[0]).is_absolute() {
        return Err("one exact absolute CLI engine argv required".into());
    }
    project_private_directory(&phase.join("engine-home"))?;
    project_private_directory(&phase.join("engine-tmp"))?;
    let mut command = ProcessCommand::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", phase.join("engine-home"))
        .env("TMPDIR", phase.join("engine-tmp"))
        .env("LANG", "en_US.UTF-8")
        .env("NO_COLOR", "1");
    project_write(
        &phase.join("engine-launch.json"),
        &json!({
            "argv": argv,
            "environment": command.get_envs().collect::<Vec<_>>(),
            "inherited_environment_cleared": true,
        }),
    )?;
    let process = group(&mut command, phase, "engine")?;
    for name in ["engine.stdout", "engine.stderr"] {
        std::fs::set_permissions(phase.join(name), Permissions::from_mode(0o600))?;
    }
    assert_eq!(process.pid, process.child.id());
    Ok(process)
}

async fn project_ready(
    root: &Path,
    cli: &mut OwnedProcess,
) -> TestResult<(Client, OwnerController)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if cli.child.try_wait()?.is_some() {
            return Err("actual CLI exited before owner readiness".into());
        }
        if let Ok(pair) = OwnerController::attach_configured(
            Client::new(root.join("e.sock"), Duration::from_millis(200))?,
            OwnerClientKind::Tui,
        )
        .await
        {
            return Ok(pair);
        }
        if Instant::now() >= deadline {
            return Err("actual CLI project readiness deadline".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn project_stop(cli: &mut OwnedProcess, root: &Path, phase: &Path) -> TestResult<()> {
    let signalled = cli.signal("-TERM");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut status = None;
    while Instant::now() < deadline {
        status = cli.child.try_wait()?;
        if status.is_some() && cli.absent() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let graceful = signalled && status.is_some_and(|status| status.success()) && cli.absent();
    let cleanup = cli.finish();
    let socket_absent = !root.join("e.sock").try_exists()?;
    project_write(
        &phase.join("engine-cleanup.json"),
        &json!({"pid": cli.pid, "term_requested": signalled, "graceful": graceful,
            "exit": status.and_then(|status| status.code()), "group_absent": cli.absent(),
            "cleanup_ok": cleanup, "socket_absent": socket_absent}),
    )?;
    if !cleanup || !cli.absent() || !socket_absent || !graceful {
        return Err("actual engine must exit gracefully with its group and socket absent".into());
    }
    Ok(())
}

fn project_phase_bounds(phase: &Path) -> TestResult<()> {
    check_backend_logs(phase)?;
    let mut bytes = 0_u64;
    let mut count = 0_u16;
    for entry in std::fs::read_dir(phase)? {
        let entry = entry?;
        let info = std::fs::symlink_metadata(entry.path())?;
        if info.is_dir() {
            if entry.file_name() == "webdriver" {
                for log in std::fs::read_dir(entry.path())? {
                    bytes += std::fs::symlink_metadata(log?.path())?.len();
                }
            }
            continue;
        }
        count += 1;
        if !info.is_file() || info.permissions().mode() & 0o077 != 0 || count > 200 {
            return Err("private project phase file admission".into());
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or("project evidence name required")?;
        let limit = if name.ends_with(".stdout") || name.ends_with(".stderr") {
            4 * 1024 * 1024
        } else if name.ends_with(".png") {
            2 * 1024 * 1024
        } else {
            1024 * 1024
        };
        if info.len() > limit {
            return Err("project phase file byte bound".into());
        }
        bytes = bytes
            .checked_add(info.len())
            .ok_or("project evidence byte overflow")?;
    }
    if bytes > 26 * 1024 * 1024 {
        return Err("project phase aggregate evidence bound".into());
    }
    Ok(())
}

async fn project_phase(
    root: &Path,
    phase: &Path,
    input: &Value,
    ordinary: &Client,
    owner: &mut OwnerController,
    specs: &[ProjectSpec],
) -> TestResult<Value> {
    let app_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("desktop root required")?
        .to_path_buf();
    let node = std::env::var("CRAZYAGENT_NODE")?;
    let app = std::env::var("CRAZYAGENT_DESKTOP_TEST_APP")?;
    if !Path::new(&node).is_absolute() || !Path::new(&app).is_absolute() {
        return Err("absolute native app and installed Node required".into());
    }
    project_write(&phase.join("input.json"), input)?;
    for name in ["home", "tmp", "webdriver"] {
        project_private_directory(&phase.join(name))?;
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let mut command = ProcessCommand::new(&node);
    command
        .current_dir(&app_root)
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", phase.join("home"))
        .env("TMPDIR", phase.join("tmp"))
        .env("LANG", "en_US.UTF-8")
        .env("NO_COLOR", "1")
        .env("CRAZYAGENT_DESKTOP_SOCKET", root.join("e.sock"))
        .env("CRAZYAGENT_DESKTOP_TEST_APP", &app)
        .env("CRAZYAGENT_DESKTOP_TEST_PORT", port.to_string())
        .env("CRAZYAGENT_DESKTOP_TEST_EVIDENCE", phase)
        .arg("--import")
        .arg(app_root.join("test/native/private-log-preload.mjs"))
        .arg(app_root.join("node_modules/@wdio/cli/bin/wdio.js"))
        .args(["run", "test/native/projects.wdio.conf.mjs"]);
    let invocation = json!({"program": command.get_program(),
        "args": command.get_args().collect::<Vec<_>>(),
        "environment": command.get_envs().collect::<Vec<_>>(),
        "inherited_environment_cleared": true});
    let mut driver = group(&mut command, phase, "webdriver")?;
    for name in ["webdriver.stdout", "webdriver.stderr"] {
        std::fs::set_permissions(phase.join(name), Permissions::from_mode(0o600))?;
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut next = 1_u8;
    let mut actions = Vec::new();
    let outcome: TestResult<_> = async {
        loop {
            project_phase_bounds(phase)?;
            project_no_effects(root, specs)?;
            if let Some(status) = driver.child.try_wait()? {
                return Ok(status.code());
            }
            if Instant::now() >= deadline {
                return Err("native project phase ninety-second deadline".into());
            }
            let request = phase.join(format!("request-{next}.json"));
            if request.try_exists()? {
                if next > 64 || std::fs::symlink_metadata(&request)?.len() > 4096 {
                    return Err("project request bounds".into());
                }
                let value: Value = serde_json::from_slice(&std::fs::read(&request)?)?;
                if value != json!({"action": "snapshot"}) {
                    return Err("closed actual project observation request required".into());
                }
                let actual = project_observation(ordinary, owner).await?;
                actions.push(json!({"sequence": next, "actual_observation": actual}));
                let temporary = phase.join(format!("reply-{next}.pending"));
                project_write(&temporary, &actual)?;
                std::fs::rename(temporary, phase.join(format!("reply-{next}.json")))?;
                next += 1;
                assert_eq!(usize::from(next) - 1, actions.len());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    .await;
    let cleaned = driver.finish();
    let mut port_error = None;
    let closed = match std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    ) {
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => true,
        Ok(_) => false,
        Err(error) => {
            port_error = Some(error.to_string());
            false
        }
    };
    project_write(
        &phase.join("completion.json"),
        &json!({"driver_pid": driver.pid, "driver_group_absent": driver.absent(),
            "cleanup_ok": cleaned, "port": port, "port_closed": closed, "port_error": port_error, "app_path": app,
            "exit": outcome.as_ref().ok(), "error": outcome.as_ref().err().map(ToString::to_string),
            "invocation": invocation, "actions": actions}),
    )?;
    if !cleaned || !driver.absent() || !closed {
        return Err("actual native app/driver group and embedded port must disappear".into());
    }
    if outcome? != Some(0) {
        return Err("actual project form WDIO phase failed".into());
    }
    project_phase_bounds(phase)?;
    let result = phase.join("ui-result.json");
    if std::fs::symlink_metadata(&result)?.len() > 1024 * 1024 {
        return Err("actual UI result byte bound".into());
    }
    let proof: Value = serde_json::from_slice(&std::fs::read(result)?)?;
    if proof["phase"] != input["phase"] {
        return Err("actual UI proof phase must match its owned lifetime".into());
    }
    Ok(proof)
}

async fn project_lifetime(
    root: &Path,
    phase: &Path,
    argv: &[std::ffi::OsString],
    input: &Value,
    specs: &[ProjectSpec],
) -> TestResult<Value> {
    let mut cli = project_cli(argv, phase)?;
    let outcome: TestResult<_> = async {
        let (ordinary, mut owner) = project_ready(root, &mut cli).await?;
        let initial = project_observation(&ordinary, &mut owner).await?;
        project_no_effects(root, specs)?;
        if input["phase"] == "create" {
            if initial["projects"] != json!([]) {
                return Err("actual initial engine must have zero projects".into());
            }
        } else if initial != input["persisted"] {
            return Err("real engine restart must preserve exact recorded state".into());
        }
        project_write(&phase.join("initial-observation.json"), &initial)?;
        let proof = project_phase(root, phase, input, &ordinary, &mut owner, specs).await?;
        let actual = project_observation(&ordinary, &mut owner).await?;
        if proof["actual"] != actual || actual["projects"].as_array().unwrap().len() != 2 {
            return Err("actual UI proof must match independent two-project readback".into());
        }
        for spec in specs {
            if !actual["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["spec"] == json!(spec))
            {
                return Err("actual stored project must preserve its exact input spec".into());
            }
        }
        let second = actual["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["spec"] == json!(&specs[1]))
            .ok_or("actual second project missing")?;
        if proof["selected_id"] != second["id"]
            || second["brief"] != json!({"version": "1", "text": input["first_brief"]})
        {
            return Err(
                "actual selected second project must alone own explicit brief version one".into(),
            );
        }
        let first = actual["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["spec"] == json!(&specs[0]))
            .ok_or("actual first project missing")?;
        if first["id"] == second["id"]
            || first["brief"] != Value::Null
            || first["history"] != json!([])
        {
            return Err(
                "first project negative control must remain distinct with no brief/history".into(),
            );
        }
        project_no_effects(root, specs)?;
        Ok(proof)
    }
    .await;
    let cleanup = project_stop(&mut cli, root, phase).await;
    let bounded = project_phase_bounds(phase);
    project_write(
        &phase.join("lifetime.json"),
        &json!({"argv": argv, "engine_pid": cli.pid, "outcome": outcome.as_ref().ok(),
            "error": outcome.as_ref().err().map(ToString::to_string),
            "cleanup_error": cleanup.as_ref().err().map(ToString::to_string),
            "bounds_error": bounded.as_ref().err().map(ToString::to_string)}),
    )?;
    let proof = outcome?;
    cleanup?;
    bounded?;
    Ok(proof)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_window_project_creation_first_brief_and_restart() {
    let run = evidence("projects-").unwrap();
    let root = tempfile::Builder::new()
        .prefix("ca-pn-")
        .tempdir_in("/tmp")
        .unwrap()
        .keep();
    std::fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    let cli = std::env::var_os("CRAZYAGENT_TEST_CLI").expect("actual built CLI required");
    let argv = vec![
        cli,
        "engine".into(),
        root.clone().into_os_string(),
        root.join("g").into_os_string(),
        root.join("p").into_os_string(),
        "e.sock".into(),
    ];
    let specs = [
        ProjectSpec {
            name: "Native first project".into(),
            repositories: vec![
                root.join("absent first ").display().to_string(),
                root.join("absent second").display().to_string(),
            ],
        },
        ProjectSpec {
            name: "Native second project".into(),
            repositories: vec![
                root.join("absent third").display().to_string(),
                root.join("absent quatrième").display().to_string(),
            ],
        },
    ];
    let first_brief = "Explicit first brief from the real native project form workflow.";
    project_write(
        &run.join("runtime.json"),
        &json!({"root": root, "argv": argv, "specs": specs}),
    )
    .unwrap();
    let outcome: TestResult<_> = async {
        project_no_effects(&root, &specs)?;
        let mut prior = Value::Null;
        let mut identities = Vec::new();
        for name in ["create", "restart"] {
            let phase = run.join(name);
            project_private_directory(&phase)?;
            let input = json!({"phase": name, "specs": specs, "first_brief": first_brief, "persisted": prior});
            let proof = project_lifetime(&root, &phase, &argv, &input, &specs).await?;
            let identity = proof["app_identity"].as_str().ok_or("actual native app lifetime identity missing")?;
            assert!(!identity.is_empty());
            identities.push(identity.to_owned());
            if name == "restart" {
                assert_eq!(proof["actual"], prior);
                assert_ne!(identities[0], identities[1], "actual native app lifetimes must differ");
            }
            prior = proof["actual"].clone();
        }
        assert_eq!(identities.len(), 2);
        assert!(!root.join("e.sock").try_exists()?);
        project_no_effects(&root, &specs)?;
        Ok(json!({"identities": identities, "persisted": prior}))
    }
    .await;
    project_write(
        &run.join("completion.json"),
        &json!({"runtime": root, "argv": argv, "outcome": outcome.as_ref().ok(),
            "error": outcome.as_ref().err().map(ToString::to_string),
            "scope": "two form projects, one first brief, full native app and CLI restart; no off-first-page UI claim"}),
    ).unwrap();
    assert!(
        outcome.is_ok(),
        "actual native project creation/restart failed: {outcome:?}; {}",
        run.display()
    );
}
