#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
use crazyagent::protocol::ProjectId;
#[cfg(target_os = "macos")]
use crazyagent::sandbox::policy::AgentAccess;
#[cfg(target_os = "macos")]
use crazyagent::sandbox::toolchains::Toolchain;
use crazyagent::terminal::tmux::installed;
use e2e_support::{ENGINE, Engine, Owner, TMUX, git, profile, repository, scratch, stop_tmux};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const COLUMNS: &str = "120";
const ROWS: &str = "40";
const SESSION: &str = "b18";
const SCREEN_TIME: Duration = Duration::from_secs(30);
const POLL_TIME: Duration = Duration::from_millis(100);
const DB_BUSY_TIME: Duration = Duration::from_secs(5);
const STEPS_MAX: usize = 12;
const SHIFT_RIGHT: [&str; 6] = ["1b", "5b", "31", "3b", "32", "43"];
#[cfg(target_os = "macos")]
const RIGHT: [&str; 3] = ["1b", "5b", "43"];
#[cfg(target_os = "macos")]
const DOWN: [&str; 3] = ["1b", "5b", "42"];
const PROJECT: &str = "Tidepool b18";
const TASKS: usize = 40;
const TASK_BYTES: usize = 6000;
const OWNER_GIT: &str = "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n";

struct Pty {
    tmux: PathBuf,
    socket: PathBuf,
    _folder: tempfile::TempDir,
}

impl Pty {
    fn launch(tmux: PathBuf, engine_socket: &Path) -> Self {
        let engine_socket = engine_socket.to_str().unwrap();
        assert!(!ENGINE.contains('\'') && !engine_socket.contains('\''));
        let folder = scratch("ca-b18-pty-");
        let pty = Self {
            tmux,
            socket: folder.path().join("pty.sock"),
            _folder: folder,
        };
        let command = format!("'{ENGINE}' tui '{engine_socket}'");
        pty.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            COLUMNS,
            "-y",
            ROWS,
            &command,
        ]);
        pty
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = crazyagent::spawn_gate::output(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .args(args)
                .env_remove("TMUX")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .expect(TMUX);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION];
        args.extend_from_slice(keys);
        self.run(&args);
    }

    fn bytes(&self, hex: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION, "-H"];
        args.extend_from_slice(hex);
        self.run(&args);
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", SESSION]).stdout)
            .into_owned()
    }

    fn until(&self, what: &str, check: impl Fn(&str) -> bool) -> String {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if check(&screen) {
                return screen;
            }
            assert!(start.elapsed() < SCREEN_TIME, "{what}:\n{screen}");
            std::thread::sleep(POLL_TIME);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.socket);
    }
}

/// Starts a real engine whose HOME is a scratch folder holding the owner's Git name and a
/// Rust toolchain folder, so Accept and the toolchain rows never read the real home, then
/// saves the profile, the projects home and one project with no repository, as New project
/// does. Inputs: the scratch prefix and the project id. Output: the engine, its home and the
/// projects home folder, all removed when dropped.
fn engine_with_project(prefix: &str, id: &str) -> (Engine, tempfile::TempDir, tempfile::TempDir) {
    let home = scratch("ca-b18-home-");
    std::fs::write(home.path().join(".gitconfig"), OWNER_GIT).unwrap();
    std::fs::create_dir_all(home.path().join(".cargo").join("bin")).unwrap();
    let mut engine = Engine::new(prefix);
    engine.home = Some(home.path().to_path_buf());
    engine.launch();
    let disk = scratch("ca-b18-disk-");
    let projects = disk.path().join("Projects");
    std::fs::create_dir(&projects).unwrap();
    owner(&engine, |mut owner| async move {
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&projects).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(id, PROJECT).await,
            OwnerResult::Applied
        );
        owner
    });
    (engine, home, disk)
}

/// Runs one owner conversation with the engine as the TUI client and detaches.
/// Inputs: the engine and the steps. Output: none; a failed step fails the test.
fn owner<F, Fut>(engine: &Engine, steps: F)
where
    F: FnOnce(Owner) -> Fut,
    Fut: std::future::Future<Output = Owner>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = steps(Owner::attach(engine).await).await;
            owner.controller.detach();
        });
}

/// Reads the project's agent policy the engine stores. Inputs: the engine and the project.
/// Output: the policy view.
#[cfg(target_os = "macos")]
fn policy(engine: &Engine, project: &ProjectId) -> crazyagent::sandbox::policy::PolicyView {
    let mut view = None;
    owner(engine, |mut owner| {
        let view = &mut view;
        async move {
            *view = Some(
                owner
                    .controller
                    .project_agent_policy(project.clone())
                    .await
                    .unwrap(),
            );
            owner
        }
    });
    view.unwrap()
}

/// Opens the engine database the way a second engine client does.
/// Input: the engine. Output: the connection.
fn database(engine: &Engine) -> Connection {
    let db = Connection::open(engine.path().join("engine.db")).unwrap();
    db.busy_timeout(DB_BUSY_TIME).unwrap();
    db
}

#[cfg(target_os = "macos")]
fn count(db: &Connection, sql: &str, value: i64) -> i64 {
    db.query_row(sql, [value], |row| row.get(0)).unwrap()
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn reach_home(pty: &Pty) {
    for step in 0..=STEPS_MAX {
        assert!(
            step < STEPS_MAX,
            "the TUI never reached Home:\n{}",
            pty.screen()
        );
        let screen = pty.until("the TUI shows Home or a first-run step", |screen| {
            at_home(screen)
                || screen.contains("Welcome to CrazyAgent")
                || screen.contains("c Continue without")
        });
        if at_home(&screen) {
            return;
        }
        assert!(
            !screen.contains("Welcome to CrazyAgent"),
            "a saved profile skips Welcome:\n{screen}"
        );
        pty.keys(&["c"]);
        pty.until("the first-run step moves on", |now| now != screen);
    }
}

fn at_home(screen: &str) -> bool {
    screen.contains("New project") && screen.contains(PROJECT)
}

fn tab_shown(screen: &str) -> Option<String> {
    let crumb = format!("Home  /  {PROJECT}");
    let line = screen.lines().next()?;
    let rest = &line[line.find(&crumb)? + crumb.len()..];
    Some(match rest.strip_prefix("  /  ") {
        Some(tab) => tab.split("  ").next()?.trim().to_owned(),
        None => "Overview".to_owned(),
    })
}

/// Opens the project from Home and moves to one of its tabs with Shift-Right, as the owner
/// does. Inputs: the TUI and the tab name.
fn open_tab(pty: &Pty, tab: &str) {
    reach_home(pty);
    for _ in 0..STEPS_MAX {
        if pty.screen().contains("Open") {
            break;
        }
        pty.keys(&["Tab"]);
        std::thread::sleep(POLL_TIME);
    }
    pty.until("Home offers to open the project", |screen| {
        screen.contains("Open")
    });
    pty.keys(&["Enter"]);
    pty.until("the project screen opens with its tabs", |screen| {
        screen.contains(PROJECT) && screen.contains("Tabs")
    });
    for step in 0..=STEPS_MAX {
        let screen = pty.screen();
        let shown = tab_shown(&screen);
        if shown.as_deref() == Some(tab) {
            return;
        }
        assert!(step < STEPS_MAX, "no project tab is {tab}:\n{screen}");
        pty.bytes(&SHIFT_RIGHT);
        pty.until("the next project tab shows", |now| tab_shown(now) != shown);
    }
}

fn on_team(screen: &str) -> bool {
    screen.contains(&format!("{PROJECT}  /  Team")) && screen.contains("Master")
}

fn open_team(pty: &Pty) {
    open_tab(pty, "Agents");
    pty.keys(&["t"]);
    pty.until("t on the Agents tab opens the team tree", on_team);
}

/// Build 18 item 1 (team loads): a team whose finished tasks carry long prompts and results,
/// far more than one engine reply frame holds, still opens in the TUI team tree, and its
/// Finished box reads the team.
#[test]
fn b18_the_team_tree_loads_a_team_with_many_long_finished_tasks() {
    let id = "b18-team-load";
    let (mut engine, _home, _disk) = engine_with_project("ca-b18-load-", id);
    let tmux = installed(engine.path()).expect(TMUX);
    let db = database(&engine);
    let now = now_ms();
    let long = "x".repeat(TASK_BYTES);
    for index in 0..TASKS {
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,result,delivered,created_ms,started_ms,finished_ms)
             VALUES (?1,'owner','codex',0,?2,'done',?3,1,?4,?4,?4)",
            params![
                id,
                format!("Task {index} {long}"),
                format!("Result {index} {long}"),
                now
            ],
        )
        .unwrap();
    }
    let stored: i64 = db
        .query_row(
            "SELECT sum(length(prompt)+length(result)) FROM team_tasks WHERE project=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored > 4 * 65_536,
        "the team holds {stored} bytes of tasks"
    );

    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty);
    let screen = pty.until("the team tree shows the master", on_team);
    assert!(!screen.contains("Failed to load the team."), "{screen}");
    assert!(!screen.contains("Not ready yet."), "{screen}");
    pty.keys(&["f"]);
    let screen = pty.until("f reads the team into the Finished box", |screen| {
        screen.contains("No finished agents.") || screen.contains("Could not read finished agents.")
    });
    assert!(screen.contains("Finished (0)"), "{screen}");
    assert!(
        !screen.contains("Could not read finished agents."),
        "{screen}"
    );
    drop(pty);
    drop(db);
    engine.stop();
}

/// Build 18 item 9 (waiting change sets): a sub-agent's commit in its own clone waits in the
/// Changes tab as ready to review, and Accept there lands it on the owner's branch under the
/// owner's Git name.
#[test]
fn b18_a_waiting_agent_change_set_shows_in_changes_and_accept_lands_it() {
    let id = "b18-changes";
    let (mut engine, _home, disk) = engine_with_project("ca-b18-changes-", id);
    let tmux = installed(engine.path()).expect(TMUX);
    let repo = disk.path().join("repos").join("tide-tables");
    repository(&repo, "README.md", "# Tide tables\n");
    let worktree = crazyagent::projects_home::state_folder(&repo)
        .join("worktrees")
        .join("agent-own");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    crazyagent::landing::add_agent_clone(&repo, &worktree).unwrap();
    let git_dir = crazyagent::agent_git::git_dir(&worktree);
    let db = crazyagent::landing::open(engine.path()).unwrap();
    crazyagent::landing::record_worktree_start(&db, &repo, &worktree, &git_dir).unwrap();
    std::fs::write(worktree.join("TIDES.md"), "High tide at 06:12.\n").unwrap();
    git(&worktree, &["add", "TIDES.md"]);
    git(&worktree, &["commit", "-q", "-m", "Add the tide table"]);
    let project = ProjectId::new(id).unwrap();
    let workspace = Workspace {
        project: project.clone(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "7".into(),
            label: "Ada".into(),
        },
        repository: repo.clone(),
        worktree: worktree.clone(),
        git_dir,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "task-7".into(),
        prompt_line: "Add the tide table".into(),
    };
    crazyagent::landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .expect("the agent's commit opens a change set");
    let before = String::from_utf8(git(&repo, &["rev-parse", "main"]).stdout).unwrap();

    let pty = Pty::launch(tmux, &engine.socket());
    open_tab(&pty, "Changes");
    let screen = pty.until("Changes lists the agent's waiting change set", |screen| {
        screen.contains("Ada") && screen.contains("Ready to review")
    });
    assert!(screen.contains("a Accept"), "{screen}");
    pty.keys(&["a"]);
    pty.until("a asks to accept with the commit message", |screen| {
        screen.contains("Accept the changes of") && screen.contains("Commit message")
    });
    pty.keys(&["Enter"]);
    let screen = pty.until("Accept lands the change set", |screen| {
        screen.contains("Accepted as")
    });
    assert!(screen.contains("on main"), "{screen}");

    let after = String::from_utf8(git(&repo, &["rev-parse", "main"]).stdout).unwrap();
    assert_ne!(before, after, "Accept moved the owner's branch");
    let landed = String::from_utf8(git(&repo, &["show", "main:TIDES.md"]).stdout).unwrap();
    assert_eq!(landed, "High tide at 06:12.\n");
    let author =
        String::from_utf8(git(&repo, &["log", "-1", "--format=%an <%ae>", "main"]).stdout).unwrap();
    assert_eq!(author.trim(), "Owner Example <owner@example.invalid>");
    let listed = crazyagent::landing::list(&db, &project).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].state,
        crazyagent::landing::ChangeState::Accepted,
        "{listed:?}"
    );
    drop(pty);
    drop(db);
    engine.stop();
}

/// Build 18 items 10 and 11 (access levels, toolchains): in Settings the owner raises the
/// project's agent access to Own branch with the Rust toolchain allowed, then to Full access,
/// which waits for an explicit y after its warning; n keeps the saved level. The engine stores
/// each saved choice, which is what the team runtime launches agents with.
#[cfg(target_os = "macos")]
#[test]
fn b18_the_owner_changes_agent_access_and_toolchains_in_settings() {
    let id = "b18-access";
    let (mut engine, _home, _disk) = engine_with_project("ca-b18-access-", id);
    let tmux = installed(engine.path()).expect(TMUX);
    let project = ProjectId::new(id).unwrap();
    let start = policy(&engine, &project);
    assert_eq!(start.access, AgentAccess::Sandboxed);
    let rust = start
        .toolchains
        .iter()
        .position(|row| row.toolchain == Toolchain::Rust)
        .expect("the Rust toolchain folder in the owner's home is listed");
    assert!(start.toolchains[rust].installed && !start.toolchains[rust].allowed);

    let pty = Pty::launch(tmux, &engine.socket());
    open_tab(&pty, "Settings");
    pty.until("Settings shows the agent access", |screen| {
        screen.contains("Sandboxed") && screen.contains("c Agent access")
    });
    pty.keys(&["c"]);
    let screen = pty.until("c opens the whole Agent access box", |screen| {
        screen.contains("< Sandboxed >")
            && screen.contains("Toolchains agents may use")
            && screen.contains("[> Save ]")
    });
    assert!(screen.contains("[ ] Rust"), "{screen}");
    assert!(screen.contains("[> Save ]"), "{screen}");
    pty.bytes(&RIGHT);
    pty.until("Right picks Own branch", |screen| {
        screen.contains("< Own branch >")
    });
    for _ in 0..=rust {
        pty.bytes(&DOWN);
    }
    pty.keys(&["Space"]);
    pty.until("Space allows Rust", |screen| screen.contains("[x] Rust"));
    pty.keys(&["Enter"]);
    pty.until("the saved level shows in Settings", |screen| {
        !screen.contains("Toolchains agents may use") && screen.contains("Own branch")
    });
    let saved = policy(&engine, &project);
    assert_eq!(saved.access, AgentAccess::OwnBranch);
    assert!(saved.toolchains[rust].allowed, "{saved:?}");

    pty.keys(&["c"]);
    pty.until("c opens the box at the saved level", |screen| {
        screen.contains("< Own branch >") && screen.contains("[x] Rust")
    });
    pty.bytes(&RIGHT);
    pty.until("Right picks Full access", |screen| {
        screen.contains("< Full access >")
    });
    pty.keys(&["Enter"]);
    pty.until("Full access warns before it saves", |screen| {
        screen.contains("Full access: Agents run with your user's permissions")
            && screen.contains("[> y Allow Full access ]")
    });
    pty.keys(&["n"]);
    pty.until("n keeps the box open without saving", |screen| {
        screen.contains("[> Save ]") && !screen.contains("Allow Full access")
    });
    assert_eq!(policy(&engine, &project).access, AgentAccess::OwnBranch);
    pty.keys(&["Enter"]);
    pty.until("Full access warns again", |screen| {
        screen.contains("[> y Allow Full access ]")
    });
    pty.keys(&["y"]);
    pty.until("the Full access level shows in Settings", |screen| {
        !screen.contains("Toolchains agents may use") && screen.contains("Full access")
    });
    let full = policy(&engine, &project);
    assert_eq!(full.access, AgentAccess::Full);
    assert!(full.toolchains[rust].allowed, "{full:?}");
    drop(pty);
    engine.stop();
}

/// Build 18 items 12 and 13 (finished agents, wake): a stopped sub-agent whose worktree is
/// kept is listed in the team tree's Finished box with its last task; h switches the
/// project's Finished agents setting and w asks the engine to wake the agent.
#[cfg(target_os = "macos")]
#[test]
fn b18_the_owner_sees_switches_and_wakes_a_finished_agent() {
    let id = "b18-finished";
    let (mut engine, _home, disk) = engine_with_project("ca-b18-finished-", id);
    let tmux = installed(engine.path()).expect(TMUX);
    let repo = disk.path().join("repos").join("buoy-alerts");
    repository(&repo, "README.md", "# Buoy alerts\n");
    let worktree = disk.path().join("agent-worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/tidy",
            worktree.to_str().unwrap(),
        ],
    );
    let git_dir = repo.join(".git").join("worktrees").join("agent-worktree");
    assert!(git_dir.is_dir(), "{git_dir:?}");
    let db = database(&engine);
    let now = now_ms();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,worktree,status,created_ms,heartbeat_ms,stopped_ms)
         VALUES (?1,'sub','codex',0,?2,'stopped',?3,?3,?3)",
        params![id, worktree.to_str().unwrap(), now],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
        params![
            agent,
            id,
            repo.to_str().unwrap(),
            worktree.to_str().unwrap(),
            git_dir.to_str().unwrap()
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,delivered,created_ms,started_ms,finished_ms)
         VALUES (?1,'owner','codex',0,'Tidy the alert README','done',?2,'Tidied.',1,?3,?3,?3)",
        params![id, agent, now],
    )
    .unwrap();
    let label = format!("Sub-agent {agent}");

    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty);
    pty.keys(&["f"]);
    let screen = pty.until("f lists the finished agent", |screen| {
        screen.contains("Finished (1)") && screen.contains(&label)
    });
    assert!(
        screen.contains("Last task: Tidy the alert README"),
        "{screen}"
    );
    assert!(
        screen.contains("Finished agents: Hide. They leave the team tree for this list."),
        "{screen}"
    );
    pty.keys(&["h"]);
    pty.until("h keeps finished agents in the team tree", |screen| {
        screen.contains("Finished agents now stay in the team tree.")
    });
    let shown: i64 = db
        .query_row(
            "SELECT show FROM team_finished_view WHERE project=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shown, 1);
    pty.keys(&["w"]);
    let screen = pty.until("w asks the engine to wake the agent", |screen| {
        screen.contains(&format!("Waking {label}.")) || screen.contains("Could not wake")
    });
    assert!(
        screen.contains("It comes back in the team tree."),
        "{screen}"
    );
    let stored = count(
        &db,
        "SELECT count(*) FROM team_wake_requests WHERE agent=?1",
        agent,
    );
    assert_eq!(
        stored, 1,
        "w stores one wake request for the team runtime's next tick"
    );
    let stopped = count(
        &db,
        "SELECT count(*) FROM team_agents WHERE id=?1 AND status='stopped'",
        agent,
    );
    assert_eq!(
        stopped, 1,
        "this engine runs no team runtime, so the agent stays stopped until one takes the wake"
    );
    drop(pty);
    drop(db);
    engine.stop();
}
