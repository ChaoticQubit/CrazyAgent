use super::*;

/// Waits for a condition the jobs under test reach on their own threads, looking every
/// `WAIT_STEP` at most `WAIT_STEPS` times. Input: the condition. Output: whether it held.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    (0..WAIT_STEPS).any(|_| {
        done() || {
            std::thread::sleep(WAIT_STEP);
            false
        }
    })
}

/// One Homebrew prefix's brew for these rows: it notes in the log when each formula's
/// install starts and ends, waits for that formula's own gate file, and puts the harness's
/// file in the prefix. Input: the machine. Output: the log's path.
fn queued_brew(machine: &Machine) -> PathBuf {
    let root = machine.root.path();
    let log = root.join("brew.log");
    let bin = root.join("brew/bin");
    executable(
        &bin.join("brew"),
        &format!(
            "#!/bin/sh\necho \"start $3\" >> {log}\ni=0\nwhile [ ! -e {root}/gate-$3 ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done\ncase \"$3\" in aider) name=aider;; block-goose-cli) name=goose;; esac\nprintf '#!/bin/sh\\necho \"%s 3.0.0\"\\n' \"$name\" > {bin}/$name\nchmod 755 {bin}/$name\necho \"end $3\" >> {log}\n",
            log = log.display(),
            root = root.display(),
            bin = bin.display(),
        ),
    );
    std::fs::create_dir_all(machine.roots().home).unwrap();
    log
}

/// The engine side of one machine for these rows: its database, roots and one update state.
struct Queue {
    machine: Machine,
    db: Connection,
    roots: Roots,
    updates: Arc<Updates>,
    log: PathBuf,
}

impl Queue {
    fn new() -> Self {
        let machine = Machine::new();
        let log = queued_brew(&machine);
        let roots = machine.roots();
        Self {
            machine,
            db: db(),
            roots,
            updates: Updates::new(),
            log,
        }
    }
    fn install(&self, id: &'static str) -> Result<(), &'static str> {
        let bin = self.machine.root.path().join("brew/bin");
        self.updates
            .install(&self.db, &entry(id, None), &self.roots, move || {
                found_in(&bin, id)
            })
    }
    fn read(&self, id: &str) -> HarnessVersions {
        self.updates
            .versions(&self.db, &entry(id, None), &self.roots, false)
    }
    fn logged(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn open(&self, formula: &str) {
        let gate = self.machine.root.path().join(format!("gate-{formula}"));
        std::fs::write(gate, "go\n").unwrap();
    }
}

/// V243-HARNESS-UPDATE-PARALLEL (owner 12:1x): a native Claude Code update and a Homebrew
/// install of Aider share no folder, so both run at once on one engine, and a second update
/// of Claude Code while its first one runs is still refused as busy.
#[test]
fn two_harnesses_that_share_no_folder_run_at_once_and_the_same_one_twice_is_refused() {
    let claude = slow_update_case("claude-code");
    let aider = slow_install_case("aider");
    let updates = claude.updates.clone();
    let roots = aider.machine.roots();
    claude.start().unwrap();
    let bin = aider.bin();
    updates
        .install(&aider.db, &aider.entry, &roots, move || {
            found_in(&bin, "aider")
        })
        .unwrap();
    let read = || updates.versions(&aider.db, &aider.entry, &roots, false);
    let args = aider.machine.root.path().join("tool.args");
    assert!(
        eventually(|| args.exists()),
        "Aider's installer runs while Claude Code updates"
    );
    assert!(claude.versions(false).running);
    assert!(read().running);
    assert_eq!(read().waiting, None);
    assert_eq!(claude.start().unwrap_err(), "harness_update_busy");
    std::fs::write(aider.gate.as_ref().unwrap(), "go\n").unwrap();
    claude.finish();
    assert!(claude.versions(false).last.unwrap().ok);
    let done = read();
    assert!(!done.running);
    assert!(done.last.unwrap().ok);
}

/// V243-HARNESS-UPDATE-PARALLEL (leader 12:21): two installs into one Homebrew prefix would
/// change the same folder, so the second is not refused: it waits, named for the first,
/// with its installer not started and no failure recorded, then starts by itself once the
/// first ends, and both finish in that order.
#[test]
fn a_job_on_a_folder_another_job_holds_waits_named_for_it_then_starts_by_itself() {
    let queue = Queue::new();
    queue.install("aider").unwrap();
    assert!(eventually(|| queue.logged() == "start aider\n"));
    queue.install("goose").unwrap();
    assert!(
        eventually(|| queue.read("goose").waiting.as_deref() == Some("aider")),
        "{:?}",
        queue.read("goose")
    );
    let goose = queue.read("goose");
    assert!(goose.running);
    assert_eq!(goose.last, None, "waiting is not a failure");
    queue.open("block-goose-cli");
    assert_eq!(queue.logged(), "start aider\n", "Goose's installer waits");
    queue.open("aider");
    assert!(eventually(|| queue.updates.idle()));
    assert_eq!(
        queue.logged(),
        "start aider\nend aider\nstart block-goose-cli\nend block-goose-cli\n"
    );
    for id in ["aider", "goose"] {
        let done = queue.read(id);
        assert_eq!(done.waiting, None);
        assert!(done.last.unwrap().ok, "{id}");
    }
}

/// V243-HARNESS-UPDATE-PARALLEL: a waiting job's tool is checked when the job is asked for
/// and again when its turn comes, since the job it waited for may have changed that folder.
/// A brew that another group can write by then is refused as the job's record, with the
/// installer never started.
#[test]
fn a_waiting_job_checks_its_tool_again_when_its_turn_comes() {
    let queue = Queue::new();
    queue.install("aider").unwrap();
    assert!(eventually(|| queue.logged() == "start aider\n"));
    queue.install("goose").unwrap();
    assert!(eventually(|| queue.read("goose").waiting.is_some()));
    let brew = queue.machine.root.path().join("brew/bin/brew");
    std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o775)).unwrap();
    let refused = install_command("goose", &queue.roots).unwrap_err();
    queue.open("block-goose-cli");
    queue.open("aider");
    assert!(eventually(|| queue.updates.idle()));
    assert_eq!(
        queue.logged(),
        "start aider\nend aider\n",
        "Goose's installer never ran"
    );
    let last = queue.read("goose").last.unwrap();
    assert!(!last.ok);
    assert_eq!(last.code.as_deref(), Some(refused));
}

/// V243-HARNESS-UPDATE-PARALLEL: a job holds npm's prefix, the Homebrew prefix brew runs
/// from, and the Homebrew prefix of a Homebrew npm whose node a brew job may replace, each in
/// its real spelling; a self-updater holds no folder.
#[test]
fn a_job_holds_its_npm_prefix_and_the_homebrew_prefix_its_tool_runs_from() {
    let machine = Machine::new();
    machine.npm("brew");
    let roots = machine.roots();
    let engine = roots.npm.clone().unwrap();
    std::fs::create_dir_all(&engine.prefix).unwrap();
    let real = |path: &Path| std::fs::canonicalize(path).unwrap();
    let homebrew = machine.root.path().join("brew");
    let fixed = |program: PathBuf, prefix: Option<PathBuf>| Fixed {
        program,
        args: vec!["install"],
        search: Vec::new(),
        prefix,
    };
    let npm = fixed(engine.program.clone(), Some(engine.prefix.clone()));
    let mut both = vec![real(&engine.prefix), real(&homebrew)];
    both.sort();
    assert_eq!(super::super::turn::folders(&npm, &roots), both);
    let brew = fixed(homebrew.join("bin/brew"), None);
    assert_eq!(
        super::super::turn::folders(&brew, &roots),
        vec![real(&homebrew)]
    );
    let own = fixed(roots.home.join(".local/bin/claude"), None);
    assert!(super::super::turn::folders(&own, &roots).is_empty());
}

/// V243-HARNESS-UPDATE-PARALLEL (owner pair): two harnesses installed with npm into one
/// prefix that is neither Homebrew's nor the engine's, as nvm or a user npm prefix leaves
/// them. Their updates both write that prefix, so the second waits named for the first and
/// starts by itself once the first ends, and both finish.
#[test]
fn two_npm_updates_into_one_prefix_take_turns_and_both_finish() {
    let machine = Machine::new();
    let root = machine.root.path().to_path_buf();
    let log = root.join("npm.log");
    executable(
        &root.join("node/bin/npm"),
        &format!(
            "#!/bin/sh\ncase \"$3\" in @anthropic-ai/*) job=claude;; @openai/*) job=codex;; esac\necho \"start $job\" >> {log}\ni=0\nwhile [ ! -e {root}/gate-$job ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done\necho \"end $job\" >> {log}\n",
            log = log.display(),
            root = root.display(),
        ),
    );
    let claude = root.join("node/lib/node_modules/@anthropic-ai/claude-code-darwin-arm64/claude");
    executable(&claude, "#!/bin/sh\necho '1.0.0 (Claude Code)'\n");
    let codex = root.join("node/lib/node_modules/@openai/codex/bin/codex.js");
    executable(&codex, "#!/bin/sh\necho 'codex-cli 0.1.0'\n");
    std::fs::create_dir_all(machine.roots().home).unwrap();
    let (db, roots, updates) = (db(), machine.roots(), Updates::new());
    let logged = || std::fs::read_to_string(&log).unwrap_or_default();
    let read = |id: &str, path: &Path| {
        updates.versions(&db, &entry(id, Some(path.to_path_buf())), &roots, false)
    };
    updates
        .start(
            &db,
            &entry("claude-code", Some(claude.clone())),
            &roots,
            |_| {},
        )
        .unwrap();
    assert!(eventually(|| logged() == "start claude\n"));
    updates
        .start(&db, &entry("codex", Some(codex.clone())), &roots, |_| {})
        .unwrap();
    assert!(
        eventually(|| read("codex", &codex).waiting.as_deref() == Some("claude-code")),
        "{:?}",
        read("codex", &codex)
    );
    assert!(read("codex", &codex).running);
    std::fs::write(root.join("gate-codex"), "go\n").unwrap();
    assert_eq!(logged(), "start claude\n", "Codex's npm waits");
    std::fs::write(root.join("gate-claude"), "go\n").unwrap();
    assert!(eventually(|| updates.idle()));
    assert_eq!(
        logged(),
        "start claude\nend claude\nstart codex\nend codex\n"
    );
    for (id, path) in [("claude-code", &claude), ("codex", &codex)] {
        let done = read(id, path);
        assert_eq!(done.waiting, None);
        assert!(done.last.unwrap().ok, "{id}");
    }
}

/// V243-SETUP-PARALLEL: setup jobs share the harness jobs' line under their tools' program
/// names and one run name, so no catalog harness may go by one of those names.
#[test]
fn no_catalog_harness_goes_by_a_setup_tool_or_the_setup_run_name() {
    let setup: Vec<&str> = crate::setup::Dependency::ALL
        .iter()
        .map(|dep| dep.program())
        .chain([crate::setup::SETUP_JOB])
        .collect();
    assert_eq!(setup.len(), crate::setup::Dependency::ALL.len() + 1);
    for spec in harnesses::specs() {
        assert!(!setup.contains(&spec.id), "{}", spec.id);
    }
    assert_eq!(
        super::super::turn::jobs_max(),
        harnesses::specs().len() + setup.len()
    );
}

/// V243-SETUP-PARALLEL: setup jobs name folders not made yet, and two spellings of one
/// folder, one through a link, are one folder in the line: the second job waits for the
/// first and takes its turn once the first ends.
#[test]
fn a_folder_not_made_yet_is_one_folder_whatever_path_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let turns = super::super::turn::Turns::default();
    let tool = |base: &Path| vec![base.join("setup/.jobs/node")];
    let first = turns
        .take("claude-code", tool(&real), Duration::from_secs(5))
        .unwrap();
    assert!(
        turns
            .take("codex", tool(&alias), Duration::from_millis(200))
            .is_none(),
        "the same folder through a link waits"
    );
    drop(first);
    assert!(
        turns
            .take("codex", tool(&alias), Duration::from_millis(200))
            .is_some()
    );
}
