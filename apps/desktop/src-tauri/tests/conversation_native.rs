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
