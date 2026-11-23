use crate::access::AccessHooks;
use crate::paths::canonical;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The most folders one project may keep its agents out of.
pub const HIDDEN_PATHS_MAX: usize = 8;
/// The most folders one sandbox hides: every hidden folder a project saves
/// ([`HIDDEN_PATHS_MAX`]) plus the two a step adds itself at most, such as the graph step's
/// `.crazyagent/`, or a native lead's part of the relay mailbox and its own temp folder,
/// which its teammates' sandbox hides.
pub const HIDDEN_MAX: usize = HIDDEN_PATHS_MAX + 2;

/// The length of one hidden folder's digest as an agent's launch records it, in hex.
pub const DIGEST_HEX_LEN: usize = 32;

const PATH_TEXT_MAX: usize = 1024;

/// Names one hidden folder as an agent's launch records it: the first 16 bytes of the
/// SHA-256 of its canonical path in lowercase hex, so a pane record stays small.
/// Input: the canonical folder. Output: the digest of `DIGEST_HEX_LEN` characters.
pub fn digest(folder: &Path) -> String {
    let bytes = Sha256::digest(folder.as_os_str().as_encoded_bytes());
    let text: String = bytes[..DIGEST_HEX_LEN / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(text.len(), DIGEST_HEX_LEN);
    text
}

/// Tells whether a recorded digest has the shape [`digest`] writes: `DIGEST_HEX_LEN`
/// lowercase hex characters. Input: the text. Output: the answer.
pub fn digest_plain(text: &str) -> bool {
    text.len() == DIGEST_HEX_LEN
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Tells whether an agent's sandbox keeps it out of every folder saved hidden now: each
/// saved folder, or a folder above it, was hidden when the agent started. A launch that
/// recorded no hidden folders, as a pane from before the engine kept them, hides none.
/// Inputs: the digests its launch recorded and the saved folders. Output: the answer.
pub fn covers(built: Option<&[String]>, saved: &[PathBuf]) -> bool {
    assert!(saved.len() <= HIDDEN_PATHS_MAX);
    assert!(built.is_none_or(|built| built.len() <= HIDDEN_PATHS_MAX));
    saved.iter().all(|folder| {
        built.is_some_and(|built| {
            folder
                .ancestors()
                .any(|above| built.contains(&digest(above)))
        })
    })
}

/// The most engine runs the hidden folders run list holds at once; a run that would pass it
/// is refused as if its hidden folders could not be read, so the list never misses one.
const RUNS_MAX: usize = 256;

/// One kind of engine run, not a team agent, that hides a project's saved folders from its
/// own start (SA-VID14-R2-N2): headless master turns (SA-VID14-MASTER-TURNS), research
/// jobs, the overview, roadmap and onboarding agents, code graph builds and lockfile
/// rebuilds. An instructions cleanup run reads only an empty
/// engine folder, so it never sees a project folder and is not one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Master,
    Research,
    Overview,
    Roadmap,
    Onboarding,
    CodeGraph,
    Lockfiles,
}

/// One running engine run as the list keeps it: its project and the folders it hid as
/// [`digest`]s, none until it has read them, and its kind. No path or project text is kept.
struct Running {
    project: String,
    hidden: Option<Vec<String>>,
    kind: RunKind,
}

/// The engine runs now running that hid folders from their start, by a key their
/// [`HiddenRun`] holds, at most [`RUNS_MAX`].
struct Registry {
    runs: BTreeMap<u64, Running>,
    next: u64,
}

impl Registry {
    const fn new() -> Self {
        Self {
            runs: BTreeMap::new(),
            next: 0,
        }
    }

    /// Lists one run under a key never handed out before. Input: the run. Output: its key,
    /// or none when the list is full or the keys ran out.
    fn add(&mut self, running: Running) -> Option<u64> {
        assert!(digest_plain(&running.project));
        assert!(
            running
                .hidden
                .as_ref()
                .is_none_or(|hidden| hidden.len() <= HIDDEN_PATHS_MAX)
        );
        if self.runs.len() >= RUNS_MAX {
            return None;
        }
        let key = self.next;
        self.next = key.checked_add(1)?;
        let fresh = self.runs.insert(key, running).is_none();
        assert!(fresh);
        assert!(self.runs.len() <= RUNS_MAX);
        Some(key)
    }

    /// Takes one run off the list. Input: the key [`Registry::add`] gave it.
    fn remove(&mut self, key: u64) {
        let removed = self.runs.remove(&key);
        assert!(removed.is_some());
    }

    /// Names the kinds of one project's runs whose start hid neither a saved folder nor a
    /// folder above it, each once. Inputs: the project digest and the saved folders.
    /// Output: the kinds in their order.
    fn seeing(&self, project: &str, saved: &[PathBuf]) -> Vec<RunKind> {
        assert!(digest_plain(project));
        let kinds: BTreeSet<RunKind> = self
            .runs
            .values()
            .filter(|run| run.project == project && !covers(run.hidden.as_deref(), saved))
            .map(|run| run.kind)
            .collect();
        kinds.into_iter().collect()
    }
}

static RUNS: Mutex<Registry> = Mutex::new(Registry::new());

/// Held by one engine run for its whole life, from before it reads its hidden folders until
/// it ends; dropping it, on a return, an error or a panic, takes the run off the list.
#[must_use = "the run is listed only while this is held"]
#[derive(Debug)]
pub struct HiddenRun(u64);

impl HiddenRun {
    /// Records, once, the folders this run's sandbox hides after it has read them, so from
    /// then on only a folder saved hidden later names it (SA-VID14 r1). Input: the folders,
    /// at most [`HIDDEN_PATHS_MAX`], none for a run that is not confined.
    pub fn hid(&self, hidden: &[PathBuf]) {
        assert!(hidden.len() <= HIDDEN_PATHS_MAX);
        let digests = hidden.iter().map(|folder| digest(folder)).collect();
        let mut runs = RUNS.lock().unwrap_or_else(PoisonError::into_inner);
        let run = runs.runs.get_mut(&self.0);
        assert!(run.as_ref().is_some_and(|run| run.hidden.is_none()));
        if let Some(run) = run {
            run.hidden = Some(digests);
        }
    }
}

impl Drop for HiddenRun {
    fn drop(&mut self) {
        RUNS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(self.0);
    }
}

/// Puts one engine run on the list as it starts, before it reads the project's saved hidden
/// folders, keeping only a digest of its project and its kind, so a hide saved meanwhile
/// never misses it: until [`HiddenRun::hid`] records what it hid, it hides nothing. Inputs:
/// the kind and the project id. Output: the guard to hold until it ends, or none when full.
pub fn starting(kind: RunKind, project: &str) -> Option<HiddenRun> {
    let entry = Running {
        project: digest(Path::new(project)),
        hidden: None,
        kind,
    };
    let key = RUNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .add(entry)?;
    Some(HiddenRun(key))
}

/// Names the kinds of one project's running engine runs that can still open a folder saved
/// hidden now, each once: those whose start hid neither it nor a folder above it. Inputs:
/// the project id and the saved folders. Output: the kinds in their order.
pub fn runs_seeing(project: &str, saved: &[PathBuf]) -> Vec<RunKind> {
    RUNS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .seeing(&digest(Path::new(project)), saved)
}

/// Checks one folder the owner wants this project's agents kept out of: an absolute existing
/// folder, taken in its canonical form, that sits strictly inside the project root. A folder
/// outside the project, the project root itself, or a path that is not a folder is refused,
/// as is text that is not plain: on Windows a drive path joined by backslashes
/// ([`crate::paths::windows_clean_text`]), elsewhere one with no quote or backslash.
/// Inputs: the typed path and the project root. Output: the canonical folder, or a plain
/// refusal code.
pub fn checked(text: &str, project_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() {
        return Err("hidden_path_not_absolute");
    }
    #[cfg(unix)]
    let plain = !text
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'');
    #[cfg(not(unix))]
    let plain = crate::paths::windows_clean_text(text);
    if !plain {
        return Err("hidden_path_not_plain");
    }
    let real = canonical(typed).map_err(|_| "hidden_path_missing")?;
    if !real.is_dir() {
        return Err("hidden_path_not_folder");
    }
    let root = canonical(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    if !real.starts_with(&root) || real == root {
        return Err("hidden_path_outside_project");
    }
    Ok(real)
}

/// Checks a project's whole list of hidden folders as [`checked`] does, at most
/// [`HIDDEN_PATHS_MAX`], dropping repeats. Inputs: the typed paths and the project root.
/// Output: the canonical folders in order, or the first refusal code.
pub fn checked_all(texts: &[String], project_root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if texts.len() > HIDDEN_PATHS_MAX {
        return Err("hidden_paths_too_many");
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(texts.len());
    for text in texts {
        let path = checked(text, project_root)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// The longest the tracked-files check lets git run before it is killed and the folder is
/// refused as unchecked.
const CHECK_TIME: std::time::Duration = std::time::Duration::from_secs(5);
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(2);
/// The most of git's answer the check reads; a tracked folder shows in the first bytes.
pub const CHECK_BYTES_MAX: u64 = 64 * 1024;

/// Whether git tracks any file inside any of these folders, asked with one `ls-files -z` run
/// through the owner's git ([`AccessHooks::owner_git`]) on their paths joined by `/`, each as
/// `:(literal,icase)`, killed after 5 s; a tracked file lives in every agent's worktree and in
/// history. Inputs: the hooks, the project root and the folders. Output: [`answer`]'s, `Ok(false)` with no
/// folder inside the root or no `.git`, else unchecked when a folder or git cannot be read.
pub fn any_tracked(
    hooks: &AccessHooks,
    project_root: &Path,
    folders: &[PathBuf],
) -> Result<bool, &'static str> {
    use std::io::Read;
    use std::path::Component;
    use std::process::Stdio;
    let unchecked = "hidden_path_unchecked";
    if folders.is_empty() {
        return Ok(false);
    }
    let root = canonical(project_root).map_err(|_| unchecked)?;
    let mut specs = Vec::with_capacity(folders.len());
    for folder in folders {
        let real = canonical(folder).map_err(|_| unchecked)?;
        let Ok(relative) = real.strip_prefix(&root) else {
            continue;
        };
        let parts = relative
            .components()
            .map(|part| match part {
                Component::Normal(name) => name.to_str(),
                _ => None,
            })
            .collect::<Option<Vec<&str>>>()
            .filter(|parts| !parts.is_empty())
            .ok_or(unchecked)?;
        specs.push(parts.join("/"));
    }
    assert!(specs.len() <= folders.len());
    if specs.is_empty() {
        return Ok(false);
    }
    let mut command = match (hooks.owner_git)(&root) {
        Ok(Some(command)) => command,
        Ok(None) => return Ok(false),
        Err(()) => return Err(unchecked),
    };
    if cfg!(any(windows, target_os = "macos")) && !specs.iter().all(|spec| spec.is_ascii()) {
        return Err("hidden_path_name_unchecked");
    }
    let magic = ":(literal,icase)";
    let pathspecs: Vec<String> = specs.iter().map(|spec| format!("{magic}{spec}")).collect();
    assert!(
        pathspecs
            .iter()
            .zip(&specs)
            .all(|(pathspec, spec)| pathspec.strip_prefix(magic) == Some(spec.as_str()))
    );
    command
        .args(["ls-files", "-z", "--"])
        .args(&pathspecs)
        .stdout(Stdio::piped());
    let mut child = kernel_processes::spawn_gate::spawn(&mut command).map_err(|_| unchecked)?;
    let mut stdout = child.stdout.take().ok_or(unchecked)?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout).take(CHECK_BYTES_MAX).read_to_end(&mut buffer);
        buffer
    });
    let deadline = std::time::Instant::now() + CHECK_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unchecked);
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unchecked);
        }
        std::thread::sleep(CHECK_POLL);
    };
    let bytes = reader.join().map_err(|_| unchecked)?;
    assert!(bytes.len() as u64 <= CHECK_BYTES_MAX);
    answer(&bytes, &specs, status.success())
}

/// Judges git's NUL-separated answer for these paths: `Ok(true)` when it names a path in one of
/// them, ASCII case ignored as git's `icase` does, `Ok(false)` when it is empty and git exited
/// clean, else unchecked, also when any line is empty, falls in none of them, or is cut short
/// before the read cap. Inputs: the bytes read, the paths joined by `/` and whether git exited
/// clean. Output: the answer.
fn answer(bytes: &[u8], specs: &[String], clean: bool) -> Result<bool, &'static str> {
    let unchecked = "hidden_path_unchecked";
    let capped = bytes.len() as u64 >= CHECK_BYTES_MAX;
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    let tail = lines.pop().unwrap_or_default();
    if !tail.is_empty() && !capped {
        return Err(unchecked);
    }
    let within = |line: &[u8]| {
        specs.iter().any(|spec| {
            let spec = spec.as_bytes();
            line.get(..spec.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(spec))
                && matches!(line.get(spec.len()), None | Some(&b'/'))
        })
    };
    if !lines.iter().all(|line| within(line)) {
        return Err(unchecked);
    }
    match (lines.is_empty(), capped, clean) {
        (false, _, _) => Ok(true),
        (true, false, true) => Ok(false),
        _ => Err(unchecked),
    }
}

/// Refuses a list that holds a folder git tracks, and refuses the whole list when the check
/// cannot answer for it, with one git run for the whole list (`any_tracked`). Inputs: the
/// project root and the checked folders. Output: ok, `hidden_path_tracked`,
/// `hidden_path_unchecked` or `hidden_path_name_unchecked`.
pub fn none_tracked(
    hooks: &AccessHooks,
    project_root: &Path,
    folders: &[PathBuf],
) -> Result<(), &'static str> {
    if any_tracked(hooks, project_root, folders)? {
        Err("hidden_path_tracked")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists one run as a seam does: before its load, then what it hid. Inputs: the kind,
    /// the project id and the folders. Output: the guard.
    fn listed_run(kind: RunKind, project: &str, hidden: &[PathBuf]) -> HiddenRun {
        let run = starting(kind, project).unwrap();
        run.hid(hidden);
        run
    }

    fn project() -> tempfile::TempDir {
        #[cfg(unix)]
        let parent = std::fs::canonicalize("/tmp").unwrap();
        #[cfg(not(unix))]
        let parent = canonical(&std::env::temp_dir()).unwrap();
        tempfile::Builder::new()
            .prefix("ca-hidden-")
            .tempdir_in(parent)
            .unwrap()
    }

    #[test]
    fn a_folder_inside_the_project_is_accepted_and_one_outside_is_refused() {
        let root = project();
        let inside = root.path().join("notes");
        std::fs::create_dir(&inside).unwrap();
        let got = checked(inside.to_str().unwrap(), root.path()).unwrap();
        assert_eq!(got, canonical(&inside).unwrap());

        let outside = project();
        assert_eq!(
            checked(outside.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked(root.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked("relative/notes", root.path()),
            Err("hidden_path_not_absolute")
        );
    }

    #[test]
    fn at_most_eight_folders_are_checked() {
        let root = project();
        let mut texts = Vec::new();
        for n in 0..9 {
            let folder = root.path().join(format!("f{n}"));
            std::fs::create_dir(&folder).unwrap();
            texts.push(folder.to_str().unwrap().to_owned());
        }
        assert_eq!(
            checked_all(&texts, root.path()),
            Err("hidden_paths_too_many")
        );
        assert_eq!(
            checked_all(&texts[..HIDDEN_PATHS_MAX], root.path())
                .unwrap()
                .len(),
            HIDDEN_PATHS_MAX
        );
    }

    /// On Windows a hidden folder is typed as a drive path joined by backslashes, so one inside
    /// the project is accepted; a quote, a forward slash or a UNC path is still refused.
    #[cfg(not(unix))]
    #[test]
    fn a_windows_drive_path_inside_the_project_is_accepted() {
        let root = project();
        let base = canonical(root.path()).unwrap();
        let inside = base.join("notes");
        std::fs::create_dir(&inside).unwrap();
        let text = inside.to_str().unwrap();
        assert!(text.contains('\\'), "{text}");
        assert_eq!(checked(text, root.path()), Ok(inside.clone()));
        for bad in [
            format!("{text}\\a\"b"),
            format!("{}/notes", base.display()),
            r"\\server\share\notes".to_owned(),
        ] {
            assert_eq!(
                checked(&bad, root.path()),
                Err("hidden_path_not_plain"),
                "{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_whose_name_holds_a_quote_or_backslash_is_refused() {
        let root = project();
        for bad in ["/tmp/a\"b", "/tmp/a\\b", "/tmp/a'b", "/tmp/a\nb"] {
            assert_eq!(checked(bad, root.path()), Err("hidden_path_not_plain"));
        }
    }

    /// V243-WIN-HIDDEN-GIT (leader 09:21, #590): git's answer counts only when every line is
    /// whole and falls in an asked folder, itself or below it, ASCII case ignored as git's
    /// `icase` does; an empty line, a line in no asked folder (a sibling sharing the name's
    /// start), or one cut short before the read cap is unchecked. Past the cap the cut tail is
    /// dropped and the whole lines still count.
    #[test]
    fn an_answer_line_cut_short_or_in_no_asked_folder_is_unchecked() {
        let specs = ["secret".to_owned(), "conf/keys".to_owned()];
        let unchecked = Err("hidden_path_unchecked");
        assert_eq!(answer(b"", &specs, true), Ok(false));
        assert_eq!(answer(b"", &specs, false), unchecked);
        assert_eq!(answer(b"secret/key.txt\0", &specs, true), Ok(true));
        assert_eq!(answer(b"conf/keys\0secret/a/b\0", &specs, false), Ok(true));
        assert_eq!(answer(b"Secret/x\0CONF/Keys/y\0", &specs, true), Ok(true));
        for doubt in [
            &b"secrets/key.txt\0"[..],
            &b"conf/key\0"[..],
            &b"Secrets/x\0"[..],
            &b"secret/key.txt\0\0"[..],
            &b"secret/key.txt"[..],
            &b"\0"[..],
        ] {
            let shown = String::from_utf8_lossy(doubt).into_owned();
            assert_eq!(answer(doubt, &specs, true), unchecked, "{shown:?}");
        }
        let cap = usize::try_from(CHECK_BYTES_MAX).unwrap();
        let mut full = b"secret/key.txt\0".repeat(cap / 15 + 1);
        full.truncate(cap);
        assert_ne!(full.last(), Some(&0));
        assert_eq!(answer(&full, &specs, false), Ok(true));
        assert_eq!(answer(&b"x".repeat(cap), &specs, false), unchecked);
    }

    #[test]
    fn an_agent_sees_a_hidden_folder_unless_it_or_a_folder_above_was_hidden_at_its_start() {
        let secrets = PathBuf::from("/tmp/app/secrets");
        let inner = secrets.join("keys");
        let vendor = PathBuf::from("/tmp/app/vendor");
        let built = vec![digest(&secrets)];
        assert!(digest_plain(&built[0]), "{}", built[0]);
        assert_ne!(digest(&secrets), digest(&vendor));
        assert!(covers(Some(&built), std::slice::from_ref(&secrets)));
        assert!(
            covers(Some(&built), std::slice::from_ref(&inner)),
            "a folder above it was hidden"
        );
        assert!(!covers(Some(&built), &[secrets.clone(), vendor.clone()]));
        assert!(
            !covers(Some(&[]), std::slice::from_ref(&secrets)),
            "started before the hide"
        );
        assert!(
            !covers(None, &[inner]),
            "a launch that kept no list hides none"
        );
        assert!(covers(None, &[]), "nothing hidden, nothing to see");
        assert!(
            !digest_plain("ABCDEF"),
            "upper case or short is not a digest"
        );
    }

    /// SA-VID14-R2-N2: an engine run that started before a folder was hidden is listed by
    /// its kind until it ends; one whose start hid the folder, or a folder above it, is
    /// not, and another project's list never shows it.
    #[test]
    fn a_run_started_before_a_folder_was_hidden_is_listed_by_kind_until_it_ends() {
        let project = "n2-listed";
        let (above, inside, other) = (
            PathBuf::from("/tmp/n2-listed/vault"),
            PathBuf::from("/tmp/n2-listed/vault/keys"),
            PathBuf::from("/tmp/n2-listed/dump"),
        );
        let research = listed_run(RunKind::Research, project, std::slice::from_ref(&above));
        assert!(runs_seeing(project, std::slice::from_ref(&inside)).is_empty());
        assert_eq!(
            runs_seeing(project, &[inside.clone(), other.clone()]),
            vec![RunKind::Research]
        );
        assert!(runs_seeing("n2-listed-elsewhere", std::slice::from_ref(&other)).is_empty());
        let overview = listed_run(RunKind::Overview, project, &[above.clone(), other.clone()]);
        let twice = listed_run(RunKind::Research, project, &[]);
        assert_eq!(
            runs_seeing(project, &[inside.clone(), other.clone()]),
            vec![RunKind::Research],
            "a kind is named once, and a run that hid both is not named"
        );
        drop((research, twice));
        assert!(runs_seeing(project, &[inside, other]).is_empty());
        drop(overview);
    }

    /// SA-VID14-R2-N2 (leader 08:04): a run leaves the list on every exit, a normal
    /// return, an early error return and a panic.
    #[test]
    fn a_run_leaves_the_list_on_every_exit_path() {
        let project = "n2-exits";
        let saved = [PathBuf::from("/tmp/n2-exits/secret")];
        let held = listed_run(RunKind::Roadmap, project, &[]);
        assert_eq!(runs_seeing(project, &saved), vec![RunKind::Roadmap]);
        drop(held);
        assert!(runs_seeing(project, &saved).is_empty(), "a return");

        let refused = || -> Result<(), &'static str> {
            let run = starting(RunKind::Lockfiles, project).ok_or("full")?;
            run.hid(&[]);
            assert_eq!(runs_seeing(project, &saved), vec![RunKind::Lockfiles]);
            Err("sandbox_refused")?;
            Ok(())
        };
        assert_eq!(refused(), Err("sandbox_refused"));
        assert!(
            runs_seeing(project, &saved).is_empty(),
            "an early error return"
        );

        let panicked = std::panic::catch_unwind(|| {
            let _run = listed_run(RunKind::Onboarding, project, &[]);
            assert_eq!(runs_seeing(project, &saved), vec![RunKind::Onboarding]);
            panic!("the run ended in a panic");
        });
        let payload = panicked.unwrap_err();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"the run ended in a panic"),
            "the run panicked after it was listed, not in an assert"
        );
        assert!(runs_seeing(project, &saved).is_empty(), "a panic");
    }

    /// SA-VID14-R2-N2 (leader 08:04): the list keeps digests and the run kind only, never a
    /// folder path or the project id.
    #[test]
    fn the_run_list_holds_digests_and_kinds_only() {
        let project = "n2-plain-project";
        let secret = PathBuf::from("/tmp/n2-plain/secret-plain");
        let _run = listed_run(RunKind::CodeGraph, project, std::slice::from_ref(&secret));
        let runs = RUNS.lock().unwrap_or_else(PoisonError::into_inner);
        let ours: Vec<&Running> = runs
            .runs
            .values()
            .filter(|run| run.project == digest(Path::new(project)))
            .collect();
        assert_eq!(ours.len(), 1, "the run is kept while it runs");
        let run = ours[0];
        assert_eq!(run.kind, RunKind::CodeGraph);
        assert!(digest_plain(&run.project));
        assert_eq!(run.hidden, Some(vec![digest(&secret)]));
        for kept in std::iter::once(&run.project).chain(run.hidden.iter().flatten()) {
            assert!(digest_plain(kept), "{kept}");
            assert!(
                !kept.contains("n2-plain") && !kept.contains("secret"),
                "{kept}"
            );
        }
    }

    /// SA-VID14-R2-N2: the list refuses a run past its bound, so callers fail closed, and
    /// takes one again once a run ends; keys are never reused.
    #[test]
    fn the_run_list_refuses_past_its_bound_and_takes_one_again_once_one_ends() {
        let mut registry = Registry::new();
        let entry = || Running {
            project: digest(Path::new("n2-bound")),
            hidden: None,
            kind: RunKind::Research,
        };
        let keys: Vec<u64> = (0..RUNS_MAX)
            .map(|_| registry.add(entry()).unwrap())
            .collect();
        assert_eq!(
            keys.iter().collect::<BTreeSet<_>>().len(),
            RUNS_MAX,
            "every key is new"
        );
        assert_eq!(registry.add(entry()), None, "a full list refuses");
        assert_eq!(registry.runs.len(), RUNS_MAX);
        registry.remove(keys[0]);
        let again = registry.add(entry()).unwrap();
        assert!(!keys.contains(&again), "a key is never reused");
        assert_eq!(registry.runs.len(), RUNS_MAX);
    }

    /// SA-VID14 r1 (rv-code4 F1, rv-sec2 F2): a run is listed before it reads the saved
    /// hidden folders, so a hide saved meanwhile names it. Until it records what it hid it
    /// counts as hiding nothing; after, only folders it did not hide name it.
    #[test]
    fn a_run_listed_before_its_load_is_named_for_any_saved_folder_until_it_records_its_folders() {
        let project = "n2-before-load";
        let (vault, dump) = (
            PathBuf::from("/tmp/n2-before-load/vault"),
            PathBuf::from("/tmp/n2-before-load/dump"),
        );
        let run = starting(RunKind::Research, project).unwrap();
        assert!(
            runs_seeing(project, &[]).is_empty(),
            "nothing saved, nothing named"
        );
        assert_eq!(
            runs_seeing(project, std::slice::from_ref(&vault)),
            vec![RunKind::Research],
            "a hide saved before its load names it"
        );
        run.hid(std::slice::from_ref(&vault));
        assert!(
            runs_seeing(project, std::slice::from_ref(&vault)).is_empty(),
            "a folder it hid does not"
        );
        assert_eq!(
            runs_seeing(project, &[vault, dump]),
            vec![RunKind::Research],
            "a folder hidden after its load does"
        );
        drop(run);
    }

    /// SA-VID14 r1 (rv-sec2 F1): a run whose stop was uncertain, or whose processes the
    /// owner keeps, forgets its guard as it forgets its lease, so it stays named while its
    /// processes may still run, until the engine starts again.
    #[test]
    fn a_run_left_running_stays_named_after_its_launch_returns() {
        let project = "n2-left-running";
        let saved = [PathBuf::from("/tmp/n2-left-running/secret")];
        let run = listed_run(RunKind::Research, project, &[]);
        std::mem::forget(run);
        assert_eq!(runs_seeing(project, &saved), vec![RunKind::Research]);
    }
}
