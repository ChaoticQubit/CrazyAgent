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

#[cfg(target_os = "macos")]
mod launch {
    use super::*;
    use crate::harnesses::launch::{AccountBinding, LaunchLimits};
    use crate::harnesses::registry::Registry;
    use crate::model_gateway::relay::{LOCAL_PLACEHOLDER, is_relay_token};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    const SENTINEL: &str = "sk-listed-master-saved-key-0123456789abcdef";
    const URL: &str = "http://127.0.0.1:9/v1";

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

    /// One listed master turn's folder: a 0700 private home and worktree under the real
    /// /tmp, an allow-all profile, the stand-in's program, and the engine, drop file and
    /// mailbox its team server is named with.
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
        let (home, work) = (base.join("home"), base.join("work"));
        for place in [&home, &work] {
            std::fs::create_dir(place).unwrap();
            std::fs::set_permissions(place, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = base.join("master.sb");
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        let program = base.join("standin-acp");
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

    /// The stand-in ACP master with one more variable in its account entry. Input: the
    /// manifest line that sets it. Output: the harness, kept for the test run.
    fn standin_with(line: &str) -> &'static Harness {
        let text = include_str!("../../../../tests/fixtures/harness/standin-acp.yaml");
        let model = "      STANDIN_MODEL: \"{model}\"\n";
        assert!(text.contains(model));
        let text = text.replace(model, &format!("{model}      {line}\n"));
        let built = Registry::build(&[("standin-acp", text.as_str())]);
        assert!(built.is_ok(), "{:?}", built.as_ref().err());
        let found = Box::leak(Box::new(built.unwrap())).get("standin-acp");
        assert!(found.is_some());
        found.unwrap()
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
    /// and the placeholder, no relay, its private home and the arguments its manifest
    /// gives; a hand-over that names the engine's TLS roots ends with the engine's value.
    #[test]
    fn a_keyless_listed_master_holds_the_placeholder_and_the_engine_s_own_variables() {
        let harness = standin_with("SSL_CERT_FILE: \"/owner/roots.pem\"");
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
        assert!(given.iter().all(|arg| arg != "--no-tools"));
    }

    /// DEC-180 Task 20 M3: with tools on, the MCP config written in the private home holds
    /// the engine's team server alone, with the command and arguments a native master's
    /// team server has; with tools off it holds no team server and the manifest's tools-off
    /// switch is in the arguments.
    #[test]
    fn a_listed_master_names_the_team_server_only_with_its_tools_on() {
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
        let config = on.request.account.home().join("config/mcp.json");
        assert!(launch.written.contains(&config));
        let text = std::fs::read_to_string(&config).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let servers = parsed["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1);
        let server = &servers[crate::team::tool::SERVER_NAME];
        assert_eq!(server["command"], on.engine.to_str().unwrap());
        let given: Vec<String> = serde_json::from_value(server["args"].clone()).unwrap();
        assert_eq!(given, tools.server_args().unwrap());
        assert!(args(&launch).iter().all(|arg| arg != "--no-tools"));
        let off = turn(false);
        let launch = launched(harness, &off, &handover);
        assert!(launch.is_ok(), "{:?}", launch.as_ref().err());
        let launch = launch.unwrap();
        assert!(args(&launch).iter().any(|arg| arg == "--no-tools"));
        let config = off.request.account.home().join("config/mcp.json");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        assert!(!text.contains(crate::team::tool::SERVER_NAME));
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
}
