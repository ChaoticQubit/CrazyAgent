use super::*;
use crate::agent_home::Given;

const STARTUP: &str = "## Project\n\nThe brief.\n";

/// Folds events into a fresh turn. Input: the events. Output: what the turn gathered.
fn gathered(events: Vec<LaunchEvent>) -> Collected {
    let mut collected = Collected::default();
    for event in events {
        collect(&mut collected, event);
    }
    collected
}

/// DEC-180 Task 20 M1: a listed master turn gathers the harness's session id, the text it
/// streams and the tokens it reports, and ends once at turn end; nothing after the end
/// counts, and a turn that reported no tokens has none, shown as not reported.
#[test]
fn a_listed_master_turn_gathers_the_reply_and_its_tokens_and_ends_once() {
    let tokens = Tokens {
        input: 12,
        output: 5,
        cache_read: 3,
        cache_write: 0,
    };
    let answered = gathered(vec![
        LaunchEvent::Ready {
            native_session: Some("s-1".into()),
        },
        LaunchEvent::Text { text: "Hel".into() },
        LaunchEvent::ToolUse {
            name: "crazyagent-team".into(),
        },
        LaunchEvent::Text { text: "lo".into() },
        LaunchEvent::Usage { tokens },
        LaunchEvent::TurnFinished { success: true },
        LaunchEvent::Text {
            text: "late".into(),
        },
        LaunchEvent::TurnError { rate_limited: true },
    ]);
    assert_eq!(answered.session.as_deref(), Some("s-1"));
    assert_eq!(answered.text, "Hello");
    assert_eq!(answered.tokens, Some(tokens));
    assert_eq!(answered.ended, Some(Ok(())));
    let unreported = gathered(vec![
        LaunchEvent::Text { text: "Hi".into() },
        LaunchEvent::TurnFinished { success: true },
    ]);
    assert_eq!(unreported.tokens, None);
    assert_eq!(unreported.ended, Some(Ok(())));
}

/// DEC-180 Task 20 M1: each way a listed master turn fails stops it with a code the master
/// runtime already names, or its own: an empty reply, a failed turn, a rate limit, an
/// interrupt and an exit before the turn ended.
#[test]
fn a_listed_master_turn_that_fails_stops_with_its_code() {
    let cases = [
        (
            vec![LaunchEvent::TurnFinished { success: true }],
            "master_empty_reply",
        ),
        (
            vec![
                LaunchEvent::Text { text: "Hi".into() },
                LaunchEvent::TurnFinished { success: false },
            ],
            TURN_FAILED,
        ),
        (
            vec![
                LaunchEvent::TurnError { rate_limited: true },
                LaunchEvent::TurnFinished { success: false },
            ],
            "gateway_rate_limited",
        ),
        (
            vec![LaunchEvent::TurnError {
                rate_limited: false,
            }],
            TURN_FAILED,
        ),
        (vec![LaunchEvent::Interrupted], CANCELLED),
        (
            vec![
                LaunchEvent::Text { text: "Hi".into() },
                LaunchEvent::Exited { code: Some(1) },
            ],
            PROCESS_EXITED,
        ),
    ];
    for (events, code) in cases {
        let ended = gathered(events).ended;
        assert_eq!(ended, Some(Err(code)), "{code}");
    }
}

/// DEC-180 Task 20 M4: a listed master's instruction file holds what an rcli master's does:
/// its startup instruction, then the owner's app and project instructions.
#[test]
fn a_listed_master_s_instructions_are_the_rcli_master_s() {
    let owner = Given {
        text: "## App instructions\n\nBe brief.\n".into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    assert_eq!(
        instruction_text(STARTUP, owner.clone()),
        format!("{STARTUP}\n{}", owner.text)
    );
    assert_eq!(instruction_text(STARTUP, Given::default()), STARTUP);
}

/// DEC-180 Task 20 Step 4: a listed master turn runs under the owner's master time limit,
/// or under the longest run a launch takes when the owner chose none, never the launch's
/// own ten minute default, with the owner's provider retry and the default stop grace.
#[test]
fn a_listed_master_turn_runs_under_the_owner_s_time_limit_or_the_longest_run() {
    use crate::harnesses::launch::{LaunchLimits, RUN_TIME_MAX};
    use crate::master_time_limit::TimeLimit;
    use crate::provider_retry::ProviderRetry;
    let unlimited = turn_limits(TimeLimit::NoLimit, ProviderRetry::StopAtOnce);
    assert_eq!(unlimited.run_time, RUN_TIME_MAX);
    assert_eq!(unlimited.provider_retry, ProviderRetry::StopAtOnce);
    let limited = turn_limits(TimeLimit::Minutes10, ProviderRetry::HarnessDefault);
    assert_eq!(limited.run_time, std::time::Duration::from_secs(600));
    assert_eq!(limited.provider_retry, ProviderRetry::HarnessDefault);
    assert_eq!(limited.stop_grace, LaunchLimits::default().stop_grace);
}

/// DEC-180 Task 20 Step 4: every way a listed master turn stops names the reason every
/// master stops for, and its harness's run clock under an owner's limit names that limit.
#[test]
fn a_listed_master_stops_for_the_reasons_every_master_stops_for() {
    use crate::master_session::MasterReason;
    use crate::master_time_limit::TimeLimit;
    let reason = |code: &str| super::super::master_runtime::reason_for_error(&failure(code));
    for (code, expected) in [
        (TURN_FAILED, MasterReason::RuntimeUnavailable),
        (PROCESS_EXITED, MasterReason::RuntimeUnavailable),
        (NOT_INSTALLED, MasterReason::RuntimeUnavailable),
        (TOO_OLD, MasterReason::RuntimeUnavailable),
        ("launch_spawn_failed", MasterReason::RuntimeUnavailable),
        ("launch_closed", MasterReason::RuntimeUnavailable),
        ("launch_input_closed", MasterReason::RuntimeUnavailable),
        ("launch_protocol_failed", MasterReason::ProtocolFault),
        ("launch_request_failed", MasterReason::ProtocolFault),
        (
            "launch_account_unsupported",
            MasterReason::InvalidAuthentication,
        ),
        ("account_key_missing", MasterReason::ReadinessUnknown),
        ("launch_deadline", MasterReason::TimeLimit),
    ] {
        assert_eq!(reason(code), expected, "{code}");
    }
    let clock = failure("launch_deadline");
    let timed = super::super::master_runtime::timed_out(clock, TimeLimit::Minutes10);
    assert_eq!(timed.0, "master_time_limit_10");
    assert_eq!(reason(&timed.0), MasterReason::TimeLimit);
}

/// DEC-180 Task 20 Step 4: a listed master session's turns run in a private home of their
/// own below the account's slot home, 0700 at each step, apart from every other session's
/// on the same account, and a home holding a sign-in's record is refused.
#[cfg(unix)]
#[test]
fn a_listed_master_session_runs_in_a_private_home_of_its_own() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::Builder::new()
        .prefix("ca-listed-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = folder.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let harness = crate::master_session::listed_master("standin-acp").unwrap();
    let account = crate::protocol::CredentialRef::new("row-account").unwrap();
    let session = |id: &str| crate::protocol::SessionId::new(id).unwrap();
    let first = listed_home(&base, &session("session-a"), harness, &account);
    assert!(first.is_ok(), "{:?}", first.as_ref().err());
    let first = first.unwrap();
    assert_eq!(first, base.join("session-a/standin-acp/row-account"));
    let steps = [base.join("session-a"), base.join("session-a/standin-acp")];
    for step in steps.iter().chain([&first]) {
        let mode = std::fs::metadata(step).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o700, "{}", step.display());
    }
    let second = listed_home(&base, &session("session-b"), harness, &account).unwrap();
    assert_ne!(second, first);
    let again = listed_home(&base, &session("session-a"), harness, &account);
    assert_eq!(again, Ok(first.clone()));
    std::fs::write(first.join(crate::engine::bootstrap::CODEX_RECORD), b"{}").unwrap();
    let refused = listed_home(&base, &session("session-a"), harness, &account);
    assert_eq!(refused, Err(failure("launch_account_home_holds_sign_in")));
}

#[cfg(target_os = "macos")]
mod launch {
    use super::*;
    use crate::harnesses::launch::{AccountBinding, LaunchLimits};
    use crate::harnesses::registry::Registry;
    use crate::model_gateway::relay::{LOCAL_PLACEHOLDER, is_relay_token};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    const SENTINEL: &str = "sk-listed-master-saved-key-0123456789abcdef";
    const STANDIN: &str = include_str!("../../../../tests/fixtures/harness/standin-acp.yaml");
    const URL: &str = "http://127.0.0.1:9/v1";
    const STANDIN_SH: &str = include_str!("../../../../tests/fixtures/harness/standin.sh");
    const SESSION: &str = "sess-listed";

    /// The stand-in ACP master a test build's registry finds. Output: the harness.
    fn standin() -> &'static Harness {
        let found = crate::harnesses::registry::registry().get("standin-acp");
        assert!(found.is_some());
        found.unwrap()
    }

    /// One pool account of the stand-in's wire format on a loopback address. Input: whether a
    /// key was saved with it. Output: the connection.
    fn connection(keyed: bool) -> ModelConnection {
        let auth = if keyed {
            serde_json::json!({"method": "api_key", "credential_ref": "row-key"})
        } else {
            serde_json::json!({"method": "none"})
        };
        serde_json::from_value(serde_json::json!({
            "id": "row-account",
            "provider": "google",
            "harness": "standin-acp",
            "protocol": "openai-chat",
            "account_ref": "row-login",
            "endpoint": {"kind": "http", "url": URL},
            "model": {"id": "m-1", "author": "google"},
            "auth": auth,
        }))
        .unwrap()
    }

    /// The saved key of a keyed account, which no launch may hold. Output: the key.
    fn saved() -> SecretString {
        SecretString::new(SENTINEL.as_bytes().to_vec()).unwrap()
    }

    /// One listed master turn's folder under the real /tmp: an agent home holding the 0700
    /// private home T18 lays out, `<agent home>/standin-acp/row-account`, a worktree, an
    /// allow-all profile, the stand-in's program, and the engine, drop file and mailbox its
    /// team server is named with.
    struct Turn {
        folder: tempfile::TempDir,
        request: LaunchRequest,
        engine: PathBuf,
        drop: PathBuf,
        mailbox: PathBuf,
    }

    /// Builds one listed master turn's folder and checked request. Input: whether the
    /// master's tools are on. Output: the turn.
    fn turn(tools: bool) -> Turn {
        let folder = tempfile::Builder::new()
            .prefix("ca-listed-master-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let base = folder.path().canonicalize().unwrap();
        let (bin, work) = (base.join("bin"), base.join("work"));
        for place in [&bin, &work] {
            std::fs::create_dir(place).unwrap();
            std::fs::set_permissions(place, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let account = crate::protocol::CredentialRef::new("row-account").unwrap();
        let prepared = crate::harnesses::home::prepare(&base, "standin-acp", &account);
        let home = prepared.unwrap();
        let profile = base.join("master.sb");
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        let program = bin.join("standin-acp");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = LaunchRequest {
            harness: "standin-acp".into(),
            executable: program,
            working_dir: work,
            account: AccountBinding::Url {
                home,
                base_url: URL.into(),
                key: None,
            },
            model: Some("m-1".into()),
            tools,
            sandbox_profile: Some(profile),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        Turn {
            engine: base.join("crazyagent"),
            drop: base.join("drop"),
            mailbox: base.join("mailbox"),
            folder,
            request,
        }
    }

    /// A harness built from one manifest text, kept for the test run. Input: the text.
    /// Output: the harness.
    fn built(text: &str) -> &'static Harness {
        let built = Registry::build(&[("standin-acp", text)]);
        assert!(built.is_ok(), "{:?}", built.as_ref().err());
        let found = Box::leak(Box::new(built.unwrap())).get("standin-acp");
        assert!(found.is_some());
        found.unwrap()
    }

    /// The stand-in ACP master with one more variable in its account entry. Input: the
    /// manifest line that sets it. Output: the harness, kept for the test run.
    fn standin_with(line: &str) -> &'static Harness {
        let model = "      STANDIN_MODEL: \"{model}\"\n";
        assert!(STANDIN.contains(model));
        built(&STANDIN.replace(model, &format!("{model}      {line}\n")))
    }

    /// Builds one keyless or keyed turn's launch with tools off. Inputs: the harness, the
    /// turn and the hand-over. Output: the launch, or the refusal.
    fn launched(
        harness: &Harness,
        turn: &Turn,
        handover: &Handover,
    ) -> Result<ListedLaunch, EngineError> {
        listed_launch(&ListedSpec {
            harness,
            request: &turn.request,
            handover,
            tools: None,
            instructions: STARTUP,
        })
    }

    /// The variables a launch sets, as text. Input: the launch. Output: the pairs.
    fn envs(launch: &ListedLaunch) -> Vec<(String, String)> {
        let text = |text: &std::ffi::OsStr| text.to_string_lossy().into_owned();
        launch
            .command
            .get_envs()
            .filter_map(|(name, value)| Some((text(name), text(value?))))
            .collect()
    }

    /// The value of one variable a launch sets. Inputs: the pairs and the name. Output: the
    /// value, or none when it is not set.
    fn env<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        let found = pairs.iter().find(|(known, _)| known == name);
        found.map(|(_, value)| value.as_str())
    }

    /// The launch's arguments, as text. Input: the launch. Output: the arguments.
    fn args(launch: &ListedLaunch) -> Vec<String> {
        let args = launch.command.get_args();
        args.map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    /// Every file's bytes below one folder, never following a link. Input: the folder.
    /// Output: the contents.
    fn contents(folder: &Path) -> Vec<Vec<u8>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                found.extend(contents(&entry.path()));
            } else if kind.is_file() {
                found.push(std::fs::read(entry.path()).unwrap());
            }
        }
        found
    }

    /// Tells whether a launch holds some text in its arguments, its variables or any file
    /// of its turn folder, the private home included. Inputs: the launch, the turn and the
    /// text. Output: true when it does.
    fn holds(launch: &ListedLaunch, turn: &Turn, needle: &str) -> bool {
        let mut places: Vec<Vec<u8>> = contents(turn.folder.path());
        places.extend(args(launch).into_iter().map(String::into_bytes));
        for (name, value) in envs(launch) {
            places.push(name.into_bytes());
            places.push(value.into_bytes());
        }
        let needle = needle.as_bytes();
        places.iter().any(|place| {
            place
                .windows(needle.len())
                .any(|window| window == needle)
        })
    }

    /// One ACP session update line the stand-in prints. Input: the update. Output: the line.
    fn update(update: serde_json::Value) -> String {
        let params = serde_json::json!({"sessionId": SESSION, "update": update});
        serde_json::json!({"jsonrpc": "2.0", "method": "session/update", "params": params})
            .to_string()
    }

    /// The line of one reply chunk the stand-in streams. Input: the text. Output: the line.
    fn chunk(text: &str) -> String {
        let content = serde_json::json!({"type": "text", "text": text});
        let part = serde_json::json!({"sessionUpdate": "agent_message_chunk", "content": content});
        update(part)
    }

    /// The line of a usage update, which gives an ACP turn no tokens. Output: the line.
    fn usage() -> String {
        let used = serde_json::json!({"sessionUpdate": "usage_update", "used": 12, "size": 900});
        update(used)
    }

    /// Makes one turn's program the stand-in and gives it its session id and the lines its
    /// first turn prints, in its state folder below the private home. Inputs: the turn and
    /// the lines.
    fn script(turn: &Turn, lines: &[String]) {
        std::fs::write(&turn.request.executable, STANDIN_SH).unwrap();
        let state = turn.request.account.home().join("standin");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(state.join("session-id"), SESSION).unwrap();
        let out: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(state.join("turn-1.out"), out).unwrap();
    }

    /// Starts one keyless tools-off turn's stand-in from the command its listed launch built.
    /// Input: the turn, whose request the session takes. Output: the running session, the
    /// stand-in's state folder and the turn folder, kept while the session runs.
    fn started(turn: Turn) -> (Session, PathBuf, tempfile::TempDir) {
        let (handover, relay) = hand_over(standin(), &connection(false), None).unwrap();
        let launch = launched(standin(), &turn, &handover).unwrap();
        let state = turn.request.account.home().join("standin");
        let session = Session::start_listed(turn.request, launch.command, relay, &launch.servers);
        assert!(session.is_ok(), "{:?}", session.as_ref().err());
        (session.unwrap(), state, turn.folder)
    }

    /// DEC-180 Task 20 Step 4: a harness that keeps working after it was asked to stop its
    /// turn is waited on only for its stop grace, and the turn ends cancelled, so a stop never
    /// waits on the harness's whole run clock.
    #[test]
    fn a_listed_master_turn_stopped_on_a_harness_that_keeps_working_ends_in_its_grace() {
        let turn = turn(false);
        let lines = [chunk("hi"), "@standin sleep 20".into(), chunk("never")];
        script(&turn, &lines);
        let (mut session, state, _folder) = started(turn);
        let working = || state.join("turns").exists();
        let began = std::time::Instant::now();
        let driven = drive(&mut session, "keep working", &working);
        let waited = began.elapsed();
        assert!(driven.is_ok(), "{:?}", driven.as_ref().err());
        let collected = driven.unwrap();
        assert_eq!(collected.ended, Some(Err(CANCELLED)));
        assert!(!collected.text.contains("never"));
        assert!(waited < std::time::Duration::from_secs(10), "{waited:?}");
        assert!(session.stop().is_ok());
    }

    /// DEC-180 Task 20 M2 (rv-sa241 ruling t20-q1, conditions 2 to 4 and 6): a keyed listed
    /// master's harness gets a relay started from its own connection, fresh for each start,
    /// and holds that relay's token in the one name its manifest lists, never the saved key;
    /// a keyed account without its key, a keyless one with a key and a manifest that wraps
    /// the token in other text are refused.
    #[test]
    fn a_keyed_listed_master_holds_only_its_own_relay_token_in_its_listed_name() {
        let harness = standin();
        let handed = hand_over(harness, &connection(true), Some(saved()));
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, relay) = handed.unwrap();
        assert!(relay.is_some());
        let relay = relay.unwrap();
        assert!(is_relay_token(relay.token()));
        assert_eq!(handover.token(), relay.token());
        assert_eq!(handover.base_url(), relay.base_url());
        let again = hand_over(harness, &connection(true), Some(saved()));
        assert!(again.is_ok(), "{:?}", again.as_ref().err());
        let (again, _relay) = again.unwrap();
        assert_ne!(again.token(), relay.token());
        let turn = turn(false);
        let launch = launched(harness, &turn, &handover);
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        let pairs = envs(&launch);
        assert_eq!(env(&pairs, "OPENAI_API_KEY"), Some(relay.token()));
        let base_url = relay.base_url();
        assert_eq!(env(&pairs, "OPENAI_BASE_URL"), Some(base_url.as_str()));
        assert_eq!(env(&pairs, "STANDIN_MODEL"), Some("m-1"));
        let holding = pairs.iter().filter(|(_, value)| value.contains(relay.token()));
        let names: Vec<&str> = holding.map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["OPENAI_API_KEY"]);
        assert!(!args(&launch).iter().any(|arg| arg.contains(relay.token())));
        assert!(!holds(&launch, &turn, SENTINEL));
        let keyless = hand_over(harness, &connection(false), Some(saved()));
        assert!(keyless.is_err());
        assert!(hand_over(harness, &connection(true), None).is_err());
        let wrapping = standin_with("AUTH_HEADER: \"Bearer {token}\"");
        let wrapped = hand_over(wrapping, &connection(true), Some(saved()));
        assert!(wrapped.is_ok(), "{:?}", wrapped.as_ref().err());
        let (wrapped, _relay) = wrapped.unwrap();
        assert!(launched(wrapping, &turn, &wrapped).is_err());
    }

    /// DEC-180 Task 20 M2: a keyless listed master's harness gets the account's own address
    /// and the placeholder, no relay, its private home, the arguments its manifest gives and
    /// the engine's TLS roots, which no account entry can hand over any more: the env-name
    /// rule refuses an `SSL_` name when the manifest loads.
    #[test]
    fn a_keyless_listed_master_holds_the_placeholder_and_the_engine_s_own_variables() {
        let harness = standin();
        let handed = hand_over(harness, &connection(false), None);
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, relay) = handed.unwrap();
        assert!(relay.is_none());
        assert_eq!(handover.token(), LOCAL_PLACEHOLDER);
        let turn = turn(false);
        let launch = launched(harness, &turn, &handover);
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        let pairs = envs(&launch);
        assert_eq!(env(&pairs, "OPENAI_API_KEY"), Some(LOCAL_PLACEHOLDER));
        assert_eq!(env(&pairs, "OPENAI_BASE_URL"), Some(handover.base_url()));
        let home = turn.request.account.home().to_str().unwrap();
        assert_eq!(env(&pairs, "HOME"), Some(home));
        let roots = crate::sandbox::AGENT_TLS_ENV[0].1;
        assert_eq!(env(&pairs, "SSL_CERT_FILE"), Some(roots));
        assert_eq!(env(&pairs, "GIT_CONFIG_GLOBAL"), Some("/dev/null"));
        let given = args(&launch);
        let pair = ["--home".to_owned(), home.to_owned()];
        assert!(given.windows(2).any(|window| window == pair));
    }

    /// DEC-180 Task 20 M3 (after T19b-1): with tools on, the launch hands the ACP session the
    /// engine's team server alone, with the command and arguments a native master's team
    /// server has, never in an argument, and the MCP config in the private home names no team
    /// server; with tools off it hands none and the manifest's tools-off switch is an argument.
    #[test]
    fn a_listed_master_hands_its_session_the_team_server_only_with_its_tools_on() {
        let harness = standin();
        let handed = hand_over(harness, &connection(false), None);
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, _) = handed.unwrap();
        let on = turn(true);
        let tools = MasterTools {
            engine: &on.engine,
            drop: &on.drop,
            mailbox: &on.mailbox,
            off: &[],
        };
        let launch = listed_launch(&ListedSpec {
            harness,
            request: &on.request,
            handover: &handover,
            tools: Some(tools),
            instructions: STARTUP,
        });
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        let team = EngineServer {
            name: crate::team::tool::SERVER_NAME.into(),
            command: on.engine.to_str().unwrap().into(),
            args: tools.server_args().unwrap(),
        };
        assert_eq!(launch.servers, [team]);
        let given = args(&launch);
        assert!(given.iter().all(|arg| arg != "--no-tools"));
        assert!(given.iter().all(|arg| !arg.contains("team-mcp")));
        let config = on.request.account.home().join("config/mcp.json");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        assert!(!text.contains(crate::team::tool::SERVER_NAME));
        let off = turn(false);
        let launch = launched(harness, &off, &handover);
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        assert!(launch.servers.is_empty());
        assert!(args(&launch).iter().any(|arg| arg == "--no-tools"));
        let config = off.request.account.home().join("config/mcp.json");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        assert!(!text.contains(crate::team::tool::SERVER_NAME));
    }

    /// DEC-180 Task 20 (guards): a listed master launch never runs unconfined, where its
    /// hand-over exception would reach a process no agent profile fences, and a tools-off
    /// master on a harness whose manifest says its tools cannot be switched off is refused.
    #[test]
    fn a_listed_master_launch_is_refused_unconfined_and_tools_off_without_a_switch() {
        let harness = standin();
        let handed = hand_over(harness, &connection(false), None);
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, _) = handed.unwrap();
        let mut open = turn(false);
        open.request.sandbox_profile = None;
        open.request.unconfined = true;
        let refused = launched(harness, &open, &handover).err();
        let code = refused.map(|error| error.0);
        assert_eq!(code.as_deref(), Some("launch_environment_refused"));
        let switch = "  tools_off:\n    - \"--no-tools\"\n";
        assert!(STANDIN.contains(switch));
        let limited = STANDIN.replace(switch, "") + "tools_limited: \"no switch\"\n";
        let limited = built(&limited);
        assert!(!limited.tools_off());
        let handed = hand_over(limited, &connection(false), None);
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, _) = handed.unwrap();
        let refused = launched(limited, &turn(false), &handover).err();
        let code = refused.map(|error| error.0);
        assert_eq!(code.as_deref(), Some("launch_tools_off_unsupported"));
    }

    /// DEC-180 Task 20 M4: the instruction text is written whole, 0600, where the manifest
    /// says below the private home.
    #[test]
    fn a_listed_master_writes_its_instructions_where_its_manifest_says() {
        let harness = standin();
        let handed = hand_over(harness, &connection(false), None);
        assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
        let (handover, _) = handed.unwrap();
        let turn = turn(false);
        let launch = launched(harness, &turn, &handover);
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        let file = turn.request.account.home().join("config/AGENTS.md");
        assert!(launch.written.contains(&file));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), STARTUP);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// DEC-180 Task 20 M1: a stand-in ACP master started from its listed launch answers one
    /// prompt: the prompt reaches it in its ACP prompt, and its session id, its reply and its
    /// turn end reach what the turn gathered; its usage update gives no tokens, so the turn's
    /// usage reads not reported.
    #[test]
    fn a_listed_master_turn_drives_its_harness_to_the_reply() {
        let turn = turn(false);
        script(&turn, &[chunk("hi"), usage()]);
        let (mut session, state, _folder) = started(turn);
        let driven = drive(&mut session, "say hi", &|| false);
        assert!(driven.is_ok(), "{:?}", driven.as_ref().err());
        let collected = driven.unwrap();
        assert_eq!(collected.session.as_deref(), Some(SESSION));
        assert_eq!(collected.text, "hi");
        assert_eq!(collected.tokens, None);
        assert_eq!(collected.ended, Some(Ok(())));
        assert!(session.stop().is_ok());
        let sent = std::fs::read_to_string(state.join("stdin-1")).unwrap();
        let prompted = |line: &str| line.contains("session/prompt") && line.contains("say hi");
        assert!(sent.lines().any(prompted));
    }

    /// DEC-180 Task 20 M6: a stop while the stand-in works on its turn asks it to cancel the
    /// turn through ACP, once, and the turn ends cancelled with the text streamed before the
    /// stop and nothing after it.
    #[test]
    fn a_listed_master_turn_stopped_mid_turn_ends_cancelled() {
        let turn = turn(false);
        let lines = [chunk("hi"), "@standin wait".into(), chunk("never")];
        script(&turn, &lines);
        let (mut session, state, _folder) = started(turn);
        let working = || state.join("turns").exists();
        let driven = drive(&mut session, "stop me", &working);
        assert!(driven.is_ok(), "{:?}", driven.as_ref().err());
        let collected = driven.unwrap();
        assert_eq!(collected.text, "hi");
        assert_eq!(collected.ended, Some(Err(CANCELLED)));
        assert!(session.stop().is_ok());
        let sent = std::fs::read_to_string(state.join("stdin-1")).unwrap();
        assert_eq!(sent.matches("session/cancel").count(), 1);
    }
}
