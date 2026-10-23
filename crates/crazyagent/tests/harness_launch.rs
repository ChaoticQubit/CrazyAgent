use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret, Session, command,
};
use crazyagent::harnesses::{HarnessStatus, Search, catalog};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const OLLAMA_OPENAI: &str = "http://127.0.0.1:11434/v1";
const OLLAMA_ANTHROPIC: &str = "http://127.0.0.1:11434";
const LOCAL_MODEL: &str = "qwen2.5:0.5b";
const DUMMY_KEY: &str = "sk-test-dummy-launch-key-0001";
static LOCAL_MODEL_OWNER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn private_dir(prefix: &str) -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn installed(id: &str) -> PathBuf {
    let entries = catalog(&Search::from_environment());
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(
        entry.status,
        HarnessStatus::Ready,
        "{id} must be installed for live launch checks"
    );
    std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap()
}

fn request(harness: &str, home: &Path, work: &Path, account: AccountBinding) -> LaunchRequest {
    assert_eq!(account.home(), home);
    LaunchRequest {
        harness: harness.into(),
        executable: installed(harness),
        working_dir: work.to_path_buf(),
        account,
        model: Some(LOCAL_MODEL.into()),
        tools: false,
        sandbox_profile: None,
        limits: LaunchLimits {
            run_time: Duration::from_secs(420),
            ..LaunchLimits::default()
        },
    }
}

fn arguments(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|argument| argument.to_str().unwrap().to_owned())
        .collect()
}

fn environment(command: &std::process::Command) -> BTreeMap<String, String> {
    command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_str().unwrap().to_owned(),
                value.unwrap().to_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn url_account(home: &Path, base_url: &str) -> AccountBinding {
    AccountBinding::Url {
        home: home.to_path_buf(),
        base_url: base_url.into(),
        key: None,
    }
}

#[test]
fn codex_tools_off_command_pins_read_only_config_and_only_the_account_home() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let built = command(&request(
        "codex",
        home.path(),
        work.path(),
        AccountBinding::Subscription {
            home: home.path().to_path_buf(),
        },
    ))
    .unwrap();
    let args = arguments(&built);
    for setting in [
        "cli_auth_credentials_store=\"file\"",
        "model_provider=\"openai\"",
        "model=\"qwen2.5:0.5b\"",
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
        "features.shell_tool=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
    ] {
        assert!(
            args.windows(2).any(|pair| pair == ["-c", setting]),
            "{setting}: {args:?}"
        );
    }
    assert!(args.ends_with(&["app-server".into(), "--listen".into(), "stdio://".into()]));
    let home_text = home.path().to_str().unwrap().to_owned();
    assert_eq!(
        environment(&built),
        BTreeMap::from([
            ("CODEX_HOME".into(), home_text.clone()),
            ("HOME".into(), home_text.clone()),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TMPDIR".into(), home_text),
        ])
    );
    assert_eq!(built.get_current_dir(), Some(work.path()));
}

#[test]
fn api_key_reaches_only_the_child_environment_never_the_arguments() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    for harness in ["codex", "claude-code"] {
        let account = AccountBinding::ApiKey {
            home: home.path().to_path_buf(),
            key: Secret::new(DUMMY_KEY.into()).unwrap(),
            base_url: None,
        };
        let built = command(&request(harness, home.path(), work.path(), account)).unwrap();
        assert!(
            arguments(&built)
                .iter()
                .all(|argument| !argument.contains(DUMMY_KEY)),
            "{harness}"
        );
        let variable = if harness == "codex" {
            "CRAZYAGENT_API_KEY"
        } else {
            "ANTHROPIC_API_KEY"
        };
        assert_eq!(
            environment(&built).get(variable).map(String::as_str),
            Some(DUMMY_KEY)
        );
        assert!(!format!("{:?}", Secret::new(DUMMY_KEY.into()).unwrap()).contains(DUMMY_KEY));
    }
}

#[test]
fn claude_subscription_sets_both_config_directories_and_switches_tools() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let profile = work.path().join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let subscription = || AccountBinding::Subscription {
        home: home.path().to_path_buf(),
    };
    let off = command(&request(
        "claude-code",
        home.path(),
        work.path(),
        subscription(),
    ))
    .unwrap();
    let variables = environment(&off);
    let home_text = home.path().to_str().unwrap();
    assert_eq!(variables["CLAUDE_CONFIG_DIR"], home_text);
    assert_eq!(variables["CLAUDE_SECURESTORAGE_CONFIG_DIR"], home_text);
    assert!(!variables.contains_key("ANTHROPIC_API_KEY"));
    let args = arguments(&off);
    assert!(
        args.windows(2).any(|pair| pair == ["--tools", ""]),
        "{args:?}"
    );
    assert!(!args.contains(&"--bare".to_owned()));
    let mut on = request("claude-code", home.path(), work.path(), subscription());
    on.tools = true;
    assert_eq!(command(&on).unwrap_err().0, "launch_tools_require_sandbox");
    on.sandbox_profile = Some(profile.clone());
    let wrapped = command(&on).unwrap();
    assert_eq!(wrapped.get_program(), "/usr/bin/sandbox-exec");
    let args = arguments(&wrapped);
    assert_eq!(
        args[..3],
        [
            "-f".to_owned(),
            profile.to_str().unwrap().to_owned(),
            on.executable.to_str().unwrap().to_owned()
        ]
    );
    assert!(args.windows(2).any(|pair| pair == ["--tools", "default"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--permission-mode", "bypassPermissions"])
    );
}

#[test]
fn codex_tools_on_runs_without_its_own_sandbox_inside_the_project_sandbox() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let profile = work.path().join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let mut on = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), OLLAMA_OPENAI),
    );
    on.tools = true;
    on.sandbox_profile = Some(profile);
    let built = command(&on).unwrap();
    assert_eq!(built.get_program(), "/usr/bin/sandbox-exec");
    let args = arguments(&built);
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-c", "sandbox_mode=\"danger-full-access\""])
    );
    assert!(
        !args
            .iter()
            .any(|argument| argument.contains("shell_tool=false"))
    );
    assert!(
        args.iter()
            .any(|argument| argument.contains("base_url=\"http://127.0.0.1:11434/v1\""))
    );
}

#[test]
fn unsafe_launch_inputs_are_refused() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let mut bad_model = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), OLLAMA_OPENAI),
    );
    bad_model.model = Some("x\"\nfeatures.shell_tool=true".into());
    assert_eq!(command(&bad_model).unwrap_err().0, "launch_model_invalid");
    let bad_url = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), "http://user:pw@127.0.0.1/v1"),
    );
    assert_eq!(command(&bad_url).unwrap_err().0, "launch_url_invalid");
    let unknown = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), OLLAMA_OPENAI),
    );
    let unknown = LaunchRequest {
        harness: "opencode".into(),
        ..unknown
    };
    assert_eq!(
        command(&unknown).unwrap_err().0,
        "launch_harness_unsupported"
    );
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let open = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), OLLAMA_OPENAI),
    );
    assert_eq!(command(&open).unwrap_err().0, "launch_account_home_unsafe");
    let owner = PathBuf::from(std::env::var_os("HOME").unwrap());
    for forbidden in [owner.join(".codex"), owner.join(".claude")] {
        if !forbidden.is_dir() {
            continue;
        }
        let real = LaunchRequest {
            account: AccountBinding::Subscription {
                home: forbidden.clone(),
            },
            ..request(
                "codex",
                home.path(),
                work.path(),
                url_account(home.path(), OLLAMA_OPENAI),
            )
        };
        let refused = command(&real).unwrap_err().0;
        assert!(
            refused == "launch_account_home_forbidden" || refused == "launch_account_home_unsafe",
            "{refused}"
        );
    }
}

fn run_turn(session: &mut Session, prompt: &str) -> (String, Vec<LaunchEvent>) {
    session.send(prompt).unwrap();
    let mut text = String::new();
    let mut seen = Vec::new();
    let until = Instant::now() + Duration::from_secs(400);
    while Instant::now() < until {
        let Some(event) = session.next_event(Duration::from_secs(5)).unwrap() else {
            continue;
        };
        seen.push(event.clone());
        match event {
            LaunchEvent::Text { text: part } => text.push_str(&part),
            LaunchEvent::TurnFinished { .. } => return (text, seen),
            LaunchEvent::Exited { .. } => panic!("harness exited early: {seen:?}"),
            _ => {}
        }
    }
    panic!("turn did not finish: {seen:?}");
}

fn group_alive(group: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
    rustix::process::test_kill_process_group(pid).is_ok()
}

#[test]
fn live_codex_and_claude_reply_from_the_local_model_and_stop_their_process_groups() {
    let _owner = LOCAL_MODEL_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (harness, base_url) in [("codex", OLLAMA_OPENAI), ("claude-code", OLLAMA_ANTHROPIC)] {
        let home = private_dir("ca-launch-live-");
        let work = private_dir("ca-launch-work-");
        let mut session = Session::start(request(
            harness,
            home.path(),
            work.path(),
            url_account(home.path(), base_url),
        ))
        .unwrap();
        let group = session.process_group();
        assert!(group_alive(group));
        let (text, events) = run_turn(&mut session, "Reply with one short greeting.");
        assert!(
            events.iter().any(|event| matches!(
                event,
                LaunchEvent::Ready {
                    native_session: Some(_)
                }
            )),
            "{harness}: {events:?}"
        );
        assert!(
            events.contains(&LaunchEvent::TurnFinished { success: true }),
            "{harness}: {events:?}"
        );
        assert!(!text.trim().is_empty(), "{harness}: {events:?}");
        session.stop();
        assert!(!group_alive(group), "{harness} process group survived stop");
    }
}

#[test]
fn live_interrupt_stops_a_running_turn_through_the_harness_protocol() {
    let _owner = LOCAL_MODEL_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (harness, base_url) in [("codex", OLLAMA_OPENAI), ("claude-code", OLLAMA_ANTHROPIC)] {
        let home = private_dir("ca-launch-live-");
        let work = private_dir("ca-launch-work-");
        let mut session = Session::start(request(
            harness,
            home.path(),
            work.path(),
            url_account(home.path(), base_url),
        ))
        .unwrap();
        session
            .send("Write the numbers from 1 to 3000 in words, one per line, with no other text.")
            .unwrap();
        let mut seen = Vec::new();
        let mut interrupted = false;
        let until = Instant::now() + Duration::from_secs(400);
        while Instant::now() < until {
            let Some(event) = session.next_event(Duration::from_secs(5)).unwrap() else {
                continue;
            };
            seen.push(event.clone());
            if matches!(event, LaunchEvent::Ready { .. }) && !interrupted {
                session.interrupt().unwrap();
                interrupted = true;
            }
            if matches!(event, LaunchEvent::TurnFinished { .. }) {
                break;
            }
        }
        assert!(interrupted, "{harness}: {seen:?}");
        assert!(
            seen.contains(&LaunchEvent::Interrupted),
            "{harness}: {seen:?}"
        );
        assert!(
            seen.contains(&LaunchEvent::TurnFinished { success: false }),
            "{harness}: {seen:?}"
        );
        session.stop();
    }
}

fn project_profile(work: &Path, home: &Path) -> PathBuf {
    let profile = work.join("project.sb");
    let rules = format!(
        "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\") (subpath \"{}\"))\n",
        std::fs::canonicalize(work).unwrap().display(),
        std::fs::canonicalize(home).unwrap().display()
    );
    std::fs::write(&profile, rules).unwrap();
    profile
}

#[test]
fn live_tools_on_turn_completes_inside_the_project_sandbox() {
    let _owner = LOCAL_MODEL_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (harness, base_url) in [("codex", OLLAMA_OPENAI), ("claude-code", OLLAMA_ANTHROPIC)] {
        let home = private_dir("ca-launch-tools-");
        let work = private_dir("ca-launch-work-");
        let mut on = request(
            harness,
            home.path(),
            work.path(),
            url_account(home.path(), base_url),
        );
        on.tools = true;
        on.sandbox_profile = Some(project_profile(work.path(), home.path()));
        let mut session = Session::start(on).unwrap();
        let (_, events) = run_turn(&mut session, "Reply with one short greeting.");
        assert!(
            events.contains(&LaunchEvent::TurnFinished { success: true }),
            "{harness}: {events:?}"
        );
        session.stop();
    }
}
