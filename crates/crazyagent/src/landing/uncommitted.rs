use crate::engine::EngineError;
use rusqlite::Connection;
use std::path::Path;

const PATH_BYTES_MAX: usize = 120;

const TAIL_BYTES_MAX: usize = 32;

/// What an agent's own clone holds beyond its branch tip. Fields: the first
/// `DIRTY_FILES_MAX` paths as text, and how many differ in all, None when git listed more
/// untracked files than it may print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uncommitted {
    pub(crate) paths: Vec<String>,
    pub(crate) total: Option<usize>,
}

/// Lists what an agent's own clone holds beyond its branch tip, for the note at a turn end
/// that landed nothing, reading the clone as `clone_holds_only` does with no git run in it.
/// Tracked edits are named by kind and size first; only when every size matches does git
/// compare contents, so it never hashes a file the agent grew. Inputs: the store and the
/// three paths. Output: None for a linked worktree, an unreadable clone or a clean one.
pub(crate) fn uncommitted(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Option<Uncommitted>, EngineError> {
    let Some(Ok((tip, _))) = super::fetch_clone(db, repository, worktree, git_dir)? else {
        return Ok(None);
    };
    let metadata = repository.join(".git");
    let folder = metadata.join(format!(
        "crazyagent-clean-{}",
        &crate::engine::owner::random_id()?[..24]
    ));
    std::fs::create_dir(&folder).map_err(|_| super::failure("changes_git_failed"))?;
    let listed = super::Repo::open(repository, &metadata, Some(worktree)).and_then(|repo| {
        let (mut paths, mut total) = super::size_mismatches(&repo, worktree, &tip, usize::MAX)?;
        let sizes_held = total == 0;
        let repo = repo.private_index(&folder.join("index")).reading()?;
        repo.run(&["read-tree", &tip])?;
        let mut add = |listing: &[u8], whole: bool| {
            let mut names: Vec<&[u8]> = listing.split(|byte| *byte == 0).collect();
            if !whole {
                names.pop();
            }
            for name in names.into_iter().filter(|name| !name.is_empty()) {
                if paths.len() < crate::landing::DIRTY_FILES_MAX {
                    paths.push(name.to_vec());
                }
                total += 1;
            }
        };
        if sizes_held {
            repo.run(&["update-index", "-q", "--refresh"])?;
            let changed = repo.capture(
                &[
                    "diff-index",
                    "-z",
                    "--name-only",
                    "--ignore-submodules=all",
                    &tip,
                    "--",
                ],
                super::TIP_LISTING_BYTES_MAX,
            )?;
            add(&changed, true);
        }
        let teammates = format!("--exclude=/{}/", crate::agent_git::TEAMMATES_FOLDER);
        let mut listing = vec![
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            teammates.as_str(),
        ];
        let graph: Vec<String> = super::graph_excludes(&repo, &tip)?
            .iter()
            .map(|pattern| format!("--exclude={pattern}"))
            .collect();
        listing.extend(graph.iter().map(String::as_str));
        let untracked = repo.exec(&listing, None, &[], super::TIP_LISTING_BYTES_MAX)?;
        if untracked.code != 0 {
            return Err(super::failure("changes_git_failed"));
        }
        add(&untracked.stdout, !untracked.overflow);
        Ok((paths, (!untracked.overflow).then_some(total)))
    });
    let _ = std::fs::remove_dir_all(&folder);
    let (paths, total) = listed?;
    if paths.is_empty() {
        return Ok(None);
    }
    assert!(paths.len() <= crate::landing::DIRTY_FILES_MAX);
    Ok(Some(Uncommitted {
        paths: paths
            .iter()
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect(),
        total,
    }))
}

/// The owner's note naming what an agent's clone holds uncommitted: as many paths as fit,
/// each with control characters replaced and cut to `PATH_BYTES_MAX`, then how many more.
/// Inputs: what ended and the listing. Output: the line, within `team::NOTE_BYTES_MAX`.
pub(crate) fn note_line(what: &str, held: &Uncommitted) -> String {
    let head = format!(
        "The edits from {what} are not committed, so nothing landed; they stay only in its worktree: "
    );
    named(&head, held, ".", crate::team::NOTE_BYTES_MAX)
}

/// The line an agent's next task turn reads about the edits its clone holds uncommitted,
/// bounded as [`note_line`]. Input: the listing. Output: the line, starting on a new line,
/// within `team::NOTE_BYTES_MAX`.
pub(crate) fn agent_line(held: &Uncommitted) -> String {
    named(
        "\nYour edits from your last turn are not committed, so they did not land; they stay only in your worktree: ",
        held,
        ". Commit them on your branch if they are part of your work.",
        crate::team::NOTE_BYTES_MAX,
    )
}

/// Writes `head`, as many of the listed paths as fit, how many more there are and `end`,
/// within `max` bytes. Inputs: the opening, the listing, the closing and the bound. Output:
/// the line, naming at least one path.
fn named(head: &str, held: &Uncommitted, end: &str, max: usize) -> String {
    assert!(!held.paths.is_empty());
    assert!(head.len() + end.len() + PATH_BYTES_MAX + TAIL_BYTES_MAX <= max);
    let mut line = head.to_owned();
    let mut shown = 0_usize;
    for path in &held.paths {
        let name =
            crate::team::bound_text(&path.replace(char::is_control, "\u{fffd}"), PATH_BYTES_MAX);
        let gap = if shown == 0 { "" } else { ", " };
        if line.len() + gap.len() + name.len() + TAIL_BYTES_MAX + end.len() > max {
            break;
        }
        line.push_str(gap);
        line.push_str(&name);
        shown += 1;
    }
    assert!(shown > 0);
    match held.total {
        Some(total) if total > shown => line.push_str(&format!(" and {} more", total - shown)),
        Some(_) => {}
        None => line.push_str(" and more"),
    }
    line.push_str(end);
    assert!(line.len() <= max);
    line
}
