#![cfg(unix)]

use crazyagent::engine::Client;
use crazyagent::isolation::{
    BoundaryError, NetworkPolicy, ResourceLimits, SandboxProfile, run_engine_boundary_probe,
};
use crazyagent::protocol::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

struct EngineProcess(Child);

impl Drop for EngineProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let status = self.0.wait().expect("reap owned disposable engine");
        eprintln!("reaped owned disposable engine {}: {status}", self.0.id());
    }
}

fn launch(root: &Path, artifacts: &Path) -> EngineProcess {
    let child = crazyagent::spawn_gate::spawn(
        ProcessCommand::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(["engine"])
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root)
            .env("TMPDIR", root)
            .stdin(Stdio::null())
            .stdout(fs::File::create(artifacts.join("engine.stdout")).unwrap())
            .stderr(fs::File::create(artifacts.join("engine.stderr")).unwrap()),
    )
    .expect("launch actual disposable engine executable");
    let mut process = EngineProcess(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            panic!(
                "UNAVAILABLE: engine exited {status}: {}; artifacts {}",
                fs::read_to_string(artifacts.join("engine.stderr")).unwrap(),
                artifacts.display()
            );
        }
        if fs::read_to_string(artifacts.join("engine.stdout"))
            .unwrap()
            .trim()
            == "ready"
        {
            return process;
        }
        assert!(
            Instant::now() < deadline,
            "UNAVAILABLE: engine ready deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn snapshot(id: &str) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("host-client").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        command: Command::Snapshot,
    }
}

#[tokio::test]
async fn worker_cannot_control_engine() {
    let root = tempfile::Builder::new()
        .prefix("crazyagent-web-")
        .tempdir_in("/private/tmp")
        .unwrap()
        .keep();
    let root = root.canonicalize().unwrap();
    eprintln!("retained worker-engine artifacts: {}", root.display());
    let candidate = root.join("candidate");
    let trusted = root.join("trusted");
    let artifacts = root.join("engine-boundary-evidence");
    for directory in [&candidate, &trusted, &artifacts] {
        fs::create_dir(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = SandboxProfile {
        id: "worker-engine-boundary".into(),
        runtime_path: PathBuf::from(
            std::env::var_os("CRAZYAGENT_SRT")
                .expect("UNAVAILABLE: set CRAZYAGENT_SRT to verified srt"),
        ),
        node_path: PathBuf::from(
            std::env::var_os("CRAZYAGENT_NODE")
                .expect("UNAVAILABLE: set CRAZYAGENT_NODE to verified Node"),
        ),
        grants: vec![candidate.clone()],
        network_policy: NetworkPolicy::DenyAll,
        credential_refs: vec![trusted.join("synthetic-credential")],
        resource_limits: ResourceLimits {
            cpu_seconds: 2,
            memory_bytes: 64 * 1024 * 1024,
            processes: 4,
            wall_time_seconds: 15,
        },
    };
    profile.validate(&candidate, &trusted).unwrap();
    let mut engine = launch(&trusted, &artifacts);
    let client = Client::new(trusted.join("engine.sock"), Duration::from_secs(2)).unwrap();
    let before = client.request(snapshot("host-before")).await.unwrap();
    assert!(
        matches!(before.outcome, Outcome::Snapshot { ref projects, .. } if projects.is_empty())
    );
    let mut create = snapshot("host-create");
    create.expected_revision = before.revision;
    create.command = Command::CreateProject {
        project_id: ProjectId::new("host-project").unwrap(),
        spec: ProjectSpec {
            name: "Disposable host control".into(),
            repositories: vec![],
        },
    };
    assert_eq!(
        client.request(create).await.unwrap().outcome,
        Outcome::Applied
    );
    fs::write(trusted.join("synthetic-credential"), "synthetic nonsecret").unwrap();
    let result = run_engine_boundary_probe(&profile, &candidate, &trusted, &mut engine.0);
    let after = client.request(snapshot("host-after")).await.unwrap();
    let Outcome::Snapshot { projects, .. } = after.outcome else {
        panic!("host snapshot required after probes")
    };
    let ids: Vec<_> = projects.iter().map(|p| p.project_id.as_str()).collect();
    eprintln!("actual persisted project ids: {ids:?}");
    let db = rusqlite::Connection::open_with_flags(
        trusted.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let mut statement = db.prepare("SELECT id FROM operations ORDER BY id").unwrap();
    let operations: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    fs::write(
        artifacts.join("persisted-observations.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"projects": ids, "operations": operations}))
            .unwrap(),
    )
    .unwrap();
    assert!(engine.0.try_wait().unwrap().is_none());
    let evidence = match result {
        Err(BoundaryError::Unenforced(evidence)) => *evidence,
        Ok(_) => panic!("resource-unqualified profile must remain unavailable"),
        Err(error) => panic!("native engine boundary failed or unavailable: {error}"),
    };
    assert_eq!(ids.len(), 3);
    for role in ["worker", "child"] {
        assert!(ids.contains(&format!("unconfined-{role}").as_str()));
        assert!(operations.contains(&format!("unconfined-{role}-control")));
        assert!(!operations.contains(&format!("confined-{role}-control")));
        for case in ["contact", "control", "state-read", "state-write", "signal"] {
            assert_eq!(evidence.denied.get(&format!("{role}-{case}")), Some(&true));
        }
        assert_eq!(
            evidence.allowed.get(&format!("{role}-candidate-write")),
            Some(&true)
        );
    }
    assert!(!evidence.admissible());
    eprintln!("engine boundary subset passed; resource profile remains unavailable");
}
