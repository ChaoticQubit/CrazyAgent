use crazyagent::conversation::{
    ConversationContent, ConversationEntry, ConversationMutation, ConversationPage,
};
use crazyagent::engine::Client;
use crazyagent::engine::owner::{
    OwnerApplied, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerRequest, OwnerResponse, OwnerResult,
};
use crazyagent::protocol::{
    ClientId, Command as EngineCommand, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec,
    RequestId, WireRequest,
};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const INITIAL_BRIEF: &str = "Retained owner brief version one";
const EXTERNAL_BRIEF: &str = "Independent owner version two";

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
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        eprintln!("owned child started pid={}", child.id());
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
            "pid={}\nstdout: {}\nstderr: {}",
            self.child.id(),
            String::from_utf8_lossy(&self.stdout.lock().unwrap().bytes),
            String::from_utf8_lossy(&self.stderr.lock().unwrap().bytes),
        )
    }

    fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let output = self.stdout.lock().unwrap();
                assert!(!output.overflow, "engine ready output exceeded 64 KiB");
                if let Some(end) = output.bytes.iter().position(|byte| *byte == b'\n') {
                    assert_eq!(&output.bytes[..=end], b"ready\n");
                    return;
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "fixture engine exited before ready: {}",
                self.diagnostics()
            );
            assert!(
                Instant::now() < deadline,
                "fixture engine readiness deadline: {}",
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
            assert!(!self.stdout.lock().unwrap().overflow);
            assert!(!self.stderr.lock().unwrap().overflow);
            std::thread::sleep(Duration::from_millis(10));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        assert!(
            !self.stdout.lock().unwrap().overflow,
            "stdout exceeded 64 KiB"
        );
        assert!(
            !self.stderr.lock().unwrap().overflow,
            "stderr exceeded 64 KiB"
        );
        eprintln!("owned child reaped pid={} status={status}", self.child.id());
        status
    }

    fn stop(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "engine exited unexpectedly"
        );
        assert!(
            Command::new("/bin/kill")
                .args(["-s", "TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let status = self.wait(Duration::from_secs(10));
        assert!(
            status.success(),
            "engine graceful shutdown failed: {}",
            self.diagnostics()
        );
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = Command::new("/bin/kill")
                .args(["-s", "TERM", &self.child.id().to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
        }
        let status = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        eprintln!(
            "owned child cleanup pid={} status={status:?}",
            self.child.id()
        );
        if std::thread::panicking() {
            eprintln!("{}", self.diagnostics());
        }
    }
}

fn start_engine(root: &Path) -> Process {
    let mut engine = Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock"),
    );
    engine.ready();
    engine
}

fn owner_request(id: &str, revision: Cursor, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: revision,
        operation,
    }
}

struct Fixture {
    engine: Process,
    ordinary: Client,
    observer: OwnerClient,
    project: ProjectId,
    other: ProjectId,
    seed: ConversationEntry,
    path: PathBuf,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::fs::canonicalize(root.path()).unwrap();
        let engine = start_engine(&path);
        let ordinary = Client::new(path.join("engine.sock"), Duration::from_secs(2)).unwrap();
        let project = ProjectId::new("a-conversation-project").unwrap();
        let other = ProjectId::new("b-isolated-project").unwrap();
        let mut revision = Cursor::new(0);
        for id in [&project, &other] {
            let reply = ordinary
                .request(WireRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_id: ClientId::new("tui-conversation-fixture").unwrap(),
                    request_id: RequestId::new(id.as_str()).unwrap(),
                    expected_revision: revision,
                    command: EngineCommand::CreateProject {
                        project_id: id.clone(),
                        spec: ProjectSpec {
                            name: id.as_str().into(),
                            repositories: vec![],
                        },
                    },
                })
                .await
                .unwrap();
            assert_eq!(reply.outcome, Outcome::Applied);
            revision = reply.revision;
        }
        let (seeder, _) =
            OwnerClient::discover(&path, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .unwrap();
        let seeded = seeder
            .request(owner_request(
                "seed-explicit-brief",
                revision,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::Conversation {
                        mutation: ConversationMutation::ReplaceBrief {
                            project_id: project.clone(),
                            expected_version: 0,
                            text: INITIAL_BRIEF.into(),
                        },
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = seeded.response else {
            panic!("fixture prerequisite: actual authenticated brief not applied");
        };
        assert_eq!(operation.result, OwnerResult::Applied);
        assert!(!operation.historical_replay);
        let (observer, identity) =
            OwnerClient::discover(&path, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .unwrap();
        let observed = observer
            .request(owner_request(
                "observe-seeded-brief",
                Cursor::new(0),
                OwnerOperation::Read {
                    query: OwnerRead::CurrentBrief {
                        project_id: project.clone(),
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = observed.response else {
            panic!("fixture prerequisite: independent actual brief read unavailable");
        };
        let OwnerData::CurrentBrief { project_id, brief } = state.data else {
            panic!("fixture prerequisite: CurrentBrief data missing");
        };
        assert_eq!(project_id, project);
        let seed = brief.expect("fixture prerequisite: explicit brief absent");
        assert_eq!(seed.text, INITIAL_BRIEF);
        assert_eq!(seed.content, ConversationContent::Brief { version: 1 });
        assert_eq!(seed.provenance, operation.provenance);
        assert_eq!(seed.provenance.engine_epoch, identity.engine_epoch());
        Self {
            engine,
            ordinary,
            observer,
            project,
            other,
            seed,
            path,
            _root: root,
        }
    }

    fn marker(&self, kind: &str) -> String {
        format!(
            "{kind}-{}",
            self.path.file_name().unwrap().to_str().unwrap()
        )
    }

    async fn read(&self, query: OwnerRead) -> OwnerData {
        let reply = self
            .observer
            .request(owner_request(
                "independent-observation",
                Cursor::new(0),
                OwnerOperation::Read { query },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("independent real owner read failed: {reply:?}");
        };
        state.data
    }

    async fn history(&self, project: &ProjectId) -> ConversationPage {
        let OwnerData::ConversationHistory { page } = self
            .read(OwnerRead::ConversationHistory {
                project_id: project.clone(),
                cursor: None,
                limit: 32,
            })
            .await
        else {
            panic!("independent real history data missing");
        };
        assert_eq!(&page.project_id, project);
        assert!(
            page.next.is_none(),
            "small fixture must fit one actual page"
        );
        page
    }

    async fn current_brief(&self) -> ConversationEntry {
        let OwnerData::CurrentBrief { project_id, brief } = self
            .read(OwnerRead::CurrentBrief {
                project_id: self.project.clone(),
            })
            .await
        else {
            panic!("independent real CurrentBrief data missing");
        };
        assert_eq!(project_id, self.project);
        brief.expect("actual project brief disappeared")
    }

    fn driver(&self, case: &str, marker: &str) -> Process {
        Process::spawn(
            Command::new("python3")
                .env_remove("PYTHONOPTIMIZE")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/qualification/tui_project_conversation.py"
                ))
                .arg(case)
                .arg(env!("CARGO_BIN_EXE_crazyagent"))
                .arg(self.path.join("engine.sock"))
                .arg(self.path.join("engine.db"))
                .arg(self.project.as_str())
                .arg(INITIAL_BRIEF)
                .arg(marker),
        )
    }

    fn finish_driver(&mut self, driver: &mut Process) {
        let status = driver.wait(Duration::from_secs(35));
        assert!(
            status.success(),
            "actual conversation PTY driver failed: {}",
            driver.diagnostics()
        );
        assert!(self.engine.child.try_wait().unwrap().is_none());
    }

    async fn assert_no_other_effects(&self) {
        assert!(self.history(&self.other).await.entries.is_empty());
        let reply = self
            .ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("tui-conversation-fixture").unwrap(),
                request_id: RequestId::new("observe-no-auto-task").unwrap(),
                expected_revision: Cursor::new(0),
                command: EngineCommand::Snapshot,
            })
            .await
            .unwrap();
        let Outcome::Snapshot { projects, tasks } = reply.outcome else {
            panic!("actual ordinary Snapshot unavailable");
        };
        assert_eq!(projects.len(), 2);
        assert!(
            tasks.is_empty(),
            "owner input must not start task execution"
        );
    }

    async fn restart_and_observe(&mut self, expected: &ConversationPage) {
        let (_, before) =
            OwnerClient::discover(&self.path, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .unwrap();
        let brief = self.current_brief().await;
        self.engine.stop();
        self.engine = start_engine(&self.path);
        let (observer, after) =
            OwnerClient::discover(&self.path, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .unwrap();
        assert_eq!(before.database_instance(), after.database_instance());
        assert_ne!(before.engine_epoch(), after.engine_epoch());
        self.observer = observer;
        assert_eq!(self.history(&self.project).await, *expected);
        assert_eq!(self.current_brief().await, brief);
        self.assert_no_other_effects().await;
    }

    async fn external_brief(&self) -> OwnerApplied {
        let (client, _) =
            OwnerClient::discover(&self.path, OwnerClientKind::Desktop, Duration::from_secs(2))
                .await
                .unwrap();
        let observed = client
            .request(owner_request(
                "external-current-brief",
                Cursor::new(0),
                OwnerOperation::Read {
                    query: OwnerRead::CurrentBrief {
                        project_id: self.project.clone(),
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = observed.response else {
            panic!("external owner real read prerequisite failed");
        };
        let reply = client
            .request(owner_request(
                "external-explicit-v2",
                state.revision,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::Conversation {
                        mutation: ConversationMutation::ReplaceBrief {
                            project_id: self.project.clone(),
                            expected_version: 1,
                            text: EXTERNAL_BRIEF.into(),
                        },
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!("external owner actual brief mutation failed");
        };
        assert_eq!(operation.result, OwnerResult::Applied);
        operation
    }

    async fn append(&self, index: usize, text: String) -> OwnerApplied {
        let read = self
            .observer
            .request(owner_request(
                "page-seed-revision",
                Cursor::new(0),
                OwnerOperation::Read {
                    query: OwnerRead::CurrentBrief {
                        project_id: self.project.clone(),
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = read.response else {
            panic!("actual page seed revision unavailable");
        };
        let reply = self
            .observer
            .request(owner_request(
                &format!("page-seed-{index}"),
                state.revision,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::Conversation {
                        mutation: ConversationMutation::AppendMessage {
                            project_id: self.project.clone(),
                            text,
                        },
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!("actual authenticated page seed not applied");
        };
        assert_eq!(operation.result, OwnerResult::Applied);
        operation
    }
}

#[tokio::test]
async fn real_tui_persists_owner_message_without_rewriting_brief() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("owner-message");
    let mut driver = fixture.driver("message", &marker);
    fixture.finish_driver(&mut driver);
    let history = fixture.history(&fixture.project).await;
    assert_eq!(
        history.entries.len(),
        2,
        "exactly one actual owner message required"
    );
    assert_eq!(history.entries[0], fixture.seed);
    let message = &history.entries[1];
    assert_eq!(message.content, ConversationContent::Message);
    assert_eq!(message.text, marker);
    assert_eq!(message.provenance.origin, "native_owner");
    assert_ne!(
        message.provenance.owner_id,
        fixture.seed.provenance.owner_id
    );
    assert_eq!(
        message.provenance.event_cursor.as_ref(),
        Some(&message.event_cursor)
    );
    assert_eq!(fixture.current_brief().await, fixture.seed);
    fixture.assert_no_other_effects().await;
    fixture.restart_and_observe(&history).await;
}

#[tokio::test]
async fn real_tui_explicitly_replaces_expected_version_brief() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("explicit-brief");
    let mut driver = fixture.driver("brief", &marker);
    fixture.finish_driver(&mut driver);
    let history = fixture.history(&fixture.project).await;
    assert_eq!(
        history.entries.len(),
        2,
        "brief v1 history must survive explicit edit"
    );
    assert_eq!(history.entries[0], fixture.seed);
    let brief = &history.entries[1];
    assert_eq!(brief.content, ConversationContent::Brief { version: 2 });
    assert_eq!(brief.text, marker);
    assert_eq!(brief.provenance.origin, "native_owner");
    assert_ne!(brief.provenance.owner_id, fixture.seed.provenance.owner_id);
    assert_eq!(
        brief.provenance.event_cursor.as_ref(),
        Some(&brief.event_cursor)
    );
    assert_eq!(fixture.current_brief().await, *brief);
    fixture.assert_no_other_effects().await;
    fixture.restart_and_observe(&history).await;
}

#[tokio::test]
async fn real_tui_keeps_explicit_edit_version_when_another_owner_replaces_brief() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("stale-draft");
    let mut driver = fixture.driver("conflict", &marker);
    let ready = fixture.path.join("brief-edit-ready");
    let deadline = Instant::now() + Duration::from_secs(12);
    while !ready.exists() {
        assert!(
            driver.child.try_wait().unwrap().is_none(),
            "driver exited before real explicit edit: {}",
            driver.diagnostics()
        );
        assert!(
            Instant::now() < deadline,
            "driver did not reach explicit edit: {}",
            driver.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let external = fixture.external_brief().await;
    let receipt = fixture.path.join("external-brief-receipt.json");
    let temporary = fixture.path.join("external-brief-receipt.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&external).unwrap()).unwrap();
    std::fs::rename(&temporary, &receipt).unwrap();
    fixture.finish_driver(&mut driver);
    let history = fixture.history(&fixture.project).await;
    assert_eq!(
        history.entries.len(),
        2,
        "stale edit must add no conversation entry"
    );
    assert_eq!(history.entries[0], fixture.seed);
    let current = fixture.current_brief().await;
    assert_eq!(current.text, EXTERNAL_BRIEF);
    assert_eq!(current.content, ConversationContent::Brief { version: 2 });
    assert_eq!(current.provenance, external.provenance);
    assert_eq!(history.entries[1], current);
    assert!(history.entries.iter().all(|entry| entry.text != marker));
    fixture.assert_no_other_effects().await;
    fixture.restart_and_observe(&history).await;
}

#[tokio::test]
async fn real_tui_isolates_project_message_and_explicit_brief_drafts() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("isolated");
    let mut driver = fixture.driver("isolation", &marker);
    fixture.finish_driver(&mut driver);
    let first = fixture.history(&fixture.project).await;
    let second = fixture.history(&fixture.other).await;
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.entries[0], fixture.seed);
    assert_eq!(first.entries[1].text, format!("{marker}-A"));
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].text, format!("{marker}-B"));
    assert_eq!(second.entries[0].content, ConversationContent::Message);
    assert_eq!(first.entries[1].content, ConversationContent::Message);
    assert_eq!(fixture.current_brief().await, fixture.seed);
    fixture.engine.stop();
    fixture.engine = start_engine(&fixture.path);
    fixture.observer = OwnerClient::discover(
        &fixture.path,
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(fixture.history(&fixture.project).await, first);
    assert_eq!(fixture.history(&fixture.other).await, second);
}

#[tokio::test]
async fn real_tui_reconciles_sqlite_contention_with_exact_intent_and_preserves_newer_text() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("contended");
    let mut driver = fixture.driver("contention", &marker);
    fixture.finish_driver(&mut driver);
    let observed: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.path.join("actual-contention-operation.json")).unwrap(),
    )
    .unwrap();
    let history = fixture.history(&fixture.project).await;
    assert_eq!(history.entries.len(), 2);
    assert_eq!(history.entries[0], fixture.seed);
    let message = &history.entries[1];
    assert_eq!(message.text, marker);
    assert_eq!(message.content, ConversationContent::Message);
    assert_eq!(
        message.provenance.operation_id.as_str(),
        observed["operation_id"].as_str().unwrap()
    );
    assert_eq!(
        message.provenance.request_hash,
        observed["request_hash"].as_str().unwrap()
    );
    assert_eq!(
        message.event_cursor.value(),
        observed["event_cursor"].as_u64().unwrap()
    );
    assert_eq!(observed["observed_stage"], "not_recorded_then_exact_retry");
    assert_eq!(fixture.current_brief().await, fixture.seed);
    fixture.assert_no_other_effects().await;
    fixture.restart_and_observe(&history).await;
}

#[tokio::test]
async fn real_tui_reads_actual_bounded_history_pages_and_sanitizes_terminal_text() {
    let mut fixture = Fixture::start().await;
    let marker = fixture.marker("paged");
    let mut last = None;
    for index in 0..33 {
        let text = format!(
            "page-{index:02}-{marker}{}",
            if index == 32 { "\u{1b}[2J\u{7}" } else { "" }
        );
        last = Some(fixture.append(index, text).await);
    }
    let last = last.unwrap();
    let mut driver = fixture.driver("pages", &marker);
    fixture.finish_driver(&mut driver);
    let mut cursor = None;
    let mut entries = Vec::new();
    for expected in [32, 2] {
        let OwnerData::ConversationHistory { page } = fixture
            .read(OwnerRead::ConversationHistory {
                project_id: fixture.project.clone(),
                cursor,
                limit: 32,
            })
            .await
        else {
            panic!("actual protected page missing");
        };
        assert_eq!(page.entries.len(), expected);
        entries.extend(page.entries);
        cursor = page.next;
    }
    assert!(cursor.is_none());
    assert_eq!(entries.len(), 34);
    assert_eq!(entries[0], fixture.seed);
    assert_eq!(entries.last().unwrap().provenance, last.provenance);
    assert!(entries.last().unwrap().text.ends_with("\u{1b}[2J\u{7}"));
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].event_cursor.value() < pair[1].event_cursor.value())
    );
    assert_eq!(fixture.current_brief().await, fixture.seed);
    fixture.assert_no_other_effects().await;
}
