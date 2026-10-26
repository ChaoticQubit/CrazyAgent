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
fn a_lapsed_claude_record_whose_refresh_the_provider_refuses_reads_as_a_revoked_sign_in() {
    let mut case = UseCase::with_harness("claude-code");
    let home = case.home();
    let (keychain, service) = claude_keychain(&home, &claude_record((now() - 60) * 1000));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &claude())),
        "native_profile_sign_in_revoked"
    );
    let stored: Value = serde_json::from_str(&keychain.find(&service)).unwrap();
    assert_eq!(
        (
            &stored["claudeAiOauth"]["accessToken"],
            &stored["claudeAiOauth"]["refreshToken"],
            &stored["claudeAiOauth"]["expiresAt"]
        ),
        (&json!(""), &json!(""), &json!(0)),
        "the real claude did not end the sign-in the token endpoint refused"
    );
    assert!(entries(&agent).is_empty());
}

#[test]
fn a_claude_record_outside_claudes_refresh_window_is_copied_without_running_claude() {
    let mut case = UseCase::with_harness("claude-code");
    let home = case.home();
    let expires = (now() + 10 * 60) * 1000;
    let (keychain, service) = claude_keychain(&home, &claude_record(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN, &claude()).unwrap();
    assert_eq!(
        read_json(&agent.join(".credentials.json"))["claudeAiOauth"]["expiresAt"],
        expires
    );
    assert!(
        !home.join(".claude.json").exists(),
        "claude ran although it would not refresh yet: {:?}",
        entries(&home)
    );
    assert_eq!(
        serde_json::from_str::<Value>(&keychain.find(&service)).unwrap(),
        claude_record(expires)
    );
    drop(copy);
    drop(guard);
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
fn a_custodian_check_waits_for_a_running_renewal_and_the_engine_admits_it_meanwhile() {
    let mut case = UseCase::new();
    let agent = execution(&mut case);
    let renewal = agent.hold_renewal(Duration::ZERO).unwrap();
    let admitted = case.recapture().acquire_use_within(
        &case.host,
        &case.binding,
        &request_id("check-admitted-during-renewal"),
        1,
        NativeProfileUsePurpose::Check,
        None,
    );
    assert!(admitted.is_ok(), "{:?}", admitted.err());
    drop(admitted);
    let mut custody = case.recapture();
    let (host, binding) = (case.host.clone(), case.binding.clone());
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let checked = custody
            .acquire_use_within(
                &host,
                &binding,
                &request_id("check-after-renewal"),
                1,
                NativeProfileUsePurpose::Check,
                Some(Duration::from_secs(20)),
            )
            .map(drop);
        (started.elapsed(), checked)
    });
    std::thread::sleep(Duration::from_millis(400));
    drop(renewal);
    let (waited, checked) = waiter.join().unwrap();
    assert!(checked.is_ok(), "{:?}", checked.err());
    assert!(waited >= Duration::from_millis(400), "{waited:?}");
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

#[test]
#[ignore = "live: renews a real managed Claude account; the leader runs it with CRAZYAGENT_LIVE_CLAUDE_PROFILE"]
fn live_a_lapsing_managed_claude_account_is_renewed_and_its_expiry_moves() {
    let profile = PathBuf::from(
        std::env::var_os("CRAZYAGENT_LIVE_CLAUDE_PROFILE")
            .expect("CRAZYAGENT_LIVE_CLAUDE_PROFILE names the managed profile folder holding home"),
    );
    let renewal = renewal::Renewal::of_profile(agent_copy::Harness::Claude, &claude(), &profile);
    let left = |expires: Duration| {
        i128::from(expires.as_secs()) - i128::from(renewal::since_epoch().unwrap().as_secs())
    };
    let before = match renewal.record() {
        Ok(record) => record.expires,
        Err(error) => panic!("the account record cannot be renewed: {}", error.0),
    };
    assert!(
        before <= renewal::since_epoch().unwrap() + crate::adapters::claude::REFRESH_WINDOW,
        "the token lapses in {} s, outside Claude Code's 5-minute refresh window, so no renewal can be shown now",
        left(before)
    );
    let started = Instant::now();
    let after = renewal.renew(before);
    let elapsed = started.elapsed();
    match after {
        Ok(record) => {
            println!(
                "renewal took {elapsed:?}; the token lapsed in {} s before and lapses in {} s now",
                left(before),
                left(record.expires)
            );
            assert!(
                record.expires > before + Duration::from_secs(3600),
                "the record's expiry did not move: it lapses in {} s (native_profile_credential_expiring)",
                left(record.expires)
            );
        }
        Err(error) => panic!("the renewal ended with {} after {elapsed:?}", error.0),
    }
}
