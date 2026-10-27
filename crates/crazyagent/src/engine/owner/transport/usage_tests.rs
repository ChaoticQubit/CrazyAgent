use super::*;
use crate::engine::bootstrap::EngineService;
use crate::usage::report::LIMITS_MAX;
use crate::usage::report::tests::fill_past_one_frame;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const OWNER_WAIT_MS: u64 = 5000;
const IN_FLIGHT: Duration = Duration::from_millis(300);
const AT_MS: i64 = 1_800_000_000_000;

/// A real engine with an owner client and the folder of its usage store.
struct Usage {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
    engine_root: PathBuf,
}

impl Usage {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-usage-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("version = 1\ntimeout_ms = {OWNER_WAIT_MS}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        let engine_root =
            super::changes::engine_root(&service.owner_runtime().unwrap().authority).unwrap();
        Self {
            root,
            service,
            _stop: stop,
            client,
            engine_root,
        }
    }

    fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    /// The handshake packet the owner client sends. Output: the packet.
    fn packet(&self) -> [u8; 37] {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&self.client.token);
        packet
    }

    async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offset_at_the_integer_edge_is_refused_over_the_owner_socket() {
    let usage = Usage::start().await;
    for offset in [i32::MIN, i32::MAX] {
        assert_eq!(
            usage
                .client
                .usage_report(None, None, offset)
                .await
                .unwrap_err()
                .0,
            "usage_request_invalid",
            "offset {offset}"
        );
    }
    assert!(usage.client.usage_report(None, None, -840).await.is_ok());
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_report_past_one_frame_is_trimmed_and_answered() {
    let usage = Usage::start().await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    fill_past_one_frame(&db, crate::team::now_ms());
    drop(db);

    let report = usage
        .client
        .usage_report(None, None, 0)
        .await
        .unwrap_or_else(|error| panic!("the full usage report answered {}", error.0));
    assert!(
        !report.limits.is_empty() && report.limits.len() < LIMITS_MAX,
        "{} limits",
        report.limits.len()
    );
    assert!(!report.projects.is_empty() && !report.days.is_empty());
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_change_holds_the_owner_barrier_until_it_is_committed() {
    let usage = Usage::start().await;
    let held = crate::usage::open(&usage.engine_root).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let account = CredentialRef::new("held-account").unwrap();
    let mut change = Box::pin(usage.client.usage_set_reset(
        account.clone(),
        crate::usage::WindowKind::FiveHour,
        Some(AT_MS),
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut change).await.is_err(),
        "the change waits behind the held write"
    );
    assert!(
        usage.runtime().engine.owner_egress.try_write().is_err(),
        "the change holds the owner barrier while it waits to commit"
    );
    held.execute_batch("ROLLBACK").unwrap();
    change
        .await
        .unwrap_or_else(|error| panic!("the held change answered {}", error.0));
    let stored = crate::usage::limits(&held, &account).unwrap().unwrap();
    assert_eq!(stored.five_hour_reset_override_ms, Some(AT_MS));
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_waits_for_a_usage_reply_in_flight_and_refuses_the_next() {
    let usage = Usage::start().await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    fill_past_one_frame(&db, crate::team::now_ms());
    drop(db);
    let runtime = usage.runtime();
    let packet = usage.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (server, client) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&server, &client] {
        end.set_nonblocking(true).unwrap();
    }
    let mut server = UnixStream::from_std(server).unwrap();
    let mut client = UnixStream::from_std(client).unwrap();
    nix::sys::socket::setsockopt(&server, nix::sys::socket::sockopt::SndBuf, &1024usize).unwrap();
    nix::sys::socket::setsockopt(&client, nix::sys::socket::sockopt::RcvBuf, &1024usize).unwrap();
    let body = serde_json::to_vec(&json!({
        "usage": { "kind": "report", "since_ms": null, "offset_minutes": 0 }
    }))
    .unwrap();
    let (engine, authority) = (runtime.engine.clone(), runtime.authority.clone());
    let serving = tokio::spawn(async move {
        super::usage::serve(&mut server, &engine, &authority, &packet, &admission, &body).await
    });

    let size = tokio::time::timeout(Duration::from_secs(10), client.read_u32())
        .await
        .unwrap()
        .unwrap() as usize;
    assert!(size > 16384 && size <= FRAME_BYTES_MAX, "{size}");
    tokio::time::sleep(IN_FLIGHT).await;
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the usage reply is written under the owner barrier"
    );
    let mut revoke = Box::pin(runtime.control(OwnerControl {
        operation_id: RequestId::new("revoke-during-usage").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
        decision: OwnerDecision::Revoke,
    }));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "a revocation waits for the usage reply already in flight"
    );
    let mut reply = vec![0; size];
    client.read_exact(&mut reply).await.unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(
        reply["kind"], "report",
        "the reply admitted before the revocation"
    );
    serving.await.unwrap().unwrap();
    assert!(!revoke.await.unwrap().active);

    assert!(
        usage.client.usage_report(None, None, 0).await.is_err(),
        "the revoked client reads no more usage"
    );
    usage.stop().await;
}
