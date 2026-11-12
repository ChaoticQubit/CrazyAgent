#[cfg(unix)]
use super::git;
use crate::engine::EngineError;
use std::path::Path;

/// The most commit subjects or tracked paths one listing returns.
pub(crate) const ENTRIES_MAX: usize = 4096;
#[cfg(unix)]
const LISTING_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The subjects of the newest commits on the branch the owner's checkout has checked out,
/// first parents only, newest first, read with one engine git repository that runs nothing
/// the repository configures. Inputs: the checkout root and the most subjects. Output: the
/// subjects, none for an unborn branch, or a plain git code.
pub(crate) fn subjects(checkout: &Path, max: usize) -> Result<Vec<String>, EngineError> {
    assert!((1..=ENTRIES_MAX).contains(&max));
    #[cfg(unix)]
    {
        let owner = git::Repo::open(checkout, &checkout.join(".git"), None)?;
        let Some(head) = owner.commit_of("HEAD")? else {
            return Ok(Vec::new());
        };
        let count = max.to_string();
        let arguments = [
            "log",
            "--first-parent",
            "-z",
            "--format=%s",
            "-n",
            count.as_str(),
            head.as_str(),
        ];
        entries(&owner, &arguments, |_| true, max)
    }
    #[cfg(not(unix))]
    {
        let _ = checkout;
        Err(super::unavailable())
    }
}

/// The paths the owner's checkout's git index tracks that `keep` accepts, in index order,
/// read from the index alone with one engine git repository that runs nothing the
/// repository configures and reads no ignore file. Inputs: the checkout root, the test of
/// one path and the most paths. Output: the paths, or a plain git code.
pub(crate) fn tracked_paths(
    checkout: &Path,
    keep: fn(&str) -> bool,
    max: usize,
) -> Result<Vec<String>, EngineError> {
    assert!((1..=ENTRIES_MAX).contains(&max));
    #[cfg(unix)]
    {
        let owner = git::Repo::open(checkout, &checkout.join(".git"), None)?;
        let arguments = ["ls-files", "-z", "--cached", "--full-name"];
        entries(&owner, &arguments, keep, max)
    }
    #[cfg(not(unix))]
    {
        let _ = (checkout, keep);
        Err(super::unavailable())
    }
}

/// Runs one git listing whose entries end in a NUL byte and keeps the first `max` distinct
/// neighbours `keep` accepts; a listing over `LISTING_BYTES_MAX` is read up to its last
/// whole entry. Inputs: the repository, the arguments, the test and the bound. Output: the
/// entries as text, or `changes_git_failed`.
#[cfg(unix)]
fn entries(
    owner: &git::Repo,
    arguments: &[&str],
    keep: fn(&str) -> bool,
    max: usize,
) -> Result<Vec<String>, EngineError> {
    let output = owner.exec(arguments, None, &[], LISTING_BYTES_MAX)?;
    if output.code != 0 {
        return Err(git::failure("changes_git_failed"));
    }
    let mut listed: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
    if output.overflow {
        listed.pop();
    }
    let mut kept: Vec<String> = Vec::new();
    for entry in listed {
        if kept.len() == max {
            break;
        }
        let text = String::from_utf8_lossy(entry);
        let repeated = kept
            .last()
            .is_some_and(|last| last.as_str() == text.as_ref());
        if !text.is_empty() && !repeated && keep(&text) {
            kept.push(text.into_owned());
        }
    }
    assert!(kept.len() <= max);
    assert!(kept.iter().all(|entry| !entry.contains('\0')));
    Ok(kept)
}
