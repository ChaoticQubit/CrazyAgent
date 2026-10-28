use super::flow::{bounded_read, set_engine_ref};
use super::git::{LISTING_BYTES_MAX, Repo, failure};
use crate::agent_git;
use crate::engine::EngineError;
use crate::worktrees::TreeBounds;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

const FETCH_OUTPUT_BYTES_MAX: usize = 4096;
const HEAD_BYTES_MAX: u64 = 512;
const ALTERNATES_BYTES_MAX: usize = 4096;
const REDIRECTS: [&str; 5] = [".git", "commondir", "shallow", "info/grafts", "../.git.git"];
const PACK_EXTENSIONS: [&str; 7] = ["pack", "idx", "rev", "keep", "promisor", "mtimes", "bitmap"];
const TEMP_KINDS: [&str; 5] = ["pack", "idx", "rev", "mtimes", "bitmap"];
const TEMP_SUFFIX_LEN: usize = 6;
const REPACK_TEMP_EXTENSIONS: [&str; 6] = ["pack", "idx", "rev", "mtimes", "bitmap", "promisor"];
const PID_DIGITS_MAX: usize = 10;

#[cfg(test)]
thread_local! {
    /// A step a landing test runs on the clone's git folder after `fetch` checked it and
    /// before git serves it, standing in for an agent that changes its clone in between.
    /// None runs nothing.
    pub(super) static BETWEEN_CHECK_AND_FETCH: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
}

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
    let line = alternate_line(repository).ok_or_else(invalid)?;
    if !worktree.is_absolute()
        || std::fs::canonicalize(parent).ok().as_deref() != Some(parent)
        || std::fs::symlink_metadata(worktree).is_ok()
        || worktree.starts_with(repository.join(".git"))
        || !std::fs::symlink_metadata(&objects).is_ok_and(|found| found.is_dir())
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
    std::fs::create_dir(worktree).map_err(|_| failure("changes_git_failed"))?;
    let filled = fill(worktree, &branch, &line, format, &head);
    if filled.is_err() {
        let _ = std::fs::remove_dir_all(worktree);
    }
    filled
}

/// Fills the folder `add` just made with the clone, so `add` can remove the folder again
/// when any step fails (GA-R2): private, its git folder made with no template, the
/// alternates line naming the owner's objects, its branch at the owner's commit and
/// checked out with nothing left modified. Inputs: the new worktree, the branch, the
/// alternates line, the object format and the commit. Output: Ok once checked out, else
/// `changes_git_failed` or a git failure.
fn fill(
    worktree: &Path,
    branch: &str,
    line: &str,
    format: &str,
    head: &str,
) -> Result<(), EngineError> {
    let git_dir = agent_git::git_dir(worktree);
    std::fs::set_permissions(worktree, std::fs::Permissions::from_mode(0o700))
        .and_then(|()| std::fs::create_dir(&git_dir))
        .map_err(|_| failure("changes_git_failed"))?;
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
        .and_then(|()| std::fs::write(info.join("alternates"), line))
        .map_err(|_| failure("changes_git_failed"))?;
    store_folders(worktree)?;
    clone.run(&["update-ref", &format!("refs/heads/{branch}"), head])?;
    clone.run(&["read-tree", "-u", "--reset", head])?;
    clone.run(&["update-index", "-q", "--refresh"])?;
    if !clone.run(&["diff-files", "--name-only", "-z"])?.is_empty() {
        return Err(failure("changes_git_failed"));
    }
    let pointer = std::fs::read(git_dir.join("HEAD")).map_err(|_| failure("changes_git_failed"))?;
    assert_eq!(pointer, format!("ref: refs/heads/{branch}\n").as_bytes());
    assert_eq!(clone.commit_of("HEAD")?.as_deref(), Some(head));
    Ok(())
}

/// Makes the clone's pack folder and its 256 loose-object folders `00` to `ff` where any is
/// missing (RL-3), so the Own branch sandbox can deny creating, moving or replacing them
/// while the agent's git still writes objects inside. Each is made in the clone's object
/// folder opened without following a link at the worktree, `.git` or `objects`, so a link
/// the agent put there is refused, not followed; an entry already there is left for
/// `check_store`. Input: the worktree. Output: Ok, else `changes_workspace_invalid`.
pub(super) fn store_folders(worktree: &Path) -> Result<(), EngineError> {
    use rustix::fs::{CWD, Mode, OFlags, mkdirat, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let git = openat(&root, ".git", flags, Mode::empty()).map_err(|_| invalid())?;
    let objects = openat(&git, "objects", flags, Mode::empty()).map_err(|_| invalid())?;
    let names =
        std::iter::once("pack".to_owned()).chain((0..=u8::MAX).map(|byte| format!("{byte:02x}")));
    for name in names {
        match mkdirat(&objects, name.as_str(), Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(invalid()),
        }
    }
    Ok(())
}

/// The one alternates line `add` writes and `fetch` requires: the owner repository's
/// object folder and a newline. Input: the owner checkout root. Output: the line, or none
/// for a path that is not text or holds a control character.
fn alternate_line(repository: &Path) -> Option<String> {
    let objects = repository.join(".git").join("objects");
    let alternate = objects.to_str()?;
    (!alternate.chars().any(char::is_control)).then(|| format!("{alternate}\n"))
}

/// Checks, right before git serves the clone, that its object store holds only its own
/// files and the project's store (DEC102-F1, AL-1): its git, object, info and pack folders
/// are real folders, its alternates file is a plain file with one link holding exactly the
/// line `add` wrote, it has no HTTP alternates, and every pack file and loose object is a
/// plain file with one link, never a link into another store. Nothing in its git folder
/// names another repository, store or history (`REDIRECTS`): no nested `.git`, which git
/// would serve in its place, no `.git.git` beside it, which git serves when the git folder
/// fails its repository test, no `commondir`, which moves its objects and refs elsewhere,
/// and no `shallow` or `info/grafts` file, which rewrite the history it serves. Its pack
/// folder holds only git's own `pack-<id>` and temp files (`pack_file`), never a
/// `multi-pack-index` that could name a pack outside it (RL-2), nor a repack temp pack git
/// would read (`repack_temp`). The Own
/// branch sandbox keeps the agent from changing these while the fetch runs. Those files also
/// bound what the fetch can carry into the project (DEC102-F2): at most `bounds.files` of
/// them, of at most `bounds.bytes` together, objects the project already has included until
/// the agent's git repacks them away. Inputs: the owner checkout root, the clone's git
/// folder and the bounds. Output: Ok, else `changes_workspace_invalid` or
/// `changes_too_large`.
fn check_store(repository: &Path, git_dir: &Path, bounds: &TreeBounds) -> Result<(), EngineError> {
    let objects = git_dir.join("objects");
    let info = objects.join("info");
    let pack = objects.join("pack");
    let folder = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|found| found.is_dir());
    let alternates = info.join("alternates");
    let single = std::fs::symlink_metadata(&alternates)
        .is_ok_and(|found| found.is_file() && found.nlink() == 1);
    let line = alternate_line(repository).ok_or_else(invalid)?;
    if ![git_dir, objects.as_path(), info.as_path(), pack.as_path()]
        .into_iter()
        .all(folder)
        || std::fs::symlink_metadata(info.join("http-alternates")).is_ok()
        || !single
        || bounded_read(&alternates, ALTERNATES_BYTES_MAX)? != line.as_bytes()
        || REDIRECTS.iter().any(|name| {
            !matches!(
                std::fs::symlink_metadata(git_dir.join(name)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        })
    {
        return Err(invalid());
    }
    let mut stored = vec![pack.clone()];
    for entry in std::fs::read_dir(&objects).map_err(|_| invalid())? {
        let entry = entry.map_err(|_| invalid())?;
        let fanout = entry.file_name().to_str().is_some_and(|name| {
            name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if fanout {
            stored.push(entry.path());
        }
    }
    let (mut files, mut bytes) = (0_usize, 0_u64);
    let (mut temp_indexes, mut temp_packs) = (Vec::new(), Vec::new());
    for held in &stored {
        if !folder(held) {
            return Err(invalid());
        }
        for entry in std::fs::read_dir(held).map_err(|_| invalid())? {
            let entry = entry.map_err(|_| invalid())?;
            let found = std::fs::symlink_metadata(entry.path()).map_err(|_| invalid())?;
            if !found.is_file()
                || found.nlink() != 1
                || (*held == pack && !pack_file(&entry.file_name()))
            {
                return Err(invalid());
            }
            files += 1;
            bytes = bytes.saturating_add(found.len());
            if files > bounds.files || bytes > bounds.bytes {
                return Err(failure("changes_too_large"));
            }
            if *held == pack {
                match entry.file_name().to_str().and_then(repack_temp) {
                    Some((name, "idx")) => temp_indexes.push(name.to_owned()),
                    Some((name, "pack")) => temp_packs.push(name.to_owned()),
                    _ => {}
                }
            }
        }
    }
    if temp_indexes.iter().any(|name| temp_packs.contains(name)) {
        return Err(invalid());
    }
    Ok(())
}

/// Tells whether a name in the clone's pack folder is one of git's own pack files:
/// `pack-`, a lower-case SHA-1 or SHA-256 hex id and one of `PACK_EXTENSIONS`, a temp
/// file an interrupted git command left, `tmp_<kind>_` and six letters or digits, which
/// git never reads as a pack since only names ending in `.idx` are, or one an interrupted
/// `git repack` left (`repack_temp`). Input: the file name. Output: the answer; a
/// `multi-pack-index` or any other name is not one.
fn pack_file(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if let Some((kind, suffix)) = name
        .strip_prefix("tmp_")
        .and_then(|rest| rest.split_once('_'))
    {
        return TEMP_KINDS.contains(&kind)
            && suffix.len() == TEMP_SUFFIX_LEN
            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric());
    }
    if repack_temp(name).is_some() {
        return true;
    }
    let Some((id, extension)) = name
        .strip_prefix("pack-")
        .and_then(|rest| rest.split_once('.'))
    else {
        return false;
    };
    pack_id(id) && PACK_EXTENSIONS.contains(&extension)
}

/// Splits a file an interrupted `git repack` left in the pack folder, named as git's
/// repack names its new pack before moving it into place, `.tmp-<pid>-pack-<id>` and one
/// of `REPACK_TEMP_EXTENSIONS`, with the pid in decimal and no leading zero. Git reads
/// such a pack, and the fetch would serve it, when its `.idx` and `.pack` are both there,
/// so `check_store` refuses that pair. Input: the file name. Output: the name without its
/// extension and the extension, or None for any other name.
fn repack_temp(name: &str) -> Option<(&str, &str)> {
    let (pid, rest) = name.strip_prefix(".tmp-")?.split_once("-pack-")?;
    let (id, extension) = rest.split_once('.')?;
    let decimal = (1..=PID_DIGITS_MAX).contains(&pid.len())
        && !pid.starts_with('0')
        && pid.bytes().all(|byte| byte.is_ascii_digit());
    (decimal && pack_id(id) && REPACK_TEMP_EXTENSIONS.contains(&extension))
        .then(|| (&name[..name.len() - extension.len() - 1], extension))
}

/// Tells whether a pack id is git's: a lower-case SHA-1 or SHA-256 hex digest.
/// Input: the id. Output: the answer.
fn pack_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64)
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks that the clone's own config reads nothing from outside the clone (DEC102-F1): no
/// `include` or `includeIf` path, no `extensions.*` key, such as `worktreeConfig`,
/// `refStorage` or `partialClone`, and no promisor remote, from which git would fetch an
/// object the clone lacks. Git lists the key names of the config file alone, never
/// following its includes. Inputs: the owner repository and the clone's git folder.
/// Output: Ok, else `changes_workspace_invalid`.
fn check_config(shared: &Repo, git_dir: &Path) -> Result<(), EngineError> {
    let config = git_dir.join("config");
    let path = config.to_str().ok_or_else(invalid)?;
    if !std::fs::symlink_metadata(&config).is_ok_and(|found| found.is_file()) {
        return Err(invalid());
    }
    let listed = shared.exec(
        &[
            "config",
            "--file",
            path,
            "--no-includes",
            "--list",
            "--name-only",
            "-z",
        ],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    let reaches_out = listed.stdout.split(|byte| *byte == 0).any(|key| {
        key.starts_with(b"include.")
            || key.starts_with(b"includeif.")
            || key.starts_with(b"extensions.")
            || (key.starts_with(b"remote.") && key.ends_with(b".promisor"))
    });
    if listed.code != 0 || listed.overflow || reaches_out {
        return Err(invalid());
    }
    Ok(())
}

/// Copies an agent's branch from its own clone into the owner repository under an
/// incoming ref of this fetch alone and reads its tip, then drops that ref, so fetches of
/// one clone never meet on a ref and need no repository hold. Git's upload-pack serves the
/// clone, the one git command safe on a repository the agent configured, and the owner
/// repository's fetch hashes every object it receives again and checks it with fsck, so a
/// planted or mislabelled object in the clone is refused rather than stored. Nothing in
/// the clone is written and no owner branch moves. The clone's object store is checked
/// and measured first (`check_store`) and its config listed (`check_config`), so git never
/// reads objects from a store the agent named or linked in, nor carries more than the bounds
/// allow. Upload-pack runs `--strict`, so it serves that git folder itself and never a
/// `.git` inside it or a `.git.git` beside it, even one made after the checks (RL-1), and
/// with lazy fetching off, so it never asks
/// a promisor remote for an object. Inputs: the owner
/// repository opened for object work, its root, the agent's worktree and the bounds.
/// Output: the tip, else `changes_workspace_invalid` for a clone that is not a real folder
/// or whose store reaches elsewhere, `changes_too_large` past the bounds or
/// `changes_agent_branch_invalid` for a branch git cannot fetch.
pub(super) fn fetch(
    shared: &Repo,
    repository: &Path,
    worktree: &Path,
    bounds: &TreeBounds,
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
    check_store(repository, &source, bounds)?;
    check_config(shared, &source)?;
    #[cfg(test)]
    if let Some(step) = BETWEEN_CHECK_AND_FETCH.get() {
        step(&source);
    }
    let source = source.to_str().ok_or_else(invalid)?;
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| failure("changes_unavailable"))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let incoming = format!("refs/crazyagent/incoming/{folder}-{nonce}");
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
            "--upload-pack=git-upload-pack --strict",
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
