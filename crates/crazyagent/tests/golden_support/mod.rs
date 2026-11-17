#![allow(dead_code)]

use crate::e2e_support::{Engine, Owner, TMUX, git, profile, repository, scratch, stop_tmux};
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::onboarding::{Step, project_id_for};
use crazyagent::project_setup::{HarnessSlot, ProjectSetup};
use crazyagent::protocol::{CredentialRef, ProjectId};
use crazyagent::terminal::FOLDER;
use crazyagent::terminal::tmux::{SOCKET, installed};
use serde_json::{Value, json};
use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);
const ONBOARD_TIME: Duration = Duration::from_secs(120);
const README: &str = "# Fruit stand\n\nSells fruit.\n";
const WAIT_MAX: Duration = Duration::from_secs(300);
const STEPS_MAX: usize = 64;
const RECORDS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/golden");

/// One golden flow on a hermetic engine: its own root, a scratch HOME and Codex home, no
/// inherited git or harness home variable, the owner attached with a profile saved and a
/// projects folder picked, the tmux a team flow's terminals use, and the flow's run record.
/// Output: the started flow; dropped unfinished, it records the steps it reached as failed.
pub struct Golden {
    pub engine: Engine,
    pub owner: Owner,
    pub projects: PathBuf,
    tmux: Option<PathBuf>,
    _place: tempfile::TempDir,
    record: Record,
}

impl Golden {
    /// Starts the flow's engine and owner as a first run leaves them, and counts that as the
    /// flow's first step. Inputs: the flow's name (lowercase words joined by `-`) and the
    /// engine's global.toml text. Output: the started flow.
    pub async fn start(flow: &'static str, global: &str) -> Self {
        Self::launch(flow, global, None).await
    }

    /// Starts a team flow: as [`Self::start`], with every Codex agent run by the stand-in
    /// pinned with `--codex-executable`, and the engine's tmux ended when the flow drops.
    /// Inputs: the flow's name, the engine's global.toml text and the stand-in's absolute
    /// path. Output: the started flow.
    pub async fn start_team(flow: &'static str, global: &str, codex: &Path) -> Self {
        assert!(
            codex.is_absolute() && codex.is_file(),
            "{}",
            codex.display()
        );
        Self::launch(flow, global, Some(codex)).await
    }

    /// Starts the engine, pinning the Codex stand-in when one is given, and the owner.
    /// Inputs: the flow's name, the engine's global.toml text and the stand-in, if any.
    /// Output: the started flow, its first step recorded.
    async fn launch(flow: &'static str, global: &str, codex: Option<&Path>) -> Self {
        let record = Record::new(flow, "engine");
        let place = scratch("ca-golden-");
        let home = place.path().join("home");
        let codex_home = home.join(".codex");
        let projects = place.path().join("projects");
        for folder in [&home, &codex_home, &projects] {
            fs::create_dir(folder).unwrap();
        }
        let mut engine = Engine::new("ca-golden-engine-");
        fs::write(engine.path().join("global.toml"), global).unwrap();
        engine.home = Some(home);
        engine.env.push(("CODEX_HOME", codex_home.into_os_string()));
        let inherited = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("GIT_") || name == "CLAUDE_CONFIG_DIR");
        engine.env_removed.extend(inherited);
        let tmux = codex.map(|_| installed(engine.path()).expect(TMUX));
        engine.harness = codex.map(|codex| ("--codex-executable", codex.to_path_buf()));
        engine.launch();
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&projects).await, OwnerResult::Applied);
        let mut golden = Self {
            engine,
            owner,
            projects,
            tmux,
            _place: place,
            record,
        };
        golden.step("start the engine");
        golden
    }

    /// Ends one step of the flow, timed from the end of the one before.
    /// Input: the step's name, a fixed text that holds no path and no name the owner typed.
    /// Output: none.
    pub fn step(&mut self, name: &'static str) {
        self.record.step(name);
    }

    /// Ends the flow as passed: writes its run record, then reads it back from disk.
    /// Input: none.
    /// Output: the record as the evidence folder holds it, or none when it holds none.
    pub fn finish(&mut self) -> Option<Value> {
        self.record.finish()
    }

    /// Makes an owner repository a project as onboarding does, then gives it the pool and one
    /// Codex sub-agent with team tools, as the app's team rows do. Inputs: the repository and
    /// the pool. Output: the project's id.
    pub async fn onboard(&mut self, repo: &Path, pool: &CredentialRef) -> ProjectId {
        let generation = self.owner.next();
        let path = repo.to_str().unwrap().to_owned();
        let controller = &mut self.owner.controller;
        let started = controller
            .start_onboarding(path, pool.clone(), generation)
            .await;
        assert_eq!(landed(started.unwrap()), OwnerResult::Applied);
        let project = project_id_for(repo).unwrap();
        let id = &project;
        wait_until("onboarding to settle", ONBOARD_TIME, async || {
            let view = controller.onboarding(id.clone()).await.unwrap();
            let busy = |step: &Step| matches!(step, Step::Waiting { .. } | Step::Running);
            (!busy(&view.graph) && !busy(&view.mapping)).then_some(())
        })
        .await;
        self.assign_pool(&project, pool).await;
        let stored = self.owner.controller.project_setup(project.clone()).await;
        let stored = stored
            .unwrap()
            .expect("onboarding records the project's setup");
        let mut team = stored.team;
        team.sub_harnesses = vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: true,
        }];
        let setup = ProjectSetup {
            about: stored.about,
            tech_stack: stored.tech_stack,
            platforms: stored.platforms,
            research: stored.research,
            pool: stored.pool,
            team: Some(team),
            documents: Vec::new(),
        };
        let generation = self.owner.next();
        let saved = self
            .owner
            .controller
            .update_project_setup(project.clone(), stored.revision, None, setup, generation)
            .await;
        assert_eq!(landed(saved.unwrap()), OwnerResult::Applied);
        project
    }

    /// Assigns a pool to a project as Model access does: reads the saved pools for the
    /// settings version a change names, then sends the assignment. Inputs: the project and
    /// the pool. Output: none; any result but applied fails the flow.
    async fn assign_pool(&mut self, project: &ProjectId, pool: &CredentialRef) {
        let query = OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 16,
        };
        let reply = self.owner.controller.read(query).await.unwrap();
        let OwnerData::ModelAccessPage {
            source_hash,
            writable: true,
            ..
        } = reply.data
        else {
            panic!("the pools are writable");
        };
        let assign = OwnerMutation::ModelAccess {
            expected_config_id: reply.config_id,
            expected_source_hash: source_hash,
            edit: ModelAccessEdit::AssignProjectPool {
                project: CredentialRef::new(project.as_str()).unwrap(),
                pool: Some(pool.clone()),
            },
        };
        let generation = self.owner.next();
        let controller = &mut self.owner.controller;
        controller.begin_mutation(assign, generation).unwrap();
        assert_eq!(
            landed(controller.send_pending().await.unwrap()),
            OwnerResult::Applied
        );
    }
}

impl Drop for Golden {
    /// Ends a team flow's tmux server and every pane in it, so a failed run leaves no
    /// stand-in behind; the engine and the run record drop after it.
    /// Input: none. Output: none.
    fn drop(&mut self) {
        if let Some(tmux) = &self.tmux {
            stop_tmux(tmux, &self.engine.path().join(FOLDER).join(SOCKET));
        }
    }
}

/// Makes an owner repository: a git repository on main with a README in one commit, and the
/// owner's Git name and email stored in it when given, as an Accept needs to commit a landed
/// set. Inputs: the folder, made if missing, and the identity as [name, email], if any.
/// Output: the first commit.
pub fn owner_repo(folder: &Path, identity: Option<[&str; 2]>) -> String {
    repository(folder, "README.md", README);
    if let Some([name, email]) = identity {
        assert!(!name.is_empty() && !email.is_empty());
        git(folder, &["config", "user.name", name]);
        git(folder, &["config", "user.email", email]);
    }
    git_text(folder, &["rev-parse", "HEAD"])
}

/// Runs git in a folder as `e2e_support::git` does and reads what it printed.
/// Inputs: the folder and git's arguments.
/// Output: its trimmed stdout; a failing git fails the flow.
pub fn git_text(folder: &Path, args: &[&str]) -> String {
    let output = git(folder, args);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Opens a loopback port that takes connections and closes each unanswered, for the
/// provider of an engine whose agents a stand-in runs: the engine's reach check finds it
/// listening and no model is asked. Input: none. Output: the port; its thread stays blocked
/// on the listener until the test process ends.
pub fn silent_provider() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(port > 0);
    std::thread::spawn(move || listener.incoming().for_each(drop));
    port
}

/// A flow's run record: its name, the client that ran it, each step's name and milliseconds,
/// its measures and the outcome, written as `<flow>-<client>.json` to `E2E_EVIDENCE` when that
/// is an absolute folder, else to the test target's golden folder. Fields: the flow, the
/// client, when it started, when the last step ended, the steps, the measures and whether it
/// was finished.
pub struct Record {
    flow: &'static str,
    client: &'static str,
    started: Instant,
    mark: Instant,
    steps: Vec<(&'static str, u64)>,
    measures: Vec<(&'static str, u64)>,
    finished: bool,
}

impl Record {
    /// Starts the clock and removes the record an earlier run of the flow left. Inputs: the
    /// flow's name, lowercase words joined by `-`, and the client: `engine` for a flow on the
    /// owner requests, `tui` for one on the real TUI. Output: the record, with no step.
    pub fn new(flow: &'static str, client: &'static str) -> Self {
        let named = |byte: u8| byte.is_ascii_lowercase() || byte == b'-';
        assert!(!flow.is_empty() && flow.len() <= 64 && flow.bytes().all(named));
        assert!(matches!(client, "engine" | "tui"), "{client}");
        let now = Instant::now();
        let record = Self {
            flow,
            client,
            started: now,
            mark: now,
            steps: Vec::new(),
            measures: Vec::new(),
            finished: false,
        };
        if let Err(error) = fs::remove_file(record.path()) {
            assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
        }
        record
    }

    /// Keeps one measure of the flow for its record, such as a start's time or a folder's
    /// growth. Inputs: the measure's name, lowercase words joined by `_`, set once, and its
    /// value. Output: none.
    pub fn measure(&mut self, name: &'static str, value: u64) {
        let named = |byte: u8| byte.is_ascii_lowercase() || byte == b'_';
        assert!(!name.is_empty() && name.bytes().all(named) && !self.finished);
        assert!(self.measures.iter().all(|(kept, _)| *kept != name), "{name}");
        assert!(self.measures.len() < STEPS_MAX);
        self.measures.push((name, value));
    }

    /// Ends the flow as passed: writes its run record, then reads it back from disk.
    /// Input: none.
    /// Output: the record as the evidence folder holds it, or none when it holds none.
    pub fn finish(&mut self) -> Option<Value> {
        assert!(!self.finished);
        self.finished = true;
        self.write("passed").unwrap();
        serde_json::from_slice(&fs::read(self.path()).ok()?).ok()
    }

    /// Ends one step at the time now, timed from the end of the one before.
    /// Input: the step's name.
    /// Output: none.
    pub fn step(&mut self, name: &'static str) {
        assert!(!name.is_empty() && !self.finished);
        assert!(
            self.steps.len() < STEPS_MAX,
            "a flow has at most {STEPS_MAX} steps"
        );
        let now = Instant::now();
        self.steps
            .push((name, millis(now.duration_since(self.mark))));
        self.mark = now;
    }

    /// Where this flow's record goes: `E2E_EVIDENCE` when that is an absolute folder, else
    /// the test target's golden folder. Input: none.
    /// Output: the file's path.
    fn path(&self) -> PathBuf {
        let folder = match std::env::var_os("E2E_EVIDENCE").map(PathBuf::from) {
            Some(folder) if folder.is_absolute() => folder,
            _ => PathBuf::from(RECORDS),
        };
        folder.join(format!("{}-{}.json", self.flow, self.client))
    }

    /// Writes the record: the flow, the client, the outcome, the whole time and each step's.
    /// Input: `passed` or `failed`.
    /// Output: the write's error, if any.
    fn write(&self, outcome: &str) -> std::io::Result<()> {
        assert!(matches!(outcome, "passed" | "failed"));
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|(step, ms)| json!({ "step": step, "ms": ms }))
            .collect();
        let record = json!({
            "flow": self.flow,
            "client": self.client,
            "outcome": outcome,
            "total_ms": millis(self.started.elapsed()),
            "steps": steps,
        });
        let path = self.path();
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, serde_json::to_vec_pretty(&record)?)
    }
}

impl Drop for Record {
    /// Records an unfinished flow as failed at the steps it reached, with no panic of its
    /// own while the test's panic unwinds.
    /// Input: none. Output: none.
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.write("failed");
        }
    }
}

/// Reads until `read` gives a value, every 100 ms, for at most `limit`: the one place a
/// golden flow sleeps (Q2). Inputs: what is awaited, for the failure, the limit (at most
/// five minutes) and the read. Output: the value the read gave.
pub async fn wait_until<T>(
    what: &str,
    limit: Duration,
    mut read: impl AsyncFnMut() -> Option<T>,
) -> T {
    assert!(
        limit > Duration::ZERO && limit <= WAIT_MAX,
        "{what}: {limit:?}"
    );
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = read().await {
            return value;
        }
        assert!(Instant::now() < deadline, "{what} never came in {limit:?}");
        tokio::time::sleep(POLL).await;
    }
}

/// The result an owner change landed with, applied at once or committed; any other reply
/// fails the flow.
/// Input: the engine's reply. Output: the change's result.
pub fn landed(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert!(!operation.historical_replay);
            operation.result
        }
        other => panic!("the owner change must land: {other:?}"),
    }
}

/// A duration in whole milliseconds, as a record holds it.
/// Input: the duration, under five hundred million years.
/// Output: the milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap()
}
