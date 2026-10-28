#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide;
use crazyagent::ide::fetch::BuildState;
use crazyagent::ide::terminal::{IDE_SHELL, SANDBOXED};
use crazyagent::protocol::ProjectId;
use crazyagent::sandbox::ide_shell::{self, Launch, TERMINAL_VARIABLES};
use e2e_support::{Engine, Owner, scratch};
use std::ffi::OsString;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PROJECT: &str = "v2-terminal";
const BUILD: &str = "vscodium-1.143.17183";
const SERVER_DATA: &str = "server-data";

/// What the engine's own IDE start left for one project: the locked Remote settings it
/// wrote, the server data folder they were written for and the Seatbelt profile the
/// server ran under.
struct Started {
    settings: serde_json::Value,
    server_data: PathBuf,
    profile: PathBuf,
}

/// The running engine with one project set up through the owner protocol, a file in its
/// folder and a loopback listener the shells try to reach.
struct Terminal {
    engine: Engine,
    owner: Owner,
    project: ProjectId,
    folder: PathBuf,
    listener: TcpListener,
    _disk: tempfile::TempDir,
}

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

impl Terminal {
    /// Starts the engine, picks a projects home and creates the project. Input: a scratch
    /// prefix. Output: the case.
    async fn start(prefix: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-term-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(PROJECT, "Buoy terminal").await,
            OwnerResult::Applied
        );
        let project = ProjectId::new(PROJECT).unwrap();
        let folder = ide::workspace::folder(engine.path(), &project).unwrap();
        fs::write(folder.join("README.md"), "buoys\n").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        ready_build(&engine, &owner).await;
        Self {
            engine,
            owner,
            project,
            folder,
            listener,
            _disk: disk,
        }
    }

    /// The probe each terminal runs: whether it reads the engine's own database, whether
    /// it reads the project's file and whether it reaches the loopback listener, with the
    /// connect's own words. Output: the shell script.
    fn probe(&self) -> String {
        let database = self.engine.path().join("engine.db");
        let port = self.listener.local_addr().unwrap().port();
        format!(
            "if /bin/cat '{}' >/dev/null 2>&1; then echo READ-db; else echo DENIED-db; fi\n\
             if /bin/cat README.md >/dev/null 2>&1; then echo READ-project; else echo DENIED-project; fi\n\
             if /usr/bin/nc -z -v -G 2 127.0.0.1 {port} 2>&1; then echo NET-OPEN; else echo NET-CLOSED; fi\n",
            database.display()
        )
    }

    /// Opens the project's IDE through the owner protocol on the running engine and stops
    /// it again, then reads back what that start wrote. Output: the start's files.
    async fn open_ide(&self) -> Started {
        let client = self.owner.controller.owner_client().unwrap();
        let opened = client.ide_tab_open(self.project.clone()).await.unwrap();
        assert!(opened.url.starts_with("http://127.0.0.1:"), "{opened:?}");
        assert!(client.ide_tab_stop(self.project.clone()).await.unwrap());
        let folder = self.engine.path().join("ide").join(PROJECT);
        let server_data = folder.join(SERVER_DATA);
        let machine = ide::settings::machine_folder(&server_data).join("settings.json");
        let settings = serde_json::from_str(&fs::read_to_string(machine).unwrap()).unwrap();
        let profile = folder.join(ide::sandbox::PROFILE);
        assert!(
            profile.is_file(),
            "the server ran under {}",
            profile.display()
        );
        Started {
            settings,
            server_data,
            profile,
        }
    }
}

/// Runs a command and returns its standard output; a failed start fails the test.
fn output(command: &mut Command) -> String {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(command).unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_terminal_sandboxed_is_the_default_and_runs_the_owner_shell_in_the_agent_sandbox() {
    let case = Terminal::start("ca-v2opt-term-sbx-").await;
    let started = case.open_ide().await;
    assert_eq!(
        started.settings["terminal.integrated.defaultProfile.osx"],
        serde_json::json!(SANDBOXED),
        "a new IDE terminal opens Sandboxed"
    );

    let place = ide_shell::project_place(case.engine.path(), &case.project).unwrap();
    assert_eq!(place.folder, case.folder);
    let shell = ide_shell::owner_shell().unwrap();
    let incoming: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(variable, _)| {
            TERMINAL_VARIABLES
                .iter()
                .any(|known| variable.as_os_str() == *known)
        })
        .collect();
    let arguments = [OsString::from("-c"), OsString::from(case.probe())];
    let mut command = ide_shell::command(&Launch {
        root: case.engine.path(),
        project: &case.project,
        folder: &place.folder,
        cwd: &place.folder,
        shell: &shell,
        arguments: &arguments,
        incoming: &incoming,
        extra_reads: &place.extra_reads,
    })
    .unwrap();
    assert_eq!(command.get_program(), crazyagent::sandbox::SANDBOX_EXEC);
    let seen = output(&mut command);
    assert!(seen.contains("DENIED-db\n"), "{seen}");
    assert!(seen.contains("READ-project\n"), "{seen}");
    assert!(
        seen.contains("NET-OPEN\n"),
        "the Sandboxed terminal has the agent sandbox's open network: {seen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_terminal_ide_shell_runs_the_owner_shell_inside_the_ide_sandbox_with_no_network() {
    let case = Terminal::start("ca-v2opt-term-ide-").await;
    let started = case.open_ide().await;
    let shell = ide_shell::owner_shell().unwrap();
    let profile = &started.settings["terminal.integrated.profiles.osx"][IDE_SHELL];
    assert_eq!(profile["path"], serde_json::json!(shell.to_str().unwrap()));
    assert_eq!(
        &started.settings["terminal.integrated.automationProfile.osx"], profile,
        "tasks run in the IDE shell too"
    );
    let arguments: Vec<String> = profile["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(arguments, ["-l"]);

    let tmp = crazyagent::sandbox::toolchains::existing_tmp(&started.server_data)
        .expect("the IDE start made the server's short temp folder");
    let mut command = ide::sandbox::command(&started.profile, Path::new(&shell));
    command
        .args(&arguments)
        .arg("-c")
        .arg(case.probe())
        .current_dir(&case.folder)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &started.server_data)
        .env("TMPDIR", &tmp)
        .env("TERM", "xterm-256color");
    let seen = output(&mut command);
    assert!(seen.contains("DENIED-db\n"), "{seen}");
    assert!(seen.contains("READ-project\n"), "{seen}");
    assert!(
        seen.contains("NET-CLOSED\n") && seen.contains("not permitted"),
        "the IDE shell runs inside the IDE server's own sandbox, which has no network: {seen}"
    );
}
