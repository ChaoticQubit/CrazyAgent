use super::{HEAD_BYTES_MAX, bounded_read};
use std::path::Path;

/// Whether a worktree's HEAD names only what Git resolves outside that worktree's own
/// folder: a detached object id, or a branch in `refs/heads/`, whose ref the walk of the
/// shared refs checks. Per-worktree refs such as `refs/worktree/` or `refs/bisect/` and
/// pseudo refs are refused. It is read with [`bounded_read`], so a FIFO swapped in is
/// never waited on. Input: the HEAD file. Output: the answer.
pub(super) fn plain_head(path: &Path) -> bool {
    let Ok(bytes) = bounded_read(path, HEAD_BYTES_MAX) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    match line.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch_name(branch),
        None => {
            matches!(line.len(), 40 | 64)
                && line
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        }
    }
}

/// Whether text is a branch name Git accepts after `refs/heads/`, by the rules of
/// `git check-ref-format`: no control character, space or any of `~^:?*[\`, no `..` or
/// `@{`, not `@`, no empty component or one starting with `.` or ending in `.lock`, and no
/// trailing `.`. Input: the name. Output: the answer.
fn branch_name(name: &str) -> bool {
    name != "@"
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f || b" ~^:?*[\\".contains(&byte))
        && name
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Whether a file name is made only of hex digits, in either case, since a folder that
/// ignores case resolves Git's lowercase name to it. Input: the name. Output: the answer.
pub(super) fn hex(name: &std::ffi::OsStr) -> bool {
    !name.is_empty() && name.as_encoded_bytes().iter().all(u8::is_ascii_hexdigit)
}
