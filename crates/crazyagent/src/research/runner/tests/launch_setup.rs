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

/// DEC-170 addendum 6 (RV-DS241-27): a research job its runner hands back because a
/// projects home move holds its folder spends no attempt, is not claimed again until its
/// backoff ends, shows why it waits and records no start, since no harness ran.
#[test]
fn a_research_job_refused_for_a_home_move_backs_off_shows_why_and_records_no_start() {
    let root = engine_root();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    register_project(&root.database, "alpha", &folder);
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    connection
        .execute(
            "INSERT INTO projects_home_move VALUES (1,'request-7',7,?1,?2,'[\"repo\"]')",
            rusqlite::params![
                root.base.to_str().unwrap(),
                root.base.join("next").to_str().unwrap()
            ],
        )
        .unwrap();
    let runner = ResearchRunner::new(
        &root.database,
        &root.state,
        Path::new("/usr/bin/true"),
        ResearchLimits::default(),
    )
    .unwrap();
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: None,
    };
    let run = runner.run_once(&project).unwrap().unwrap();
    assert_eq!((run.job, run.status), (id, ResearchStatus::Queued));
    assert!(
        runner.run_once(&project).unwrap().is_none(),
        "the job waits out its backoff"
    );
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!((job.attempts, job.claimer), (0, None));
    assert_eq!(
        job.error.as_deref(),
        Some("Waiting for the projects home move")
    );
    let started: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM project_activity WHERE kind='research_started'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(started, 0, "no harness started");
}
