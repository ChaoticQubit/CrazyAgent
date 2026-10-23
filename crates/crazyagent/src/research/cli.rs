use super::accounts::AccountPick;
use super::jobs::{ResearchJobs, valid_kind};
use super::runner::{ResearchAccount, ResearchLimits, ResearchRunner};
use super::service::{ResearchService, progress};
use crate::protocol::CredentialRef;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Usage line for `crazyagent research`.
pub const USAGE: &str = "crazyagent research STATE_DIR PROJECT_ID PROJECT_DIR --codex-executable ABS_PATH --endpoint URL --model MODEL [--tools on|off] [--minutes N] KIND [KIND...]";

const KINDS_MAX: usize = 8;
const POLL: Duration = Duration::from_secs(2);

/// Queues one research job per KIND for the project, runs them on the given
/// keyless endpoint inside the project sandbox, prints plain progress lines
/// and returns an error when any job did not finish as done.
pub fn run(args: &[OsString]) -> Result<(), String> {
    let usage = || format!("usage: {USAGE}");
    if args.len() < 5 || args[0] != "research" {
        return Err(usage());
    }
    let text = |value: &OsString| value.to_str().map(str::to_owned).ok_or_else(usage);
    let state = PathBuf::from(&args[1]);
    let project = text(&args[2])?;
    let root = PathBuf::from(&args[3]);
    let mut options = BTreeMap::new();
    let mut kinds = Vec::new();
    let mut index = 4;
    while index < args.len() {
        let arg = text(&args[index])?;
        if let Some(name) = arg.strip_prefix("--") {
            let value = args.get(index + 1).ok_or_else(usage)?;
            if options.insert(name.to_owned(), text(value)?).is_some() {
                return Err(usage());
            }
            index += 2;
        } else {
            if !valid_kind(&arg) || kinds.contains(&arg) || kinds.len() >= KINDS_MAX {
                return Err(format!("research kind refused: {arg}"));
            }
            kinds.push(arg);
            index += 1;
        }
    }
    let option = |name: &str| options.get(name).cloned().ok_or_else(usage);
    let codex = PathBuf::from(option("codex-executable")?);
    let account = ResearchAccount::Endpoint {
        base_url: option("endpoint")?,
        model: option("model")?,
    };
    let tools = match options.get("tools").map(String::as_str) {
        None | Some("on") => true,
        Some("off") => false,
        Some(_) => return Err(usage()),
    };
    let mut limits = ResearchLimits::default();
    if let Some(minutes) = options.get("minutes") {
        let minutes: u64 = minutes.parse().map_err(|_| usage())?;
        if !(1..=120).contains(&minutes) {
            return Err(usage());
        }
        limits.wall_time = Duration::from_secs(minutes * 60);
    }
    if kinds.is_empty()
        || options.keys().any(|name| {
            !["codex-executable", "endpoint", "model", "tools", "minutes"].contains(&name.as_str())
        })
    {
        return Err(usage());
    }
    let state = state
        .canonicalize()
        .map_err(|error| format!("state directory: {error}"))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("project directory: {error}"))?;
    let database = state.join("research.sqlite3");
    let jobs = ResearchJobs::open(&database).map_err(|error| error.to_string())?;
    let mut ids = Vec::new();
    for kind in &kinds {
        ids.push(
            jobs.enqueue(&project, kind, "codex", tools)
                .map_err(|error| error.to_string())?,
        );
    }
    let runner = Arc::new(
        ResearchRunner::new(&database, &state, &codex, limits.clone())
            .map_err(|error| error.to_string())?,
    );
    let pick = AccountPick::Account {
        connection: CredentialRef::new("command-line-endpoint")
            .map_err(|error| error.to_string())?,
        account,
    };
    let resolver = {
        let project = project.clone();
        let root = root.clone();
        move |id: &str| (id == project).then(|| (root.clone(), pick.clone()))
    };
    let service = ResearchService::start(runner, resolver, Duration::from_millis(250));
    let deadline =
        Instant::now() + limits.wall_time * limits.attempts_max + Duration::from_secs(120);
    let mut shown = BTreeMap::new();
    let finished = loop {
        let rows = progress(&jobs, &project).map_err(|error| error.to_string())?;
        let mine: Vec<_> = rows
            .into_iter()
            .filter(|row| ids.contains(&row.job))
            .collect();
        for row in &mine {
            let line = match (&row.detail, &row.report) {
                (Some(detail), _) => format!("{}: {} - {detail}", row.title, row.status),
                (None, Some(report)) => format!(
                    "{}: {} - {}",
                    row.title,
                    row.status,
                    root.join(report).display()
                ),
                (None, None) => format!("{}: {}", row.title, row.status),
            };
            if shown.insert(row.job, line.clone()).as_ref() != Some(&line) {
                println!("{line}");
            }
        }
        if mine.len() == ids.len()
            && mine
                .iter()
                .all(|row| matches!(row.status, "Done" | "Failed" | "Cancelled"))
        {
            break mine;
        }
        if Instant::now() >= deadline {
            service.stop();
            return Err("research did not finish in time".into());
        }
        std::thread::sleep(POLL);
    };
    service.stop();
    if finished.iter().all(|row| row.status == "Done") {
        Ok(())
    } else {
        Err("some research did not finish".into())
    }
}
