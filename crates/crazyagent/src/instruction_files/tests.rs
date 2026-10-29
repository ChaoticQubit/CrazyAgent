use super::*;

#[test]
fn settings_default_to_both_texts_with_transcripts_and_store_every_option() {
    let connection = Connection::open_in_memory().unwrap();
    let project = ProjectId::new("instructions-1").unwrap();
    assert_eq!(
        load(&connection, &project).unwrap(),
        Settings {
            mode: Mode::Both,
            transcripts: true
        },
        "a database that never stored settings gets the defaults"
    );
    initialize(&connection).unwrap();
    assert_eq!(load(&connection, &project).unwrap(), Settings::default());
    for mode in [Mode::ProjectOnly, Mode::AppOnly, Mode::Both] {
        for transcripts in [true, false] {
            let settings = Settings { mode, transcripts };
            save(&connection, &project, settings).unwrap();
            assert_eq!(load(&connection, &project).unwrap(), settings);
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
    }
    let other = ProjectId::new("instructions-2").unwrap();
    assert_eq!(load(&connection, &other).unwrap(), Settings::default());
    connection
        .execute(
            "UPDATE agent_instruction_settings SET mode='everything' WHERE project=?1",
            [project.as_str()],
        )
        .unwrap();
    assert_eq!(
        load(&connection, &project).unwrap_err(),
        "instruction_settings_invalid"
    );
    connection
        .execute(
            "UPDATE agent_instruction_settings SET mode='both', transcripts=2 WHERE project=?1",
            [project.as_str()],
        )
        .unwrap();
    assert_eq!(
        load(&connection, &project).unwrap_err(),
        "instruction_settings_invalid"
    );
}

#[cfg(target_os = "macos")]
mod launch {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    const PROJECT_CLAUDE: &str = "PROJECT-CLAUDE-RULE-71";
    const PROJECT_DOT_CLAUDE: &str = "PROJECT-DOT-CLAUDE-RULE-72";
    const PROJECT_AGENTS: &str = "PROJECT-AGENTS-RULE-73";
    const PROJECT_LOCAL: &str = "PROJECT-LOCAL-RULE-74";
    const USER_CLAUDE: &str = "USER-CLAUDE-RULE-75";
    const USER_CODEX: &str = "USER-CODEX-RULE-76";
    const UNCOMMITTED: &str = "UNCOMMITTED-OWNER-EDIT-77";

    /// One test's folders: the owner's checkout, a linked agent worktree of it, the owner's
    /// home with its own instruction files, and the engine data folder.
    struct Case {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        checkout: PathBuf,
        data: PathBuf,
    }

    /// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
    /// Inputs: the owner home, the folder and the arguments. Output: unit.
    fn git(home: &Path, dir: &Path, args: &[&str]) {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
                .args([
                    "-c",
                    "user.name=Owner",
                    "-c",
                    "user.email=owner@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .arg("-C")
                .arg(dir)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    impl Case {
        /// Makes a checkout whose HEAD commits the project's instruction files, an untracked
        /// `CLAUDE.local.md`, an uncommitted edit of `CLAUDE.md`, and an owner home whose
        /// `~/.claude/CLAUDE.md` is a link into a dotfiles folder, as the owner's often is.
        fn new() -> Self {
            let tmp = tempfile::Builder::new()
                .prefix("ca-ifiles-")
                .tempdir_in(fs::canonicalize("/tmp").unwrap())
                .unwrap();
            let root = fs::canonicalize(tmp.path()).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let (checkout, home, data) = (root.join("repo"), root.join("home"), root.join("data"));
            for folder in [&checkout, &home, &data] {
                fs::create_dir(folder).unwrap();
            }
            fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
            git(&home, &checkout, &["init", "-q"]);
            write(
                &checkout.join("CLAUDE.md"),
                &format!("# Rules\n\n{PROJECT_CLAUDE}\n"),
            );
            write(
                &checkout.join(".claude/CLAUDE.md"),
                &format!("{PROJECT_DOT_CLAUDE}\n"),
            );
            write(&checkout.join("AGENTS.md"), &format!("{PROJECT_AGENTS}\n"));
            write(
                &checkout.join(".gitignore"),
                "CLAUDE.local.md\n.crazyagent/\n",
            );
            git(&home, &checkout, &["add", "-A"]);
            git(&home, &checkout, &["commit", "-q", "-m", "rules"]);
            write(
                &checkout.join("CLAUDE.local.md"),
                &format!("{PROJECT_LOCAL}\n"),
            );
            write(
                &checkout.join("CLAUDE.md"),
                &format!("# Rules\n\n{UNCOMMITTED}\n"),
            );
            fs::create_dir(checkout.join(".crazyagent")).unwrap();
            write(
                &root.join("dotfiles/CLAUDE.md"),
                &format!("{USER_CLAUDE}\n"),
            );
            fs::create_dir(home.join(".claude")).unwrap();
            std::os::unix::fs::symlink(
                root.join("dotfiles/CLAUDE.md"),
                home.join(".claude/CLAUDE.md"),
            )
            .unwrap();
            write(&home.join(".codex/AGENTS.md"), &format!("{USER_CODEX}\n"));
            Self {
                _tmp: tmp,
                root,
                checkout,
                data,
            }
        }
    }
    use crate::harnesses::launch::{
        AccountBinding, LaunchLimits, LaunchRequest, command_with_context,
    };
    use crate::project_setup::{StoredSetup, TeamPlan};

    /// Records one project in a real engine database whose folder is the case's checkout.
    fn engine(case: &Case) -> (Connection, ProjectId) {
        let project = ProjectId::new("instructions-launch").unwrap();
        let connection = Connection::open(case.data.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, body) VALUES (?1, '{}')",
                [project.as_str()],
            )
            .unwrap();
        let setup = StoredSetup {
            project_id: project.clone(),
            name: "Instructions".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "codex".into(),
                master_tools: false,
                sub_harnesses: vec![],
                min_agents: 1,
                max_agents: 1,
            },
            documents: vec![],
            folder: case.checkout.display().to_string(),
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
        (connection, project)
    }

    fn agent_home(case: &Case, name: &str) -> PathBuf {
        let home = case.root.join(name);
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        home
    }

    fn request(case: &Case, harness: &str, home: &Path) -> LaunchRequest {
        let profile = case.root.join("agent.sb");
        fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        LaunchRequest {
            harness: harness.into(),
            executable: PathBuf::from("/usr/bin/true"),
            working_dir: case.checkout.clone(),
            account: AccountBinding::Url {
                home: home.to_path_buf(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                key: None,
            },
            model: Some("qwen2.5:0.5b".into()),
            tools: false,
            sandbox_profile: Some(profile),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        }
    }

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_headless_claude_run_keeps_its_transcript_only_while_the_setting_is_on() {
        let case = Case::new();
        let (connection, project) = engine(&case);
        for transcripts in [true, false] {
            save(
                &connection,
                &project,
                Settings {
                    mode: Mode::Both,
                    transcripts,
                },
            )
            .unwrap();
            let context = crate::launch_context::for_project(
                &connection,
                &project,
                crate::agent_git::Layout::Linked,
            )
            .unwrap();
            let home = agent_home(&case, &format!("claude-transcripts-{transcripts}"));
            let (command, installed) =
                command_with_context(&request(&case, "claude-code", &home), &context).unwrap();
            assert_eq!(installed.keeps_transcript(), transcripts);
            let arguments = arguments(&command);
            assert_eq!(
                arguments
                    .iter()
                    .any(|argument| argument == "--no-session-persistence"),
                !transcripts,
                "transcripts {transcripts}"
            );
            assert!(arguments.iter().any(|argument| argument == "--print"));
        }
        let plain = crate::harnesses::launch::command(&request(
            &case,
            "claude-code",
            &agent_home(&case, "claude-plain"),
        ))
        .unwrap();
        assert!(
            arguments(&plain)
                .iter()
                .any(|argument| argument == "--no-session-persistence"),
            "a launch with no project context writes no transcript, as before"
        );
    }
}
