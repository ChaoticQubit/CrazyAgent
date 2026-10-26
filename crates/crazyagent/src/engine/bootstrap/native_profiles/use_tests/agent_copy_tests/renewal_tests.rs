use super::*;
use crate::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use crate::team::runtime::managed_agent::{claude, codex};

const COPY_WAIT: Duration = Duration::from_secs(90);

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn replace(path: &Path, bytes: &[u8]) {
    let staged = path.with_extension("staged");
    private_file(&staged, bytes);
    std::fs::rename(&staged, path).unwrap();
}

#[test]
fn a_record_that_covers_the_turn_is_copied_without_running_the_harness() {
    let mut case = UseCase::new();
    let home = case.home();
    private_file(&home.join("auth.json"), &codex_auth(now() + 3 * 86_400));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN, &codex()).unwrap();
    assert_eq!(entries(&home), ["auth.json"]);
    assert_eq!(entries(&agent), ["auth.json"]);
    drop(copy);
}

#[test]
fn a_lapsed_codex_record_is_renewed_by_the_real_codex_in_the_managed_home_before_it_is_refused() {
    let mut case = UseCase::new();
    let home = case.home();
    private_file(&home.join("auth.json"), &codex_auth(now() - 60));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &codex())),
        "native_profile_credential_expiring"
    );
    assert!(
        home.join("installation_id").is_file(),
        "the real codex never ran in the managed home: {:?}",
        entries(&home)
    );
    assert!(entries(&agent).is_empty());
}

#[test]
fn a_lapsed_claude_record_is_renewed_by_the_real_claude_in_the_managed_home_before_it_is_refused() {
    let mut case = UseCase::with_harness("claude-code");
    let home = case.home();
    let record = claude_record((now() - 60) * 1000);
    let (keychain, service) = claude_keychain(&home, &record);
    let path = keychain.to_str().unwrap();
    security(&home, &["list-keychains", "-d", "user", "-s", path], None);
    security(&home, &["default-keychain", "-d", "user", "-s", path], None);
    let user = std::env::var("USER").unwrap();
    security(
        &home,
        &[
            "add-generic-password",
            "-U",
            "-a",
            &user,
            "-s",
            &service,
            "-w",
            &record.to_string(),
            path,
        ],
        None,
    );
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &claude())),
        "native_profile_credential_expiring"
    );
    assert!(
        home.join(".claude.json").is_file(),
        "the real claude never ran in the managed home: {:?}",
        entries(&home)
    );
    assert!(entries(&agent).is_empty());
}

#[test]
fn the_agents_copy_is_replaced_during_the_turn_once_the_account_is_renewed() {
    let mut case = UseCase::new();
    let home = case.home();
    let lapsing = now() + 4 * 60 + 8;
    private_file(&home.join("auth.json"), &codex_auth(lapsing));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN, &codex()).unwrap();
    let path = agent.join("auth.json");
    assert_eq!(
        read_json(&path)["tokens"]["access_token"],
        token(lapsing).as_str()
    );
    let renewed = now() + 3 * 86_400;
    replace(&home.join("auth.json"), &codex_auth(renewed));
    let deadline = Instant::now() + COPY_WAIT;
    while read_json(&path)["tokens"]["access_token"] != token(renewed).as_str() {
        assert!(
            Instant::now() < deadline,
            "the agent's copy was never replaced"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    let written = read_json(&path);
    assert_eq!(written["tokens"]["refresh_token"], "");
    assert_eq!(written["OPENAI_API_KEY"], Value::Null);
    assert_eq!(entries(&agent), ["auth.json"]);
    drop(copy);
    assert!(entries(&agent).is_empty());
    drop(guard);
}

#[test]
fn an_account_check_runs_beside_agents_but_never_beside_a_renewal() {
    let mut case = UseCase::new();
    let agent = execution(&mut case);
    let check = |case: &UseCase, name: &str| {
        case.recapture().acquire_use(
            &case.host,
            &case.binding,
            &request_id(name),
            1,
            NativeProfileUsePurpose::Check,
        )
    };
    let renewal = agent.hold_renewal(Duration::ZERO).unwrap();
    assert_eq!(
        check(&case, "check-during-renewal").err().unwrap().0,
        "native_profile_in_use"
    );
    drop(renewal);
    let running = check(&case, "check-beside-agent").unwrap();
    assert_eq!(
        agent
            .hold_renewal(Duration::from_millis(200))
            .err()
            .unwrap()
            .0,
        "native_profile_renewal_busy"
    );
    let second = execution(&mut case);
    drop(running);
    let renewed = second.hold_renewal(Duration::ZERO).unwrap();
    drop(renewed);
    drop(second);
    drop(agent);
}

#[test]
fn one_renewal_runs_at_a_time_per_account() {
    let mut case = UseCase::new();
    let first = execution(&mut case);
    let second = execution(&mut case);
    let held = first.hold_renewal(Duration::ZERO).unwrap();
    assert_eq!(
        second
            .hold_renewal(Duration::from_millis(200))
            .err()
            .unwrap()
            .0,
        "native_profile_renewal_busy"
    );
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let held = second.hold_renewal(Duration::from_secs(20)).map(drop);
        (started.elapsed(), held)
    });
    std::thread::sleep(Duration::from_millis(400));
    drop(held);
    let (waited, held) = waiter.join().unwrap();
    assert!(held.is_ok());
    assert!(waited >= Duration::from_millis(400), "{waited:?}");
    drop(first);
}

fn codex_sign_in(store: &str) -> String {
    let case = UseCase::new();
    let home = case.home();
    let mut record: Value = serde_json::from_slice(&codex_auth(now() + 3 * 86_400)).unwrap();
    record["OPENAI_API_KEY"] = Value::Null;
    private_file(&home.join("auth.json"), record.to_string().as_bytes());
    let project = case.root.path().join("project");
    let tmp = case.root.path().join("tmp");
    for folder in [&project, &tmp] {
        private_directory(folder);
    }
    let executable = codex();
    let sandbox = ProjectSandbox::new(
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    )
    .unwrap()
    .allow_read(executable.parent().unwrap())
    .unwrap();
    let profile = case.root.path().join("master.sb");
    sandbox.install(&profile).unwrap();
    let mut command = sandbox.command(&profile, &executable);
    command
        .args([
            "-c",
            &format!("cli_auth_credentials_store=\"{store}\""),
            "login",
            "status",
        ])
        .env("CODEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("codex login status did not finish under the sandbox");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_codex_master_reads_its_sign_in_from_the_managed_auth_file_inside_a_keychain_denying_sandbox() {
    let file = codex_sign_in("file");
    assert!(file.contains("Logged in using ChatGPT"), "{file}");
    let keyring = codex_sign_in("keyring");
    assert!(!keyring.contains("Logged in using ChatGPT"), "{keyring}");
}
