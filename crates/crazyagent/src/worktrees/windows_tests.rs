use super::git_until;
use std::path::Path;
use std::time::{Duration, Instant};

/// The engine's Git command names `/usr/bin/git`, which on Windows resolves on the current
/// drive, where another local user can plant it. The spawn gate refuses it, and the refusal
/// reaches the worktree caller by its code, not as a missing Git
/// (V244-WIN-ROOT-RELATIVE-PROGRAM, rv-sec2 #512 N3).
#[test]
fn the_engine_git_command_is_refused_by_the_spawn_gate_on_windows() {
    let started = git_until(
        Path::new("."),
        &["--version"],
        4096,
        Instant::now() + Duration::from_secs(10),
    );
    assert_eq!(
        started.map_err(|error| error.0),
        Err("spawn_relative_program_refused".to_owned()),
        "the engine's Git is refused by the gate"
    );
}
