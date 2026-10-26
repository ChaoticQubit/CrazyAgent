use super::*;
use crate::harnesses::launch::LaunchLimits;
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
    }
}

fn with_git_env(mut env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.extend(
        crate::sandbox::AGENT_GIT_ENV.map(|(name, value)| (name.to_owned(), value.to_owned())),
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
                ("PATH".to_owned(), TOOLS_ON_PATH.to_owned()),
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

#[test]
fn codex_runs_its_own_app_with_the_headless_settings_and_no_app_server() {
    let case = Case::new();
    let mut request = case.request("codex", case.subscription(), false);
    request.model = Some("gpt-5.5".into());
    let built = command(&request, None, &run(None)).unwrap();
    let mut argv = strings(&[
        SANDBOX_EXEC,
        "-f",
        case.profile().to_str().unwrap(),
        request.executable.to_str().unwrap(),
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
    assert_eq!(built.argv, argv);
    assert!(!built.argv.iter().any(|argument| argument == "app-server"));
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

    let on = case.request("codex", case.subscription(), true);
    let built = command(&on, None, &run(None)).unwrap();
    for setting in [
        "sandbox_mode=\"danger-full-access\"",
        "approval_policy=\"never\"",
        "features.goals=false",
        "mcp_servers={}",
    ] {
        assert!(
            built.argv.iter().any(|argument| argument == setting),
            "{setting}"
        );
    }
    assert!(
        built
            .env
            .contains(&("PATH".to_owned(), TOOLS_ON_PATH.to_owned()))
    );
}

#[test]
fn a_keyed_codex_reads_its_key_from_the_key_file_inside_the_sandbox() {
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
            KEY_SCRIPT,
            KEY_SCRIPT_NAME,
            key_file.to_str().unwrap(),
            request.executable.to_str().unwrap(),
        ])
    );
    assert!(built.argv.iter().any(|argument| argument
        == "model_providers.crazyagent={name=\"CrazyAgent\",base_url=\"https://models.example.test/v1\",wire_api=\"responses\",env_key=\"CRAZYAGENT_API_KEY\"}"));

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
    assert!(
        seen.lines()
            .any(|line| line == format!("CRAZYAGENT_API_KEY={KEY}"))
    );
    assert!(
        seen.lines()
            .any(|line| line == format!("CODEX_HOME={}", case.home().display()))
    );

    std::fs::remove_file(&key_file).unwrap();
    let missing = crate::spawn_gate::output(
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
    assert!(!missing.status.success(), "no key file, no harness");
    assert!(!String::from_utf8_lossy(&missing.stdout).contains("CODEX_HOME="));
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
        "{\"disableAllHooks\":true,\"skipDangerousModePermissionPrompt\":true}",
        "--session-id",
        SESSION,
    ]);
    argv.extend(strings(&CLAUDE_NO_MCP));
    argv.extend(strings(&[
        "--model",
        "sonnet",
        "--tools",
        "",
        "--disable-slash-commands",
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
        built.argv[built.argv.len() - 4..],
        strings(&[
            "--tools",
            "default",
            "--permission-mode",
            "bypassPermissions"
        ])
    );
    assert!(
        built
            .env
            .contains(&("SHELL".to_owned(), "/bin/zsh".to_owned()))
    );

    let bad = Run {
        session_id: "not-a-uuid",
        key_file: None,
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
