use super::*;

/// The launch state file holding the hook trust keys a Codex launch pinned under
/// `[hooks.state]`, as one JSON list of strings, so the engine reads them back without
/// parsing `config.toml`, whose MCP tables hold secrets (SECRET-PARSE-COPIES R2).
pub(super) const CODEX_HOOK_KEYS: &str = "codex-hook-keys.json";

/// The most bytes of a Codex launch's hook key list `codex_pinned_keys` reads.
pub(super) const CODEX_HOOK_KEYS_BYTES_MAX: u64 = 256 * 1024;

/// Why a Codex home gave no hook keys: its launch left no key list, as for an agent an
/// earlier CrazyAgent started, or the list there cannot be read (#166 N0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKeysUnread {
    Absent,
    Unreadable,
}

/// The hook trust keys a Codex launch pinned in its home's `config.toml`, read from the
/// launch's own key list ([`CODEX_HOOK_KEYS`]) and never from the config, so the engine can
/// ask Codex whether it trusts each one (PA1213R2-F2). Takes the agent's `CODEX_HOME`.
/// Returns the keys, empty when the launch pinned none, or why there are none.
pub fn codex_pinned_keys(home: &Path) -> Result<Vec<String>, HookKeysUnread> {
    let file = match open_unfollowed(&home.join(STATE).join(CODEX_HOOK_KEYS)) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Err(HookKeysUnread::Absent);
        }
        Err(_) => return Err(HookKeysUnread::Unreadable),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, CODEX_HOOK_KEYS_BYTES_MAX + 1),
        &mut bytes,
    )
    .map_err(|_| HookKeysUnread::Unreadable)?;
    if bytes.len() as u64 > CODEX_HOOK_KEYS_BYTES_MAX {
        return Err(HookKeysUnread::Unreadable);
    }
    serde_json::from_slice(&bytes).map_err(|_| HookKeysUnread::Unreadable)
}

/// Opens a launch file for reading without following a link in its place or waiting on a
/// FIFO there, so a link, FIFO, folder or device planted there is refused instead of read or
/// left holding the caller (#166 N1, #168 F1). Input: the path. Output: the plain file, or
/// the error, `NotFound` when nothing is there.
#[cfg(unix)]
fn open_unfollowed(path: &Path) -> std::io::Result<File> {
    let file = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(std::io::Error::from)?;
    if !plain_file(&file) {
        return Err(std::io::Error::other("launch_file_not_plain"));
    }
    Ok(file)
}

/// Opens a launch file for reading where no-follow opening does not exist; no launch writes
/// one there, since `write_private` refuses. Input: the path. Output: the file or the error.
#[cfg(not(unix))]
fn open_unfollowed(path: &Path) -> std::io::Result<File> {
    File::open(path)
}
