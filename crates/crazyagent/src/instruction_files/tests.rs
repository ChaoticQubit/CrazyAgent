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
const AGENT_EDIT: &str = "AGENT-WORKTREE-EDIT-78";

/// One test's folders: the owner's checkout, a linked agent worktree of it, the owner's
/// home with its own instruction files, and the engine data folder.
struct Case {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    checkout: PathBuf,
    home: PathBuf,
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
            home,
            data,
        }
    }

    fn read(&self, choice: Choice) -> Delivery {
        read(Some(&self.checkout), Some(&self.home), choice)
    }
}

fn given(delivery: &Delivery) -> Vec<&str> {
    delivery
        .records
        .iter()
        .filter(|record| record.state == State::Given)
        .map(|record| record.name.as_str())
        .collect()
}

#[test]
fn each_choice_gives_only_its_files_as_committed_at_the_checkouts_head() {
    let case = Case::new();

    let project = case.read(Choice::Project);
    for rule in [
        PROJECT_CLAUDE,
        PROJECT_DOT_CLAUDE,
        PROJECT_AGENTS,
        PROJECT_LOCAL,
    ] {
        assert!(project.text.contains(rule), "{rule}");
    }
    for rule in [USER_CLAUDE, USER_CODEX, UNCOMMITTED] {
        assert!(!project.text.contains(rule), "{rule}");
    }
    assert_eq!(
        given(&project),
        [
            "CLAUDE.md",
            ".claude/CLAUDE.md",
            "AGENTS.md",
            "CLAUDE.local.md"
        ]
    );
    assert!(project.text.starts_with("\n## Instruction files\n"));
    assert!(
        project.text.find(PROJECT_CLAUDE).unwrap() < project.text.find(PROJECT_AGENTS).unwrap()
    );
    assert_eq!(
        project.records[0].bytes,
        format!("# Rules\n\n{PROJECT_CLAUDE}\n").len() as u64,
        "the record holds the committed file's size"
    );

    let both = case.read(Choice::ProjectAndUser);
    for rule in [
        PROJECT_CLAUDE,
        PROJECT_AGENTS,
        PROJECT_LOCAL,
        USER_CLAUDE,
        USER_CODEX,
    ] {
        assert!(both.text.contains(rule), "{rule}");
    }
    assert!(!both.text.contains(UNCOMMITTED));
    assert_eq!(
        given(&both),
        [
            "CLAUDE.md",
            ".claude/CLAUDE.md",
            "AGENTS.md",
            "CLAUDE.local.md",
            "~/.claude/CLAUDE.md",
            "~/.codex/AGENTS.md"
        ]
    );

    let none = case.read(Choice::None);
    assert_eq!(none, Delivery::default());

    let folder = case.root.join("plain-folder");
    write(&folder.join("CLAUDE.md"), "UNTRACKED-RULE\n");
    write(
        &folder.join("CLAUDE.local.md"),
        &format!("{PROJECT_LOCAL}\n"),
    );
    let plain = read(Some(&folder), None, Choice::Project);
    assert_eq!(
        given(&plain),
        ["CLAUDE.local.md"],
        "a folder that is not a git checkout has no committed files"
    );
    assert_eq!(plain.records.len(), 1);
    let unborn = case.root.join("unborn");
    fs::create_dir(&unborn).unwrap();
    git(&case.home, &unborn, &["init", "-q"]);
    write(&unborn.join("CLAUDE.md"), "UNCOMMITTED-RULE\n");
    assert_eq!(
        read(Some(&unborn), None, Choice::Project),
        Delivery::default(),
        "a checkout with no commit has no committed files"
    );

    assert_eq!(
        read(None, Some(&case.home), Choice::ProjectAndUser)
            .records
            .len(),
        2,
        "a project with no recorded folder still gets the owner's own files"
    );
    assert_eq!(
        read(Some(&case.checkout), None, Choice::ProjectAndUser)
            .records
            .len(),
        4
    );
}

#[test]
fn a_file_that_cannot_be_given_is_recorded_with_its_reason_and_hidden_text_is_shown() {
    let case = Case::new();
    let home = &case.home;
    fs::remove_file(case.checkout.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink("CLAUDE.md", case.checkout.join("AGENTS.md")).unwrap();
    write(
        &case.checkout.join(".claude/CLAUDE.md"),
        &"x".repeat(FILE_BYTES_MAX + 1),
    );
    write(&case.checkout.join("CLAUDE.md"), "Keep\u{200b} it short.\n");
    git(home, &case.checkout, &["add", "-A"]);
    git(home, &case.checkout, &["commit", "-q", "-m", "edge cases"]);
    fs::remove_file(case.checkout.join("CLAUDE.local.md")).unwrap();
    std::os::unix::fs::symlink(
        case.root.join("dotfiles/CLAUDE.md"),
        case.checkout.join("CLAUDE.local.md"),
    )
    .unwrap();
    fs::write(home.join(".codex/AGENTS.md"), "Keep\u{200b} it short.\n").unwrap();

    let delivery = case.read(Choice::ProjectAndUser);
    let states: Vec<(&str, State)> = delivery
        .records
        .iter()
        .map(|record| (record.name.as_str(), record.state))
        .collect();
    assert_eq!(
        states,
        [
            ("CLAUDE.md", State::Given),
            (".claude/CLAUDE.md", State::TooLarge),
            ("AGENTS.md", State::Unsafe),
            ("CLAUDE.local.md", State::Unsafe),
            ("~/.claude/CLAUDE.md", State::Given),
            ("~/.codex/AGENTS.md", State::Same),
        ]
    );
    assert!(delivery.text.contains("Keep\\u200b it short."));
    assert!(!delivery.text.contains('\u{200b}'));
    assert_eq!(delivery.records[1].bytes, FILE_BYTES_MAX as u64 + 1);
    assert!(
        !delivery.text.contains(&"x".repeat(64)),
        "a file over its bound is left out whole, never cut"
    );

    let fill = FILE_BYTES_MAX - 16;
    fs::remove_file(case.checkout.join("CLAUDE.local.md")).unwrap();
    write(&case.checkout.join("CLAUDE.local.md"), &"y".repeat(fill));
    fs::remove_file(home.join(".claude/CLAUDE.md")).unwrap();
    write(&home.join(".claude/CLAUDE.md"), &"w".repeat(fill));
    write(&home.join(".codex/AGENTS.md"), &"z".repeat(fill));
    let full = case.read(Choice::ProjectAndUser);
    let states: Vec<State> = full.records.iter().map(|record| record.state).collect();
    assert_eq!(
        states,
        [
            State::Given,
            State::TooLarge,
            State::Unsafe,
            State::Given,
            State::Given,
            State::TooLarge
        ],
        "past the section's bound a file is left out whole"
    );
    assert!(full.text.contains(&"w".repeat(fill)));
    assert!(!full.text.contains(&"z".repeat(64)));
    assert!(full.text.len() <= SECTION_BYTES_MAX);
}

#[test]
fn settings_default_to_project_files_with_transcripts_and_store_every_option() {
    let connection = Connection::open_in_memory().unwrap();
    let project = ProjectId::new("instructions-1").unwrap();
    assert_eq!(
        load(&connection, &project).unwrap(),
        Settings {
            files: Choice::Project,
            transcripts: true
        },
        "a database that never stored settings gets the defaults"
    );
    initialize(&connection).unwrap();
    assert_eq!(load(&connection, &project).unwrap(), Settings::default());
    for files in [Choice::Project, Choice::ProjectAndUser, Choice::None] {
        for transcripts in [true, false] {
            let settings = Settings { files, transcripts };
            save(&connection, &project, settings).unwrap();
            assert_eq!(load(&connection, &project).unwrap(), settings);
            assert_eq!(Choice::parse(files.as_str()), Some(files));
        }
    }
    let other = ProjectId::new("instructions-2").unwrap();
    assert_eq!(load(&connection, &other).unwrap(), Settings::default());
    connection
        .execute(
            "UPDATE agent_instruction_settings SET files='everything' WHERE project=?1",
            [project.as_str()],
        )
        .unwrap();
    assert_eq!(
        load(&connection, &project).unwrap_err(),
        "instruction_settings_invalid"
    );
    connection
        .execute(
            "UPDATE agent_instruction_settings SET files='none', transcripts=2 WHERE project=?1",
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
    use crate::agent_git::Layout;
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

    fn after<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
        arguments
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    }

    /// Launches one Claude and one Codex agent of the project through the production
    /// launch path and returns what each was handed: Claude's prompt file and Codex's
    /// AGENTS.md and config.toml, with the record each home keeps.
    fn launch_both(
        case: &Case,
        connection: &Connection,
        project: &ProjectId,
        round: &str,
    ) -> Launched {
        let context =
            crate::launch_context::for_project(connection, project, Layout::Linked).unwrap();
        let claude = agent_home(case, &format!("claude-{round}"));
        let (command, installed) =
            command_with_context(&request(case, "claude-code", &claude), &context).unwrap();
        let claude_arguments = arguments(&command);
        let prompt = fs::read_to_string(
            after(&claude_arguments, "--append-system-prompt-file").expect("a prompt file"),
        )
        .unwrap();
        let claude_record = crate::agent_home::given_files(&case.data, &claude).unwrap();
        drop(installed);
        let codex = agent_home(case, &format!("codex-{round}"));
        let (_, installed) =
            command_with_context(&request(case, "codex", &codex), &context).unwrap();
        let agents = fs::read_to_string(codex.join("AGENTS.md")).unwrap_or_default();
        let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        let codex_record = crate::agent_home::given_files(&case.data, &codex).unwrap();
        drop(installed);
        Launched {
            claude_arguments,
            prompt,
            agents,
            config,
            claude_record,
            codex_record,
        }
    }

    struct Launched {
        claude_arguments: Vec<String>,
        prompt: String,
        agents: String,
        config: toml::Table,
        claude_record: Vec<Record>,
        codex_record: Vec<Record>,
    }

    #[test]
    fn each_option_reaches_claude_and_codex_through_the_production_launch_path() {
        let case = Case::new();
        OWNER_HOME.with(|home| *home.borrow_mut() = Some(case.home.clone()));
        let (connection, project) = engine(&case);
        let cases = [
            (
                Choice::Project,
                vec![PROJECT_CLAUDE, PROJECT_AGENTS, PROJECT_LOCAL],
                vec![USER_CLAUDE, USER_CODEX],
            ),
            (
                Choice::ProjectAndUser,
                vec![
                    PROJECT_CLAUDE,
                    PROJECT_AGENTS,
                    PROJECT_LOCAL,
                    USER_CLAUDE,
                    USER_CODEX,
                ],
                vec![],
            ),
            (
                Choice::None,
                vec![],
                vec![
                    PROJECT_CLAUDE,
                    PROJECT_AGENTS,
                    PROJECT_LOCAL,
                    USER_CLAUDE,
                    USER_CODEX,
                ],
            ),
        ];
        for (choice, present, absent) in cases {
            save(
                &connection,
                &project,
                Settings {
                    files: choice,
                    transcripts: true,
                },
            )
            .unwrap();
            let launched = launch_both(&case, &connection, &project, choice.as_str());
            assert_eq!(
                after(&launched.claude_arguments, "--setting-sources"),
                Some(""),
                "{choice:?}: settings files stay out, only instruction text goes in"
            );
            for rule in &present {
                assert!(launched.prompt.contains(rule), "{choice:?} Claude: {rule}");
                assert!(launched.agents.contains(rule), "{choice:?} Codex: {rule}");
            }
            for rule in absent.iter().chain(&[UNCOMMITTED]) {
                assert!(!launched.prompt.contains(rule), "{choice:?} Claude: {rule}");
                assert!(!launched.agents.contains(rule), "{choice:?} Codex: {rule}");
            }
            assert_eq!(
                launched.prompt.contains("## Instruction files"),
                choice != Choice::None
            );
            assert_eq!(
                launched.config["project_doc_max_bytes"].as_integer(),
                Some(0),
                "{choice:?}: Codex never reads AGENTS.md from the agent's worktree itself"
            );
            let expected = read(Some(&case.checkout), Some(&case.home), choice).records;
            assert_eq!(launched.claude_record, expected, "{choice:?}");
            assert_eq!(launched.codex_record, expected, "{choice:?}");
            let count = match choice {
                Choice::Project => 4,
                Choice::ProjectAndUser => 6,
                Choice::None => 0,
            };
            assert_eq!(expected.len(), count, "{choice:?}");
            assert!(expected.iter().all(|record| record.state == State::Given));
        }
        OWNER_HOME.with(|home| *home.borrow_mut() = None);
    }

    #[test]
    fn an_agent_that_edits_claude_md_in_its_worktree_does_not_change_the_next_launch() {
        let case = Case::new();
        let (connection, project) = engine(&case);
        let worktree = case.root.join("agent-worktree");
        git(
            &case.home,
            &case.checkout,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent-1",
                worktree.to_str().unwrap(),
            ],
        );
        let first = launch_both(&case, &connection, &project, "first");
        write(&worktree.join("CLAUDE.md"), &format!("{AGENT_EDIT}\n"));
        write(&worktree.join("AGENTS.md"), &format!("{AGENT_EDIT}\n"));
        write(
            &worktree.join("CLAUDE.local.md"),
            &format!("{AGENT_EDIT}\n"),
        );
        git(&case.home, &worktree, &["add", "-A", "-f"]);
        git(
            &case.home,
            &worktree,
            &["commit", "-q", "-m", "agent rules"],
        );

        let next = launch_both(&case, &connection, &project, "next");
        assert!(!next.prompt.contains(AGENT_EDIT));
        assert!(!next.agents.contains(AGENT_EDIT));
        assert!(next.prompt.contains(PROJECT_CLAUDE));
        assert_eq!(next.prompt, first.prompt);
        assert_eq!(next.agents, first.agents);
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
                    files: Choice::Project,
                    transcripts,
                },
            )
            .unwrap();
            let context =
                crate::launch_context::for_project(&connection, &project, Layout::Linked).unwrap();
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
