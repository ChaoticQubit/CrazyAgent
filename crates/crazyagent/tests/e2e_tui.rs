#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::{OwnerData, OwnerRead, OwnerResult};
use crazyagent::master_session::{MasterReason, MasterTurnPhase, MasterTurnView};
use crazyagent::notifications::{
    Banners, Delivery, NoticeAction, NoticeKind, NoticeReport, NoticeSettings,
};
use crazyagent::protocol::ProjectId;
use crazyagent::questions::{self, Ask, EngineSource, QuestionKind};
use crazyagent::setup::{Dependency, DependencyState, PackState, SetupStatus};
use crazyagent::terminal::tmux::installed;
use e2e_support::{
    ENGINE, Engine, Owner, TMUX, hex, profile, repository, result, scratch, stop_tmux,
};
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

/// Whether the crumbs end on the project named exactly `project`, go on to one of its
/// tabs, or are followed by the header's right status, such as the notification counts,
/// which sits at least three spaces after the last crumb. Inputs: the screen and the
/// project name. Output: true when the crumbs name it.
fn in_project(screen: &str, project: &str) -> bool {
    let crumbs = screen.lines().next().unwrap_or_default().trim_end();
    let place = format!("/  {project}");
    crumbs.ends_with(&place)
        || crumbs.contains(&format!("{place}  /  "))
        || crumbs.contains(&format!("{place}   "))
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

const NOTICE_PROJECT: &str = "e2e-notice";

/// The header row of a captured screen, where the notification counts show.
/// Input: the screen. Output: its first line, empty for an empty capture.
fn header_row(screen: &str) -> &str {
    screen.lines().next().unwrap_or("")
}

/// Sends one change to the notification store as the owner, the way another client does
/// while the TUI runs, and expects it applied. Inputs: the runtime, the engine and the
/// change. Output: none; panics when the engine refuses it.
fn notice_change(runtime: &tokio::runtime::Runtime, engine: &Engine, action: NoticeAction) {
    runtime.block_on(async {
        let mut owner = Owner::attach(engine).await;
        let generation = owner.next();
        let response = owner
            .controller
            .notification_action(action, generation)
            .await
            .unwrap();
        owner.controller.detach();
        assert_eq!(result(response), OwnerResult::Applied);
    });
}

/// DEC-136a decision 10: the TUI header shows the notification store's unread and pending
/// counts over every project and reads them again on each refresh. The project is set up
/// with market research and no pool, so its research waits for a pool: that Action is the
/// first unread and pending item. A reported app update adds one unread, an open question
/// one unread and pending, and marking everything read from another client leaves the two
/// pending Actions.
#[test]
fn the_tui_header_counts_unread_and_pending_notifications_as_they_change() {
    let mut engine = Engine::start("ca-e2e-notice-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-notice-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(NOTICE_PROJECT, PROJECT).await,
            OwnerResult::Applied
        );
        owner.controller.detach();
    });

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, PROJECT));
    pty.until(
        "the header counts the research waiting for a pool",
        |screen| header_row(screen).contains("1 unread, 1 pending"),
    );

    notice_change(
        &runtime,
        &engine,
        NoticeAction::Report {
            report: NoticeReport::AppUpdateReady {
                version: "3.0.1".into(),
            },
        },
    );
    pty.until("the header counts the unread app update", |screen| {
        header_row(screen).contains("2 unread, 1 pending")
    });

    let db = questions::open(engine.path()).unwrap();
    questions::raise(
        &db,
        &ProjectId::new(NOTICE_PROJECT).unwrap(),
        &EngineSource::Planning,
        &Ask {
            question: "Which page ships first?".into(),
            kind: QuestionKind::Single,
            options: vec!["Shop".into(), "Blog".into()],
            recommended: Some("Shop".into()),
            context: None,
        },
    )
    .unwrap();
    drop(db);
    pty.until("the header counts the open question as pending", |screen| {
        header_row(screen).contains("3 unread, 2 pending")
    });

    notice_change(
        &runtime,
        &engine,
        NoticeAction::ReadAll { project_id: None },
    );
    pty.until("mark all read leaves the two pending Actions", |screen| {
        let header = header_row(screen);
        header.contains("2 pending") && !header.contains("unread")
    });
    drop(pty);
    engine.stop();
}

/// DEC-136a key n: n on Home opens the notifications list over every project, Esc goes
/// back, and Enter on the newest row, two open questions folded, opens that project's
/// Questions tab on the newest question rather than the first one listed and marks both
/// read, so the header drops two unread and keeps them pending.
#[test]
fn the_tui_n_key_lists_notifications_and_enter_opens_the_question_and_marks_it_read() {
    let mut engine = Engine::start("ca-e2e-notice-n-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-notice-n-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(NOTICE_PROJECT, PROJECT).await,
            OwnerResult::Applied
        );
        owner.controller.detach();
    });

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, PROJECT));
    pty.until("the research waiting for a pool is counted", |screen| {
        header_row(screen).contains("1 unread, 1 pending")
    });
    let db = questions::open(engine.path()).unwrap();
    questions::raise(
        &db,
        &ProjectId::new(NOTICE_PROJECT).unwrap(),
        &EngineSource::Planning,
        &Ask {
            question: "Which colour should the shop use?".into(),
            kind: QuestionKind::Single,
            options: vec!["Green".into(), "Blue".into()],
            recommended: Some("Green".into()),
            context: None,
        },
    )
    .unwrap();
    pty.until("the older question is counted", |screen| {
        header_row(screen).contains("2 unread, 2 pending")
    });
    questions::raise(
        &db,
        &ProjectId::new(NOTICE_PROJECT).unwrap(),
        &EngineSource::Planning,
        &Ask {
            question: "Which page ships first?".into(),
            kind: QuestionKind::Single,
            options: vec!["Shop".into(), "Blog".into()],
            recommended: Some("Shop".into()),
            context: None,
        },
    )
    .unwrap();
    drop(db);
    pty.until("the newer question is counted", |screen| {
        header_row(screen).contains("3 unread, 3 pending")
    });

    pty.keys(&["n"]);
    pty.until("n opens the notifications list", |screen| {
        header_row(screen).contains("Notifications")
            && screen.contains("Which page ships first?")
            && screen.contains("Waiting for a pool")
    });
    pty.keys(&["Escape"]);
    pty.until("Esc goes back to Home", |screen| at_home(screen, PROJECT));

    pty.keys(&["n"]);
    pty.until("the list opens again on the newest item", |screen| {
        header_row(screen).contains("Notifications") && screen.contains("Which page ships first?")
    });
    pty.keys(&["Enter"]);
    pty.until(
        "Enter opens Questions on the newest question, both read but still pending",
        |screen| {
            let header = header_row(screen);
            header.contains(&format!("/  {PROJECT}  /  Questions"))
                && header.contains("1 unread, 3 pending")
                && screen.contains("1. Shop (recommended)")
                && !screen.contains("1. Green (recommended)")
        },
    );
    drop(pty);
    engine.stop();
}

/// DEC-136a decision 4: the project Overview's Needs you is the store's pending items of
/// the project, newest first, with no inline memory card; Enter on the picked one opens
/// its page and marks it read, so the header drops one unread and keeps it pending.
#[test]
fn the_tui_overview_needs_you_lists_the_projects_pending_items_and_enter_opens_one() {
    let mut engine = Engine::start("ca-e2e-notice-needs-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-notice-needs-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(NOTICE_PROJECT, PROJECT).await,
            OwnerResult::Applied
        );
        owner.controller.detach();
    });

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, PROJECT));
    pty.until("the research waiting for a pool is counted", |screen| {
        header_row(screen).contains("1 unread, 1 pending")
    });
    let db = questions::open(engine.path()).unwrap();
    questions::raise(
        &db,
        &ProjectId::new(NOTICE_PROJECT).unwrap(),
        &EngineSource::Planning,
        &Ask {
            question: "Which page ships first?".into(),
            kind: QuestionKind::Single,
            options: vec!["Shop".into(), "Blog".into()],
            recommended: Some("Shop".into()),
            context: None,
        },
    )
    .unwrap();
    drop(db);
    pty.until(
        "the question and the research pool wait are counted",
        |screen| header_row(screen).contains("2 unread, 2 pending"),
    );

    tab_until_enter(&pty, "Open");
    pty.keys(&["Enter"]);
    pty.until(
        "the Overview lists what needs you from the store",
        |screen| {
            tab_shown(screen, PROJECT).as_deref() == Some("Overview")
                && screen.contains("Needs you  2")
                && screen.contains("Which page ships first?")
                && !screen.contains("Save to memory?")
        },
    );
    pty.keys(&["Enter"]);
    pty.until(
        "Enter opens the newest item on Questions, read but still pending",
        |screen| {
            let header = header_row(screen);
            header.contains(&format!("/  {PROJECT}  /  Questions"))
                && header.contains("1 unread, 2 pending")
                && screen.contains("1. Shop (recommended)")
        },
    );
    drop(pty);
    engine.stop();
}

/// DEC-136a slice 3: saves `seed` as the stored notification settings, opens Settings /
/// Notifications through Go to against that engine, sends `keys`, saves with Enter and
/// reads back what the engine holds. Inputs: the settings stored first and the keys.
/// Output: the settings after the save; panics when the page or the save never shows.
fn notice_settings_after(seed: NoticeSettings, keys: &[&str]) -> NoticeSettings {
    let mut engine = Engine::start("ca-e2e-notice-set-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-notice-set-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(NOTICE_PROJECT, PROJECT).await,
            OwnerResult::Applied
        );
        owner.controller.detach();
    });
    notice_change(
        &runtime,
        &engine,
        NoticeAction::SaveSettings { settings: seed },
    );

    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, PROJECT));
    pty.keys(&["C-k"]);
    pty.text("Settings / Notif");
    pty.keys(&["Enter"]);
    pty.until("Go to opens Settings / Notifications", |screen| {
        header_row(screen).contains("Settings  /  Notifications")
            && screen.contains("Action items")
            && screen.contains("What reaches you, and how long finished items stay.")
    });
    pty.keys(keys);
    pty.until("the pick waits for Enter", |screen| {
        screen.contains("Save changes")
    });
    pty.keys(&["Enter"]);
    pty.until("the save is confirmed", |screen| {
        screen.contains("Notification settings saved.")
    });
    let settings = runtime.block_on(async {
        let mut owner = Owner::attach(&engine).await;
        let settings = owner.controller.notification_settings().await.unwrap();
        owner.controller.detach();
        settings
    });
    drop(pty);
    engine.stop();
    settings
}

/// The default settings with one kind's delivery changed. Inputs: the kind and the
/// delivery. Output: the settings.
fn notice_kind(kind: NoticeKind, delivery: Delivery) -> NoticeSettings {
    let mut settings = NoticeSettings::default();
    for item in &mut settings.kinds {
        if item.kind == kind {
            item.delivery = delivery;
        }
    }
    settings
}

#[test]
fn the_tui_saves_an_action_kind_as_bell_only() {
    let saved = notice_settings_after(NoticeSettings::default(), &["Right"]);
    assert_eq!(saved, notice_kind(NoticeKind::Question, Delivery::BellOnly));
}

#[test]
fn the_tui_saves_an_action_kind_back_to_on() {
    let saved = notice_settings_after(
        notice_kind(NoticeKind::Question, Delivery::BellOnly),
        &["Right"],
    );
    assert_eq!(saved, NoticeSettings::default());
}

#[test]
fn the_tui_saves_an_info_kind_as_off() {
    let saved = notice_settings_after(
        NoticeSettings::default(),
        &["Down", "Down", "Down", "Down", "Down", "Right", "Right"],
    );
    assert_eq!(saved, notice_kind(NoticeKind::AgentFinished, Delivery::Off));
}

#[test]
fn the_tui_saves_macos_notifications_off() {
    let saved = notice_settings_after(NoticeSettings::default(), &["End", "Up", "Right"]);
    assert_eq!(saved.banners, Banners::Off);
}

#[test]
fn the_tui_saves_macos_notifications_for_action_items_only() {
    let saved = notice_settings_after(NoticeSettings::default(), &["End", "Up", "Right", "Right"]);
    assert_eq!(saved.banners, Banners::ActionOnly);
}

#[test]
fn the_tui_saves_macos_notifications_for_all_items() {
    let seed = NoticeSettings {
        banners: Banners::Off,
        ..NoticeSettings::default()
    };
    let saved = notice_settings_after(seed, &["End", "Up", "Right", "Right"]);
    assert_eq!(saved.banners, Banners::All);
}

#[test]
fn the_tui_saves_the_days_done_items_stay() {
    let saved = notice_settings_after(NoticeSettings::default(), &["End", "4", "5"]);
    assert_eq!(
        saved,
        NoticeSettings {
            retention_days: 45,
            ..NoticeSettings::default()
        }
    );
}

/// The Models page rows (DEC-150): each model choice option the TUI offers, on a real engine
/// whose PATH starts with stand-in `claude`, `opencode` and `crazyagent-rcli` files and whose
/// HOME is scratch, checked on screen and in the engine.
mod models {
    use super::*;
    use crazyagent::client_controller::OwnerController;
    use crazyagent::harness_settings::{Access, Source};
    use crazyagent::harnesses::finding::Found;
    use crazyagent::model_choice::rules::{FallbackRule, PickRule, Rule, RuleWritten, Rules};
    use crazyagent::model_choice::{Models, NOT_SUPPORTED, Origin, Row, Slot, Written};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;

    const ID: &str = "e2e-models";
    const PROJECT: &str = "Models e2e";
    const HARNESS: &str = "claude-code";
    const CLAUDE_CODE: &str = "Claude Code";
    const OPENCODE: &str = "OpenCode";
    const CODEX: &str = "Codex";
    const RCLI: &str = "rcli";
    const RCLI_NAME: &str = "RCLI";

    /// A TUI on Home with the engine it talks to, the runtime the owner reads run on and the
    /// scratch folders that hold the stand-ins, HOME and the projects home.
    struct Tui {
        engine: Engine,
        pty: Pty,
        runtime: tokio::runtime::Runtime,
        lists: PathBuf,
        _disk: tempfile::TempDir,
    }

    /// Writes a Codex stand-in that prints its version and, asked for its bundled model
    /// catalog, one listed and one hidden model, adding a line to `lists` each time.
    /// Inputs: the path and the count file.
    fn codex_stand_in(path: &Path, lists: &Path) {
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\nif [ \"$1 $2 $3\" = 'debug models --bundled' ]; then\n  echo run >> '{}'\n  echo '{{\"models\": [{{\"slug\": \"gpt-e2e-live\", \"visibility\": \"list\"}}, {{\"slug\": \"gpt-e2e-hidden\", \"visibility\": \"hide\"}}]}}'\n  exit 0\nfi\necho 'codex-cli 0.167.1'\n",
                lists.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// How many times the Codex stand-in printed its model list. Input: the count file.
    /// Output: the count, 0 before the first.
    fn list_runs(lists: &Path) -> usize {
        std::fs::read_to_string(lists).map_or(0, |text| text.lines().count())
    }

    /// The Claude Code versions the shipped model catalog lists; the stand-in answers the
    /// first, so it reads as checked whichever versions the catalog qualifies.
    fn qualified_versions() -> Vec<String> {
        crazyagent::model_choice::shipped(HARNESS)
            .expect("Claude Code ships a model catalog")
            .qualified_versions
    }

    fn project_id() -> ProjectId {
        ProjectId::new(ID).unwrap()
    }

    fn crumbs(screen: &str) -> &str {
        screen.lines().next().unwrap_or_default().trim_end()
    }

    fn stand_in(path: &Path, version: &str) {
        std::fs::write(path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn row(models: &Models, slot: Slot) -> Row {
        models
            .rows
            .iter()
            .find(|row| row.slot == slot)
            .cloned()
            .expect("the engine lists every slot")
    }

    fn own(models: &Models) -> Vec<(String, Origin)> {
        models
            .candidates
            .iter()
            .filter(|candidate| candidate.origin != Origin::Catalog)
            .map(|candidate| (candidate.id.clone(), candidate.origin))
            .collect()
    }

    impl Tui {
        /// Starts the engine with the stand-ins in the test HOME's `~/.local/bin`, a place the
        /// engine keeps, first on its PATH so the harness pick takes them, in a scratch folder
        /// outside the agents' `ca-` temp names; saves a profile, a projects home and one
        /// project, and opens the TUI on a settled Home. Input: a scratch prefix. Output: the TUI.
        fn start(prefix: &str) -> Self {
            let disk = scratch("e2e-models-");
            let home = disk.path().join("home");
            let bin = home.join(".local/bin");
            let projects = disk.path().join("Projects");
            for folder in [&bin, &projects] {
                std::fs::create_dir_all(folder).unwrap();
            }
            stand_in(
                &bin.join("claude"),
                &format!("{} (Claude Code)", qualified_versions()[0]),
            );
            stand_in(&bin.join("opencode"), "1.0.0");
            let lists = disk.path().join("codex-lists");
            codex_stand_in(&bin.join("codex"), &lists);
            stand_in(&bin.join("crazyagent-rcli"), "3.0.4");
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let mut engine = Engine::new(prefix);
            engine.home = Some(home);
            engine.env_path = Some(
                std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))
                    .unwrap(),
            );
            engine.launch();
            let tmux = installed(engine.path()).expect(TMUX);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut owner = Owner::attach(&engine).await;
                assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
                assert_eq!(owner.set_home(&projects).await, OwnerResult::Applied);
                assert_eq!(
                    owner.create_project(ID, PROJECT).await,
                    OwnerResult::Applied
                );
                owner.controller.detach();
            });
            let pty = Pty::launch(tmux, &engine.socket());
            reach_home(&pty, |screen| at_home(screen, PROJECT));
            setup_checked(&pty);
            Self {
                engine,
                pty,
                runtime,
                lists,
                _disk: disk,
            }
        }

        /// Runs one owner request through the real owner socket. Input: the request on a
        /// fresh owner connection. Output: its answer.
        fn owner<T>(
            &self,
            request: impl for<'a> FnOnce(
                &'a OwnerController,
            ) -> std::pin::Pin<Box<dyn Future<Output = T> + 'a>>,
        ) -> T {
            self.runtime.block_on(async {
                let mut owner = Owner::attach(&self.engine).await;
                let answer = request(&owner.controller).await;
                owner.controller.detach();
                answer
            })
        }

        fn models(&self, harness: &'static str, project: bool) -> Models {
            self.owner(move |controller| {
                Box::pin(async move {
                    controller
                        .model_choice_values(harness, project.then(project_id), false)
                        .await
                        .unwrap()
                })
            })
        }

        fn rules(&self) -> Rules {
            self.owner(|controller| {
                Box::pin(async move { controller.model_choice_rules().await.unwrap() })
            })
        }

        /// Seeds one Claude Code slot through the owner protocol, naming the version held.
        /// Inputs: whether it is the project's, the slot and the value.
        fn set(&self, project: bool, slot: Slot, value: Value) {
            self.set_on(HARNESS, project, slot, value);
        }

        /// Seeds one slot of one harness through the owner protocol, naming the version
        /// held. Inputs: the harness id, whether it is the project's, the slot and the value.
        fn set_on(&self, harness: &'static str, project: bool, slot: Slot, value: Value) {
            let version = row(&self.models(harness, project), slot).version;
            let written = self.owner(move |controller| {
                Box::pin(async move {
                    controller
                        .model_choice_set(harness, project.then(project_id), slot, value, version)
                        .await
                        .unwrap()
                })
            });
            assert!(matches!(written, Written::Saved { .. }), "{written:?}");
            assert!(
                row(&self.models(harness, project), slot).here.is_some(),
                "the engine holds the seeded {slot:?}"
            );
        }

        fn add(&self, model: &'static str) {
            self.owner(move |controller| {
                Box::pin(async move {
                    controller
                        .model_choice_add(HARNESS, model, None)
                        .await
                        .unwrap()
                })
            });
        }

        fn set_rule(&self, rule: Rule) {
            let rules = self.rules();
            let version = match rule {
                Rule::Pick(_) => rules.pick_version,
                Rule::Fallback(_) => rules.fallback_version,
            };
            let written = self.owner(move |controller| {
                Box::pin(async move {
                    controller
                        .model_choice_set_rule(rule, version)
                        .await
                        .unwrap()
                })
            });
            assert!(matches!(written, RuleWritten::Saved { .. }), "{written:?}");
        }

        /// Opens Settings > Harnesses, then one harness's page and its Models page with m.
        /// Inputs: the harness name and the Models page title.
        fn app_page(&self, name: &str, title: &str) {
            let pty = &self.pty;
            pty.keys(&["s"]);
            pty.until("s on Home opens Settings", |screen| {
                crumbs(screen).contains("Settings")
            });
            for step in 0..=STEPS_MAX {
                let screen = pty.screen();
                if crumbs(&screen).ends_with("Settings  /  Harnesses") {
                    break;
                }
                assert!(
                    step < STEPS_MAX,
                    "no Settings section is Harnesses:\n{screen}"
                );
                pty.bytes(&SHIFT_RIGHT);
                pty.until("the next Settings section shows", |now| {
                    crumbs(now) != crumbs(&screen)
                });
            }
            self.harness_models(name, title);
        }

        /// Opens the project's Settings tab, its harness list with h, Claude Code's page and
        /// its Models page with m.
        fn project_page(&self) {
            let pty = &self.pty;
            tab_until_enter(pty, "Open");
            pick_card(pty, PROJECT);
            pty.keys(&["Enter"]);
            pty.until("the project screen opens with its tabs", |screen| {
                screen.contains("Tabs")
            });
            to_tab(pty, PROJECT, "Settings", &SHIFT_RIGHT);
            pty.keys(&["h"]);
            pty.until(
                "h on the Settings tab opens the project's harness list",
                |screen| crumbs(screen).ends_with(&format!("{PROJECT}  /  Harnesses")),
            );
            self.harness_models(CLAUDE_CODE, &format!("{CLAUDE_CODE} models for {PROJECT}"));
        }

        fn harness_models(&self, name: &str, title: &str) {
            let pty = &self.pty;
            let picked = format!("> {name} ");
            for step in 0..=STEPS_MAX {
                let screen = pty.until("the harness list shows its rows", |screen| {
                    screen.contains(name)
                });
                if screen
                    .lines()
                    .any(|line| line.trim_start().starts_with(&picked))
                {
                    break;
                }
                assert!(step < STEPS_MAX, "no harness row is {name}:\n{screen}");
                pty.keys(&["Down"]);
                pty.until("the pick moves", |now| now != screen);
            }
            pty.keys(&["Enter"]);
            pty.until("Enter opens the harness page", |screen| {
                crumbs(screen).ends_with(&format!("Harnesses  /  {name}"))
            });
            pty.keys(&["m"]);
            pty.until("m on the harness page opens its Models page", |screen| {
                screen.contains(title) && screen.contains("Master model")
            });
        }

        fn says(&self, words: &str) -> String {
            self.pty.until(&format!("the TUI says {words}"), |screen| {
                screen.contains(words)
            })
        }

        fn finish(self) {
            let Self {
                mut engine, pty, ..
            } = self;
            drop(pty);
            engine.stop();
        }
    }

    /// The engine pins the Codex and Claude Code stand-ins of the test HOME at start, never
    /// an install found on this machine, so no Models row runs a real harness with the test
    /// HOME (V3SI-S1, STD-M1).
    #[test]
    fn the_models_rows_pin_their_stand_ins_and_never_a_real_install() {
        let tui = Tui::start("ca-e2e-m00-");
        let bin = tui
            .engine
            .home
            .clone()
            .expect("the Models rows set HOME")
            .join(".local/bin");
        for (id, file) in [("codex", "codex"), (HARNESS, "claude")] {
            let found = tui.owner(move |controller| {
                Box::pin(async move { controller.harness_find(id).await.unwrap() })
            });
            let stand_in = bin.join(file);
            assert!(
                matches!(
                    &found,
                    Found::Registered { path } | Found::Ready { path } if *path == stand_in
                ),
                "{id} must pin {stand_in:?}, found {found:?}"
            );
        }
        tui.finish();
    }

    #[test]
    fn the_tui_models_page_saves_a_master_model_app_wide() {
        let tui = Tui::start("ca-e2e-m01-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        let screen = tui.says(&format!(
            "Checked against {}",
            qualified_versions().join(", ")
        ));
        assert!(screen.contains("Account's own model"), "{screen}");
        tui.pty.keys(&["Enter", "Right", "Enter"]);
        tui.says("Saved.");
        let master = row(&tui.models(HARNESS, false), Slot::Master);
        assert_eq!(
            (master.here, master.effective, master.source),
            (Some(json!("opus")), json!("opus"), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn x_on_the_master_model_clears_it_in_the_tui() {
        let tui = Tui::start("ca-e2e-m02-");
        tui.set(false, Slot::Master, json!("opus"));
        let before = row(&tui.models(HARNESS, false), Slot::Master).version;
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let master = row(&tui.models(HARNESS, false), Slot::Master);
        assert_eq!((master.here, master.effective), (None, Value::Null));
        assert!(master.version > before, "the reset is a new version");
        tui.finish();
    }

    #[test]
    fn the_tui_saves_an_agent_default_model() {
        let tui = Tui::start("ca-e2e-m03-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["Down", "Enter", "Right", "Right", "Enter"]);
        tui.says("Saved.");
        let agent = row(&tui.models(HARNESS, false), Slot::Agent);
        assert_eq!(
            (agent.here, agent.source),
            (Some(json!("sonnet")), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn choosing_the_account_s_own_model_clears_the_agent_default_in_the_tui() {
        let tui = Tui::start("ca-e2e-m04-");
        tui.set(false, Slot::Agent, json!("sonnet"));
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["Down", "Enter", "Left", "Left", "Enter"]);
        tui.says("Saved.");
        let agent = row(&tui.models(HARNESS, false), Slot::Agent);
        assert_eq!(
            (agent.here, agent.effective, agent.source),
            (None, Value::Null, Source::Default)
        );
        tui.finish();
    }

    #[test]
    fn ticking_allowed_models_saves_the_list_in_page_order_in_the_tui() {
        let tui = Tui::start("ca-e2e-m05-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["Down", "Down", "Enter"]);
        tui.says("[ ] opus");
        tui.pty.keys(&["Down", "Space", "Up", "Space", "Enter"]);
        tui.says("Saved.");
        let allowed = row(&tui.models(HARNESS, false), Slot::Allowed);
        assert_eq!(allowed.here, Some(json!(["opus", "sonnet"])));
        tui.finish();
    }

    #[test]
    fn unticking_an_allowed_model_saves_the_shorter_list_in_the_tui() {
        let tui = Tui::start("ca-e2e-m06-");
        tui.set(false, Slot::Allowed, json!(["opus", "sonnet"]));
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["Down", "Down", "Enter"]);
        tui.says("[x] opus");
        tui.pty.keys(&["Space", "Enter"]);
        tui.says("Saved.");
        let allowed = row(&tui.models(HARNESS, false), Slot::Allowed);
        assert_eq!(allowed.here, Some(json!(["sonnet"])));
        tui.finish();
    }

    #[test]
    fn x_on_the_allowed_list_clears_it_in_the_tui() {
        let tui = Tui::start("ca-e2e-m07-");
        tui.set(false, Slot::Allowed, json!(["opus"]));
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty.keys(&["Down", "Down", "x"]);
        tui.says("Saved.");
        let allowed = row(&tui.models(HARNESS, false), Slot::Allowed);
        assert_eq!((allowed.here, allowed.effective), (None, json!([])));
        tui.finish();
    }

    #[test]
    fn typing_an_own_model_id_adds_it_waiting_for_its_test_launch_in_the_tui() {
        let tui = Tui::start("ca-e2e-m08-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty
            .keys(&["Down", "Down", "Down", "Down", "Down", "Down", "Enter"]);
        tui.says("Model id");
        tui.pty.text("my-model-1");
        tui.pty.keys(&["Enter"]);
        let screen = tui.says("Added my-model-1.");
        assert!(screen.contains("Waiting for its test launch"), "{screen}");
        assert_eq!(
            own(&tui.models(HARNESS, false)),
            vec![("my-model-1".to_owned(), Origin::Pending)]
        );
        tui.finish();
    }

    #[test]
    fn adding_an_id_the_model_list_has_adds_nothing_in_the_tui() {
        let tui = Tui::start("ca-e2e-m09-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty
            .keys(&["Down", "Down", "Down", "Down", "Down", "Down", "Enter"]);
        tui.says("Model id");
        tui.pty.text("opus");
        tui.pty.keys(&["Enter"]);
        tui.says("opus is already on the Claude Code model list.");
        assert_eq!(own(&tui.models(HARNESS, false)), vec![]);
        tui.finish();
    }

    #[test]
    fn an_id_with_characters_a_model_id_cannot_have_never_leaves_the_tui() {
        let tui = Tui::start("ca-e2e-m10-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty
            .keys(&["Down", "Down", "Down", "Down", "Down", "Down", "Enter"]);
        tui.says("Model id");
        tui.pty.text("bad id!");
        tui.pty.keys(&["Enter"]);
        tui.says("Use letters, digits and - _ . : / @ +");
        assert_eq!(own(&tui.models(HARNESS, false)), vec![]);
        tui.finish();
    }

    #[test]
    fn x_on_an_own_model_id_removes_it_in_the_tui() {
        let tui = Tui::start("ca-e2e-m11-");
        tui.add("my-model-1");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        tui.pty
            .keys(&["Down", "Down", "Down", "Down", "Down", "Down", "Down", "x"]);
        tui.says("Removed my-model-1.");
        assert_eq!(own(&tui.models(HARNESS, false)), vec![]);
        tui.finish();
    }

    /// Picks one rule option from the app Models page and checks the engine holds it as a
    /// new version. Inputs: the TUI, the rule row's place, the keys and the rule expected.
    fn rule_row(tui: &Tui, downs: usize, keys: &[&str], expected: Rule) {
        let before = tui.rules();
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        for _ in 0..downs {
            tui.pty.keys(&["Down"]);
        }
        tui.pty.keys(&["Enter"]);
        tui.pty.keys(keys);
        tui.pty.keys(&["Enter"]);
        tui.says("Saved.");
        let after = tui.rules();
        match expected {
            Rule::Pick(pick) => {
                assert_eq!(after.pick, pick);
                assert!(after.pick_version > before.pick_version);
            }
            Rule::Fallback(fallback) => {
                assert_eq!(after.fallback, fallback);
                assert!(after.fallback_version > before.fallback_version);
            }
        }
    }

    #[test]
    fn the_tui_pick_rule_any_offered_model_saves_hint() {
        let tui = Tui::start("ca-e2e-m12-");
        rule_row(&tui, 4, &["Right"], Rule::Pick(PickRule::Hint));
        tui.finish();
    }

    #[test]
    fn the_tui_pick_rule_always_the_default_saves_default() {
        let tui = Tui::start("ca-e2e-m13-");
        rule_row(&tui, 4, &["Right", "Right"], Rule::Pick(PickRule::Default));
        tui.finish();
    }

    #[test]
    fn the_tui_pick_rule_only_the_allowed_list_saves_strict() {
        let tui = Tui::start("ca-e2e-m14-");
        tui.set_rule(Rule::Pick(PickRule::Hint));
        rule_row(&tui, 4, &["Left"], Rule::Pick(PickRule::Strict));
        tui.finish();
    }

    #[test]
    fn the_tui_fallback_rule_stop_and_say_why_saves_stop() {
        let tui = Tui::start("ca-e2e-m15-");
        rule_row(&tui, 5, &["Right"], Rule::Fallback(FallbackRule::Stop));
        tui.finish();
    }

    #[test]
    fn the_tui_fallback_rule_quietly_saves_silent() {
        let tui = Tui::start("ca-e2e-m16-");
        rule_row(
            &tui,
            5,
            &["Right", "Right"],
            Rule::Fallback(FallbackRule::Silent),
        );
        tui.finish();
    }

    #[test]
    fn the_tui_fallback_rule_tell_me_saves_notice() {
        let tui = Tui::start("ca-e2e-m17-");
        tui.set_rule(Rule::Fallback(FallbackRule::Stop));
        rule_row(&tui, 5, &["Left"], Rule::Fallback(FallbackRule::Notice));
        tui.finish();
    }

    #[test]
    fn a_harness_whose_model_crazyagent_does_not_choose_shows_the_reason_in_the_tui() {
        let tui = Tui::start("ca-e2e-m18-");
        let before = tui.models("opencode", false);
        for slot in Slot::ALL {
            assert_eq!(
                row(&before, slot).access,
                Access::ReadOnly {
                    reason: NOT_SUPPORTED.into()
                }
            );
        }
        tui.app_page(OPENCODE, "OpenCode models");
        let screen = tui.says("No model list");
        assert!(
            screen.matches(NOT_SUPPORTED).count() >= 4,
            "each slot gives the reason:\n{screen}"
        );
        tui.pty.keys(&["Enter"]);
        tui.says("CrazyAgent does not choose this model here.");
        assert_eq!(tui.models("opencode", false), before);
        tui.finish();
    }

    #[test]
    fn a_project_with_nothing_set_uses_the_app_value_in_the_tui() {
        let tui = Tui::start("ca-e2e-m19-");
        tui.set(false, Slot::Master, json!("opus"));
        tui.project_page();
        let screen = tui.says("Claude Code models for Models e2e");
        assert!(
            !screen.contains("Pick rule") && !screen.contains("Add a model id"),
            "the rules and own ids are app-wide:\n{screen}"
        );
        tui.pty.keys(&["Enter"]);
        tui.says("Use the app value");
        tui.pty.keys(&["Escape"]);
        let master = row(&tui.models(HARNESS, true), Slot::Master);
        assert_eq!(
            (master.here, master.effective, master.source),
            (None, json!("opus"), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn choosing_a_model_for_the_project_overrides_the_app_value_there_only_in_the_tui() {
        let tui = Tui::start("ca-e2e-m20-");
        tui.set(false, Slot::Master, json!("opus"));
        tui.project_page();
        tui.pty.keys(&["Enter", "Right", "Right", "Enter"]);
        tui.says("Saved.");
        let master = row(&tui.models(HARNESS, true), Slot::Master);
        assert_eq!(
            (master.here, master.source),
            (Some(json!("sonnet")), Source::Project)
        );
        let app = row(&tui.models(HARNESS, false), Slot::Master);
        assert_eq!(app.here, Some(json!("opus")), "the app value stays");
        tui.finish();
    }

    #[test]
    fn unticking_every_allowed_model_saves_an_own_empty_project_list_in_the_tui() {
        let tui = Tui::start("ca-e2e-m21-");
        tui.set(false, Slot::Allowed, json!(["opus"]));
        tui.project_page();
        tui.pty.keys(&["Down", "Down", "Enter"]);
        tui.says("[x] opus");
        tui.pty.keys(&["Space", "Enter"]);
        tui.says("Saved.");
        let allowed = row(&tui.models(HARNESS, true), Slot::Allowed);
        assert_eq!(
            (allowed.here, allowed.effective, allowed.source),
            (Some(json!([])), json!([]), Source::Project)
        );
        let app = row(&tui.models(HARNESS, false), Slot::Allowed);
        assert_eq!(app.here, Some(json!(["opus"])), "the app list stays");
        tui.finish();
    }

    #[test]
    fn x_on_a_project_value_goes_back_to_the_app_value_in_the_tui() {
        let tui = Tui::start("ca-e2e-m22-");
        tui.set(false, Slot::Master, json!("opus"));
        tui.set(true, Slot::Master, json!("sonnet"));
        tui.project_page();
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let master = row(&tui.models(HARNESS, true), Slot::Master);
        assert_eq!(
            (master.here, master.effective, master.source),
            (None, json!("opus"), Source::App)
        );
        tui.finish();
    }

    /// Moves from the first row of a Models page to the Native sub-agent model, the fourth
    /// slot. Input: the TUI.
    fn to_native(tui: &Tui) {
        tui.pty.keys(&["Down", "Down", "Down"]);
        tui.says("What the sub-agents a native lead starts itself run on.");
    }

    #[test]
    fn the_tui_native_sub_agent_model_let_the_harness_choose_saves_harness_for_claude_code() {
        let tui = Tui::start("ca-e2e-m23-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        to_native(&tui);
        tui.says("Nothing set: native sub-agents follow the agent's model.");
        let before = row(&tui.models(HARNESS, false), Slot::Native).version;
        tui.pty.keys(&["Enter", "Right"]);
        tui.says("The harness picks each sub-agent's model");
        tui.pty.keys(&["Enter"]);
        tui.says("Saved.");
        let native = row(&tui.models(HARNESS, false), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (Some(json!("harness")), json!("harness"), Source::App)
        );
        assert!(native.version > before, "the choice is a new version");
        tui.finish();
    }

    #[test]
    fn the_tui_native_sub_agent_model_follow_the_agent_s_model_saves_follow_for_claude_code() {
        let tui = Tui::start("ca-e2e-m24-");
        tui.set(false, Slot::Native, json!("harness"));
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        to_native(&tui);
        tui.pty.keys(&["Enter", "Right"]);
        tui.says("Every sub-agent a native lead starts runs on the lead's model.");
        tui.pty.keys(&["Enter"]);
        tui.says("Saved.");
        let native = row(&tui.models(HARNESS, false), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (Some(json!("follow")), json!("follow"), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn x_on_the_native_sub_agent_model_goes_back_to_the_default_for_claude_code() {
        let tui = Tui::start("ca-e2e-m25-");
        tui.set(false, Slot::Native, json!("harness"));
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        to_native(&tui);
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let native = row(&tui.models(HARNESS, false), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (None, json!("follow"), Source::Default)
        );
        tui.finish();
    }

    #[test]
    fn the_tui_native_sub_agent_model_let_the_harness_choose_saves_harness_for_codex() {
        let tui = Tui::start("ca-e2e-m26-");
        tui.app_page(CODEX, "Codex models");
        to_native(&tui);
        tui.says("Follow the agent's model");
        tui.pty.keys(&["Enter", "Right", "Enter"]);
        tui.says("Saved.");
        let native = row(&tui.models("codex", false), Slot::Native);
        assert_eq!(
            (native.here, native.source),
            (Some(json!("harness")), Source::App)
        );
        let claude = row(&tui.models(HARNESS, false), Slot::Native);
        assert_eq!(claude.here, None, "Claude Code keeps its own value");
        tui.finish();
    }

    #[test]
    fn the_tui_native_sub_agent_model_follow_the_agent_s_model_saves_follow_for_codex() {
        let tui = Tui::start("ca-e2e-m27-");
        tui.set_on("codex", false, Slot::Native, json!("harness"));
        tui.app_page(CODEX, "Codex models");
        to_native(&tui);
        tui.pty.keys(&["Enter", "Right", "Enter"]);
        tui.says("Saved.");
        let native = row(&tui.models("codex", false), Slot::Native);
        assert_eq!(
            (native.here, native.source),
            (Some(json!("follow")), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn x_on_the_native_sub_agent_model_goes_back_to_the_default_for_codex() {
        let tui = Tui::start("ca-e2e-m28-");
        tui.set_on("codex", false, Slot::Native, json!("harness"));
        tui.app_page(CODEX, "Codex models");
        to_native(&tui);
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let native = row(&tui.models("codex", false), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (None, json!("follow"), Source::Default)
        );
        tui.finish();
    }

    #[test]
    fn rcli_shows_the_native_sub_agent_model_not_supported_in_the_tui() {
        let tui = Tui::start("ca-e2e-m29-");
        let before = tui.models("rcli", false);
        assert_eq!(
            row(&before, Slot::Native).access,
            Access::ReadOnly {
                reason: NOT_SUPPORTED.into()
            }
        );
        tui.app_page(RCLI_NAME, "RCLI models");
        to_native(&tui);
        let screen = tui.says(NOT_SUPPORTED);
        assert!(
            !screen.contains("Follow the agent's model"),
            "no option shows for RCLI:\n{screen}"
        );
        tui.pty.keys(&["Enter"]);
        tui.says("CrazyAgent does not choose this model here.");
        assert_eq!(tui.models("rcli", false), before);
        tui.finish();
    }

    #[test]
    fn follow_for_a_project_overrides_the_app_native_sub_agent_model_in_the_tui() {
        let tui = Tui::start("ca-e2e-m30-");
        tui.set(false, Slot::Native, json!("harness"));
        tui.project_page();
        to_native(&tui);
        tui.says("Uses the app value: Let the harness choose.");
        tui.pty.keys(&["Enter", "Right", "Enter"]);
        tui.says("Saved.");
        let native = row(&tui.models(HARNESS, true), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (Some(json!("follow")), json!("follow"), Source::Project)
        );
        let app = row(&tui.models(HARNESS, false), Slot::Native);
        assert_eq!(app.here, Some(json!("harness")), "the app value stays");
        tui.finish();
    }

    #[test]
    fn x_on_a_project_s_native_sub_agent_model_goes_back_to_the_app_value_in_the_tui() {
        let tui = Tui::start("ca-e2e-m31-");
        tui.set(false, Slot::Native, json!("harness"));
        tui.set(true, Slot::Native, json!("follow"));
        tui.project_page();
        to_native(&tui);
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let native = row(&tui.models(HARNESS, true), Slot::Native);
        assert_eq!(
            (native.here, native.effective, native.source),
            (None, json!("harness"), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn the_codex_models_page_lists_what_codex_reports_and_reads_it_once_per_open_in_the_tui() {
        let tui = Tui::start("ca-e2e-m23-");
        assert_eq!(
            list_runs(&tui.lists),
            0,
            "nothing reads the list before the page opens"
        );
        tui.app_page("Codex", "Codex models");
        let screen = tui.says("Master model");
        assert!(
            !screen.contains("live list not reported by Codex"),
            "{screen}"
        );
        assert_eq!(list_runs(&tui.lists), 1, "one read for the open");
        let models = tui.models("codex", false);
        let ids: Vec<&str> = models
            .candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect();
        assert!(ids.contains(&"gpt-e2e-live"), "{ids:?}");
        assert!(!ids.contains(&"gpt-e2e-hidden"), "{ids:?}");
        assert_eq!(models.live_note, None);
        assert_eq!(
            list_runs(&tui.lists),
            1,
            "a read that is not an open starts no harness"
        );
        tui.pty.keys(&["r"]);
        tui.pty
            .until("r reads the list again", |_| list_runs(&tui.lists) == 2);
        tui.finish();
    }

    #[test]
    fn a_harness_with_no_list_command_says_live_list_not_reported_in_the_tui() {
        let tui = Tui::start("ca-e2e-m24-");
        tui.app_page(CLAUDE_CODE, "Claude Code models");
        let screen = tui.says("live list not reported by Claude Code.");
        assert!(
            screen.contains("Checked against"),
            "the shipped list stays: {screen}"
        );
        tui.finish();
    }

    /// Picks one RCLI catalog id as the master model on the TUI's RCLI Models page, with
    /// Right once per choice before it, and checks the engine saved it app-wide (DEC-150
    /// M-5). Inputs: the scratch prefix, the provider the catalog names for it and the id.
    fn rcli_master_saves(prefix: &str, provider: &str, model: &str) {
        let catalog = crazyagent::model_choice::shipped(RCLI).expect("RCLI ships a model catalog");
        assert_eq!(
            catalog.providers.get(model).map(String::as_str),
            Some(provider)
        );
        let at = catalog
            .models
            .iter()
            .position(|id| id == model)
            .expect("the RCLI catalog lists the id");
        let tui = Tui::start(prefix);
        tui.app_page(RCLI_NAME, "RCLI models");
        let mut keys = vec!["Enter"];
        keys.extend(std::iter::repeat_n("Right", at + 1));
        keys.push("Enter");
        tui.pty.keys(&keys);
        tui.says("Saved.");
        let master = row(&tui.models(RCLI, false), Slot::Master);
        assert_eq!(
            (master.here, master.effective, master.source),
            (Some(json!(model)), json!(model), Source::App)
        );
        tui.finish();
    }

    #[test]
    fn the_tui_rcli_models_page_offers_only_the_master_model_on_each_account_s_own() {
        let tui = Tui::start("ca-e2e-m23-");
        let models = tui.models(RCLI, false);
        let master = row(&models, Slot::Master);
        assert_eq!(
            (master.here, master.effective, master.access),
            (None, Value::Null, Access::Editable)
        );
        for slot in [Slot::Agent, Slot::Allowed] {
            assert_eq!(
                row(&models, slot).access,
                Access::ReadOnly {
                    reason: NOT_SUPPORTED.into()
                }
            );
        }
        tui.app_page(RCLI_NAME, "RCLI models");
        let versions = crazyagent::model_choice::shipped(RCLI)
            .expect("RCLI ships a model catalog")
            .qualified_versions
            .join(", ");
        let screen = tui.says(&format!("Checked against {versions}"));
        assert!(screen.contains("Account's own model"), "{screen}");
        assert!(
            screen.matches(NOT_SUPPORTED).count() >= 2,
            "the agent default and the allowed list give the reason:\n{screen}"
        );
        tui.finish();
    }

    #[test]
    fn an_anthropic_model_saves_as_the_rcli_master_model_in_the_tui() {
        rcli_master_saves("ca-e2e-m24-", "anthropic", "claude-opus-5-5");
    }

    #[test]
    fn an_openai_model_saves_as_the_rcli_master_model_in_the_tui() {
        rcli_master_saves("ca-e2e-m25-", "openai", "gpt-6.1-sol");
    }

    #[test]
    fn an_openrouter_model_saves_as_the_rcli_master_model_in_the_tui() {
        rcli_master_saves("ca-e2e-m26-", "openrouter", "anthropic/claude-opus-5.5");
    }

    #[test]
    fn x_on_the_rcli_master_model_goes_back_to_each_account_s_own_model_in_the_tui() {
        let tui = Tui::start("ca-e2e-m27-");
        let version = row(&tui.models(RCLI, false), Slot::Master).version;
        let written = tui.owner(move |controller| {
            Box::pin(async move {
                controller
                    .model_choice_set(RCLI, None, Slot::Master, json!("claude-opus-5-5"), version)
                    .await
                    .unwrap()
            })
        });
        assert!(matches!(written, Written::Saved { .. }), "{written:?}");
        let before = row(&tui.models(RCLI, false), Slot::Master).version;
        tui.app_page(RCLI_NAME, "RCLI models");
        tui.pty.keys(&["x"]);
        tui.says("Saved.");
        let master = row(&tui.models(RCLI, false), Slot::Master);
        assert_eq!((master.here, master.effective), (None, Value::Null));
        assert!(master.version > before, "the reset is a new version");
        tui.finish();
    }
}

/// Starts an engine with a saved profile and projects folder, and the TUI on it at Home.
/// Input: the scratch prefix. Output: the engine, the TUI and the projects folder's parent,
/// which the caller keeps until the test ends.
fn tui_at_home(prefix: &str) -> (Engine, Pty, tempfile::TempDir) {
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
            owner.controller.detach();
        });
    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| screen.contains("New project"));
    (engine, pty, disk)
}

/// Opens a place through Go to: Ctrl-K, the query, then Enter once Go to lists the place
/// first. Inputs: the TUI past first run, the query and the place's Go to label. Output:
/// none; panics when Go to never opens or never lists the place first.
fn go_to(pty: &Pty, query: &str, label: &str) {
    pty.keys(&["C-k"]);
    pty.until("Ctrl-K opens Go to", |screen| {
        screen.contains("Enter Go") && screen.contains("Esc Close")
    });
    pty.text(query);
    let first = format!("> {label}");
    pty.until("Go to lists the place first", |screen| {
        screen.contains(&first)
    });
    pty.keys(&["Enter"]);
    pty.until("Go to closes", |screen| !screen.contains("Esc Close"));
}

/// DEC-155 step 3: Settings, Checks opens from Go to on a real engine and reads as the
/// desktop's: its purpose, the Check rules section with when a change applies, and its keys.
#[test]
fn settings_checks_opens_from_go_to_with_the_desktop_words() {
    let (mut engine, pty, _home) = tui_at_home("ca-e2e-checks-");
    go_to(&pty, "Settings / Checks", "Settings / Checks");
    let screen = pty.until("Settings, Checks reads its rules", |screen| {
        screen.contains("Accept gate")
    });
    for words in [
        "The check settings every project starts from",
        "Check rules",
        "Changes apply to check runs queued after you save and to the next Accept.",
        "Enter Change",
        "Esc Back",
    ] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    assert!(!screen.contains("Not ready yet"), "{screen}");
    drop(pty);
    engine.stop();
}

/// DEC-155 step 3: Settings, Tickets opens from Go to on a real engine and reads as the
/// desktop's: its purpose, the Ticket rules section with when a change applies, and its keys.
#[test]
fn settings_tickets_opens_from_go_to_with_the_desktop_words() {
    let (mut engine, pty, _home) = tui_at_home("ca-e2e-tickets-");
    go_to(&pty, "Settings / Tickets", "Settings / Tickets");
    let screen = pty.until("Settings, Tickets reads its rules", |screen| {
        screen.contains("Who closes tickets")
    });
    for words in [
        "The ticket settings every project with tickets starts from",
        "Ticket rules",
        "Changes apply to the next ticket action. Tickets already closed stay closed.",
        "Enter Change",
        "Esc Back",
    ] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    assert!(
        !screen.contains("Could not read the ticket settings"),
        "{screen}"
    );
    drop(pty);
    engine.stop();
}

/// DEC-155 step 3: the go/no-go scorecard is reached from Usage on a real engine and reads
/// in the desktop's words. A scratch engine has no tickets, so it shows the honest zeros,
/// the empty task line and the daily question off.
#[test]
fn the_scorecard_opens_from_usage_with_the_desktop_words() {
    let (mut engine, pty, _home) = tui_at_home("ca-e2e-scorecard-");
    go_to(&pty, "Usage", "Usage");
    pty.until("Usage shows its scorecard key", |screen| {
        screen.contains("g Scorecard")
    });
    pty.keys(&["g"]);
    let screen = pty.until("the scorecard reads its numbers", |screen| {
        screen.contains("Go/no-go scorecard") && screen.contains("Target 10 done tickets")
    });
    for words in [
        "Whether CrazyAgent earns its place over",
        "Real tasks 0  Target 10 done tickets you accepted",
        "The daily question is off",
        "Real tasks behind the numbers",
        "No real tasks in this window yet.",
        "w Window",
        "g Usage",
    ] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    assert!(!screen.contains("Could not read the scorecard"), "{screen}");
    drop(pty);
    engine.stop();
}

/// The most PgDn presses a harness page takes to reach its locked settings: a page of up to
/// 160 rows at 10 rows a press.
const HARNESS_PAGES_MAX: usize = 16;

/// The harness row the Settings, Harnesses list marks, if any. Input: the screen. Output: the
/// marked row's text after its marker.
fn marked_row(screen: &str) -> Option<&str> {
    screen
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("> "))
}

/// DEC-155 step 3: Codex's settings page opens from Settings, Harnesses on a real engine and
/// reads as the desktop's: the harness as its title with its purpose, the version line, when
/// a change applies, its settings under the desktop's group titles, and Esc back to All
/// harnesses. Codex must be installed on this machine; a machine without it fails as a
/// missing prerequisite.
#[test]
fn a_harness_settings_page_opens_from_settings_harnesses_with_the_desktop_words() {
    let (mut engine, pty, _home) = tui_at_home("ca-e2e-harness-page-");
    go_to(&pty, "Settings / Harnesses", "Settings / Harnesses");
    pty.until("Settings, Harnesses lists Codex", |screen| {
        screen.contains("Codex") && marked_row(screen).is_some()
    });
    for step in 0..=STEPS_MAX {
        let screen = pty.screen();
        let marked = marked_row(&screen).map(str::to_owned);
        if marked.as_deref().is_some_and(|row| row.contains("Codex")) {
            break;
        }
        assert!(
            step < STEPS_MAX,
            "no Codex row in Settings, Harnesses:\n{screen}"
        );
        pty.keys(&["Down"]);
        pty.until("the list marks the next harness", |now| {
            marked_row(now).map(str::to_owned) != marked
        });
    }
    let screen = pty.screen();
    assert!(
        screen.contains("Enter Settings"),
        "prerequisite: Codex is installed on this machine:\n{screen}"
    );
    pty.keys(&["Enter"]);
    let screen = pty.until("Codex's settings page reads its version", |screen| {
        screen.contains("Changes apply the next time a master or agent starts with this harness.")
    });
    let qualified = screen.contains("is qualified. You can change the settings below.");
    let first_group = if qualified {
        "Settings ("
    } else {
        "Read-only ("
    };
    for words in [
        "What every master and agent using this harness starts with, in every project.",
        "Running ones keep their settings.",
        first_group,
        "Esc All harnesses",
    ] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    let mut locked = !qualified || screen.contains("Set by CrazyAgent (");
    for _ in 0..HARNESS_PAGES_MAX {
        if locked {
            break;
        }
        let before = pty.screen();
        pty.keys(&["PageDown"]);
        let now = pty.until("PgDn moves down the settings", |now| now != before);
        locked = now.contains("Set by CrazyAgent (");
    }
    assert!(
        locked,
        "PgDn reaches the Set by CrazyAgent group of a qualified Codex:\n{}",
        pty.screen()
    );
    assert!(
        screen.contains("is qualified. You can change the settings below.")
            || screen.contains("so every setting is read-only"),
        "the version line:\n{screen}"
    );
    pty.keys(&["Escape"]);
    pty.until("Esc goes back to All harnesses", |screen| {
        marked_row(screen).is_some_and(|row| row.contains("Codex"))
    });
    drop(pty);
    engine.stop();
}

/// DEC-155 step 3: on a real engine a project's own V3 pages are reached from its tabs on the
/// 2.4.0 shell: s on the Tests tab opens the project's check settings in the desktop's words
/// and Esc comes back, and w on the Settings tab opens the Repository trust box.
#[test]
fn a_project_s_check_settings_and_repository_trust_open_from_its_tabs() {
    let engine = Engine::start("ca-e2e-project-v3-");
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
                owner.create_project("e2e-v3-tabs", PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });
    let mut engine = engine;
    let pty = Pty::launch(tmux, &engine.socket());
    reach_home(&pty, |screen| at_home(screen, PROJECT));
    setup_checked(&pty);
    tab_until_enter(&pty, "Open");
    pick_card(&pty, PROJECT);
    pty.keys(&["Enter"]);
    pty.until("the project screen opens with its tabs", |screen| {
        screen.contains("Tabs") && in_project(screen, PROJECT)
    });

    to_tab(&pty, PROJECT, "Tests", &SHIFT_RIGHT);
    pty.until("the Tests tab offers its settings", |screen| {
        screen.contains("s Settings")
    });
    pty.keys(&["s"]);
    let screen = pty.until("s opens the project's check settings", |screen| {
        screen.contains("Check rules") && screen.contains("Check profiles")
    });
    for words in [
        "wins over the app value for this project only.",
        "A check runs only under a profile you approved.",
        "a App values",
        "Esc Back",
    ] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    assert!(
        !screen.contains("The check settings every project starts from"),
        "the app purpose stays on the app page:\n{screen}"
    );
    pty.keys(&["Escape"]);
    pty.until("Esc goes back to the Tests tab", |screen| {
        tab_shown(screen, PROJECT).as_deref() == Some("Tests")
    });

    to_tab(&pty, PROJECT, "Settings", &SHIFT_RIGHT);
    pty.until("the Settings tab names Repository trust", |screen| {
        screen.contains("w Repository trust")
    });
    pty.keys(&["w"]);
    pty.until("w opens the Repository trust box", |screen| {
        screen.contains("How far you trust this repository's code and files.")
            && screen.contains("Level ")
    });
    drop(pty);
    engine.stop();
}
