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
    screen.contains(&format!("{project}  /  Agents"))
}

fn on_team(screen: &str, project: &str) -> bool {
    screen.contains(&format!("{project}  /  Team")) && screen.contains("Master")
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

fn open_team(pty: &Pty, project: &str) {
    reach_home(pty, |screen| at_home(screen, project));
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
        screen.contains(project) && screen.contains("Tabs")
    });
    for step in 0..=STEPS_MAX {
        let screen = pty.screen();
        if on_agents(&screen, project) {
            break;
        }
        assert!(step < STEPS_MAX, "no project tab is Agents:\n{screen}");
        pty.bytes(&SHIFT_RIGHT);
        pty.until("the next project tab shows", |now| now != screen);
    }
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
        screen.contains("Build the code graph") && screen.contains("Map the project")
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

    for _ in 0..STEPS_MAX {
        if pty.screen().contains("Enter Open") {
            break;
        }
        pty.keys(&["Tab"]);
        std::thread::sleep(POLL_TIME);
    }
    pty.until("Home offers to open the project", |screen| {
        screen.contains("Enter Open")
    });
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
        assert!(ask.contains("stays on disk"), "{ask}");
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
    open_team(&pty, &project);
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
