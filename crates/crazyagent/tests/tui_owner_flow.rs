#![cfg(unix)]

use crazyagent::engine::Client;
use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerRequest, OwnerResponse, OwnerResult, OwnerSummary, OwnerView,
};
use crazyagent::protocol::{
    ClientId, Command as EngineCommand, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec,
    RequestId, WireRequest,
};
use crazyagent::verification::{BaselineDraft, BaselineOrigin, CheckKind, CheckSpec, Risk};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    overflow: bool,
}

fn capture(mut input: impl Read + Send + 'static) -> (Arc<Mutex<Capture>>, JoinHandle<()>) {
    let output = Arc::new(Mutex::new(Capture::default()));
    let captured = output.clone();
    let reader = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            let count = input.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            let mut output = captured.lock().unwrap();
            let remaining = 65536 - output.bytes.len();
            output
                .bytes
                .extend_from_slice(&buffer[..count.min(remaining)]);
            output.overflow |= count > remaining;
        }
    });
    (output, reader)
}

struct Process {
    child: Child,
    stdout: Arc<Mutex<Capture>>,
    stderr: Arc<Mutex<Capture>>,
    readers: Vec<JoinHandle<()>>,
}

impl Process {
    fn spawn(command: &mut Command) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let (stdout, out_reader) = capture(child.stdout.take().unwrap());
        let (stderr, err_reader) = capture(child.stderr.take().unwrap());
        Self {
            child,
            stdout,
            stderr,
            readers: vec![out_reader, err_reader],
        }
    }

    fn diagnostics(&self) -> String {
        format!(
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&self.stdout.lock().unwrap().bytes),
            String::from_utf8_lossy(&self.stderr.lock().unwrap().bytes),
        )
    }

    fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let output = self.stdout.lock().unwrap();
                assert!(!output.overflow, "engine readiness output exceeded 64 KiB");
                if let Some(end) = output.bytes.iter().position(|byte| *byte == b'\n') {
                    assert_eq!(&output.bytes[..=end], b"ready\n");
                    return;
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "engine exited before ready: {}",
                self.diagnostics()
            );
            assert!(
                Instant::now() < deadline,
                "engine readiness deadline: {}",
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "owned child deadline: {}",
                self.diagnostics()
            );
            assert!(
                !self.stdout.lock().unwrap().overflow && !self.stderr.lock().unwrap().overflow,
                "owned child output exceeded 64 KiB per stream"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        assert!(!self.stdout.lock().unwrap().overflow);
        assert!(!self.stderr.lock().unwrap().overflow);
        status
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = crazyagent::spawn_gate::status(
                Command::new("/bin/kill")
                    .args(["-s", "TERM", &self.child.id().to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        if std::thread::panicking() {
            eprintln!("{}", self.diagnostics());
        }
    }
}

fn owner_request(id: &str, revision: &Cursor, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: revision.clone(),
        operation,
    }
}

fn baseline(project_id: ProjectId, marker: &str) -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id,
        origin: BaselineOrigin::Imported,
        requirements: marker.into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"actual owner Evidence code check".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"actual owner Evidence integrity check".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

#[tokio::test]
async fn real_tui_discovers_baseline_detail_in_evidence() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let binary = env!("CARGO_BIN_EXE_crazyagent");
    let mut engine = Process::spawn(
        Command::new(binary)
            .arg("engine")
            .arg(&root_path)
            .arg(root_path.join("global.toml"))
            .arg(root_path.join("project.toml"))
            .arg("engine.sock"),
    );
    engine.ready();
    let socket = root_path.join("engine.sock");
    let ordinary = Client::new(socket.clone(), Duration::from_secs(2)).unwrap();
    let project_id = ProjectId::new("owner-evidence-project").unwrap();
    let project_name = "Owner Evidence project";
    let created = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("owner-evidence-fixture").unwrap(),
            request_id: RequestId::new("create-project").unwrap(),
            expected_revision: Cursor::new(0),
            command: EngineCommand::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: project_name.into(),
                    repositories: vec![],
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(created.outcome, Outcome::Applied);
    let (seeder, identity) =
        OwnerClient::discover(&root_path, OwnerClientKind::Desktop, Duration::from_secs(2))
            .await
            .unwrap();
    for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
        let (client, observed) = OwnerClient::discover(&root_path, kind, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(observed.database_instance(), identity.database_instance());
        assert_eq!(observed.engine_epoch(), identity.engine_epoch());
        let reply = client
            .request(owner_request(
                "fixture-capabilities",
                &created.revision,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities,
                },
            ))
            .await
            .unwrap();
        assert!(matches!(
            reply.response,
            OwnerResponse::Read { state }
                if matches!(state.data, OwnerData::Capabilities {
                    authenticated_owner_transport: true, ..
                })
        ));
    }
    let marker = format!(
        "owner-detail-{}",
        root_path.file_name().unwrap().to_str().unwrap()
    );
    let draft = baseline(project_id.clone(), &marker);
    let baseline_id = draft.identity().unwrap();
    let proposal = seeder
        .request(owner_request(
            "propose-baseline",
            &created.revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProposeBaseline {
                    draft: draft.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = proposal.response else {
        panic!("fixture prerequisite: actual authenticated baseline proposal was not applied")
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(!operation.historical_replay);
    assert_eq!(operation.provenance.engine_epoch, identity.engine_epoch());
    let (observer, _) =
        OwnerClient::discover(&root_path, OwnerClientKind::Desktop, Duration::from_secs(2))
            .await
            .unwrap();
    let page = observer
        .request(owner_request(
            "observe-baselines",
            &operation.current_revision,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Baselines,
                    cursor: None,
                    limit: 32,
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = page.response else {
        panic!("fixture prerequisite: Desktop baseline page unavailable")
    };
    assert_eq!(state.revision, operation.current_revision);
    let OwnerData::Page { items, next } = state.data else {
        panic!("fixture prerequisite: actual baseline page required")
    };
    assert!(next.is_none());
    assert_eq!(items.len(), 1);
    assert!(matches!(
        &items[0],
        OwnerSummary::Baseline { baseline_id: found, project_id: project, approved: false,
            revoked: false, .. } if found == &baseline_id && project == &project_id
    ));
    let detail = observer
        .request(owner_request(
            "observe-baseline-detail",
            &operation.current_revision,
            OwnerOperation::Read {
                query: OwnerRead::Baseline {
                    baseline_id: baseline_id.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = detail.response else {
        panic!("fixture prerequisite: Desktop baseline detail unavailable")
    };
    assert_eq!(state.revision, operation.current_revision);
    let OwnerData::Baseline {
        draft: observed,
        approved,
        revoked,
        ..
    } = state.data
    else {
        panic!("fixture prerequisite: actual baseline detail required")
    };
    assert_eq!(observed, draft);
    assert_eq!(observed.identity().unwrap(), baseline_id);
    assert_eq!(observed.requirements, marker);
    assert!(!approved && !revoked);
    let mut driver = Process::spawn(
        Command::new("python3")
            .env_remove("PYTHONOPTIMIZE")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qualification/tui_owner_flow.py"
            ))
            .arg(binary)
            .arg(&socket)
            .arg(project_name)
            .arg(&marker)
            .arg(baseline_id.as_str()),
    );
    let status = driver.wait(Duration::from_secs(45));
    assert!(status.success(), "{}", driver.diagnostics());
    assert!(engine.child.try_wait().unwrap().is_none());
}
