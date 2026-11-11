use super::{GitOutcome, GitPlace, HARDENING, ran, ran_stopped};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The first argument of the one `ide.git` call that is no network command: the engine's
/// shim sends it to carry the staged changes of one git command into the project's index.
pub(crate) const VERB: &str = crate::ide::bridge::INDEX_VERB;

const SLOT_DIGITS_MAX: usize = 10;
const INDEX_BYTES_MAX: u64 = 256 * 1024 * 1024;
const LISTING_BYTES_MAX: usize = 256 * 1024 * 1024;
const REFUSALS_BYTES_MAX: usize = 1024 * 1024;
const REFRESH_OUTPUT_BYTES_MAX: usize = 4096;
const LIST_TIME: Duration = Duration::from_secs(60);
const APPLY_TIME: Duration = Duration::from_secs(60);
const LOCK_RETRY: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(100);
const REFRESH_TIME: Duration = Duration::from_secs(10);
const TERM_GRACE: Duration = Duration::from_secs(2);
const REFUSAL_PREFIX: &[u8] = b"Ignoring path ";
const INDEX_SETTINGS: [&str; 6] = [
    "-c",
    "core.trustctime=true",
    "-c",
    "core.splitIndex=false",
    "-c",
    "core.untrackedCache=false",
];
const LIST_PROFILE: &str = concat!(
    "(version 1)\n(allow default)\n(deny network*)\n",
    "(deny process-exec* (require-not (require-any (literal (param \"GIT\")) (subpath (param \"HELPERS\")))))\n",
    "(deny file-read-data (require-not (require-any (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/share\") (subpath \"/dev\") (subpath (param \"TOOLS\")) (subpath (param \"GITDIR\")) (literal (param \"COPY\")))))\n",
    "(deny file-write* (require-not (literal \"/dev/null\")))\n",
);
const WRITE_PROFILE: &str = concat!(
    "(version 1)\n(allow default)\n(deny network*)\n",
    "(deny process-exec* (require-not (require-any (literal (param \"GIT\")) (subpath (param \"HELPERS\")))))\n",
    "(deny file-read-data (require-not (require-any (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/share\") (subpath \"/dev\") (subpath (param \"TOOLS\")) (subpath (param \"ROOT\")))))\n",
    "(deny file-write* (require-not (require-any (literal \"/dev/null\") (literal (param \"INDEX\")) (literal (param \"LOCK\")))))\n",
);

/// The per-project gates of the running index calls: one apply and its refresh hold a
/// project's gate, so the next apply waits instead of losing its lock retry.
pub(crate) type Gates = Mutex<Vec<(ProjectId, Arc<Mutex<()>>)>>;

/// One project's gate, shared by every running index call of the project; gates no call
/// holds are dropped, so the list stays within the calls that hold a place. Inputs: the
/// gates and the project. Output: the gate, or `ide_unavailable` for a poisoned list.
pub(crate) fn gate(gates: &Gates, project: &ProjectId) -> Result<Arc<Mutex<()>>, EngineError> {
    let mut held = gates.lock().map_err(|_| failure("ide_unavailable"))?;
    held.retain(|(_, gate)| Arc::strong_count(gate) > 1);
    if let Some((_, gate)) = held.iter().find(|(known, _)| known == project) {
        return Ok(Arc::clone(gate));
    }
    let gate = Arc::new(Mutex::new(()));
    held.push((project.clone(), Arc::clone(&gate)));
    assert!(held.len() <= super::GIT_CALLS_MAX);
    Ok(gate)
}

/// Answers one index call: checks the folder as a network call's and carries the slot's
/// changes ([`carry`]) on a blocking thread that holds the call's place. Inputs: the engine
/// root, the project, git's arguments, the folder the shim ran in, the place and the
/// project's gate. Output: the [`GitOutcome`] as JSON, or a plain refusal.
pub(crate) async fn answer(
    root: PathBuf,
    project: ProjectId,
    args: Vec<String>,
    folder: String,
    place: GitPlace,
    gate: Arc<Mutex<()>>,
) -> Result<serde_json::Value, EngineError> {
    tokio::task::spawn_blocking(move || {
        let _held = place;
        let [_, slot] = args.as_slice() else {
            return Err(failure("ide_git_refused"));
        };
        let project_folder = crate::ide::workspace::folder(&root, &project)?;
        let folder = super::checked_folder(Path::new(&folder), &project_folder)?;
        let engine =
            crate::sandbox::canonical(&root.join(crate::ide::FOLDER).join(project.as_str()))
                .map_err(|_| failure("ide_folder_unavailable"))?;
        let private = engine
            .join(crate::ide::server::SERVER_DATA)
            .join(crate::ide::git::PRIVATE_INDEX);
        let outcome = carry(&gate, &private, &engine, &folder, slot)?;
        serde_json::to_value(outcome).map_err(|_| failure("ide_git_outcome_unknown"))
    })
    .await
    .map_err(|_| failure("ide_git_outcome_unknown"))?
}

/// Whether a slot is what the shim sends, its own pid: 1 to [`SLOT_DIGITS_MAX`] ASCII
/// digits, so it can name nothing but that command's two files. Input: the text. Output:
/// the answer.
fn slot_shaped(slot: &str) -> bool {
    (1..=SLOT_DIGITS_MAX).contains(&slot.len()) && slot.bytes().all(|byte| byte.is_ascii_digit())
}

/// Carries one git command's staged changes from the IDE server's private index into the
/// project's real index, through git's own path check (SECAUDIT-IDE-SERVER-INDEX). Holds the
/// project's gate for the whole step. Inputs: the gate, the private folder, the engine's
/// folder for the copies, the canonical project folder and the slot. Output: the outcome,
/// code 1 with one plain line when it failed or git refused entries; `ide_git_refused` for a
/// slot of any other shape.
pub(crate) fn carry(
    gate: &Mutex<()>,
    private: &Path,
    engine: &Path,
    project: &Path,
    slot: &str,
) -> Result<GitOutcome, EngineError> {
    if !slot_shaped(slot) {
        return Err(failure("ide_git_refused"));
    }
    assert!(private.is_absolute() && engine.is_absolute() && project.is_absolute());
    let _gated = gate.lock().map_err(|_| failure("ide_unavailable"))?;
    let (code, stderr) = match recorded(private, engine, project, slot) {
        Ok(Refused { count: 0, .. }) => (0, String::new()),
        Ok(Refused { count, more }) => (
            1,
            format!(
                "CrazyAgent refused {}{count} index entries git itself calls invalid.\n",
                if more { "at least " } else { "" }
            ),
        ),
        Err(code) => (
            1,
            format!(
                "CrazyAgent could not record this in the project's git index ({code}). The staged list may be out of date; git status in your own terminal shows it, and git reset rebuilds it from the current commit without touching your files.\n"
            ),
        ),
    };
    assert!(code == 0 || stderr.ends_with(".\n"));
    Ok(GitOutcome {
        code,
        stdout: String::new(),
        stderr,
    })
}

/// How many distinct paths git refused in one apply and whether its output was cut, so
/// more were refused than counted.
struct Refused {
    count: usize,
    more: bool,
}

/// The engine's four private files for one call, removed when the call ends.
struct Copies {
    base: PathBuf,
    work: PathBuf,
    objects: PathBuf,
    delta: PathBuf,
}

impl Copies {
    /// Names the files of one slot in the engine's folder, which the server cannot write.
    /// Inputs: the folder and the slot. Output: the names.
    fn new(engine: &Path, slot: &str) -> Self {
        Self {
            base: engine.join(format!("carry-{slot}.base")),
            work: engine.join(format!("carry-{slot}.work")),
            objects: engine.join(format!("carry-{slot}.objects")),
            delta: engine.join(format!("carry-{slot}.delta")),
        }
    }
}

impl Drop for Copies {
    fn drop(&mut self) {
        for file in [&self.base, &self.work, &self.objects, &self.delta] {
            let _ = fs::remove_file(file);
        }
    }
}

/// The step under the gate: copies the slot's two files, lists both under Seatbelt, checks
/// that every object the changed entries name is stored ([`stored`]), applies what differs
/// and refreshes the index's stat data, best effort, after any change. An absent base is an
/// empty index; an absent work file with a base present is refused, so a lost file never
/// reads as "everything was unstaged". Inputs: as [`carry`]. Output: what git refused, or a
/// plain failure code.
fn recorded(
    private: &Path,
    engine: &Path,
    project: &Path,
    slot: &str,
) -> Result<Refused, &'static str> {
    let nothing = Refused {
        count: 0,
        more: false,
    };
    if crate::research::runner::sandboxed_git() != Path::new(crate::research::runner::TOOLS_GIT) {
        return Err("index_git_unavailable");
    }
    let git_folder = project.join(".git");
    let copies = Copies::new(engine, slot);
    let had_base = copied(&private.join(format!("{slot}.base")), &copies.base)?;
    let had_work = copied(&private.join(format!("{slot}.work")), &copies.work)?;
    if !had_work {
        return if had_base {
            Err("index_work_missing")
        } else {
            Ok(nothing)
        };
    }
    let base = if had_base {
        listing(&git_folder, &copies.base)?
    } else {
        Vec::new()
    };
    let work = listing(&git_folder, &copies.work)?;
    let (base, work) = (entries(&base)?, entries(&work)?);
    let delta = delta(&base, &work);
    if delta.is_empty() {
        return Ok(nothing);
    }
    stored(&git_folder, &copies.objects, &named(&base, &work))?;
    private_write(&copies.delta, &delta)?;
    let refused = applied(&git_folder, &copies.delta)?;
    refresh(project);
    Ok(refused)
}

/// Copies one file of the private folder into the engine's folder: never through a link at
/// any part of the path and never waiting on a FIFO, a regular file only, at most
/// [`INDEX_BYTES_MAX`] bytes. Inputs: the source and the target. Output: whether the source
/// existed, or a plain failure code.
fn copied(source: &Path, target: &Path) -> Result<bool, &'static str> {
    let below = source.strip_prefix("/").map_err(|_| "index_unreadable")?;
    let input = match crate::platform::open_plain_beneath(Path::new("/"), below) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("index_unreadable"),
    };
    let metadata = input.metadata().map_err(|_| "index_unreadable")?;
    if !metadata.is_file() {
        return Err("index_unreadable");
    }
    if metadata.len() > INDEX_BYTES_MAX {
        return Err("index_too_large");
    }
    let mut output = private_file(target)?;
    let written = std::io::copy(&mut input.take(INDEX_BYTES_MAX + 1), &mut output)
        .map_err(|_| "index_unreadable")?;
    if written > INDEX_BYTES_MAX {
        return Err("index_too_large");
    }
    Ok(true)
}

/// Opens a file of the engine's folder for writing, 0600 and never through a link. Input:
/// the path. Output: the file, or a plain failure code.
fn private_file(path: &Path) -> Result<fs::File, &'static str> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "index_copy_failed")
}

/// Writes the bytes to a file of the engine's folder. Inputs: the path and the bytes.
/// Output: Ok, or a plain failure code.
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    use std::io::Write;
    private_file(path)?
        .write_all(bytes)
        .map_err(|_| "index_copy_failed")
}

/// Lists one copied index as data with `git ls-files -z -s`, under a profile that reads
/// only system files, git and the project's git folder and writes nothing, since this is
/// C parsing bytes the server wrote. Inputs: the git folder and the copy. Output: the
/// listing, or a plain failure code.
fn listing(git_folder: &Path, copy: &Path) -> Result<Vec<u8>, &'static str> {
    let command = confined(
        LIST_PROFILE,
        &[("GITDIR", git_folder), ("COPY", copy)],
        git_folder,
        git_folder,
        Some(("GIT_INDEX_FILE", copy)),
        &["ls-files", "-z", "-s"],
    );
    let done = ran(command, LISTING_BYTES_MAX, LIST_TIME).map_err(|_| "index_git_unavailable")?;
    match done.status {
        Some(0) if done.cut => Err("index_too_large"),
        Some(0) => Ok(done.stdout),
        None => Err("index_timed_out"),
        Some(_) => Err("index_unreadable"),
    }
}

/// One index listing keyed by path and stage, each with its mode and object name.
type Entries<'a> = BTreeMap<(&'a [u8], u8), (&'a [u8], &'a [u8])>;

/// Reads `ls-files -z -s` output strictly: every record `<mode> <oid> <stage>\t<path>`, a
/// known mode, a 40 or 64 digit lowercase object name, a stage from 0 to 3, a non-empty
/// path and no repeated key. Input: the listing. Output: the entries, or
/// `index_malformed`.
fn entries(listing: &[u8]) -> Result<Entries<'_>, &'static str> {
    const MALFORMED: &str = "index_malformed";
    let mut found = Entries::new();
    if listing.is_empty() {
        return Ok(found);
    }
    let body = listing.strip_suffix(b"\0").ok_or(MALFORMED)?;
    for record in body.split(|byte| *byte == 0) {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(MALFORMED)?;
        let (meta, path) = (&record[..tab], &record[tab + 1..]);
        let fields: Vec<&[u8]> = meta.split(|byte| *byte == b' ').collect();
        let [mode, oid, stage] = fields.as_slice() else {
            return Err(MALFORMED);
        };
        let known = matches!(*mode, b"100644" | b"100755" | b"120000" | b"160000");
        let named = matches!(oid.len(), 40 | 64)
            && oid
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        let [digit @ b'0'..=b'3'] = **stage else {
            return Err(MALFORMED);
        };
        if !known || !named || path.is_empty() {
            return Err(MALFORMED);
        }
        if found.insert((path, digit - b'0'), (*mode, *oid)).is_some() {
            return Err(MALFORMED);
        }
    }
    Ok(found)
}

/// The paths with an entry in `from` that `to` lacks or holds differently. Inputs: the two
/// listings. Output: the paths.
fn differing<'a>(from: &Entries<'a>, to: &Entries<'a>) -> BTreeSet<&'a [u8]> {
    from.iter()
        .filter(|(key, value)| to.get(key) != Some(value))
        .map(|((path, _), _)| *path)
        .collect()
}

/// The paths with any difference between the two listings. Inputs: the two listings.
/// Output: the paths.
fn changed<'a>(base: &Entries<'a>, work: &Entries<'a>) -> BTreeSet<&'a [u8]> {
    let mut paths = differing(base, work);
    paths.extend(differing(work, base));
    paths
}

/// The objects the lines of [`delta`] write: every stage `work` has for a changed path,
/// except submodule entries, which name a commit in the submodule's own store. Inputs: the
/// two listings. Output: the object names.
fn named<'a>(base: &Entries<'a>, work: &Entries<'a>) -> BTreeSet<&'a [u8]> {
    changed(base, work)
        .into_iter()
        .flat_map(|path| work.range((path, 0u8)..=(path, u8::MAX)))
        .filter(|(_, (mode, _))| *mode != b"160000")
        .map(|(_, (_, oid))| *oid)
        .collect()
}

/// Checks that every named object is in the project's object store as a blob
/// (IDE-INDEX-MISSING-OBJECT): `--index-info` writes any object name, and an entry naming a
/// missing object or a tree breaks the owner's status and commit until a reset. Runs
/// `git cat-file --batch-check` under the listing profile, which reads only the git folder
/// and the list and writes nothing, with no lazy fetch. Inputs: the git folder, the engine's
/// file for the list and the names. Output: Ok, or a plain failure code.
fn stored(git_folder: &Path, list: &Path, names: &BTreeSet<&[u8]>) -> Result<(), &'static str> {
    if names.is_empty() {
        return Ok(());
    }
    let (mut lines, mut expected) = (Vec::new(), Vec::new());
    for name in names {
        lines.extend_from_slice(name);
        lines.push(b'\n');
        expected.extend_from_slice(name);
        expected.extend_from_slice(b" blob\n");
    }
    private_write(list, &lines)?;
    let input = fs::File::open(list).map_err(|_| "index_copy_failed")?;
    let mut command = confined(
        LIST_PROFILE,
        &[("GITDIR", git_folder), ("COPY", list)],
        git_folder,
        git_folder,
        Some(("GIT_NO_LAZY_FETCH", Path::new("1"))),
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
    );
    command.stdin(Stdio::from(input));
    let done = ran(command, LISTING_BYTES_MAX, LIST_TIME).map_err(|_| "index_git_unavailable")?;
    match done.status {
        Some(0) if done.cut => Err("index_too_large"),
        Some(0) if done.stdout == expected => Ok(()),
        Some(0) => Err("index_object_missing"),
        None => Err("index_timed_out"),
        Some(_) => Err("index_unreadable"),
    }
}

/// The `update-index -z --index-info` input that turns `base` into `work`: for each path
/// with any difference, one line removing it and one line per stage it has in `work`.
/// Measuring against the command's own base keeps whatever the owner staged meanwhile.
/// Inputs: the two listings. Output: the lines, empty when nothing differs.
fn delta(base: &Entries<'_>, work: &Entries<'_>) -> Vec<u8> {
    let mut lines = Vec::new();
    for path in changed(base, work) {
        let range = (path, 0u8)..=(path, u8::MAX);
        let length = work
            .range(range.clone())
            .chain(base.range(range.clone()))
            .next()
            .map_or(40, |(_, (_, oid))| oid.len());
        lines.extend_from_slice(b"0 ");
        lines.extend(std::iter::repeat_n(b'0', length));
        lines.push(b'\t');
        lines.extend_from_slice(path);
        lines.push(0);
        for ((_, stage), (mode, oid)) in work.range(range) {
            lines.extend_from_slice(mode);
            lines.push(b' ');
            lines.extend_from_slice(oid);
            lines.push(b' ');
            lines.push(b'0' + stage);
            lines.push(b'\t');
            lines.extend_from_slice(path);
            lines.push(0);
        }
    }
    assert!(lines.is_empty() || lines.ends_with(&[0]));
    lines
}

/// Applies the lines to the real index under a profile that writes only the index and its
/// lock. A held lock is retried for [`LOCK_RETRY`]. Git drops a bad path with `Ignoring
/// path`, exits 0 and applies the rest, so its refusals are counted here. Inputs: the git
/// folder and the file of lines. Output: what git refused, or a plain failure code.
fn applied(git_folder: &Path, lines: &Path) -> Result<Refused, &'static str> {
    let started = Instant::now();
    let index = git_folder.join("index");
    let lock = git_folder.join("index.lock");
    let stamp = fs::symlink_metadata(&index)
        .and_then(|found| found.modified())
        .ok();
    loop {
        let input = fs::File::open(lines).map_err(|_| "index_copy_failed")?;
        let mut command = confined(
            WRITE_PROFILE,
            &[("ROOT", git_folder), ("INDEX", &index), ("LOCK", &lock)],
            git_folder,
            git_folder,
            None,
            &["update-index", "-z", "--index-info"],
        );
        command.stdin(Stdio::from(input));
        let done = ran_stopped(command, REFUSALS_BYTES_MAX, APPLY_TIME, TERM_GRACE)
            .map_err(|_| "index_git_unavailable")?;
        let locked = done
            .stderr
            .windows(b"index.lock".len())
            .any(|part| part == b"index.lock");
        match done.status {
            Some(0) => {
                keep_racy(&index, stamp);
                return Ok(refusals(&done.stderr, done.cut));
            }
            None => return Err("index_timed_out"),
            Some(_) if !locked => return Err("index_refused"),
            Some(_) if started.elapsed() >= LOCK_RETRY => return Err("index_locked"),
            Some(_) => std::thread::sleep(LOCK_POLL),
        }
    }
}

/// Puts the index's modification time back after an apply. The apply runs in the git
/// folder, where git cannot compare a racily clean entry with its file, and a new time would
/// end that entry's racy status and hide a same-size change from the owner. A failure logs
/// one line and leaves the stage, which has landed. Inputs: the index and its time before
/// the apply, none for an index that did not exist. Output: none.
fn keep_racy(index: &Path, stamp: Option<SystemTime>) {
    let Some(stamp) = stamp else { return };
    let kept = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(index)
        .and_then(|file| file.set_modified(stamp));
    if kept.is_err() {
        eprintln!("ide_git_index_stamp_failed");
    }
}

/// Counts the distinct paths git's `Ignoring path` lines name; a path it refuses prints
/// one line for its removal and one for its entry. The hostile text is only counted, never
/// shown. Inputs: git's standard error and whether it was cut. Output: the count.
fn refusals(stderr: &[u8], cut: bool) -> Refused {
    let paths: BTreeSet<&[u8]> = stderr
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(REFUSAL_PREFIX))
        .collect();
    Refused {
        count: paths.len(),
        more: cut,
    }
}

/// Saves the stat data `--index-info` leaves out, so status does not re-hash staged files:
/// `update-index -q --refresh` under a profile that reads the project and writes only the
/// index and its lock, runs nothing but git and its helpers and has no network. Best
/// effort: a failure or the [`REFRESH_TIME`] limit logs one line and leaves the stage, which
/// has landed. TERM comes first so git removes its own lock, KILL after [`TERM_GRACE`].
/// Input: the canonical project folder. Output: none.
fn refresh(project: &Path) {
    let git_folder = project.join(".git");
    let index = git_folder.join("index");
    let lock = git_folder.join("index.lock");
    let command = confined(
        WRITE_PROFILE,
        &[("ROOT", project), ("INDEX", &index), ("LOCK", &lock)],
        &git_folder,
        project,
        Some(("GIT_WORK_TREE", project)),
        &["update-index", "-q", "--ignore-submodules", "--refresh"],
    );
    match ran_stopped(command, REFRESH_OUTPUT_BYTES_MAX, REFRESH_TIME, TERM_GRACE) {
        Ok(done) if done.status == Some(0) => {}
        Ok(done) => eprintln!("ide_git_index_refresh_failed status={:?}", done.status),
        Err(_) => eprintln!("ide_git_index_refresh_failed status=unavailable"),
    }
}

/// One git process of this step: the Command Line Tools git under a Seatbelt profile with
/// its paths passed as parameters, [`HARDENING`] and the index settings before the
/// arguments, global and system config off, the git folder named so no search or
/// `commondir` file decides it, and its own process group. Inputs: the profile, its
/// parameters, the git folder, the working folder, one more variable if any and the
/// arguments. Output: the unspawned command.
fn confined(
    profile: &str,
    params: &[(&str, &Path)],
    git_folder: &Path,
    cwd: &Path,
    extra: Option<(&str, &Path)>,
    args: &[&str],
) -> Command {
    let git = Path::new(crate::research::runner::TOOLS_GIT);
    let tools = Path::new(crate::research::runner::TOOLS_ROOT);
    let helpers = tools.join("usr/libexec/git-core");
    let mut command = Command::new(crate::sandbox::SANDBOX_EXEC);
    for (name, value) in [
        ("GIT", git),
        ("HELPERS", helpers.as_path()),
        ("TOOLS", tools),
    ]
    .iter()
    .chain(params)
    {
        let mut pair = OsString::from(name);
        pair.push("=");
        pair.push(value);
        command.arg("-D").arg(pair);
    }
    command
        .args(["-p", profile])
        .arg(git)
        .args(HARDENING)
        .args(INDEX_SETTINGS)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_DIR", git_folder)
        .env("GIT_COMMON_DIR", git_folder)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some((name, value)) = extra {
        command.env(name, value);
    }
    command
}

#[cfg(all(test, target_os = "macos"))]
mod tests;

#[cfg(all(test, unix, not(target_os = "macos")))]
mod unavailable_tests {
    /// Off macOS no private index is carried, since the step runs the Command Line Tools
    /// git under Seatbelt: it is refused with `index_git_unavailable` before any copy is
    /// made, so nothing is written beside the private folder.
    #[test]
    fn a_private_index_off_macos_is_refused_before_any_copy_is_made() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let refused = super::recorded(
            &root.join("private"),
            &root.join("engine"),
            &root.join("project"),
            "77",
        );
        assert_eq!(refused.err(), Some("index_git_unavailable"));
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    }
}
