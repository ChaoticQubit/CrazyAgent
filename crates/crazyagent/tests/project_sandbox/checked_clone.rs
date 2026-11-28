use super::*;

/// An Own branch agent's clone the engine made and checked once (DEC-102), for the tests
/// of what an agent plants after that check: a committed project, `add_agent_clone`,
/// `record_worktree_start`, the agent's sandbox, its first commit and the turn end where
/// the engine checks the clone's store and fetches it.
struct CheckedClone {
    _owner: owner_support::Owner,
    case: Case,
    clone: PathBuf,
    git_dir: PathBuf,
    db: rusqlite::Connection,
    workspace: crazyagent::landing::Workspace,
    confined: ProjectSandbox,
    profile: PathBuf,
    git: String,
}

impl CheckedClone {
    /// Makes and checks the clone. Output: the clone and the change set its first turn
    /// end made.
    fn new() -> (Self, crazyagent::landing::ChangeSetId) {
        use crazyagent::landing::{AgentKind, ChangeAgent, Workspace};
        assert!(Path::new(AGENT_GIT).is_file());
        let case = case();
        for arguments in [
            &["init", "-q"][..],
            &["config", "user.email", "owner@local"],
            &["config", "user.name", "owner"],
            &["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            let made = owner_git(&case.project, &case.home, arguments);
            assert!(made.status.success(), "{made:?}");
        }
        let worktrees = case.project.join(".crazyagent").join("worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        let clone = worktrees.join("agent-1");
        crazyagent::landing::add_agent_clone(&case.project, &clone).unwrap();
        let clone = clone.canonicalize().unwrap();
        let git_dir = crazyagent::agent_git::git_dir(&clone);
        let engine = case.root.join("engine");
        fs::create_dir(&engine).unwrap();
        let owner = owner_support::owner_on(&case.root, &engine);
        let db = crazyagent::landing::open(&engine).unwrap();
        crazyagent::landing::record_worktree_start(&db, &case.project, &clone, &git_dir).unwrap();
        let workspace = Workspace {
            project: crazyagent::protocol::ProjectId::new("project-1").unwrap(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "1".into(),
                label: "Agent 1".into(),
            },
            repository: case.project.clone(),
            worktree: clone.clone(),
            git_dir: git_dir.clone(),
        };
        let profile = case.profiles.join("agent.sb");
        let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
            .confine_to_clone(&clone)
            .unwrap();
        let checked = Self {
            _owner: owner,
            case,
            clone,
            git_dir,
            db,
            workspace,
            confined,
            profile,
            git: format!(
                "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
            ),
        };
        checked.commit("one.txt");
        let set = checked
            .turn_end("turn-1")
            .expect("the engine checked and fetched the clone");
        (checked, set)
    }

    /// Runs a script as the agent, under its profile. Input: the script. Output: its result.
    fn agent(&self, script: &str) -> Output {
        run(&self.confined, &self.profile, script)
    }

    /// Commits one new file as the agent. Input: the file name.
    fn commit(&self, file: &str) {
        let committed = self.agent(&format!(
            "cd {} && printf 'agent work\\n' > {file} && {git} add {file} && {git} commit -q -m {file}",
            quoted(&self.clone),
            git = self.git,
        ));
        assert!(committed.status.success(), "{committed:?}");
    }

    /// Ends one successful turn, where the engine checks the clone's store and fetches its
    /// commits. Input: the turn name. Output: the change set the work belongs to.
    fn turn_end(&self, name: &str) -> Option<crazyagent::landing::ChangeSetId> {
        use crazyagent::landing::{TurnEnd, TurnOutcome};
        crazyagent::landing::record_turn_end(
            &self.db,
            &self.workspace,
            &TurnEnd {
                outcome: TurnOutcome::Succeeded,
                turn: name.into(),
                prompt_line: "Write work".into(),
            },
        )
        .unwrap()
    }

    /// Makes a second commit and turn end, then checks that the engine's check passed and
    /// the project's one change set ends at the clone's HEAD, so only the agent's own
    /// commits landed. Input: the set the first turn end made.
    fn lands_only_its_own_commits(&self, set: &crazyagent::landing::ChangeSetId) {
        self.commit("two.txt");
        let again = self
            .turn_end("turn-2")
            .expect("the next check passes: nothing foreign was planted");
        assert_eq!(&again, set);
        let head = self.agent(&format!(
            "cd {} && {} rev-parse HEAD",
            quoted(&self.clone),
            self.git
        ));
        let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
        let sets = crazyagent::landing::list(&self.db, &self.workspace.project).unwrap();
        assert_eq!(sets.len(), 1, "{sets:#?}");
        assert_eq!(
            sets[0].head, head,
            "the set holds only the agent's own commits"
        );
    }
}

#[test]
fn a_multi_pack_index_an_agent_plants_after_the_engine_checked_its_clone_never_lands() {
    let (checked, set) = CheckedClone::new();
    let git = &checked.git;
    let pack = checked.git_dir.join("objects").join("pack");
    let index = pack.join("multi-pack-index");
    for script in [
        format!("printf x > {}", quoted(&index)),
        format!("printf x > {}", quoted(&pack.join("MULTI-PACK-INDEX"))),
        format!(
            "cd {} && {git} repack -q -a -d && {git} multi-pack-index write",
            quoted(&checked.clone)
        ),
    ] {
        write_refused(&checked.confined, &checked.profile, &script, &index);
    }
    checked.lands_only_its_own_commits(&set);
    assert!(!index.exists());
}

/// A pack and a loose object in a repository outside the project, the objects an agent
/// would slip into its clone's store. Input: the case. Output: the pack file, its index,
/// the loose object's file and its id, and the id of a commit only the pack holds.
fn foreign_objects(case: &Case) -> (PathBuf, PathBuf, PathBuf, String, String) {
    let foreign = case.outside.join("foreign");
    fs::create_dir(&foreign).unwrap();
    let git = |arguments: &[&str]| {
        let done = owner_git(&foreign, &case.home, arguments);
        assert!(done.status.success(), "{done:?}");
        String::from_utf8(done.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    fs::write(foreign.join("foreign.txt"), "foreign work\n").unwrap();
    git(&["add", "foreign.txt"]);
    git(&["commit", "-q", "-m", "foreign"]);
    let commit = git(&["rev-parse", "HEAD"]);
    git(&["repack", "-q", "-a", "-d"]);
    fs::write(foreign.join("loose.txt"), "foreign loose\n").unwrap();
    let loose = git(&["hash-object", "-w", "loose.txt"]);
    let objects = foreign.join(".git").join("objects");
    let named = |suffix: &str| {
        fs::read_dir(objects.join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_str().unwrap().ends_with(suffix))
            .unwrap()
    };
    let file = objects.join(&loose[..2]).join(&loose[2..]);
    assert!(file.is_file());
    (named(".pack"), named(".idx"), file, loose, commit)
}

#[test]
fn a_folder_an_agent_moves_into_its_clones_store_after_the_engine_checked_it_never_lands() {
    let (checked, set) = CheckedClone::new();
    let (pack_file, index_file, loose_file, loose, commit) = foreign_objects(&checked.case);
    let objects = checked.git_dir.join("objects");
    let fanout = objects.join(&loose[..2]);
    assert!(objects.join("pack").is_dir() && fanout.is_dir());
    let stage = checked.clone.join("stage");
    let staged_pack = stage.join("pack");
    let staged_fanout = stage.join(&loose[..2]);
    let pack_name = pack_file.file_name().unwrap().to_str().unwrap();
    let index_name = index_file.file_name().unwrap().to_str().unwrap();
    let staged = checked.agent(&format!(
        "mkdir -p {sp} {sf} && ln -s {pf} {sp}/{pack_name} && ln -s {xf} {sp}/{index_name} && printf x > {sp}/multi-pack-index && ln -s {lf} {sf}/{rest} && mkdir {stage}/control && /usr/bin/perl -e 'rename($ARGV[0], $ARGV[1]) or exit 1' {stage}/control {stage}/renamed",
        sp = quoted(&staged_pack),
        sf = quoted(&staged_fanout),
        pf = quoted(&pack_file),
        xf = quoted(&index_file),
        lf = quoted(&loose_file),
        rest = &loose[2..],
        stage = quoted(&stage),
    ));
    assert!(staged.status.success(), "{staged:?}");
    assert!(
        stage.join("renamed").is_dir(),
        "perl renames a folder in the sandbox"
    );
    let rename = |from: &Path, to: &Path| {
        format!(
            "/usr/bin/perl -e 'rename($ARGV[0], $ARGV[1]) or exit 1' {} {}",
            quoted(from),
            quoted(to)
        )
    };
    let pack = objects.join("pack");
    let planted_pack = pack.join(pack_name);
    let planted_loose = fanout.join(&loose[2..]);
    let upper = objects.join(loose[..2].to_uppercase());
    for (script, target) in [
        (
            format!(
                "mv {} {}",
                quoted(&pack),
                quoted(&objects.join("pack-away"))
            ),
            objects.join("pack-away"),
        ),
        (
            format!(
                "mv {} {} && mv {} {}",
                quoted(&pack),
                quoted(&objects.join("pack-away")),
                quoted(&staged_pack),
                quoted(&pack)
            ),
            planted_pack.clone(),
        ),
        (rename(&staged_pack, &pack), planted_pack.clone()),
        (format!("rmdir {}", quoted(&fanout)), stage.join("never")),
        (rename(&staged_fanout, &fanout), planted_loose.clone()),
        (
            format!("mv {} {}", quoted(&upper), quoted(&stage.join("moved"))),
            stage.join("moved"),
        ),
        (
            format!(
                "rmdir {} && mv {} {}",
                quoted(&fanout),
                quoted(&staged_fanout),
                quoted(&fanout)
            ),
            planted_loose.clone(),
        ),
    ] {
        write_refused(&checked.confined, &checked.profile, &script, &target);
    }
    assert!(pack.is_dir() && fanout.is_dir());
    assert!(!pack.join("multi-pack-index").exists());
    fs::remove_dir_all(&stage).unwrap();
    checked.lands_only_its_own_commits(&set);
    for id in [loose.as_str(), commit.as_str()] {
        let found = owner_git(
            &checked.case.project,
            &checked.case.home,
            &["cat-file", "-e", id],
        );
        assert!(
            !found.status.success(),
            "the project store never holds {id}"
        );
    }
}
