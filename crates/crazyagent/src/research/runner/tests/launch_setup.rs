use super::*;

#[cfg(target_os = "macos")]
#[test]
fn adding_an_agent_worktree_runs_no_command_the_repository_configures() {
    let mut problems = Vec::new();
    for hostile in [
        "smudge",
        "process",
        "required",
        "dotted include",
        "worktree include",
        "info attributes",
        "agent global config",
        "post-checkout hook",
        "unsafe driver name",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("agent").join("home");
        let tmp = base.join("agent").join("tmp");
        let profiles = base.join("profiles");
        for folder in [&project, &home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        ensure_repository(&project, &tmp).unwrap();
        let marker = tmp.join("ran");
        let command = format!("/usr/bin/touch '{}'; /bin/cat", marker.display());
        fs::write(project.join("a.txt"), "plain\n").unwrap();
        let attributes = match hostile {
            "dotted include" => Some("* filter=a.b\n"),
            "worktree include" => Some("* filter=late\n"),
            "unsafe driver name" => Some("* filter=x=y\n"),
            "info attributes" | "agent global config" | "post-checkout hook" => None,
            _ => Some("* filter=hostile\n"),
        };
        if let Some(attributes) = attributes {
            fs::write(project.join(".gitattributes"), attributes).unwrap();
        }
        repository_git(&project, &["add", "-A"]);
        repository_git(&project, &["commit", "-q", "-m", "files"]);
        let git_dir = project.join(".git");
        match hostile {
            "smudge" => repository_git(&project, &["config", "filter.hostile.smudge", &command]),
            "process" => repository_git(&project, &["config", "filter.hostile.process", &command]),
            "required" => {
                repository_git(&project, &["config", "filter.hostile.smudge", &command]);
                repository_git(&project, &["config", "filter.hostile.required", "true"]);
            }
            "dotted include" => {
                fs::write(
                    git_dir.join("hostile.config"),
                    format!("[filter \"a.b\"]\n\tsmudge = {command}\n"),
                )
                .unwrap();
                repository_git(&project, &["config", "include.path", "hostile.config"]);
            }
            "worktree include" => {
                fs::write(
                    git_dir.join("late.config"),
                    format!("[filter \"late\"]\n\tsmudge = {command}\n"),
                )
                .unwrap();
                repository_git(
                    &project,
                    &[
                        "config",
                        "includeIf.gitdir:**/worktrees/**.path",
                        &git_dir.join("late.config").display().to_string(),
                    ],
                );
            }
            "info attributes" => {
                fs::write(
                    git_dir.join("info").join("attributes"),
                    "* filter=hostile\n",
                )
                .unwrap();
                repository_git(&project, &["config", "filter.hostile.smudge", &command]);
            }
            "agent global config" => {
                fs::write(home.join("attributes"), "* filter=h\n").unwrap();
                fs::write(
                    home.join(".gitconfig"),
                    format!(
                        "[core]\n\tattributesFile = {}\n[filter \"h\"]\n\tsmudge = {command}\n",
                        home.join("attributes").display()
                    ),
                )
                .unwrap();
            }
            "post-checkout hook" => {
                let hooks = git_dir.join("hooks");
                fs::create_dir_all(&hooks).unwrap();
                let hook = hooks.join("post-checkout");
                fs::write(
                    &hook,
                    format!("#!/bin/sh\n/usr/bin/touch '{}'\n", marker.display()),
                )
                .unwrap();
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "unsafe driver name" => {
                repository_git(&project, &["config", "filter.x=y.smudge", &command])
            }
            other => panic!("no setup for {other}"),
        }
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap();
        let profile = profiles.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let worktree = project.join("worktrees").join("agent-one");
        let added = add_worktree(&sandbox, &profile, &worktree);
        if marker.exists() {
            problems.push(format!("{hostile}: the repository's command ran"));
        }
        let checked_out = fs::read_to_string(worktree.join("a.txt")).ok();
        match (hostile, &added) {
            ("unsafe driver name", Ok(())) => {
                problems.push(format!("{hostile}: the worktree was added"))
            }
            ("unsafe driver name", Err(_)) => {}
            (_, Err(error)) => problems.push(format!("{hostile}: {error}")),
            (_, Ok(())) if checked_out.as_deref() != Some("plain\n") => {
                problems.push(format!("{hostile}: checked out {checked_out:?}"))
            }
            _ => {}
        }
    }
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn a_tools_off_claude_run_takes_the_no_tools_prompt_one_turn_and_its_output_cap() {
    let account = ResearchAccount::AnthropicEndpoint {
        base_url: "http://127.0.0.1:11434".into(),
        model: "local-model".into(),
        key: None,
    };
    let launch = |tools: bool, output_tokens: Option<u32>| {
        let mut command = Command::new("/usr/bin/true");
        claude_arguments(
            &mut command,
            Path::new("/agent/home"),
            Path::new("/agent/tmp"),
            tools,
            output_tokens,
            &account,
            crate::provider_retry::ProviderRetry::HarnessDefault,
        )
        .unwrap();
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_str().unwrap().to_owned())
            .collect();
        let cap = command
            .get_envs()
            .find(|(key, _)| *key == "CLAUDE_CODE_MAX_OUTPUT_TOKENS")
            .map(|(_, value)| value.unwrap().to_str().unwrap().to_owned());
        (arguments, cap)
    };
    let has =
        |arguments: &[String], pair: [&str; 2]| arguments.windows(2).any(|window| window == pair);
    let (off, cap) = launch(false, Some(16_000));
    assert!(has(&off, ["--tools", ""]), "{off:?}");
    assert!(
        has(&off, ["--system-prompt", CLAUDE_NO_TOOLS_PROMPT]),
        "{off:?}"
    );
    assert!(has(&off, ["--max-turns", "1"]), "{off:?}");
    assert!(has(&off, ["--output-format", "stream-json"]), "{off:?}");
    assert!(
        off.iter().any(|argument| argument == "--verbose"),
        "{off:?}"
    );
    assert!(!has(&off, ["--output-format", "text"]), "{off:?}");
    assert_eq!(cap.as_deref(), Some("16000"));
    assert!(CLAUDE_NO_TOOLS_PROMPT.contains("no tools"));
    let (on, cap) = launch(true, None);
    assert!(has(&on, ["--tools", "default"]), "{on:?}");
    assert!(has(&on, ["--output-format", "stream-json"]), "{on:?}");
    assert!(!has(&on, ["--output-format", "text"]), "{on:?}");
    assert!(
        !on.iter()
            .any(|argument| argument == "--system-prompt" || argument == "--max-turns")
    );
    assert_eq!(cap, None);
}

#[test]
fn the_engine_makes_a_repository_only_in_a_folder_it_created() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("git-home");
    fs::create_dir(&home).unwrap();
    let refused = |folder: &Path| match ensure_repository(folder, &home) {
        Err(RunError::Invalid(reason)) => reason.contains("not a git repository"),
        _ => false,
    };

    let owners = base.join("owners");
    fs::create_dir_all(owners.join(kernel_common::names::CRAZYAGENT_DIRECTORY)).unwrap();
    fs::write(owners.join("notes.txt"), "the owner's file\n").unwrap();
    assert!(refused(&owners), "an owner's folder gets no repository");
    assert!(!owners.join(".git").exists());
    let state = owners.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
    assert!(refused(&state), "nor does its .crazyagent folder");
    assert!(!state.join(".git").exists());

    let linked = base.join("linked");
    fs::create_dir(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        "gitdir: /elsewhere/.git/worktrees/linked\n",
    )
    .unwrap();
    ensure_repository(&linked, &home).unwrap();
    assert!(
        fs::symlink_metadata(linked.join(".git")).unwrap().is_file(),
        "a linked worktree stays as the owner made it"
    );

    let created = base.join("created");
    fs::create_dir(&created).unwrap();
    ensure_repository(&created, &home).unwrap();
    assert!(
        created.join(".git").is_dir(),
        "a folder the engine made gets its repository"
    );
    ensure_repository(&created, &home).unwrap();
}

/// A team or research agent's worktree, added the way the runtime adds it, is
/// local-only: the owner's own git pushing from it gets the plain refusal and the
/// remote gains nothing.
#[cfg(target_os = "macos")]
#[test]
fn an_added_agent_worktree_refuses_remotes_with_the_plain_message() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let project = base.join("project");
    let home = base.join("agent").join("home");
    let tmp = base.join("agent").join("tmp");
    let profiles = base.join("profiles");
    let remote = base.join("remote.git");
    for folder in [&project, &home, &tmp, &profiles] {
        fs::create_dir_all(folder).unwrap();
    }
    ensure_repository(&project, &tmp).unwrap();
    let owner_git = |dir: &Path, args: &[&str]| {
        kernel_processes::spawn_gate::output(
            Command::new(GIT)
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .arg("-C")
                .arg(dir)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap()
    };
    let remote_path = remote.to_str().unwrap();
    assert!(
        owner_git(&base, &["init", "-q", "--bare", remote_path])
            .status
            .success()
    );
    assert!(
        owner_git(&project, &["remote", "add", "origin", remote_path])
            .status
            .success()
    );
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let profile = profiles.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let worktree = project.join("worktrees").join("agent-one");
    add_worktree(&sandbox, &profile, &worktree).unwrap();
    let pushed = owner_git(&worktree, &["push", "origin", "HEAD:refs/heads/agent-work"]);
    assert!(!pushed.status.success(), "{pushed:?}");
    assert!(
        String::from_utf8_lossy(&pushed.stderr).contains(crate::worktrees::REMOTE_REFUSAL),
        "{pushed:?}"
    );
    let refs = owner_git(&remote, &["for-each-ref"]);
    assert!(refs.status.success(), "{refs:?}");
    assert!(refs.stdout.is_empty(), "{refs:?}");
}
