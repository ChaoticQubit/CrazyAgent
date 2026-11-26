use super::{Repo, TIP_LISTING_BYTES_MAX, failure, graph_excludes, store};
use crate::agent_git;
use crate::engine::EngineError;
use rusqlite::Connection;
use std::path::Path;

/// What an agent worktree holds where git ignores files. Fields: the first
/// `DIRTY_FILES_MAX` entries as text, a folder git ignores whole named once with a trailing
/// `/`, and how many there are in all, None when git listed more than it may print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ignored {
    pub(crate) paths: Vec<String>,
    pub(crate) total: Option<usize>,
}

/// Lists the entries an agent worktree holds that git ignores, read before a removal that
/// takes them along. The owner repository's git reads the worktree against a private index
/// holding the commit the worktree was recorded to start from, so neither the agent's own
/// git metadata nor the owner's index is used. The teammates' folder and the code graph's
/// copy are engine-made and left out. Inputs: the store, the owner checkout root and the
/// worktree. Output: None when it holds no such entry or has no recorded base.
pub(crate) fn ignored(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
) -> Result<Option<Ignored>, EngineError> {
    let Some(text) = worktree.to_str() else {
        return Ok(None);
    };
    let Some(base) = store::base_of(db, text)? else {
        return Ok(None);
    };
    let metadata = repository.join(".git");
    let folder = metadata.join(format!(
        "crazyagent-ignored-{}",
        &crate::engine::owner::random_id()?[..24]
    ));
    std::fs::create_dir(&folder).map_err(|_| failure("changes_git_failed"))?;
    let listed = Repo::open(repository, &metadata, Some(worktree)).and_then(|repo| {
        let repo = repo.private_index(&folder.join("index")).reading()?;
        repo.run(&["read-tree", &base])?;
        let graph = graph_excludes(&repo, &base)?;
        let output = repo.exec(
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
            None,
            &[],
            TIP_LISTING_BYTES_MAX,
        )?;
        Ok((graph, output))
    });
    let _ = std::fs::remove_dir_all(&folder);
    let (graph, output) = listed?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    let engine_made: Vec<String> = std::iter::once(agent_git::TEAMMATES_FOLDER)
        .chain(graph.iter().copied())
        .map(|name| format!("{}/", name.trim_matches('/')))
        .collect();
    let mut names: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
    if output.overflow {
        names.pop();
    }
    let mut paths = Vec::new();
    let mut total = 0_usize;
    for name in names.into_iter().filter(|name| !name.is_empty()) {
        if engine_made
            .iter()
            .any(|made| name.starts_with(made.as_bytes()))
        {
            continue;
        }
        if paths.len() < crate::landing::DIRTY_FILES_MAX {
            paths.push(String::from_utf8_lossy(name).into_owned());
        }
        total += 1;
    }
    if paths.is_empty() {
        return Ok(None);
    }
    assert!(paths.len() <= total.min(crate::landing::DIRTY_FILES_MAX));
    Ok(Some(Ignored {
        paths,
        total: (!output.overflow).then_some(total),
    }))
}
