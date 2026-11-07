use super::store::Record;
use crate::engine::EngineError;
use rusqlite::Connection;

/// The program that writes a known lockfile, by the lockfile's file name: `cargo` for
/// `Cargo.lock`, `uv`, `npm`, `pnpm`, `yarn` and `poetry` for theirs. Input: the lockfile's
/// path in the repository. Output: the program's name, or None for another file or a path
/// that leaves the repository.
pub(super) fn program_for(path: &str) -> Option<&'static str> {
    let _ = path;
    None
}

/// The command that rewrites a known lockfile from its manifests, for the major version of
/// its tool (OS-8): none for Yarn 1, which has no lockfile-only mode, `--no-update` for
/// Poetry 1, and none when the tool is missing or printed no version. Inputs: the lockfile's
/// path in the repository and the tool's major version. Output: the program and its
/// arguments, or None when no command fits.
pub(super) fn tool_for(path: &str, major: Option<u32>) -> Option<&'static [&'static str]> {
    let _ = (path, major);
    None
}

/// Reads a tool's major version from what its `--version` printed: the leading number of
/// the first word that starts with a digit, or with `v` and a digit, and goes on with a dot.
/// Input: the printed bytes. Output: the major version, or None when there is none.
pub(super) fn major_of(printed: &[u8]) -> Option<u32> {
    let _ = printed;
    None
}

/// Rewrites the known lockfiles that alone clash between a change set and the tip it lands
/// on, as the engine's own commit on top of the set (OS-8): it merges the tip into the set's
/// head in the owner repository taking the tip's side of each lockfile, runs each lockfile's
/// tool on that merge in a temporary linked worktree confined like an agent, commits the
/// rewritten lockfiles there as the engine with the set's head and the tip as parents, and
/// tells the set's agent to reset to it. The temporary worktree is removed on every path and
/// the engine never runs git in the agent's own clone (CC-N1). Inputs: the store, the held
/// change set and its clashing lockfiles. Output: the new head, or None, with nothing
/// changed, when a tool is missing or fails or another path clashes too.
pub(super) fn regenerate(
    db: &Connection,
    record: &mut Record,
    paths: &[String],
) -> Result<Option<String>, EngineError> {
    let _ = (db, record, paths);
    Ok(None)
}

#[cfg(test)]
mod tests;
