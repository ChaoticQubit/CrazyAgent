use super::*;
use crate::harnesses::launch::{CLAUDE_NATIVE_TEAM_TOOLS, LaunchLimits};
use std::os::unix::fs::PermissionsExt;

const SESSION: &str = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
const KEY: &str = "sk-interactive-test-7731";
const SWAPPED_SECRET_MARKER: &str = "owner-secret-fixture-8818";
const SWAP_TIME: std::time::Duration = std::time::Duration::from_secs(20);
const SWAP_LAUNCHES_MAX: u64 = 20_000;

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-interactive-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let case = Self { root };
        for folder in ["home", "work", "bin"] {
            std::fs::create_dir(case.path().join(folder)).unwrap();
            std::fs::set_permissions(
                case.path().join(folder),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        std::fs::write(
            case.profile(),
            format!(
                "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\"))\n",
                case.path().display()
            ),
        )
        .unwrap();
        case
    }

    fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn home(&self) -> PathBuf {
        self.path().join("home")
    }

    fn profile(&self) -> PathBuf {
        self.path().join("project.sb")
    }

    fn executable(&self, name: &str, script: &str) -> PathBuf {
        let path = self.path().join("bin").join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn request(&self, harness: &str, account: AccountBinding, tools: bool) -> LaunchRequest {
        LaunchRequest {
            harness: harness.into(),
            executable: self.executable(harness, "#!/bin/sh\nexit 0\n"),
            working_dir: self.path().join("work"),
            account,
            model: None,
            tools,
            sandbox_profile: Some(self.profile()),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        }
    }

    fn subscription(&self) -> AccountBinding {
        AccountBinding::Subscription { home: self.home() }
    }

    fn keyed(&self, base_url: Option<&str>) -> AccountBinding {
        let key = Secret::new(KEY.into()).unwrap();
        match base_url {
            Some(base_url) => AccountBinding::Url {
                home: self.home(),
                base_url: base_url.into(),
                key: Some(key),
            },
            None => AccountBinding::ApiKey {
                home: self.home(),
                key,
                base_url: None,
            },
        }
    }
}

fn run(key_file: Option<&Path>) -> Run<'_> {
    Run {
        session_id: SESSION,
        key_file,
        resume: false,
    }
}

fn with_git_env(mut env: Vec<(String, String)>) -> Vec<(String, String)> {
    let home = env
        .iter()
        .find(|(name, _)| name == "HOME")
        .map(|(_, home)| PathBuf::from(home))
        .unwrap();
    env.extend(
        toolchains::agent_env(&home, &ToolHomes::owner().unwrap())
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.into_string().unwrap())),
    );
    let prefix = env
        .iter()
        .find(|(name, _)| name == "TMPDIR")
        .map(|(_, tmp)| crate::sandbox::shell_tmp_prefix(Path::new(tmp)));
    if let Some(prefix) = prefix {
        env.push(("TMPPREFIX".to_owned(), prefix.to_str().unwrap().to_owned()));
    }
    env.extend(
        crate::sandbox::AGENT_GIT_ENV
            .into_iter()
            .chain(crate::sandbox::AGENT_TLS_ENV)
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    env
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn no_secret(command: &InteractiveCommand) {
    assert!(command.argv.iter().all(|argument| !argument.contains(KEY)));
    assert!(
        command
            .env
            .iter()
            .all(|(name, value)| !name.contains(KEY) && !value.contains(KEY))
    );
}

#[test]
fn every_catalog_harness_has_its_own_interactive_app_except_rcli() {
    let case = Case::new();
    let expected: [(&str, &[&str], Option<&str>); 20] = [
        ("opencode", &[], Some("--model")),
        ("gemini-cli", &[], Some("--model")),
        ("cursor-agent", &[], Some("--model")),
        ("fx", &[], None),
        ("antigravity", &[], None),
        ("openclaw", &["tui"], None),
        ("pi", &[], Some("--model")),
        ("amp", &[], None),
        ("goose", &["session"], None),
        ("aider", &[], Some("--model")),
        ("qwen-code", &[], Some("--model")),
        ("crush", &[], None),
        ("droid", &[], None),
        ("kiro", &["chat"], None),
        ("copilot", &[], Some("--model")),
        ("cline", &[], None),
        ("continue", &[], None),
        ("openhands", &[], None),
        ("auggie", &[], None),
        ("kimi", &[], None),
    ];
    let ids: Vec<&str> = crate::harnesses::specs()
        .iter()
        .map(|spec| spec.id)
        .collect();
    assert_eq!(ids.len(), expected.len() + 3);
    for id in ids {
        let wanted = expected.iter().find(|(name, _, _)| *name == id);
        match (id, support(id)) {
            ("codex" | "claude-code", Support::Terminal) => {}
            ("rcli", Support::HeadlessOnly { reason }) => {
                assert_eq!(reason, "runs only as a CrazyAgent master");
            }
            (_, Support::Unavailable { reason }) if wanted.is_some() => {
                assert_eq!(reason, "CrazyAgent cannot read its replies yet");
            }
            (id, other) => panic!("{id}: {other:?}"),
        }
    }
    assert_eq!(support("claude"), Support::Terminal);
    assert!(matches!(support("unknown"), Support::HeadlessOnly { .. }));

    for (id, subcommand, model_flag) in expected {
        let request = case.request(id, case.subscription(), true);
        let built = command(&request, None, &run(None)).unwrap();
        let mut argv = strings(&[
            SANDBOX_EXEC,
            "-f",
            case.profile().to_str().unwrap(),
            request.executable.to_str().unwrap(),
        ]);
        argv.extend(strings(subcommand));
        assert_eq!(built.argv, argv, "{id}");
        assert_eq!(built.cwd, case.path().join("work"), "{id}");
        assert_eq!(built.transcript, Transcript::Unavailable, "{id}");
        assert_eq!(
            built.env,
            with_git_env(vec![
                ("HOME".to_owned(), case.home().to_str().unwrap().to_owned()),
                ("PATH".to_owned(), tools_on_path()),
                ("LANG".to_owned(), LOCALE.to_owned()),
                ("LC_ALL".to_owned(), LOCALE.to_owned()),
            ]),
            "{id}"
        );

        let mut chosen = case.request(id, case.subscription(), true);
        chosen.model = Some("model-1".into());
        match model_flag {
            Some(flag) => {
                let built = command(&chosen, None, &run(None)).unwrap();
                assert_eq!(
                    built.argv[built.argv.len() - 2..],
                    strings(&[flag, "model-1"])
                );
            }
            None => assert_eq!(
                command(&chosen, None, &run(None)).unwrap_err().0,
                "launch_model_unsupported",
                "{id}"
            ),
        }
        let off = case.request(id, case.subscription(), false);
        assert_eq!(
            command(&off, None, &run(None)).unwrap_err().0,
            "launch_tools_off_unsupported",
            "{id}: tools off is never claimed without a verified switch"
        );
        let keyed = case.request(id, case.keyed(None), true);
        let key_file = case.path().join("key");
        assert_eq!(
            command(&keyed, None, &run(Some(&key_file))).unwrap_err().0,
            "launch_account_unsupported",
            "{id}"
        );
    }
    let rcli = case.request("rcli", case.subscription(), true);
    assert_eq!(
        command(&rcli, None, &run(None)).unwrap_err().0,
        "launch_harness_unsupported"
    );
}

/// A tools-off Codex agent runs its own app with `--no-daemon` and the headless settings,
/// never through the bridge: Codex 0.164's shared app server keeps unified exec on, so a
/// session that turns it off refuses that server and the app exits. It also turns off
/// `request_user_input`, the one tool those settings leave on. With tools on, Codex
/// runs its app through the bridge script (K2b), which gets the executable and an empty
/// resume id before the settings, the same for the app server it starts first and the app.
#[test]
fn a_tools_off_codex_runs_its_app_without_the_shared_app_server() {
    let case = Case::new();
    let mut request = case.request("codex", case.subscription(), false);
    request.model = Some("gpt-5.5".into());
    let built = command(&request, None, &run(None)).unwrap();
    let mut argv = strings(&[
        SANDBOX_EXEC,
        "-f",
        case.profile().to_str().unwrap(),
        request.executable.to_str().unwrap(),
        "--no-daemon",
    ]);
    for setting in [
        "cli_auth_credentials_store=\"file\"",
        "model_provider=\"openai\"",
        "model=\"gpt-5.5\"",
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
        "features.shell_tool=false",
        "features.apply_patch_freeform=false",
        "features.unified_exec=false",
        "features.view_image=false",
        "features.multi_agent=false",
        "features.code_mode=false",
        "features.apps=false",
        "features.plugins=false",
        "features.goals=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
        "tools.experimental_request_user_input.enabled=false",
        "check_for_update_on_startup=false",
    ] {
        argv.extend(strings(&["-c", setting]));
    }
    argv.extend(strings(&[
        "-c",
        &format!(
            "projects={{\"{}\"={{trust_level=\"trusted\"}}}}",
            built.cwd.display()
        ),
    ]));
    argv.extend(strings(&["-c", "tui.alternate_screen=\"never\""]));
    assert_eq!(built.argv, argv);
    let home = case.home().to_str().unwrap().to_owned();
    assert_eq!(
        built.env,
        with_git_env(vec![
            ("HOME".to_owned(), home.clone()),
            ("PATH".to_owned(), TOOLS_OFF_PATH.to_owned()),
            ("LANG".to_owned(), LOCALE.to_owned()),
            ("LC_ALL".to_owned(), LOCALE.to_owned()),
            ("CODEX_HOME".to_owned(), home.clone()),
            ("TMPDIR".to_owned(), home),
        ])
    );
    assert_eq!(
        built.transcript,
        Transcript::CodexRollouts {
            sessions: case.home().join("sessions")
        }
    );

    assert!(
        !built
            .argv
            .iter()
            .any(|argument| argument == CODEX_BRIDGE_SCRIPT)
    );

    let on = case.request("codex", case.subscription(), true);
    let built = command(&on, None, &run(None)).unwrap();
    assert_eq!(
        built.argv[..9],
        strings(&[
            SANDBOX_EXEC,
            "-f",
            case.profile().to_str().unwrap(),
            "/bin/sh",
            "-c",
            CODEX_BRIDGE_SCRIPT,
            "codex-bridge",
            on.executable.to_str().unwrap(),
            "",
        ])
    );
    assert!(!built.argv.iter().any(|argument| argument == "--no-daemon"));
    assert!(
        !built
            .argv
            .iter()
            .any(|argument| argument == "tools.experimental_request_user_input.enabled=false")
    );
    for setting in [
        "sandbox_mode=\"danger-full-access\"",
        "approval_policy=\"never\"",
        "features.goals=false",
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
        "mcp_servers={}",
    ] {
        assert!(
            built.argv.iter().any(|argument| argument == setting),
            "{setting}"
        );
    }
    assert!(built.env.contains(&("PATH".to_owned(), tools_on_path())));
}

/// A resumed Codex or Claude Code app continues the conversation it recorded under the
/// stored id: Codex with tools on through the bridge's resume id in place of the empty one,
/// which the bridge runs as the `resume` subcommand after its settings, Codex with tools
/// off through that `resume` subcommand after its own settings, and Claude Code through
/// `--resume` in place of a new `--session-id`, with the same transcript source.
#[test]
fn a_resumed_harness_continues_the_conversation_it_recorded() {
    let case = Case::new();
    let resumed = Run {
        resume: true,
        ..run(None)
    };
    let off = case.request("codex", case.subscription(), false);
    let fresh = command(&off, None, &run(None)).unwrap();
    let again = command(&off, None, &resumed).unwrap();
    let mut argv = fresh.argv.clone();
    argv.extend(strings(&["resume", SESSION]));
    assert_eq!(again.argv, argv);
    assert_eq!(again.transcript, fresh.transcript);
    assert_eq!(again.env, fresh.env);

    let codex = case.request("codex", case.subscription(), true);
    let fresh = command(&codex, None, &run(None)).unwrap();
    let again = command(&codex, None, &resumed).unwrap();
    let resume = fresh
        .argv
        .iter()
        .position(|argument| argument == "codex-bridge")
        .unwrap()
        + 2;
    assert_eq!(fresh.argv[resume], "");
    let mut argv = fresh.argv.clone();
    argv[resume] = SESSION.into();
    assert_eq!(again.argv, argv);
    assert_eq!(again.transcript, fresh.transcript);
    assert_eq!(again.env, fresh.env);
    let unknown = Run {
        session_id: "not-a-uuid",
        ..resumed
    };
    for request in [&codex, &off] {
        assert_eq!(
            command(request, None, &unknown).unwrap_err().0,
            "launch_session_id_invalid"
        );
    }

    let claude = case.request("claude-code", case.subscription(), false);
    let fresh = command(&claude, None, &run(None)).unwrap();
    let again = command(&claude, None, &resumed).unwrap();
    let at = fresh
        .argv
        .iter()
        .position(|argument| argument == "--session-id")
        .unwrap();
    let mut argv = fresh.argv.clone();
    argv[at] = "--resume".into();
    assert_eq!(argv[at + 1], SESSION);
    assert_eq!(again.argv, argv);
    assert!(!again.argv.iter().any(|argument| argument == "--session-id"));
    assert_eq!(again.transcript, fresh.transcript);
    assert_eq!(
        command(&claude, None, &unknown).unwrap_err().0,
        "launch_session_id_invalid"
    );
}

/// A Claude Code app renders its system prompt fresh on every request (QR2): Claude Code
/// 2.1.309 otherwise records the prompt of a conversation's first request and sends that
/// record on every resume, so a relaunch after an instructions edit would run with the old
/// text. A signed-in launch, where recording is on by default, and a keyed `--bare` one
/// both turn it off, fresh and resumed, tools on and off.
#[test]
fn a_claude_app_renders_its_system_prompt_fresh_so_a_resume_runs_with_the_relaunch_s_text() {
    let case = Case::new();
    let key = case.keyed(None);
    let key_file = write_key(&case.home(), key.key().unwrap()).unwrap();
    for keyed in [false, true] {
        let fresh = run(keyed.then_some(key_file.as_path()));
        let resumed = Run {
            resume: true,
            ..fresh
        };
        for tools in [true, false] {
            let account = if keyed {
                case.keyed(None)
            } else {
                case.subscription()
            };
            let request = case.request("claude-code", account, tools);
            assert_eq!(claude_bare(&request.account), keyed);
            for launch in [fresh, resumed] {
                let built = command(&request, None, &launch).unwrap();
                let at = built
                    .argv
                    .iter()
                    .position(|argument| argument == "--system-prompt-snapshot")
                    .unwrap_or_else(|| panic!("no snapshot setting: {:?}", built.argv));
                assert_eq!(built.argv[at + 1], "off", "{:?}", built.argv);
                assert_eq!(
                    built
                        .argv
                        .iter()
                        .filter(|argument| *argument == "--system-prompt-snapshot")
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn a_keyed_codex_reads_its_key_from_the_key_file_and_never_from_its_environment() {
    let case = Case::new();
    let mut request = case.request(
        "codex",
        case.keyed(Some("https://models.example.test/v1")),
        true,
    );
    request.executable = case.executable("codex-env", "#!/bin/sh\nexec /usr/bin/env\n");
    assert_eq!(
        command(&request, None, &run(None)).unwrap_err().0,
        "launch_key_file_invalid"
    );
    let key_file = write_key(&case.home(), request.account.key().unwrap()).unwrap();
    let built = command(&request, None, &run(Some(&key_file))).unwrap();
    no_secret(&built);
    assert_eq!(
        built.argv[..9],
        strings(&[
            SANDBOX_EXEC,
            "-f",
            case.profile().to_str().unwrap(),
            "/bin/sh",
            "-c",
            CODEX_BRIDGE_SCRIPT,
            "codex-bridge",
            request.executable.to_str().unwrap(),
            "",
        ])
    );
    let provider = format!(
        "model_providers.crazyagent={{name=\"CrazyAgent\",base_url=\"https://models.example.test/v1\",wire_api=\"responses\",auth={{command=\"/bin/cat\",args=[\"{}\"]}}}}",
        key_file.display()
    );
    assert!(built.argv.contains(&provider), "{:?}", built.argv);
    let helper = crate::spawn_gate::output(
        std::process::Command::new("/bin/cat")
            .arg(&key_file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert_eq!(String::from_utf8(helper.stdout).unwrap(), KEY);

    #[cfg(target_os = "macos")]
    {
        let output = crate::spawn_gate::output(
            std::process::Command::new(&built.argv[0])
                .args(&built.argv[1..])
                .env_clear()
                .envs(built.env.iter().map(|(name, value)| (name, value)))
                .current_dir(&built.cwd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        let seen = String::from_utf8(output.stdout).unwrap();
        assert!(!seen.contains(KEY), "{seen}");
        assert!(!seen.contains("CRAZYAGENT_API_KEY"), "{seen}");
        assert!(
            seen.lines()
                .any(|line| line == format!("CODEX_HOME={}", case.home().display()))
        );
    }

    let quoted = case.path().join("say \"hi\"");
    std::fs::create_dir(&quoted).unwrap();
    assert_eq!(
        command(&request, None, &run(Some(&quoted.join("key"))))
            .unwrap_err()
            .0,
        "launch_key_file_invalid"
    );
}

/// K2b: the real bridge script starts the Codex app server on its control socket, then the
/// app attached to it with `--remote unix://` and the resume id, returns the app's exit code
/// and stops the app server; with no socket from the app server the app runs on its own.
#[cfg(target_os = "macos")]
#[test]
fn the_codex_bridge_attaches_the_app_to_its_app_server_and_stops_the_server_after() {
    let case = Case::new();
    let home = case.home();
    std::fs::create_dir_all(home.join("app-server-control")).unwrap();
    let calls = case.path().join("calls");
    let server = case.path().join("server.pid");
    let stand_in = |name: &str, server_runs: &str| {
        case.executable(
            name,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = app-server ]; then {server_runs}; fi\nexit 7\n",
                calls.display()
            ),
        )
    };
    let bridge = |codex: &Path, resume: &str| {
        crate::spawn_gate::output(
            std::process::Command::new("/bin/sh")
                .args(["-c", CODEX_BRIDGE_SCRIPT, "codex-bridge"])
                .arg(codex)
                .args([resume, "-c", "model=\"gpt-5.5\""])
                .env_clear()
                .env("CODEX_HOME", &home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap()
    };
    let called = || -> Vec<String> {
        let text = std::fs::read_to_string(&calls).unwrap();
        std::fs::remove_file(&calls).unwrap();
        text.lines().map(str::to_owned).collect()
    };

    let attached = stand_in(
        "codex-attached",
        &format!(
            "echo $$ > '{}'; exec /usr/bin/nc -dlU \"$CODEX_HOME/app-server-control/app-server-control.sock\"",
            server.display()
        ),
    );
    let output = bridge(&attached, SESSION);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        called(),
        [
            "app-server --listen unix:// -c model=\"gpt-5.5\"".to_owned(),
            format!("--remote unix:// -c model=\"gpt-5.5\" resume {SESSION}"),
        ]
    );
    let pid = std::fs::read_to_string(&server).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::spawn_gate::output(
        std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap()
    .status
    .success()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the app server {pid} still runs"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let alone = stand_in("codex-alone", "exit 0");
    let output = bridge(&alone, "");
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        called(),
        [
            "app-server --listen unix:// -c model=\"gpt-5.5\"",
            "-c model=\"gpt-5.5\"",
        ]
    );
}

#[test]
fn claude_runs_its_own_app_under_a_known_session_and_keeps_its_transcript() {
    let case = Case::new();
    let mut request = case.request("claude-code", case.subscription(), false);
    request.model = Some("sonnet".into());
    let built = command(&request, None, &run(None)).unwrap();
    let mut argv = strings(&[
        SANDBOX_EXEC,
        "-f",
        case.profile().to_str().unwrap(),
        request.executable.to_str().unwrap(),
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--settings",
        "{\"attribution\":{\"commit\":\"\",\"pr\":\"\"},\"disableAllHooks\":true,\"includeCoAuthoredBy\":false,\"skipDangerousModePermissionPrompt\":true}",
        "--session-id",
        SESSION,
        "--system-prompt-snapshot",
        "off",
    ]);
    argv.extend(strings(&CLAUDE_NO_MCP));
    argv.extend(strings(&[
        "--model",
        "sonnet",
        "--tools",
        "",
        "--disable-slash-commands",
        "--system-prompt",
        crate::harnesses::launch::CLAUDE_NO_TOOLS_PROMPT,
    ]));
    assert_eq!(built.argv, argv);
    for headless in [
        "--print",
        "--input-format",
        "--output-format",
        "--no-session-persistence",
    ] {
        assert!(
            !built.argv.iter().any(|argument| argument == headless),
            "{headless}"
        );
    }
    let home = case.home().to_str().unwrap().to_owned();
    assert_eq!(
        built.env,
        with_git_env(vec![
            ("HOME".to_owned(), home.clone()),
            ("PATH".to_owned(), TOOLS_OFF_PATH.to_owned()),
            ("LANG".to_owned(), LOCALE.to_owned()),
            ("LC_ALL".to_owned(), LOCALE.to_owned()),
            ("CLAUDE_CONFIG_DIR".to_owned(), home.clone()),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR".to_owned(), home.clone()),
            ("CLAUDE_CODE_TMPDIR".to_owned(), home),
            ("DISABLE_TELEMETRY".to_owned(), "1".to_owned()),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
                "1".to_owned()
            ),
            (
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_owned(),
                "1".to_owned()
            ),
        ])
    );
    let folder = claude_project_folder(&case.path().join("work")).unwrap();
    assert_eq!(
        built.transcript,
        Transcript::ClaudeSession {
            file: case
                .home()
                .join("projects")
                .join(folder)
                .join(format!("{SESSION}.jsonl"))
        }
    );

    let on = case.request("claude-code", case.subscription(), true);
    let built = command(&on, None, &run(None)).unwrap();
    assert_eq!(
        built.argv[built.argv.len() - 8..],
        strings(&[
            "--tools",
            "default",
            "--permission-mode",
            "bypassPermissions",
            "--disallowedTools",
            "Agent",
            "Workflow",
            "TeamCreate"
        ])
    );
    assert!(
        built
            .env
            .contains(&("SHELL".to_owned(), "/bin/zsh".to_owned()))
    );
    assert!(
        built
            .env
            .contains(&("MCP_TIMEOUT".to_owned(), "60000".to_owned())),
        "a tools-on agent waits 60 s, not Claude Code's 30 s, for its MCP servers: {:?}",
        built.env
    );

    let bad = Run {
        session_id: "not-a-uuid",
        key_file: None,
        resume: false,
    };
    assert_eq!(
        command(&request, None, &bad).unwrap_err().0,
        "launch_session_id_invalid"
    );
}

#[test]
fn a_keyed_claude_gets_its_key_through_its_key_helper_and_a_local_one_a_placeholder() {
    let case = Case::new();
    let request = case.request("claude-code", case.keyed(None), true);
    let key_file = write_key(&case.home(), request.account.key().unwrap()).unwrap();
    let built = command(&request, None, &run(Some(&key_file))).unwrap();
    no_secret(&built);
    let settings: serde_json::Value = serde_json::from_str(&built.argv[8]).unwrap();
    assert_eq!(
        settings["apiKeyHelper"],
        format!("/bin/cat '{}'", key_file.display())
    );
    assert_eq!(built.argv.last().unwrap(), "--bare");
    assert!(
        !built
            .env
            .iter()
            .any(|(name, _)| name == "ANTHROPIC_API_KEY")
    );
    let helper = crate::spawn_gate::output(
        std::process::Command::new("/bin/sh")
            .args(["-c", settings["apiKeyHelper"].as_str().unwrap()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert_eq!(String::from_utf8(helper.stdout).unwrap(), KEY);

    let local = case.request(
        "claude-code",
        AccountBinding::Url {
            home: case.home(),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        false,
    );
    let built = command(&local, None, &run(None)).unwrap();
    assert!(built.env.contains(&(
        "ANTHROPIC_API_KEY".to_owned(),
        LOCAL_PLACEHOLDER_KEY.to_owned()
    )));
    assert!(built.env.contains(&(
        "ANTHROPIC_BASE_URL".to_owned(),
        "http://127.0.0.1:11434".to_owned()
    )));
    let quoted = case.path().join("it's");
    std::fs::create_dir(&quoted).unwrap();
    assert_eq!(
        command(&request, None, &run(Some(&quoted.join("key"))))
            .unwrap_err()
            .0,
        "launch_key_file_invalid"
    );
}

#[test]
fn every_launch_needs_its_sandbox_profile_and_a_safe_home() {
    let case = Case::new();
    let mut bare = case.request("codex", case.subscription(), true);
    bare.sandbox_profile = None;
    assert_eq!(
        command(&bare, None, &run(None)).unwrap_err().0,
        "launch_sandbox_required"
    );
    std::fs::set_permissions(case.home(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let open = case.request("claude-code", case.subscription(), true);
    assert_eq!(
        command(&open, None, &run(None)).unwrap_err().0,
        "launch_account_home_unsafe"
    );
}

#[test]
fn the_key_file_is_private_and_replaced_never_followed() {
    let case = Case::new();
    let first = Secret::new("first-key-1".into()).unwrap();
    let path = write_key(&case.home(), &first).unwrap();
    assert_eq!(path, case.home().join(KEY_FILE));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first-key-1");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let second = Secret::new("second-key-2".into()).unwrap();
    write_key(&case.home(), &second).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second-key-2");

    std::fs::remove_file(&path).unwrap();
    let target = case.path().join("elsewhere");
    std::fs::write(&target, "kept").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(
        write_key(&case.home(), &second).unwrap_err().0,
        "launch_key_file_unsafe"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
}

#[test]
fn session_ids_are_fresh_version_four_uuids() {
    let first = session_id().unwrap();
    let second = session_id().unwrap();
    assert_ne!(first, second);
    for id in [&first, &second] {
        assert!(session_id_checked(id).is_ok(), "{id}");
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
    }
    for bad in [
        "",
        "0F8C2C1E-5A7B-4C3D-9E2F-1A2B3C4D5E6F",
        "0f8c2c1e5a7b4c3d9e2f1a2b3c4d5e6f",
    ] {
        assert!(session_id_checked(bad).is_err(), "{bad}");
    }
}

#[test]
fn claude_project_folders_follow_claude_codes_naming() {
    assert_eq!(
        claude_project_folder(Path::new(
            "/Users/me/Projects/CrazyAgent/.crazyagent/worktrees/agent-3"
        ))
        .unwrap(),
        "-Users-me-Projects-CrazyAgent--crazyagent-worktrees-agent-3"
    );
    assert_eq!(
        claude_project_folder(Path::new("/private/tmp/a_b c")).unwrap(),
        "-private-tmp-a-b-c"
    );
    let long = format!("/{}", "a".repeat(CLAUDE_PROJECT_NAME_MAX - 1));
    assert_eq!(
        claude_project_folder(Path::new(&long)).unwrap().len(),
        CLAUDE_PROJECT_NAME_MAX
    );
    let longer = format!("/{}", "a".repeat(CLAUDE_PROJECT_NAME_MAX));
    assert_eq!(claude_project_folder(Path::new(&longer)), None);
}

#[test]
fn a_claude_home_skips_first_run_questions_and_keeps_its_sign_in() {
    let case = Case::new();
    let project = case.path().join("work");
    prepare_claude_home(&case.home(), &project).unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(case.home().join(".claude.json")).unwrap()).unwrap();
    assert_eq!(config["hasCompletedOnboarding"], true);
    assert_eq!(
        config["projects"][project.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    assert_eq!(
        config["customApiKeyResponses"]["approved"],
        serde_json::json!([LOCAL_PLACEHOLDER_KEY])
    );
    let mode = std::fs::metadata(case.home().join(".claude.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    std::fs::write(
        case.home().join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"owner@example.test\"},\"projects\":{\"/other\":{\"allowedTools\":[\"Read\"]}},\"customApiKeyResponses\":{\"approved\":[\"owner-approved-tail\"],\"rejected\":[\"crazyagent-local\",\"owner-rejected-tail\"]}}",
    )
    .unwrap();
    prepare_claude_home(&case.home(), &project).unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(case.home().join(".claude.json")).unwrap()).unwrap();
    assert_eq!(config["oauthAccount"]["emailAddress"], "owner@example.test");
    assert_eq!(config["projects"]["/other"]["allowedTools"][0], "Read");
    assert_eq!(
        config["customApiKeyResponses"],
        serde_json::json!({
            "approved": ["owner-approved-tail", LOCAL_PLACEHOLDER_KEY],
            "rejected": ["owner-rejected-tail"]
        })
    );
    assert_eq!(
        config["projects"][project.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    assert!(!case.home().join(".claude.json.crazyagent").exists());

    std::fs::write(case.home().join(".claude.json"), "[1]").unwrap();
    assert_eq!(
        prepare_claude_home(&case.home(), &project).unwrap_err().0,
        "launch_claude_config_invalid"
    );
}

/// Whether the file at `path` is a config this module wrote from the swapped-in secret.
/// Takes the path. Returns true only for a merged config holding the secret marker.
fn merged_secret(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        text.contains(SWAPPED_SECRET_MARKER) && text.contains("hasCompletedOnboarding")
    })
}

#[test]
fn a_claude_config_swapped_for_a_link_mid_launch_is_never_followed() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let case = Case::new();
    let project = case.path().join("work");
    let home = case.home();
    let secret = case.path().join("owner-secret.json");
    std::fs::write(
        &secret,
        format!("{{\"token\":\"{SWAPPED_SECRET_MARKER}\"}}"),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let leaks = Arc::new(AtomicUsize::new(0));
    let swapper = {
        let (home, stop, leaks) = (home.clone(), stop.clone(), leaks.clone());
        std::thread::spawn(move || {
            let config = home.join(".claude.json");
            let (link, file) = (home.join(".swap-link"), home.join(".swap-file"));
            let mut swaps = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                for staged in [&link, &file] {
                    if merged_secret(&config) {
                        leaks.fetch_add(1, Ordering::Relaxed);
                    }
                    if staged == &link {
                        std::os::unix::fs::symlink(&secret, &link).unwrap();
                    } else {
                        std::fs::write(&file, "{}").unwrap();
                    }
                    std::fs::rename(staged, &config).unwrap();
                }
                swaps += 1;
            }
            swaps
        })
    };
    let mut outcomes = std::collections::BTreeMap::<String, u64>::new();
    let deadline = std::time::Instant::now() + SWAP_TIME;
    let mut launches = 0_u64;
    while launches < SWAP_LAUNCHES_MAX
        && std::time::Instant::now() < deadline
        && leaks.load(Ordering::Relaxed) == 0
    {
        let outcome = match prepare_claude_home(&home, &project) {
            Ok(()) => "ok".to_owned(),
            Err(error) => error.0,
        };
        if merged_secret(&home.join(".claude.json")) {
            leaks.fetch_add(1, Ordering::Relaxed);
        }
        *outcomes.entry(outcome).or_default() += 1;
        launches += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0);
    assert!(outcomes.get("ok").is_some_and(|ok| *ok > 0), "{outcomes:?}");
    assert_eq!(
        leaks.load(Ordering::Relaxed),
        0,
        "the engine merged a linked file: {outcomes:?} after {launches} launches and {swaps} swaps"
    );
}

#[test]
fn a_claude_config_that_is_a_fifo_or_link_is_refused_without_blocking() {
    let case = Case::new();
    let project = case.path().join("work");
    let config = case.home().join(".claude.json");
    assert!(
        crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(&config))
            .unwrap()
            .success()
    );
    assert_eq!(
        prepare_claude_home(&case.home(), &project).unwrap_err().0,
        "launch_claude_config_unsafe"
    );
    std::fs::remove_file(&config).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &config).unwrap();
    assert_eq!(
        prepare_claude_home(&case.home(), &project).unwrap_err().0,
        "launch_claude_config_unsafe"
    );
    std::fs::remove_file(&config).unwrap();
    std::fs::write(&config, vec![b' '; CLAUDE_CONFIG_BYTES_MAX as usize + 1]).unwrap();
    assert_eq!(
        prepare_claude_home(&case.home(), &project).unwrap_err().0,
        "launch_claude_config_unsafe"
    );
}

#[test]
fn the_owner_git_identity_reaches_a_pane_agent_environment() {
    let case = Case::new();
    let mut request = case.request("claude-code", case.subscription(), true);
    request.git_identity = Some(
        crate::harnesses::launch::GitIdentity::new(
            "Repo Owner".into(),
            "owner@example.invalid".into(),
        )
        .unwrap(),
    );
    let built = command(&request, None, &run(None)).unwrap();
    for (name, value) in [
        ("GIT_AUTHOR_NAME", "Repo Owner"),
        ("GIT_AUTHOR_EMAIL", "owner@example.invalid"),
        ("GIT_COMMITTER_NAME", "Repo Owner"),
        ("GIT_COMMITTER_EMAIL", "owner@example.invalid"),
    ] {
        assert!(
            built.env.contains(&(name.to_owned(), value.to_owned())),
            "{name}: {:?}",
            built.env
        );
    }
}

fn tools_on_path() -> String {
    toolchains::agent_path(&ToolHomes::owner().unwrap(), TOOLS_ON_PATH)
}

#[test]
fn a_pane_gets_the_agent_short_temp_folder_and_the_toolchain_settings() {
    let case = Case::new();
    let tmp = toolchains::make_tmp(&case.home()).unwrap();
    for harness in ["codex", "claude-code"] {
        let request = case.request(harness, case.subscription(), true);
        let built = command(&request, None, &run(None)).unwrap();
        let tmpdirs: Vec<&str> = built
            .env
            .iter()
            .filter(|(name, _)| name == "TMPDIR")
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(tmpdirs, [tmp.to_str().unwrap()], "{harness}");
        for (name, value) in [
            ("RUSTUP_AUTO_INSTALL", "0".to_owned()),
            (
                "CARGO_HOME",
                case.home().join(".cargo").to_str().unwrap().to_owned(),
            ),
        ] {
            assert!(
                built.env.contains(&(name.to_owned(), value)),
                "{harness} {name}"
            );
        }
    }
    toolchains::remove_tmp(&case.home()).unwrap();
}

/// Builds the real project context `for_project` gives the case's project: an engine
/// database in its own data folder of the case with the project and its setup, whose folder
/// is the case's working folder. Inputs: the case and the data folder name.
/// Output: the context.
fn project_context(case: &Case, data: &str) -> crate::launch_context::AgentContext {
    use crate::project_setup::{StoredSetup, TeamPlan};
    let data = case.path().join(data);
    std::fs::create_dir(&data).unwrap();
    let connection = rusqlite::Connection::open(data.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let project = crate::protocol::ProjectId::new("interactive-launch").unwrap();
    connection
        .execute(
            "INSERT INTO projects(id, body) VALUES (?1, '{}')",
            [project.as_str()],
        )
        .unwrap();
    let setup = StoredSetup {
        project_id: project.clone(),
        name: "Interactive".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "claude-code".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: case.path().join("work").display().to_string(),
        created_ms: 1,
        revision: 1,
    };
    connection
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            [
                project.as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            ],
        )
        .unwrap();
    crate::launch_context::for_project(&connection, &project, crate::agent_git::Layout::Linked)
        .unwrap()
}

/// Reads the Claude settings a built command passes with `--settings`.
/// Input: the command. Output: the settings.
fn claude_settings(built: &InteractiveCommand) -> serde_json::Value {
    let at = built
        .argv
        .iter()
        .position(|argument| argument == "--settings")
        .unwrap();
    serde_json::from_str(&built.argv[at + 1]).unwrap()
}

/// The display variables a built Claude Code command carries, in order, after the launch's
/// environment filter. Input: the command. Output: the name and value pairs of the three
/// variables the N-D9 display sets.
fn display_variables(built: &InteractiveCommand) -> Vec<(String, String)> {
    built
        .env
        .iter()
        .filter(|(name, _)| {
            name == "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"
                || name == "CLAUDE_CODE_SCROLL_SPEED"
                || name == "CLAUDE_CODE_NO_FLICKER"
        })
        .cloned()
        .collect()
}

/// A Claude Code terminal at the Classic display, the default and the display of a launch
/// without a project context, turns Claude Code's alternate screen off (N-D9), so Claude
/// Code draws on the normal screen and leaves the wheel and a plain drag to the terminal,
/// and never asks for Claude Code's fullscreen renderer. A native lead's teammates get the
/// same variables through their script.
#[test]
fn a_classic_claude_code_terminal_turns_the_alternate_screen_off() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), true);
    let classic = [(
        "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_owned(),
        "1".to_owned(),
    )];
    let bare = command(&request, None, &run(None)).unwrap();
    assert_eq!(display_variables(&bare), classic, "{:?}", bare.env);

    let plain = project_context(&case, "plain")
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&plain), &run(None)).unwrap();
    assert_eq!(display_variables(&built), classic, "{:?}", built.env);
    drop(plain);

    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let lead = project_context(&case, "lead")
        .with_native_team(&tmux, &case.path().join("work"))
        .with_terminal_display(crate::coord::settings::TerminalDisplay::Classic)
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&lead), &run(None)).unwrap();
    assert_eq!(display_variables(&built), classic, "{:?}", built.env);
    write_teammate_command(&case.home(), &built, &request, lead.native().unwrap()).unwrap();
    let written =
        std::fs::read_to_string(crate::launch_context::teammate_command(&case.home())).unwrap();
    assert!(
        written.contains(" 'CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1' "),
        "{written}"
    );
    assert!(!written.contains("CLAUDE_CODE_NO_FLICKER"), "{written}");
    drop(lead);
}

/// A Claude Code terminal at the Fullscreen display asks for Claude Code's fullscreen
/// renderer with `CLAUDE_CODE_NO_FLICKER=1` (R8-FS), so the account's own settings and
/// rollout cannot leave it on the classic one, at 3 rows a wheel notch (N-D9), the step
/// Claude Code picks itself for xterm.js, and does not turn the alternate screen off. A
/// native lead's teammates get the same variables.
#[test]
fn a_fullscreen_claude_code_terminal_asks_for_the_fullscreen_renderer_at_three_rows_a_notch() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), true);
    let fullscreen = [
        ("CLAUDE_CODE_SCROLL_SPEED".to_owned(), "3".to_owned()),
        ("CLAUDE_CODE_NO_FLICKER".to_owned(), "1".to_owned()),
    ];
    let chosen = project_context(&case, "chosen")
        .with_terminal_display(crate::coord::settings::TerminalDisplay::Fullscreen)
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&chosen), &run(None)).unwrap();
    assert_eq!(display_variables(&built), fullscreen, "{:?}", built.env);
    drop(chosen);

    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let lead = project_context(&case, "lead")
        .with_native_team(&tmux, &case.path().join("work"))
        .with_terminal_display(crate::coord::settings::TerminalDisplay::Fullscreen)
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&lead), &run(None)).unwrap();
    assert_eq!(display_variables(&built), fullscreen, "{:?}", built.env);
    write_teammate_command(&case.home(), &built, &request, lead.native().unwrap()).unwrap();
    let written =
        std::fs::read_to_string(crate::launch_context::teammate_command(&case.home())).unwrap();
    assert!(
        written.contains(" 'CLAUDE_CODE_SCROLL_SPEED=3' "),
        "{written}"
    );
    assert!(
        written.contains(" 'CLAUDE_CODE_NO_FLICKER=1' "),
        "{written}"
    );
    assert!(
        !written.contains("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"),
        "{written}"
    );
    drop(lead);
}

#[test]
fn a_native_lead_runs_agent_teams_and_its_teammates_get_a_plain_quoted_script() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), true);
    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let installed = project_context(&case, "data")
        .with_native_team(&tmux, &case.path().join("work"))
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&installed), &run(None)).unwrap();

    assert!(
        built
            .argv
            .windows(2)
            .any(|pair| pair == ["--teammate-mode", "tmux"])
    );
    let denied: Vec<&String> = built
        .argv
        .iter()
        .skip_while(|argument| *argument != CLAUDE_DENY_FLAG)
        .skip(1)
        .take_while(|argument| !argument.starts_with('-'))
        .collect();
    assert!(denied.iter().any(|tool| *tool == "Workflow"), "{denied:?}");
    assert!(
        !denied
            .iter()
            .any(|tool| CLAUDE_NATIVE_TEAM_TOOLS.contains(&tool.as_str())),
        "a native lead keeps Agent and TeamCreate: {denied:?}"
    );
    let script = crate::launch_context::teammate_command(&case.home());
    let variable = |name: &str| {
        built
            .env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        variable("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS").as_deref(),
        Some("1")
    );
    assert_eq!(
        variable("CLAUDE_CODE_TEAMMATE_COMMAND"),
        Some(script.to_str().unwrap().to_owned())
    );
    let folder = case.path().join("bin");
    assert_eq!(
        variable("PATH")
            .unwrap()
            .split(':')
            .filter(|entry| *entry == folder.to_str().unwrap())
            .count(),
        1
    );

    write_teammate_command(&case.home(), &built, &request, installed.native().unwrap()).unwrap();
    let written = std::fs::read_to_string(&script).unwrap();
    assert_eq!(
        std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(written.starts_with("#!/bin/sh\nexec '/usr/bin/env' "));
    assert!(written.ends_with(" '--disallowedTools' 'Agent' 'TeamCreate' \"$@\"\n"));
    let instructions = installed.native().unwrap().instructions.clone();
    assert!(written.contains(&format!(
        " '{}' '--append-system-prompt-file' '{}'",
        request.executable.display(),
        instructions.display()
    )));
    for inherited in [
        "--mcp-config",
        "--plugin-dir",
        "--settings",
        "--strict-mcp-config",
    ] {
        assert!(
            !written.contains(inherited),
            "Claude Code passes {inherited} to each teammate itself"
        );
    }
    drop(installed);
}

/// A native lead and every teammate its script starts get Claude Code's task list tools
/// (OP-3): Claude Code 2.1.309 offers TaskCreate, TaskGet, TaskUpdate and TaskList to a
/// current model only when `CLAUDE_CODE_ENABLE_TODO_TOOLS` is set, and without them a lead
/// cannot share its team's tasks. An agent outside a native team does not get the variable.
#[test]
fn a_native_lead_and_its_teammates_get_claude_code_s_task_list_tools() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), true);
    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let todo = |built: &InteractiveCommand| -> Vec<String> {
        built
            .env
            .iter()
            .filter(|(name, _)| name == "CLAUDE_CODE_ENABLE_TODO_TOOLS")
            .map(|(_, value)| value.clone())
            .collect()
    };
    let alone = project_context(&case, "alone")
        .install("claude-code", &case.home(), true)
        .unwrap();
    let agent = command(&request, Some(&alone), &run(None)).unwrap();
    assert!(todo(&agent).is_empty(), "{:?}", agent.env);
    drop(alone);

    let installed = project_context(&case, "data")
        .with_native_team(&tmux, &case.path().join("work"))
        .install("claude-code", &case.home(), true)
        .unwrap();
    let lead = command(&request, Some(&installed), &run(None)).unwrap();
    assert_eq!(todo(&lead), ["1"], "{:?}", lead.env);
    write_teammate_command(&case.home(), &lead, &request, installed.native().unwrap()).unwrap();
    let written =
        std::fs::read_to_string(crate::launch_context::teammate_command(&case.home())).unwrap();
    assert!(
        written.contains(" 'CLAUDE_CODE_ENABLE_TODO_TOOLS=1' "),
        "{written}"
    );
    drop(installed);
}

/// A native lead's teammates run in their own config home beside the lead's (TPH-2 C): the
/// script the lead's Claude Code runs for each teammate names that home in place of the
/// lead's for Claude Code's config and temp folders, `HOME` and the cargo home, and keeps the
/// lead's sign-in folder, where the engine keeps the account's copy the teammates read.
#[test]
fn a_native_lead_s_teammates_get_their_own_config_home_and_the_lead_s_sign_in() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), true);
    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let installed = project_context(&case, "data")
        .with_native_team(&tmux, &case.path().join("work"))
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&installed), &run(None)).unwrap();
    write_teammate_command(&case.home(), &built, &request, installed.native().unwrap()).unwrap();
    let written =
        std::fs::read_to_string(crate::launch_context::teammate_command(&case.home())).unwrap();
    let home = case.home();
    let mates = home.with_file_name("mates-home");
    let word = |name: &str, value: &Path| format!(" '{name}={}' ", value.display());

    for name in ["CLAUDE_CONFIG_DIR", "CLAUDE_CODE_TMPDIR", "HOME"] {
        assert!(written.contains(&word(name, &mates)), "{name}: {written}");
        assert!(!written.contains(&word(name, &home)), "{name}: {written}");
    }
    assert!(
        written.contains(&word("CARGO_HOME", &mates.join(".cargo"))),
        "{written}"
    );
    assert!(
        written.contains(&word("CLAUDE_SECURESTORAGE_CONFIG_DIR", &home)),
        "{written}"
    );
    drop(installed);
}

/// A native lead's temp files are its own (TPH-2 residual): the lead's `TMPDIR` and zsh
/// prefix name its own folder in the short temp folder, and the script its Claude Code runs
/// for each teammate names the shared short temp folder in their place, never the lead's
/// own. A script that would still name the lead's own folder is refused.
#[test]
fn a_native_lead_s_temp_folder_is_its_own_and_its_teammates_keep_the_shared_one() {
    let case = Case::new();
    let shared = toolchains::make_tmp(&case.home()).unwrap();
    let own = toolchains::native_lead_tmp(&shared);
    let request = case.request("claude-code", case.subscription(), true);
    let tmux = case.executable("tmux", "#!/bin/sh\nexit 0\n");
    let installed = project_context(&case, "data")
        .with_native_team(&tmux, &case.path().join("work"))
        .install("claude-code", &case.home(), true)
        .unwrap();
    let built = command(&request, Some(&installed), &run(None)).unwrap();
    let value = |name: &str| {
        built
            .env
            .iter()
            .filter(|(known, _)| known == name)
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(value("TMPDIR"), [own.to_str().unwrap()]);
    assert_eq!(value("TMPPREFIX"), [own.join("zsh").to_str().unwrap()]);
    assert_ne!(own, shared);

    write_teammate_command(&case.home(), &built, &request, installed.native().unwrap()).unwrap();
    let script = crate::launch_context::teammate_command(&case.home());
    let written = std::fs::read_to_string(&script).unwrap();
    let word = |name: &str, value: &Path| format!(" '{name}={}' ", value.display());
    assert!(written.contains(&word("TMPDIR", &shared)), "{written}");
    assert!(
        written.contains(&word("TMPPREFIX", &shared.join("zsh"))),
        "{written}"
    );
    assert!(!written.contains(own.to_str().unwrap()), "{written}");

    std::fs::remove_file(&script).unwrap();
    let mut leaky = built.clone();
    leaky.env.push((
        "XDG_RUNTIME_DIR".to_owned(),
        own.join("run").display().to_string(),
    ));
    assert_eq!(
        write_teammate_command(&case.home(), &leaky, &request, installed.native().unwrap())
            .unwrap_err()
            .0,
        "launch_teammate_command_invalid"
    );
    assert!(!script.exists());
    drop(installed);
    toolchains::remove_tmp(&case.home()).unwrap();
}

#[test]
fn a_relayed_claude_launch_allows_hooks_and_a_plain_one_disables_them() {
    let case = Case::new();
    let request = case.request("claude-code", case.subscription(), false);
    let plain = project_context(&case, "data-plain")
        .install("claude-code", &case.home(), false)
        .unwrap();
    let built = command(&request, Some(&plain), &run(None)).unwrap();
    assert_eq!(
        claude_settings(&built)["disableAllHooks"],
        serde_json::Value::Bool(true)
    );
    drop(plain);

    let relayed = project_context(&case, "data-relayed")
        .with_relay(
            &case.path().join("bin").join("crazyagent"),
            &case.path().join("mailbox"),
        )
        .install("claude-code", &case.home(), false)
        .unwrap();
    assert!(relayed.runs_hooks());
    let built = command(&request, Some(&relayed), &run(None)).unwrap();
    let settings = claude_settings(&built);
    assert!(settings.get("disableAllHooks").is_none(), "{settings}");
    assert_eq!(
        settings["includeCoAuthoredBy"],
        serde_json::Value::Bool(false)
    );
    drop(relayed);
}

#[test]
fn the_tmux_folder_is_added_to_path_once_and_a_folder_with_a_colon_is_refused() {
    let tmux = Path::new("/opt/homebrew/bin/tmux");
    let mut env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    tmux_on_path(&mut env, tmux).unwrap();
    assert_eq!(env[0].1, "/usr/bin:/bin:/opt/homebrew/bin");
    tmux_on_path(&mut env, tmux).unwrap();
    assert_eq!(env[0].1, "/usr/bin:/bin:/opt/homebrew/bin");

    let mut named = vec![("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned())];
    tmux_on_path(&mut named, tmux).unwrap();
    assert_eq!(named[0].1, "/opt/homebrew/bin:/usr/bin");

    let mut env = vec![("PATH".to_owned(), "/usr/bin".to_owned())];
    assert_eq!(
        tmux_on_path(&mut env, Path::new("/opt/odd:folder/tmux"))
            .unwrap_err()
            .0,
        "launch_path_invalid"
    );
    assert_eq!(env[0].1, "/usr/bin");
    let mut none = vec![("HOME".to_owned(), "/private/tmp/home".to_owned())];
    assert_eq!(
        tmux_on_path(&mut none, tmux).unwrap_err().0,
        "launch_path_invalid"
    );
}

#[test]
fn a_pane_path_starts_with_the_agent_git_only_while_the_temp_folder_holds_it() {
    let case = Case::new();
    let path_of = |tools: bool| -> String {
        let request = case.request("claude-code", case.subscription(), tools);
        let built = command(&request, None, &run(None)).unwrap();
        let paths: Vec<&String> = built
            .env
            .iter()
            .filter(|(name, _)| name == "PATH")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(paths.len(), 1);
        paths[0].clone()
    };
    assert_eq!(path_of(true), tools_on_path());
    assert_eq!(path_of(false), TOOLS_OFF_PATH);
    let tmp = toolchains::make_tmp(&case.home()).unwrap();
    let bin = tmp.join("bin");
    assert_eq!(
        path_of(true),
        format!("{}:{}", bin.display(), tools_on_path())
    );
    assert_eq!(
        path_of(false),
        format!("{}:{TOOLS_OFF_PATH}", bin.display())
    );
    toolchains::remove_tmp(&case.home()).unwrap();
}

#[test]
fn a_codex_folder_decides_the_trust_setting_on_both_launch_shapes() {
    let case = Case::new();
    let work = case.path().join("work");
    std::fs::create_dir(work.join(".git")).unwrap();
    let names = |level: &str| {
        format!(
            "projects={{\"{}\"={{trust_level=\"{level}\"}}}}",
            work.display()
        )
    };
    for tools in [false, true] {
        let request = case.request("codex", case.subscription(), tools);
        let built = command(&request, None, &run(None)).unwrap();
        let wanted = names("trusted");
        assert!(
            built
                .argv
                .windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == wanted),
            "{tools}: {:?}",
            built.argv
        );
    }
    std::fs::create_dir(work.join(".codex")).unwrap();
    for tools in [false, true] {
        let request = case.request("codex", case.subscription(), tools);
        let built = command(&request, None, &run(None)).unwrap();
        let wanted = names("untrusted");
        assert!(
            built
                .argv
                .windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == wanted),
            "{tools}: {:?}",
            built.argv
        );
    }
}

/// The values of the `tui.alternate_screen` settings a built Codex command carries, in order.
/// Input: the command. Output: the values as written, quotes included.
fn codex_screen(built: &InteractiveCommand) -> Vec<String> {
    built
        .argv
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .filter_map(|pair| pair[1].strip_prefix("tui.alternate_screen="))
        .map(str::to_owned)
        .collect()
}

/// Builds a Codex command in a project whose owner chose `display`, for each launch shape.
/// Inputs: the case, a name for the context's folder, the display, whether tools are on and
/// whether the run resumes a conversation. Output: the command.
fn codex_at(
    case: &Case,
    name: &str,
    display: crate::coord::settings::TerminalDisplay,
    tools: bool,
    resume: bool,
) -> InteractiveCommand {
    let request = case.request("codex", case.subscription(), tools);
    let installed = project_context(case, name)
        .with_terminal_display(display)
        .install("codex", &case.home(), tools)
        .unwrap();
    let run = Run {
        session_id: SESSION,
        key_file: None,
        resume,
    };
    command(&request, Some(&installed), &run).unwrap()
}

/// DEC-141 parity: the one Terminal display setting has a mapping for every harness kind.
/// Classic, the display where a plain drag selects text, holds for every kind. Two displays a
/// kind can draw start it differently, and a display it cannot draw starts it as Classic does
/// and is left out of the displays the settings list for it. A kind with a terminal app
/// carries exactly its display's switches in its built command, on both launch shapes, and
/// none of the other display's. A kind added without a mapping fails here.
#[test]
fn every_harness_kind_maps_every_terminal_display() {
    use crate::coord::settings::{HarnessKind, TerminalDisplay};
    assert_eq!(
        TerminalDisplay::ALL,
        [TerminalDisplay::Classic, TerminalDisplay::Fullscreen]
    );
    assert_eq!(TerminalDisplay::default(), TerminalDisplay::Classic);
    let starts = |switches: DisplaySwitches| (switches.variables, switches.arguments);
    let case = Case::new();
    let mut launched = 0;
    for kind in HarnessKind::ALL {
        let classic = display_switches(kind, TerminalDisplay::Classic);
        assert!(classic.honoured, "{kind:?} has no Classic display");
        for display in TerminalDisplay::ALL {
            let switches = display_switches(kind, display);
            assert_eq!(
                displays(kind).contains(&display),
                switches.honoured,
                "{kind:?} {display:?}"
            );
            if display == TerminalDisplay::Classic {
                continue;
            }
            assert_eq!(
                switches.honoured,
                starts(switches) != starts(classic),
                "{kind:?} {display:?} is honoured only when it starts the kind differently"
            );
        }
        if support(kind.as_str()) != Support::Terminal {
            for display in TerminalDisplay::ALL {
                let switches = display_switches(kind, display);
                assert!(
                    switches.variables.is_empty() && switches.arguments.is_empty(),
                    "{kind:?} has no terminal app to switch"
                );
            }
            continue;
        }
        for display in TerminalDisplay::ALL {
            let switches = display_switches(kind, display);
            assert!(switches.honoured, "{kind:?} {display:?}");
            let others: Vec<DisplaySwitches> = TerminalDisplay::ALL
                .into_iter()
                .filter(|other| *other != display)
                .map(|other| display_switches(kind, other))
                .collect();
            for tools in [false, true] {
                launched += 1;
                let request = case.request(kind.as_str(), case.subscription(), tools);
                let installed = project_context(&case, &format!("parity-{launched}"))
                    .with_terminal_display(display)
                    .install(kind.as_str(), &case.home(), tools)
                    .unwrap();
                let built = command(&request, Some(&installed), &run(None)).unwrap();
                let has_variable = |(name, value): &(&str, &str)| {
                    built.env.iter().any(|(n, v)| n == name && v == value)
                };
                let has_arguments = |arguments: &[&str]| {
                    !arguments.is_empty()
                        && built
                            .argv
                            .windows(arguments.len())
                            .any(|window| window == arguments)
                };
                assert!(
                    switches.variables.iter().all(has_variable),
                    "{kind:?} {display:?} tools {tools}: {:?}",
                    built.env
                );
                assert!(
                    switches.arguments.is_empty() || has_arguments(switches.arguments),
                    "{kind:?} {display:?} tools {tools}: {:?}",
                    built.argv
                );
                for other in &others {
                    assert!(
                        !other.variables.iter().any(has_variable)
                            && !has_arguments(other.arguments),
                        "{kind:?} {display:?} tools {tools} carries another display's switches"
                    );
                }
            }
        }
    }
    assert_eq!(
        launched, 8,
        "Claude Code and Codex, two displays, two launch shapes"
    );
}

/// The displays the settings list for each kind (DEC-141): Claude Code and Codex draw both,
/// RCLI, a one-turn master with no view of its own, only Classic.
#[test]
fn the_displays_of_each_harness_kind_are_named_for_the_settings() {
    use crate::coord::settings::{HarnessKind, TerminalDisplay};
    let both = TerminalDisplay::ALL.to_vec();
    assert_eq!(displays(HarnessKind::ClaudeCode), both);
    assert_eq!(displays(HarnessKind::Codex), both);
    assert_eq!(displays(HarnessKind::Rcli), [TerminalDisplay::Classic]);
}

/// OB-8: a Codex terminal at the Classic display, the default and the display of a launch
/// without a project context, starts with `tui.alternate_screen="never"`, so Codex draws on
/// the normal screen with no mouse reporting and a plain drag in the app's terminal view
/// selects text. The setting is a `-c` setting among the others, ahead of `resume`, because
/// the bridge script hands the same arguments to `codex app-server`, which takes `-c` and
/// exits on `--no-alt-screen`.
#[test]
fn a_classic_codex_terminal_draws_on_the_normal_screen_on_both_launch_shapes() {
    use crate::coord::settings::TerminalDisplay;
    let case = Case::new();
    let never = ["\"never\"".to_owned()];
    for tools in [false, true] {
        let request = case.request("codex", case.subscription(), tools);
        let bare = command(&request, None, &run(None)).unwrap();
        assert_eq!(codex_screen(&bare), never, "{tools}: {:?}", bare.argv);
        for resume in [false, true] {
            let name = format!("classic-{tools}-{resume}");
            let built = codex_at(&case, &name, TerminalDisplay::Classic, tools, resume);
            assert_eq!(codex_screen(&built), never, "{name}: {:?}", built.argv);
            assert!(
                !built
                    .argv
                    .iter()
                    .any(|argument| argument == "--no-alt-screen"),
                "{name}: {:?}",
                built.argv
            );
            let at = |word: &str| built.argv.iter().position(|argument| argument == word);
            let setting = at("tui.alternate_screen=\"never\"").unwrap();
            assert_eq!(built.argv[setting - 1], "-c", "{name}");
            if tools {
                let script = at(CODEX_BRIDGE_SCRIPT).unwrap();
                assert!(
                    setting > script + 3,
                    "{name}: after the program and the resume word"
                );
                assert_eq!(built.argv[script + 3] == SESSION, resume, "{name}");
            } else if resume {
                assert!(setting < at("resume").unwrap(), "{name}: {:?}", built.argv);
            }
        }
    }
}

/// A Codex terminal at the Fullscreen display starts with `tui.alternate_screen="always"`,
/// Codex's own view on the alternate screen, which takes the mouse, whatever the account's
/// own Codex settings say, on both launch shapes, and never also asks for the normal screen.
#[test]
fn a_fullscreen_codex_terminal_keeps_codex_own_view_on_both_launch_shapes() {
    use crate::coord::settings::TerminalDisplay;
    let case = Case::new();
    for tools in [false, true] {
        for resume in [false, true] {
            let name = format!("fullscreen-{tools}-{resume}");
            let built = codex_at(&case, &name, TerminalDisplay::Fullscreen, tools, resume);
            assert_eq!(
                codex_screen(&built),
                ["\"always\"".to_owned()],
                "{name}: {:?}",
                built.argv
            );
            assert!(
                !built
                    .argv
                    .iter()
                    .any(|argument| argument == "--no-alt-screen"),
                "{name}: {:?}",
                built.argv
            );
        }
    }
}

/// Every display switch Codex gets is a `-c` setting and no variable: the bridge script hands
/// a tools-on launch's arguments to `codex app-server` too, which exits on any TUI-only flag
/// and would leave the agent without its native bridge, and the Codex launch adds no display
/// variables. Claude Code's are variables only, as its launch adds no display arguments.
#[test]
fn each_kind_s_display_switches_are_of_the_shape_its_launch_carries() {
    use crate::coord::settings::{HarnessKind, TerminalDisplay};
    for display in TerminalDisplay::ALL {
        let codex = display_switches(HarnessKind::Codex, display);
        assert!(codex.variables.is_empty(), "{display:?}");
        assert_eq!(codex.arguments.len(), 2, "{display:?}");
        assert_eq!(codex.arguments[0], "-c", "{display:?}");
        let claude = display_switches(HarnessKind::ClaudeCode, display);
        assert!(claude.arguments.is_empty(), "{display:?}");
        assert!(!claude.variables.is_empty(), "{display:?}");
    }
}
