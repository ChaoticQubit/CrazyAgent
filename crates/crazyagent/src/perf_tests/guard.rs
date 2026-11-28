use crate::perf_kit::{Row, SAMPLES, time};
use crate::sandbox::PROFILE_HOOKS;
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::profile::ProjectSandbox;
use std::fs;
use std::hint::black_box;

/// The engine's five record folders, by the names they carry on disk in both builds.
const RECORDS: [&str; 5] = [
    "agent-skills",
    "agent-instruction-files",
    "agent-instructions-given",
    "agent-home-levels",
    "project-instructions-written",
];

/// Times one agent launch's sandbox build on the engine guard path, as 2.4.4 times its twin:
/// a project sandbox made with the production hooks, then guarded against an engine folder
/// holding the database and the five record folders, the project outside it. Prints one
/// PERF line and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn guard_engine_launch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [project, home, tmp, engine] = ["project", "home", "tmp", "engine"].map(|n| root.join(n));
    for folder in [&project, &home, &tmp, &engine] {
        fs::create_dir(folder).unwrap();
    }
    fs::write(engine.join("engine.db"), "engine secret\n").unwrap();
    for kind in RECORDS {
        fs::create_dir(engine.join(kind)).unwrap();
    }
    let build = || {
        ProjectSandbox::new(
            &PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .and_then(|sandbox| sandbox.guard_engine(&engine))
    };
    assert!(build().is_ok());
    let row = Row {
        layer: 1,
        module: "sandbox",
        name: "profile.guard_engine_launch",
        batch: 1,
    };
    time(&row, SAMPLES, || {
        let _ = black_box(build());
    });
}
