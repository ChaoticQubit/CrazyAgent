#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide;
use crazyagent::ide::fetch::BuildState;
use crazyagent::protocol::ProjectId;
use crazyagent::sandbox::policy::AgentPolicy;
use e2e_support::{Engine, Owner, git, repository, scratch};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const PROJECT: &str = "v2-ide-hooks";
const BUILD: &str = "vscodium-1.143.17183";
const SERVER_DATA: &str = "server-data";

/// Records the pinned IDE build kept in a `.local-state/ide-cache` above the crate as the
/// engine's ready build, the state a finished fetch records, and reads it back through the
/// owner protocol, so the owner's IDE open starts a real server from it. Inputs: the
/// engine and its owner. Output: none.
async fn ready_build(engine: &Engine, owner: &Owner) {
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|folder| folder.join(".local-state").join("ide-cache").join(BUILD))
        .find(|build| build.join("out").join("server-main.js").is_file())
        .expect("no pinned VSCodium reh-web build in a .local-state/ide-cache above the crate");
    let ready = BuildState::Ready {
        version: BUILD.trim_start_matches("vscodium-").to_owned(),
        root: fs::canonicalize(build).unwrap(),
    };
    let ide = engine.path().join("ide");
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&ide)
        .unwrap();
    fs::write(
        ide.join("build-state.json"),
        serde_json::to_vec(&ready).unwrap(),
    )
    .unwrap();
    let client = owner.controller.owner_client().unwrap();
    assert_eq!(client.ide_tab_state(false).await.unwrap().build, ready);
}

/// One project set up through the running engine, with a real repository in its folder
/// whose pre-commit hook appends a line to a marker file outside the repository, and an
/// agent worktree below the project folder as the team runtime adds one.
struct Hooked {
    engine: Engine,
    owner: Owner,
    project: ProjectId,
    folder: PathBuf,
    agent: PathBuf,
    marker: PathBuf,
    _disk: tempfile::TempDir,
}

impl Hooked {
    /// Starts the engine, sets the projects home and creates the project through the owner
    /// protocol, then puts the hooked repository in the folder the engine recorded.
    /// Input: a scratch prefix. Output: the case.
    async fn start(prefix: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-ide-home-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(PROJECT, "Buoy hooks").await,
            OwnerResult::Applied
        );
        let project = ProjectId::new(PROJECT).unwrap();
        let folder = ide::workspace::folder(engine.path(), &project).unwrap();
        assert!(folder.starts_with(fs::canonicalize(&home).unwrap()));
        if folder.join(".git").exists() {
            fs::write(folder.join("README.md"), "buoys\n").unwrap();
            git(&folder, &["add", "README.md"]);
            git(&folder, &["commit", "--quiet", "-m", "first"]);
        } else {
            repository(&folder, "README.md", "buoys\n");
        }
        let marker = disk.path().join("hook-ran.log");
        let hook = folder.join(".git").join("hooks").join("pre-commit");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(
            &hook,
            format!("#!/bin/sh\necho \"$(pwd -P)\" >> '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let agent = crazyagent::projects_home::state_folder(&folder)
            .join("worktrees")
            .join("agent-1");
        git(
            &folder,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                agent.to_str().unwrap(),
                "HEAD",
            ],
        );
        let agent = fs::canonicalize(agent).unwrap();
        ready_build(&engine, &owner).await;
        Self {
            engine,
            owner,
            project,
            folder,
            agent,
            marker,
            _disk: disk,
        }
    }

    /// Saves the owner's "IDE git runs the project's hooks" choice through the owner
    /// protocol, from the version the owner read, keeping the saved access level.
    /// Input: the switch. Output: none; the save must be recorded and read back.
    async fn choose(&mut self, on: bool) {
        let view = self
            .owner
            .controller
            .project_agent_policy(self.project.clone())
            .await
            .unwrap();
        let generation = self.owner.next();
        let saved = self
            .owner
            .controller
            .set_project_agent_policy(
                self.project.clone(),
                view.version,
                AgentPolicy {
                    access: view.access,
                    toolchains: Vec::new(),
                    ide_git_hooks: on,
                    tool_cache: Default::default(),
                    ide_agent_files: Default::default(),
                    network: crazyagent::sandbox::policy::AgentNetwork::default(),
                },
                generation,
            )
            .await
            .unwrap();
        assert_eq!(e2e_support::result(saved), OwnerResult::Applied);
        let read = self
            .owner
            .controller
            .project_agent_policy(self.project.clone())
            .await
            .unwrap();
        assert_eq!(read.ide_git_hooks, on);
        assert_eq!(read.version, view.version + 1);
    }

    /// Opens the project's IDE through the owner protocol on the running engine, whose IDE
    /// start reads the stored hooks switch and writes the server's locked Remote settings and
    /// git shim from it, then stops the server. Output: the shim the IDE's git runs.
    async fn ide_start(&self) -> PathBuf {
        let client = self.owner.controller.owner_client().unwrap();
        let opened = client.ide_tab_open(self.project.clone()).await.unwrap();
        assert!(opened.url.starts_with("http://127.0.0.1:"), "{opened:?}");
        assert!(client.ide_tab_stop(self.project.clone()).await.unwrap());
        let machine = ide::settings::machine_folder(
            &self
                .engine
                .path()
                .join("ide")
                .join(PROJECT)
                .join(SERVER_DATA),
        );
        let shim = machine.join(ide::git::SHIM);
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(machine.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["git.path"].as_str(),
            shim.to_str(),
            "the IDE's git is the engine's shim"
        );
        shim
    }

    /// Lines the project's hook appended, one per run, each naming where it ran.
    fn hook_runs(&self) -> Vec<String> {
        match fs::read_to_string(&self.marker) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("marker unreadable: {error}"),
        }
    }
}

/// Commits through the IDE's git shim in a folder with the environment the IDE server
/// gives every git it starts: the forced configuration and the search ceiling at the
/// project's agents root, the only root it has while the engine records no agent in an
/// older one, and the forwarder variables the connected extension host adds, without which
/// the shim refuses an index-writing command in the project folder. The forwarder here is
/// `/usr/bin/true`, as in the shim's own rows: an empty commit's carry step, if any, is not
/// what these rows check. Inputs: the shim, the project folder and where git runs.
/// Output: git's output; a failed commit fails the test.
fn ide_commit(shim: &Path, project: &Path, folder: &Path, message: &str) -> Output {
    let mut command = Command::new("/bin/sh");
    command
        .arg(shim)
        .args(["commit", "--quiet", "--allow-empty", "-m", message])
        .current_dir(folder)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", "/var/empty")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "E2E Owner")
        .env("GIT_AUTHOR_EMAIL", "e2e@example.invalid")
        .env("GIT_COMMITTER_NAME", "E2E Owner")
        .env("GIT_COMMITTER_EMAIL", "e2e@example.invalid")
        .env("CRAZYAGENT_IDE_GIT_NODE", "/usr/bin/true")
        .env("CRAZYAGENT_IDE_GIT_FORWARD", "/dev/null")
        .env("CRAZYAGENT_IDE_GIT_SECRET", "synthetic")
        .env(
            ide::git::CEILING_VARIABLE,
            crazyagent::projects_home::state_folder(project).join("worktrees"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in ide::git::CONFIG_ENV {
        command.env(key, value);
    }
    let output = crazyagent::spawn_gate::output(&mut command).unwrap();
    assert!(
        output.status.success(),
        "IDE git commit in {}: {}",
        folder.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// The subject of the newest commit in a folder. Input: the folder. Output: the subject.
fn subject(folder: &Path) -> String {
    let output = git(folder, &["log", "-1", "--format=%s"]);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_git_hooks_off_commits_from_the_ide_without_running_the_project_hook() {
    let mut case = Hooked::start("ca-v2opt-ide-off-").await;
    let fresh = case
        .owner
        .controller
        .project_agent_policy(case.project.clone())
        .await
        .unwrap();
    assert!(!fresh.ide_git_hooks, "a new project starts with Off");

    let shim = case.ide_start().await;
    ide_commit(&shim, &case.folder, &case.folder, "off by default");
    assert_eq!(subject(&case.folder), "off by default");
    assert_eq!(case.hook_runs(), Vec::<String>::new());

    case.choose(true).await;
    case.choose(false).await;
    let shim = case.ide_start().await;
    ide_commit(&shim, &case.folder, &case.folder, "off again");
    assert_eq!(subject(&case.folder), "off again");
    assert_eq!(
        case.hook_runs(),
        Vec::<String>::new(),
        "Off saved after On keeps the project's hook out of the IDE's git"
    );
    git(
        &case.folder,
        &["commit", "--quiet", "--allow-empty", "-m", "own"],
    );
    assert_eq!(
        case.hook_runs(),
        vec![case.folder.display().to_string()],
        "the hook itself is live: the owner's own git runs it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_git_hooks_on_runs_the_project_hook_in_the_project_folder_alone() {
    let mut case = Hooked::start("ca-v2opt-ide-on-").await;
    case.choose(true).await;
    let shim = case.ide_start().await;

    ide_commit(&shim, &case.folder, &case.folder, "hooked");
    assert_eq!(subject(&case.folder), "hooked");
    assert_eq!(
        case.hook_runs(),
        vec![case.folder.display().to_string()],
        "On runs the project's hook once for the IDE's commit in the project folder"
    );

    ide_commit(&shim, &case.folder, &case.agent, "agent folder");
    assert_eq!(subject(&case.agent), "agent folder");
    assert_eq!(
        case.hook_runs(),
        vec![case.folder.display().to_string()],
        "an agent worktree below the project never runs the project's hook"
    );

    case.engine.restart();
    let mut owner = Owner::attach(&case.engine).await;
    let read = owner
        .controller
        .project_agent_policy(case.project.clone())
        .await
        .unwrap();
    assert!(read.ide_git_hooks, "On survives an engine restart");
    case.owner = owner;
    let shim = case.ide_start().await;
    ide_commit(&shim, &case.folder, &case.folder, "after restart");
    assert_eq!(case.hook_runs().len(), 2);
}
