use crate::research::runner::RunError;
#[cfg(unix)]
use crate::research::runner::{AgentProcess, drain, private_directory};
#[cfg(unix)]
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::{Read, Write};
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
/// The most bytes of a graph's keep-out mark that are read.
#[cfg(unix)]
pub(crate) const MARK_BYTES_MAX: u64 = 128;

/// The file the engine writes into `graphify-out` once a graph is built under the keep-out
/// (MAP-2), holding a digest of the paths kept out and hidden. The mapping reads a graph
/// only when it holds the mark of the keep-out as it is now, so never one an older build or
/// another tool made nor one built before a folder was hidden or a path kept out, and the
/// next graph step rebuilds such a graph from nothing.
#[cfg(unix)]
pub(crate) const KEPT_OUT_MARK: &str = ".crazyagent-keep-out";

/// The plain reason a graph step fails with when no graphify is installed.
pub(crate) const GRAPHIFY_MISSING: &str = "graphify not installed";
/// The plain reason a graph step fails with when the project's git cannot say which files it
/// ignores, so the build could not leave them out (MAP-2).
#[cfg(unix)]
pub(crate) const GRAPH_IGNORES_UNKNOWN: &str =
    "Graph not built: the project's git could not tell which files it ignores.";
/// The plain reason a graph step fails with when the paths it must leave out are more than
/// the sandbox holds or one has a name the sandbox cannot hold (MAP-2).
#[cfg(unix)]
pub(crate) const GRAPH_KEEP_OUT_REFUSED: &str =
    "Graph not built: the files it must leave out could not all be kept out.";

/// The owner's line for a graph build on a system that runs no confined graphify in V1:
/// Linux (DEC-094), where the build would start the macOS Seatbelt tool that is not there.
/// Input: the operating system name as `std::env::consts::OS` gives it. Output: the line,
/// or None where the graph builds.
#[cfg(unix)]
pub(crate) fn unavailable(os: &str) -> Option<&'static str> {
    (os == "linux").then_some("Graph build is not available on Linux in this version.")
}

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
/// no network that may read the project but not its engine files in `.crazyagent/`,
/// the hidden folders, nor any secret-like or git-ignored path (MAP-2), which stay out
/// of the graph, and graphify's install, and write only `repo/graphify-out`, its own
/// home and temp dirs; without git's answer no graph is built, and nothing is made or
/// removed before every check passes. A graph without the current [`KEPT_OUT_MARK`] is
/// removed first, so it is rebuilt from nothing, and a built graph is marked. Takes the
/// graphify executable, the project folder, the engine state folder, a unique run name,
/// the hidden folders, the time limit and `stop`, polled while it runs. Returns how it
/// ended; its process group is stopped on every path.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn update(
    graphify: &Path,
    repo: &Path,
    state: &Path,
    name: &str,
    hidden: &[std::path::PathBuf],
    limit: Duration,
    stop: &dyn Fn() -> bool,
) -> Result<GraphEnding, RunError> {
    if let Some(line) = unavailable(std::env::consts::OS) {
        return Ok(GraphEnding::Failed(line));
    }
    let Some(graphify) = graphify
        .canonicalize()
        .ok()
        .filter(|graphify| graphify.is_file())
    else {
        return Ok(GraphEnding::Failed(GRAPHIFY_MISSING));
    };
    let out = repo.join(GRAPH_OUT);
    let existing = match std::fs::symlink_metadata(&out) {
        Ok(metadata) if metadata.file_type().is_dir() => true,
        Ok(_) => return Ok(GraphEnding::Failed("graphify-out is not a folder")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let Some(kept_out) = crate::onboarding::draft::kept_out(repo) else {
        return Ok(GraphEnding::Failed(
            if crate::onboarding::draft::outside_git(repo) {
                GRAPH_KEEP_OUT_REFUSED
            } else {
                GRAPH_IGNORES_UNKNOWN
            },
        ));
    };
    if let Err(code) = crate::sandbox::hidden_paths::none_tracked(repo, hidden) {
        return Ok(GraphEnding::Failed(code));
    }
    let kept = kept_paths(repo, &kept_out);
    if kept.len() > crate::sandbox::KEPT_OUT_MAX
        || !kept.iter().all(|path| ProjectSandbox::keepable(path))
    {
        return Ok(GraphEnding::Failed(GRAPH_KEEP_OUT_REFUSED));
    }
    let agent = state.join("agents").join(name);
    if std::fs::symlink_metadata(&agent).is_ok() {
        return Err(RunError::Invalid(
            crate::research::runner::RUN_NAME_USED.into(),
        ));
    }
    let mark = kept_out_mark(&kept, hidden);
    if existing && marked(&out).as_deref() != Some(mark.as_str()) {
        std::fs::remove_dir_all(&out)?;
    }
    match std::fs::DirBuilder::new().mode(0o755).create(&out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    if !std::fs::symlink_metadata(&out)?.file_type().is_dir() {
        return Ok(GraphEnding::Failed("graphify-out is not a folder"));
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
    sandbox = sandbox.hide(&repo.join(crate::projects_home::CRAZYAGENT_DIRECTORY))?;
    for folder in hidden {
        sandbox = sandbox.hide(folder)?;
    }
    for path in &kept {
        sandbox = match sandbox.keep_out(path) {
            Ok(sandbox) => sandbox,
            Err(_) => return Ok(GraphEnding::Failed(GRAPH_KEEP_OUT_REFUSED)),
        };
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
    let mut running = AgentProcess::spawn(&mut command)?;
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
    running.stop().map_err(RunError::Uncertain)?;
    if ending == GraphEnding::Built {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(out.join(KEPT_OUT_MARK))?
            .write_all(mark.as_bytes())?;
    }
    Ok(ending)
}

/// The mark a graph built now under the keep-out of `repo` and its `hidden` folders carries,
/// as [`update`] writes it. Takes the folder and the hidden folders. Returns the mark's text,
/// or `None` when the paths to keep out cannot be listed, so no graph is read.
#[cfg(unix)]
pub(crate) fn current_mark(repo: &Path, hidden: &[PathBuf]) -> Option<String> {
    let kept_out = crate::onboarding::draft::kept_out(repo)?;
    Some(kept_out_mark(&kept_paths(repo, &kept_out), hidden))
}

/// The paths of `kept_out`, below `repo`, the sandbox keeps out: all but those under the
/// graph's own output, which graphify writes, and the engine folder, hidden anyway. Takes
/// the folder and the listed paths. Returns the absolute paths.
#[cfg(unix)]
fn kept_paths(repo: &Path, kept_out: &[String]) -> Vec<PathBuf> {
    kept_out
        .iter()
        .filter(|path| {
            let top = path.split('/').next().unwrap_or_default();
            top != GRAPH_OUT && top != crate::projects_home::CRAZYAGENT_DIRECTORY
        })
        .map(|path| repo.join(path.trim_end_matches('/')))
        .collect()
}

/// The mark of a graph built under the keep-out: a SHA-256 digest of the paths kept out
/// and of the hidden folders, each path followed by a NUL and the two lists split by one
/// more. Takes both lists. Returns the mark's text.
#[cfg(unix)]
fn kept_out_mark(kept: &[PathBuf], hidden: &[PathBuf]) -> String {
    use sha2::Digest as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut digest = sha2::Sha256::new();
    for path in kept {
        digest.update(path.as_os_str().as_bytes());
        digest.update([0]);
    }
    digest.update([0]);
    for folder in hidden {
        digest.update(folder.as_os_str().as_bytes());
        digest.update([0]);
    }
    format!("keep-out v1 {:x}\n", digest.finalize())
}

/// The mark the graph in `out` carries, read without following a link and within 128
/// bytes. Takes the graph folder. Returns the mark's text, or `None` when it has none.
#[cfg(unix)]
fn marked(out: &Path) -> Option<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(out.join(KEPT_OUT_MARK))
        .ok()?;
    let mut text = String::new();
    file.take(MARK_BYTES_MAX).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Reports a failed graph build where graphify cannot run in its own stoppable process
/// group under the sandbox. Takes the build inputs, all unused. Returns a failed ending.
#[cfg(not(unix))]
pub(crate) fn update(
    _graphify: &Path,
    _repo: &Path,
    _state: &Path,
    _name: &str,
    _hidden: &[std::path::PathBuf],
    _limit: Duration,
    _stop: &dyn Fn() -> bool,
) -> Result<GraphEnding, RunError> {
    Ok(GraphEnding::Failed(
        "Graph build is unavailable on this platform",
    ))
}
