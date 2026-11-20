use crate::engine::EngineError;

/// The oldest git that opening a repository, an agent worktree and every engine git call
/// outside those two run on: 2.39, the Apple Git-154 of Xcode 16 and its Command Line Tools
/// (W-GIT-FLOOR, V243-WORKTREE-GIT-FLOOR).
pub(crate) const OPEN_VERSION_MIN: (u32, u32) = (2, 39);

#[cfg(test)]
thread_local! {
    /// What `git version` prints for engine git on this thread in a row, in place of the real
    /// git's answer, so a row can shape the git as an older one (W-GIT-FLOOR).
    pub(super) static VERSION_PRINTED: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Refuses a git version older than `needed`, logging the version found and the one needed
/// once per process for each pair (W-GIT-FLOOR). Inputs: the numbers of the version, major
/// first, the oldest major and minor, and the code an older git is refused with. Output:
/// Ok, else `old`, or `changes_git_unsupported` for fewer than two numbers.
pub(crate) fn refuse_older(
    found: &[u32],
    needed: (u32, u32),
    old: &str,
) -> Result<(), EngineError> {
    let (Some(&major), Some(&minor)) = (found.first(), found.get(1)) else {
        return Err(failure("changes_git_unsupported"));
    };
    if (major, minor) >= needed {
        return Ok(());
    }
    let shown: Vec<String> = found.iter().map(u32::to_string).collect();
    super::log_once(&format!(
        "{old} found={} needed={}.{}",
        shown.join("."),
        needed.0,
        needed.1
    ));
    Err(failure(old))
}

/// The numbers a `git version` line names, or a row's shaped line on this thread
/// (W-GIT-FLOOR). Input: what `git version` printed. Output: its first numbers, at most
/// three, which [`refuse_older`] judges.
pub(crate) fn version_numbers(printed: Vec<u8>) -> Vec<u32> {
    #[cfg(test)]
    let printed = VERSION_PRINTED
        .with_borrow(Clone::clone)
        .map_or(printed, String::into_bytes);
    let text = String::from_utf8_lossy(&printed);
    text.trim()
        .strip_prefix("git version ")
        .unwrap_or_default()
        .split(|c: char| !c.is_ascii_digit())
        .map_while(|part| part.parse::<u32>().ok())
        .take(3)
        .collect()
}

pub(super) fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}
