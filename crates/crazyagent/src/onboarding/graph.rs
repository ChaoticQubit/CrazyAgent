use crate::research::runner::RunError;
#[cfg(unix)]
use crate::research::runner::{AgentProcess, drain, private_directory};
#[cfg(unix)]
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::AtomicU64;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const GRAPH_OUT: &str = "graphify-out";
#[cfg(unix)]
const GRAPH_FILE: &str = "graph.json";
#[cfg(unix)]
const VENV_FILE: &str = "pyvenv.cfg";
#[cfg(unix)]
const VENV_BYTES_MAX: u64 = 4096;
#[cfg(unix)]
const LOG_BYTES_MAX: u64 = 1024 * 1024;
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(100);

/// The plain reason a graph step fails with when no graphify is installed.
pub(crate) const GRAPHIFY_MISSING: &str = "graphify not installed";

/// How one `graphify update .` run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GraphEnding {
    #[cfg(unix)]
    Built,
    Failed(&'static str),
    #[cfg(unix)]
    TimedOut,
    #[cfg(unix)]
    Stopped,
}

/// The folders graphify needs to read to start: its own tool folder and, for
/// a uv tool install, the Python installation its `pyvenv.cfg` names.
#[cfg(unix)]
fn read_roots(graphify: &Path) -> Vec<PathBuf> {
    let Some(bin) = graphify.parent() else {
        return Vec::new();
    };
    let tool = bin.parent().unwrap_or(bin);
    let mut roots = vec![tool.to_path_buf()];
    let venv = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(tool.join(VENV_FILE))
        .ok()
        .and_then(|file| {
            let mut text = String::new();
            file.take(VENV_BYTES_MAX).read_to_string(&mut text).ok()?;
            Some(text)
        });
    if let Some(home) = venv.as_deref().and_then(|text| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(key, _)| key.trim() == "home")
            .map(|(_, value)| PathBuf::from(value.trim()))
    }) && let Some(python) = home.parent()
        && let Ok(python) = python.canonicalize()
        && !roots.contains(&python)
    {
        roots.push(python);
    }
    roots
}

/// Runs the free, AST-only `graphify update .` in `repo` under a sandbox with
/// no network that may read the project and graphify's install but write only
/// `repo/graphify-out`, its own home and temp dirs. Takes the graphify
/// executable, the project folder, the engine state folder, a unique run
/// name, the time limit and `stop`, polled while it runs. Returns how it
/// ended; its process group is stopped on every path.
#[cfg(unix)]
pub(crate) fn update(
    graphify: &Path,
    repo: &Path,
    state: &Path,
    name: &str,
    limit: Duration,
    stop: &dyn Fn() -> bool,
) -> Result<GraphEnding, RunError> {
    let Some(graphify) = graphify
        .canonicalize()
        .ok()
        .filter(|graphify| graphify.is_file())
    else {
        return Ok(GraphEnding::Failed(GRAPHIFY_MISSING));
    };
    let out = repo.join(GRAPH_OUT);
    match std::fs::DirBuilder::new().mode(0o755).create(&out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    if !std::fs::symlink_metadata(&out)?.file_type().is_dir() {
        return Ok(GraphEnding::Failed("graphify-out is not a folder"));
    }
    let agent = state.join("agents").join(name);
    if std::fs::symlink_metadata(&agent).is_ok() {
        return Err(RunError::Invalid("graph run name already used".into()));
    }
    private_directory(&agent)?;
    let home = agent.join("home");
    private_directory(&home)?;
    let tmp = agent.join("tmp");
    private_directory(&tmp)?;
    let mut sandbox = ProjectSandbox::new(&out, &home, &tmp, SandboxNetwork::Only(Vec::new()))?
        .allow_read(repo)?;
    for root in read_roots(&graphify) {
        sandbox = sandbox.allow_read(&root)?;
    }
    let profile = state.join("profiles").join(format!("{name}.sb"));
    sandbox.install(&profile)?;
    let mut command = sandbox.command(&profile, &graphify);
    command
        .args(["update", "."])
        .env("GRAPHIFY_NO_TIPS", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONNOUSERSITE", "1")
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut running = AgentProcess::new(command.spawn()?);
    let counted = Arc::new(AtomicU64::new(0));
    let streams = [
        (
            running
                .child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            "stdout.log",
        ),
        (
            running
                .child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            "stderr.log",
        ),
    ];
    for (stream, log) in streams {
        let Some(stream) = stream else { continue };
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(agent.join(log))?;
        let counted = counted.clone();
        std::thread::spawn(move || drain(stream, &counted, log, LOG_BYTES_MAX));
    }
    let started = Instant::now();
    let ending = loop {
        if let Some(status) = running.child.try_wait()? {
            break if !status.success() {
                GraphEnding::Failed("Graph build failed")
            } else if std::fs::symlink_metadata(out.join(GRAPH_FILE))
                .is_ok_and(|metadata| metadata.file_type().is_file())
            {
                GraphEnding::Built
            } else {
                GraphEnding::Failed("Graph build wrote no graph")
            };
        }
        if started.elapsed() >= limit {
            break GraphEnding::TimedOut;
        }
        if stop() {
            break GraphEnding::Stopped;
        }
        std::thread::sleep(POLL);
    };
    running.stop();
    Ok(ending)
}

/// Reports a failed graph build where graphify cannot run in its own stoppable process
/// group under the sandbox. Takes the build inputs, all unused. Returns a failed ending.
#[cfg(not(unix))]
pub(crate) fn update(
    _graphify: &Path,
    _repo: &Path,
    _state: &Path,
    _name: &str,
    _limit: Duration,
    _stop: &dyn Fn() -> bool,
) -> Result<GraphEnding, RunError> {
    Ok(GraphEnding::Failed(
        "Graph build is unavailable on this platform",
    ))
}
