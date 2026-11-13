#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::{OwnerData, OwnerRead, OwnerResult};
use crazyagent::master_session::{MasterReason, MasterTurnPhase, MasterTurnView};
use crazyagent::protocol::ProjectId;
use crazyagent::setup::{Dependency, DependencyState, PackState, SetupStatus};
use crazyagent::terminal::tmux::installed;
use e2e_support::{ENGINE, Engine, Owner, TMUX, hex, profile, repository, scratch, stop_tmux};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLUMNS: &str = "120";
const ROWS: &str = "40";
const SESSION: &str = "e2e";
const SCREEN_TIME: Duration = Duration::from_secs(30);
const POLL_TIME: Duration = Duration::from_millis(100);
const STEPS_MAX: usize = 12;
const SHIFT_RIGHT: [&str; 6] = ["1b", "5b", "31", "3b", "32", "43"];
const SHIFT_LEFT: [&str; 6] = ["1b", "5b", "31", "3b", "32", "44"];
const SETTLE_TIME: Duration = Duration::from_secs(180);
const TURN_TIME: Duration = Duration::from_secs(60);
const REPO: &str = "plant-shop";
const ABOUT: &str = "A small shop that sells house plants online.";
const STACK: &str = "TypeScript";
const MESSAGE: &str = "Hello master, what is in this repository?";
const MASTER_OPEN: &str = "End this project's master conversation first, then remove the project.";
const LEFTOVERS: [&str; 4] = ["Setting up", "Not set up", "being set up", "Continue setup"];
const MODEL_ACCESS: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:PORT/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
const PROJECT: &str = "Tidepool e2e";
const LIVE_SOCKET: &str = "CRAZYAGENT_E2E_LIVE_SOCKET";
const LIVE_PROJECT: &str = "CRAZYAGENT_E2E_LIVE_PROJECT";

struct Pty {
    tmux: PathBuf,
    socket: PathBuf,
    _folder: tempfile::TempDir,
}

impl Pty {
    fn launch(tmux: PathBuf, engine_socket: &Path) -> Self {
        let engine_socket = engine_socket.to_str().unwrap();
        assert!(!ENGINE.contains('\'') && !engine_socket.contains('\''));
        let folder = scratch("ca-e2e-pty-");
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
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
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

    fn text(&self, text: &str) {
        assert!(!text.is_empty() && !text.starts_with('-'));
        self.run(&["send-keys", "-t", SESSION, "-l", text]);
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", SESSION]).stdout)
            .into_owned()
    }

    fn until(&self, what: &str, check: impl Fn(&str) -> bool) -> String {
        self.until_within(what, SCREEN_TIME, check)
    }

    fn until_within(&self, what: &str, limit: Duration, check: impl Fn(&str) -> bool) -> String {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if check(&screen) {
                return screen;
            }
            assert!(start.elapsed() < limit, "{what}:\n{screen}");
            std::thread::sleep(POLL_TIME);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.socket);
    }
}

fn at_home(screen: &str, project: &str) -> bool {
    screen.contains("New project") && screen.contains(project)
}

fn on_agents(screen: &str, project: &str) -> bool {
    screen.contains(&format!("/  {project}  /  Agents"))
}

fn on_team(screen: &str, project: &str) -> bool {
    screen.contains(&format!("/  {project}  /  Team")) && screen.contains("Master")
}

/// Whether the crumbs end on the project named exactly `project`, or go on to one of its
/// tabs. Inputs: the screen and the project name. Output: true when the crumbs name it.
fn in_project(screen: &str, project: &str) -> bool {
    let crumbs = screen.lines().next().unwrap_or_default().trim_end();
    let place = format!("/  {project}");
    crumbs.ends_with(&place) || crumbs.contains(&format!("{place}  /  "))
}

/// Whether Home's selection bar marks the card named exactly `project`: the bar, then the
/// name, then nothing or the gap before the card's state.
/// Inputs: the screen and the project name. Output: true when the picked card is it.
fn picked_card(screen: &str, project: &str) -> bool {
    let gap = format!("{project}  ");
    screen.lines().any(|line| {
        ["\u{258c} ", "| "].iter().any(|bar| {
            line.trim_start()
                .strip_prefix(bar)
                .is_some_and(|rest| rest.trim_end() == project || rest.starts_with(&gap))
        })
    })
}

/// The name on the first line Home's selection bar marks, up to the gap before its state.
/// Input: the screen. Output: that name, or none when no line carries the bar.
fn picked_name(screen: &str) -> Option<&str> {
    screen.lines().find_map(|line| {
        let rest = ["\u{258c} ", "| "]
            .iter()
            .find_map(|bar| line.trim_start().strip_prefix(bar))?;
        rest.split("  ").next()
    })
}

/// The action Enter takes on Home's footer: the hint after `Enter `, up to the gap before
/// the next hint. Input: the screen. Output: that action, or none when no hint names Enter.
fn enter_hint(screen: &str) -> Option<&str> {
    let footer = screen.lines().rfind(|line| !line.trim().is_empty())?;
    footer
        .split("   ")
        .find_map(|hint| hint.trim().strip_prefix("Enter "))
}

/// Presses Tab until Home's footer offers Enter `action`, waiting for each press to redraw
/// the footer, so no press lands while the last one is still undrawn.
/// Inputs: the TUI on Home and the action. Output: none; panics when no panel offers it.
fn tab_until_enter(pty: &Pty, action: &str) {
    for step in 0..=STEPS_MAX {
        let screen = pty.until("Home's footer names what Enter does", |now| {
            enter_hint(now).is_some()
        });
        let shown = enter_hint(&screen).map(str::to_owned);
        if shown.as_deref() == Some(action) {
            return;
        }
        assert!(
            step < STEPS_MAX,
            "no Home panel offers Enter {action}:\n{screen}"
        );
        pty.keys(&["Tab"]);
        pty.until("Tab moves Home to its other panel", |now| {
            enter_hint(now) != shown.as_deref()
        });
    }
}

/// Moves Home's selection to the card named exactly `project`: filters Home by the name,
/// then steps down until the bar marks that card, so a newer card whose name starts with it
/// is passed over. Each step waits for the bar to leave the card it marked.
/// Inputs: the TUI on Home with its project list focused and the name.
/// Output: none; panics when no card has exactly that name.
fn pick_card(pty: &Pty, project: &str) {
    pty.keys(&["/"]);
    pty.text(project);
    let filter = format!("/ {project}");
    pty.until("Home's filter holds the project name", |screen| {
        screen.contains(&filter)
    });
    pty.keys(&["Enter"]);
    for step in 0..=STEPS_MAX {
        let screen = pty.until("Home keeps the filter", |screen| {
            !screen.contains("Keep filter")
        });
        assert_eq!(
            enter_hint(&screen),
            Some("Open"),
            "Home's project list lost the focus:\n{screen}"
        );
        if picked_card(&screen, project) {
            return;
        }
        assert!(
            step < STEPS_MAX && !screen.contains("No project matches"),
            "no Home card is named exactly {project}:\n{screen}"
        );
        let picked = picked_name(&screen).map(str::to_owned);
        assert!(picked.is_some(), "Home's bar marks no card:\n{screen}");
        pty.keys(&["Down"]);
        pty.until(
            &format!("no Home card below the picked one is named exactly {project}"),
            |now| picked_name(now) != picked.as_deref(),
        );
    }
}

fn reach_home(pty: &Pty, home: impl Fn(&str) -> bool) {
    for step in 0..=STEPS_MAX {
        assert!(
            step < STEPS_MAX,
            "the TUI never reached Home:\n{}",
            pty.screen()
        );
        let screen = pty.until("the TUI shows Home or a first-run step", |screen| {
            home(screen)
                || screen.contains("Welcome to CrazyAgent")
                || screen.contains("c Continue without")
        });
        if home(&screen) {
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

/// Waits for Home's side panel to settle on its tools check, the last answer Home waits for
/// before it moves the focus to the panel, so from that frame on only keys move the focus.
/// "Get set up" shows the check's words; "Learn CrazyAgent" (DEC-153) shows them only on the
/// picked row, so there the focus reaching Add an account is the sign.
/// Input: the TUI on its first Home, on an engine with no account. Output: none; panics.
fn setup_checked(pty: &Pty) {
    let screen = pty.until("Home's side panel settles on its tools check", |now| {
        (now.contains("Get set up")
            && (now.contains("[x] Install tools") || now.contains("Missing: ")))
            || (now.contains("Learn CrazyAgent") && enter_hint(now) == Some("Add an account"))
    });
    assert!(
        enter_hint(&screen).is_some(),
        "Home's footer names what Enter does:\n{screen}"
    );
}

/// Opens the team tree of the project named exactly `project` from the TUI's first Home,
/// on an engine with no account, once Home's setup panel shows its tools check.
/// Inputs: the TUI and the project name. Output: none; panics when a step fails.
fn open_team(pty: &Pty, project: &str) {
    reach_home(pty, |screen| at_home(screen, project));
    setup_checked(pty);
    team_from_home(pty, project);
}

/// Opens the team tree of the project named exactly `project` from Home: Tab to the
/// project list, pick the card, open it and walk to the Agents tab's team tree.
/// Inputs: the TUI on Home and the project name. Output: none; panics when a step fails.
fn team_from_home(pty: &Pty, project: &str) {
    tab_until_enter(pty, "Open");
    pick_card(pty, project);
    pty.keys(&["Enter"]);
    let screen = pty.until("the project screen opens with its tabs", |screen| {
        screen.contains("Tabs")
    });
    assert!(
        in_project(&screen, project),
        "Home opened another project than {project}:\n{screen}"
    );
    to_tab(pty, project, "Agents", &SHIFT_RIGHT);
    let agents = pty.screen();
    assert!(
        on_agents(&agents, project),
        "the Agents tab shows:\n{agents}"
    );
    pty.keys(&["t"]);
    pty.until("t on the Agents tab opens the team tree", |screen| {
        on_team(screen, project)
    });
}

#[test]
fn the_tui_walks_from_home_into_the_project_team_tree() {
    let mut engine = Engine::start("ca-e2e-tui-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
            assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
            assert_eq!(
                owner.create_project("e2e-tui", PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });

    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty, PROJECT);
    let screen = pty.screen();
    assert!(screen.contains("Master"), "{screen}");
    assert!(!screen.contains("Not ready yet"), "{screen}");
    drop(pty);
    engine.stop();
}

const NEWER_PROJECT: &str = "Tidepool e2e readd";

/// Home highlights its most recent card; a project whose name that newer card's name starts
/// with is still the one opened, by its exact name (live row 7 on 18.15 opened
/// ca-trial-4-readd-202611051754 for ca-trial-4).
#[test]
fn the_tui_opens_the_project_named_exactly_when_a_newer_card_starts_with_its_name() {
    let mut engine = Engine::start("ca-e2e-tui-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
            assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
            assert_eq!(
                owner.create_project("e2e-tui", PROJECT).await,
                OwnerResult::Applied
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(
                owner.create_project("e2e-tui-readd", NEWER_PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, NEWER_PROJECT));
    open_team(&pty, PROJECT);
    let screen = pty.screen();
    assert!(on_team(&screen, PROJECT), "{screen}");
    assert!(!screen.contains(NEWER_PROJECT), "{screen}");
    drop(pty);
    engine.stop();
}

const STOPPED_PROJECT: &str = "Stopped team e2e";
const LEAD_TASK: &str = "Run a native agent team";
const WORKER_TASKS: [&str; 2] = ["Write greet.py", "Write the README"];

/// Writes a lead and its two workers as the engine leaves them after Stop all: stopped, no
/// task held, each with the done task it ran, the workers' handed out by the lead. Inputs:
/// the engine root and the project id. Output: the lead and worker ids.
fn stopped_team(root: &Path, project: &str) -> (i64, [i64; 2]) {
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    db.busy_timeout(Duration::from_secs(10)).unwrap();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let agent = |parent: Option<i64>| {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,stopped_ms,turns,parent)
             VALUES (?1,'sub','codex',0,'local-connection','stopped',?2,?2,?2,1,?3)",
            rusqlite::params![project, now, parent],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let task = |prompt: &str, agent: i64, parent: Option<i64>| {
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,attempts,result,delivered,created_ms,started_ms,finished_ms,parent)
             VALUES (?1,'master','codex',0,?2,'done',?3,1,'Done',1,?4,?4,?4,?5)",
            rusqlite::params![project, prompt, agent, now, parent],
        )
        .unwrap();
    };
    let lead = agent(None);
    let workers = [agent(Some(lead)), agent(Some(lead))];
    task(LEAD_TASK, lead, None);
    for (prompt, worker) in WORKER_TASKS.iter().zip(workers) {
        task(prompt, worker, Some(lead));
    }
    (lead, workers)
}

#[test]
fn after_every_agent_stops_its_tasks_keep_its_name_and_f_lists_it_in_the_tui() {
    let mut engine = Engine::start("ca-e2e-stopped-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let project = ProjectId::new("e2e-stopped").unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner
                .create_project(project.as_str(), STOPPED_PROJECT)
                .await,
            OwnerResult::Applied
        );
        owner.controller.detach();
    });
    let (lead, workers) = stopped_team(engine.path(), project.as_str());
    let graph = runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        let graph = owner.controller.team_graph(project.clone()).await.unwrap();
        owner.controller.detach();
        graph
    });
    assert!(
        graph.nodes.iter().all(|node| !node.kind.agent()),
        "Finished agents is Hide, so the stopped agents leave the tree: {:#?}",
        graph.nodes
    );
    let name = |agent: i64| -> String {
        graph
            .edges
            .iter()
            .find(|edge| edge.to == format!("team-{agent}"))
            .and_then(|edge| edge.agent_label.clone())
            .unwrap_or_else(|| panic!("no task edge keeps agent {agent}: {:#?}", graph.edges))
    };
    let lead_name = name(lead);
    let worker_names = workers.map(name);

    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty, STOPPED_PROJECT);
    let screen = pty.until("the Tasks rows show every task", |screen| {
        screen.contains(LEAD_TASK) && WORKER_TASKS.iter().all(|task| screen.contains(task))
    });
    let row = line_of(&screen, LEAD_TASK);
    assert!(
        row.contains(&lead_name) && !row.contains("Master"),
        "the lead's task keeps the lead's name, not the master's:\n{screen}"
    );
    for (task, worker) in WORKER_TASKS.iter().zip(&worker_names) {
        let row = line_of(&screen, task);
        assert!(
            row.contains(worker.as_str()) && !row.contains(&lead_name),
            "a worker's task keeps the worker's name, not its lead's:\n{screen}"
        );
    }

    pty.keys(&["s"]);
    pty.until(
        "the agents list points at the Finished box for the three stopped agents",
        |screen| screen.contains("3 finished agents are in Finished: press Esc, then f."),
    );
    pty.keys(&["Escape"]);
    pty.until("Esc goes back to the team tree", |screen| {
        on_team(screen, STOPPED_PROJECT) && !screen.contains("in Finished: press Esc")
    });
    pty.keys(&["f"]);
    let finished = pty.until("f lists the stopped agents", |screen| {
        screen.contains("Finished (3)")
            && screen.contains(&lead_name)
            && worker_names
                .iter()
                .all(|worker| screen.contains(worker.as_str()))
    });
    assert!(
        finished.contains("Finished agents: Hide. They leave the team tree for this list."),
        "{finished}"
    );
    drop(pty);
    engine.stop();
}

fn line_of<'a>(screen: &'a str, label: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(label))
        .unwrap_or_else(|| panic!("no line shows {label:?}:\n{screen}"))
}

fn nothing_left_setting_up(screen: &str) {
    for leftover in LEFTOVERS {
        assert!(
            !screen.contains(leftover),
            "Home still shows {leftover:?}:\n{screen}"
        );
    }
}

fn tab_shown(screen: &str, project: &str) -> Option<String> {
    let crumb = format!("Home  /  {project}");
    let line = screen.lines().next()?;
    if line.contains(&format!("CrazyAgent  /  {project}  /  Master")) {
        return Some("Master".to_owned());
    }
    let rest = &line[line.find(&crumb)? + crumb.len()..];
    Some(match rest.strip_prefix("  /  ") {
        Some(tab) => tab.split("  ").next()?.trim().to_owned(),
        None => "Overview".to_owned(),
    })
}

fn to_tab(pty: &Pty, project: &str, tab: &str, keys: &[&str]) {
    for step in 0..=STEPS_MAX {
        let screen = pty.screen();
        let shown = tab_shown(&screen, project);
        if shown.as_deref() == Some(tab) {
            return;
        }
        if tab == "Conversation" && shown.as_deref() == Some("Master") {
            pty.keys(&["Escape"]);
            pty.until("Esc leaves the master view for the console", |now| {
                tab_shown(now, project).as_deref() == Some(tab)
            });
            return;
        }
        assert!(step < STEPS_MAX, "no project tab is {tab}:\n{screen}");
        pty.bytes(keys);
        pty.until("the next project tab shows", |now| {
            tab_shown(now, project) != shown
        });
    }
}

fn unreachable_provider(engine: &Engine) {
    let closed = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    std::fs::write(
        engine.path().join("global.toml"),
        MODEL_ACCESS.replace("PORT", &port.to_string()),
    )
    .unwrap();
}

async fn setup_status(engine: &Engine) -> SetupStatus {
    let mut owner = Owner::attach(engine).await;
    let status = owner.setup_checked().await;
    owner.controller.detach();
    status
}

async fn master_turns(engine: &Engine, project: &ProjectId) -> Vec<MasterTurnView> {
    let mut owner = Owner::attach(engine).await;
    let state = owner
        .controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ProjectMaster { session } = state.data else {
        panic!("the engine answers with the project master")
    };
    let Some(session) = session else {
        owner.controller.detach();
        return Vec::new();
    };
    let state = owner
        .controller
        .read(OwnerRead::ProjectMasterTurns {
            project_id: project.clone(),
            session_id: session.session_id,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    owner.controller.detach();
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        panic!("the engine answers with the master's turns")
    };
    page.turns
}

fn settled(turn: &MasterTurnView) -> bool {
    matches!(
        turn.phase,
        MasterTurnPhase::Succeeded
            | MasterTurnPhase::Failed
            | MasterTurnPhase::Cancelled
            | MasterTurnPhase::Uncertain
            | MasterTurnPhase::Blocked
            | MasterTurnPhase::WaitingApproval
    )
}

#[test]
fn the_owner_adds_a_folder_sets_it_up_opens_its_team_and_removes_it_in_the_tui() {
    let mut engine = Engine::new("ca-e2e-flow-");
    unreachable_provider(&engine);
    engine.launch();
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-disk-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let repo = disk.path().join("repos").join(REPO);
    repository(&repo, "README.md", "# Plant shop\n");
    let project = crazyagent::onboarding::project_id_for(&repo).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        owner.controller.detach();
    });

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| screen.contains("No projects yet."));
    nothing_left_setting_up(&pty.screen());

    pty.keys(&["a"]);
    pty.until("a on Home offers the ways to add a project", |screen| {
        screen.contains("Existing folder")
    });
    pty.keys(&["e"]);
    pty.until("the Existing folder page asks for the folder", |screen| {
        screen.contains("Use where it is") && screen.contains("Set up here")
    });
    pty.text(repo.to_str().unwrap());
    pty.until("the typed folder shows", |screen| screen.contains(REPO));
    pty.keys(&["Enter"]);
    pty.until("the setup page offers the pool", |screen| {
        screen.contains(&format!("Set up {REPO}"))
            && screen.contains("local-pool")
            && screen.contains("Start setup")
    });
    pty.keys(&["Enter"]);
    pty.until("the setup page shows both steps", |screen| {
        screen.contains("Build the code graph")
            && (screen.contains("Map the project") || screen.contains("Mapping failed:"))
    });

    let review = pty.until_within(
        "both setup steps finish and the draft shows",
        SETTLE_TIME,
        |screen| screen.contains("Confirm setup"),
    );
    let status = runtime.block_on(setup_status(&engine));
    let graph_state = status
        .pack
        .iter()
        .find(|entry| entry.id == "graphify")
        .map(|entry| entry.state.clone());
    let graph_on = graph_state == Some(PackState::On);
    let graphify_found = status.dependencies.iter().any(|tool| {
        tool.name == Dependency::Graphify
            && matches!(
                tool.state,
                DependencyState::Found { .. } | DependencyState::Installed { .. }
            )
    });
    let graph = line_of(&review, "Build the code graph");
    if graph_on {
        assert!(
            graph.contains("Done") || graph.contains("Failed"),
            "Graphify is on, so the graph step ends done or failed:\n{review}"
        );
        assert!(
            graphify_found || !graph.contains("Done"),
            "Graphify is missing, so the graph step cannot claim done:\n{review}"
        );
    } else if graph_state == Some(PackState::Off) {
        assert!(
            graph.contains("Turned off") && review.contains("Graphify is off in Setup"),
            "Graphify is off in Setup, so the setup says no code graph is built:\n{review}"
        );
    } else {
        assert!(
            graph.contains("Not installed") && review.contains("Graphify isn't installed"),
            "Setup never installed Graphify, so the setup says so:\n{review}"
        );
    }
    let mapping_failed = review.contains("Mapping failed:");
    if mapping_failed {
        let reason = line_of(&review, "Mapping failed:")
            .split("Mapping failed:")
            .nth(1)
            .unwrap_or_default()
            .trim();
        assert!(!reason.is_empty(), "a failed mapping shows why:\n{review}");
        assert!(
            !review.contains("Agent stopped with an error"),
            "a failed mapping shows its own reason:\n{review}"
        );
        assert!(
            review.contains("Fill in the setup yourself to skip it"),
            "a failed mapping offers to skip it:\n{review}"
        );
    }
    let missing = review
        .lines()
        .find(|line| line.contains("Missing:"))
        .unwrap_or_default()
        .to_owned();
    pty.keys(&["Tab"]);
    if missing.contains("About") {
        pty.text(ABOUT);
        pty.until("the description shows", |screen| screen.contains(ABOUT));
    }
    pty.keys(&["Tab"]);
    if missing.contains("Tech stack") {
        pty.text(STACK);
        pty.until("the tech stack shows", |screen| screen.contains(STACK));
    }
    pty.keys(&["Tab"]);
    if missing.contains("Platforms") {
        pty.keys(&["Space"]);
    }
    pty.keys(&["Tab"]);
    if review.contains("Plan a roadmap after research") {
        pty.keys(&["Tab"]);
    }
    let confirm = pty.until("Confirm is focused", |screen| {
        screen.contains("Enter Confirm setup")
    });
    if mapping_failed {
        assert!(
            confirm.contains("r Retry mapping"),
            "a failed mapping offers Retry on Confirm:\n{confirm}"
        );
    }
    pty.keys(&["Enter"]);
    pty.until("the setup is saved and the project opens", |screen| {
        screen.contains(&format!("{REPO} is ready."))
    });
    pty.until("the project page opens", |screen| {
        tab_shown(screen, REPO).as_deref() == Some("Overview")
    });

    pty.keys(&["Escape"]);
    let ready = pty.until("Home lists the project", |screen| {
        screen.contains(REPO) && screen.contains("Most recent first")
    });
    nothing_left_setting_up(&ready);
    assert!(!ready.contains("No projects yet."), "{ready}");
    assert!(
        !ready.contains("Needs a pool"),
        "the chosen pool went to the project:\n{ready}"
    );

    tab_until_enter(&pty, "Open");
    pty.keys(&["Enter"]);
    pty.until("the project opens on Overview", |screen| {
        tab_shown(screen, REPO).as_deref() == Some("Overview")
    });
    to_tab(&pty, REPO, "Agents", &SHIFT_RIGHT);
    pty.keys(&["t"]);
    let team = pty.until("t on the Agents tab opens the team tree", |screen| {
        on_team(screen, REPO)
    });
    assert!(!team.contains("Not ready yet"), "{team}");
    assert!(!team.contains("Couldn't"), "{team}");
    pty.keys(&["Escape"]);
    pty.until("Esc goes back to the project", |screen| {
        tab_shown(screen, REPO).is_some_and(|tab| tab != "Team")
    });

    to_tab(&pty, REPO, "Conversation", &SHIFT_LEFT);
    let conversation = pty.until("the master conversation shows", |screen| {
        screen.contains("Master") && screen.contains("Message the master")
    });
    assert!(
        !conversation.contains("Needs a pool"),
        "the master has the project's pool:\n{conversation}"
    );
    pty.keys(&["Enter"]);
    pty.until("Enter starts typing", |screen| {
        screen.contains("Enter Send")
    });
    pty.text(MESSAGE);
    pty.until("the message shows in the input", |screen| {
        screen.contains(MESSAGE)
    });
    pty.keys(&["Enter"]);
    pty.until_within("the master's first turn shows", TURN_TIME, |screen| {
        screen.contains(MESSAGE)
            && (screen.contains("Thinking...")
                || screen.contains("Stopped")
                || screen.contains("Not signed in")
                || screen.contains("Needs you"))
    });
    let start = Instant::now();
    let turn = loop {
        let turns = runtime.block_on(master_turns(&engine, &project));
        assert!(
            !turns.is_empty() || start.elapsed() < SCREEN_TIME,
            "the engine admitted the master's first turn:\n{}",
            pty.screen()
        );
        if let Some(turn) = turns.last().filter(|turn| settled(turn)) {
            break Some(turn.clone());
        }
        if start.elapsed() > TURN_TIME {
            break None;
        }
        std::thread::sleep(POLL_TIME);
    };
    match turn {
        Some(turn) => {
            let shown = pty.until("the TUI shows how the turn ended", |screen| {
                screen.contains("stopped:")
                    || screen.contains("Not signed in")
                    || screen.contains("It is not known whether")
                    || screen.contains("waiting for your approval")
                    || screen.contains("Replied")
            });
            let coded = !matches!(turn.reason, None | Some(MasterReason::Unknown));
            assert!(
                !coded || !shown.contains("something went wrong"),
                "the engine said why the turn stopped, so the owner sees it ({:?}, {:?}):\n{shown}",
                turn.phase,
                turn.reason
            );
        }
        None => {
            let screen = pty.screen();
            assert!(screen.contains("Thinking..."), "{screen}");
        }
    }

    let remove = || {
        to_tab(&pty, REPO, "Settings", &SHIFT_RIGHT);
        pty.keys(&["x"]);
        let ask = pty.until("x asks before removing the project", |screen| {
            screen.contains(&format!("Remove {REPO} from CrazyAgent?"))
        });
        let dialog = ask
            .lines()
            .filter_map(|line| line.split_once('│').map(|(_, rest)| rest))
            .map(|rest| rest.rsplit_once('│').map_or(rest, |(inside, _)| inside))
            .collect::<Vec<_>>()
            .join(" ");
        let words = dialog.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("The project folder stays on disk"), "{ask}");
        pty.keys(&["y"]);
        pty.until(
            "the project is removed or the owner is told why not",
            |screen| screen.contains("No projects yet.") || screen.contains(MASTER_OPEN),
        )
    };
    if remove().contains(MASTER_OPEN) {
        to_tab(&pty, REPO, "Conversation", &SHIFT_LEFT);
        pty.until("the Conversation tab opens in its input", |screen| {
            screen.contains("Esc Leave input")
        });
        pty.keys(&["Escape"]);
        pty.until(
            "the Conversation tab offers to end the conversation",
            |screen| screen.contains("e End conversation"),
        );
        pty.keys(&["e"]);
        pty.until("the master conversation ends", |screen| {
            screen.contains("The master conversation is closed.")
        });
        let removed = remove();
        assert!(removed.contains("No projects yet."), "{removed}");
    }
    nothing_left_setting_up(&pty.screen());
    assert!(repo.join(".git").is_dir(), "the folder is still on disk");
    assert!(
        repo.join("README.md").is_file(),
        "the folder is still on disk"
    );
    drop(pty);
    engine.stop();
}

fn live_team_missing() -> ! {
    panic!(
        "UNAVAILABLE: a running engine with a live team agent. Start a project master on a \
         signed-in pool, then set {LIVE_SOCKET}=<its engine.sock> and \
         {LIVE_PROJECT}=<the project name>"
    )
}

#[test]
fn the_tui_types_into_a_live_agent_terminal_from_the_team_tree() {
    let socket = std::env::var_os(LIVE_SOCKET)
        .map(PathBuf::from)
        .unwrap_or_else(|| live_team_missing());
    let project = std::env::var(LIVE_PROJECT).unwrap_or_else(|_| live_team_missing());
    assert!(socket.exists(), "{LIVE_SOCKET} names no socket");
    let probe = scratch("ca-e2e-probe-");
    let tmux = installed(probe.path()).expect(TMUX);
    let pty = Pty::launch(tmux, &socket);
    reach_home(&pty, |screen| at_home(screen, &project));
    team_from_home(&pty, &project);
    pty.keys(&["Enter"]);
    for step in 0..=STEPS_MAX {
        if pty.screen().contains("Back to the team") {
            break;
        }
        assert!(
            step < STEPS_MAX,
            "Enter then Tab never moved the keys into the live master's terminal:\n{}",
            pty.screen()
        );
        pty.keys(&["Tab"]);
        std::thread::sleep(POLL_TIME);
    }
    let marker = format!("e2e{}", hex(4));
    pty.text(&marker);
    pty.until("the typed text shows in the terminal pane", |screen| {
        screen.contains(&marker)
    });
    for _ in 0..marker.len() {
        pty.keys(&["BSpace"]);
    }
    pty.until("the typed text is erased again", |screen| {
        !screen.contains(&marker)
    });
}

const KEYBOARD_QUERY: &[u8] = b"\x1b[?u";
const KEYBOARD_PUSH: &[u8] = b"\x1b[>1u";
const KEYBOARD_POP: &[u8] = b"\x1b[<1u";
const KEYBOARD_REPLY: &[u8] = b"\x1b[?0u\x1b[?62;22c";
const DEVICE_ONLY_REPLY: &[u8] = b"\x1b[?62;22c";
const LEAVE_SCREEN: &[u8] = b"\x1b[?1049l";
const SHIFT_ENTER: &[u8] = b"\x1b[13;2u";

/// A TUI on a pseudo-terminal from `/usr/bin/script` that the test plays the terminal for:
/// it keeps every byte the TUI writes and types bytes back, so it can answer the keyboard
/// protocol query tmux leaves unanswered. Built from the engine socket and the home folder
/// the TUI keeps its choices in; dropping it ends `script` and with it the TUI.
struct ScriptPty {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl ScriptPty {
    fn launch(engine_socket: &Path, home: &Path) -> Self {
        let engine_socket = engine_socket.to_str().unwrap();
        assert!(!ENGINE.contains('\'') && !engine_socket.contains('\''));
        let command =
            format!("stty cols {COLUMNS} rows {ROWS}; exec '{ENGINE}' tui '{engine_socket}'");
        let mut script = Command::new("/usr/bin/script");
        if cfg!(target_os = "macos") {
            script.args(["-q", "/dev/null", "/bin/sh", "-c", &command]);
        } else {
            script.args(["-q", "-e", "-c", &command, "/dev/null"]);
        }
        script
            .env("HOME", home)
            .env("TERM", "xterm-256color")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crazyagent::spawn_gate::spawn(&mut script)
            .expect("/usr/bin/script runs the TUI on a pseudo-terminal");
        let input = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut stdout, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink.lock().unwrap().extend_from_slice(&chunk[..read]),
                }
            }
        });
        Self {
            child,
            input,
            output,
            reader: Some(reader),
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        std::io::Write::write_all(&mut self.input, bytes).unwrap();
        std::io::Write::flush(&mut self.input).unwrap();
    }

    fn written(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    fn screen(&self) -> String {
        let mut parser = vt100::Parser::new(ROWS.parse().unwrap(), COLUMNS.parse().unwrap(), 0);
        parser.process(&self.written());
        parser.screen().contents()
    }

    fn until_written(&self, what: &str, check: impl Fn(&[u8]) -> bool) {
        let start = Instant::now();
        while !check(&self.written()) {
            assert!(start.elapsed() < SCREEN_TIME, "{what}:\n{}", self.screen());
            std::thread::sleep(POLL_TIME);
        }
    }

    fn until(&self, what: &str, check: impl Fn(&str) -> bool) {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if check(&screen) {
                return;
            }
            assert!(start.elapsed() < SCREEN_TIME, "{what}:\n{screen}");
            std::thread::sleep(POLL_TIME);
        }
    }

    fn exited(mut self) -> Vec<u8> {
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                start.elapsed() < SCREEN_TIME,
                "the TUI never exited:\n{}",
                self.screen()
            );
            std::thread::sleep(POLL_TIME);
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.written()
    }
}

impl Drop for ScriptPty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn found(written: &[u8], sequence: &[u8]) -> Vec<usize> {
    written
        .windows(sequence.len())
        .enumerate()
        .filter(|(_, window)| *window == sequence)
        .map(|(index, _)| index)
        .collect()
}

fn pushes_a_keyboard_protocol(written: &[u8]) -> bool {
    found(written, b"\x1b[>").into_iter().any(|start| {
        let rest = &written[start + 3..];
        let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
        digits > 0 && rest.get(digits) == Some(&b'u')
    })
}

fn bio_lines(screen: &str, first: &str, second: &str) -> bool {
    let rows: Vec<&str> = screen.lines().collect();
    rows.windows(2).any(|pair| {
        pair[0].contains(&format!("[ {first} ")) && pair[1].contains(&format!("[ {second} "))
    })
}

fn welcome_bio(pty: &ScriptPty) {
    pty.until("the first-run profile form shows", |screen| {
        screen.contains("Welcome to CrazyAgent") && screen.contains("Short bio")
    });
}

/// N-T1 C, Auto (the default): a terminal that answers the keyboard protocol query gets
/// the protocol turned on, Shift-Enter (CSI 13;2u) then makes a new line in the bio, and
/// the exit turns the protocol off before it leaves the alternate screen, so the shell is
/// back to legacy keys.
#[test]
fn shift_enter_auto_turns_the_keyboard_protocol_on_and_the_exit_turns_it_off() {
    let mut engine = Engine::start("ca-e2e-keys-");
    let home = scratch("ca-e2e-keys-home-");
    let mut pty = ScriptPty::launch(&engine.socket(), home.path());
    pty.until_written(
        "Auto asks the terminal for the keyboard protocol",
        |written| !found(written, KEYBOARD_QUERY).is_empty(),
    );
    pty.type_bytes(KEYBOARD_REPLY);
    pty.until_written(
        "a terminal that has it gets the protocol turned on",
        |written| !found(written, KEYBOARD_PUSH).is_empty(),
    );
    welcome_bio(&pty);
    pty.type_bytes(b"\t\tfirst");
    pty.type_bytes(SHIFT_ENTER);
    pty.type_bytes(b"second");
    pty.until("Shift-Enter makes a new line in the bio", |screen| {
        bio_lines(screen, "first", "second")
    });
    pty.type_bytes(b"\x1b[117;5u");
    pty.until("Ctrl-U from the protocol clears the bio", |screen| {
        !screen.contains("second")
    });
    pty.type_bytes(b"\x1b[113;5u");
    let written = pty.exited();
    let pushes = found(&written, KEYBOARD_PUSH);
    let pops = found(&written, KEYBOARD_POP);
    let leaves = found(&written, LEAVE_SCREEN);
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert_eq!(pops.len(), 1, "the exit turns the protocol off once");
    assert!(
        pushes[0] < pops[0] && leaves.last().is_some_and(|leave| pops[0] < *leave),
        "popped after the push and before the alternate screen is left: {pushes:?} {pops:?} {leaves:?}"
    );
    engine.stop();
}

/// N-T1 C, Auto on a terminal without the protocol: it answers the query with its device
/// attributes only, so the TUI never turns the protocol on and the exit has nothing to turn
/// off; Ctrl-J still makes a new line in the bio.
#[test]
fn shift_enter_auto_leaves_a_terminal_without_the_protocol_as_it_is() {
    let mut engine = Engine::start("ca-e2e-keys-");
    let home = scratch("ca-e2e-keys-home-");
    let mut pty = ScriptPty::launch(&engine.socket(), home.path());
    pty.until_written(
        "Auto asks the terminal for the keyboard protocol",
        |written| !found(written, KEYBOARD_QUERY).is_empty(),
    );
    pty.type_bytes(DEVICE_ONLY_REPLY);
    welcome_bio(&pty);
    pty.type_bytes(b"\t\tfirst\nsecond");
    pty.until("Ctrl-J makes a new line in the bio", |screen| {
        bio_lines(screen, "first", "second")
    });
    pty.type_bytes(b"\x15");
    pty.until("Ctrl-U clears the bio", |screen| !screen.contains("second"));
    pty.type_bytes(b"\x11");
    let written = pty.exited();
    assert_eq!(found(&written, KEYBOARD_QUERY).len(), 1, "Auto asks once");
    assert!(
        !pushes_a_keyboard_protocol(&written),
        "a terminal without the protocol never gets it turned on"
    );
    assert!(
        found(&written, KEYBOARD_POP).is_empty(),
        "nothing pushed, nothing popped"
    );
    engine.stop();
}

/// N-T1 C, Off: the TUI never asks the terminal and never turns the protocol on, and Ctrl-J
/// makes a new line in the bio.
#[test]
fn shift_enter_off_never_asks_the_terminal_and_ctrl_j_makes_a_new_line() {
    let mut engine = Engine::start("ca-e2e-keys-");
    let home = scratch("ca-e2e-keys-home-");
    let prefs = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support/CrazyAgent")
    } else {
        home.path().join(".config/crazyagent")
    };
    std::fs::create_dir_all(&prefs).unwrap();
    std::fs::write(prefs.join("tui-shift-enter"), "off").unwrap();
    let mut pty = ScriptPty::launch(&engine.socket(), home.path());
    welcome_bio(&pty);
    pty.type_bytes(b"\t\tfirst\nsecond");
    pty.until("Ctrl-J makes a new line in the bio", |screen| {
        bio_lines(screen, "first", "second")
    });
    pty.type_bytes(b"\x15");
    pty.until("Ctrl-U clears the bio", |screen| !screen.contains("second"));
    pty.type_bytes(b"\x11");
    let written = pty.exited();
    assert!(
        found(&written, KEYBOARD_QUERY).is_empty(),
        "Off never asks the terminal"
    );
    assert!(
        !pushes_a_keyboard_protocol(&written),
        "Off never turns the protocol on"
    );
    assert!(found(&written, KEYBOARD_POP).is_empty());
    engine.stop();
}

fn script_home(pty: &mut ScriptPty) {
    for step in 0..=STEPS_MAX {
        assert!(
            step < STEPS_MAX,
            "the TUI never reached Home:\n{}",
            pty.screen()
        );
        pty.until("the TUI shows Home or a first-run step", |screen| {
            at_home(screen, PROJECT) || screen.contains("c Continue without")
        });
        let screen = pty.screen();
        if at_home(&screen, PROJECT) {
            return;
        }
        pty.type_bytes(b"c");
        pty.until("the first-run step moves on", |now| now != screen);
    }
}

/// N-T1 C: a Shift-Enter choice saved in Settings > Profile applies at once. Saving Off
/// turns the protocol off without asking the terminal, saving Auto asks again and turns it
/// back on, and the exit turns it off once more before it leaves the alternate screen.
#[test]
fn a_saved_shift_enter_choice_switches_the_keyboard_protocol_at_once() {
    let mut engine = Engine::start("ca-e2e-keys-");
    let disk = scratch("ca-e2e-keys-projects-");
    let projects = disk.path().join("Projects");
    std::fs::create_dir(&projects).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
            assert_eq!(owner.set_home(&projects).await, OwnerResult::Applied);
            assert_eq!(
                owner.create_project("e2e-keys", PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });
    let home = scratch("ca-e2e-keys-home-");
    let mut pty = ScriptPty::launch(&engine.socket(), home.path());
    pty.until_written("Auto asks the terminal at start", |written| {
        found(written, KEYBOARD_QUERY).len() == 1
    });
    pty.type_bytes(KEYBOARD_REPLY);
    pty.until_written("the protocol is turned on at start", |written| {
        found(written, KEYBOARD_PUSH).len() == 1
    });
    script_home(&mut pty);
    pty.type_bytes(b"s");
    pty.until("Home opens Settings", |screen| screen.contains("Settings"));
    pty.type_bytes(b"\x1b[1;2D");
    pty.until("Settings > Profile shows the Shift-Enter row", |screen| {
        screen.contains("Shift-Enter") && screen.contains("( ) Off")
    });
    pty.type_bytes(b"\t\t\t\t\t\t\t\t\t\x1b[C\t\r");
    pty.until_written("saving Off turns the protocol off at once", |written| {
        found(written, KEYBOARD_POP).len() == 1
    });
    pty.until("the profile is saved with Off", |screen| {
        screen.contains("Saved.") && screen.contains("(*) Off")
    });
    assert_eq!(
        found(&pty.written(), KEYBOARD_QUERY).len(),
        1,
        "saving Off never asks the terminal"
    );
    pty.type_bytes(b"\x1b[Z\x1b[C\t\r");
    pty.until_written("saving Auto asks the terminal again", |written| {
        found(written, KEYBOARD_QUERY).len() == 2
    });
    pty.type_bytes(KEYBOARD_REPLY);
    pty.until_written("saving Auto turns the protocol back on", |written| {
        found(written, KEYBOARD_PUSH).len() == 2
    });
    pty.type_bytes(b"\x1b[113;5u");
    let written = pty.exited();
    let pushes = found(&written, KEYBOARD_PUSH);
    let pops = found(&written, KEYBOARD_POP);
    let leaves = found(&written, LEAVE_SCREEN);
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    assert_eq!(pops.len(), 2, "{pops:?}");
    assert!(
        pushes[0] < pops[0]
            && pops[0] < pushes[1]
            && pushes[1] < pops[1]
            && leaves.last().is_some_and(|leave| pops[1] < *leave),
        "push, pop on Off, push on Auto, pop before the alternate screen is left: {pushes:?} {pops:?} {leaves:?}"
    );
    engine.stop();
}

const SWITCH_ID: &str = "e2e-master-switch";
const SWITCH_PROJECT: &str = "Master switch e2e";
const APPLY_IDLE: &str = ", when the master is idle";
const APPLY_NEXT: &str = ", at the next start";

/// Whether the Team settings summary shows the Apply a master harness change choice after
/// the On harness switch choice. Inputs: the screen and the choice words. Output: a boolean.
fn applied(screen: &str, choice: &str) -> bool {
    screen
        .lines()
        .any(|line| line.contains("On harness switch  ") && line.contains(choice))
}

/// Starts an engine with one fresh project and opens that project's Team settings in the
/// TUI. Input: a scratch prefix. Output: the engine, the TUI, the project id and the
/// projects home, which lives as long as the test needs it.
fn switch_settings(prefix: &str) -> (Engine, Pty, ProjectId, tempfile::TempDir) {
    let engine = Engine::start(prefix);
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
            assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
            assert_eq!(
                owner.create_project(SWITCH_ID, SWITCH_PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });
    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty, SWITCH_PROJECT);
    pty.keys(&["s"]);
    pty.until("s on the team tree opens the Team settings", |screen| {
        screen.contains("Team settings") && screen.contains("On harness switch  ")
    });
    (engine, pty, ProjectId::new(SWITCH_ID).unwrap(), disk)
}

/// Reads what the engine keeps for the master harness switch.
/// Inputs: the engine and the project. Output: the Apply a master harness change mode and
/// the master harness id saved in the project setup.
async fn switch_state(
    engine: &Engine,
    project: &ProjectId,
) -> (crazyagent::team::SwitchApply, String) {
    let mut owner = Owner::attach(engine).await;
    let status = owner
        .controller
        .read(OwnerRead::TeamStatus {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    let OwnerData::TeamStatus { status } = status.data else {
        panic!("the engine answers with the team status")
    };
    let setup = owner
        .controller
        .read(OwnerRead::ProjectSetup {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    owner.controller.detach();
    let OwnerData::ProjectSetup {
        setup: Some(setup), ..
    } = setup.data
    else {
        panic!("the engine answers with the saved project setup")
    };
    (status.apply_harness_switch, setup.team.master)
}

/// The master harness picker as the screen shows it: each offered harness name in order
/// and the picked row. Input: the screen. Output: the names and the picked index, none
/// when no row is picked.
fn picker_rows(screen: &str) -> (Vec<String>, Option<usize>) {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(top) = lines
        .iter()
        .position(|line| line.contains("Harness for the master"))
    else {
        return (Vec::new(), None);
    };
    let mut names = Vec::new();
    let mut picked = None;
    for line in &lines[top + 1..] {
        let inner = match (line.find('│'), line.rfind('│')) {
            (Some(left), Some(right)) if left < right => &line[left + '│'.len_utf8()..right],
            _ => line,
        }
        .trim();
        if inner.is_empty() || inner.starts_with("Enter saves it.") {
            break;
        }
        let (chosen, name) = match inner.strip_prefix("> ") {
            Some(name) => (true, name),
            None => (false, inner),
        };
        if chosen {
            picked = Some(names.len());
        }
        names.push(name.trim_end_matches("  now").trim().to_owned());
    }
    (names, picked)
}

/// Opens the master harness picker with H and saves the harness next to the saved one.
/// Input: the TUI on the Team settings. Output: the name of the harness it saved; panics
/// as a missing prerequisite when fewer than two harnesses can run the master here.
fn pick_other_master(pty: &Pty) -> String {
    pty.keys(&["H"]);
    let screen = pty.until(
        "H opens the master harness picker with its harnesses",
        |screen| {
            screen.contains("Enter saves it.") || screen.contains("No harness can run the master")
        },
    );
    let (names, picked) = picker_rows(&screen);
    assert!(
        names.len() >= 2,
        "prerequisite: switching the master needs two harnesses that can run it; the picker offers {names:?}:\n{screen}"
    );
    let picked =
        picked.unwrap_or_else(|| panic!("the picker starts on the saved harness:\n{screen}"));
    let (key, target) = if picked + 1 < names.len() {
        ("Down", picked + 1)
    } else {
        ("Up", picked - 1)
    };
    pty.keys(&[key]);
    pty.until("the picker moves to the other harness", |screen| {
        picker_rows(screen).1 == Some(target)
    });
    pty.keys(&["Enter"]);
    names[target].clone()
}

/// When the master is idle (the default, OP-28): the TUI saves a master harness picked in
/// Team settings through the project setup and says the running master switches once it is
/// idle.
#[test]
fn the_tui_saves_a_master_harness_that_reaches_the_master_once_it_is_idle() {
    let (mut engine, pty, project, _disk) = switch_settings("ca-e2e-idle-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mode, before) = runtime.block_on(switch_state(&engine, &project));
    assert_eq!(mode, crazyagent::team::SwitchApply::WhenIdle);
    pty.until("the summary names the default", |screen| {
        applied(screen, APPLY_IDLE)
    });

    let name = pick_other_master(&pty);
    let said = format!("The master switches to {name} once it is idle.");
    pty.until("the save says when the running master switches", |screen| {
        screen.contains(&said) && !screen.contains("Harness for the master")
    });
    let saved = format!("{name} leads.");
    pty.until("the summary names the saved harness", |screen| {
        screen.contains(&saved) && applied(screen, APPLY_IDLE)
    });
    let (mode, after) = runtime.block_on(switch_state(&engine, &project));
    assert_eq!(mode, crazyagent::team::SwitchApply::WhenIdle);
    assert_ne!(after, before, "the project setup keeps the picked master");
    drop(pty);
    engine.stop();
}

/// At the next start (OP-28): A saves the choice in the engine, a master harness picked
/// after it says it waits for the master's next start, and A again goes back to When the
/// master is idle.
#[test]
fn the_tui_saves_a_master_harness_that_waits_for_the_masters_next_start() {
    let (mut engine, pty, project, _disk) = switch_settings("ca-e2e-next-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    pty.until("the summary names the default", |screen| {
        applied(screen, APPLY_IDLE)
    });
    pty.keys(&["A"]);
    pty.until("A picks At the next start", |screen| {
        applied(screen, APPLY_NEXT)
            && screen
                .contains("A saved master harness change now waits for the master's next start.")
    });
    let (mode, before) = runtime.block_on(switch_state(&engine, &project));
    assert_eq!(mode, crazyagent::team::SwitchApply::NextStart);

    let name = pick_other_master(&pty);
    let said = format!("The master switches to {name} at its next start.");
    pty.until(
        "the save says the change waits for the next start",
        |screen| screen.contains(&said) && !screen.contains("Harness for the master"),
    );
    let saved = format!("{name} leads.");
    pty.until("the summary names the saved harness", |screen| {
        screen.contains(&saved) && applied(screen, APPLY_NEXT)
    });
    let (mode, after) = runtime.block_on(switch_state(&engine, &project));
    assert_eq!(mode, crazyagent::team::SwitchApply::NextStart);
    assert_ne!(after, before, "the project setup keeps the picked master");

    pty.keys(&["A"]);
    pty.until("A goes back to When the master is idle", |screen| {
        applied(screen, APPLY_IDLE)
            && screen.contains(
                "A saved master harness change now reaches the running master once it is idle.",
            )
    });
    let (mode, _) = runtime.block_on(switch_state(&engine, &project));
    assert_eq!(mode, crazyagent::team::SwitchApply::WhenIdle);
    drop(pty);
    engine.stop();
}

const RIGHT_KEY: &[u8] = b"\x1b[C";
const ESC_KEY: &[u8] = b"\x1b";
const GUIDE_QUIET: Duration = Duration::from_secs(3);

/// Whether the engine holds the Learn CrazyAgent guide as skipped, read through the real
/// owner socket. Inputs: the runtime and the engine. Output: the skipped flag.
fn guide_skipped(runtime: &tokio::runtime::Runtime, engine: &Engine) -> bool {
    runtime.block_on(async {
        let mut owner = Owner::attach(engine).await;
        let progress = owner.controller.learning_progress().await.unwrap();
        owner.controller.detach();
        progress.skipped
    })
}

/// Takes a fresh engine through the TUI first run to its last page: saves a profile, keeps
/// the suggested projects folder, continues past Setup without installing, then waits for
/// the Accounts step with its ticked Learn CrazyAgent offer. Inputs: the engine and the home
/// folder the TUI runs with. Output: the TUI on the Accounts step.
fn reach_accounts_step(engine: &Engine, home: &Path) -> ScriptPty {
    std::fs::create_dir(home.join("Projects")).unwrap();
    let prefs = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/CrazyAgent")
    } else {
        home.join(".config/crazyagent")
    };
    std::fs::create_dir_all(&prefs).unwrap();
    std::fs::write(prefs.join("tui-shift-enter"), "off").unwrap();
    let mut pty = ScriptPty::launch(&engine.socket(), home);
    welcome_bio(&pty);
    pty.type_bytes(b"Mira Okafor\t\t\t\t\t\t\r");
    pty.until("the projects folder step opens", |screen| {
        screen.contains("Your projects folder")
    });
    pty.type_bytes(b"\r");
    pty.until("Setup offers to continue without installing", |screen| {
        screen.contains("c Continue without")
    });
    pty.type_bytes(b"c");
    pty.until("the Accounts step opens with the ticked guide", |screen| {
        screen.contains("Add an account")
            && screen.contains("[> Add account ]   [ Skip for now ]")
            && screen.contains("Learn CrazyAgent")
            && screen.contains("[x] Show the guide on Home")
    });
    pty
}

/// DEC-153: keeping the Learn CrazyAgent guide ticked and picking Skip for now ends first run
/// on Home with the guide, and the engine holds nothing as skipped.
#[test]
fn first_run_with_the_guide_ticked_skips_accounts_and_keeps_the_guide_on_home() {
    let mut engine = Engine::start("ca-e2e-learn-");
    let home = scratch("ca-e2e-learn-home-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut pty = reach_accounts_step(&engine, home.path());
    pty.type_bytes(RIGHT_KEY);
    pty.until("Right focuses Skip for now", |screen| {
        screen.contains("[ Add account ]   [> Skip for now ]")
    });
    pty.type_bytes(b"\r");
    pty.until("Home shows the Learn CrazyAgent guide", |screen| {
        screen.contains("New project")
            && screen.contains("Learn CrazyAgent")
            && !screen.contains("Show the guide on Home")
    });
    assert!(
        !guide_skipped(&runtime, &engine),
        "a ticked guide writes no skip"
    );
    drop(pty);
    engine.stop();
}

/// Ends a fresh engine's TUI first run with the guide unticked and Skip for now, then waits
/// for Home without the guide and for the engine to hold the guide as skipped. Inputs: the
/// runtime, the engine and the home folder the TUI runs with. Output: the TUI on Home.
fn first_run_skipping_the_guide(
    runtime: &tokio::runtime::Runtime,
    engine: &Engine,
    home: &Path,
) -> ScriptPty {
    let mut pty = reach_accounts_step(engine, home);
    pty.type_bytes(b" ");
    pty.until("Space unticks the guide", |screen| {
        screen.contains("[ ] Show the guide on Home")
    });
    pty.type_bytes(RIGHT_KEY);
    pty.until("Right focuses Skip for now", |screen| {
        screen.contains("[ Add account ]   [> Skip for now ]")
    });
    pty.type_bytes(b"\r");
    pty.until("Home opens with Get set up and no guide", |screen| {
        screen.contains("New project")
            && screen.contains("Get set up")
            && !screen.contains("Learn CrazyAgent")
    });
    let start = Instant::now();
    while !guide_skipped(runtime, engine) {
        assert!(
            start.elapsed() < SCREEN_TIME,
            "the engine never held the guide as skipped"
        );
        std::thread::sleep(POLL_TIME);
    }
    pty
}

/// Opens Settings, Profile from Home and waits for its Learn CrazyAgent entries.
/// Input: the TUI on Home. Output: none; the TUI is on Settings, Profile.
fn open_profile_replay(pty: &mut ScriptPty) {
    pty.type_bytes(b"s");
    pty.until("Settings opens on Accounts", |screen| {
        screen.contains("Settings  /  Accounts")
    });
    pty.type_bytes(b"[");
    pty.until("Profile shows the Learn CrazyAgent entries", |screen| {
        screen.contains("Settings  /  Profile")
            && screen.contains("g  Show the learning guide again")
            && screen.contains("f  Run first-run setup again")
    });
}

/// DEC-153 slice 4: after first run skipped the guide, Settings, Profile's Show the learning
/// guide again clears the skip in the engine and Home shows the guide again.
#[test]
fn show_the_learning_guide_again_on_profile_clears_the_skip_and_home_shows_the_guide() {
    let mut engine = Engine::start("ca-e2e-learn-");
    let home = scratch("ca-e2e-learn-home-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut pty = first_run_skipping_the_guide(&runtime, &engine, home.path());
    open_profile_replay(&mut pty);
    pty.type_bytes(b"g");
    pty.until("Home shows the Learn CrazyAgent guide again", |screen| {
        screen.contains("New project")
            && screen.contains("Learn CrazyAgent")
            && !screen.contains("Settings  /  Profile")
    });
    assert!(
        !guide_skipped(&runtime, &engine),
        "the replay clears the skip in the engine"
    );
    drop(pty);
    engine.stop();
}

/// DEC-153 slice 4: after first run skipped the guide, Settings, Profile's Run first-run
/// setup again clears the skip in the engine, starts first run with the saved profile, and
/// Skip for now with the guide ticked ends on Home with the guide.
#[test]
fn run_first_run_setup_again_on_profile_clears_the_skip_and_ends_on_home_with_the_guide() {
    let mut engine = Engine::start("ca-e2e-learn-");
    let home = scratch("ca-e2e-learn-home-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut pty = first_run_skipping_the_guide(&runtime, &engine, home.path());
    open_profile_replay(&mut pty);
    pty.type_bytes(b"f");
    pty.until("first run opens with the saved profile", |screen| {
        screen.contains("Welcome to CrazyAgent") && screen.contains("Mira Okafor")
    });
    assert!(
        !guide_skipped(&runtime, &engine),
        "the replay clears the skip before first run"
    );
    pty.type_bytes(b"\t\t\t\t\t\t\r");
    pty.until("the projects folder step opens", |screen| {
        screen.contains("Your projects folder")
    });
    pty.type_bytes(b"\r");
    pty.until("Setup offers to continue without installing", |screen| {
        screen.contains("c Continue without")
    });
    pty.type_bytes(b"c");
    pty.until("the Accounts step opens with the ticked guide", |screen| {
        screen.contains("[x] Show the guide on Home")
    });
    pty.type_bytes(RIGHT_KEY);
    pty.until("Right focuses Skip for now", |screen| {
        screen.contains("[ Add account ]   [> Skip for now ]")
    });
    pty.type_bytes(b"\r");
    pty.until("Home shows the Learn CrazyAgent guide", |screen| {
        screen.contains("New project")
            && screen.contains("Learn CrazyAgent")
            && !screen.contains("Show the guide on Home")
    });
    assert!(
        !guide_skipped(&runtime, &engine),
        "a ticked guide writes no skip"
    );
    drop(pty);
    engine.stop();
}

/// DEC-153: unticking the guide and picking Add account ends first run on Add account over
/// Home, the engine holds the guide as skipped, and Home shows no guide.
#[test]
fn first_run_with_the_guide_unticked_adds_an_account_and_skips_the_guide() {
    let mut engine = Engine::start("ca-e2e-learn-");
    let home = scratch("ca-e2e-learn-home-");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut pty = reach_accounts_step(&engine, home.path());
    pty.type_bytes(b" ");
    pty.until("Space unticks the guide", |screen| {
        screen.contains("[ ] Show the guide on Home")
    });
    pty.type_bytes(b"\r");
    pty.until("Add account opens", |screen| {
        screen.contains("Accounts  /  Add account") && !screen.contains("Show the guide on Home")
    });
    let start = Instant::now();
    while !guide_skipped(&runtime, &engine) {
        assert!(
            start.elapsed() < SCREEN_TIME,
            "the engine never held the guide as skipped"
        );
        std::thread::sleep(POLL_TIME);
    }
    pty.type_bytes(ESC_KEY);
    pty.until("Esc leaves Add account for Home", |screen| {
        screen.contains("New project") && !screen.contains("Accounts  /  Add account")
    });
    let quiet = Instant::now();
    while quiet.elapsed() < GUIDE_QUIET {
        let screen = pty.screen();
        assert!(
            !screen.contains("Learn CrazyAgent"),
            "a skipped guide stays off Home:\n{screen}"
        );
        std::thread::sleep(POLL_TIME);
    }
    drop(pty);
    engine.stop();
}

/// Reads the option the engine keeps for When a model provider can't be reached.
/// Input: the engine. Output: the stored option.
async fn stored_retry(engine: &Engine) -> crazyagent::provider_retry::ProviderRetry {
    let mut owner = Owner::attach(engine).await;
    let read = owner
        .controller
        .read(OwnerRead::ProviderRetry)
        .await
        .unwrap();
    owner.controller.detach();
    let OwnerData::ProviderRetry { settings } = read.data else {
        panic!("the engine answers with the provider retry setting")
    };
    settings.choice
}

/// Opens Settings, Setup through Go to, presses p and saves the option `keys` move to,
/// waiting until the page names it, and returns what the engine then keeps. Inputs: the
/// TUI, the runtime, the engine, the arrow keys and the option's title. Output: the
/// stored option.
fn save_retry(
    pty: &Pty,
    runtime: &tokio::runtime::Runtime,
    engine: &Engine,
    keys: &[&str],
    title: &str,
) -> crazyagent::provider_retry::ProviderRetry {
    pty.keys(&["p"]);
    pty.until(
        "p asks what harnesses do, with each harness line",
        |screen| {
            screen.contains("When a model provider can't be reached: what harnesses do.")
                && screen.contains("Harness default")
                && screen.contains("RCLI: one try. The engine never repeats an RCLI request.")
                && screen.contains("Claude Code: no retries.")
        },
    );
    pty.keys(keys);
    let picked = format!("> {title}");
    pty.until("the arrows pick the option", |screen| {
        screen.contains(&picked)
    });
    pty.keys(&["Enter"]);
    let said = format!("Provider retry: {title}.");
    pty.until(
        "the save names the option and the page shows it",
        |screen| {
            screen.contains(&said)
                && screen
                    .lines()
                    .any(|line| line.contains("Provider retry  ") && line.contains(title))
        },
    );
    runtime.block_on(stored_retry(engine))
}

/// One row per option of When a model provider can't be reached (DEC-163): the TUI opens
/// Settings, Setup, shows Retry briefly on a fresh engine and each harness line in its ask,
/// saves the option and the engine keeps it. Inputs: the scratch prefix, the arrow keys, the
/// option's title and the option. Output: none; panics when a step fails.
fn provider_retry_row(
    prefix: &str,
    keys: &[&str],
    title: &str,
    wanted: crazyagent::provider_retry::ProviderRetry,
) {
    use crazyagent::provider_retry::ProviderRetry;
    let (mut engine, pty, _project, _disk) = switch_settings(prefix);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        runtime.block_on(stored_retry(&engine)),
        ProviderRetry::RetryBriefly
    );
    pty.keys(&["C-k"]);
    pty.until("Ctrl-K opens Go to", |screen| {
        screen.contains("Projects, tabs, settings, actions. Enter goes there.")
    });
    pty.text("Settings / Setup");
    pty.keys(&["Enter"]);
    pty.until(
        "Settings, Setup shows Provider retry at Retry briefly",
        |screen| {
            screen
                .lines()
                .any(|line| line.contains("Provider retry  ") && line.contains("Retry briefly"))
        },
    );
    if title == "Retry briefly" {
        let stored = save_retry(&pty, &runtime, &engine, &["Up"], "Stop at once");
        assert_eq!(stored, ProviderRetry::StopAtOnce);
    }
    let stored = save_retry(&pty, &runtime, &engine, keys, title);
    assert_eq!(stored, wanted, "the engine keeps {title}");
    drop(pty);
    engine.stop();
}

#[test]
fn the_tui_saves_stop_at_once_for_a_model_provider_that_cant_be_reached() {
    provider_retry_row(
        "ca-e2e-retry-stop-",
        &["Up"],
        "Stop at once",
        crazyagent::provider_retry::ProviderRetry::StopAtOnce,
    );
}

#[test]
fn the_tui_saves_retry_briefly_for_a_model_provider_that_cant_be_reached() {
    provider_retry_row(
        "ca-e2e-retry-brief-",
        &["Down"],
        "Retry briefly",
        crazyagent::provider_retry::ProviderRetry::RetryBriefly,
    );
}

#[test]
fn the_tui_saves_harness_default_for_a_model_provider_that_cant_be_reached() {
    provider_retry_row(
        "ca-e2e-retry-default-",
        &["Down"],
        "Harness default",
        crazyagent::provider_retry::ProviderRetry::HarnessDefault,
    );
}
