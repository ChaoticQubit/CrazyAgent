#![cfg(unix)]

use crazyagent::config::{ProviderQuotaPolicy, resolve_config};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STREAM_MAX: u64 = 65536;
const GLOBAL: &str = r#"version = 1
[model_access]
accounts = ["account-1"]
pools = []
[[model_access.connections]]
id = "connection-1"
provider = "provider-1"
harness = "codex"
protocol = "native"
account_ref = "account-1"
[model_access.connections.endpoint]
kind = "native"
[model_access.connections.model]
id = "model-1"
author = "author-1"
[model_access.connections.auth]
method = "none"
"#;

struct PythonRun {
    child: Child,
    deadline: Instant,
}

impl Drop for PythonRun {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = crazyagent::spawn_gate::status(
            Command::new("/bin/kill")
                .args(["-TERM", &self.child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        let cleanup = self.deadline.min(Instant::now() + Duration::from_secs(10));
        while Instant::now() < cleanup {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn private_file(path: &Path) -> File {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
}

fn read_bounded(path: &Path, limit: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    File::open(path)
        .unwrap()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(
        bytes.len() as u64 <= limit,
        "{} exceeded {limit} bytes",
        path.display()
    );
    bytes
}

fn run_case(root: &Path) {
    let global = root.join("global.toml");
    let project = root.join("project.toml");
    fs::write(&global, GLOBAL).unwrap();
    fs::write(&project, "version = 1\n").unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&project, fs::Permissions::from_mode(0o600)).unwrap();
    let initial = resolve_config(&global, &project)
        .unwrap()
        .settings
        .model_access
        .unwrap();
    assert!(initial.pools.is_empty());
    assert_eq!(initial.accounts.len(), 1);
    assert_eq!(initial.connections.len(), 1);

    let stdout = root.join("python.stdout");
    let stderr = root.join("python.stderr");
    let stdout_file = private_file(&stdout);
    let stderr_file = private_file(&stderr);
    let unix_deadline =
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_secs(60);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut python = PythonRun {
        child: crazyagent::spawn_gate::spawn(
            Command::new("python3")
                .env_remove("PYTHONOPTIMIZE")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/qualification/tui_quota_flow.py"
                ))
                .arg(env!("CARGO_BIN_EXE_crazyagent"))
                .arg(root)
                .arg(unix_deadline.as_secs_f64().to_string())
                .stdin(Stdio::null())
                .stdout(stdout_file)
                .stderr(stderr_file),
        )
        .unwrap(),
        deadline,
    };
    eprintln!(
        "actual quota Python PID {} fixture {}",
        python.child.id(),
        root.display()
    );
    let status = loop {
        for path in [&stdout, &stderr] {
            assert!(
                fs::metadata(path).unwrap().len() <= STREAM_MAX,
                "{} exceeded the monitored 64 KiB stream limit; fixture {}",
                path.display(),
                root.display()
            );
        }
        if let Some(status) = python.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "60-second quota scenario watchdog expired; actual cleanup identities are retained at {}",
            root.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(python);
    let stdout = read_bounded(&stdout, STREAM_MAX);
    let stderr = read_bounded(&stderr, STREAM_MAX);
    assert!(
        status.success(),
        "actual quota PTY exited {status}; fixture {}\n{}\n{}",
        root.display(),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let cleanup: serde_json::Value =
        serde_json::from_slice(&read_bounded(&root.join("cleanup.json"), STREAM_MAX)).unwrap();
    assert_eq!(cleanup["groups_absent"].as_bool(), Some(true), "{cleanup}");
    assert_eq!(
        cleanup["terminal_modes_restored"].as_bool(),
        Some(true),
        "{cleanup}"
    );
    assert_eq!(cleanup["engine_exit"].as_i64(), Some(0), "{cleanup}");
    assert_eq!(
        cleanup["sessions"].as_array().unwrap().len(),
        2,
        "{cleanup}"
    );
    for session in cleanup["sessions"].as_array().unwrap() {
        assert_eq!(session["tui_exit"].as_i64(), Some(0), "{session}");
        assert_eq!(session["supervisor_exit"].as_i64(), Some(0), "{session}");
        assert_eq!(session["group_absent"].as_bool(), Some(true), "{session}");
    }

    let created = root.join("created-default.toml");
    let default_access = resolve_config(&created, &project)
        .unwrap()
        .settings
        .model_access
        .unwrap();
    let strict_access = resolve_config(&global, &project)
        .unwrap()
        .settings
        .model_access
        .unwrap();
    assert_eq!(default_access.accounts, initial.accounts);
    assert_eq!(default_access.connections, initial.connections);
    assert_eq!(strict_access.accounts, initial.accounts);
    assert_eq!(strict_access.connections, initial.connections);
    assert_eq!(default_access.pools.len(), 1);
    assert_eq!(strict_access.pools.len(), 1);
    let pool = &default_access.pools[0];
    assert!(
        pool.id.as_str().starts_with("pool-ptyquotapool-"),
        "{}",
        pool.id.as_str()
    );
    assert_eq!(pool.name.as_deref(), Some("pty quota pool"));
    assert_eq!(
        pool.members
            .iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>(),
        ["connection-1"]
    );
    assert!(pool.projects.is_empty(), "{:?}", pool.projects);
    assert!(pool.roles.is_empty(), "{:?}", pool.roles);
    assert!(!pool.fallback_allowed);
    assert_eq!(
        pool.provider_quota_policy,
        ProviderQuotaPolicy::BoundedAttempt
    );
    let mut expected = pool.clone();
    expected.provider_quota_policy = ProviderQuotaPolicy::Strict;
    assert_eq!(strict_access.pools, vec![expected]);

    let default_toml: toml::Table =
        toml::from_str(std::str::from_utf8(&read_bounded(&created, STREAM_MAX)).unwrap()).unwrap();
    let strict_toml: toml::Table =
        toml::from_str(std::str::from_utf8(&read_bounded(&global, STREAM_MAX)).unwrap()).unwrap();
    let default_pools = default_toml["model_access"]["pools"].as_array().unwrap();
    let strict_pools = strict_toml["model_access"]["pools"].as_array().unwrap();
    assert_eq!(default_pools.len(), 1);
    assert_eq!(strict_pools.len(), 1);
    assert!(
        !default_pools[0]
            .as_table()
            .unwrap()
            .contains_key("provider_quota_policy")
    );
    assert_eq!(
        strict_pools[0]["provider_quota_policy"].as_str(),
        Some("strict")
    );
    eprintln!(
        "PTY quota create/inspect/strict edit/fresh owner and typed disk readback passed\n{}",
        String::from_utf8_lossy(&stdout)
    );
}

#[test]
fn real_pty_pool_quota_create_inspect_edit_readback() {
    let root = tempfile::Builder::new()
        .prefix("ca-q586-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let result = std::panic::catch_unwind(|| run_case(root.path()));
    if let Err(problem) = result {
        let retained = root.keep();
        eprintln!(
            "actual quota fixture and bounded evidence retained at {}",
            retained.display()
        );
        std::panic::resume_unwind(problem);
    }
}
