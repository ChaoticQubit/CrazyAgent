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

#[cfg(unix)]
mod import {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    const TRACKED_RULE: &str = "TRACKED-CLAUDE-RULE-81";
    const TRACKED_DOT_RULE: &str = "TRACKED-DOT-CLAUDE-RULE-82";

    /// One owner checkout on `main` with its instruction files committed, a bare remote it
    /// pushed to, and an owner home with no git config, so the engine reads only the
    /// repository's own identity.
    struct Checkout {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        folder: PathBuf,
        remote: PathBuf,
    }

    /// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
    /// Inputs: the owner home, the folder and the arguments. Output: standard output.
    fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
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
        String::from_utf8(output.stdout).unwrap()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    impl Checkout {
        fn new() -> Self {
            let tmp = tempfile::Builder::new()
                .prefix("ca-icommit-")
                .tempdir_in(fs::canonicalize("/tmp").unwrap())
                .unwrap();
            let root = fs::canonicalize(tmp.path()).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let (home, folder, remote) = (
                root.join("home"),
                root.join("repo"),
                root.join("remote.git"),
            );
            for made in [&home, &folder] {
                fs::create_dir(made).unwrap();
            }
            git(&home, &folder, &["init", "-q", "--template=", "-b", "main"]);
            git(&home, &folder, &["config", "user.name", "Repo Owner"]);
            git(
                &home,
                &folder,
                &["config", "user.email", "repo@example.invalid"],
            );
            git(&home, &folder, &["config", "commit.gpgSign", "false"]);
            write(
                &folder.join("CLAUDE.md"),
                &format!("# Rules\n\n{TRACKED_RULE}\n"),
            );
            write(
                &folder.join(".claude/CLAUDE.md"),
                &format!("{TRACKED_DOT_RULE}\n"),
            );
            write(&folder.join("README.md"), "Read me.\n");
            write(
                &folder.join(".gitignore"),
                ".crazyagent/\nCLAUDE.local.md\n",
            );
            git(&home, &folder, &["add", "-A"]);
            git(&home, &folder, &["commit", "-q", "-m", "Start"]);
            git(
                &home,
                &root,
                &["init", "-q", "--bare", "--template=", "remote.git"],
            );
            git(
                &home,
                &folder,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            git(&home, &folder, &["push", "-q", "origin", "main"]);
            Self {
                _tmp: tmp,
                root,
                home,
                folder,
                remote,
            }
        }

        fn git(&self, args: &[&str]) -> String {
            git(&self.home, &self.folder, args)
        }

        fn tip(&self) -> String {
            self.git(&["rev-parse", "main"]).trim().to_owned()
        }
    }

    fn marks(files: &[ImportFile]) -> Vec<(&str, bool, bool)> {
        files
            .iter()
            .map(|file| (file.path.as_str(), file.tracked, file.uncommitted))
            .collect()
    }

    #[test]
    fn committing_the_import_move_holds_only_the_moved_tracked_files_and_keeps_other_work() {
        let checkout = Checkout::new();
        write(
            &checkout.folder.join("CLAUDE.local.md"),
            "UNTRACKED-LOCAL-RULE\n",
        );
        let start = checkout.tip();
        let moved = import(&checkout.folder).unwrap();
        assert_eq!(
            marks(&moved),
            [
                ("CLAUDE.md", true, true),
                (".claude/CLAUDE.md", true, true),
                ("CLAUDE.local.md", false, false)
            ]
        );
        assert_eq!(
            uncommitted(&checkout.folder).unwrap(),
            ["CLAUDE.md", ".claude/CLAUDE.md"]
        );
        write(&checkout.folder.join("README.md"), "Read me twice.\n");
        let made = commit(&checkout.folder, &checkout.home).unwrap();
        assert_eq!(made.branch, "main");
        assert_eq!(made.files, ["CLAUDE.md", ".claude/CLAUDE.md"]);
        assert_eq!(checkout.tip(), made.commit);
        assert_eq!(
            checkout
                .git(&[
                    "log",
                    "-1",
                    "--format=%P|%an <%ae>|%cn <%ce>|%s",
                    &made.commit
                ])
                .trim(),
            format!(
                "{start}|Repo Owner <repo@example.invalid>|Repo Owner <repo@example.invalid>|Move CLAUDE.md, .claude/CLAUDE.md into .crazyagent/imported/"
            )
        );
        assert_eq!(
            checkout.git(&[
                "diff",
                "--name-status",
                "--no-renames",
                &start,
                &made.commit
            ]),
            "D\t.claude/CLAUDE.md\nD\tCLAUDE.md\n",
            "the commit holds the two tracked deletions and nothing else"
        );
        assert_eq!(
            checkout.git(&["status", "--porcelain"]),
            " M README.md\n",
            "the owner's own edit stays unstaged and out of the commit"
        );
        assert_eq!(
            git(&checkout.home, &checkout.remote, &["rev-parse", "main"]).trim(),
            start,
            "nothing was pushed"
        );
        assert_eq!(
            checkout
                .git(&["rev-parse", "refs/remotes/origin/main"])
                .trim(),
            start
        );
        assert!(uncommitted(&checkout.folder).unwrap().is_empty());
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_none"
        );
        let instructions =
            fs::read_to_string(checkout.folder.join(".crazyagent/instructions.md")).unwrap();
        assert!(instructions.contains(TRACKED_RULE) && instructions.contains(TRACKED_DOT_RULE));
    }

    #[test]
    fn the_import_commit_refuses_and_changes_nothing() {
        let checkout = Checkout::new();
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_none",
            "nothing was moved"
        );
        import(&checkout.folder).unwrap();
        let start = checkout.tip();
        let unchanged = |staged: &str| {
            assert_eq!(checkout.tip(), start);
            assert_eq!(checkout.git(&["diff", "--cached", "--name-only"]), staged);
            assert_eq!(
                uncommitted(&checkout.folder).unwrap(),
                ["CLAUDE.md", ".claude/CLAUDE.md"]
            );
        };
        write(&checkout.folder.join("NEW.md"), "new\n");
        checkout.git(&["add", "NEW.md"]);
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_staged"
        );
        unchanged("NEW.md\n");
        checkout.git(&["rm", "-q", "--cached", "NEW.md"]);
        for state in ["MERGE_HEAD", "index.lock"] {
            let path = checkout.folder.join(".git").join(state);
            fs::write(&path, format!("{start}\n")).unwrap();
            assert_eq!(
                commit(&checkout.folder, &checkout.home).unwrap_err().0,
                "process_commit_busy",
                "{state}"
            );
            fs::remove_file(&path).unwrap();
            unchanged("");
        }
        checkout.git(&["config", "--unset", "user.name"]);
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_identity"
        );
        unchanged("");
        checkout.git(&["config", "user.name", "Repo Owner"]);
        checkout.git(&["checkout", "-q", "--detach"]);
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_detached"
        );
        assert_eq!(checkout.git(&["rev-parse", "HEAD"]).trim(), start);
        checkout.git(&["checkout", "-q", "main"]);
        unchanged("");
        checkout.git(&["checkout", "-q", "--", "CLAUDE.md", ".claude/CLAUDE.md"]);
        assert!(
            uncommitted(&checkout.folder).unwrap().is_empty(),
            "restored originals need no commit"
        );
        assert_eq!(
            commit(&checkout.folder, &checkout.home).unwrap_err().0,
            "process_commit_none"
        );
        assert_eq!(checkout.tip(), start);
    }

    #[test]
    fn a_folder_without_git_moves_its_files_with_nothing_to_commit() {
        let checkout = Checkout::new();
        let plain = checkout.root.join("plain");
        write(&plain.join("CLAUDE.md"), "PLAIN-RULE\n");
        let moved = import(&plain).unwrap();
        assert_eq!(marks(&moved), [("CLAUDE.md", false, false)]);
        assert!(plain.join(".crazyagent/imported/CLAUDE.md").is_file());
        assert!(uncommitted(&plain).unwrap().is_empty());
        assert_eq!(
            commit(&plain, &checkout.home).unwrap_err().0,
            "process_commit_none"
        );
    }

    #[test]
    fn the_onboarding_moved_list_marks_a_tracked_move_until_it_is_committed() {
        let checkout = Checkout::new();
        let connection = Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        let project = ProjectId::new("import-commit").unwrap();
        record_moved(&connection, &project, &import(&checkout.folder).unwrap()).unwrap();
        let listed = || {
            moved(&connection, &project, &checkout.folder)
                .unwrap()
                .into_iter()
                .map(|file| (file.path, file.uncommitted))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            listed(),
            [
                ("CLAUDE.md".to_owned(), true),
                (".claude/CLAUDE.md".to_owned(), true)
            ]
        );
        commit(&checkout.folder, &checkout.home).unwrap();
        assert_eq!(
            listed(),
            [
                ("CLAUDE.md".to_owned(), false),
                (".claude/CLAUDE.md".to_owned(), false)
            ]
        );
    }
}
