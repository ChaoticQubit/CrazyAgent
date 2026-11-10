use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REPORT_MARGIN: Duration = Duration::from_secs(20);
const LOCK_SEEN_TIME: Duration = Duration::from_secs(20);
const NOT_SHOWN: &str = "<not shown>";

/// One scratch project with two committed files, the engine's folder and the server's
/// private folder, all by their real paths.
struct Case {
    _scratch: tempfile::TempDir,
    project: PathBuf,
    engine: PathBuf,
    private: PathBuf,
    gate: Mutex<()>,
}

impl Case {
    /// Makes the folders and the first commit. Output: the case.
    fn new() -> Self {
        let scratch = tempfile::Builder::new()
            .prefix("ca-index-")
            .tempdir_in(fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let base = fs::canonicalize(scratch.path()).unwrap();
        let (project, engine) = (base.join("project"), base.join("engine"));
        let private = engine.join("git-index");
        for folder in [&project, &engine, &private] {
            fs::create_dir_all(folder).unwrap();
        }
        ok(&project, None, &["init", "-q", "-b", "main"]);
        fs::write(project.join("a.txt"), "one\n").unwrap();
        fs::write(project.join("b.txt"), "two\n").unwrap();
        ok(&project, None, &["add", "a.txt", "b.txt"]);
        ok(&project, None, &["commit", "-q", "-m", "first"]);
        Self {
            _scratch: scratch,
            project,
            engine,
            private,
            gate: Mutex::new(()),
        }
    }

    /// The real index. Output: its path.
    fn index(&self) -> PathBuf {
        self.project.join(".git").join("index")
    }

    /// The real index's lock. Output: its path.
    fn lock(&self) -> PathBuf {
        self.project.join(".git").join("index.lock")
    }

    /// One slot's two private files. Input: the slot. Output: the base and the work file.
    fn slot(&self, slot: &str) -> (PathBuf, PathBuf) {
        (
            self.private.join(format!("{slot}.base")),
            self.private.join(format!("{slot}.work")),
        )
    }

    /// Copies the real index to both of a slot's files, as the shim does. Input: the slot.
    /// Output: the work file.
    fn seed(&self, slot: &str) -> PathBuf {
        let (base, work) = self.slot(slot);
        fs::copy(self.index(), &base).unwrap();
        fs::copy(self.index(), &work).unwrap();
        work
    }

    /// Runs the engine step for a slot. Input: the slot. Output: its outcome.
    fn carried(&self, slot: &str) -> GitOutcome {
        carry(&self.gate, &self.private, &self.engine, &self.project, slot).unwrap()
    }

    /// What `git ls-files -s` prints for the real index. Output: the text.
    fn listed(&self) -> String {
        ok(&self.project, None, &["ls-files", "-s"])
    }

    /// The names of what is staged against HEAD. Output: the text.
    fn staged(&self) -> String {
        ok(&self.project, None, &["diff", "--cached", "--name-only"])
    }

    /// Rewrites one path's entry as `--index-info` leaves it, with no stat data, so a
    /// refresh has to read the file. Input: the path. Output: none.
    fn zero_stat(&self, path: &str) {
        let line = self.listed();
        let entry = line
            .lines()
            .find(|entry| entry.ends_with(&format!("\t{path}")))
            .unwrap();
        let mut command = plain(&self.project, None, &["update-index", "-z", "--index-info"]);
        let lines = self.engine.join("zero-stat");
        fs::write(&lines, format!("{entry}\0")).unwrap();
        command.stdin(Stdio::from(fs::File::open(&lines).unwrap()));
        let done = crate::spawn_gate::output(&mut command).unwrap();
        assert!(done.status.success(), "{done:?}");
    }

    /// Makes one path's entry as new as the index, so the next writer of the index re-checks
    /// it against the file. Input: the path. Output: none.
    fn make_racy(&self, path: &str) {
        let modified = fs::metadata(self.project.join(path))
            .unwrap()
            .modified()
            .unwrap();
        let index = fs::File::options().write(true).open(self.index()).unwrap();
        index.set_modified(modified).unwrap();
    }

    /// Whether the index holds no modification time for a path. Input: the path. Output:
    /// the answer.
    fn stat_is_empty(&self, path: &str) -> bool {
        ok(&self.project, None, &["ls-files", "--debug", path]).contains("mtime: 0:0")
    }

    /// Makes `.gitattributes` a FIFO nothing writes to, so a refresh that reads it hangs.
    /// Output: none.
    fn plant_fifo(&self) {
        let mut command = Command::new("/usr/bin/mkfifo");
        command.arg(self.project.join(".gitattributes"));
        assert!(
            crate::spawn_gate::output(&mut command)
                .unwrap()
                .status
                .success()
        );
    }
}

/// A git of the owner's, outside any sandbox. Inputs: the folder, the index file if any and
/// the arguments. Output: the command.
fn plain(dir: &Path, index: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new(crate::research::runner::TOOLS_GIT);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=Owner"])
        .args(["-c", "user.email=owner@example.invalid"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}

/// Runs [`plain`] to its end. Inputs: as [`plain`]. Output: its output.
fn output(dir: &Path, index: Option<&Path>, args: &[&str]) -> Output {
    crate::spawn_gate::output(&mut plain(dir, index, args)).unwrap()
}

/// Runs [`plain`], which must exit 0. Inputs: as [`plain`]. Output: what it printed.
fn ok(dir: &Path, index: Option<&Path>, args: &[&str]) -> String {
    let done = output(dir, index, args);
    assert!(
        done.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).unwrap()
}

/// The bytes of a version 2 index written straight to disk, as a sandboxed writer can: the
/// entries sorted by path, no stat data, and the all-zero trailer git accepts in place of a
/// checksum. Input: path and 40-digit object name pairs. Output: the file's bytes.
fn raw_index(entries: &[(&[u8], &str)]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(path, _)| path.to_vec());
    let mut file = b"DIRC".to_vec();
    file.extend(2u32.to_be_bytes());
    file.extend(u32::try_from(sorted.len()).unwrap().to_be_bytes());
    for (path, oid) in sorted {
        let start = file.len();
        file.extend([0u8; 24]);
        file.extend(0o100_644u32.to_be_bytes());
        file.extend([0u8; 12]);
        for pair in oid.as_bytes().chunks(2) {
            let digits = std::str::from_utf8(pair).unwrap();
            file.push(u8::from_str_radix(digits, 16).unwrap());
        }
        file.extend(u16::try_from(path.len().min(0xFFF)).unwrap().to_be_bytes());
        file.extend(path);
        let padding = 8 - (file.len() - start) % 8;
        file.extend(std::iter::repeat_n(0u8, padding));
    }
    file.extend([0u8; 20]);
    file
}

/// The object name of a stored blob. Inputs: the folder and the text. Output: the name.
fn blob(project: &Path, text: &str) -> String {
    let file = project.join("blob-source");
    fs::write(&file, text).unwrap();
    let name = ok(project, None, &["hash-object", "-w", "blob-source"]);
    fs::remove_file(&file).unwrap();
    name.trim().to_owned()
}

/// A script that leaves `marker` when it runs and passes its input through. Inputs: the
/// script path and the marker. Output: none.
fn marking_script(script: &Path, marker: &Path) {
    fs::write(
        script,
        format!("#!/bin/sh\n: > '{}'\nexec cat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Stage 3 of the delta: one path changed by the private index, and a path the owner staged
/// in the real index after the seed is kept, since the delta is measured against the
/// command's own base and never against the real index.
#[test]
fn a_stage_carries_only_what_differs_and_keeps_what_the_owner_staged_meanwhile() {
    let case = Case::new();
    let work = case.seed("101");
    fs::write(case.project.join("a.txt"), "changed\n").unwrap();
    fs::write(case.project.join("b.txt"), "owner\n").unwrap();
    ok(&case.project, None, &["add", "b.txt"]);
    ok(&case.project, Some(&work), &["add", "a.txt"]);
    let outcome = case.carried("101");
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(case.staged(), "a.txt\nb.txt\n");
}

/// SECAUDIT-IDE-SERVER-INDEX: a private index written raw, with every spelling git itself
/// refuses, never reaches the real index. The reply is a failure naming only a count, the
/// valid entry lands, and the owner's own `git restore .` writes nothing into `.git`.
#[test]
fn a_raw_written_private_index_never_reaches_the_real_one() {
    let case = Case::new();
    let hostile = blob(&case.project, "planted\n");
    let good = blob(&case.project, "valid\n");
    let bad: [&[u8]; 15] = [
        b".git/hooks/x",
        b".GIT/hooks/x",
        b".Git/config",
        b"../x",
        b"a/../../b",
        b"/abs",
        b"sub/.git/config",
        b"sub/.GIT/hooks/x",
        ".g\u{200c}it/config".as_bytes(),
        b"git~1/config",
        b".git /config",
        b".git./config",
        b"a//b",
        b"./x",
        b".",
    ];
    let listed = case.listed();
    let (mut names, mut owned) = (Vec::new(), Vec::new());
    for line in listed.lines() {
        let (meta, path) = line.split_once('\t').unwrap();
        owned.push((
            path.as_bytes().to_vec(),
            meta.split(' ').nth(1).unwrap().to_owned(),
        ));
    }
    for (path, oid) in &owned {
        names.push((path.as_slice(), oid.as_str()));
    }
    names.extend(bad.iter().map(|path| (*path, hostile.as_str())));
    names.push((b"valid.txt", good.as_str()));
    case.seed("201");
    fs::write(case.slot("201").1, raw_index(&names)).unwrap();

    let outcome = case.carried("201");
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.stderr,
        "CrazyAgent refused 15 index entries git itself calls invalid.\n"
    );
    assert_eq!(
        ok(&case.project, None, &["ls-files"]),
        "a.txt\nb.txt\nvalid.txt\n"
    );
    ok(&case.project, None, &["restore", "."]);
    let hooks = case.project.join(".git").join("hooks").join("x");
    assert!(!hooks.exists(), "{}", hooks.display());
    assert_eq!(
        fs::read_to_string(case.project.join("valid.txt")).unwrap(),
        "valid\n"
    );
    for hostile_text in [".git/hooks", "../x", "/abs"] {
        assert!(!outcome.stderr.contains(hostile_text), "{NOT_SHOWN}");
    }
}

/// Hostile files in the private folder change nothing: a link, a FIFO that must not hang
/// the engine, the folder swapped for a link, and a file past the bound, with the real
/// index byte-identical and no copy left in the engine's folder after each.
#[test]
fn hostile_private_files_leave_the_real_index_alone() {
    let case = Case::new();
    let before = fs::read(case.index()).unwrap();
    let refused = |slot: &str, code: &str| {
        let outcome = case.carried(slot);
        assert_eq!(outcome.code, 1, "{code}");
        assert!(
            outcome.stderr.contains(&format!("({code})")),
            "{}",
            outcome.stderr
        );
        assert_eq!(fs::read(case.index()).unwrap(), before, "{code}");
        let left: Vec<_> = fs::read_dir(&case.engine)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("carry-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    };

    let work = case.seed("1");
    fs::remove_file(&work).unwrap();
    std::os::unix::fs::symlink(case.index(), &work).unwrap();
    refused("1", "index_unreadable");

    let work = case.seed("2");
    fs::remove_file(&work).unwrap();
    let mut fifo = Command::new("/usr/bin/mkfifo");
    fifo.arg(&work);
    assert!(
        crate::spawn_gate::output(&mut fifo)
            .unwrap()
            .status
            .success()
    );
    refused("2", "index_unreadable");

    let work = case.seed("3");
    fs::OpenOptions::new()
        .write(true)
        .open(&work)
        .unwrap()
        .set_len(INDEX_BYTES_MAX)
        .unwrap();
    refused("3", "index_unreadable");

    let work = case.seed("4");
    fs::OpenOptions::new()
        .write(true)
        .open(&work)
        .unwrap()
        .set_len(INDEX_BYTES_MAX + 1)
        .unwrap();
    refused("4", "index_too_large");

    case.seed("5");
    fs::remove_file(case.slot("5").1).unwrap();
    refused("5", "index_work_missing");

    case.seed("6");
    let aside = case.engine.join("aside");
    fs::rename(&case.private, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &case.private).unwrap();
    refused("6", "index_unreadable");
}

/// A slot is the shim's pid and nothing else: at most ten digits, with no separator, so a
/// request can never name another file.
#[test]
fn a_slot_of_any_other_shape_is_refused() {
    let case = Case::new();
    let accepted = carry(
        &case.gate,
        &case.private,
        &case.engine,
        &case.project,
        "1234567890",
    );
    assert_eq!(accepted.unwrap().code, 0);
    for slot in ["12345678901", "", "1/2", "..", "../1", "1a", "-1", "1\n"] {
        let refused = carry(&case.gate, &case.private, &case.engine, &case.project, slot);
        assert!(refused.is_err(), "{slot:?}");
    }
}

/// The listing parser takes only what `ls-files -z -s` prints.
#[test]
fn only_a_strict_listing_is_read() {
    let oid = "a".repeat(40);
    let good = format!("100644 {oid} 0\ta.txt\0100644 {oid} 1\tb\tc\0");
    let found = entries(good.as_bytes()).unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.contains_key(&(&b"b\tc"[..], 1)));
    assert!(entries(b"").unwrap().is_empty());
    for bad in [
        format!("100644 {oid} 0\ta.txt"),
        format!("100644 {oid} 0\t\0"),
        format!("100600 {oid} 0\ta\0"),
        format!("100644 {oid} 4\ta\0"),
        format!("100644 {} 0\ta\0", "a".repeat(39)),
        format!("100644 {} 0\ta\0", "A".repeat(40)),
        format!("100644 {oid} 0 x\ta\0"),
        format!("100644 {oid} 0\ta\0100644 {oid} 0\ta\0"),
    ] {
        assert_eq!(
            entries(bad.as_bytes()).err(),
            Some("index_malformed"),
            "{bad}"
        );
    }
}

/// The delta removes a changed path first and then adds each of its stages, and it names
/// nothing that did not change.
#[test]
fn the_delta_names_each_changed_path_once_with_all_its_stages() {
    let (one, two) = ("1".repeat(40), "2".repeat(40));
    let zero = "0".repeat(40);
    let base = format!("100644 {one} 0\tsame\0100644 {one} 0\tgone\0100644 {one} 0\tmerge\0");
    let work = format!(
        "100644 {one} 0\tsame\0100644 {one} 1\tmerge\0100644 {two} 2\tmerge\0100644 {two} 0\tnew\0"
    );
    let (base, work) = (
        entries(base.as_bytes()).unwrap(),
        entries(work.as_bytes()).unwrap(),
    );
    let expected = format!(
        "0 {zero}\tgone\0\
         0 {zero}\tmerge\0100644 {one} 1\tmerge\0100644 {two} 2\tmerge\0\
         0 {zero}\tnew\0100644 {two} 0\tnew\0"
    );
    assert_eq!(String::from_utf8(delta(&base, &work)).unwrap(), expected);
    assert!(delta(&base, &base).is_empty());
}

/// A refresh changes only stat data: `git ls-files -s` is identical before and after, and
/// the entry `--index-info` left without stat data has it afterwards.
#[test]
fn a_refresh_changes_only_stat_data() {
    let case = Case::new();
    case.zero_stat("a.txt");
    let before = case.listed();
    assert!(case.stat_is_empty("a.txt"));
    refresh(&case.project);
    assert_eq!(case.listed(), before);
    assert!(
        !case.stat_is_empty("a.txt"),
        "the refresh saved no stat data"
    );
}

/// A refresh runs nothing from the project: a planted `.gitattributes` naming a filter and
/// a post-index-change hook leave no marker, where an ordinary refresh runs both. The marks
/// are cleared after the setup's own git writes the index, since that writer runs both.
#[test]
fn a_refresh_runs_no_filter_and_no_hook() {
    let case = Case::new();
    let marks = case.engine.join("marks");
    fs::create_dir(&marks).unwrap();
    let (filter_mark, hook_mark) = (marks.join("filter"), marks.join("hook"));
    let filter = case.project.join("filter.sh");
    marking_script(&filter, &filter_mark);
    fs::create_dir_all(case.project.join(".git/hooks")).unwrap();
    marking_script(
        &case.project.join(".git/hooks/post-index-change"),
        &hook_mark,
    );
    ok(
        &case.project,
        None,
        &["config", "filter.evil.clean", filter.to_str().unwrap()],
    );
    fs::write(case.project.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    let clear = || {
        for mark in [&filter_mark, &hook_mark] {
            if mark.exists() {
                fs::remove_file(mark).unwrap();
            }
        }
    };
    case.zero_stat("a.txt");
    clear();

    refresh(&case.project);
    assert!(!filter_mark.exists(), "the project's filter ran");
    assert!(!hook_mark.exists(), "the project's hook ran");

    case.zero_stat("a.txt");
    clear();
    ok(&case.project, None, &["update-index", "-q", "--refresh"]);
    assert!(filter_mark.exists(), "the control ran no filter");
    assert!(hook_mark.exists(), "the control ran no hook");
}

/// Waits until just after a new second starts, so the steps after it share that second.
/// Output: none.
fn start_of_a_second() {
    let into = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::thread::sleep(Duration::from_nanos(u64::from(1_000_000_000 - into)));
    std::thread::sleep(Duration::from_millis(5));
}

/// A change saved in the second the index was written, at the old size, looks clean by its
/// stat data, so git keeps the entry racy and compares the file. The apply cannot compare
/// from the git folder and must not end that racy status, or the change leaves the owner's
/// status and a later `add -u` or checkout passes over it.
#[test]
fn a_same_size_change_beside_a_stage_stays_in_the_owners_status() {
    let case = Case::new();
    let path = case.project.join("b.txt");
    let second = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let held = (0..5).any(|_| {
        start_of_a_second();
        let started = second();
        fs::write(&path, "BBBB\n").unwrap();
        ok(&case.project, None, &["add", "b.txt"]);
        fs::write(&path, "CCCC\n").unwrap();
        second() == started
    });
    assert!(held, "the three steps never fit one second");
    std::thread::sleep(Duration::from_millis(1100));
    let work = case.seed("501");
    fs::write(case.project.join("a.txt"), "changed\n").unwrap();
    ok(&case.project, Some(&work), &["add", "a.txt"]);

    let outcome = case.carried("501");
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(
        ok(&case.project, None, &["status", "--porcelain"]),
        "M  a.txt\nMM b.txt\n",
        "the change to b.txt left the owner's status"
    );
}

/// The exec limit of a refresh, seen in the one trace its write limit leaves: a filter that
/// rewrites what it reads would make the entry look changed and leave its stat data unsaved.
/// The refresh never starts it, so the file hashes as itself and the stat data is saved.
#[test]
fn a_refresh_starts_no_filter_that_would_rewrite_the_file() {
    let case = Case::new();
    let filter = case.project.join("filter.sh");
    fs::write(
        &filter,
        "#!/bin/sh\nwhile read -r line; do :; done\necho rewritten\n",
    )
    .unwrap();
    fs::set_permissions(&filter, fs::Permissions::from_mode(0o755)).unwrap();
    ok(
        &case.project,
        None,
        &["config", "filter.evil.clean", filter.to_str().unwrap()],
    );
    fs::write(case.project.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    case.zero_stat("a.txt");
    assert!(case.stat_is_empty("a.txt"));

    refresh(&case.project);
    assert!(
        !case.stat_is_empty("a.txt"),
        "the refresh started the project's filter"
    );
}

/// A writer of the index re-checks an entry as new as the index through the project's
/// filter, and a filter marked required ends that writer if it cannot start. The apply
/// looks at no work tree file, so the stage lands and the filter never starts, where an
/// ordinary writer runs it on the same entry. The refresh fails on it and the stage stands.
#[test]
fn a_stage_lands_past_a_required_filter_on_a_racy_entry() {
    let case = Case::new();
    fs::write(case.project.join("a.txt"), "changed\n").unwrap();
    let work = case.seed("401");
    ok(&case.project, Some(&work), &["add", "a.txt"]);
    let marks = case.engine.join("marks");
    fs::create_dir(&marks).unwrap();
    let mark = marks.join("filter");
    let filter = case.project.join("filter.sh");
    marking_script(&filter, &mark);
    for (key, value) in [
        ("filter.evil.clean", filter.to_str().unwrap()),
        ("filter.evil.required", "true"),
    ] {
        ok(&case.project, None, &["config", key, value]);
    }
    fs::write(case.project.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    case.make_racy("b.txt");

    let outcome = case.carried("401");
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(outcome.stderr, "");
    assert!(!mark.exists(), "the project's filter ran");
    assert_eq!(case.staged(), "a.txt\n");

    case.make_racy("b.txt");
    case.zero_stat("a.txt");
    assert!(mark.exists(), "the control found no racy entry");
}

/// A refresh hung on a tracked-path FIFO ends at its limit, TERM first, so git removes its
/// own lock; the stage has landed and stands, and the owner is told nothing.
#[test]
fn a_hung_refresh_ends_leaves_no_lock_and_the_stage_stands() {
    let case = Case::new();
    fs::write(case.project.join("a.txt"), "changed\n").unwrap();
    let work = case.seed("301");
    ok(&case.project, Some(&work), &["add", "a.txt"]);
    case.plant_fifo();

    let started = Instant::now();
    let outcome = case.carried("301");
    let spent = started.elapsed();
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(outcome.stderr, "");
    assert!(spent >= REFRESH_TIME, "the refresh did not hang: {spent:?}");
    assert!(
        spent < REFRESH_TIME + TERM_GRACE + REPORT_MARGIN,
        "the refresh outlived its limit: {spent:?}"
    );
    assert!(!case.lock().exists(), "a killed refresh left its lock");
    assert_eq!(case.staged(), "a.txt\n");
}

/// The next stage waits for a running refresh in the project instead of losing its short
/// lock retry to it, so one stage cannot fail another.
#[test]
fn the_next_stage_waits_for_a_running_refresh() {
    let case = Arc::new(Case::new());
    fs::write(case.project.join("a.txt"), "changed\n").unwrap();
    fs::write(case.project.join("b.txt"), "changed\n").unwrap();
    let first = case.seed("401");
    let second = case.seed("402");
    ok(&case.project, Some(&first), &["add", "a.txt"]);
    ok(&case.project, Some(&second), &["add", "b.txt"]);
    case.plant_fifo();

    let running = Arc::clone(&case);
    let hung = std::thread::spawn(move || running.carried("401"));
    let deadline = Instant::now() + LOCK_SEEN_TIME;
    while !case.lock().exists() {
        assert!(Instant::now() < deadline, "the refresh never took the lock");
        std::thread::sleep(Duration::from_millis(5));
    }
    let next = case.carried("402");
    assert_eq!(next.code, 0, "{}", next.stderr);
    assert_eq!(hung.join().unwrap().code, 0);
    assert_eq!(case.staged(), "a.txt\nb.txt\n");
    assert!(!case.lock().exists());
}

/// Measures `git status` as VS Code runs it after staging 5,000 of 20,000 files in place
/// and through the engine step. Run by hand with `--ignored --nocapture`. It fails above
/// twice the in-place time.
#[test]
#[ignore = "builds a 20,000 file repository; measures status speed"]
fn status_after_a_large_stage_is_no_slower_than_twice_in_place() {
    const STATUS: [&str; 4] = ["status", "-z", "-uall", "--porcelain=v1"];
    let median = |case: &Case| {
        let mut samples: Vec<Duration> = (0..7)
            .map(|_| {
                let mut command = plain(&case.project, None, &STATUS);
                command.env("GIT_OPTIONAL_LOCKS", "0");
                let started = Instant::now();
                assert!(
                    crate::spawn_gate::output(&mut command)
                        .unwrap()
                        .status
                        .success()
                );
                started.elapsed()
            })
            .collect();
        samples.sort();
        samples[3]
    };
    let case = Case::new();
    let padding = "x".repeat(3000);
    for number in 0..20_000 {
        let folder = case.project.join(format!("d{:02}", number % 100));
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("f{number:06}.txt")),
            format!("{number}\n{padding}\n"),
        )
        .unwrap();
    }
    ok(&case.project, None, &["add", "-A"]);
    ok(&case.project, None, &["commit", "-q", "-m", "tree"]);
    for number in 0..5_000 {
        let folder = case.project.join(format!("d{:02}", number % 100));
        fs::write(
            folder.join(format!("f{number:06}.txt")),
            format!("changed {number}\n{padding}\n"),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));
    ok(&case.project, None, &["add", "-A"]);
    let in_place = median(&case);
    ok(&case.project, None, &["reset", "-q"]);
    ok(&case.project, None, &["status", "--porcelain"]);
    std::thread::sleep(Duration::from_secs(2));
    let work = case.seed("501");
    ok(&case.project, Some(&work), &["add", "-A"]);
    let started = Instant::now();
    let outcome = case.carried("501");
    let step = started.elapsed();
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let carried = median(&case);
    println!("status_ms in_place={in_place:?} carried={carried:?} step={step:?}");
    assert!(carried <= in_place * 2, "{carried:?} against {in_place:?}");
}
