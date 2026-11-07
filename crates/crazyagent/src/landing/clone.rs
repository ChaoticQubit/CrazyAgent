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
const REF_ENTRIES_MAX: usize = 1024;
const REF_DEPTH_MAX: usize = 16;
const PACKED_REFS_BYTES_MAX: u64 = 65_536;
const REFS_LISTED_MAX: usize = 16;
const BASE_NAME_BYTES_MAX: usize = 128;
const REF_STORES: [&str; 2] = ["reftable", "worktrees"];
const TEAMMATES_MAX: usize = 64;
const MAIN_LOCK: &str = "main.lock";
/// The engine's ref in every agent clone, and in the owner repository for linked
/// worktrees, that holds the tip of the project's Land on branch, so an agent brings main
/// into its own branch with `git merge crazyagent/main` (OS-8).
pub(super) const MAIN_REF: &str = "refs/crazyagent/main";

#[cfg(test)]
thread_local! {
    /// A step a landing test runs on the clone's git folder after `fetch` checked it and
    /// before git serves it, standing in for an agent that changes its clone in between.
    /// None runs nothing.
    pub(super) static BETWEEN_CHECK_AND_FETCH: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
}

/// The engine ref in the owner repository that keeps the commit an agent's own clone
/// borrows as its base, so the owner's gc never drops objects the clone reads through its
/// alternates (GA-R5). Input: the clone folder. Output: the ref, or
/// `changes_workspace_invalid` for a folder whose name is not one plain ref part.
pub(super) fn base_ref(worktree: &Path) -> Result<String, EngineError> {
    let name = worktree
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let plain = !name.is_empty()
        && name.len() <= BASE_NAME_BYTES_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !plain {
        return Err(invalid());
    }
    Ok(format!("refs/crazyagent/base/{name}"))
}

fn invalid() -> EngineError {
    failure("changes_workspace_invalid")
}

/// Makes an agent's own clone of the owner repository in a new `worktree` folder (DEC-102
/// Own branch): a git folder of its own at `agent_git::git_dir`, created by the engine
/// with no template, hooks or config beyond git's defaults, that reads the project's
/// objects through `objects/info/alternates` and never writes them, its branch at the
/// owner's HEAD commit and checked out, with no command the owner's repository configures
/// run. A clone cut at another commit, such as the Master's branch tip, starts there
/// instead; the owner repository already holds it. Inputs: the owner checkout root, the
/// new worktree folder, absolute, whose parent exists, that commit, None for the owner's
/// HEAD, and the branch, `agent_git::branch` or a task branch. Output: Ok once the clone
/// holds the commit, else `changes_workspace_invalid`, `changes_git_unsupported` or
/// `changes_git_failed`.
pub(super) fn add(
    repository: &Path,
    worktree: &Path,
    base: Option<&str>,
    branch: &str,
) -> Result<(), EngineError> {
    if agent_git::branch(worktree).is_none() {
        return Err(invalid());
    }
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
    let shared = Repo::open(repository, &repository.join(".git"), None)?;
    let head = shared
        .commit_of(base.unwrap_or("HEAD"))?
        .ok_or_else(|| failure("changes_git_failed"))?;
    let format = shared.run(&["rev-parse", "--show-object-format"])?;
    let format = match format.as_slice() {
        b"sha1\n" => "sha1",
        b"sha256\n" => "sha256",
        _ => return Err(failure("changes_git_unsupported")),
    };
    std::fs::create_dir(worktree).map_err(|_| failure("changes_git_failed"))?;
    let filled = fill(worktree, branch, &line, format, &head);
    if filled.is_err() {
        let _ = std::fs::remove_dir_all(worktree);
    }
    filled
}

/// Fills the folder `add` just made with the clone, so `add` can remove the folder again
/// when any step fails (GA-R2): private, its git folder made with no template, the
/// alternates line naming the owner's objects, an `info/exclude` that keeps the
/// teammates' worktree folder and the project graph the engine shows there out of what the
/// agent's own `git add` takes, its branch at
/// the owner's commit and checked out with nothing left modified. Inputs: the new worktree, the branch, the
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
    std::fs::create_dir_all(git_dir.join("info"))
        .and_then(|()| {
            std::fs::write(
                git_dir.join("info").join("exclude"),
                format!(
                    "/{}/\n{}\n",
                    agent_git::TEAMMATES_FOLDER,
                    agent_git::GRAPH_EXCLUDE
                ),
            )
        })
        .map_err(|_| failure("changes_git_failed"))?;
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

/// The most bytes of a clone's `info/exclude` that `exclude_graph` reads and appends to.
const EXCLUDE_BYTES_MAX: u64 = 256 * 1024;

/// Adds the graph line to a clone's `info/exclude` when no line there keeps the project graph
/// out of git yet, as `fill` writes it for every new clone, so a clone made before that
/// shows the graph too (18.15.1). Every step goes through its folder's own descriptor
/// without following a link, and only a plain file with no other hard link (F1) of at most
/// `EXCLUDE_BYTES_MAX` bytes is read and appended to. Anything else, or a failed write,
/// leaves the clone without the graph, never without its launch. Takes the clone's worktree.
pub(super) fn exclude_graph(worktree: &Path) {
    use rustix::fs::{CWD, FileType, Mode, OFlags, fstat, openat};
    use std::io::{Read, Write};
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let append = OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let opened = openat(CWD, worktree, folder, Mode::empty())
        .and_then(|tree| openat(&tree, ".git", folder, Mode::empty()))
        .and_then(|git| openat(&git, "info", folder, Mode::empty()))
        .and_then(|info| openat(&info, "exclude", append | OFlags::CLOEXEC, Mode::empty()));
    let Ok(fd) = opened else {
        return;
    };
    let plain = fstat(&fd).is_ok_and(|stat| {
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_nlink == 1
            && u64::try_from(stat.st_size).is_ok_and(|size| size <= EXCLUDE_BYTES_MAX)
    });
    let mut file = std::fs::File::from(fd);
    let mut text = String::new();
    if !plain
        || Read::by_ref(&mut file)
            .take(EXCLUDE_BYTES_MAX)
            .read_to_string(&mut text)
            .is_err()
        || agent_git::graph_rule(&text) == Some(true)
    {
        return;
    }
    assert!(text.len() as u64 <= EXCLUDE_BYTES_MAX);
    let lead = match text.is_empty() || text.ends_with('\n') {
        true => "",
        false => "\n",
    };
    let line = format!("{lead}{}\n", agent_git::GRAPH_EXCLUDE);
    let _ = file
        .write_all(line.as_bytes())
        .and_then(|()| file.sync_all());
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

/// Lists the refs an agent's own clone holds besides its branch, read as data from its git
/// folder with no git run and no link followed (CC-1): each file under `refs`, each name in
/// `packed-refs`, `HEAD` unless it is the symbolic ref to the branch, and `reftable` or
/// `worktrees` when either exists, since git keeps refs there too. The engine's clone makes
/// no ref but its branch and [`MAIN_REF`], which is left out as well. With `teammates`, the
/// teammate branches of the branch (`agent_git::is_teammate_branch`) and the `worktrees`
/// folder are left out, for a caller that checks those itself. Inputs: the worktree, its branch and `teammates`. Output: at
/// most `REFS_LISTED_MAX` names, empty when nothing else is held, else
/// `changes_workspace_invalid` for ref data that is not a plain file or folder or
/// `changes_too_large` past the bounds, never a clean read.
pub(super) fn other_refs(
    worktree: &Path,
    branch: &str,
    teammates: bool,
) -> Result<Vec<String>, EngineError> {
    use rustix::fs::{AtFlags, CWD, Mode, OFlags, openat, statat};
    let own = format!("refs/heads/{branch}");
    let task = |name: &str| {
        teammates
            && name
                .strip_prefix("refs/heads/")
                .is_some_and(|name| agent_git::is_teammate_branch(branch, name))
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let git = openat(&root, ".git", flags, Mode::empty()).map_err(|_| invalid())?;
    let mut found = Vec::new();
    let pointer = format!("ref: {own}\n");
    if read_at(&git, "HEAD", HEAD_BYTES_MAX)?.as_deref() != Some(pointer.as_bytes()) {
        listed(&mut found, "HEAD".to_owned());
    }
    for store in REF_STORES {
        if teammates && store == "worktrees" {
            continue;
        }
        match statat(&git, store, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => listed(&mut found, store.to_owned()),
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return Err(invalid()),
        }
    }
    let packed = read_at(&git, "packed-refs", PACKED_REFS_BYTES_MAX)?.unwrap_or_default();
    for line in packed.split(|byte| *byte == b'\n') {
        if line.is_empty() || line.starts_with(b"#") || line.starts_with(b"^") {
            continue;
        }
        let space = line
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(invalid)?;
        let name = String::from_utf8_lossy(&line[space + 1..]).into_owned();
        if name != own && name != MAIN_REF && !task(&name) {
            listed(&mut found, name);
        }
    }
    let refs = openat(&git, "refs", flags, Mode::empty()).map_err(|_| invalid())?;
    let mut budget = REF_ENTRIES_MAX;
    let mut loose = Vec::new();
    loose_refs(&refs, "refs", 0, &mut budget, &own, &mut loose)?;
    for name in loose.into_iter().filter(|name| !task(name)) {
        listed(&mut found, name);
    }
    Ok(found)
}

/// Points [`MAIN_REF`] in an agent's own clone at the project's landed tip with no git run
/// in the clone, whose config is the agent's (CC-N1): `refs` and `crazyagent` opened
/// without following a link and made when missing, the ref written to a new `main.lock`
/// and renamed over `main`, as git itself writes a loose ref. The clone reads the commit
/// through its alternates. Inputs: the owner checkout root, the worktree and the tip.
/// Output: Ok, else `changes_workspace_invalid` for a link or a foreign entry on the way.
pub(super) fn give_main(repository: &Path, worktree: &Path, tip: &str) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, CWD, Mode, OFlags, mkdirat, openat, renameat, unlinkat};
    use std::io::Write;
    if !super::git::is_object_id(tip) {
        return Err(failure("changes_git_failed"));
    }
    if super::flow::check_places(repository, worktree, &agent_git::git_dir(worktree))?
        != agent_git::Layout::Clone
    {
        return Err(invalid());
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let mut folder = openat(&root, ".git", flags, Mode::empty()).map_err(|_| invalid())?;
    for part in ["refs", "crazyagent"] {
        match mkdirat(&folder, part, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(invalid()),
        }
        folder = openat(&folder, part, flags, Mode::empty()).map_err(|_| invalid())?;
    }
    let created = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let lock = openat(&folder, MAIN_LOCK, created, Mode::from_raw_mode(0o644))
        .map_err(|_| invalid())?;
    let mut file = std::fs::File::from(lock);
    let written = file
        .write_all(format!("{tip}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("changes_git_failed"))
        .and_then(|()| renameat(&folder, MAIN_LOCK, &folder, "main").map_err(|_| invalid()));
    if written.is_err() {
        let _ = unlinkat(&folder, MAIN_LOCK, AtFlags::empty());
    }
    written
}

/// Says whether an agent's own clone still holds its branch, read as data from its git
/// folder with no git run and no link followed: a loose ref file, a `packed-refs` line, or
/// a `reftable` store, which the engine does not read and so counts as holding it. Inputs:
/// the worktree and its branch. Output: the answer, else `changes_workspace_invalid` for
/// ref data that is not a plain file or folder or `changes_too_large` past the bounds.
pub(super) fn has_branch(worktree: &Path, branch: &str) -> Result<bool, EngineError> {
    use rustix::fs::{AtFlags, CWD, Mode, OFlags, openat, statat};
    let own = format!("refs/heads/{branch}");
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let git = openat(&root, ".git", flags, Mode::empty()).map_err(|_| invalid())?;
    match statat(&git, "reftable", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => return Ok(true),
        Err(rustix::io::Errno::NOENT) => {}
        Err(_) => return Err(invalid()),
    }
    let parts: Vec<&str> = own.split('/').collect();
    let (name, folders) = parts.split_last().ok_or_else(invalid)?;
    let mut folder: Option<rustix::fd::OwnedFd> = None;
    let mut loose = true;
    for part in folders {
        match openat(folder.as_ref().unwrap_or(&git), *part, flags, Mode::empty()) {
            Ok(opened) => folder = Some(opened),
            Err(rustix::io::Errno::NOENT) => {
                loose = false;
                break;
            }
            Err(_) => return Err(invalid()),
        }
    }
    if loose
        && let Some(folder) = &folder
        && read_at(folder, name, HEAD_BYTES_MAX)?.is_some()
    {
        return Ok(true);
    }
    let packed = read_at(&git, "packed-refs", PACKED_REFS_BYTES_MAX)?.unwrap_or_default();
    Ok(packed.split(|byte| *byte == b'\n').any(|line| {
        !line.starts_with(b"#")
            && !line.starts_with(b"^")
            && line
                .iter()
                .position(|byte| *byte == b' ')
                .is_some_and(|space| &line[space + 1..] == own.as_bytes())
    }))
}

/// Walks one folder of a clone's loose refs for `other_refs`, opening each inner folder
/// without following a link, at most `REF_DEPTH_MAX` deep and `REF_ENTRIES_MAX` entries in
/// all. Inputs: the open folder, its ref path, its depth, the entries left, the branch's
/// ref and the names found. Output: Ok, else `changes_workspace_invalid` for an entry that
/// is not a plain file or folder, or `changes_too_large`.
fn loose_refs(
    folder: &rustix::fd::OwnedFd,
    path: &str,
    depth: usize,
    budget: &mut usize,
    own: &str,
    found: &mut Vec<String>,
) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
    if depth > REF_DEPTH_MAX {
        return Err(failure("changes_too_large"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for entry in Dir::read_from(folder).map_err(|_| invalid())? {
        let entry = entry.map_err(|_| invalid())?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| failure("changes_too_large"))?;
        let inner = format!("{path}/{}", name.to_string_lossy());
        let kind = statat(folder, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| invalid())?;
        match FileType::from_raw_mode(kind.st_mode) {
            FileType::Directory => {
                let opened = openat(folder, name, flags, Mode::empty()).map_err(|_| invalid())?;
                loose_refs(&opened, &inner, depth + 1, budget, own, found)?;
            }
            FileType::RegularFile if inner == own || inner == MAIN_REF => {}
            FileType::RegularFile => listed(found, inner),
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

/// Adds a name to `other_refs`'s list while it holds fewer than `REFS_LISTED_MAX`, so the
/// list stays capped while the walk still reads every entry within its bounds. Inputs: the
/// list and the name.
fn listed(found: &mut Vec<String>, name: String) {
    if found.len() < REFS_LISTED_MAX {
        found.push(name);
    }
}

/// Reads a small file in a clone's git folder an agent may have replaced: opened without
/// following a link and without waiting on a FIFO or device, kept only when it is a regular
/// file of at most `limit` bytes. Inputs: the open folder, the name and the limit. Output:
/// the bytes, None when it does not exist, else `changes_workspace_invalid` or
/// `changes_too_large`.
fn read_at(
    folder: &rustix::fd::OwnedFd,
    name: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>, EngineError> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::io::Read;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match openat(folder, name, flags, Mode::empty()) {
        Ok(opened) => std::fs::File::from(opened),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(invalid()),
    };
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > limit {
        return Err(failure("changes_too_large"));
    }
    Ok(Some(bytes))
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
/// a promisor remote for an object. A tip that holds the teammates' worktree folder at its
/// root is read without it (`leave_out_teammates`), so a harness's change set never carries
/// its teammates' worktrees. Inputs: the owner
/// repository opened for object work, its root, the agent's worktree, its branch and the
/// bounds.
/// Output: the tip, else `changes_workspace_invalid` for a clone that is not a real folder
/// or whose store reaches elsewhere, `changes_too_large` past the bounds or
/// `changes_agent_branch_invalid` for a branch git cannot fetch.
pub(super) fn fetch(
    shared: &Repo,
    repository: &Path,
    worktree: &Path,
    branch: &str,
    bounds: &TreeBounds,
) -> Result<String, EngineError> {
    let fetched = fetch_refs(
        shared,
        repository,
        worktree,
        &format!("refs/heads/{branch}"),
        bounds,
    )?;
    let [(_, tip)] = fetched.as_slice() else {
        return Err(failure("changes_agent_branch_invalid"));
    };
    leave_out_teammates(shared, tip)
}

/// Copies the teammate branches of a harness's task branch (`agent_git::is_teammate_branch`)
/// from its clone into the owner repository as `fetch` copies the branch itself, so the
/// engine can tell whether each one's work is already in the task branch. Inputs: as
/// `fetch`. Output: each teammate branch with its tip, at most `TEAMMATES_MAX`, else as
/// `fetch`, or `changes_too_large` for more.
pub(super) fn fetch_teammates(
    shared: &Repo,
    repository: &Path,
    worktree: &Path,
    branch: &str,
    bounds: &TreeBounds,
) -> Result<Vec<(String, String)>, EngineError> {
    let pattern = format!("refs/heads/{branch}--*");
    let fetched = fetch_refs(shared, repository, worktree, &pattern, bounds)?;
    if fetched.len() > TEAMMATES_MAX {
        return Err(failure("changes_too_large"));
    }
    Ok(fetched
        .into_iter()
        .filter_map(|(name, tip)| {
            let name = name.strip_prefix("refs/heads/")?.to_owned();
            agent_git::is_teammate_branch(branch, &name).then_some((name, tip))
        })
        .collect())
}

/// Runs the checked fetch `fetch` describes for one ref, or for a pattern with one `*`,
/// into incoming refs of this fetch alone, reads what arrived and drops those refs.
/// Inputs: the owner repository opened for object work, its root, the agent's worktree,
/// the clone's ref or pattern and the bounds. Output: each fetched ref of the clone with
/// its commit, else as `fetch`.
fn fetch_refs(
    shared: &Repo,
    repository: &Path,
    worktree: &Path,
    wanted: &str,
    bounds: &TreeBounds,
) -> Result<Vec<(String, String)>, EngineError> {
    let folder = agent_git::branch(worktree).ok_or_else(invalid)?;
    let folder = folder.trim_start_matches("agent/");
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
    let pattern = wanted.split_once('*');
    let refspec = match pattern {
        Some(_) => format!("+{wanted}:{incoming}/*"),
        None => format!("+{wanted}:{incoming}"),
    };
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
    let Some((before, after)) = pattern else {
        let tip = shared
            .commit_of(&incoming)?
            .ok_or_else(|| failure("changes_agent_branch_invalid"));
        set_engine_ref(shared, repository, &incoming, None)?;
        return Ok(vec![(wanted.to_owned(), tip?)]);
    };
    let listed = shared.capture(
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            &format!("{incoming}/"),
        ],
        LISTING_BYTES_MAX,
    );
    let mut found = Vec::new();
    let mut names = Vec::new();
    for line in listed
        .as_deref()
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
    {
        let Some((id, name)) = std::str::from_utf8(line)
            .ok()
            .and_then(|line| line.split_once(' '))
        else {
            continue;
        };
        names.push(name.to_owned());
        let matched = name.strip_prefix(&format!("{incoming}/"));
        if let Some(matched) = matched
            && super::git::is_object_id(id)
            && found.len() <= TEAMMATES_MAX
        {
            found.push((format!("{before}{matched}{after}"), id.to_owned()));
        }
    }
    for name in &names {
        set_engine_ref(shared, repository, name, None)?;
    }
    listed?;
    Ok(found)
}

/// Reads a harness's tip without the teammates' worktree folder at its root: the tip
/// itself when it holds none, else a commit the engine makes above it with that one root
/// entry left out, authored and dated as the tip's committer date by the engine, so the
/// same tip always gives the same commit and bases recorded from it stay comparable.
/// Inputs: the owner repository and the tip. Output: the commit.
fn leave_out_teammates(shared: &Repo, tip: &str) -> Result<String, EngineError> {
    let root = shared.capture(&["ls-tree", "-z", tip], LISTING_BYTES_MAX)?;
    let entries: Vec<&[u8]> = root
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    let folder = |entry: &&[u8]| {
        entry
            .iter()
            .position(|byte| *byte == b'\t')
            .is_some_and(|tab| &entry[tab + 1..] == agent_git::TEAMMATES_FOLDER.as_bytes())
    };
    if !entries.iter().any(folder) {
        return Ok(tip.to_owned());
    }
    let mut kept = Vec::new();
    for entry in entries.iter().filter(|entry| !folder(entry)) {
        kept.extend_from_slice(entry);
        kept.push(0);
    }
    let made = shared.exec(&["mktree", "-z"], Some(&kept), &[], 256)?;
    if made.code != 0 || made.overflow {
        return Err(failure("changes_git_failed"));
    }
    let tree = super::git::object_id(&made.stdout)?;
    let dated = shared.capture(&["show", "-s", "--format=%ct", tip], 64)?;
    let seconds = std::str::from_utf8(&dated)
        .ok()
        .map(str::trim)
        .filter(|seconds| !seconds.is_empty() && seconds.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| failure("changes_git_failed"))?;
    let date = format!("@{seconds} +0000");
    let (name, email) = super::flow::ENGINE;
    let env = [
        ("GIT_AUTHOR_NAME", name),
        ("GIT_AUTHOR_EMAIL", email),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", name),
        ("GIT_COMMITTER_EMAIL", email),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let committed = shared.exec(
        &["commit-tree", "--no-gpg-sign", &tree, "-p", tip],
        Some(b"CrazyAgent: leave out the teammates' worktrees\n"),
        &env,
        256,
    )?;
    if committed.code != 0 || committed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let commit = super::git::object_id(&committed.stdout)?;
    assert_ne!(commit, tip);
    Ok(commit)
}

/// Where one teammate worktree inside a harness's clone stands: on a branch of the clone,
/// or at a commit with no branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TeammateHead {
    Branch(String),
    Commit(String),
}

/// One teammate worktree inside a harness's clone, `<clone>/.worktrees/<name>`, as its
/// git record in the clone names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Teammate {
    pub(super) folder: std::path::PathBuf,
    pub(super) head: TeammateHead,
}

/// Lists the teammate worktrees inside a linked lead's worktree (OP-9b), read as data with
/// no git run and no link followed: every entry of `<worktree>/.worktrees` must be a plain
/// folder whose `.git` file names a record in the project repository's `.git/worktrees`,
/// a record that names that folder back and holds a HEAD on a teammate branch of the task
/// branch or at a commit, so nothing there is left uncounted. Inputs: the owner checkout
/// root, the lead's worktree and its task branch. Output: the teammates, at most
/// `TEAMMATES_MAX`, else `changes_workspace_invalid` for anything else there, or
/// `changes_too_large`.
pub(super) fn linked_teammates(
    repository: &Path,
    worktree: &Path,
    branch: &str,
) -> Result<Vec<Teammate>, EngineError> {
    use rustix::fs::{CWD, Dir, Mode, OFlags, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let folders = match openat(&root, agent_git::TEAMMATES_FOLDER, flags, Mode::empty()) {
        Ok(folders) => folders,
        Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
        Err(_) => return Err(invalid()),
    };
    let records_path = repository.join(".git").join("worktrees");
    let records = openat(CWD, &records_path, flags, Mode::empty()).map_err(|_| invalid())?;
    let place = worktree.join(agent_git::TEAMMATES_FOLDER);
    let mut found = Vec::new();
    for entry in Dir::read_from(&folders).map_err(|_| invalid())? {
        let entry = entry.map_err(|_| invalid())?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| invalid())?
            .to_owned();
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        if found.len() >= TEAMMATES_MAX {
            return Err(failure("changes_too_large"));
        }
        let folder =
            openat(&folders, name.as_str(), flags, Mode::empty()).map_err(|_| invalid())?;
        let link = read_at(&folder, ".git", HEAD_BYTES_MAX)?.ok_or_else(invalid)?;
        let admin = std::str::from_utf8(&link)
            .ok()
            .and_then(|link| link.strip_prefix("gitdir: "))
            .and_then(|link| link.strip_suffix('\n'))
            .map(Path::new)
            .filter(|admin| admin.parent() == Some(records_path.as_path()))
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(invalid)?;
        let record = openat(&records, admin, flags, Mode::empty()).map_err(|_| invalid())?;
        let back = read_at(&record, "gitdir", HEAD_BYTES_MAX)?.ok_or_else(invalid)?;
        let own = place.join(&name).join(".git");
        if std::str::from_utf8(&back)
            .ok()
            .and_then(|back| back.strip_suffix('\n'))
            .map(Path::new)
            != Some(own.as_path())
        {
            return Err(invalid());
        }
        let head = read_at(&record, "HEAD", HEAD_BYTES_MAX)?.ok_or_else(invalid)?;
        let head = std::str::from_utf8(&head)
            .ok()
            .and_then(|head| head.strip_suffix('\n'))
            .ok_or_else(invalid)?;
        let head = match head.strip_prefix("ref: refs/heads/") {
            Some(name) if agent_git::is_teammate_branch(branch, name) => {
                TeammateHead::Branch(name.to_owned())
            }
            None if super::git::is_object_id(head) => TeammateHead::Commit(head.to_owned()),
            _ => return Err(invalid()),
        };
        found.push(Teammate {
            folder: place.join(&name),
            head,
        });
    }
    Ok(found)
}

/// Lists the teammate worktrees of a harness's clone, read as data from the clone's git
/// folder and its teammates' folder with no git run and no link followed: each record in
/// `.git/worktrees` must name a plain folder directly inside `<clone>/.worktrees` and hold
/// a HEAD on a teammate branch of the task branch or at a commit, and every entry of that
/// folder must be such a recorded worktree, so nothing there is left uncounted. Inputs:
/// the worktree and its task branch. Output: the teammates, at most `TEAMMATES_MAX`, else
/// `changes_workspace_invalid` for anything else there, or `changes_too_large`.
pub(super) fn teammates(worktree: &Path, branch: &str) -> Result<Vec<Teammate>, EngineError> {
    use rustix::fs::{CWD, Dir, Mode, OFlags, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let names = |folder: &rustix::fd::OwnedFd| -> Result<Vec<String>, EngineError> {
        let mut found = Vec::new();
        for entry in Dir::read_from(folder).map_err(|_| invalid())? {
            let entry = entry.map_err(|_| invalid())?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| invalid())?
                .to_owned();
            if matches!(name.as_str(), "." | "..") {
                continue;
            }
            if found.len() >= TEAMMATES_MAX {
                return Err(failure("changes_too_large"));
            }
            found.push(name);
        }
        Ok(found)
    };
    let root = openat(CWD, worktree, flags, Mode::empty()).map_err(|_| invalid())?;
    let git = openat(&root, ".git", flags, Mode::empty()).map_err(|_| invalid())?;
    let records = match openat(&git, "worktrees", flags, Mode::empty()) {
        Ok(records) => Some(records),
        Err(rustix::io::Errno::NOENT) => None,
        Err(_) => return Err(invalid()),
    };
    let folders = match openat(&root, agent_git::TEAMMATES_FOLDER, flags, Mode::empty()) {
        Ok(folders) => {
            let listed = names(&folders)?;
            for name in &listed {
                openat(&folders, name.as_str(), flags, Mode::empty()).map_err(|_| invalid())?;
            }
            listed
        }
        Err(rustix::io::Errno::NOENT) => Vec::new(),
        Err(_) => return Err(invalid()),
    };
    let place = worktree.join(agent_git::TEAMMATES_FOLDER);
    let mut found = Vec::new();
    if let Some(records) = &records {
        for name in names(records)? {
            let record =
                openat(records, name.as_str(), flags, Mode::empty()).map_err(|_| invalid())?;
            let link = read_at(&record, "gitdir", HEAD_BYTES_MAX)?.ok_or_else(invalid)?;
            let link = std::str::from_utf8(&link).map_err(|_| invalid())?;
            let folder = link
                .strip_suffix('\n')
                .map(Path::new)
                .filter(|git_file| git_file.file_name() == Some(std::ffi::OsStr::new(".git")))
                .and_then(Path::parent)
                .filter(|folder| folder.parent() == Some(place.as_path()))
                .ok_or_else(invalid)?;
            let head = read_at(&record, "HEAD", HEAD_BYTES_MAX)?.ok_or_else(invalid)?;
            let head = std::str::from_utf8(&head)
                .ok()
                .and_then(|head| head.strip_suffix('\n'))
                .ok_or_else(invalid)?;
            let head = match head.strip_prefix("ref: refs/heads/") {
                Some(name) if agent_git::is_teammate_branch(branch, name) => {
                    TeammateHead::Branch(name.to_owned())
                }
                None if super::git::is_object_id(head) => TeammateHead::Commit(head.to_owned()),
                _ => return Err(invalid()),
            };
            found.push(Teammate {
                folder: folder.to_path_buf(),
                head,
            });
        }
    }
    let recorded = |name: &String| {
        found
            .iter()
            .any(|teammate| teammate.folder == place.join(name))
    };
    if !folders.iter().all(recorded) {
        return Err(invalid());
    }
    Ok(found)
}
