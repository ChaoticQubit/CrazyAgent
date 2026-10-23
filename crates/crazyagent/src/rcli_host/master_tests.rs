use super::{RcliMasterBinding, RcliMasterHost, RcliMasterLaunch};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::protocol::{CredentialRef, ProjectId, ProtocolError, RequestId, SessionId};
use crate::sandbox::{InstalledSandbox, ProjectSandbox, SandboxNetwork, SandboxRemote};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Case {
    _root: tempfile::TempDir,
    launch: RcliMasterLaunch,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let home_dir = root_path.join("profile-home");
        let cwd = root_path.join("project");
        for path in [&home_dir, &cwd] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::create_dir(cwd.join(".git")).unwrap();
        fs::create_dir(home_dir.join(".backboard")).unwrap();
        let executable = root_path.join("actual-false");
        fs::copy("/usr/bin/false", &executable).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let launch = RcliMasterLaunch {
            binding: RcliMasterBinding {
                project_id: ProjectId::new("project-a").unwrap(),
                session_id: SessionId::new("session-a").unwrap(),
                instance_id: RequestId::new("instance-a").unwrap(),
                connection: connection(),
                target_profile_ref: credential("profile-a"),
            },
            artifact_sha256: digest(&fs::read(&executable).unwrap()),
            executable,
            home_dir,
            cwd,
            profile_config_sha256: String::new(),
        };
        let mut case = Self {
            _root: root,
            launch,
        };
        case.config(config());
        case
    }

    fn config(&mut self, config: Value) {
        let bytes = serde_json::to_vec(&config).unwrap();
        let path = self.launch.home_dir.join(".backboard/config.json");
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        self.launch.profile_config_sha256 = digest(&bytes);
    }
}

fn sandbox(case: &Case, project: &Path) -> InstalledSandbox {
    let root = case.launch.executable.parent().unwrap();
    let tmp = root.join("agent-tmp");
    let profiles = root.join("profiles");
    for path in [&tmp, &profiles] {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        project,
        &case.launch.home_dir,
        &tmp,
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(11434)]),
    )
    .unwrap()
    .allow_read(root)
    .unwrap();
    InstalledSandbox::install(sandbox, &profiles.join("rcli.sb")).unwrap()
}

fn credential(text: &str) -> CredentialRef {
    CredentialRef::new(text).unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn connection() -> ModelConnection {
    ModelConnection {
        id: credential("connection-a"),
        provider: credential("local-a"),
        harness: credential("rcli"),
        protocol: credential("openai-chat"),
        account_ref: credential("account-a"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: "selected-model".into(),
            author: credential("model-author-a"),
        },
        auth: ModelAuthentication::None {},
    }
}

fn config() -> Value {
    json!({
        "model": {"provider": "local-a", "model": "selected-model"},
        "memory": "off",
        "providers": [{
            "id": "local-a", "name": "Selected local endpoint",
            "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
            "auth": {"type": "none"},
            "models": [{"id": "selected-model"}]
        }]
    })
}

fn rejection(result: Result<std::process::Command, ProtocolError>, expected: &str) {
    match result {
        Ok(_) => panic!("expected rejection {expected}"),
        Err(error) => assert_eq!(error.0, expected),
    }
}

#[test]
fn exact_selection_builds_only_supported_host_argv_without_execution() {
    let case = Case::new();
    let command = case.launch.command().unwrap();
    assert_eq!(command.get_program(), case.launch.executable.as_os_str());
    let args: Vec<_> = command.get_args().map(|value| value.to_owned()).collect();
    let expected: Vec<_> = [
        "--session-id", "session-a", "--instance-id", "instance-a", "--home-dir",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .chain([
        case.launch.home_dir.as_os_str().to_owned(),
        "--cwd".into(),
        case.launch.cwd.as_os_str().to_owned(),
    ])
    .collect();
    assert_eq!(args, expected);
    assert_eq!(command.get_current_dir(), Some(case.launch.cwd.as_path()));
    let environment: Vec<_> = command.get_envs().collect();
    assert_eq!(environment.len(), 1);
    assert_eq!(environment[0].0, "HOME");
    assert_eq!(environment[0].1, Some(case.launch.home_dir.as_os_str()));
    assert_eq!(case.launch.binding.connection.account_ref, credential("account-a"));
    assert_eq!(case.launch.binding.target_profile_ref, credential("profile-a"));
    assert_eq!(case.launch.binding.project_id.as_str(), "project-a");
}

#[test]
fn selected_model_endpoint_protocol_and_provider_must_match_existing_profile() {
    for field in ["model", "endpoint", "protocol", "provider"] {
        let mut case = Case::new();
        match field {
            "model" => case.launch.binding.connection.model.id = "other-model".into(),
            "endpoint" => case.launch.binding.connection.endpoint = ModelEndpoint::Http {
                url: "http://127.0.0.1:11435/v1".into(),
            },
            "protocol" => case.launch.binding.connection.protocol = credential("openai-responses"),
            "provider" => case.launch.binding.connection.provider = credential("other-provider"),
            _ => unreachable!(),
        }
        rejection(case.launch.command(), "rcli_master_profile_mismatch");
    }
}

#[test]
fn defaults_disabled_models_expert_mode_and_subscription_conversion_are_rejected() {
    for field in ["default", "disabled", "expert", "subscription"] {
        let mut case = Case::new();
        let mut value = config();
        match field {
            "default" => { value.as_object_mut().unwrap().remove("model"); }
            "disabled" => value["providers"][0]["models"][0]["enabled"] = json!(false),
            "expert" => value["expert"] = json!({"enabled": true}),
            "subscription" => case.launch.binding.connection.auth =
                ModelAuthentication::NativeManaged { profile_ref: credential("codex-profile") },
            _ => unreachable!(),
        }
        case.config(value);
        assert!(case.launch.command().is_err(), "{field}");
    }
}

#[test]
fn inherited_remote_insecure_or_credential_bearing_endpoint_is_not_a_valid_plan() {
    for endpoint in [
        "http://remote.invalid/v1", "https://user:secret@example.invalid/v1",
        "file:///private/model", "https://example.invalid/v1?api_key=secret",
    ] {
        let mut case = Case::new();
        let mut value = config();
        value["providers"][0]["baseUrl"] = json!(endpoint);
        case.config(value);
        case.launch.binding.connection.endpoint = ModelEndpoint::Http { url: endpoint.into() };
        assert!(case.launch.command().is_err());
    }
}

#[test]
fn artifact_and_selected_profile_are_rechecked_against_root_supplied_pins() {
    let mut case = Case::new();
    assert!(case.launch.command().is_ok());
    case.launch.artifact_sha256 = "0".repeat(64);
    rejection(case.launch.command(), "rcli_master_artifact_changed");
    case.launch.artifact_sha256 = digest(&fs::read(&case.launch.executable).unwrap());
    case.launch.profile_config_sha256 = "0".repeat(64);
    rejection(case.launch.command(), "rcli_master_profile_changed");
    assert!(!case.launch.home_dir.join(".backboard/keys.json").exists());
}

#[test]
fn real_private_paths_reject_aliases_overlap_and_group_access() {
    let mut case = Case::new();
    let actual = case.launch.home_dir.clone();
    let alias = actual.parent().unwrap().join("home-alias");
    symlink(&actual, &alias).unwrap();
    case.launch.home_dir = alias;
    rejection(case.launch.command(), "rcli_master_paths_rejected");
    case.launch.home_dir = actual;
    fs::set_permissions(&case.launch.cwd, fs::Permissions::from_mode(0o750)).unwrap();
    rejection(case.launch.command(), "rcli_master_paths_rejected");
    fs::set_permissions(&case.launch.cwd, fs::Permissions::from_mode(0o700)).unwrap();
    let original = case.launch.cwd.clone();
    case.launch.cwd = case.launch.home_dir.clone();
    rejection(case.launch.command(), "rcli_master_paths_rejected");
    case.launch.cwd = original;
    let executable = case.launch.executable.clone();
    let alias = executable.with_extension("alias");
    symlink(executable, &alias).unwrap();
    case.launch.executable = alias;
    assert!(case.launch.command().is_err());
}

#[test]
fn real_process_exit_never_becomes_ready_or_reply_and_shutdown_reaps_owned_child() {
    let case = Case::new();
    let root = case.launch.executable.parent().unwrap().to_path_buf();
    let other = root.join("other-project");
    fs::create_dir(&other).unwrap();
    let elsewhere = sandbox(&case, &other);
    let refused = RcliMasterHost::launch(case.launch.clone(), &elsewhere)
        .err()
        .unwrap();
    assert_eq!(refused.0, "rcli_master_sandbox_refused");
    let boundary = sandbox(&case, &case.launch.cwd);
    let mut host = RcliMasterHost::launch(case.launch, &boundary).unwrap();
    assert_eq!(host.binding().connection.account_ref, credential("account-a"));
    let turn = RequestId::new("turn-before-ready").unwrap();
    let error = host.prompt(turn, "A prompt cannot run before actual Ready").unwrap_err();
    assert_eq!(error.0, "rcli_master_not_ready");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match host.poll() {
            Ok(None) => assert!(Instant::now() < deadline, "real process exit was not observed"),
            Ok(Some(_)) => panic!("/usr/bin/false produced no host observation"),
            Err(error) => {
                assert_eq!(error.0, "rcli_master_process_exited");
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
    host.shutdown().unwrap();
}

#[test]
fn bounded_config_input_never_uses_unpinned_or_oversized_fallback() {
    let mut case = Case::new();
    let path = case.launch.home_dir.join(".backboard/config.json");
    let bytes = vec![b' '; 65537];
    fs::write(path, &bytes).unwrap();
    case.launch.profile_config_sha256 = digest(&bytes);
    assert!(case.launch.command().is_err());
    case.launch.executable = PathBuf::from("rcli");
    assert!(case.launch.command().is_err());
    assert!(!Path::new("rcli").is_absolute());
}
