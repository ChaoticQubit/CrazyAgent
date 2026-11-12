#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::harnesses::{HarnessEntry, HarnessStatus};
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::project_setup::{ProjectSetup, TeamPlan};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::setup::{Dependency, DependencyState, SetupStatus};
use crazyagent::team::{AgentRole, TeamAction, TeamStatus};
use crazyagent::terminal::{AgentId, TerminalFrame};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ENGINE: &str = env!("CARGO_BIN_EXE_crazyagent");
const PROJECT: &str = "clean-mac";
const NODE_VERSION: &str = "v22.11.0";
const CODEX_VERSION: &str = "codex-cli 0.167.1";
const MARKER_FILE: &str = "clean-mac-marker";
const MARKER_LINE: &[u8] = b"clean-mac stand-in ran";
const FINDER_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const MAPPED_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];
const SYSTEM_ROOT_VARIABLE: &str = "CRAZYAGENT_GATE_SYSTEM_ROOT";
const GATE_DIR_VARIABLE: &str = "CRAZYAGENT_CLEAN_MAC_DIR";
const HOST_TMUX: [&str; 2] = ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"];
const REFUSED: &str = "gate refused";
const RESTARTS_ONLY: &str = "had to start";
const READY_LINE: &[u8] = b"ready\n";
const READY_TIME: Duration = Duration::from_secs(30);
const SETTLE_TIME: Duration = Duration::from_secs(30);
const LIVE_TIME: Duration = Duration::from_secs(90);
const PANE_TIME: Duration = Duration::from_secs(5);
const STOP_TIME: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(250);
const MARKER_DEPTH_MAX: usize = 14;
const MARKER_ENTRIES_MAX: usize = 50_000;

const CODEX_SCRIPT: &str = "#!/usr/bin/env node\nimport(\"../dist/cli.js\");\n";

/// The four fresh-Mac shapes the gate starts the engine on: nothing installed, tmux, node
/// and an npm Codex under Homebrew, node and npm Codex under nvm with tmux from Homebrew,
/// and the Homebrew shape with tmux installed only after the engine started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Nothing,
    Homebrew,
    Nvm,
    TmuxLater,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Nothing => "a-nothing-installed",
            Self::Homebrew => "b-homebrew-npm-codex",
            Self::Nvm => "c-nvm-npm-codex",
            Self::TmuxLater => "d-tmux-after-engine-start",
        }
    }
}

fn gate_dir_set() -> Option<PathBuf> {
    std::env::var_os(GATE_DIR_VARIABLE)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// The gate's output folder, from `CRAZYAGENT_CLEAN_MAC_DIR` as `scripts/gate/clean-mac.sh`
/// sets it. Output: the absolute folder; panics when the gate was not started by its script.
fn gate_dir() -> PathBuf {
    let dir = gate_dir_set()
        .unwrap_or_else(|| panic!("{REFUSED}: run it through scripts/gate/clean-mac.sh"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// One summary line per shape in `<gate dir>/summary.txt`: PASS once the shape's checks
/// all held, else FAIL with the last step reached, written even when a check panics, each
/// followed by what every live master start came to.
struct Report {
    shape: Shape,
    step: String,
    passed: bool,
    seen: Vec<String>,
}

impl Report {
    fn new(shape: Shape) -> Self {
        Self {
            shape,
            step: "start".into(),
            passed: false,
            seen: Vec::new(),
        }
    }

    fn saw(&mut self, outcome: &Live) {
        self.seen.push(format!("{outcome:?}"));
    }

    fn step(&mut self, step: &str) {
        self.step = step.to_owned();
        eprintln!("[{}] {step}", self.shape.name());
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        let verdict = if self.passed {
            "PASS".to_owned()
        } else {
            format!("FAIL during {}", self.step)
        };
        let line = if self.seen.is_empty() {
            format!("{}: {verdict}\n", self.shape.name())
        } else {
            format!(
                "{}: {verdict}, live starts: {}\n",
                self.shape.name(),
                self.seen.join("; ")
            )
        };
        let Some(dir) = gate_dir_set() else {
            return;
        };
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("summary.txt"))
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// The host's own tmux, which the tmux stand-in runs, since no script can play a terminal
/// server. Output: its real path; panics when this Mac has none.
fn host_tmux() -> PathBuf {
    HOST_TMUX
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(|path| path.canonicalize().unwrap())
        .unwrap_or_else(|| panic!("{REFUSED}: the tmux stand-in needs a tmux on this Mac"))
}

fn write_program(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn link(target: &str, at: &Path) {
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// The node stand-in: answers `--version` for itself and for the Codex script it is
/// handed, else leaves a marker naming the node and script it ran as, in the first of the
/// agent's home, working or temp folder it may write (the first two sit inside the gate's
/// private Mac, the agent's temp folder may not), says so on its pane and stays open.
fn node_text() -> String {
    format!(
        "#!/bin/sh\n\
         for argument in \"$@\"; do\n\
         \x20 if [ \"$argument\" = \"--version\" ]; then\n\
         \x20   case \"$1\" in\n\
         \x20     *codex*) printf '%s\\n' '{CODEX_VERSION}' ;;\n\
         \x20     *) printf '%s\\n' '{NODE_VERSION}' ;;\n\
         \x20   esac\n\
         \x20   exit 0\n\
         \x20 fi\n\
         done\n\
         for folder in \"$HOME\" \"$PWD\" \"$TMPDIR\"; do\n\
         \x20 if [ -n \"$folder\" ] && printf 'node=%s\\nscript=%s\\n' \"$0\" \"$1\" > \"$folder/{MARKER_FILE}\" 2>/dev/null; then\n\
         \x20   break\n\
         \x20 fi\n\
         done\n\
         printf '%s\\n' '{marker}'\n\
         exec /bin/cat\n",
        marker = String::from_utf8_lossy(MARKER_LINE),
    )
}

/// A private fresh Mac for one shape: a new HOME, the system root the engine's
/// `/opt/homebrew` and `/usr/local` folders are mapped into, the engine root, and the
/// gate folder its logs are kept in. Dropping it stops the engine and its tmux server.
struct Mac {
    shape: Shape,
    _temp: tempfile::TempDir,
    run: PathBuf,
    home: PathBuf,
    system: PathBuf,
    root: PathBuf,
    kept: PathBuf,
    tmux: PathBuf,
    engine: Option<Child>,
}

impl Mac {
    fn new(shape: Shape) -> Self {
        assert!(
            cfg!(feature = "clean-mac-gate"),
            "{REFUSED}: this build has no system-root remap, so the engine would see this Mac's real Homebrew; run scripts/gate/clean-mac.sh, which builds with --features clean-mac-gate"
        );
        let temp = tempfile::Builder::new()
            .prefix("cm-")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let run = temp.path().canonicalize().unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(entry) = crazyagent::service::changeable_by_others(&run) {
            panic!(
                "{REFUSED}: other users can change {}, so the engine passes over every stand-in in {}; set TMPDIR to a folder only you can change",
                entry.display(),
                run.display()
            );
        }
        let home = run.join("home");
        let system = run.join("system");
        let root = run.join("engine");
        for folder in [
            home.join("tmp"),
            home.join("Projects"),
            system.join("opt/homebrew/bin"),
            system.join("usr/local/bin"),
            root.clone(),
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        for private in [&home, &root] {
            fs::set_permissions(private, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let kept = gate_dir().join(shape.name());
        let _ = fs::remove_dir_all(&kept);
        fs::create_dir_all(&kept).unwrap();
        let mac = Self {
            shape,
            _temp: temp,
            run,
            home,
            system,
            root,
            kept,
            tmux: host_tmux(),
            engine: None,
        };
        mac.write_settings();
        mac
    }

    fn brew(&self) -> PathBuf {
        self.system.join("opt/homebrew")
    }

    fn nvm(&self) -> PathBuf {
        self.home.join(".nvm/versions/node").join(NODE_VERSION)
    }

    fn write_settings(&self) {
        let global = self.root.join("global.toml");
        fs::write(
            &global,
            format!(
                "version = 1\n\
                 timeout_ms = 5000\n\
                 allowed_destinations = []\n\
                 [model_access]\n\
                 accounts = [\"gate-account\"]\n\
                 [[model_access.connections]]\n\
                 id = \"gate-codex\"\n\
                 provider = \"openai-compatible\"\n\
                 harness = \"codex\"\n\
                 protocol = \"openai-chat\"\n\
                 account_ref = \"gate-account\"\n\
                 endpoint = {{ kind = \"http\", url = \"http://127.0.0.1:9/v1\" }}\n\
                 model = {{ id = \"gate-model\", author = \"openai-compatible\" }}\n\
                 auth = {{ method = \"none\" }}\n\
                 [[model_access.pools]]\n\
                 id = \"gate-pool\"\n\
                 members = [\"gate-codex\"]\n\
                 projects = [\"{PROJECT}\"]\n\
                 roles = []\n\
                 fallback_allowed = false\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn install_brew_node(&self) {
        let cellar = self.brew().join("Cellar/node/22.11.0/bin/node");
        write_program(&cellar, &node_text());
        link(
            "../Cellar/node/22.11.0/bin/node",
            &self.brew().join("bin/node"),
        );
    }

    fn install_npm_codex(&self, prefix: &Path) {
        write_program(
            &prefix.join("lib/node_modules/@openai/codex/bin/codex.js"),
            CODEX_SCRIPT,
        );
        link(
            "../lib/node_modules/@openai/codex/bin/codex.js",
            &prefix.join("bin/codex"),
        );
    }

    fn install_brew_tmux(&self) {
        let real = self.tmux.to_str().unwrap();
        assert!(!real.contains('\''), "{real}");
        write_program(
            &self.brew().join("Cellar/tmux/3.8/bin/tmux"),
            &format!("#!/bin/sh\nexec '{real}' \"$@\"\n"),
        );
        link("../Cellar/tmux/3.8/bin/tmux", &self.brew().join("bin/tmux"));
    }

    fn install_nvm_node(&self) {
        write_program(&self.nvm().join("bin/node"), &node_text());
        self.install_npm_codex(&self.nvm());
        fs::create_dir_all(self.home.join(".nvm/alias")).unwrap();
        fs::write(
            self.home.join(".nvm/alias/default"),
            format!("{NODE_VERSION}\n"),
        )
        .unwrap();
        fs::write(self.home.join(".nvm/nvm.sh"), "").unwrap();
    }

    fn mapped(&self, folder: &Path) -> PathBuf {
        MAPPED_PREFIXES
            .iter()
            .find_map(|prefix| folder.strip_prefix(prefix).ok().map(|rest| (prefix, rest)))
            .map_or_else(
                || folder.to_path_buf(),
                |(prefix, rest)| self.system.join(prefix.trim_start_matches('/')).join(rest),
            )
    }

    /// The environment launchd gives the login service the app installs: the plist's PATH,
    /// built by the app from a Finder-started PATH, with `/opt/homebrew` and `/usr/local`
    /// mapped into the system root, and the user basics launchd adds. Nothing else is
    /// inherited. Output: the variables.
    fn environment(&self) -> Vec<(OsString, OsString)> {
        let path = crazyagent::service::service_path(
            Some(OsStr::new(FINDER_PATH)),
            Some(&self.home),
            None,
        );
        let mapped: Vec<PathBuf> = std::env::split_paths(&path)
            .map(|folder| self.mapped(&folder))
            .collect();
        let user = nix::unistd::User::from_uid(nix::unistd::getuid())
            .unwrap()
            .unwrap()
            .name;
        vec![
            ("HOME".into(), self.home.clone().into()),
            ("USER".into(), user.clone().into()),
            ("LOGNAME".into(), user.into()),
            ("SHELL".into(), "/bin/zsh".into()),
            ("TMPDIR".into(), self.home.join("tmp").into()),
            ("PATH".into(), std::env::join_paths(mapped).unwrap()),
            (SYSTEM_ROOT_VARIABLE.into(), self.system.clone().into()),
        ]
    }

    fn launch(&mut self) {
        assert!(self.engine.is_none());
        let stdout = fs::File::create(self.kept.join("engine-stdout.log")).unwrap();
        let stderr = fs::File::create(self.kept.join("engine-stderr.log")).unwrap();
        let mut command = Command::new(ENGINE);
        command
            .env_clear()
            .envs(self.environment())
            .arg("engine")
            .arg("--home")
            .arg(&self.root)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        self.engine = Some(crazyagent::spawn_gate::spawn(&mut command).unwrap());
        let start = Instant::now();
        while fs::read(self.kept.join("engine-stdout.log")).unwrap() != READY_LINE {
            if let Some(status) = self.engine.as_mut().unwrap().try_wait().unwrap() {
                panic!(
                    "the engine exited ({status}) before it was ready; see {}",
                    self.kept.display()
                );
            }
            assert!(
                start.elapsed() < READY_TIME,
                "the engine was not ready within 30 s; see {}",
                self.kept.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn engine_pid(&self) -> u32 {
        self.engine.as_ref().expect("the engine runs").id()
    }

    fn stop(&mut self) {
        let Some(mut child) = self.engine.take() else {
            return;
        };
        let _ = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &child.id().to_string(),
        ]));
        let deadline = Instant::now() + STOP_TIME;
        while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Mac {
    fn drop(&mut self) {
        self.stop();
        let socket = self
            .root
            .join(crazyagent::terminal::FOLDER)
            .join(crazyagent::terminal::tmux::SOCKET);
        let _ = crazyagent::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        let _ = fs::copy(
            self.root.join("logs/engine.log"),
            self.kept.join("engine.log"),
        );
    }
}

/// The owner's connection to the gate's engine, as the desktop app holds it.
struct Owner {
    controller: OwnerController,
    generation: u64,
}

fn applied(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } => operation.result,
        other => panic!("the owner change was not recorded: {other:?}"),
    }
}

impl Owner {
    async fn attach(mac: &Mac) -> Self {
        Self {
            controller: OwnerController::attach(
                &mac.root,
                OwnerClientKind::Desktop,
                Duration::from_secs(10),
            )
            .await
            .unwrap(),
            generation: 0,
        }
    }

    fn next(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    async fn first_project(&mut self, mac: &Mac) {
        let generation = self.next();
        let profile = OwnerProfile {
            name: "Gate Owner".into(),
            role: "Release gate".into(),
            bio: "Checks a fresh Mac.".into(),
            preferred_stack: vec!["Rust".into()],
            working_style: Default::default(),
        };
        let saved = self
            .controller
            .save_owner_profile(profile, generation)
            .await
            .unwrap();
        assert_eq!(applied(saved), OwnerResult::Applied);
        let generation = self.next();
        let home = mac.home.join("Projects");
        let set = self
            .controller
            .set_projects_home(home.to_str().unwrap().into(), generation)
            .await
            .unwrap();
        assert_eq!(applied(set), OwnerResult::Applied);
        let generation = self.next();
        let made = self
            .controller
            .setup_project(
                ProjectId::new(PROJECT).unwrap(),
                ProjectSpec {
                    name: "Clean Mac".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "A fresh Mac check.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: Some(TeamPlan {
                        master: "codex".into(),
                        master_tools: false,
                        sub_harnesses: vec![],
                        min_agents: 1,
                        max_agents: 1,
                    }),
                    documents: vec![],
                },
                generation,
            )
            .await
            .unwrap();
        assert_eq!(applied(made), OwnerResult::Applied);
    }

    async fn setup(&mut self) -> SetupStatus {
        let start = Instant::now();
        loop {
            let status = self.controller.setup_status().await.unwrap();
            if status
                .dependencies
                .iter()
                .all(|dependency| dependency.state != DependencyState::Checking)
            {
                return status;
            }
            assert!(
                start.elapsed() < SETTLE_TIME,
                "tools still checking: {status:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    async fn codex(&self) -> HarnessEntry {
        self.controller
            .harness_catalog(true)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == "codex")
            .expect("the catalog lists Codex")
    }

    async fn team(&mut self) -> TeamStatus {
        self.controller
            .team_status(ProjectId::new(PROJECT).unwrap())
            .await
            .unwrap()
    }

    /// Asks for the live master. Output: none when the engine took the change, else the
    /// code it refused it with.
    async fn live_on(&mut self) -> Option<String> {
        let version = self.team().await.settings_version;
        let generation = self.next();
        let response = self
            .controller
            .team_action(
                TeamAction::LiveMaster {
                    project_id: ProjectId::new(PROJECT).unwrap(),
                    on: true,
                    version,
                },
                generation,
            )
            .await
            .unwrap();
        match applied(response) {
            OwnerResult::Applied => None,
            OwnerResult::Rejected { code } => Some(code),
            other => panic!("the live master start came back as {other:?}"),
        }
    }
}

fn tmux_state(setup: &SetupStatus) -> &DependencyState {
    &setup
        .dependencies
        .iter()
        .find(|dependency| dependency.name == Dependency::Tmux)
        .expect("setup lists tmux")
        .state
}

fn found(state: &DependencyState) -> bool {
    matches!(
        state,
        DependencyState::Found { .. } | DependencyState::Installed { .. }
    )
}

/// Fails the shape before any live start when setup or the harness catalog sees a tool
/// outside this shape's private Mac: with the remap on, only a search that skips the one
/// tool resolver looks in this Mac's real Homebrew, and a live start could then run the
/// host's real Codex. Inputs: the shape's Mac, its setup list and Codex catalog row.
fn stop_on_host_tools(mac: &Mac, setup: &SetupStatus, codex: &HarnessEntry) {
    if let Some(executable) = &codex.executable {
        assert!(
            executable.starts_with(&mac.run),
            "the harness catalog found Codex at {} outside the gate's Mac, so it does not search through the one tool resolver; no live start was tried",
            executable.display()
        );
    }
    let tmux_planted = mac.brew().join("bin/tmux").exists();
    assert!(
        tmux_planted || !found(tmux_state(setup)),
        "setup found a tmux the gate never planted, so its tool list does not search through the one tool resolver the launch path uses: {setup:?}"
    );
}

/// What one live master start came to: the stand-in harness ran (with the node and script
/// it ran as, when its marker file was readable), the start stopped with a problem line, or
/// it was still waiting when the gate's time ran out, with the last line shown.
#[derive(Debug)]
enum Live {
    Ran {
        node: Option<String>,
        script: Option<String>,
    },
    Stopped(String),
    Waiting(Option<String>),
}

fn marker(folder: &Path, depth: usize, seen: &mut usize) -> Option<String> {
    if depth > MARKER_DEPTH_MAX || *seen > MARKER_ENTRIES_MAX {
        return None;
    }
    for entry in fs::read_dir(folder).ok()?.flatten() {
        *seen += 1;
        let kind = entry.file_type().ok()?;
        if kind.is_file() && entry.file_name() == MARKER_FILE {
            return fs::read_to_string(entry.path()).ok();
        }
        if kind.is_dir()
            && let Some(text) = marker(&entry.path(), depth + 1, seen)
        {
            return Some(text);
        }
    }
    None
}

fn marker_field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_owned)
}

async fn pane_says_ran(owner: &mut Owner, team: &TeamStatus) -> bool {
    let Some(master) = team
        .agents
        .iter()
        .find(|agent| agent.role == AgentRole::Master && agent.stopped_ms.is_none())
    else {
        return false;
    };
    let Ok(agent) = AgentId::team(master.id) else {
        return false;
    };
    let Ok(mut session) = owner.controller.terminal_attach(agent).await else {
        return false;
    };
    let mut seen = Vec::new();
    tokio::time::timeout(PANE_TIME, async {
        while !seen
            .windows(MARKER_LINE.len())
            .any(|window| window == MARKER_LINE)
        {
            match session.output.next().await {
                Ok(TerminalFrame::Output { bytes }) => seen.extend_from_slice(&bytes),
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false)
}

async fn live_outcome(owner: &mut Owner, mac: &Mac, refused: Option<String>) -> Live {
    if let Some(code) = refused {
        return Live::Stopped(owner.team().await.live_problem.unwrap_or(code));
    }
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < LIVE_TIME {
        if let Some(text) = marker(&mac.run, 0, &mut 0) {
            return Live::Ran {
                node: marker_field(&text, "node"),
                script: marker_field(&text, "script"),
            };
        }
        let team = owner.team().await;
        if team.live_master == Some(false)
            && let Some(problem) = &team.live_problem
        {
            return Live::Stopped(problem.clone());
        }
        if pane_says_ran(owner, &team).await {
            return Live::Ran {
                node: None,
                script: None,
            };
        }
        last.clone_from(&team.live_problem);
        tokio::time::sleep(POLL).await;
    }
    Live::Waiting(last)
}

/// Whether a stopped start says which tool is missing in the resolver's typed line: the
/// exact tmux line, or a line that starts with the missing tool's name, never one that only
/// says the master had to start again. Inputs: the line and the tools it may name.
/// Output: true for a typed reason.
fn typed_reason(line: &str, tools: &[&str]) -> bool {
    let tmux = tools.contains(&"tmux") && line == crazyagent::tools::TMUX_MISSING;
    let named = tools
        .iter()
        .filter(|tool| **tool != "tmux")
        .any(|tool| line.starts_with(&format!("{tool} ")));
    (tmux || named) && !line.contains(RESTARTS_ONLY)
}

fn real(path: &str) -> PathBuf {
    Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path))
}

fn assert_ran_with(outcome: &Live, node: &Path, script: &Path) {
    match outcome {
        Live::Ran {
            node: Some(ran_node),
            script: Some(ran_script),
        } => {
            assert_eq!(real(ran_node), node.canonicalize().unwrap(), "{outcome:?}");
            assert_eq!(
                real(ran_script),
                script.canonicalize().unwrap(),
                "{outcome:?}"
            );
        }
        Live::Ran { .. } => {}
        other => panic!("the live master never ran the stand-in Codex: {other:?}"),
    }
}

async fn ready_shape(mac: &mut Mac, report: &mut Report) -> (Owner, SetupStatus, HarnessEntry) {
    report.step("engine start through the login service launch");
    mac.launch();
    let mut owner = Owner::attach(mac).await;
    report.step("first run and project");
    owner.first_project(mac).await;
    report.step("setup tool list and harness catalog");
    let setup = owner.setup().await;
    let codex = owner.codex().await;
    stop_on_host_tools(mac, &setup, &codex);
    (owner, setup, codex)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "clean-Mac release gate: run by scripts/gate/clean-mac.sh with --features clean-mac-gate"]
async fn a_nothing_installed_mac_says_what_is_missing_and_never_waits_forever() {
    let mut report = Report::new(Shape::Nothing);
    let mut mac = Mac::new(Shape::Nothing);
    let (mut owner, setup, codex) = ready_shape(&mut mac, &mut report).await;
    assert!(!found(tmux_state(&setup)), "{setup:?}");
    assert_eq!(codex.status, HarnessStatus::NotInstalled, "{codex:?}");
    report.step("live master start");
    let refused = owner.live_on().await;
    let outcome = live_outcome(&mut owner, &mac, refused).await;
    report.saw(&outcome);
    match outcome {
        Live::Stopped(line) => assert!(
            typed_reason(&line, &["tmux", "codex"]),
            "the stop line is not a typed missing-tool line: {line}"
        ),
        other => panic!("a Mac with nothing installed must stop with a typed reason: {other:?}"),
    }
    report.passed = true;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "clean-Mac release gate: run by scripts/gate/clean-mac.sh with --features clean-mac-gate"]
async fn a_homebrew_mac_with_npm_codex_runs_the_live_master_through_the_found_node() {
    let mut report = Report::new(Shape::Homebrew);
    let mut mac = Mac::new(Shape::Homebrew);
    mac.install_brew_tmux();
    mac.install_brew_node();
    mac.install_npm_codex(&mac.brew());
    let (mut owner, setup, codex) = ready_shape(&mut mac, &mut report).await;
    assert!(found(tmux_state(&setup)), "{setup:?}");
    assert_eq!(codex.status, HarnessStatus::Ready, "{codex:?}");
    let script = mac
        .brew()
        .join("lib/node_modules/@openai/codex/bin/codex.js");
    assert_eq!(
        codex
            .executable
            .as_deref()
            .map(|path| real(path.to_str().unwrap())),
        Some(script.canonicalize().unwrap()),
        "{codex:?}"
    );
    report.step("live master start");
    let refused = owner.live_on().await;
    let outcome = live_outcome(&mut owner, &mac, refused).await;
    report.saw(&outcome);
    assert_ran_with(
        &outcome,
        &mac.brew().join("Cellar/node/22.11.0/bin/node"),
        &script,
    );
    report.passed = true;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "clean-Mac release gate: run by scripts/gate/clean-mac.sh with --features clean-mac-gate"]
async fn an_nvm_mac_with_npm_codex_runs_the_live_master_through_the_nvm_node() {
    let mut report = Report::new(Shape::Nvm);
    let mut mac = Mac::new(Shape::Nvm);
    mac.install_brew_tmux();
    mac.install_nvm_node();
    let (mut owner, setup, codex) = ready_shape(&mut mac, &mut report).await;
    assert!(found(tmux_state(&setup)), "{setup:?}");
    assert_eq!(codex.status, HarnessStatus::Ready, "{codex:?}");
    let script = mac
        .nvm()
        .join("lib/node_modules/@openai/codex/bin/codex.js");
    report.step("live master start");
    let refused = owner.live_on().await;
    let outcome = live_outcome(&mut owner, &mac, refused).await;
    report.saw(&outcome);
    assert_ran_with(&outcome, &mac.nvm().join("bin/node"), &script);
    report.passed = true;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "clean-Mac release gate: run by scripts/gate/clean-mac.sh with --features clean-mac-gate"]
async fn tmux_installed_after_the_engine_started_is_used_without_an_engine_restart() {
    let mut report = Report::new(Shape::TmuxLater);
    let mut mac = Mac::new(Shape::TmuxLater);
    mac.install_brew_node();
    mac.install_npm_codex(&mac.brew());
    let (mut owner, setup, codex) = ready_shape(&mut mac, &mut report).await;
    let pid = mac.engine_pid();
    assert!(!found(tmux_state(&setup)), "{setup:?}");
    assert_eq!(codex.status, HarnessStatus::Ready, "{codex:?}");
    report.step("live master start before tmux");
    let refused = owner.live_on().await;
    let outcome = live_outcome(&mut owner, &mac, refused).await;
    report.saw(&outcome);
    match outcome {
        Live::Stopped(line) => assert!(
            typed_reason(&line, &["tmux"]),
            "the stop line is not the typed tmux line: {line}"
        ),
        other => panic!("a start before tmux is installed must say tmux is missing: {other:?}"),
    }
    report.step("tmux installed, setup list again");
    mac.install_brew_tmux();
    let start = Instant::now();
    while !found(tmux_state(&owner.setup().await)) {
        assert!(
            start.elapsed() < SETTLE_TIME,
            "setup still says tmux is missing after it was installed"
        );
        tokio::time::sleep(POLL).await;
    }
    report.step("live master start after tmux");
    let refused = owner.live_on().await;
    let outcome = live_outcome(&mut owner, &mac, refused).await;
    report.saw(&outcome);
    assert_ran_with(
        &outcome,
        &mac.brew().join("Cellar/node/22.11.0/bin/node"),
        &mac.brew()
            .join("lib/node_modules/@openai/codex/bin/codex.js"),
    );
    assert_eq!(mac.engine_pid(), pid, "the engine was restarted");
    assert!(mac.engine.as_mut().unwrap().try_wait().unwrap().is_none());
    report.passed = true;
}

fn plist_strings(text: &str, key: &str, close: &str) -> Vec<String> {
    let start = text
        .find(&format!("<key>{key}</key>"))
        .unwrap_or_else(|| panic!("no {key} in {text}"));
    let block = &text[start..];
    let block = &block[..block.find(close).unwrap()];
    block
        .split("<string>")
        .skip(1)
        .map(|part| part[..part.find("</string>").unwrap()].to_owned())
        .collect()
}

fn plist_keys(text: &str, key: &str) -> Vec<String> {
    let start = text.find(&format!("<key>{key}</key>")).unwrap();
    let block = &text[start + key.len() + 11..];
    let block = &block[..block.find("</dict>").unwrap()];
    block
        .split("<key>")
        .skip(1)
        .map(|part| part[..part.find("</key>").unwrap()].to_owned())
        .collect()
}

/// The gate starts the engine as the app's login service does: the same arguments and only
/// the PATH the service definition sets, plus what launchd itself adds, so a change to the
/// service definition fails here until the gate follows it.
#[test]
fn the_gate_starts_the_engine_with_the_login_service_arguments_and_environment() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let layout = crazyagent::paths::Layout::new(root.clone()).unwrap();
    let service = crazyagent::service::Service::current_user(
        crazyagent::paths::Platform::MacOs,
        PathBuf::from(ENGINE),
        &layout,
    )
    .unwrap();
    let text = service.definition_text().unwrap();
    assert_eq!(
        plist_strings(&text, "ProgramArguments", "</array>"),
        [
            ENGINE.to_owned(),
            "engine".into(),
            "--home".into(),
            root.to_str().unwrap().to_owned()
        ]
    );
    assert_eq!(plist_keys(&text, "EnvironmentVariables"), ["PATH"]);
}
