use super::flow::{check_places, clone_branch, open_agent};
use super::git::{OUTPUT_BYTES_MAX, Repo, failure, is_object_id};
use super::store::Record;
use super::{AgentKind, Workspace, clone, store, subagent};
use crate::agent_git::Layout;
use crate::engine::EngineError;
use crate::worktrees::TREE_BOUNDS;
use rusqlite::Connection;
use std::collections::HashMap;

/// How much of a file git reads when it decides the file is binary.
const BINARY_PROBE_BYTES: usize = 8000;
/// The most merge commits one change set's own commits may hold before the engine stops
/// reading them and refuses the set as too large, well past the 130 merges one lead made
/// in the DEC-107 evidence.
const MERGES_MAX: usize = 1024;
/// The most parents one listed merge may have.
const PARENTS_MAX: usize = 16;
/// The most bytes one object id with its separator takes, for a SHA-256 repository.
const ID_FIELD_BYTES_MAX: usize = 65;
const LISTING_BYTES_MAX: usize = (MERGES_MAX + 1) * (PARENTS_MAX + 1) * ID_FIELD_BYTES_MAX;
const UNCOVERED_BYTES_MAX: usize = ID_FIELD_BYTES_MAX;
/// The most commits of its own a master set may hold and still land by itself: one fix per
/// conflicted set, well past the two sets of the HackerBlog case.
const OWN_COMMITS_MAX: usize = 32;

/// Refuses a change set whose own commits merge in work from another branch (DEC-107
/// rule 2), which the agent git wrapper alone cannot stop, since an agent can run git by
/// its full path. A merge passes when every parent after its first is already in the
/// owner's current branch, as the engine's own update merges are, or in the tip of the
/// project's Land on branch, the landed main the agent brings in with `git merge
/// crazyagent/main` (OS-8), read from the owner repository and left out when it cannot be
/// read, or in the commit the agent's worktree starts from, or in the head of the agent's open change set, which the
/// engine recorded at an earlier turn end or made itself as the update merge it hands the
/// agent, or, for a lead, in the tip of one of its own workers, or, for the master at Full
/// access, in the head of any open change set of the project (DEC-117 Q2). "Catch up with
/// main" Off is a rule of the git the app writes for an agent and is not read here: the
/// owner's branch must stay known for the update merges, so landed work merged any other
/// way passes. The caller
/// holds the repository hold, so nothing here takes it. Inputs: the store, the workspace, the owner
/// repository and the agent's head. Output: Ok, else `changes_outside_merge`,
/// `changes_too_large` or a plain git or store code.
pub(super) fn refuse_outside_merges(
    db: &Connection,
    workspace: &Workspace,
    shared: &Repo,
    head: &str,
) -> Result<(), EngineError> {
    assert!(is_object_id(head));
    let Some(owner) = shared.commit_of("HEAD")? else {
        return Ok(());
    };
    let mut known = vec![owner];
    if let Ok(Some((_, landed))) = super::owner::land_on(db, shared, &workspace.project) {
        known.push(landed);
    }
    let worktree = subagent::path_text(&workspace.worktree)?;
    if let Some(start) = store::base_of(db, worktree)?
        && shared.commit_of(&start)?.as_deref() == Some(start.as_str())
    {
        known.push(start);
    }
    if let Some(open) = store::open_for(db, worktree)?
        && shared.commit_of(&open.head)?.as_deref() == Some(open.head.as_str())
    {
        known.push(open.head);
    }
    let parents = merged_parents(shared, head, &known)?;
    if parents.is_empty() || covered(shared, &parents, &known)? {
        return Ok(());
    }
    known.extend(worker_tips(db, workspace, shared)?);
    if workspace.agent.kind == AgentKind::Master
        && crate::sandbox::policy::load(db, workspace.project.as_str())
            .map_err(|_| failure("changes_unavailable"))?
            .access
            == crate::sandbox::policy::AgentAccess::Full
    {
        for head in store::open_heads(db, &workspace.project)? {
            if shared.commit_of(&head)?.as_deref() == Some(head.as_str()) {
                known.push(head);
            }
        }
    }
    if covered(shared, &parents, &known)? {
        return Ok(());
    }
    Err(failure("changes_outside_merge"))
}

/// Tells whether a master's set holds nothing but fixes of real conflicts with the other
/// open sets its head holds, so it may land with no Accept (MASTER-LAND-SPLIT). Every own
/// commit must be such a fix, and every one of those sets must come in through one. Any
/// doubt is false and the set waits for the owner. The caller holds the repository hold
/// and has verified the objects. Inputs: the owner repository, the master's set, the sets
/// its head holds, the Land on tip and the commit its squash measures from. Output: the
/// answer.
pub(super) fn only_conflict_fixes(
    owner: &Repo,
    master: &Record,
    covered: &[Record],
    tip: &str,
    from: &str,
) -> Result<bool, EngineError> {
    assert!(!covered.is_empty());
    assert!(is_object_id(&master.head) && is_object_id(from) && is_object_id(tip));
    let Some(merges) = own_merges(owner, master, covered, tip, from)? else {
        return Ok(false);
    };
    for [commit, ours, theirs] in &merges {
        if !fixes_conflict(owner, commit, ours, theirs)? {
            return Ok(false);
        }
    }
    for set in covered {
        if !brought_in(owner, &set.head, &merges)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Lists every commit the master's squash carries, leaving out where it measures from, the
/// Land on tip and the sets its head holds, as commit, first parent and second parent.
/// Inputs: the owner repository, the master's set, the sets, the tip and the commit the
/// squash measures from. Output: the merges, or None when there are no own commits, too
/// many, or one is not a merge of exactly two parents.
fn own_merges(
    owner: &Repo,
    master: &Record,
    covered: &[Record],
    tip: &str,
    from: &str,
) -> Result<Option<Vec<[String; 3]>>, EngineError> {
    let count = format!("--max-count={}", OWN_COMMITS_MAX + 1);
    let mut arguments = vec![
        "rev-list",
        "--parents",
        &count,
        &master.head,
        "--not",
        from,
        tip,
    ];
    arguments.extend(covered.iter().map(|set| set.head.as_str()));
    let listed = owner.exec(&arguments, None, &[], LISTING_BYTES_MAX)?;
    if listed.code != 0 || listed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let text = std::str::from_utf8(&listed.stdout).map_err(|_| failure("changes_git_failed"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || lines.len() > OWN_COMMITS_MAX {
        return Ok(None);
    }
    let mut merges = Vec::with_capacity(lines.len());
    for line in lines {
        let ids: Vec<&str> = line.split(' ').collect();
        if !ids.iter().all(|id| is_object_id(id)) {
            return Err(failure("changes_git_failed"));
        }
        let [commit, ours, theirs] = ids[..] else {
            return Ok(None);
        };
        merges.push([commit.to_owned(), ours.to_owned(), theirs.to_owned()]);
    }
    assert!(!merges.is_empty() && merges.len() <= OWN_COMMITS_MAX);
    Ok(Some(merges))
}

/// Tells whether a merge commit is only the fix of a real conflict: git's own merge of its
/// parents really conflicts, its tree keeps every clean change of that merge, and no
/// conflicted file still holds a conflict marker or has a marker size of its own. Inputs:
/// the owner repository, the commit and its two parents. Output: the answer.
fn fixes_conflict(
    owner: &Repo,
    commit: &str,
    ours: &str,
    theirs: &str,
) -> Result<bool, EngineError> {
    let Some(conflicts) = kept_clean_changes(owner, commit, ours, theirs)? else {
        return Ok(false);
    };
    if conflicts.is_empty() || sets_marker_size(owner, &conflicts, &[commit, ours, theirs])? {
        return Ok(false);
    }
    for path in &conflicts {
        if holds_conflict_marker(owner, commit, path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Tells whether a landed commit holds a change set's work: merging the set's head into it
/// again would change nothing but conflicted paths, and at each of those the landed tree
/// shows the set's work ([`holds_work_at`]). Doubt stays open: a reworded resolution waits
/// for the owner. Inputs: the owner repository, the commit the land left the branch at and
/// the set's base and head. Output: the answer.
pub(super) fn holds_set_work(
    owner: &Repo,
    landed: &str,
    base: &str,
    head: &str,
) -> Result<bool, EngineError> {
    let Some(conflicts) = kept_clean_changes(owner, landed, landed, head)? else {
        return Ok(false);
    };
    for path in &conflicts {
        if !holds_work_at(owner, [base, head, landed], path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// One side of a path: its object id and bytes.
type Blob = (Vec<u8>, Vec<u8>);

/// Reads whether a landed tree shows what a set did at one path. A set that deleted the file
/// needs it absent; a binary path needs the set's blob; otherwise see [`kept_lines`]. A
/// path that is no file on some side, or too large to read, counts as not shown. Inputs: the
/// owner repository, the base, head and landed commits and the path as git printed it.
/// Output: the answer.
fn holds_work_at(owner: &Repo, commits: [&str; 3], path: &[u8]) -> Result<bool, EngineError> {
    let Some([before, after, now]) = blobs_at(owner, commits, path)? else {
        return Ok(false);
    };
    let Some(after) = after else {
        return Ok(before.is_some() && now.is_none());
    };
    let binary = |blob: &Blob| blob.1[..blob.1.len().min(BINARY_PROBE_BYTES)].contains(&0);
    if binary(&after) || before.as_ref().is_some_and(binary) || now.as_ref().is_some_and(binary) {
        return Ok(now.is_some_and(|now| now.0 == after.0));
    }
    let text = |blob: &Option<Blob>| blob.as_ref().map_or(Vec::new(), |blob| blob.1.clone());
    Ok(kept_lines(&text(&before), &after.1, &text(&now)))
}

/// Reads one path at three commits with no filter. Inputs: the owner repository, the commits
/// and the path as git printed it. Output: each side, None for a path that commit lacks, or
/// None overall for a side that is no file or a reply over the bound; a path with a line
/// break makes the reply too long, which is an error.
fn blobs_at(
    owner: &Repo,
    commits: [&str; 3],
    path: &[u8],
) -> Result<Option<[Option<Blob>; 3]>, EngineError> {
    let mut request = Vec::new();
    for commit in commits {
        request.extend_from_slice(commit.as_bytes());
        request.push(b':');
        request.extend_from_slice(path);
        request.push(b'\n');
    }
    let read = owner.exec(
        &["cat-file", "--batch"],
        Some(&request),
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if read.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if read.overflow {
        return Ok(None);
    }
    let mut rest = read.stdout.as_slice();
    let mut sides: [Option<Blob>; 3] = [None, None, None];
    for side in &mut sides {
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| failure("changes_git_failed"))?;
        let (header, tail) = rest.split_at(end);
        rest = &tail[1..];
        if header.ends_with(b" missing") {
            continue;
        }
        let fields: Vec<&[u8]> = header.split(|byte| *byte == b' ').collect();
        let [id, kind, size] = fields[..] else {
            return Err(failure("changes_git_failed"));
        };
        if kind != b"blob" {
            return Ok(None);
        }
        let size: usize = std::str::from_utf8(size)
            .ok()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| failure("changes_git_failed"))?;
        if rest.len() <= size {
            return Err(failure("changes_git_failed"));
        }
        *side = Some((id.to_vec(), rest[..size].to_vec()));
        rest = &rest[size + 1..];
    }
    if !rest.is_empty() {
        return Err(failure("changes_git_failed"));
    }
    Ok(Some(sides))
}

/// Tells whether a file still shows a set's edit, line by line: every line the set added is
/// there as many times as it was added, or, for a set that only removed lines, those lines
/// are gone. A line main already held counts as there, and a set that changed no line count
/// shows nothing. Inputs: the file at the base, at the head and as landed. Output: the answer.
fn kept_lines(before: &[u8], after: &[u8], now: &[u8]) -> bool {
    let (before, after, now) = (line_counts(before), line_counts(after), line_counts(now));
    let times = |lines: &HashMap<&[u8], usize>, line: &[u8]| lines.get(line).copied().unwrap_or(0);
    let added: Vec<(&[u8], usize)> = after
        .iter()
        .filter(|(line, count)| **count > times(&before, line))
        .map(|(line, count)| (*line, count - times(&before, line)))
        .collect();
    if !added.is_empty() {
        return added
            .iter()
            .all(|(line, count)| times(&now, line) >= *count);
    }
    let removed: Vec<&[u8]> = before
        .keys()
        .filter(|line| times(&after, line) < times(&before, line))
        .copied()
        .collect();
    !removed.is_empty()
        && removed
            .iter()
            .all(|line| times(&now, line) <= times(&after, line))
}

/// Counts each line of a text, the final line break not making a line of its own. Input: the
/// text. Output: each line and how many times it occurs.
fn line_counts(text: &[u8]) -> HashMap<&[u8], usize> {
    let mut counts = HashMap::new();
    if text.is_empty() {
        return counts;
    }
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    for line in body.split(|byte| *byte == b'\n') {
        *counts.entry(line).or_insert(0) += 1;
    }
    counts
}

/// Tells whether any path has a conflict-marker-size attribute in any of the commits, read
/// from each commit and the repository's own info/attributes, so its markers may be a
/// length [`holds_conflict_marker`] does not know. Inputs: the owner repository, the paths
/// as git printed them and the commits. Output: the answer.
fn sets_marker_size(
    owner: &Repo,
    paths: &[Vec<u8>],
    commits: &[&str],
) -> Result<bool, EngineError> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    for commit in commits {
        let output = owner.exec(
            &[
                "check-attr",
                "-z",
                "--stdin",
                "--source",
                commit,
                "conflict-marker-size",
            ],
            Some(&input),
            &[],
            OUTPUT_BYTES_MAX,
        )?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let fields: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
        if fields
            .chunks(3)
            .any(|triple| triple.len() == 3 && triple[2] != b"unspecified")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Tells whether a commit holds a change set's clean changes as [`holds_clean_changes`]
/// does, merging from the set's own base. For a commit with the set's head in its history
/// the merge base is that head and the merge always agrees, so a merge that kept none of
/// the work shows only from the set's base. Inputs: the owner repository, the commit and
/// the set's base and head. A path the merge has and the commit lacks still counts as held
/// when the commit holds that file whole under another name than the head does, so a lead
/// that moved a worker's file holds it; a file moved and changed does not, and that set
/// waits for the owner. Output: the answer.
pub(super) fn holds_changes_since(
    owner: &Repo,
    commit: &str,
    base: &str,
    head: &str,
) -> Result<bool, EngineError> {
    assert!(is_object_id(base));
    assert!(is_object_id(head));
    let Some(Remerged {
        conflicts,
        differences,
    }) = merge_differences(owner, commit, base, commit, head)?
    else {
        return Ok(false);
    };
    let left: Vec<&Difference> = differences
        .iter()
        .filter(|difference| !conflicts.contains(&difference.path))
        .collect();
    if left.is_empty() {
        return Ok(true);
    }
    let moved = moved_whole(owner, head, commit)?;
    Ok(left.iter().all(|difference| {
        difference.gone.as_ref().is_some_and(|blob| {
            moved
                .iter()
                .any(|(path, moved_blob)| *path == difference.path && moved_blob == blob)
        })
    }))
}

/// Lists the files a commit holds whole under another name than a set's head does: git's
/// exact renames from the head to the commit. Past the output bound it lists none, so the
/// set waits. Inputs: the owner repository, the head and the commit. Output: each moved
/// file's path in the head and its blob.
fn moved_whole(
    owner: &Repo,
    head: &str,
    commit: &str,
) -> Result<Vec<(Vec<u8>, String)>, EngineError> {
    assert!(is_object_id(head));
    let listed = owner.exec(
        &[
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-abbrev",
            "--find-renames=100%",
            "--diff-filter=R",
            head,
            commit,
        ],
        None,
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if listed.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if listed.overflow {
        return Ok(Vec::new());
    }
    let mut moved = Vec::new();
    let mut fields = listed.stdout.split(|byte| *byte == 0);
    while let Some(header) = fields.next() {
        if header.is_empty() {
            continue;
        }
        let [_, _, blob, renamed, status] = raw_header(header)?;
        let (Some(from), Some(_)) = (fields.next(), fields.next()) else {
            return Err(failure("changes_git_failed"));
        };
        if !status.starts_with('R') || blob != renamed || from.is_empty() {
            return Err(failure("changes_git_failed"));
        }
        moved.push((from.to_vec(), blob.to_owned()));
    }
    assert!(moved.iter().all(|(_, blob)| is_object_id(blob)));
    Ok(moved)
}

/// Splits one header of git's raw diff output into its two modes, two blobs and status.
/// Inputs: the header as git printed it. Output: the five fields, or `changes_git_failed`.
fn raw_header(header: &[u8]) -> Result<[&str; 5], EngineError> {
    let text = std::str::from_utf8(header).map_err(|_| failure("changes_git_failed"))?;
    let text = text
        .strip_prefix(':')
        .ok_or_else(|| failure("changes_git_failed"))?;
    let Ok(fields) = <[&str; 5]>::try_from(text.split(' ').collect::<Vec<&str>>()) else {
        return Err(failure("changes_git_failed"));
    };
    if !is_object_id(fields[2]) || !is_object_id(fields[3]) {
        return Err(failure("changes_git_failed"));
    }
    Ok(fields)
}

/// Reads whether a file of a commit has a line a conflict marker starts. A path the commit
/// deleted has none; a path git cannot show as one file, or too large to read, counts as
/// having one, so the set waits. Inputs: the owner repository, the commit and the path as
/// git printed it. Output: the answer.
fn holds_conflict_marker(owner: &Repo, commit: &str, path: &[u8]) -> Result<bool, EngineError> {
    if path.contains(&b'\n') {
        return Ok(true);
    }
    let mut request = format!("{commit}:").into_bytes();
    request.extend_from_slice(path);
    request.push(b'\n');
    let read = owner.exec(
        &["cat-file", "--batch"],
        Some(&request),
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if read.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if read.overflow {
        return Ok(true);
    }
    let end = read
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| failure("changes_git_failed"))?;
    let (header, rest) = read.stdout.split_at(end);
    if header.ends_with(b" missing") {
        return Ok(false);
    }
    if header.split(|byte| *byte == b' ').nth(1) != Some(b"blob".as_slice()) {
        return Ok(true);
    }
    Ok(rest[1..].split(|byte| *byte == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> ") || line == b"======="
    }))
}

/// Merges two commits again and tells whether `commit`'s tree differs from the result only
/// at the paths that merge conflicts on, so it kept every clean change of both sides. The
/// sides need one merge base and no configured command. Paths are compared as git printed
/// them. Inputs: the owner repository, the commit and both sides. Output: the conflicted
/// paths, empty for a clean merge, or None when the commit did not keep that merge.
fn kept_clean_changes(
    owner: &Repo,
    commit: &str,
    ours: &str,
    theirs: &str,
) -> Result<Option<Vec<Vec<u8>>>, EngineError> {
    let bases = owner.exec(
        &["merge-base", "--all", ours, theirs],
        None,
        &[],
        ID_FIELD_BYTES_MAX * 3,
    )?;
    if bases.code == 1 || bases.overflow {
        return Ok(None);
    }
    if bases.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    let text = std::str::from_utf8(&bases.stdout).map_err(|_| failure("changes_git_failed"))?;
    let [base] = text.lines().collect::<Vec<&str>>()[..] else {
        return Ok(None);
    };
    if !is_object_id(base) {
        return Err(failure("changes_git_failed"));
    }
    kept_from_base(owner, commit, base, ours, theirs)
}

/// As [`kept_clean_changes`], from a merge base the caller names. Inputs: the owner
/// repository, the commit, the base and both sides. Output: the conflicted paths, empty for
/// a clean merge, or None when the commit did not keep that merge or a configured command
/// would run.
fn kept_from_base(
    owner: &Repo,
    commit: &str,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<Option<Vec<Vec<u8>>>, EngineError> {
    let Some(Remerged {
        conflicts,
        differences,
    }) = merge_differences(owner, commit, base, ours, theirs)?
    else {
        return Ok(None);
    };
    let kept = differences
        .iter()
        .all(|difference| conflicts.contains(&difference.path));
    Ok(kept.then_some(conflicts))
}

/// One path where a commit's tree differs from a merge's result, with the merge's blob
/// there when the commit has nothing at that path.
struct Difference {
    path: Vec<u8>,
    gone: Option<String>,
}

/// A merge made again from a base: the paths it conflicts on, as git printed them, and
/// every path where a commit's tree differs from its result.
struct Remerged {
    conflicts: Vec<Vec<u8>>,
    differences: Vec<Difference>,
}

/// Merges two sides from a base and lists every path where a commit's tree differs from
/// the result. Paths are compared as git printed them. Inputs: the owner repository, the
/// commit, the base and both sides. Output: the conflicted paths and the differences, or
/// None when a configured command would run.
fn merge_differences(
    owner: &Repo,
    commit: &str,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<Option<Remerged>, EngineError> {
    assert!(is_object_id(base));
    let paths = super::owner::merge_paths(owner, base, ours, theirs)?;
    if super::owner::uses_configured_commands(owner, &paths, &[base, ours, theirs])? {
        return Ok(None);
    }
    let (merged, conflicts) = super::owner::merge_raw(owner, base, ours, theirs)?;
    let tree = owner.tree_of(commit)?;
    let changed = owner.exec(
        &[
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            &merged,
            &tree,
        ],
        None,
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if changed.code != 0 || changed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let mut differences = Vec::new();
    let mut fields = changed.stdout.split(|byte| *byte == 0);
    while let Some(header) = fields.next() {
        if header.is_empty() {
            continue;
        }
        let [_, _, blob, _, status] = raw_header(header)?;
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| failure("changes_git_failed"))?;
        differences.push(Difference {
            path: path.to_vec(),
            gone: (status == "D").then(|| blob.to_owned()),
        });
    }
    assert!(
        differences
            .iter()
            .all(|difference| !difference.path.is_empty())
    );
    Ok(Some(Remerged {
        conflicts,
        differences,
    }))
}

/// Tells whether one of the merges brought a set's head in through its second parent, so
/// a set the master fast-forwarded onto does not count. Inputs: the owner repository, the
/// set's head and the merges. Output: the answer.
fn brought_in(owner: &Repo, head: &str, merges: &[[String; 3]]) -> Result<bool, EngineError> {
    for [_, ours, theirs] in merges {
        if owner.test(&["merge-base", "--is-ancestor", head, theirs])?
            && !owner.test(&["merge-base", "--is-ancestor", head, ours])?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Lists the parents after the first of every merge commit the head holds above the known
/// commits, once each. Inputs: the owner repository, the head and the known commits.
/// Output: the parents, or `changes_too_large` past `MERGES_MAX` merges.
fn merged_parents(shared: &Repo, head: &str, known: &[String]) -> Result<Vec<String>, EngineError> {
    assert!(!known.is_empty() && known.iter().all(|id| is_object_id(id)));
    let count = format!("--max-count={}", MERGES_MAX + 1);
    let mut arguments = vec!["rev-list", "--merges", "--parents", &count, head, "--not"];
    arguments.extend(known.iter().map(String::as_str));
    let listed = shared.exec(&arguments, None, &[], LISTING_BYTES_MAX)?;
    if listed.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if listed.overflow {
        return Err(failure("changes_too_large"));
    }
    let text = std::str::from_utf8(&listed.stdout).map_err(|_| failure("changes_git_failed"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > MERGES_MAX {
        return Err(failure("changes_too_large"));
    }
    let mut parents: Vec<String> = Vec::new();
    for line in lines {
        let ids: Vec<&str> = line.split(' ').collect();
        if ids.len() < 3 || ids.len() > PARENTS_MAX + 1 || !ids.iter().all(|id| is_object_id(id)) {
            return Err(failure("changes_git_failed"));
        }
        for parent in &ids[2..] {
            if !parents.iter().any(|seen| seen == parent) {
                parents.push((*parent).to_owned());
            }
        }
    }
    assert!(parents.len() <= MERGES_MAX * PARENTS_MAX);
    Ok(parents)
}

/// Tells whether every commit is an ancestor of, or one of, the known commits, with one
/// git run that reads the commits from its input. Inputs: the owner repository, the
/// commits and the known commits. Output: the answer.
fn covered(shared: &Repo, commits: &[String], known: &[String]) -> Result<bool, EngineError> {
    assert!(!commits.is_empty() && !known.is_empty());
    let mut input = String::new();
    for commit in commits {
        input.push_str(commit);
        input.push('\n');
    }
    for commit in known {
        input.push('^');
        input.push_str(commit);
        input.push('\n');
    }
    let listed = shared.exec(
        &["rev-list", "--max-count=1", "--stdin"],
        Some(input.as_bytes()),
        &[],
        UNCOVERED_BYTES_MAX,
    )?;
    if listed.code != 0 || listed.overflow {
        return Err(failure("changes_git_failed"));
    }
    Ok(listed.stdout.is_empty())
}

/// Reads the tips of a lead's own workers in the owner repository: each sub-agent it
/// started, read as the engine reads any agent's tip, and, for a lead that runs native
/// teammates in its own clone, each teammate branch there. An agent with no workers has
/// none. Inputs: the store, the workspace and the owner repository. Output: the tips.
fn worker_tips(
    db: &Connection,
    workspace: &Workspace,
    shared: &Repo,
) -> Result<Vec<String>, EngineError> {
    if workspace.agent.kind != AgentKind::Sub {
        return Ok(Vec::new());
    }
    let Some(lead) = workspace.agent.id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return Ok(Vec::new());
    };
    let mut tips = Vec::new();
    for (_, sub) in subagent::sub_agents(db, &workspace.project, lead)? {
        if sub.repository != workspace.repository {
            continue;
        }
        tips.push(
            match check_places(&sub.repository, &sub.worktree, &sub.git_dir)? {
                Layout::Linked => open_agent(
                    db,
                    AgentKind::Sub,
                    &sub.repository,
                    &sub.worktree,
                    &sub.git_dir,
                )?
                .head()?,
                Layout::Clone => clone::fetch(
                    shared,
                    &sub.repository,
                    &sub.worktree,
                    &clone_branch(db, &sub.worktree)?,
                    &TREE_BOUNDS,
                )?,
            },
        );
    }
    let native = crate::team::lead_mode(db, &workspace.project, lead)?
        == crate::coord::settings::SubagentMode::Native;
    if native
        && check_places(
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        )? == Layout::Clone
    {
        let teammates = clone::fetch_teammates(
            shared,
            &workspace.repository,
            &workspace.worktree,
            &clone_branch(db, &workspace.worktree)?,
            &TREE_BOUNDS,
        )?;
        tips.extend(teammates.into_iter().map(|(_, tip)| tip));
    }
    assert!(tips.iter().all(|tip| is_object_id(tip)));
    Ok(tips)
}
