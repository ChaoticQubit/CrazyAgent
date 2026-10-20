use super::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl, NativeAuthCustodyLaunch,
};
use super::*;
use crate::engine::bootstrap::{EngineService, NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::native_auth_host::custody::CustodySession;
use crate::engine::owner::native_auth::Command;
use crate::storage::{NativeAuthBegin, NativeAuthWork};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::task::{Context, Poll, Waker};

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-alias"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-alias", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a", "connection-alias"]
projects = ["project-a"]
roles = ["leader"]
fallback_allowed = false
"#;

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    facts: Vec<NativeAuthFact>,
    fences: Vec<(String, String, i64, String, String, String, String, i64)>,
}

struct ChannelCase {
    root: PathBuf,
    global: PathBuf,
    config: crate::config::EffectiveConfig,
    source_hash: String,
    host: OwnerRuntimeHandle,
    service: tokio::sync::Mutex<Option<EngineService>>,
}

fn request(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn snapshot(path: &Path) -> Snapshot {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let facts = db
        .prepare("SELECT fact FROM native_auth_operations ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|row| crate::engine::owner::native_auth::decode_fact(&row.unwrap()).unwrap())
        .collect();
    let fences = db
        .prepare(
            "SELECT profile_key,operation_id,generation,db_instance,engine_epoch,custody_id,
             owner_id,owner_epoch FROM native_auth_fences ORDER BY profile_key",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    Snapshot { facts, fences }
}

async fn channel_case<F, T>(case: F)
where
    F: FnOnce(Arc<ChannelCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::Builder::new()
        .prefix("ca-can-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let project = root.path().join("p.toml");
    let config = crate::config::resolve_config(&global, &project).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &project,
        &stopped,
    )
    .await
    .unwrap();
    let handle = service.owner_runtime().unwrap();
    let client = OwnerClient::attach(
        &handle.authority.root.path,
        OwnerClientKind::Tui,
        Duration::from_secs(2),
    )
    .unwrap();
    let response = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: request("source-for-channel-fixture"),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Connections,
                    id: CredentialRef::new("connection-a").unwrap(),
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = response.response else {
        panic!("actual owner did not return model access source")
    };
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
        panic!("actual owner did not return connection source")
    };
    let context = Arc::new(ChannelCase {
        root: root.path().to_owned(),
        global,
        config,
        source_hash,
        host: OwnerRuntimeHandle {
            engine: handle.engine.clone(),
            authority: handle.authority.clone(),
        },
        service: tokio::sync::Mutex::new(Some(service)),
    });
    let mut task = tokio::spawn(case(context.clone()));
    let (outcome, timed_out) = match tokio::time::timeout(Duration::from_secs(30), &mut task).await
    {
        Ok(outcome) => (outcome, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        context
            .service
            .lock()
            .await
            .take()
            .unwrap()
            .shutdown()
            .await
    })
    .await;
    if timed_out || outcome.is_err() || !matches!(&cleanup, Ok(Ok(()))) {
        eprintln!(
            "native auth channel evidence retained at {}",
            root.keep().display()
        );
    }
    assert!(
        matches!(&cleanup, Ok(Ok(()))),
        "channel cleanup: {cleanup:?}"
    );
    assert!(!timed_out, "native auth channel case exceeded 30 seconds");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("native auth channel task: {error}");
    }
}

impl ChannelCase {
    fn client(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::attach(&self.host.authority.root.path, kind, Duration::from_secs(2)).unwrap()
    }

    fn start(&self, id: &str) -> NativeAuthStart {
        NativeAuthStart {
            request_id: request(id),
            scope: NativeAuthScope {
                expected_database_instance: self.host.identity().db_instance,
                connection: CredentialRef::new("connection-a").unwrap(),
                expected_config_id: self.config.id.clone(),
                expected_source_hash: self.source_hash.clone(),
            },
            method: NativeAuthMethod::CodexBrowser,
        }
    }

    fn admission(&self) -> Admission {
        let client = self.client(OwnerClientKind::Tui);
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&client.token);
        self.host.authority.authenticate(&packet).unwrap()
    }

    async fn begin(&self, command: Command) -> NativeAuthBegin {
        self.host
            .engine
            .native_auth_job(|reply| Job::NativeAuth(self.admission(), command, reply))
            .await
            .unwrap()
    }

    async fn work(&self, id: &str) -> (NativeAuthFact, NativeAuthWork) {
        let begun = self.begin(Command::Start(self.start(id))).await;
        assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
        assert!(begun.reply.challenge.is_none());
        assert!(begun.reply.account.is_none());
        (begun.reply.fact, begun.work.unwrap())
    }

    async fn launch(&self, id: &str) -> (NativeAuthFact, NativeAuthCustodyLaunch) {
        let (fact, work) = self.work(id).await;
        let launch = self.host.prepare_native_auth_custody(work).await.unwrap();
        (fact, launch)
    }

    fn token(&self) -> [u8; 32] {
        self.host
            .authority
            .native_auth
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .token
    }

    fn packet(&self) -> [u8; 37] {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAN1");
        packet[5..].copy_from_slice(&self.token());
        packet
    }

    fn snapshot(&self) -> Snapshot {
        snapshot(&self.root.join("engine.db"))
    }

    fn manifest(&self) -> PathBuf {
        self.root
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(b"profile-a")))
            .join("identity.json")
    }

    fn guard(&self, fact: &NativeAuthFact, purpose: NativeProfileUsePurpose) -> NativeProfileUse {
        let (_, _, binding) = crate::engine::bootstrap::runner_binding(&self.root).unwrap();
        let mut custody = binding.capture_native_profile_custody().unwrap();
        custody
            .acquire_use(
                &self.host.identity(),
                &fact.binding,
                &fact.operation_id,
                fact.generation,
                purpose,
            )
            .unwrap()
    }

    fn probe(&self) -> File {
        File::from(
            rustix::fs::open(
                self.manifest(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap(),
        )
    }

    async fn channel(&self) -> NativeAuthCustodyChannel {
        NativeAuthCustodyChannel::connect(&self.root, self.token())
            .await
            .unwrap()
    }

    async fn raw(&self, packet: &[u8; 37]) -> UnixStream {
        let mut stream = UnixStream::connect(self.host.authority.root.path.join(SOCKET))
            .await
            .unwrap();
        stream.write_all(packet).await.unwrap();
        stream
    }

    async fn attached(&self) -> UnixStream {
        let mut stream = self.raw(&self.packet()).await;
        stream
            .write_all(&encode_frame(&json!({"protocol_version": 1, "kind": "attach"})).unwrap())
            .await
            .unwrap();
        let frame = read_frame(&mut stream).await.unwrap();
        let grant: Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(grant["kind"], "grant");
        assert_eq!(grant["protocol_version"], 1);
        assert_eq!(
            grant["fact"],
            serde_json::to_value(&self.snapshot().facts[0]).unwrap()
        );
        stream
    }

    fn query(&self, fact: &NativeAuthFact, id: &str) -> NativeAuthQuery {
        NativeAuthQuery {
            request_id: request(id),
            scope: self.start("query-scope").scope,
            operation_id: fact.operation_id.clone(),
        }
    }
}

async fn rejected(mut stream: UnixStream) {
    let mut byte = [0; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
        .await
        .unwrap();
    match read {
        Ok(size) => assert_eq!(size, 0, "rejected role/frame received reply bytes"),
        Err(error) => assert!(matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
        )),
    }
}

fn check_frame(fact: &NativeAuthFact, seq: u64) -> Value {
    json!({
        "protocol_version": 1,
        "operation_id": fact.operation_id,
        "generation": fact.generation,
        "seq": seq,
        "kind": "check"
    })
}

async fn capabilities(client: &OwnerClient, id: &str) {
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: request(id),
            operation: OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("actual owner did not return capabilities")
    };
    assert!(matches!(
        state.data,
        OwnerData::Capabilities {
            authenticated_owner_transport: true,
            ..
        }
    ));
}

fn read_barrier(path: &Path) -> Connection {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.execute_batch("BEGIN").unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM native_auth_fences", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    db
}

fn pending<F: Future>(future: Pin<&mut F>) {
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.poll(&mut context), Poll::Pending));
}

async fn retirement_write_started<F: Future>(case: &ChannelCase, future: &mut Pin<Box<F>>) {
    let journal = case.root.join("engine.db-journal");
    assert!(!journal.exists());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            pending(future.as_mut());
            if journal.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn reap(child: &mut Child) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = child.kill();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            panic!("inert child exceeded its two-second completion bound");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("inert child {} did not reap after kill", child.id());
}

#[tokio::test]
async fn native_auth_channel_requires_committed_work_and_exact_origin() {
    trait NonClone<A> {
        fn evidence() {}
    }
    impl<T: ?Sized> NonClone<()> for T {}
    impl<T: ?Sized + Clone> NonClone<u8> for T {}
    let _ = <NativeAuthCustodyLaunch as NonClone<_>>::evidence;
    let _ = <NativeAuthCustodyChannel as NonClone<_>>::evidence;
    channel_case(|case| async move {
        assert!(case.host.authority.native_auth.lock().unwrap().is_none());
        let (fact, launch) = case.launch("committed-channel").await;
        let before = case.snapshot();
        assert_eq!(before.facts, vec![fact.clone()]);
        assert_eq!(before.fences.len(), 1);
        assert_eq!(before.fences[0].6, case.admission().owner_id);
        assert_eq!(before.fences[0].7, case.admission().owner_epoch);
        let mut channel = case.channel().await;
        assert_eq!(channel.fact(), &fact);
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        let control = channel.revalidate(&guard).await.unwrap();
        assert!(matches!(control, NativeAuthCustodyControl::Continue));
        assert_eq!(case.snapshot(), before);
        let replay = case
            .begin(Command::Start(case.start("committed-channel")))
            .await;
        assert!(replay.work.is_none());
        assert_eq!(replay.reply.fact, fact);
        drop((guard, channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_bootstrap_writes_exact_token_length_and_closes_child_stdin() {
    channel_case(|case| async move {
        let (_, launch) = case.launch("bootstrap-token-pipe").await;
        let before = case.snapshot();
        let mut child = ProcessCommand::new("/usr/bin/wc")
            .arg("-c")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let delivered = launch.write_bootstrap(child.stdin.take().unwrap());
        reap(&mut child);
        delivered.unwrap();
        let mut count = String::new();
        std::io::Read::read_to_string(
            &mut std::io::Read::take(child.stdout.take().unwrap(), 16),
            &mut count,
        )
        .unwrap();
        assert_eq!(count.trim(), "32");
        assert_eq!(case.snapshot(), before);
        let replay = case
            .begin(Command::Start(case.start("bootstrap-token-pipe")))
            .await;
        assert!(replay.work.is_none());
        assert_eq!(case.snapshot(), before);
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_rejects_owner_runner_and_reserved_capability_roles() {
    for role in [
        "tui",
        "desktop",
        "as-tui",
        "as-desktop",
        "as-runner",
        "reserved",
        "bad-token",
        "magic",
    ] {
        channel_case(move |case| async move {
            let (_, launch) = case.launch("separate-custody-role").await;
            let before = case.snapshot();
            let mut packet = case.packet();
            match role {
                "tui" | "desktop" => {
                    let kind = if role == "tui" {
                        OwnerClientKind::Tui
                    } else {
                        OwnerClientKind::Desktop
                    };
                    packet[5..].copy_from_slice(&case.client(kind).token);
                }
                "as-tui" | "as-desktop" => {
                    packet[..4].copy_from_slice(b"CAO1");
                    packet[4] = u8::from(role == "as-desktop");
                }
                "as-runner" => packet[..4].copy_from_slice(b"CAR1"),
                "reserved" => packet[4] = 1,
                "bad-token" => packet[5] ^= 1,
                "magic" => packet[..4].copy_from_slice(b"CAN2"),
                _ => unreachable!(),
            }
            let mut stream = case.raw(&packet).await;
            let frame = encode_frame(&json!({"protocol_version": 1, "kind": "attach"})).unwrap();
            let _ = stream.write_all(&frame).await;
            rejected(stream).await;
            assert_eq!(case.snapshot(), before);
            drop(launch);
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_consumes_capability_once_and_rejects_second_attach() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("one-capability-attachment").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(matches!(
            channel.revalidate(&guard).await.unwrap(),
            NativeAuthCustodyControl::Continue
        ));
        let mut second = case.raw(&case.packet()).await;
        let _ = second
            .write_all(&encode_frame(&json!({"protocol_version": 1, "kind": "attach"})).unwrap())
            .await;
        rejected(second).await;
        assert!(matches!(
            channel.revalidate(&guard).await.unwrap(),
            NativeAuthCustodyControl::Continue
        ));
        assert_eq!(case.snapshot().fences.len(), 1);
        drop((guard, channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_rejects_stale_owner_and_source_before_token_grant() {
    for change in ["owner", "source"] {
        channel_case(move |case| async move {
            let (_, work) = case.work("stale-before-token").await;
            if change == "owner" {
                case.host
                    .control(OwnerControl {
                        operation_id: request("revoke-before-token"),
                        target: OwnerClientKind::Tui,
                        expected_epoch: case.admission().owner_epoch,
                        decision: OwnerDecision::Revoke,
                    })
                    .await
                    .unwrap();
            } else {
                std::fs::write(&case.global, format!("{CATALOG}\n")).unwrap();
            }
            let before = case.snapshot();
            assert!(case.host.prepare_native_auth_custody(work).await.is_err());
            assert!(case.host.authority.native_auth.lock().unwrap().is_none());
            assert_eq!(case.snapshot(), before);
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_rejects_changed_scope_and_frame_order() {
    for change in [
        "operation",
        "generation",
        "version",
        "zero",
        "skip",
        "overflow",
        "ready",
        "binding",
        "unknown",
    ] {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("closed-check-frame").await;
            let before = case.snapshot();
            let mut stream = case.attached().await;
            let mut frame = check_frame(&fact, 1);
            match change {
                "operation" => frame["operation_id"] = json!("different-operation"),
                "generation" => frame["generation"] = json!(fact.generation + 1),
                "version" => frame["protocol_version"] = json!(2),
                "zero" => frame["seq"] = json!(0),
                "skip" => frame["seq"] = json!(2),
                "overflow" => frame["seq"] = json!(u64::MAX),
                "ready" => frame["kind"] = json!("ready"),
                "binding" => frame["binding"] = json!({"connection": "connection-alias"}),
                "unknown" => frame["authority"] = json!(true),
                _ => unreachable!(),
            }
            stream
                .write_all(&encode_frame(&frame).unwrap())
                .await
                .unwrap();
            rejected(stream).await;
            assert_eq!(case.snapshot().fences, before.fences);
            assert_eq!(case.snapshot().facts[0].generation, fact.generation);
            drop(launch);
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_rejects_repeated_sequence_and_attach_replay() {
    for repeat in ["check", "attach"] {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("sequence-replay").await;
            let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
            let mut stream = case.attached().await;
            stream
                .write_all(&encode_frame(&check_frame(&fact, 1)).unwrap())
                .await
                .unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            let response: Value = serde_json::from_slice(&frame[4..]).unwrap();
            assert_eq!(response["seq"], 1);
            assert_eq!(response["kind"], "continue");
            let replay = if repeat == "check" {
                check_frame(&fact, 1)
            } else {
                json!({"protocol_version": 1, "kind": "attach"})
            };
            stream
                .write_all(&encode_frame(&replay).unwrap())
                .await
                .unwrap();
            rejected(stream).await;
            assert_eq!(case.snapshot().fences.len(), 1);
            drop((guard, launch));
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_rejects_invalid_lengths_truncation_and_attach_fields() {
    for invalid in [
        "zero",
        "oversize",
        "truncated",
        "unknown",
        "version",
        "ready",
    ] {
        channel_case(move |case| async move {
            let (_, launch) = case.launch("bounded-attach").await;
            let before = case.snapshot();
            let mut stream = case.raw(&case.packet()).await;
            match invalid {
                "zero" => stream.write_all(&0_u32.to_be_bytes()).await.unwrap(),
                "oversize" => stream.write_all(&65_537_u32.to_be_bytes()).await.unwrap(),
                "truncated" => {
                    stream.write_all(&20_u32.to_be_bytes()).await.unwrap();
                    stream.write_all(b"{").await.unwrap();
                    stream.shutdown().await.unwrap();
                }
                _ => {
                    let value = match invalid {
                        "unknown" => {
                            json!({"protocol_version": 1, "kind": "attach", "ready": true})
                        }
                        "version" => json!({"protocol_version": 2, "kind": "attach"}),
                        "ready" => json!({"protocol_version": 1, "kind": "ready"}),
                        _ => unreachable!(),
                    };
                    stream
                        .write_all(&encode_frame(&value).unwrap())
                        .await
                        .unwrap();
                }
            }
            rejected(stream).await;
            assert_eq!(case.snapshot().fences, before.fences);
            drop(launch);
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_accepts_the_exact_frame_payload_limit() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("maximum-attach-frame").await;
        let mut stream = case.raw(&case.packet()).await;
        let mut body = br#"{"protocol_version":1,"kind":"attach"}"#.to_vec();
        body.resize(65_536, b' ');
        stream.write_all(&65_536_u32.to_be_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        let frame = read_frame(&mut stream).await.unwrap();
        let grant: Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(grant["kind"], "grant");
        assert_eq!(grant["fact"], serde_json::to_value(&fact).unwrap());
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        stream
            .write_all(&encode_frame(&check_frame(&fact, 1)).unwrap())
            .await
            .unwrap();
        let response = read_frame(&mut stream).await.unwrap();
        let response: Value = serde_json::from_slice(&response[4..]).unwrap();
        assert_eq!(response["kind"], "continue");
        assert_eq!(response["seq"], 1);
        assert_eq!(case.snapshot().fences.len(), 1);
        drop((guard, stream, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_revalidates_real_guard_operation_generation_alias_and_purpose() {
    for mismatch in ["operation", "generation", "alias", "purpose"] {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("guard-scope").await;
            let mut channel = case.channel().await;
            let mut input = fact.clone();
            let mut purpose = NativeProfileUsePurpose::Authentication;
            match mismatch {
                "operation" => input.operation_id = request("different-guard-operation"),
                "generation" => input.generation += 1,
                "alias" => {
                    input.binding.connection = case
                        .config
                        .settings
                        .model_access
                        .as_ref()
                        .unwrap()
                        .connections[1]
                        .clone()
                }
                "purpose" => purpose = NativeProfileUsePurpose::Logout,
                _ => unreachable!(),
            }
            let guard = case.guard(&input, purpose);
            let before = case.snapshot();
            assert!(channel.revalidate(&guard).await.is_err());
            assert_eq!(case.snapshot().fences, before.fences);
            drop((guard, channel, launch));
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_revalidates_actual_descriptor_layout_and_mode() {
    for change in ["descriptor", "mode", "home"] {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("actual-guard-revalidation").await;
            let mut channel = case.channel().await;
            let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
            assert!(channel.revalidate(&guard).await.is_ok());
            let manifest = case.manifest();
            match change {
                "descriptor" => {
                    let bytes = std::fs::read(&manifest).unwrap();
                    std::fs::rename(&manifest, manifest.with_extension("retained")).unwrap();
                    std::fs::write(&manifest, bytes).unwrap();
                    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                "mode" => {
                    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o640))
                        .unwrap()
                }
                "home" => std::fs::set_permissions(
                    manifest.parent().unwrap().join("home"),
                    std::fs::Permissions::from_mode(0o750),
                )
                .unwrap(),
                _ => unreachable!(),
            }
            let before = case.snapshot();
            assert!(channel.revalidate(&guard).await.is_err());
            assert_eq!(case.snapshot().fences, before.fences);
            assert!(CustodySession::new(guard).seal_without_effect().is_err());
            drop((channel, launch));
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_does_not_consume_owner_capacity_or_human_wait_timeout() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("retained-owner-capacity").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let tui = case.client(OwnerClientKind::Tui);
        let desktop = case.client(OwnerClientKind::Desktop);
        tokio::join!(
            capabilities(&tui, "live-tui"),
            capabilities(&desktop, "live-desktop")
        );
        let observed = tui
            .read_native_auth(case.query(&fact, "live-custody-read"))
            .await
            .unwrap();
        assert_eq!(observed.fact, fact);
        assert!(observed.account.is_none());
        assert!(observed.challenge.is_none());
        assert!(matches!(
            channel.revalidate(&guard).await.unwrap(),
            NativeAuthCustodyControl::Continue
        ));
        assert_eq!(case.snapshot().fences.len(), 1);
        drop((guard, channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_cancellation_uses_committed_actor_state_and_keeps_guard() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("cancel-live-custody").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        let client = case.client(OwnerClientKind::Tui);
        let cancelled = client
            .cancel_native_auth(NativeAuthCancel {
                request_id: request("commit-custody-cancel"),
                scope: case.start("cancel-scope").scope,
                operation_id: fact.operation_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelling);
        assert!(matches!(
            channel.revalidate(&guard).await.unwrap(),
            NativeAuthCustodyControl::Cancel
        ));
        let probe = case.probe();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        assert_eq!(case.snapshot().fences.len(), 1);
        drop((guard, channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_source_change_or_owner_revoke_cannot_continue_or_retire() {
    for change in ["source", "owner"] {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("stale-live-custody").await;
            let mut channel = case.channel().await;
            let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
            assert!(channel.revalidate(&guard).await.is_ok());
            if change == "source" {
                std::fs::write(&case.global, format!("{CATALOG}\n")).unwrap();
            } else {
                case.host
                    .control(OwnerControl {
                        operation_id: request("revoke-live-custody"),
                        target: OwnerClientKind::Tui,
                        expected_epoch: case.admission().owner_epoch,
                        decision: OwnerDecision::Revoke,
                    })
                    .await
                    .unwrap();
            }
            let before = case.snapshot();
            assert!(channel.revalidate(&guard).await.is_err());
            let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
            assert!(channel.retire(retirement).await.is_err());
            assert_eq!(case.snapshot().fences, before.fences);
            drop((channel, launch));
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_retirement_rejects_each_changed_durable_scope_join() {
    let updates = [
        "UPDATE native_auth_fences SET generation=generation+1",
        "UPDATE native_auth_fences SET owner_epoch=owner_epoch+1",
        "UPDATE native_auth_fences SET db_instance=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET engine_epoch=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET custody_id=lower(hex(randomblob(32)))",
        "UPDATE native_auth_fences SET profile_key=lower(hex(randomblob(32)))",
    ];
    for sql in updates {
        channel_case(move |case| async move {
            let (fact, launch) = case.launch("retirement-scope-join").await;
            let mut channel = case.channel().await;
            let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
            assert!(channel.revalidate(&guard).await.is_ok());
            let db = Connection::open_with_flags(
                case.root.join("engine.db"),
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap();
            assert_eq!(db.execute(sql, []).unwrap(), 1);
            drop(db);
            let before = case.snapshot();
            let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
            assert!(channel.retire(retirement).await.is_err(), "accepted {sql}");
            assert_eq!(case.snapshot().fences, before.fences);
            assert_ne!(case.snapshot().facts[0].phase, NativeAuthPhase::Unavailable);
            drop((channel, launch));
        })
        .await;
    }
}

#[tokio::test]
async fn native_auth_channel_no_child_retirement_is_atomic_under_retained_physical_guard() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("no-child-retirement").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        let probe = case.probe();
        let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        assert_eq!(case.snapshot().fences.len(), 1);
        let fact = channel.retire(retirement).await.unwrap();
        assert_eq!(fact.phase, NativeAuthPhase::Unavailable);
        let after = case.snapshot();
        assert_eq!(after.facts, vec![fact.clone()]);
        assert!(after.fences.is_empty());
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop(probe);
        let replay = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("no-child-retirement"))
            .await
            .unwrap();
        assert_eq!(replay.fact, fact);
        assert!(replay.challenge.is_none());
        assert!(replay.account.is_none());
        assert_eq!(case.snapshot(), after);
        drop((channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_dropped_retirement_before_commit_preserves_fence() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("retirement-delivery-lost").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        let before = case.snapshot();
        let barrier = read_barrier(&case.root.join("engine.db"));
        let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
        let mut future = Box::pin(channel.retire(retirement));
        retirement_write_started(&case, &mut future).await;
        drop(future);
        drop(channel);
        tokio::time::timeout(Duration::from_secs(2), async {
            while case
                .host
                .authority
                .native_auth
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|slot| slot.session_is_live())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        barrier.execute_batch("ROLLBACK").unwrap();
        let _ = case
            .client(OwnerClientKind::Tui)
            .read_native_auth(case.query(&fact, "retirement-loss-barrier"))
            .await
            .unwrap();
        assert_eq!(case.snapshot().fences, before.fences);
        assert_ne!(case.snapshot().facts[0].phase, NativeAuthPhase::Unavailable);
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_lost_committed_retirement_ack_reconciles_without_second_work() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("committed-retirement-ack-lost").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        let probe = case.probe();
        let barrier = read_barrier(&case.root.join("engine.db"));
        let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
        let mut future = Box::pin(channel.retire(retirement));
        retirement_write_started(&case, &mut future).await;
        barrier.execute_batch("ROLLBACK").unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let observed = case.snapshot();
                if observed.fences.is_empty() {
                    assert_eq!(observed.facts[0].phase, NativeAuthPhase::Unavailable);
                    break;
                }
                assert_eq!(observed.facts[0].phase, NativeAuthPhase::Starting);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        drop(future);
        drop(channel);
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop(probe);
        let replay = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("committed-retirement-ack-lost"))
            .await
            .unwrap();
        assert_eq!(replay.fact.operation_id, fact.operation_id);
        assert_eq!(replay.fact.generation, fact.generation);
        assert_eq!(replay.fact.phase, NativeAuthPhase::Unavailable);
        assert!(case.snapshot().fences.is_empty());
        assert_eq!(case.snapshot().facts.len(), 1);
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_any_real_child_effect_forbids_no_effect_retirement() {
    channel_case(|case| async move {
        let (fact, launch) = case.launch("real-child-is-not-no-effect").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Authentication);
        assert!(channel.revalidate(&guard).await.is_ok());
        let mut session = CustodySession::new(guard);
        let mut child = ProcessCommand::new("/usr/bin/true")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        reap(&mut child);
        session.retain_child(child).unwrap();
        let before = case.snapshot();
        assert!(session.seal_without_effect().is_err());
        drop(channel);
        let client = case.client(OwnerClientKind::Tui);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let observed = client
                    .read_native_auth(case.query(&fact, "effect-loss-read"))
                    .await
                    .unwrap();
                if observed.fact.phase == NativeAuthPhase::Uncertain {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(case.snapshot().fences, before.fences);
        let probe = case.probe();
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop(probe);
        let mut alias = case.start("effect-still-fences-alias");
        alias.scope.connection = CredentialRef::new("connection-alias").unwrap();
        assert!(client.start_native_auth(alias).await.is_err());
        assert_eq!(case.snapshot().fences, before.fences);
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_channel_logout_no_child_retirement_preserves_real_action_and_replay() {
    channel_case(|case| async move {
        let original = NativeAuthLogout {
            request_id: request("logout-custody-retirement"),
            scope: case.start("logout-scope").scope,
        };
        let begun = case.begin(Command::Logout(original.clone())).await;
        let fact = begun.reply.fact;
        assert_eq!(fact.action, NativeAuthAction::Logout);
        let launch = case
            .host
            .prepare_native_auth_custody(begun.work.unwrap())
            .await
            .unwrap();
        let mut channel = case.channel().await;
        let guard = case.guard(&fact, NativeProfileUsePurpose::Logout);
        assert!(channel.revalidate(&guard).await.is_ok());
        let retirement = CustodySession::new(guard).seal_without_effect().unwrap();
        let retired = channel.retire(retirement).await.unwrap();
        assert_eq!(retired.action, NativeAuthAction::Logout);
        assert_eq!(retired.phase, NativeAuthPhase::Unavailable);
        assert!(case.snapshot().fences.is_empty());
        let replay = case
            .client(OwnerClientKind::Tui)
            .logout_native_auth(original)
            .await
            .unwrap();
        assert_eq!(replay.fact, retired);
        assert_eq!(case.snapshot().facts.len(), 1);
        drop((channel, launch));
    })
    .await;
}
