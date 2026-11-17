use crate::e2e_support::{Engine, Owner, profile, scratch};
use crazyagent::engine::owner::{OwnerResponse, OwnerResult};
use serde_json::{Value, json};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);
const WAIT_MAX: Duration = Duration::from_secs(300);
const STEPS_MAX: usize = 64;
const RECORDS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/golden");

/// One golden flow on a hermetic engine: its own root, a scratch HOME, no inherited git
/// variable, the owner attached with a profile saved and a projects folder picked, and the
/// flow's run record. Inputs: the flow's name and the engine's global.toml text. Output: the
/// started flow; dropped unfinished, it records the steps it reached as failed.
pub struct Golden {
    pub engine: Engine,
    pub owner: Owner,
    pub projects: PathBuf,
    _place: tempfile::TempDir,
    record: Record,
}

impl Golden {
    /// Starts the flow's engine and owner as a first run leaves them, and counts that as the
    /// flow's first step. Inputs: the flow's name (lowercase words joined by `-`) and the
    /// engine's global.toml text. Output: the started flow.
    pub async fn start(flow: &'static str, global: &str) -> Self {
        let record = Record::new(flow);
        let place = scratch("ca-golden-");
        let home = place.path().join("home");
        let projects = place.path().join("projects");
        for folder in [&home, &projects] {
            fs::create_dir(folder).unwrap();
        }
        let mut engine = Engine::new("ca-golden-engine-");
        fs::write(engine.path().join("global.toml"), global).unwrap();
        engine.home = Some(home);
        let inherited = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("GIT_"));
        engine.env_removed.extend(inherited);
        engine.launch();
        let mut owner = Owner::attach(&engine).await;
        assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
        assert_eq!(owner.set_home(&projects).await, OwnerResult::Applied);
        let mut golden = Self {
            engine,
            owner,
            projects,
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
        assert!(!self.record.finished);
        self.record.finished = true;
        serde_json::from_slice(&fs::read(self.record.path()).ok()?).ok()
    }
}

/// A flow's run record: its name, each step's name and milliseconds, and the outcome,
/// written as `<flow>-engine.json` to `E2E_EVIDENCE` when that is an absolute folder, else
/// to the test target's golden folder. Fields: the flow, when it started, when the last
/// step ended, the steps and whether it was finished.
struct Record {
    flow: &'static str,
    started: Instant,
    mark: Instant,
    steps: Vec<(&'static str, u64)>,
    finished: bool,
}

impl Record {
    /// Starts the clock and removes the record an earlier run of the flow left.
    /// Input: the flow's name, lowercase words joined by `-`.
    /// Output: the record, with no step.
    fn new(flow: &'static str) -> Self {
        let named = |byte: u8| byte.is_ascii_lowercase() || byte == b'-';
        assert!(!flow.is_empty() && flow.len() <= 64 && flow.bytes().all(named));
        let now = Instant::now();
        let record = Self {
            flow,
            started: now,
            mark: now,
            steps: Vec::new(),
            finished: false,
        };
        if let Err(error) = fs::remove_file(record.path()) {
            assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
        }
        record
    }

    /// Ends one step at the time now, timed from the end of the one before.
    /// Input: the step's name.
    /// Output: none.
    fn step(&mut self, name: &'static str) {
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
        folder.join(format!("{}-engine.json", self.flow))
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
            "client": "engine",
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
