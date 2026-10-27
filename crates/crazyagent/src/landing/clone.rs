use super::flow::set_engine_ref;
use super::git::{Repo, failure};
use crate::agent_git;
use crate::engine::EngineError;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const FETCH_OUTPUT_BYTES_MAX: usize = 4096;
const HEAD_BYTES_MAX: u64 = 512;

fn invalid() -> EngineError {
    failure("changes_workspace_invalid")
}

/// Makes an agent's own clone of the owner repository in a new `worktree` folder (DEC-102
/// Own branch): a git folder of its own at `agent_git::git_dir`, created by the engine
/// with no template, hooks or config beyond git's defaults, that reads the project's
/// objects through `objects/info/alternates` and never writes them, its branch
/// `agent_git::branch` at the owner's HEAD commit and checked out, with no command the
/// owner's repository configures run. Inputs: the owner checkout root and the new
/// worktree folder, absolute, whose parent exists. Output: Ok once the clone holds the
/// commit, else `changes_workspace_invalid`, `changes_git_unsupported` or
/// `changes_git_failed`.
pub(super) fn add(repository: &Path, worktree: &Path) -> Result<(), EngineError> {
    let branch = agent_git::branch(worktree).ok_or_else(invalid)?;
    let parent = worktree.parent().ok_or_else(invalid)?;
    let objects = repository.join(".git").join("objects");
    let alternate = objects.to_str().ok_or_else(invalid)?;
    if !worktree.is_absolute()
        || std::fs::canonicalize(parent).ok().as_deref() != Some(parent)
        || std::fs::symlink_metadata(worktree).is_ok()
        || worktree.starts_with(repository.join(".git"))
        || !std::fs::symlink_metadata(&objects).is_ok_and(|found| found.is_dir())
        || alternate.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let shared = Repo::open(&repository.join(".git"), None)?;
    let head = shared
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    let format = shared.run(&["rev-parse", "--show-object-format"])?;
    let format = match format.as_slice() {
        b"sha1\n" => "sha1",
        b"sha256\n" => "sha256",
        _ => return Err(failure("changes_git_unsupported")),
    };
    let git_dir = agent_git::git_dir(worktree);
    let made = std::fs::create_dir(worktree)
        .and_then(|()| std::fs::set_permissions(worktree, std::fs::Permissions::from_mode(0o700)))
        .and_then(|()| std::fs::create_dir(&git_dir));
    made.map_err(|_| failure("changes_git_failed"))?;
    let clone = Repo::fresh(&git_dir, worktree);
    clone.run(&[
        "init",
        "-q",
        "--template=",
        &format!("--object-format={format}"),
        &format!("--initial-branch={branch}"),
    ])?;
    let info = git_dir.join("objects").join("info");
    std::fs::create_dir_all(&info)
        .and_then(|()| std::fs::write(info.join("alternates"), format!("{alternate}\n")))
        .map_err(|_| failure("changes_git_failed"))?;
    clone.run(&["update-ref", &format!("refs/heads/{branch}"), &head])?;
    clone.run(&["read-tree", "-u", "--reset", &head])?;
    clone.run(&["update-index", "-q", "--refresh"])?;
    if !clone.run(&["diff-files", "--name-only", "-z"])?.is_empty() {
        return Err(failure("changes_git_failed"));
    }
    let pointer = std::fs::read(git_dir.join("HEAD")).map_err(|_| failure("changes_git_failed"))?;
    assert_eq!(pointer, format!("ref: refs/heads/{branch}\n").as_bytes());
    assert_eq!(clone.commit_of("HEAD")?.as_deref(), Some(head.as_str()));
    Ok(())
}

/// Copies an agent's branch from its own clone into the owner repository under the
/// engine's incoming ref and reads its tip, then drops that ref. Git's upload-pack serves
/// the clone, the one git command safe on a repository the agent configured, and the
/// owner repository's fetch hashes every object it receives again and checks it with
/// fsck, so a planted or mislabelled object in the clone is refused rather than stored.
/// Nothing in the clone is written and no owner branch moves. Inputs: the owner
/// repository opened for object work, its root and the agent's worktree. Output: the
/// tip, else `changes_workspace_invalid` for a clone that is not a real folder or
/// `changes_agent_branch_invalid` for a branch git cannot fetch.
pub(super) fn fetch(
    shared: &Repo,
    repository: &Path,
    worktree: &Path,
) -> Result<String, EngineError> {
    let branch = agent_git::branch(worktree).ok_or_else(invalid)?;
    let folder = branch.trim_start_matches("agent/");
    let source = agent_git::git_dir(worktree);
    if !std::fs::symlink_metadata(&source).is_ok_and(|found| found.is_dir())
        || !std::fs::symlink_metadata(source.join("HEAD"))
            .is_ok_and(|found| found.is_file() && found.len() <= HEAD_BYTES_MAX)
    {
        return Err(invalid());
    }
    let source = source.to_str().ok_or_else(invalid)?;
    let incoming = format!("refs/crazyagent/incoming/{folder}");
    let refspec = format!("+refs/heads/{branch}:{incoming}");
    let fetched = shared.exec(
        &[
            "-c",
            "protocol.file.allow=always",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "fetch.fsckObjects=true",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--end-of-options",
            source,
            &refspec,
        ],
        None,
        &[],
        FETCH_OUTPUT_BYTES_MAX,
    )?;
    if fetched.code != 0 {
        return Err(failure("changes_agent_branch_invalid"));
    }
    let tip = shared
        .commit_of(&incoming)?
        .ok_or_else(|| failure("changes_agent_branch_invalid"));
    set_engine_ref(shared, repository, &incoming, None)?;
    tip
}
