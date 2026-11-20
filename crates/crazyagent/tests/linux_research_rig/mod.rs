use crate::research_support::EngineRuntime;
use crazyagent::research::jobs::{ResearchJob, ResearchJobs};
use crazyagent::research::runner::{
    ResearchAccount, ResearchLimits, ResearchProject, ResearchRun, ResearchRunner, RunError,
};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// A Codex stand-in that writes the report Codex writes at `-o`, after two probes from
/// inside its boundary: an append to its worktree's admin `HEAD`, judged by its error, and
/// the Seccomp mode of its own process. It uses shell built-ins only.
const STAND_IN: &str = r#"#!/bin/sh
out=""
dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift ;;
    -C) dir=$2; shift ;;
  esac
  shift
done
[ -n "$out" ] && [ -n "$dir" ] || exit 2
cd "$dir" || exit 3
read -r link < .git
admin=${link#gitdir: }
if e=$( { printf x >> "$admin/HEAD"; } 2>&1 ); then head=wrote; else head="denied($e)"; fi
seccomp=none
while read -r key value; do
  [ "$key" = "Seccomp:" ] && seccomp=$value
done < /proc/self/status
printf '# Design research\n\nSTANDIN head=%s seccomp=%s\n' "$head" "$seccomp" > "$out"
"#;

/// One Linux research setup on a real engine runtime ([`EngineRuntime`]), with its root
/// recorded as this process's engine root, as the engine binary records it at its start:
/// the project `project-linux`, the owner's "Linux call supervisor" choice in its
/// `engine.db`, a Codex stand-in, and a local model listener that accepts what the provider
/// check sends. It is kept for the row's whole run.
pub struct Rig {
    _tools: tempfile::TempDir,
    _listener: TcpListener,
    pub runtime: EngineRuntime,
    pub project: PathBuf,
    pub runner: ResearchRunner,
    pub jobs: ResearchJobs,
    pub port: u16,
}

/// Makes the rig. Input: the stored supervisor word, "on" or "off". Output: the rig.
pub fn rig(supervisor: &str) -> Rig {
    assert!(matches!(supervisor, "on" | "off"));
    let runtime = EngineRuntime::with_projects(&[("project-linux", "A small Linux tool.")]);
    crazyagent::tools::record_engine_root(&runtime.root);
    let connection = rusqlite::Connection::open(&runtime.database).unwrap();
    let saved = connection
        .execute(
            "INSERT OR REPLACE INTO sandbox_settings
                (singleton, linux_sockets, linux_supervisor, linux_strict)
             VALUES (1, 'tcp_only', ?1, 'off')",
            [supervisor],
        )
        .unwrap();
    assert_eq!(saved, 1);
    drop(connection);
    let tools = tempfile::tempdir().unwrap();
    let codex = tools.path().canonicalize().unwrap().join("codex");
    fs::write(&codex, STAND_IN).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let limits = ResearchLimits {
        attempts_max: 3,
        ..ResearchLimits::default()
    };
    let runner = ResearchRunner::new(&runtime.database, &runtime.state, &codex, limits).unwrap();
    let jobs = ResearchJobs::open(&runtime.database).unwrap();
    let project = runtime.folder("project-linux");
    Rig {
        _tools: tools,
        _listener: listener,
        runtime,
        project,
        runner,
        jobs,
        port,
    }
}

impl Rig {
    /// Queues one design research job for `project-linux` with tools off and runs it once.
    /// No input. Output: the run's answer and the job as it was left.
    pub fn run(&self) -> (Result<Option<ResearchRun>, RunError>, ResearchJob) {
        let id = self
            .jobs
            .enqueue("project-linux", "design_research", "codex", false)
            .unwrap();
        let ran = self.runner.run_once(&ResearchProject {
            id: "project-linux",
            root: &self.project,
            account: Some(ResearchAccount::Endpoint {
                base_url: format!("http://127.0.0.1:{}/v1", self.port),
                model: "linux-model".into(),
                key: None,
            }),
        });
        (ran, self.jobs.get(id).unwrap().unwrap())
    }

    /// The design research report in the project, if one landed. No input. Output: its
    /// text.
    pub fn report(&self) -> Option<String> {
        let folder: &Path = &crazyagent::projects_home::state_folder(&self.project);
        fs::read_to_string(
            folder
                .join("context")
                .join("research")
                .join("design_research.md"),
        )
        .ok()
    }
}
