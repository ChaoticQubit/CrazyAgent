use super::master::clock_ended;
use super::master_reply::TurnCapture;
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
        fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
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
            instructions: None,
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
        "--session-id",
        "session-a",
        "--instance-id",
        "instance-a",
        "--home-dir",
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
    assert_eq!(
        case.launch.binding.connection.account_ref,
        credential("account-a")
    );
    assert_eq!(
        case.launch.binding.target_profile_ref,
        credential("profile-a")
    );
    assert_eq!(case.launch.binding.project_id.as_str(), "project-a");
}

#[test]
fn selected_model_endpoint_protocol_and_provider_must_match_existing_profile() {
    for field in ["model", "endpoint", "protocol", "provider"] {
        let mut case = Case::new();
        match field {
            "model" => case.launch.binding.connection.model.id = "other-model".into(),
            "endpoint" => {
                case.launch.binding.connection.endpoint = ModelEndpoint::Http {
                    url: "http://127.0.0.1:11435/v1".into(),
                }
            }
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
            "default" => {
                value.as_object_mut().unwrap().remove("model");
            }
            "disabled" => value["providers"][0]["models"][0]["enabled"] = json!(false),
            "expert" => value["expert"] = json!({"enabled": true}),
            "subscription" => {
                case.launch.binding.connection.auth = ModelAuthentication::NativeManaged {
                    profile_ref: credential("codex-profile"),
                }
            }
            _ => unreachable!(),
        }
        case.config(value);
        assert!(case.launch.command().is_err(), "{field}");
    }
}

#[test]
fn inherited_remote_insecure_or_credential_bearing_endpoint_is_not_a_valid_plan() {
    for endpoint in [
        "http://remote.invalid/v1",
        "https://user:secret@example.invalid/v1",
        "file:///private/model",
        "https://example.invalid/v1?api_key=secret",
    ] {
        let mut case = Case::new();
        let mut value = config();
        value["providers"][0]["baseUrl"] = json!(endpoint);
        case.config(value);
        case.launch.binding.connection.endpoint = ModelEndpoint::Http {
            url: endpoint.into(),
        };
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
    let first =
        crate::spawn_gate::first_run(&mut std::process::Command::new(&case.launch.executable));
    assert_eq!(
        first.code(),
        Some(1),
        "the fixture did not run to its own exit"
    );
    let root = case.launch.executable.parent().unwrap().to_path_buf();
    let other = root.join("other-project");
    fs::create_dir(&other).unwrap();
    let elsewhere = sandbox(&case, &other);
    let refused = RcliMasterHost::launch(case.launch.clone(), &elsewhere, None)
        .err()
        .unwrap();
    assert_eq!(refused.0, "rcli_master_sandbox_refused");
    let boundary = sandbox(&case, &case.launch.cwd);
    let mut host = RcliMasterHost::launch(case.launch, &boundary, None).unwrap();
    assert_eq!(
        host.binding().connection.account_ref,
        credential("account-a")
    );
    let turn = RequestId::new("turn-before-ready").unwrap();
    let error = host
        .prompt(turn, "A prompt cannot run before actual Ready")
        .unwrap_err();
    assert_eq!(error.0, "rcli_master_not_ready");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match host.poll() {
            Ok(None) => assert!(
                Instant::now() < deadline,
                "real process exit was not observed"
            ),
            Ok(Some(_)) => panic!("the exiting fixture produced no host observation"),
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

/// With no master time limit and no cancel asked for, no clock ends an rcli turn however
/// long it runs (OB-5d): it used to end 120 s after its prompt and 600 s after its host
/// started. A run clock, one per limit the owner can pick, ends it from its end on, and so
/// does the wait for a cancel.
#[test]
fn no_clock_ends_a_turn_without_a_time_limit_and_each_limit_ends_it_on_time() {
    use crate::master_time_limit::TimeLimit;

    let start = Instant::now();
    let day = start + Duration::from_secs(86_400);
    for seconds in [0, 120, 600, 3_600, 86_400] {
        let now = start + Duration::from_secs(seconds);
        assert!(!clock_ended(now, None, None), "no limit at {seconds} s");
    }
    let clocks: Vec<Duration> = TimeLimit::ALL
        .into_iter()
        .filter_map(TimeLimit::run_time)
        .collect();
    assert_eq!(clocks.len(), 3, "10, 30 and 60 minutes");
    for time in clocks {
        let end = start + time;
        let before = end - Duration::from_millis(1);
        assert!(!clock_ended(before, Some(end), None), "{time:?}");
        assert!(clock_ended(end, Some(end), None), "{time:?}");
        assert!(clock_ended(day, Some(end), None), "{time:?}");
    }
    let cancel = start + Duration::from_secs(6);
    assert!(!clock_ended(start, None, Some(cancel)));
    assert!(clock_ended(cancel, None, Some(cancel)));
    assert!(
        clock_ended(cancel, Some(day), Some(cancel)),
        "the earlier end decides"
    );
    let turn = TurnCapture::new(RequestId::new("turn-clock").unwrap());
    assert!(
        turn.deadline.is_none(),
        "a prompted turn has no clock of its own"
    );
    assert!(!turn.cancelling);
}

/// A stand-in rcli that never says it is ready and waits on its input until it is stopped.
const WAITING_HOST: &str = "#!/bin/sh\nIFS= read -r line\n";

/// Runs the waiting stand-in as a real rcli master host under a run clock and polls it
/// until the host ends. Input: the clock, none for no master time limit. Output: the code
/// the host ended with and how long after its launch.
fn waiting_host_end(clock: Option<Duration>) -> (String, Duration) {
    let mut case = Case::new();
    fs::write(&case.launch.executable, WAITING_HOST).unwrap();
    case.launch.artifact_sha256 = digest(WAITING_HOST.as_bytes());
    let first =
        crate::spawn_gate::first_run(&mut std::process::Command::new(&case.launch.executable));
    assert_eq!(
        first.code(),
        Some(1),
        "the stand-in did not end with its input"
    );
    let boundary = sandbox(&case, &case.launch.cwd);
    let started = Instant::now();
    let mut host = RcliMasterHost::launch(case.launch.clone(), &boundary, clock).unwrap();
    let ended = loop {
        match host.poll() {
            Ok(None) => assert!(
                started.elapsed() < Duration::from_secs(20),
                "the host never ended"
            ),
            Ok(Some(_)) => panic!("the silent stand-in produced a host observation"),
            Err(error) => break (error.0, started.elapsed()),
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    host.shutdown().unwrap();
    assert!(!ended.0.is_empty());
    ended
}

/// A launched rcli host obeys the run clock it is given, the owner's master time limit, and
/// has none without one (OB-5d). The stand-in never becomes ready: under a 1 s clock the
/// host ends as out of time, and with no clock only the 5 s start bound ends it, which is
/// no turn clock.
#[test]
fn a_launched_host_ends_at_its_run_clock_and_has_no_clock_without_one() {
    let (code, after) = waiting_host_end(Some(Duration::from_secs(1)));
    assert_eq!(code, "rcli_master_turn_deadline");
    assert!(after >= Duration::from_secs(1), "{after:?}");
    assert!(after < Duration::from_secs(5), "{after:?}");
    let (code, after) = waiting_host_end(None);
    assert_eq!(
        code,
        "rcli_master_startup_deadline",
        "no run clock ended it"
    );
    assert!(after >= Duration::from_secs(5), "{after:?}");
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

#[test]
fn an_instructions_file_adds_one_flag_and_only_a_private_single_link_file_outside_both_folders_passes()
 {
    let mut case = Case::new();
    assert_eq!(case.launch.command().unwrap().get_args().count(), 8);
    let runtime = case.launch.cwd.parent().unwrap().join("master-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let private = |path: &Path, text: &str| {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    };
    let file = runtime.join("instructions.md");
    private(&file, "OWNER-RULE-86\n");
    case.launch.instructions = Some(file.clone());
    let command = case.launch.command().unwrap();
    let args: Vec<_> = command.get_args().map(|value| value.to_owned()).collect();
    assert_eq!(args.len(), 10);
    assert_eq!(args[8], "--instructions-file");
    assert_eq!(args[9], file.as_os_str());
    assert_eq!(
        command.get_envs().count(),
        1,
        "the file adds no environment"
    );
    for inside in [
        case.launch.cwd.join("instructions.md"),
        case.launch.home_dir.join("instructions.md"),
    ] {
        private(&inside, "OWNER-RULE-86\n");
        case.launch.instructions = Some(inside);
        rejection(case.launch.command(), "rcli_master_paths_rejected");
    }
    let linked = runtime.join("linked.md");
    symlink(&file, &linked).unwrap();
    case.launch.instructions = Some(linked);
    rejection(case.launch.command(), "rcli_master_paths_rejected");
    case.launch.instructions = Some(runtime.join("missing.md"));
    rejection(case.launch.command(), "rcli_master_paths_rejected");
    case.launch.instructions = Some(file.clone());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    rejection(case.launch.command(), "rcli_master_file_rejected");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let second = runtime.join("second.md");
    fs::hard_link(&file, &second).unwrap();
    rejection(case.launch.command(), "rcli_master_file_rejected");
    fs::remove_file(&second).unwrap();
    let empty = runtime.join("empty.md");
    private(&empty, "");
    case.launch.instructions = Some(empty);
    rejection(case.launch.command(), "rcli_master_file_rejected");
    case.launch.instructions = Some(file);
    assert_eq!(case.launch.command().unwrap().get_args().count(), 10);
}

/// One frame of a prompted turn as its host reads it, for the capture rows. Inputs: the
/// frame's sequence and its body. Output: the event.
fn turn_event(sequence: u64, event: super::EventBody) -> super::Event {
    super::Event(super::RawEvent {
        protocol_version: super::HOST_PROTOCOL_VERSION,
        session_id: SessionId::new("capture-session").unwrap(),
        instance_id: RequestId::new("capture-instance").unwrap(),
        sequence,
        event,
    })
}

/// No count of messages and no total of frames ends an rcli master turn (OB-5 r2): a turn
/// of 10,000 messages, where the 129th used to end it as a reply cut off, each beside a
/// tool output frame of the largest size, where 1 MiB of frames in all used to end it,
/// ends with its whole reply.
#[test]
fn a_turn_of_ten_thousand_messages_and_large_frames_ends_with_its_reply() {
    use super::master::RcliMasterStatus;
    use super::{EventBody, TurnStatus};

    let turn_id = RequestId::new("turn-long").unwrap();
    let mut turn = TurnCapture::new(turn_id.clone());
    let largest = crate::protocol::FRAME_BYTES_MAX + 4;
    let mut sequence = 0;
    let mut frame = |event: EventBody| {
        sequence += 1;
        turn_event(sequence, event)
    };
    for index in 1..=10_000 {
        let message_id = RequestId::new(format!("msg-{index}")).unwrap();
        let started = EventBody::MessageStarted {
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
        };
        let delta = EventBody::MessageDelta {
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            text: format!("step {index}"),
        };
        let output = EventBody::Output {
            turn_id: turn_id.clone(),
            text: "tool output".into(),
        };
        let finished = EventBody::MessageFinished {
            turn_id: turn_id.clone(),
            message_id,
        };
        for event in [started, delta, output, finished] {
            let taken = turn.accept(&frame(event), largest);
            assert!(taken.is_ok_and(|done| done.is_none()), "message {index}");
        }
    }
    let completed = EventBody::Completed {
        turn_id: turn_id.clone(),
        status: TurnStatus::Succeeded,
    };
    let done = turn.accept(&frame(completed), largest).unwrap().unwrap();
    assert_eq!(done.turn_id, turn_id);
    assert!(matches!(done.status, RcliMasterStatus::Succeeded));
    let reply = done.reply.unwrap();
    assert_eq!(reply.split("\n\n").count(), 10_000);
    assert!(
        reply.starts_with("step 1\n\nstep 2\n\n"),
        "{}",
        &reply[..32]
    );
    assert!(reply.ends_with("\n\nstep 9999\n\nstep 10000"));
}

/// The capture is bounded by the bytes it keeps: text past 8 MiB ends the turn with the
/// output limit code, which the owner reads as output too large to keep, never with the
/// order code, which reads as a reply cut off. A message id used twice still gets the
/// order code.
#[test]
fn kept_reply_bytes_past_the_bound_end_the_turn_with_the_output_code() {
    use super::EventBody;

    let turn_id = RequestId::new("turn-large").unwrap();
    let message_id = RequestId::new("msg-1").unwrap();
    let mut turn = TurnCapture::new(turn_id.clone());
    let started = || EventBody::MessageStarted {
        turn_id: turn_id.clone(),
        message_id: message_id.clone(),
    };
    assert!(turn.accept(&turn_event(1, started()), 128).is_ok());
    let twice = turn.accept(&turn_event(2, started()), 128).err().unwrap();
    assert_eq!(twice.0, "rcli_master_message_order");
    let text = "x".repeat(60_000);
    let mut kept = 0;
    let failure = loop {
        let delta = EventBody::MessageDelta {
            turn_id: turn_id.clone(),
            message_id: message_id.clone(),
            text: text.clone(),
        };
        match turn.accept(&turn_event(3 + kept, delta), 60_200) {
            Ok(done) => assert!(done.is_none()),
            Err(failure) => break failure,
        }
        kept += 1;
        assert!(kept <= 200, "no byte bound ended the turn");
    };
    assert_eq!(failure.0, "rcli_master_output_limit");
    assert_eq!(
        kept,
        139,
        "8 MiB less the message's entry, in 60,000 byte deltas"
    );
}
