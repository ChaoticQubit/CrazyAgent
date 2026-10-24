#![cfg(unix)]

use crazyagent::protocol::encode_frame;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Service {
    root: tempfile::TempDir,
    process: Child,
}

impl Service {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-r-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let executable = env!("CARGO_BIN_EXE_crazyagent-runner");
        let process = Command::new(executable)
            .arg("serve")
            .arg(root.path().canonicalize().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.path().join("stderr.log")).unwrap())
            .spawn()
            .unwrap();
        let mut service = Self { root, process };
        service.wait_ready();
        service
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                self.process.try_wait().unwrap().is_none(),
                "runner exited before ready: {}",
                fs::read_to_string(self.root.path().join("stderr.log")).unwrap()
            );
            if UnixStream::connect(self.root.path().join("control.sock")).is_ok() {
                let status = self.status();
                assert_eq!(status["kind"], "status");
                assert_eq!(status["identity"]["pid"], self.process.id());
                return;
            }
            assert!(Instant::now() < deadline, "runner readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn exchange(&self, command: Value) -> Value {
        exchange(self.root.path(), command)
    }

    fn status(&self) -> Value {
        self.exchange(json!({"kind":"status"}))
    }

    fn claim(&self, epoch: &str, generation: u64) -> Value {
        self.exchange(
            json!({"kind":"claim", "runner_id":self.status()["identity"]["runner_id"],
            "expected_generation":generation, "controller_epoch":epoch}),
        )
    }

    fn restart(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
        self.process = Command::new(env!("CARGO_BIN_EXE_crazyagent-runner"))
            .arg("serve")
            .arg(self.root.path().canonicalize().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(
                fs::OpenOptions::new()
                    .append(true)
                    .open(self.root.path().join("stderr.log"))
                    .unwrap(),
            )
            .spawn()
            .unwrap();
        self.wait_ready();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.process.try_wait().unwrap().is_none() {
            self.process.kill().unwrap();
        }
        self.process.wait().unwrap();
    }
}

fn exchange(root: &std::path::Path, command: Value) -> Value {
    let mut stream = UnixStream::connect(root.join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .write_all(&encode_frame(&json!({"protocol_version":1,"command":command})).unwrap())
        .unwrap();
    let mut size = [0; 4];
    stream.read_exact(&mut size).unwrap();
    let size = u32::from_be_bytes(size) as usize;
    assert!(size <= crazyagent::protocol::FRAME_BYTES_MAX);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn launch(service: &Service, epoch: &str, id: &str, harness: &str) -> Value {
    service.exchange(
        json!({"kind":"operate", "runner_id":service.status()["identity"]["runner_id"],
        "controller_epoch":epoch, "operation_id":id,
        "action":{"kind":"launch", "session_id":"session", "harness":harness,
            "profile_ref":"unadmitted", "worktree_lease":"lease"}}),
    )
}

#[test]
fn native_launch_is_unavailable_for_every_required_harness() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    for (id, harness) in [("r", "rcli"), ("c", "claude"), ("x", "codex")] {
        let reply = launch(&service, "engine-1", id, harness);
        assert_eq!(reply["kind"], "operation");
        assert_eq!(reply["state"], "unavailable");
        assert_eq!(reply["reason"], "native_runtime_admission_unavailable");
        assert!(reply["native_identity"].is_null());
    }
    assert_eq!(service.status()["native_admission"], "unavailable");
}

#[test]
fn private_control_service_survives_controller_disconnect() {
    let service = Service::start();
    let before = service.status();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let client = UnixStream::connect(service.root.path().join("control.sock")).unwrap();
    drop(client);
    assert_eq!(service.claim("engine-2", 1)["kind"], "claimed");
    let after = service.status();
    assert_eq!(before["identity"], after["identity"]);
    assert_eq!(after["controller_epoch"], "engine-2");
    assert_eq!(after["generation"], 2);
    assert!(after["native_identity"].is_null());
    assert_eq!(
        fs::metadata(service.root.path().join("control.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn replaced_controller_cannot_dispatch_or_reclaim_with_a_stale_generation() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    assert_eq!(service.claim("engine-2", 1)["kind"], "claimed");
    let stale = launch(&service, "engine-1", "stale-operation", "codex");
    assert_eq!(stale["kind"], "error");
    assert_eq!(stale["code"], "stale_controller");
    let stale_claim = service.claim("engine-3", 1);
    assert_eq!(stale_claim["kind"], "error");
    assert_eq!(stale_claim["code"], "stale_generation");
    assert_eq!(service.status()["controller_epoch"], "engine-2");
    assert_eq!(service.status()["operation_count"], 0);
}

#[test]
fn repeated_operation_retains_its_result_and_rejects_changed_action() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let original = launch(&service, "engine-1", "operation", "codex");
    assert_eq!(original["kind"], "operation");
    assert_eq!(original["state"], "unavailable");
    assert_eq!(launch(&service, "engine-1", "operation", "codex"), original);
    let conflict = launch(&service, "engine-1", "operation", "claude");
    assert_eq!(conflict["kind"], "error");
    assert_eq!(conflict["code"], "operation_conflict");
    assert_eq!(service.status()["operation_count"], 1);
    assert!(service.status()["native_identity"].is_null());
}

#[test]
fn supplied_success_cannot_open_native_admission() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let reply = service.exchange(json!({"kind":"operate",
        "runner_id":service.status()["identity"]["runner_id"],
        "controller_epoch":"engine-1", "operation_id":"forged-admission",
        "action":{"kind":"launch", "session_id":"session", "harness":"codex",
            "profile_ref":"unadmitted", "worktree_lease":"lease", "admission":{"outcome":"proven"}}}));
    assert_eq!(reply["kind"], "error");
    assert_eq!(reply["code"], "invalid_request");
    assert_eq!(service.status()["operation_count"], 0);
    assert_eq!(service.status()["native_admission"], "unavailable");
}

#[test]
fn runner_restart_retains_receipts_but_exposes_replacement_identity() {
    let mut service = Service::start();
    let original_identity = service.status()["identity"].clone();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let receipt = launch(&service, "engine-1", "operation", "codex");
    service.restart();
    let replacement = service.status();
    assert_ne!(replacement["identity"], original_identity);
    assert_eq!(replacement["generation"], 1);
    assert!(replacement["controller_epoch"].is_null());
    assert_eq!(replacement["operation_count"], 1);
    assert_eq!(service.claim("engine-1", 1)["code"], "retired_controller");
    assert_eq!(service.claim("engine-2", 1)["kind"], "claimed");
    assert_eq!(launch(&service, "engine-2", "operation", "codex"), receipt);
    assert!(service.status()["native_identity"].is_null());
}

#[test]
fn journal_mutation_stops_service_without_acknowledging_native_work() {
    let mut service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let runner_id = service.status()["identity"]["runner_id"].clone();
    fs::OpenOptions::new()
        .write(true)
        .open(service.root.path().join("control.log"))
        .unwrap()
        .set_len(1)
        .unwrap();
    let reply = service.exchange(json!({"kind":"operate", "runner_id":runner_id,
        "controller_epoch":"engine-1", "operation_id":"blocked",
        "action":{"kind":"launch", "session_id":"session", "harness":"codex",
            "profile_ref":"unadmitted", "worktree_lease":"lease"}}));
    assert_eq!(reply["kind"], "error");
    assert_eq!(reply["code"], "journal_unavailable");
    let deadline = Instant::now() + Duration::from_secs(3);
    while service.process.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "journal failure did not stop runner"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn unqualified_child_cancellation_never_reports_child_quiescence() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let reply = service.exchange(json!({"kind":"operate",
        "runner_id":service.status()["identity"]["runner_id"],
        "controller_epoch":"engine-1", "operation_id":"cancel",
        "action":{"kind":"cancel_child", "session_id":"session", "native_child_id":"child"}}));
    assert_eq!(reply["kind"], "operation");
    assert_eq!(reply["state"], "unavailable");
    assert!(reply.get("child_state").is_none());
    assert!(reply["native_identity"].is_null());
}

#[test]
fn oversized_frames_are_rejected_without_disrupting_other_clients() {
    let service = Service::start();
    let mut stream = UnixStream::connect(service.root.path().join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
    let mut size = [0; 4];
    stream.read_exact(&mut size).unwrap();
    let size = u32::from_be_bytes(size) as usize;
    assert!(size < 1024);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["code"],
        "invalid_request"
    );
    assert_eq!(service.status()["kind"], "status");
}

#[test]
fn operation_capacity_keeps_earlier_deduplication_receipts() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    for index in 0..128 {
        assert_eq!(
            launch(&service, "engine-1", &format!("operation-{index}"), "codex")["state"],
            "unavailable"
        );
    }
    assert_eq!(
        launch(&service, "engine-1", "overflow", "codex")["code"],
        "operation_capacity"
    );
    assert_eq!(
        launch(&service, "engine-1", "operation-0", "codex")["state"],
        "unavailable"
    );
    assert_eq!(service.status()["operation_count"], 128);
}

fn wait_exit(process: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = process.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "process exit deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn graceful_termination_removes_the_owned_control_endpoint() {
    let mut service = Service::start();
    assert_eq!(service.status()["kind"], "status");
    assert!(
        Command::new("/bin/kill")
            .arg("-TERM")
            .arg(service.process.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    assert!(wait_exit(&mut service.process).success());
    assert!(!service.root.path().join("control.sock").exists());
}

#[test]
fn control_service_survives_actual_controller_peer_process_death() {
    if let Some(root) = std::env::var_os("T03_CONTROLLER_PEER_ROOT") {
        let root = std::path::Path::new(&root);
        let status = exchange(root, json!({"kind":"status"}));
        let claim = exchange(
            root,
            json!({"kind":"claim", "runner_id":status["identity"]["runner_id"],
            "expected_generation":0, "controller_epoch":"doomed-peer"}),
        );
        assert_eq!(claim["kind"], "claimed");
        std::thread::sleep(Duration::from_secs(10));
        panic!("parent did not terminate the owned controller peer");
    }
    let service = Service::start();
    let identity = service.status()["identity"].clone();
    let mut peer = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "control_service_survives_actual_controller_peer_process_death",
        ])
        .env("T03_CONTROLLER_PEER_ROOT", service.root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while service.status()["controller_epoch"] != "doomed-peer" {
        if Instant::now() >= deadline || peer.try_wait().unwrap().is_some() {
            let _termination = peer.kill();
            let _reap = peer.wait();
            panic!("owned controller peer did not acquire control");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    peer.kill().unwrap();
    assert!(!peer.wait().unwrap().success());
    assert_eq!(service.status()["identity"], identity);
    assert_eq!(service.claim("replacement-peer", 1)["kind"], "claimed");
    assert!(service.status()["native_identity"].is_null());
}

fn reject_start(root: &std::path::Path, reason: &str) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_crazyagent-runner"))
        .arg("serve")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = process.try_wait().unwrap() {
            assert!(!status.success());
            let mut error = String::new();
            process
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            assert!(error.contains(reason), "unexpected rejection: {error}");
            return;
        }
        if Instant::now() >= deadline {
            process.kill().unwrap();
            process.wait().unwrap();
            panic!("runner admitted rejected startup path");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn public_state_directory_is_rejected_before_any_state_write() {
    let root = tempfile::Builder::new()
        .prefix("ca-p-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    reject_start(
        &root.path().canonicalize().unwrap(),
        "requires_owner_mode_0700",
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn directory_symlink_is_rejected_before_any_state_write() {
    let root = tempfile::Builder::new()
        .prefix("ca-s-")
        .tempdir_in("/tmp")
        .unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().canonicalize().unwrap().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    reject_start(&link, "runner_directory");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn journal_symlink_cannot_overwrite_another_file() {
    let root = tempfile::Builder::new()
        .prefix("ca-s-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("canary");
    fs::write(&target, b"retained nonsecret canary").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("control.log")).unwrap();
    reject_start(&root.path().canonicalize().unwrap(), "Error:");
    assert_eq!(fs::read(&target).unwrap(), b"retained nonsecret canary");
}

#[test]
fn hard_linked_journal_cannot_modify_its_other_name() {
    let root = tempfile::Builder::new()
        .prefix("ca-h-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("canary");
    fs::write(&target, b"retained nonsecret canary").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&target, root.path().join("control.log")).unwrap();
    reject_start(
        &root.path().canonicalize().unwrap(),
        "state_requires_private_regular_owner_file",
    );
    assert_eq!(fs::read(&target).unwrap(), b"retained nonsecret canary");
}

#[test]
fn competing_runner_does_not_modify_the_live_owners_journal() {
    let service = Service::start();
    let before = fs::read(service.root.path().join("control.log")).unwrap();
    reject_start(&service.root.path().canonicalize().unwrap(), "runner_lock:");
    assert_eq!(
        fs::read(service.root.path().join("control.log")).unwrap(),
        before
    );
    assert_eq!(service.status()["kind"], "status");
}

#[test]
fn torn_journal_is_retained_and_never_repaired_into_success() {
    let mut service = Service::start();
    service.process.kill().unwrap();
    service.process.wait().unwrap();
    let path = service.root.path().join("control.log");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0, 0, 0])
        .unwrap();
    let before = fs::read(&path).unwrap();
    reject_start(
        &service.root.path().canonicalize().unwrap(),
        "torn_journal_header",
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn unknown_control_endpoint_is_retained_on_startup() {
    let root = tempfile::Builder::new()
        .prefix("ca-u-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("control.sock");
    fs::write(&path, b"unowned endpoint canary").unwrap();
    reject_start(
        &root.path().canonicalize().unwrap(),
        "unrecognized_control_endpoint",
    );
    assert_eq!(fs::read(path).unwrap(), b"unowned endpoint canary");
}

#[test]
fn changed_endpoint_is_never_deleted_by_graceful_cleanup() {
    let mut service = Service::start();
    let path = service.root.path().join("control.sock");
    fs::rename(&path, service.root.path().join("moved.sock")).unwrap();
    fs::write(&path, b"replacement endpoint canary").unwrap();
    assert!(
        Command::new("/bin/kill")
            .arg("-TERM")
            .arg(service.process.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    assert!(!wait_exit(&mut service.process).success());
    assert_eq!(fs::read(path).unwrap(), b"replacement endpoint canary");
    assert!(service.root.path().join("moved.sock").exists());
}

#[test]
fn stale_epoch_reuse_and_epoch_capacity_do_not_erase_history() {
    let service = Service::start();
    for generation in 0..128 {
        assert_eq!(
            service.claim(&format!("engine-{generation}"), generation)["kind"],
            "claimed"
        );
    }
    assert_eq!(service.claim("engine-0", 128)["code"], "retired_controller");
    assert_eq!(
        service.claim("engine-overflow", 128)["code"],
        "epoch_capacity"
    );
    assert_eq!(service.status()["generation"], 128);
    assert_eq!(service.status()["controller_epoch"], "engine-127");
}

#[test]
fn concurrent_controller_claims_have_one_compare_and_swap_winner() {
    let service = Service::start();
    let runner = service.status()["identity"]["runner_id"].clone();
    let root = service.root.path().to_owned();
    let other_root = root.clone();
    let other_runner = runner.clone();
    let first = std::thread::spawn(move || {
        exchange(
            &root,
            json!({"kind":"claim", "runner_id":runner,
        "expected_generation":0, "controller_epoch":"first"}),
        )
    });
    let second = std::thread::spawn(move || {
        exchange(
            &other_root,
            json!({"kind":"claim", "runner_id":other_runner,
        "expected_generation":0, "controller_epoch":"second"}),
        )
    });
    let replies = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(replies.iter().filter(|r| r["kind"] == "claimed").count(), 1);
    assert_eq!(
        replies
            .iter()
            .filter(|r| r["code"] == "stale_generation")
            .count(),
        1
    );
    assert_eq!(service.status()["generation"], 1);
}

#[test]
fn timed_out_incomplete_clients_release_bounded_connection_capacity() {
    let service = Service::start();
    let peers: Vec<_> = (0..16)
        .map(|_| UnixStream::connect(service.root.path().join("control.sock")).unwrap())
        .collect();
    assert_eq!(service.status()["kind"], "status");
    assert_eq!(peers.len(), 16);
}

#[test]
fn receipt_is_retained_when_requester_disconnects_before_acknowledgement() {
    let service = Service::start();
    assert_eq!(service.claim("engine-1", 0)["kind"], "claimed");
    let runner = service.status()["identity"]["runner_id"].clone();
    let mut stream = UnixStream::connect(service.root.path().join("control.sock")).unwrap();
    stream
        .write_all(
            &encode_frame(&json!({"protocol_version":1,
        "command":{"kind":"operate", "runner_id":runner, "controller_epoch":"engine-1",
            "operation_id":"lost-ack", "action":{"kind":"launch", "session_id":"session",
                "harness":"codex", "profile_ref":"unadmitted", "worktree_lease":"lease"}}}))
            .unwrap(),
        )
        .unwrap();
    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(3);
    while service.status()["operation_count"] != 1 {
        assert!(
            Instant::now() < deadline,
            "lost-ack operation was not retained"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        launch(&service, "engine-1", "lost-ack", "codex")["state"],
        "unavailable"
    );
    assert_eq!(service.status()["operation_count"], 1);
}
