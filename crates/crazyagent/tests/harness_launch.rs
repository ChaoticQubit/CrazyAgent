#![cfg(target_os = "macos")]

use crazyagent::harnesses::interactive::{self, KEY_FILE, Run};
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret, Session, command, route,
};
use crazyagent::harnesses::leftovers::{UNCHECKED, Unstopped};
use crazyagent::harnesses::{HarnessStatus, Search, catalog};
use crazyagent::model_gateway::relay::{TOKEN_PREFIX, is_placeholder};
use crazyagent::protocol::CredentialRef;
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
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
        sandbox_profile: Some(project_profile(work, home)),
        unconfined: false,
        git_identity: None,
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
        "features.goals=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
    ] {
        assert!(
            args.windows(2).any(|pair| pair == ["-c", setting]),
            "{setting}: {args:?}"
        );
    }
    assert!(args.ends_with(&["app-server".into(), "--listen".into(), "stdio://".into()]));
    assert_eq!(built.get_program(), "/usr/bin/sandbox-exec");
    let mut start = vec![
        "-f".to_owned(),
        work.path().join("project.sb").to_str().unwrap().to_owned(),
    ];
    start.extend(crazyagent::sandbox::SESSION_SHIM.map(str::to_owned));
    start.push(installed("codex").to_str().unwrap().to_owned());
    assert_eq!(args[..start.len()], start);
    let home_text = home.path().to_str().unwrap().to_owned();
    let mut expected = BTreeMap::from([
        ("CODEX_HOME".into(), home_text.clone()),
        ("HOME".into(), home_text.clone()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TMPDIR".into(), home_text),
        (
            "TMPPREFIX".into(),
            home.path().join("zsh").to_str().unwrap().to_owned(),
        ),
    ]);
    expected.extend(
        crazyagent::sandbox::AGENT_GIT_ENV
            .into_iter()
            .chain(crazyagent::sandbox::AGENT_TLS_ENV)
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    assert_eq!(environment(&built), expected);
    assert_eq!(built.get_current_dir(), Some(work.path()));
}

#[test]
fn a_full_access_agent_starts_without_seatbelt_but_with_the_same_cleared_environment() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let subscription = || AccountBinding::Subscription {
        home: home.path().to_path_buf(),
    };
    let confined = command(&request("codex", home.path(), work.path(), subscription())).unwrap();
    let full = LaunchRequest {
        sandbox_profile: None,
        unconfined: true,
        ..request("codex", home.path(), work.path(), subscription())
    };
    let built = command(&full).unwrap();
    assert_ne!(built.get_program(), "/usr/bin/sandbox-exec");
    assert_eq!(built.get_program(), crazyagent::sandbox::SESSION_SHIM[0]);
    let mut start: Vec<String> = crazyagent::sandbox::SESSION_SHIM[1..]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    start.push(installed("codex").to_str().unwrap().to_owned());
    let args = arguments(&built);
    assert_eq!(args[..start.len()], start);
    assert!(!args.iter().any(|arg| arg.ends_with("project.sb")));
    let confined_args = arguments(&confined);
    assert_eq!(
        args[start.len()..],
        confined_args[confined_args.len() - (args.len() - start.len())..],
        "only the Seatbelt prefix differs"
    );
    assert_eq!(environment(&built), environment(&confined));
    assert_eq!(built.get_current_dir(), Some(work.path()));

    let neither = LaunchRequest {
        sandbox_profile: None,
        ..request("codex", home.path(), work.path(), subscription())
    };
    assert_eq!(command(&neither).unwrap_err().0, "launch_sandbox_required");
    let both = LaunchRequest {
        unconfined: true,
        ..request("codex", home.path(), work.path(), subscription())
    };
    assert_eq!(command(&both).unwrap_err().0, "launch_sandbox_conflict");
}

#[test]
fn a_real_api_key_never_reaches_the_agent_and_a_routed_launch_reads_its_token_from_a_file() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let keyed = || AccountBinding::ApiKey {
        home: home.path().to_path_buf(),
        key: Secret::new(DUMMY_KEY.into()).unwrap(),
        base_url: None,
    };
    let key_file = home.path().join(KEY_FILE);
    for harness in ["codex", "claude-code"] {
        assert_eq!(
            command(&request(harness, home.path(), work.path(), keyed()))
                .unwrap_err()
                .0,
            "launch_environment_refused",
            "{harness}"
        );
        let (routed, relay) = route(request(harness, home.path(), work.path(), keyed())).unwrap();
        let relay = relay.unwrap();
        assert!(is_placeholder(relay.token()));
        let built = command(&routed).unwrap();
        let variables = environment(&built);
        let arguments = arguments(&built);
        no_key_in(&arguments, &variables, harness);
        assert!(!variables.contains_key("ANTHROPIC_API_KEY"), "{harness}");
        assert!(!variables.contains_key("CRAZYAGENT_API_KEY"), "{harness}");
        if harness == "codex" {
            assert_eq!(
                relay.base_url(),
                format!("http://127.0.0.1:{}/v1", relay.port())
            );
            let provider = format!(
                "model_providers.crazyagent={{name=\"CrazyAgent\",base_url=\"{}\",wire_api=\"responses\",auth={{command=\"/bin/cat\",args=[\"{}\"]}}}}",
                relay.base_url(),
                key_file.display()
            );
            assert!(arguments.contains(&provider), "{arguments:?}");
        } else {
            let at = arguments
                .iter()
                .position(|argument| argument == "--settings");
            let settings: serde_json::Value =
                serde_json::from_str(&arguments[at.unwrap() + 1]).unwrap();
            assert_eq!(
                settings["apiKeyHelper"],
                format!("/bin/cat '{}'", key_file.display())
            );
            assert_eq!(
                variables.get("ANTHROPIC_BASE_URL"),
                Some(&format!("http://127.0.0.1:{}", relay.port()))
            );
        }
        let (again, none) = route(routed).unwrap();
        assert!(none.is_none());
        assert_eq!(
            environment(&command(&again).unwrap()),
            variables,
            "{harness}"
        );
    }
    let unsupported = LaunchRequest {
        harness: "not-a-harness".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: work.path().to_path_buf(),
        account: keyed(),
        model: None,
        tools: false,
        sandbox_profile: Some(project_profile(work.path(), home.path())),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    assert_eq!(
        route(unsupported).err().map(|error| error.0).as_deref(),
        Some("launch_key_relay_unsupported")
    );
    assert!(!format!("{:?}", Secret::new(DUMMY_KEY.into()).unwrap()).contains(DUMMY_KEY));
}

fn no_key_in(arguments: &[String], variables: &BTreeMap<String, String>, kind: &str) {
    for text in arguments
        .iter()
        .chain(variables.keys())
        .chain(variables.values())
    {
        assert!(!text.contains(DUMMY_KEY), "{kind}: {text}");
        assert!(!text.contains(TOKEN_PREFIX), "{kind}: {text}");
    }
}

/// Builds every command a keyed account can launch: the piped harnesses masters and headless
/// team agents run, with and without tools, and the interactive apps team agents and research
/// jobs run in panes. Research accounts carry no key at all. No argument or variable of any
/// of them holds the relay token or the provider key.
#[test]
fn no_launch_command_kind_carries_a_relay_token_or_a_provider_key() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    for harness in ["codex", "claude-code"] {
        for tools in [false, true] {
            let mut keyed = request(
                harness,
                home.path(),
                work.path(),
                AccountBinding::ApiKey {
                    home: home.path().to_path_buf(),
                    key: Secret::new(DUMMY_KEY.into()).unwrap(),
                    base_url: None,
                },
            );
            keyed.tools = tools;
            let (routed, relay) = route(keyed).unwrap();
            let relay = relay.unwrap();
            let piped = command(&routed).unwrap();
            no_key_in(
                &arguments(&piped),
                &environment(&piped),
                &format!("piped {harness} tools {tools}"),
            );
            let key_file = interactive::write_key(
                home.path(),
                &Secret::new(relay.token().to_owned()).unwrap(),
            )
            .unwrap();
            assert_eq!(std::fs::read_to_string(&key_file).unwrap(), relay.token());
            let session = interactive::session_id().unwrap();
            let pane = interactive::command(
                &routed,
                None,
                &Run {
                    session_id: &session,
                    key_file: Some(&key_file),
                    resume: false,
                },
            )
            .unwrap();
            no_key_in(
                &pane.argv,
                &pane.env.iter().cloned().collect(),
                &format!("pane {harness} tools {tools}"),
            );
            std::fs::remove_file(&key_file).unwrap();
        }
    }
}

#[test]
fn team_plan_claude_name_launches_claude_code() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    let subscription = || AccountBinding::Subscription {
        home: home.path().to_path_buf(),
    };
    let canonical = command(&request(
        "claude-code",
        home.path(),
        work.path(),
        subscription(),
    ))
    .unwrap();
    let mut named = request("claude-code", home.path(), work.path(), subscription());
    named.harness = "claude".into();
    let alias = command(&named).unwrap();
    assert_eq!(alias.get_program(), canonical.get_program());
    assert_eq!(arguments(&alias), arguments(&canonical));
    assert_eq!(environment(&alias), environment(&canonical));
    named.harness = "claude-codex".into();
    assert_eq!(command(&named).unwrap_err().0, "launch_harness_unsupported");
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
    assert_eq!(variables["CLAUDE_CODE_TMPDIR"], home_text);
    assert!(!variables.contains_key("ANTHROPIC_API_KEY"));
    let args = arguments(&off);
    assert!(
        args.windows(2).any(|pair| pair == ["--tools", ""]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--system-prompt" && pair[1].contains("with no tools")),
        "{args:?}"
    );
    assert!(!args.contains(&"--bare".to_owned()));
    let mut on = request("claude-code", home.path(), work.path(), subscription());
    on.tools = true;
    on.sandbox_profile = None;
    assert_eq!(command(&on).unwrap_err().0, "launch_sandbox_required");
    on.sandbox_profile = Some(profile.clone());
    let wrapped = command(&on).unwrap();
    assert_eq!(wrapped.get_program(), "/usr/bin/sandbox-exec");
    let args = arguments(&wrapped);
    let mut start = vec!["-f".to_owned(), profile.to_str().unwrap().to_owned()];
    start.extend(crazyagent::sandbox::SESSION_SHIM.map(str::to_owned));
    start.push(on.executable.to_str().unwrap().to_owned());
    assert_eq!(args[..start.len()], start);
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
fn no_launch_starts_without_the_project_sandbox_profile() {
    let home = private_dir("ca-launch-home-");
    let work = private_dir("ca-launch-work-");
    for harness in ["codex", "claude-code"] {
        for tools in [false, true] {
            let mut bare = request(
                harness,
                home.path(),
                work.path(),
                url_account(home.path(), OLLAMA_OPENAI),
            );
            bare.tools = tools;
            assert!(command(&bare).is_ok(), "{harness} {tools}");
            bare.sandbox_profile = None;
            assert_eq!(
                command(&bare).unwrap_err().0,
                "launch_sandbox_required",
                "{harness} {tools}"
            );
            for unusable in [
                PathBuf::from("project.sb"),
                work.path().join("missing.sb"),
                work.path().to_path_buf(),
            ] {
                bare.sandbox_profile = Some(unusable);
                assert_eq!(
                    command(&bare).unwrap_err().0,
                    "launch_sandbox_profile_unavailable",
                    "{harness} {tools}"
                );
            }
        }
    }
    let mut session = request(
        "codex",
        home.path(),
        work.path(),
        url_account(home.path(), OLLAMA_OPENAI),
    );
    session.sandbox_profile = None;
    assert_eq!(
        Session::start(session).err().unwrap().0,
        "launch_sandbox_required"
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

const DETACHING_AGENT: &str = r#"#!/bin/sh
[ "$1" = first-exec ] && exit 3
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec "/bin/sh", "-c", q(/usr/bin/nohup /bin/sleep 30 < /dev/null > /dev/null 2>&1 & echo $! > orphan.pid)' < /dev/null > /dev/null 2>&1
echo $$ > leader.pid
exec /bin/sleep 30
"#;

fn private_folder(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::canonicalize(folder).unwrap()
}

fn written_pid(folder: &Path, name: &str) -> Option<rustix::process::Pid> {
    std::fs::read_to_string(folder.join(name))
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .and_then(rustix::process::Pid::from_raw)
}

/// Starts a stand-in Claude Code harness running `script` headless under a tools-off project
/// sandbox in a private folder, once it has run outside the sandbox so its first exec is
/// done. Takes the folder prefix and the script. Returns the folder, the project the script
/// works in, its executable and the session.
fn sandboxed_stand_in(
    prefix: &str,
    script: &str,
) -> (tempfile::TempDir, PathBuf, PathBuf, Session) {
    use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};

    let root = private_dir(prefix);
    let project = private_folder(root.path(), "project");
    let home = private_folder(root.path(), "home");
    let tmp = private_folder(root.path(), "tmp");
    let bin = private_folder(root.path(), "bin");
    let harness = bin.join("claude");
    std::fs::write(&harness, script).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = crazyagent::spawn_gate::status(
        std::process::Command::new(&harness)
            .arg("first-exec")
            .current_dir(&bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(first.code(), Some(3));
    let profile = root.path().join("agent.sb");
    ProjectSandbox::new(
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    )
    .unwrap()
    .allow_read(&bin)
    .unwrap()
    .install(&profile)
    .unwrap();
    let session = Session::start(LaunchRequest {
        harness: "claude-code".into(),
        executable: harness.clone(),
        working_dir: project.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    })
    .unwrap();
    (root, project, harness, session)
}

/// Kills, by id, the warden of every running agent of this user started from `harness`: the
/// child the session shim forked keeps the shim's command line naming the harness after the
/// shim itself became the harness. Takes the executable, unique to this test. Returns the
/// wardens killed, so a caller that expects exactly one can check.
fn kill_wardens_of(harness: &Path) -> Vec<i32> {
    let listing = crazyagent::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-A", "-ww", "-o", "pid=,uid=,command="])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(listing.status.success());
    let user = rustix::process::getuid().as_raw().to_string();
    let own = std::process::id();
    let named = format!(" -- {}", harness.display());
    let text = String::from_utf8_lossy(&listing.stdout);
    let wardens: Vec<i32> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let owner = fields.next()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (owner == user
                && pid as u32 != own
                && command.starts_with("/usr/bin/perl -mPOSIX -e setpgrp(0,")
                && command.contains(&named))
            .then_some(pid)
        })
        .collect();
    for warden in &wardens {
        let pid = rustix::process::Pid::from_raw(*warden).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    }
    wardens
}

#[test]
fn stopping_a_headless_agent_stops_what_its_shells_detached() {
    let (_root, project, _harness, session) =
        sandboxed_stand_in("crazyagent-detached-agent-", DETACHING_AGENT);
    let deadline = Instant::now() + Duration::from_secs(10);
    let (leader, detached) = loop {
        if let (Some(leader), Some(session), Some(orphan)) = (
            written_pid(&project, "leader.pid"),
            written_pid(&project, "descendant.pid"),
            written_pid(&project, "orphan.pid"),
        ) {
            break (leader, [session, orphan]);
        }
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    for process in detached {
        assert_ne!(rustix::process::getpgid(Some(process)), Ok(leader));
        assert_eq!(rustix::process::test_kill_process(process), Ok(()));
    }
    session.stop().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let survivors = loop {
        let survivors: Vec<_> = [leader]
            .into_iter()
            .chain(detached)
            .filter(|process| {
                rustix::process::test_kill_process(*process) != Err(rustix::io::Errno::SRCH)
            })
            .collect();
        if survivors.is_empty() || Instant::now() >= deadline {
            break survivors;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    for process in &survivors {
        let _ = rustix::process::kill_process(*process, rustix::process::Signal::KILL);
    }
    assert!(survivors.is_empty(), "{survivors:?} survived the agent");
}

const LEAVING_AGENT: &str = r#"#!/bin/sh
[ "$1" = first-exec ] && exit 3
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
(/usr/bin/perl -e 'setpgrp(0, 0) or exit 1; open(my $file, ">", "member.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &)
echo $$ > leader.pid
exec /bin/sleep 30
"#;

/// Kills, by id, the processes a test's stand-in agent left, however the test ends.
struct Leftover(Vec<rustix::process::Pid>);

impl Drop for Leftover {
    fn drop(&mut self) {
        for process in &self.0 {
            let _ = rustix::process::kill_process(*process, rustix::process::Signal::KILL);
        }
    }
}

/// Reads a process's parent id with `ps`. Input: the process. Output: the parent, or none
/// once it has ended.
fn parent_of(process: rustix::process::Pid) -> Option<i32> {
    let listing = crazyagent::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &process.as_raw_nonzero().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    String::from_utf8_lossy(&listing.stdout).trim().parse().ok()
}

/// The ids and programs a leftovers report names. Input: the report. Output: them.
fn named_in(report: &str) -> BTreeMap<i32, String> {
    let (_, named) = report.split_once("still running: ").unwrap();
    let (named, _) = named.split_once(". ").unwrap();
    named
        .split(", ")
        .map(|entry| {
            let (pid, program) = entry.split_once(' ').unwrap();
            (pid.parse().unwrap(), program.to_owned())
        })
        .collect()
}

#[test]
fn stopping_a_full_access_headless_agent_names_what_it_left_running_and_holds_until_it_ends() {
    let root = private_dir("crazyagent-leaving-agent-");
    let project = private_folder(root.path(), "project");
    let home = private_folder(root.path(), "home");
    let bin = private_folder(root.path(), "bin");
    let harness = bin.join("claude");
    std::fs::write(&harness, LEAVING_AGENT).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = crazyagent::spawn_gate::status(
        std::process::Command::new(&harness)
            .arg("first-exec")
            .current_dir(&bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(first.code(), Some(3));
    let session = Session::start(LaunchRequest {
        harness: "claude-code".into(),
        executable: harness,
        working_dir: project.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: false,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (leader, descendant, member) = loop {
        if let (Some(leader), Some(descendant), Some(member)) = (
            written_pid(&project, "leader.pid"),
            written_pid(&project, "descendant.pid"),
            written_pid(&project, "member.pid"),
        ) && parent_of(member) == Some(1)
        {
            break (leader, descendant, member);
        }
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    let leftover = Leftover(vec![descendant, member]);
    assert_eq!(rustix::process::getsid(Some(member)), Ok(leader));
    assert_ne!(rustix::process::getsid(Some(descendant)), Ok(leader));

    let left = match session.stop() {
        Err(Unstopped::Left(left)) => left,
        other => panic!(
            "the stop of a Full access agent was reported {other:?} while {descendant:?} and {member:?} kept running"
        ),
    };
    let report = left.plain();
    assert!(
        report.starts_with(
            "Full access cannot stop what the agent started outside its terminal; still running: "
        ),
        "{report}"
    );
    assert_eq!(
        named_in(&report),
        BTreeMap::from([
            (descendant.as_raw_nonzero().get(), "perl".to_owned()),
            (member.as_raw_nonzero().get(), "perl".to_owned()),
        ]),
        "{report}"
    );
    assert_eq!(
        rustix::process::test_kill_process(leader),
        Err(rustix::io::Errno::SRCH)
    );

    let (held, released) = std::sync::mpsc::channel::<()>();
    left.hold(
        Some(CredentialRef::new("leaving-headless").unwrap()),
        held,
        crazyagent::harnesses::leftovers::HoldBy {
            project: None,
            who: "a headless test agent".to_owned(),
        },
        crazyagent::harnesses::leftovers::LeftoverMode::Ask,
    );
    assert_eq!(
        released.recv_timeout(Duration::from_secs(3)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout),
        "the account was let go while the agent's processes still run"
    );
    drop(leftover);
    assert_eq!(
        released.recv_timeout(Duration::from_secs(10)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        "the account stayed held after the agent's processes ended"
    );
}

const ORPHANING_AGENT: &str = r#"#!/bin/sh
[ "$1" = first-exec ] && exit 3
/usr/bin/perl -mPOSIX -e 'defined(my $child = fork()) or exit 1; if ($child == 0) { POSIX::setsid() > 0 or exit 1; open(my $file, ">", "orphan.pid") or exit 1; print $file "$$\n"; close($file); sleep 60; exit 0 } sleep 5' < /dev/null > /dev/null 2>&1 &
echo $$ > leader.pid
exec /bin/sleep 30
"#;

/// SETSID-L2: a Full access headless agent's process that left its session and lost its
/// parent before the stop, running a program whose environment macOS hides, so nothing at the
/// stop links it to the agent, is still named: the follow from the start kept it.
#[test]
fn stopping_a_full_access_headless_agent_names_what_left_its_session_and_parent() {
    let root = private_dir("crazyagent-orphaning-agent-");
    let project = private_folder(root.path(), "project");
    let home = private_folder(root.path(), "home");
    let bin = private_folder(root.path(), "bin");
    let harness = bin.join("claude");
    std::fs::write(&harness, ORPHANING_AGENT).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = crazyagent::spawn_gate::status(
        std::process::Command::new(&harness)
            .arg("first-exec")
            .current_dir(&bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(first.code(), Some(3));
    let session = Session::start(LaunchRequest {
        harness: "claude-code".into(),
        executable: harness,
        working_dir: project.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: false,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (leader, orphan) = loop {
        if let (Some(leader), Some(orphan)) = (
            written_pid(&project, "leader.pid"),
            written_pid(&project, "orphan.pid"),
        ) && parent_of(orphan) == Some(1)
        {
            break (leader, orphan);
        }
        assert!(
            Instant::now() < deadline,
            "the stand-in agent left no orphan"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let leftover = Leftover(vec![orphan]);
    assert_ne!(rustix::process::getsid(Some(orphan)), Ok(leader));

    let left = match session.stop() {
        Err(Unstopped::Left(left)) => left,
        other => panic!(
            "the stop of a Full access agent was reported {other:?} while its orphan {orphan:?} kept running"
        ),
    };
    let report = left.plain();
    assert_eq!(
        named_in(&report).get(&orphan.as_raw_nonzero().get()),
        Some(&"perl".to_owned()),
        "{report}"
    );
    drop(leftover);
}

const FAST_ORPHANING_AGENT: &str = r#"#!/bin/sh
[ "$1" = first-exec ] && exit 3
/usr/bin/perl -mPOSIX -e 'defined(my $child = fork()) or exit 1; if ($child == 0) { POSIX::setsid() > 0 or exit 1; open(my $file, ">", "orphan.pid") or exit 1; print $file "$$\n"; close($file); sleep 60; exit 0 } POSIX::_exit(0)' < /dev/null > /dev/null 2>&1 &
echo $$ > leader.pid
exec /bin/sleep 30
"#;

/// SETSID-L2 r1 L2, the SECAUDIT-SETSID-LEFTOVER option (a) residual: a Full access headless
/// agent's process that leaves its session and loses its parent within microseconds, between
/// two tables the follower takes, running a program whose environment macOS hides, is never
/// known, so the stop reports clean, or unchecked under load, naming nothing, while it keeps
/// running. Pins today's result; a later fix that catches it turns this row red on purpose.
#[test]
fn a_full_access_headless_stop_misses_an_orphan_made_between_two_tables() {
    let root = private_dir("crazyagent-fast-orphan-");
    let project = private_folder(root.path(), "project");
    let home = private_folder(root.path(), "home");
    let bin = private_folder(root.path(), "bin");
    let harness = bin.join("claude");
    std::fs::write(&harness, FAST_ORPHANING_AGENT).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = crazyagent::spawn_gate::status(
        std::process::Command::new(&harness)
            .arg("first-exec")
            .current_dir(&bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(first.code(), Some(3));
    let session = Session::start(LaunchRequest {
        harness: "claude-code".into(),
        executable: harness,
        working_dir: project.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: false,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (leader, orphan) = loop {
        if let (Some(leader), Some(orphan)) = (
            written_pid(&project, "leader.pid"),
            written_pid(&project, "orphan.pid"),
        ) && parent_of(orphan) == Some(1)
        {
            break (leader, orphan);
        }
        assert!(
            Instant::now() < deadline,
            "the stand-in agent left no orphan"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let leftover = Leftover(vec![orphan]);
    assert_ne!(rustix::process::getsid(Some(orphan)), Ok(leader));

    match session.stop() {
        Ok(_) => {}
        Err(Unstopped::Left(left)) => assert_eq!(left.code(), UNCHECKED, "{}", left.plain()),
        Err(other) => panic!("the stop of a Full access agent was reported {other:?}"),
    }
    assert_eq!(
        rustix::process::test_kill_process(orphan),
        Ok(()),
        "the orphan {orphan:?} ended with the stop, so the row no longer shows the residual"
    );
    drop(leftover);
}

#[test]
fn stopping_a_headless_agent_whose_warden_was_killed_reports_cleanup_uncertain() {
    let (_root, project, harness, session) =
        sandboxed_stand_in("crazyagent-wardenless-agent-", DETACHING_AGENT);
    let deadline = Instant::now() + Duration::from_secs(10);
    let detached = loop {
        if let Some(detached) = written_pid(&project, "descendant.pid") {
            break detached;
        }
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    let killed = kill_wardens_of(&harness);
    assert_eq!(
        killed.len(),
        1,
        "one warden per run, killed by id: {killed:?}"
    );
    let stopped = session.stop().map_err(|problem| problem.code());
    let outlived = rustix::process::test_kill_process(detached) == Ok(());
    let _ = rustix::process::kill_process(detached, rustix::process::Signal::KILL);
    assert_eq!(stopped.map(drop), Err("harness_cleanup_uncertain"));
    assert!(
        outlived,
        "with its warden killed the detached process outlives the group"
    );
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
        session.stop().unwrap();
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
        session.stop().unwrap();
    }
}

#[test]
fn live_keyed_codex_and_claude_reach_the_model_through_the_relay_with_their_key_file() {
    let _owner = LOCAL_MODEL_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (harness, base_url) in [("codex", OLLAMA_OPENAI), ("claude-code", OLLAMA_ANTHROPIC)] {
        let home = private_dir("ca-launch-keyed-");
        let work = private_dir("ca-launch-work-");
        let mut session = Session::start(request(
            harness,
            home.path(),
            work.path(),
            AccountBinding::Url {
                home: home.path().to_path_buf(),
                base_url: base_url.into(),
                key: Some(Secret::new(DUMMY_KEY.into()).unwrap()),
            },
        ))
        .unwrap();
        let key_file = home.path().join(KEY_FILE);
        assert!(
            is_placeholder(&std::fs::read_to_string(&key_file).unwrap()),
            "{harness}"
        );
        assert_eq!(
            std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let (text, events) = run_turn(&mut session, "Reply with one short greeting.");
        assert!(
            events.contains(&LaunchEvent::TurnFinished { success: true }),
            "{harness}: {events:?}"
        );
        assert!(!text.trim().is_empty(), "{harness}: {events:?}");
        session.stop().unwrap();
        assert!(
            !key_file.exists(),
            "{harness}: the key file ends with its session"
        );
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
        session.stop().unwrap();
    }
}

/// SECAUDIT harness-interactive-codex-trusted-cwd-project-config: the trust setting a terminal
/// launch gives its folder loads no MCP server the repository's own config names, with tools
/// off or on. The trusted shape is the control: it shows Codex does read that file.
#[test]
fn the_launch_trust_setting_loads_no_mcp_server_from_the_repository() {
    let codex = installed("codex");
    let home = private_dir("ca-trust-home-");
    let code_home = home.path().join(".codex");
    std::fs::create_dir_all(&code_home).unwrap();
    std::fs::write(code_home.join("config.toml"), "project_doc_max_bytes = 0\n").unwrap();
    let repo = private_dir("ca-trust-repo-");
    let root = std::fs::canonicalize(repo.path()).unwrap();
    let git = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .current_dir(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(git.status.success(), "{git:?}");
    std::fs::create_dir_all(root.join(".codex")).unwrap();
    std::fs::write(
        root.join(".codex/config.toml"),
        "[mcp_servers.dummy]\ncommand = \"true\"\n",
    )
    .unwrap();
    let trust = |level: &str| {
        format!(
            "projects={{\"{}\"={{trust_level=\"{level}\"}}}}",
            root.display()
        )
    };
    let list = |args: &[&str]| {
        let output = crazyagent::spawn_gate::output(
            std::process::Command::new(&codex)
                .args(args)
                .current_dir(&root)
                .env_clear()
                .env("HOME", home.path())
                .env("CODEX_HOME", &code_home)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let control = list(&["-c", &trust("trusted"), "mcp", "list"]);
    assert!(control.contains("dummy"), "inconclusive: {control}");
    let launch = trust("untrusted");
    let off = list(&["-c", "mcp_servers={}", "-c", &launch, "mcp", "list"]);
    let on = list(&["-c", &launch, "mcp", "list"]);
    assert!(!off.contains("dummy"), "tools-off launch: {off}");
    assert!(!on.contains("dummy"), "tools-on launch: {on}");
}
