#![cfg(unix)]

#[cfg(target_os = "macos")]
use crazyagent::landing::MergeOutcome;
use crazyagent::landing::{
    self, AcceptOutcome, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace,
};
use crazyagent::protocol::ProjectId;
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PROJECT: &str = "project-1";

/// Runs git as the owner would, with no system or global config, an isolated home and a
/// fixed identity. Takes the folder, the home and the arguments. Returns how it ended.
fn git_output(folder: &Path, home: &Path, arguments: &[&str]) -> Output {
    crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args(["-c", "user.email=agent@local", "-c", "user.name=agent"])
            .arg("-C")
            .arg(folder)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

/// Runs git as [`git_output`] does and requires it to succeed. Takes the folder, the home
/// and the arguments. Returns its trimmed standard output.
fn git(folder: &Path, home: &Path, arguments: &[&str]) -> String {
    let output = git_output(folder, home, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// One team agent in its own clone: its team id, worktree, git folder and branch.
struct Agent {
    id: i64,
    worktree: PathBuf,
    git_dir: PathBuf,
    branch: String,
}

/// One owner repository with the engine store beside it and a lead, team agent 1, in its
/// own clone on `crazyagent/lead`, recorded as the engine records a team agent's worktree.
struct Team {
    _temp: tempfile::TempDir,
    #[cfg(target_os = "macos")]
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    db: Connection,
    id: ProjectId,
    lead: Agent,
}

impl Team {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (project, home, engine) =
            (root.join("project"), root.join("home"), root.join("engine"));
        for folder in [&project, &home, &engine] {
            fs::create_dir(folder).unwrap();
        }
        git(&project, &home, &["init", "-q", "-b", "main"]);
        git(&project, &home, &["config", "user.name", "Repo Owner"]);
        git(
            &project,
            &home,
            &["config", "user.email", "owner@example.invalid"],
        );
        fs::write(project.join("README"), "one\ntwo\nthree\n").unwrap();
        git(&project, &home, &["add", "README"]);
        git(&project, &home, &["commit", "-q", "-m", "Start"]);
        fs::create_dir_all(project.join(".git/info")).unwrap();
        fs::write(project.join(".git/info/exclude"), ".crazyagent/\n").unwrap();
        fs::create_dir_all(project.join(".crazyagent/worktrees")).unwrap();
        let db = landing::open(&engine).unwrap();
        crazyagent::team::initialize(&db).unwrap();
        crazyagent::coord::ensure(&db).unwrap();
        db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)")
            .unwrap();
        crazyagent::activity::initialize(&db).unwrap();
        let worktree = project.join(".crazyagent/worktrees/agent-1");
        let branch = landing::claim_task_branch(&db, &project, &worktree, "lead").unwrap();
        assert_eq!(branch, "crazyagent/lead");
        landing::add_agent_clone_on(&project, &worktree, None, &branch).unwrap();
        let lead = Agent {
            id: 1,
            git_dir: crazyagent::agent_git::git_dir(&worktree),
            worktree,
            branch,
        };
        let team = Self {
            _temp: temp,
            #[cfg(target_os = "macos")]
            root,
            project,
            home,
            db,
            id: ProjectId::new(PROJECT).unwrap(),
            lead,
        };
        team.record(&team.lead, None);
        team
    }

    /// Records a team agent and its worktree as the engine does when it makes one, and the
    /// worktree's start. Takes the agent and its lead, None for a lead of the master.
    fn record(&self, agent: &Agent, parent: Option<i64>) {
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
                 VALUES (?1,?2,'sub','claude-code',1,'working',0,0,?3)",
                params![agent.id, PROJECT, parent],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent.id,
                    PROJECT,
                    self.project.to_str().unwrap(),
                    agent.worktree.to_str().unwrap(),
                    agent.git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
        landing::record_worktree_start(&self.db, &self.project, &agent.worktree, &agent.git_dir)
            .unwrap();
    }

    /// Starts a worker of a lead in its own clone, cut from the lead's commit, on the branch
    /// the engine claims for it. Takes the worker's id, its lead and its name. Returns it.
    fn worker(&self, id: i64, lead: &Agent, name: &str) -> Agent {
        let worktree = self
            .project
            .join(".crazyagent/worktrees")
            .join(format!("agent-{id}"));
        let base =
            landing::agent_tip(&self.db, &self.project, &lead.worktree, &lead.git_dir).unwrap();
        let claimed = landing::claim_sub_agent_branch(
            &self.db,
            &self.project,
            &worktree,
            &lead.branch,
            Some(&lead.worktree),
            name,
        )
        .unwrap();
        let branch = claimed.expect("the lead's branch carries sub-agent branches");
        landing::add_agent_clone_on(&self.project, &worktree, Some(&base), &branch).unwrap();
        let agent = Agent {
            id,
            git_dir: crazyagent::agent_git::git_dir(&worktree),
            worktree,
            branch,
        };
        self.record(&agent, Some(lead.id));
        agent
    }

    fn git(&self, folder: &Path, arguments: &[&str]) -> String {
        git(folder, &self.home, arguments)
    }

    fn head(&self, agent: &Agent) -> String {
        self.git(&agent.worktree, &["rev-parse", "HEAD"])
    }

    /// Commits one file in an agent's clone as the agent does. Takes the agent, the path,
    /// the text and the message. Returns the new commit.
    fn commit(&self, agent: &Agent, path: &str, text: &str, message: &str) -> String {
        fs::write(agent.worktree.join(path), text).unwrap();
        self.git(&agent.worktree, &["add", path]);
        self.git(&agent.worktree, &["commit", "-q", "-m", message]);
        self.head(agent)
    }

    /// Merges a worker's branch into its lead's clone by hand with real git, as a lead does
    /// itself. Takes the lead and the worker. Returns the lead's new commit.
    fn hand_merge(&self, lead: &Agent, worker: &Agent) -> String {
        self.git(
            &lead.worktree,
            &[
                "fetch",
                "-q",
                worker.worktree.to_str().unwrap(),
                &worker.branch,
            ],
        );
        self.git(
            &lead.worktree,
            &["merge", "--no-ff", "-q", "--no-edit", "FETCH_HEAD"],
        );
        self.head(lead)
    }

    fn workspace(&self, agent: &Agent) -> Workspace {
        Workspace {
            project: self.id.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: agent.id.to_string(),
                label: format!("Agent {}", agent.id),
            },
            repository: self.project.clone(),
            worktree: agent.worktree.clone(),
            git_dir: agent.git_dir.clone(),
        }
    }

    /// Ends one successful turn of an agent, which records its work as a change set. Takes
    /// the agent. Returns the change set.
    fn turn_end(&self, agent: &Agent) -> ChangeSetId {
        landing::record_turn_end(
            &self.db,
            &self.workspace(agent),
            &TurnEnd {
                outcome: TurnOutcome::Succeeded,
                turn: "turn-1".into(),
                prompt_line: "Do the work".into(),
            },
        )
        .unwrap()
        .expect("the turn left work for review")
    }

    /// The stored state of a change set and the commit it landed as. Takes the set.
    fn state(&self, set: &ChangeSetId) -> (String, Option<String>) {
        self.db
            .query_row(
                "SELECT state,landed FROM landing_change_sets WHERE id=?1",
                [set.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn view_head(&self, set: &ChangeSetId) -> String {
        landing::change_set(&self.db, set).unwrap().unwrap().1.head
    }

    /// The engine's pending ref of a change set in the owner repository, empty once
    /// dropped. Takes the set.
    fn pending_ref(&self, set: &ChangeSetId) -> String {
        self.git(
            &self.project,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/crazyagent/pending/{}", set.as_str()),
            ],
        )
    }

    fn lands(&self, lands: &str) {
        self.db
            .execute(
                "INSERT INTO team_nesting(project,depth,lands) VALUES (?1,3,?2)
                 ON CONFLICT(project) DO UPDATE SET lands=excluded.lands",
                params![PROJECT, lands],
            )
            .unwrap();
    }

    fn owner_listed(&self) -> Vec<ChangeSetId> {
        let mut ids: Vec<ChangeSetId> = landing::owner_list(&self.db, &self.id)
            .unwrap()
            .into_iter()
            .map(|view| view.id)
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    /// The lead's own rights for a merge: its sandbox confined to its clone and the profile
    /// installed for it outside every writable root, as the engine installs a lead's.
    /// Returns the sandbox and the profile.
    #[cfg(target_os = "macos")]
    fn lead_rights(&self) -> (crazyagent::sandbox::ProjectSandbox, PathBuf) {
        let (home, tmp, profiles) = (
            self.root.join("lead/home"),
            self.root.join("lead/tmp"),
            self.root.join("profiles"),
        );
        for folder in [&home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        let sandbox = crazyagent::sandbox::ProjectSandbox::new(
            &self.project,
            &home,
            &tmp,
            crazyagent::sandbox::SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .confine_to_clone(&self.lead.worktree)
        .unwrap();
        let profile = profiles.join("agent-1.sb");
        sandbox.install(&profile).unwrap();
        (sandbox, profile)
    }
}

/// A lead cannot report its work complete while a sub-agent's work is not in its task
/// branch: a sub-agent branch with a commit the lead has not merged is listed until the lead
/// merges it with real git (feat/coord-spawn 3cf399d3, DEC-104 K5b).
#[test]
fn unmerged_lists_a_sub_agent_until_its_branch_is_in_the_lead() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    assert_eq!(fix.branch, "crazyagent/lead--fix");
    assert_eq!(
        landing::unmerged_sub_agents(&team.db, &team.id, 1).unwrap(),
        Vec::<String>::new(),
        "a sub-agent at its lead's commit has nothing to merge"
    );
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    assert_eq!(
        landing::unmerged_sub_agents(&team.db, &team.id, 1).unwrap(),
        vec!["crazyagent/lead--fix".to_owned()]
    );
    team.hand_merge(&team.lead, &fix);
    assert_eq!(
        landing::unmerged_sub_agents(&team.db, &team.id, 1).unwrap(),
        Vec::<String>::new()
    );
}

/// A sub-agent's branch is `<task branch>--<name>`, or its first free `-2` to `-64` form
/// when a branch in the lead's clone or one stored for another agent holds the name, and a
/// name past 40 bytes is cut (feat/coord-spawn 8a85b292).
#[test]
fn claim_names_task_dash_dash_name_and_skips_taken_names() {
    let team = Team::new();
    team.git(&team.lead.worktree, &["branch", "crazyagent/lead--fix"]);
    let first = team.worker(2, &team.lead, "fix");
    let second = team.worker(3, &team.lead, "fix");
    let long = team.worker(4, &team.lead, &format!("{}klmno", "abcdefghij".repeat(4)));
    assert_eq!(
        [first.branch, second.branch, long.branch],
        [
            "crazyagent/lead--fix-2".to_owned(),
            "crazyagent/lead--fix-3".to_owned(),
            format!("crazyagent/lead--{}", "abcdefghij".repeat(4)),
        ]
    );
    use crazyagent::agent_git::is_agent_branch;
    assert!(is_agent_branch("crazyagent/lead--fix-2"));
    assert!(!is_agent_branch("agent/lead--fix"));
    assert!(!is_agent_branch("crazyagent/lead--fix x"));
}

/// A lead's merge of its sub-agent runs git in the lead's own clone under the lead's
/// profile: a merge commit with both parents on the task branch and the sub-agent's branch
/// beside it, nothing to merge the second time, and the branch moved forward when the
/// sub-agent committed more (feat/coord-spawn 8a85b292).
#[cfg(target_os = "macos")]
#[test]
fn merge_into_the_lead_clone_under_its_profile() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    let before = team.head(&team.lead);
    let first = team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let (sandbox, profile) = team.lead_rights();
    let rights = landing::LeadGit {
        sandbox: &sandbox,
        profile: &profile,
    };
    let merge = || landing::merge_sub_agent(&team.db, &team.id, 1, 2, &rights);
    let MergeOutcome::Merged { commit, branch } = merge().unwrap() else {
        panic!("the first merge did not merge");
    };
    assert_eq!(branch, "crazyagent/lead--fix");
    let lead = &team.lead.worktree;
    assert_eq!(team.git(lead, &["rev-parse", "crazyagent/lead"]), commit);
    assert_eq!(
        team.git(lead, &["log", "-1", "--format=%P", &commit]),
        format!("{before} {first}")
    );
    assert_eq!(
        team.git(lead, &["rev-parse", "refs/heads/crazyagent/lead--fix"]),
        first
    );
    assert_eq!(team.git(lead, &["show", "HEAD:fix.txt"]), "fixed");
    assert_eq!(
        merge().unwrap(),
        MergeOutcome::AlreadyMerged {
            branch: "crazyagent/lead--fix".into()
        }
    );
    let second = team.commit(&fix, "fix.txt", "fixed twice\n", "Fix it again");
    let MergeOutcome::Merged { commit: again, .. } = merge().unwrap() else {
        panic!("the second merge did not merge");
    };
    assert_eq!(
        team.git(lead, &["log", "-1", "--format=%P", &again]),
        format!("{commit} {second}")
    );
    assert_eq!(
        team.git(lead, &["rev-parse", "refs/heads/crazyagent/lead--fix"]),
        second
    );
    let merged: Vec<(String, String)> =
        crazyagent::activity::list(&team.db, &team.id, crazyagent::activity::LIST_MAX)
            .unwrap()
            .into_iter()
            .filter(|item| item.kind == crazyagent::activity::ActivityKind::LeadMerged)
            .map(|item| (item.actor, item.text))
            .collect();
    let label = |agent: i64| crazyagent::team::events::label(&team.db, agent).unwrap();
    let once = (
        label(1),
        format!("Merged the work of {} from crazyagent/lead--fix", label(2)),
    );
    assert_eq!(
        merged,
        vec![once.clone(), once],
        "two merges, not the no-op"
    );
}

/// A merge refuses a lead with uncommitted changes, a lead on another branch and a worker
/// that is not its own, and a conflicting merge is undone, naming the file, with no merge
/// left in progress (feat/coord-spawn 8a85b292).
#[cfg(target_os = "macos")]
#[test]
fn merge_refusals_and_conflicts_leave_the_lead_clone_as_it_was() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "README", "one\nTWO\nthree\n", "Worker edit");
    let lead_tip = team.commit(&team.lead, "README", "one\n2\nthree\n", "Lead edit");
    let (sandbox, profile) = team.lead_rights();
    let rights = landing::LeadGit {
        sandbox: &sandbox,
        profile: &profile,
    };
    let merge = |lead, sub| landing::merge_sub_agent(&team.db, &team.id, lead, sub, &rights);
    let lead = &team.lead.worktree;
    assert_eq!(
        merge(1, 2).unwrap(),
        MergeOutcome::Conflict {
            branch: "crazyagent/lead--fix".into(),
            files: vec!["README".into()],
        }
    );
    assert!(
        !git_output(
            lead,
            &team.home,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        )
        .status
        .success(),
        "the conflicting merge was left in progress"
    );
    assert_eq!(
        (
            team.head(&team.lead),
            team.git(lead, &["status", "--porcelain", "--untracked-files=no"])
        ),
        (lead_tip.clone(), String::new())
    );
    fs::write(lead.join("README"), "uncommitted\n").unwrap();
    assert_eq!(merge(1, 2).unwrap_err().0, "changes_lead_uncommitted");
    team.git(lead, &["checkout", "-q", "--", "README"]);
    team.git(lead, &["checkout", "-q", "-b", "other"]);
    assert_eq!(merge(1, 2).unwrap_err().0, "changes_lead_off_task_branch");
    team.git(lead, &["checkout", "-q", "crazyagent/lead"]);
    assert_eq!(merge(2, 1).unwrap_err().0, "changes_not_your_sub_agent");
    assert_eq!(team.head(&team.lead), lead_tip);
}

/// A worker's set closes as accepted at the end of its lead's turn once the lead merged
/// its branch by hand, a resolved conflict included, landed as the lead's tip and with its
/// pending ref dropped; before the merge it stays open (fix/hand-merged-set 8f98740a).
#[test]
fn hand_merged_worker_set_closes_at_the_lead_turn_end() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "README", "one\nTWO\nthree\n", "Worker edit");
    let set = team.turn_end(&fix);
    team.commit(&team.lead, "README", "one\n2\nthree\n", "Lead edit");
    landing::settle_sub_agents(&team.db, &team.id, 1).unwrap();
    assert_eq!(team.state(&set), ("pending".to_owned(), None));
    let lead = &team.lead.worktree;
    team.git(
        lead,
        &["fetch", "-q", fix.worktree.to_str().unwrap(), &fix.branch],
    );
    assert!(
        !git_output(
            lead,
            &team.home,
            &["merge", "--no-ff", "--no-edit", "FETCH_HEAD"]
        )
        .status
        .success(),
        "the hand merge was meant to conflict"
    );
    fs::write(lead.join("README"), "one\nTWO and 2\nthree\n").unwrap();
    team.git(lead, &["add", "README"]);
    team.git(lead, &["commit", "-q", "--no-edit"]);
    let tip = team.head(&team.lead);
    landing::settle_sub_agents(&team.db, &team.id, 1).unwrap();
    assert_eq!(team.state(&set), ("accepted".to_owned(), Some(tip)));
    assert_eq!(team.pending_ref(&set), "");
}

/// OS-8 L4: a lead's merge that holds a worker's change set closes that set as landed and
/// releases the claims of the worker's review-ready work, so another agent's start on the
/// same file no longer overlaps it.
#[test]
fn a_lead_merge_covering_a_worker_set_releases_the_worker_s_claims() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.db
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (9,?1,'sub','claude-code',1,'working',0,0)",
            [PROJECT],
        )
        .unwrap();
    let now = i64::try_from(now_ms()).unwrap();
    let (work, _) = crazyagent::coord::start_work(
        &team.db,
        &team.id,
        2,
        "Fix".into(),
        vec!["README".into()],
        now,
    )
    .unwrap();
    crazyagent::coord::finish(
        &team.db,
        work,
        1,
        crazyagent::coord::Finish::ReviewReady,
        "Green.".into(),
        2,
        now,
    )
    .unwrap();
    team.commit(&fix, "README", "one\nTWO\nthree\n", "Worker edit");
    let set = team.turn_end(&fix);
    let start = || {
        crazyagent::coord::start_work(
            &team.db,
            &team.id,
            9,
            "Docs".into(),
            vec!["README".into()],
            now,
        )
        .unwrap()
        .1
        .len()
    };
    assert_eq!(start(), 1, "the worker's set still waits");
    let tip = team.hand_merge(&team.lead, &fix);
    landing::settle_sub_agents(&team.db, &team.id, 1).unwrap();
    assert_eq!(team.state(&set), ("accepted".to_owned(), Some(tip)));
    assert_eq!(start(), 0, "the merged worker set's claim is released");
}

/// When the owner accepts a lead's set that holds a worker's work, the worker's set closes
/// with it as accepted, landed as the owner's squash commit (fix/hand-merged-set 8f98740a).
#[test]
fn lead_accept_closes_worker_sets_it_holds() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.hand_merge(&team.lead, &fix);
    let lead_set = team.turn_end(&team.lead);
    let AcceptOutcome::Landed { commit, branch } = landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &team.view_head(&lead_set),
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the lead's accept conflicted");
    };
    assert_eq!(branch, "main");
    assert_eq!(team.git(&team.project, &["show", "main:fix.txt"]), "fixed");
    assert_eq!(
        team.state(&lead_set),
        ("accepted".to_owned(), Some(commit.clone()))
    );
    assert_eq!(
        team.state(&worker_set),
        ("accepted".to_owned(), Some(commit))
    );
    assert_eq!(team.pending_ref(&worker_set), "");
}

/// A lead that merged a worker's new file and then moved it holds the work, so the owner's
/// Accept of the lead closes the worker's set and does not offer it again (TEAM-4-L1).
#[test]
fn lead_accept_closes_a_worker_set_whose_new_file_the_lead_moved() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.hand_merge(&team.lead, &fix);
    team.git(&team.lead.worktree, &["mv", "fix.txt", "moved.txt"]);
    team.git(&team.lead.worktree, &["commit", "-q", "-m", "Move it"]);
    let lead_set = team.turn_end(&team.lead);
    let AcceptOutcome::Landed { commit, .. } = landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &team.view_head(&lead_set),
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the lead's accept conflicted");
    };
    assert_eq!(
        team.git(&team.project, &["show", "main:moved.txt"]),
        "fixed"
    );
    assert_eq!(
        team.state(&worker_set),
        ("accepted".to_owned(), Some(commit))
    );
    assert_eq!(team.pending_ref(&worker_set), "");
    assert!(!team.owner_listed().contains(&worker_set));
}

/// A lead whose merge kept none of a worker's new file and whose own file is only like it
/// does not hold the work: the worker's set stays open and offered after the owner's
/// Accept of the lead (TEAM-4-L1).
#[test]
fn lead_accept_leaves_open_a_worker_set_whose_file_the_lead_has_only_a_like_one_of() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    merge_keeping_nothing(&team, &fix, true);
    team.commit(&team.lead, "moved.txt", "fixed\nmore\n", "Own file");
    let lead_set = team.turn_end(&team.lead);
    landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &team.view_head(&lead_set),
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap();
    assert_eq!(team.state(&lead_set).0, "accepted");
    assert_eq!(team.state(&worker_set), ("pending".to_owned(), None));
    assert!(team.owner_listed().contains(&worker_set));
}

/// An empty file a worker added is not held by an unrelated new empty file on the lead's
/// line, though git would pair any two empty files as a rename: the worker's set stays open
/// and offered after the owner's Accept of the lead (LAND-PROTO r1 L1).
#[test]
fn an_unrelated_empty_file_on_the_lead_line_does_not_hold_a_worker_s_empty_file() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "empty.txt", "", "Add a marker");
    let worker_set = team.turn_end(&fix);
    merge_keeping_nothing(&team, &fix, true);
    team.commit(&team.lead, "__init__.py", "", "Own empty file");
    let lead_set = team.turn_end(&team.lead);
    landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &team.view_head(&lead_set),
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap();
    assert_eq!(team.state(&lead_set).0, "accepted");
    assert_eq!(team.state(&worker_set), ("pending".to_owned(), None));
    assert!(team.owner_listed().contains(&worker_set));
}

/// While Lands is Lead, the owner's Changes list leaves out a lead's workers' sets, since
/// the lead merges them; with Lands set to Owner it shows them (fix/hand-merged-set
/// 8f98740a).
#[test]
fn owner_list_leaves_out_lead_workers() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    assert_eq!(team.owner_listed(), vec![lead_set.clone()]);
    assert_eq!(landing::list(&team.db, &team.id).unwrap().len(), 2);
    team.lands("owner");
    let mut both = vec![lead_set, worker_set];
    both.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(team.owner_listed(), both);
}

/// A lead's discarded set discards its workers' pending sets while the lead merges them
/// (Lands = Lead), since no one else would; with Lands = Owner they stay for the owner
/// (fix/changes-vs-owner-branch, DEC-107 orphans).
#[test]
fn a_lead_discard_discards_its_workers_sets_only_while_the_lead_merges_them() {
    for (lands, kept) in [("lead", "discarded"), ("owner", "pending")] {
        let team = Team::new();
        team.lands(lands);
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        landing::discard(&team.db, &lead_set, &team.view_head(&lead_set), now_ms()).unwrap();
        assert_eq!(team.state(&lead_set).0, "discarded");
        assert_eq!(team.state(&worker_set).0, kept, "Lands = {lands}");
    }
}

/// A lead's accepted set accepts each worker set its head holds and discards the others
/// while Lands is Lead (fix/changes-vs-owner-branch, DEC-107 orphans).
#[test]
fn a_lead_accept_accepts_covered_worker_sets_and_discards_the_rest() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    let docs = team.worker(3, &team.lead, "docs");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    team.commit(&docs, "docs.txt", "documented\n", "Document it");
    let (fix_set, docs_set) = (team.turn_end(&fix), team.turn_end(&docs));
    team.hand_merge(&team.lead, &fix);
    let lead_set = team.turn_end(&team.lead);
    let AcceptOutcome::Landed { commit, .. } = landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &team.view_head(&lead_set),
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the lead's accept conflicted");
    };
    assert_eq!(team.state(&fix_set), ("accepted".to_owned(), Some(commit)));
    assert_eq!(team.state(&docs_set).0, "discarded");
}

/// Workers' workers close with their lead too, three levels below it, the most the engine
/// follows (fix/changes-vs-owner-branch, DEC-107 orphans).
#[test]
fn worker_sets_close_with_their_lead_three_levels_down() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    let deep = team.worker(3, &fix, "deep");
    let deeper = team.worker(4, &deep, "deeper");
    assert_eq!(deeper.branch, "crazyagent/lead--fix--deep--deeper");
    let mut sets = Vec::new();
    for (agent, file) in [
        (&fix, "fix.txt"),
        (&deep, "deep.txt"),
        (&deeper, "deeper.txt"),
    ] {
        team.commit(agent, file, "work\n", "Work");
        sets.push(team.turn_end(agent));
    }
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    landing::discard(&team.db, &lead_set, &team.view_head(&lead_set), now_ms()).unwrap();
    let states: Vec<String> = sets.iter().map(|set| team.state(set).0).collect();
    assert_eq!(states, ["discarded", "discarded", "discarded"]);
}

/// A worker set left open after its lead's set closed stays open at every later read when
/// the owner's Lands setting then moves to Lead, and the owner's list offers it, since
/// nobody pressed a discard; a set opened after the lead's closed stays out of that list,
/// as the lead may still merge it (fix/v21-team-4 F2, was DEC-107 orphans).
#[test]
fn a_worker_set_left_open_by_its_closed_lead_stays_open_and_listed_but_a_newer_one_is_not() {
    for accepted in [false, true] {
        let team = Team::new();
        team.lands("owner");
        let fix = team.worker(2, &team.lead, "fix");
        let docs = team.worker(3, &team.lead, "docs");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let left = team.turn_end(&fix);
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        let head = team.view_head(&lead_set);
        if accepted {
            landing::accept_into(
                &team.db,
                &team.home,
                &lead_set,
                &head,
                None,
                "main",
                crazyagent::activity::AcceptPlace::Changes,
            )
            .unwrap();
        } else {
            landing::discard(&team.db, &lead_set, &head, now_ms()).unwrap();
        }
        assert_eq!(team.state(&left).0, "pending", "accepted {accepted}");
        std::thread::sleep(std::time::Duration::from_millis(5));
        team.commit(&docs, "docs.txt", "documented\n", "Document it");
        let newer = team.turn_end(&docs);
        team.lands("lead");
        for _ in 0..2 {
            landing::observe(&team.db, Some(&team.id)).unwrap();
        }
        assert_eq!(team.state(&left).0, "pending", "accepted {accepted}");
        assert_eq!(team.state(&newer).0, "pending", "accepted {accepted}");
        let listed = team.owner_listed();
        assert!(listed.contains(&left), "accepted {accepted}");
        assert!(!listed.contains(&newer), "accepted {accepted}");
    }
}

/// A worker set whose work its closed lead holds, left open because a live close skipped
/// it, lands with the lead's commit at the next read, and a deeper worker's set with work
/// nothing holds stays open and is offered to the owner (fix/v21-team-4 F2, r2).
#[test]
fn a_worker_set_its_closed_lead_holds_lands_at_the_next_read() {
    let team = Team::new();
    team.lands("lead");
    let fix = team.worker(2, &team.lead, "fix");
    let deep = team.worker(3, &fix, "deep");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&deep, "deep.txt", "deep\n", "Deep work");
    let deep_set = team.turn_end(&deep);
    team.hand_merge(&team.lead, &fix);
    let lead_set = team.turn_end(&team.lead);
    let step = serde_json::json!({ "step": "reset", "target": team.view_head(&worker_set) });
    team.db
        .execute(
            "UPDATE landing_change_sets SET intent=?1 WHERE id=?2",
            params![step.to_string(), worker_set.as_str()],
        )
        .unwrap();
    let head = team.view_head(&lead_set);
    let AcceptOutcome::Landed { commit, .. } = landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &head,
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the lead's accept conflicted");
    };
    assert_eq!(team.state(&worker_set).0, "pending");
    team.db
        .execute(
            "UPDATE landing_change_sets SET intent=NULL WHERE id=?1",
            [worker_set.as_str()],
        )
        .unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(
        team.state(&worker_set),
        ("accepted".to_owned(), Some(commit))
    );
    assert_eq!(team.pending_ref(&worker_set), "");
    assert_eq!(team.state(&deep_set), ("pending".to_owned(), None));
    assert!(team.owner_listed().contains(&deep_set));
}

/// An owner's Accept stopped after its branch moved and finished at the next start is not
/// a live press: a worker set opened before it finished stays open and is offered to the
/// owner (fix/v21-team-4 r2, sec F1).
#[test]
fn a_land_finished_after_a_restart_leaves_a_worker_set_open_and_offered() {
    let team = Team::new();
    team.lands("lead");
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    let old = team.git(&team.project, &["rev-parse", "refs/heads/main"]);
    let head = team.view_head(&lead_set);
    let AcceptOutcome::Landed { commit, .. } = landing::accept_into(
        &team.db,
        &team.home,
        &lead_set,
        &head,
        None,
        "main",
        crazyagent::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the lead's accept conflicted");
    };
    let step = serde_json::json!({
        "step": "land",
        "reference": "refs/heads/main",
        "old": old,
        "new": commit,
    });
    team.db
        .execute(
            "UPDATE landing_change_sets
             SET state='pending',landed=NULL,landed_on=NULL,made=0,intent=?1 WHERE id=?2",
            params![step.to_string(), lead_set.as_str()],
        )
        .unwrap();
    let pending = format!("refs/crazyagent/pending/{}", lead_set.as_str());
    team.git(&team.project, &["update-ref", &pending, &head]);
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let fix_set = team.turn_end(&fix);
    landing::reconcile(&team.db).unwrap();
    assert_eq!(team.state(&lead_set), ("accepted".to_owned(), Some(commit)));
    assert_eq!(team.state(&fix_set), ("pending".to_owned(), None));
    assert!(team.owner_listed().contains(&fix_set));
}

/// A Discard of a lead's set by anyone but the owner leaves its worker's open set open and
/// offered, since only the owner's own press discards the rest (fix/v21-team-4 r2, sec F1).
#[test]
fn a_discard_by_anyone_but_the_owner_leaves_a_worker_set_open_and_offered() {
    let team = Team::new();
    team.lands("lead");
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let fix_set = team.turn_end(&fix);
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    let head = team.view_head(&lead_set);
    let by = crazyagent::activity::MASTER_ACTOR;
    landing::discard_as(&team.db, &lead_set, &head, now_ms(), by).unwrap();
    assert_eq!(team.state(&fix_set), ("pending".to_owned(), None));
    assert!(team.owner_listed().contains(&fix_set));
}

/// Merges a worker's branch into its lead's clone and keeps none of its work, as a lead
/// can by hand: with `git merge -s ours`, or with a real merge reverted after. Takes the
/// team, the worker and which of the two.
fn merge_keeping_nothing(team: &Team, worker: &Agent, ours: bool) {
    let lead = &team.lead.worktree;
    if ours {
        let from = worker.worktree.to_str().unwrap();
        team.git(lead, &["fetch", "-q", from, &worker.branch]);
        let merge = [
            "merge",
            "--no-ff",
            "-q",
            "--no-edit",
            "-s",
            "ours",
            "FETCH_HEAD",
        ];
        team.git(lead, &merge);
    } else {
        team.hand_merge(&team.lead, worker);
        team.git(lead, &["revert", "-m", "1", "--no-edit", "HEAD"]);
    }
}

/// A lead's hand merge that kept none of a worker's work holds its head without the work,
/// so the worker's set stays open at the end of the lead's turn (fix/v21-team-4 M1).
#[test]
fn a_lead_merge_that_kept_no_work_leaves_the_set_open_at_the_lead_turn_end() {
    for ours in [true, false] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let set = team.turn_end(&fix);
        merge_keeping_nothing(&team, &fix, ours);
        landing::settle_sub_agents(&team.db, &team.id, 1).unwrap();
        assert_eq!(
            team.state(&set),
            ("pending".to_owned(), None),
            "ours {ours}"
        );
        assert_ne!(team.pending_ref(&set), "", "ours {ours}");
    }
}

/// While Lands is Lead a worker's set is judged against its lead's tip: a tip that holds
/// the head without the work does not land it at the next read (fix/v21-team-4 M1).
#[test]
fn a_lead_merge_that_kept_no_work_leaves_the_set_open_at_the_next_read() {
    for ours in [true, false] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let set = team.turn_end(&fix);
        merge_keeping_nothing(&team, &fix, ours);
        landing::observe(&team.db, Some(&team.id)).unwrap();
        assert_eq!(
            team.state(&set),
            ("pending".to_owned(), None),
            "ours {ours}"
        );
        assert_ne!(team.pending_ref(&set), "", "ours {ours}");
    }
}

/// The owner's Accept of a lead that holds a worker's head without its work neither
/// accepts nor discards the worker's set: it stays open, the owner's list offers it once
/// the lead closed, and the owner can land the work (fix/v21-team-4 M1).
#[test]
fn a_lead_accept_leaves_open_and_offers_a_worker_set_it_merged_without_the_work() {
    for ours in [true, false] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        merge_keeping_nothing(&team, &fix, ours);
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        let head = team.view_head(&lead_set);
        landing::accept_into(
            &team.db,
            &team.home,
            &lead_set,
            &head,
            None,
            "main",
            crazyagent::activity::AcceptPlace::Changes,
        )
        .unwrap();
        assert_eq!(team.state(&lead_set).0, "accepted", "ours {ours}");
        assert_eq!(team.state(&worker_set).0, "pending", "ours {ours}");
        assert!(team.owner_listed().contains(&worker_set), "ours {ours}");
        let head = team.view_head(&worker_set);
        let landed = landing::accept_into(
            &team.db,
            &team.home,
            &worker_set,
            &head,
            None,
            "main",
            crazyagent::activity::AcceptPlace::Changes,
        );
        assert!(
            matches!(landed, Ok(AcceptOutcome::Landed { .. })),
            "ours {ours}: {landed:?}"
        );
        assert_eq!(team.git(&team.project, &["show", "main:fix.txt"]), "fixed");
        assert_eq!(team.state(&worker_set).0, "accepted", "ours {ours}");
    }
}

/// A lead's merge tool finds nothing to merge for a branch its clone already holds; when
/// the clone holds it without the work the worker's set stays open and the lead is told
/// the work is absent (fix/v21-team-4 M1, r2 L2).
#[cfg(target_os = "macos")]
#[test]
fn merge_of_a_branch_held_without_its_work_leaves_the_set_open() {
    for ours in [true, false] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let set = team.turn_end(&fix);
        merge_keeping_nothing(&team, &fix, ours);
        let (sandbox, profile) = team.lead_rights();
        let rights = landing::LeadGit {
            sandbox: &sandbox,
            profile: &profile,
        };
        assert_eq!(
            landing::merge_sub_agent(&team.db, &team.id, 1, 2, &rights).unwrap(),
            MergeOutcome::WorkAbsent {
                branch: "crazyagent/lead--fix".into()
            },
            "ours {ours}"
        );
        assert_eq!(
            team.state(&set),
            ("pending".to_owned(), None),
            "ours {ours}"
        );
    }
}

/// A lead's merge in its own clone is refused while a worker below that worker holds an
/// open set, before any git step; a hand merge at the end of the lead's turn closes
/// nothing either. Once the deeper set closed the merge closes the worker's set
/// (fix/v21-team-4, rv-code 15122 M1).
#[cfg(target_os = "macos")]
#[test]
fn merge_in_a_lead_clone_is_refused_while_a_worker_below_holds_an_open_set() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    let deep = team.worker(3, &fix, "deep");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let fix_set = team.turn_end(&fix);
    team.commit(&deep, "deep.txt", "deep\n", "Deep work");
    let deep_set = team.turn_end(&deep);
    let before = team.head(&team.lead);
    let (sandbox, profile) = team.lead_rights();
    let rights = landing::LeadGit {
        sandbox: &sandbox,
        profile: &profile,
    };
    let merge = || landing::merge_sub_agent(&team.db, &team.id, 1, 2, &rights);
    assert_eq!(merge().unwrap_err().0, landing::WORKER_OPEN);
    assert_eq!(team.head(&team.lead), before);
    team.hand_merge(&team.lead, &fix);
    landing::settle_sub_agents(&team.db, &team.id, 1).unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&fix_set).0, "pending");
    assert_eq!(team.state(&deep_set).0, "pending");
    landing::discard(&team.db, &deep_set, &team.view_head(&deep_set), now_ms()).unwrap();
    assert_eq!(
        merge().unwrap(),
        MergeOutcome::AlreadyMerged {
            branch: "crazyagent/lead--fix".into()
        }
    );
    assert_eq!(team.state(&fix_set).0, "accepted");
}

/// The owner's branch takes the lead's README line in a commit of its own and then moves
/// that line on, so a merge of the lead's set conflicts though main holds its work. Takes
/// the team.
fn owner_takes_the_lead_line(team: &Team) {
    for (text, message) in [
        ("one\nlead\nthree\n", "Port the lead's work"),
        ("one\nowner\nthree\n", "Build on the port"),
    ] {
        fs::write(team.project.join("README"), text).unwrap();
        team.git(&team.project, &["add", "README"]);
        team.git(&team.project, &["commit", "-q", "-m", message]);
    }
}

/// A lead's set main took stays open while a worker main never took holds an open set, as
/// a sub-agent or as a native teammate and under either Lands setting: closing the lead
/// would hide that work and let the orphan sweep discard it (HB-S4).
#[test]
fn a_lead_main_took_stays_open_while_a_worker_main_never_took_is_open() {
    for (lands, role) in [
        ("lead", "sub"),
        ("owner", "sub"),
        ("lead", "teammate"),
        ("owner", "teammate"),
    ] {
        let team = Team::new();
        team.lands(lands);
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.db
            .execute("UPDATE team_agents SET role=?1 WHERE id=2", [role])
            .unwrap();
        team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        owner_takes_the_lead_line(&team);
        landing::observe(&team.db, Some(&team.id)).unwrap();
        assert_eq!(team.state(&worker_set).0, "pending", "{lands} {role}");
        assert_eq!(team.state(&lead_set).0, "pending", "{lands} {role}");
        let listed = landing::list(&team.db, &team.id).unwrap();
        assert!(listed.iter().any(|view| view.id == worker_set));
        assert!(team.owner_listed().contains(&lead_set), "{lands} {role}");
    }
}

/// A worker's worker counts: a lead's set main took stays open while a level below its
/// direct worker holds an open set (HB-S4).
#[test]
fn a_lead_main_took_stays_open_while_a_deeper_worker_is_open() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    let deep = team.worker(3, &fix, "deep");
    team.commit(&deep, "deep.txt", "deep\n", "Go deep");
    let deep_set = team.turn_end(&deep);
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    owner_takes_the_lead_line(&team);
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&deep_set).0, "pending");
    assert_eq!(team.state(&lead_set).0, "pending");
}

/// HB-S4-R2-LOWS L1: an open set holds a lead's set main took open down to the third level
/// below the lead, the most the engine follows, and one at the fourth level does not. The
/// fourth level is a clone cut under the second whose team row names the third as its lead.
#[test]
fn a_lead_main_took_stays_open_for_a_worker_three_levels_down_and_not_four() {
    for (level, lead_state) in [(3, "pending"), (4, "discarded")] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        let deep = team.worker(3, &fix, "deep");
        let deeper = team.worker(4, &deep, "deeper");
        let worker = if level == 3 {
            deeper
        } else {
            let deepest = team.worker(5, &deep, "deepest");
            team.db
                .execute("UPDATE team_agents SET parent=4 WHERE id=5", [])
                .unwrap();
            deepest
        };
        team.commit(&worker, "deep.txt", "deep\n", "Go deep");
        let worker_set = team.turn_end(&worker);
        team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        owner_takes_the_lead_line(&team);
        landing::observe(&team.db, Some(&team.id)).unwrap();
        assert_eq!(
            team.state(&lead_set).0,
            lead_state,
            "a worker at level {level}"
        );
        assert_eq!(team.state(&worker_set).0, "pending", "level {level}");
    }
}

/// HB-S4-R2-LOWS L2: when a later step of closing a lead's set main took fails, here the
/// drop of its pending ref behind a held ref lock, the set still reads closed and the
/// project's activity still says once that the engine closed it.
#[test]
fn a_lead_main_took_closed_with_a_failed_later_step_still_says_so_in_activity() {
    let team = Team::new();
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    owner_takes_the_lead_line(&team);
    let lock = team
        .project
        .join(".git/refs/crazyagent/pending")
        .join(format!("{}.lock", lead_set.as_str()));
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "").unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "discarded");
    assert_ne!(
        team.pending_ref(&lead_set),
        "",
        "the step after the stored close failed"
    );
    let noted = crazyagent::activity::list(&team.db, &team.id, crazyagent::activity::LIST_MAX)
        .unwrap()
        .into_iter()
        .filter(|item| {
            item.kind == crazyagent::activity::ActivityKind::Discarded
                && item.change_set.as_deref() == Some(lead_set.as_str())
                && item.actor == crazyagent::activity::ENGINE_ACTOR
        })
        .count();
    assert_eq!(noted, 1);
    fs::remove_file(&lock).unwrap();
}

/// Once no worker holds an open set the lead's set main took closes at the next read, and
/// Restore gives back that set, its head and its pending ref (HB-S4).
#[test]
fn a_lead_main_took_closes_after_its_workers_and_restore_gives_it_back_whole() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    let head = team.view_head(&lead_set);
    owner_takes_the_lead_line(&team);
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "pending");
    landing::discard(
        &team.db,
        &worker_set,
        &team.view_head(&worker_set),
        now_ms(),
    )
    .unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "discarded");
    assert_eq!(team.pending_ref(&lead_set), "");
    let closed = landing::change_set(&team.db, &lead_set).unwrap().unwrap().1;
    landing::restore(&team.db, &lead_set, closed.restorable_until_ms.unwrap() - 1).unwrap();
    assert_eq!(team.state(&lead_set).0, "pending");
    assert_eq!(team.view_head(&lead_set), head);
    assert_ne!(team.pending_ref(&lead_set), "");
}

/// A real store trigger on this connection that refuses any update closing a change set as
/// discarded, as a store that cannot write would. `DROP TRIGGER refuse_discard` lifts it.
const REFUSE_DISCARD: &str =
    "CREATE TEMP TRIGGER refuse_discard BEFORE UPDATE ON landing_change_sets
     WHEN NEW.state='discarded' BEGIN SELECT RAISE(ABORT, 'refused'); END;";

/// HB-S4-R2-LOWS (fix/v21-activity-save): when the store refuses the close of a lead's set
/// main took, the set stays open and the project's activity says nothing, at that read or
/// the next; once a read closes it, activity says so once. The refusal is a real store
/// trigger on the change set's row.
#[test]
fn a_lead_main_took_whose_stored_close_fails_writes_no_activity_until_it_closes() {
    let team = Team::new();
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    owner_takes_the_lead_line(&team);
    team.db.execute_batch(REFUSE_DISCARD).unwrap();
    let noted = || {
        crazyagent::activity::list(&team.db, &team.id, crazyagent::activity::LIST_MAX)
            .unwrap()
            .into_iter()
            .filter(|item| {
                item.kind == crazyagent::activity::ActivityKind::Discarded
                    && item.change_set.as_deref() == Some(lead_set.as_str())
            })
            .count()
    };
    for read in ["first", "next"] {
        landing::observe(&team.db, Some(&team.id)).unwrap();
        assert_eq!(team.state(&lead_set).0, "pending", "{read} read");
        assert_eq!(
            noted(),
            0,
            "no row for a close the store refused, {read} read"
        );
    }
    team.db
        .execute_batch("DROP TRIGGER refuse_discard")
        .unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "discarded");
    assert_eq!(noted(), 1);
}

/// HB-S4-R2-LOWS (fix/v21-activity-save): when the store refuses the close of a lead's set
/// main took, the set, still open, keeps no discarded ref, which prune would never drop
/// since it reads discarded sets only, and keeps its pending ref; once a read closes it, the
/// discarded ref holds its head for a restore.
#[test]
fn a_lead_main_took_whose_stored_close_fails_keeps_no_discarded_ref() {
    let team = Team::new();
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    owner_takes_the_lead_line(&team);
    team.db.execute_batch(REFUSE_DISCARD).unwrap();
    let discarded = format!("refs/crazyagent/discarded/{}", lead_set.as_str());
    let held = || {
        team.git(
            &team.project,
            &["for-each-ref", "--format=%(objectname)", &discarded],
        )
    };
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "pending");
    assert_eq!(held(), "", "no discarded ref on a set the store kept open");
    assert_ne!(team.pending_ref(&lead_set), "");
    team.db
        .execute_batch("DROP TRIGGER refuse_discard")
        .unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "discarded");
    assert_eq!(held(), team.view_head(&lead_set));
}

/// The owner's branch holds a lead's README line outside the app and moves no further, so
/// the next read judges the lead's set landed: by a merge of its head or by a port of the
/// same line in a commit of its own. Takes the team, the lead's set and which of the two.
fn owner_lands_the_lead_line(team: &Team, lead_set: &ChangeSetId, by_merge: bool) {
    if by_merge {
        let head = team.view_head(lead_set);
        team.git(
            &team.project,
            &["merge", "--no-ff", "-q", "--no-edit", &head],
        );
        return;
    }
    fs::write(team.project.join("README"), "one\nlead\nthree\n").unwrap();
    team.git(&team.project, &["add", "README"]);
    team.git(
        &team.project,
        &["commit", "-q", "-m", "Port the lead's work"],
    );
}

/// A lead's set main landed outside the app stays open and listed while a worker it does
/// not hold has an open set, as a sub-agent or as a native teammate, under either Lands
/// setting, by a merge or by a port: a close the owner did not press never discards a
/// worker's open set (HB-S4-LANDED).
#[test]
fn a_lead_main_landed_stays_open_while_a_worker_it_does_not_hold_is_open() {
    for (lands, role) in [
        ("lead", "sub"),
        ("owner", "sub"),
        ("lead", "teammate"),
        ("owner", "teammate"),
    ] {
        for by_merge in [true, false] {
            let team = Team::new();
            team.lands(lands);
            let fix = team.worker(2, &team.lead, "fix");
            team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
            let worker_set = team.turn_end(&fix);
            team.db
                .execute("UPDATE team_agents SET role=?1 WHERE id=2", [role])
                .unwrap();
            team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
            let lead_set = team.turn_end(&team.lead);
            owner_lands_the_lead_line(&team, &lead_set, by_merge);
            landing::observe(&team.db, Some(&team.id)).unwrap();
            let shape = format!("{lands} {role} by_merge={by_merge}");
            assert_eq!(team.state(&worker_set).0, "pending", "{shape}");
            assert_eq!(team.state(&lead_set).0, "pending", "{shape}");
            let listed = landing::list(&team.db, &team.id).unwrap();
            assert!(listed.iter().any(|view| view.id == worker_set), "{shape}");
            assert!(team.owner_listed().contains(&lead_set), "{shape}");
        }
    }
}

/// Once no worker holds an open set, the lead's set main landed outside the app closes as
/// landed at the next read, on the commit that holds it (HB-S4-LANDED).
#[test]
fn a_lead_main_landed_closes_as_landed_once_its_workers_end() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    owner_lands_the_lead_line(&team, &lead_set, true);
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&lead_set).0, "pending");
    landing::discard(
        &team.db,
        &worker_set,
        &team.view_head(&worker_set),
        now_ms(),
    )
    .unwrap();
    landing::observe(&team.db, Some(&team.id)).unwrap();
    let tip = team.git(&team.project, &["rev-parse", "HEAD"]);
    assert_eq!(team.state(&lead_set), ("accepted".to_owned(), Some(tip)));
}

/// A worker set the lead's head holds lands by itself, so a lead main landed outside the
/// app does not wait on it past the next read (HB-S4-LANDED).
#[test]
fn a_lead_main_landed_closes_with_a_worker_set_its_head_holds() {
    for lands in ["lead", "owner"] {
        let team = Team::new();
        team.lands(lands);
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.hand_merge(&team.lead, &fix);
        team.commit(&team.lead, "README", "one\nlead\nthree\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        owner_lands_the_lead_line(&team, &lead_set, true);
        for _ in 0..2 {
            landing::observe(&team.db, Some(&team.id)).unwrap();
        }
        assert_eq!(team.state(&worker_set).0, "accepted", "{lands}");
        assert_eq!(team.state(&lead_set).0, "accepted", "{lands}");
    }
}

/// A worker set the owner restored after its lead's Discard closed it stays open: the next
/// read's orphan sweep does not discard it again (HB-S4-LANDED).
#[test]
fn a_worker_set_the_owner_restored_survives_the_next_read() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    landing::discard(&team.db, &lead_set, &team.view_head(&lead_set), now_ms()).unwrap();
    assert_eq!(team.state(&worker_set).0, "discarded");
    let closed = landing::change_set(&team.db, &worker_set)
        .unwrap()
        .unwrap()
        .1;
    landing::restore(
        &team.db,
        &worker_set,
        closed.restorable_until_ms.unwrap() - 1,
    )
    .unwrap();
    assert_eq!(team.state(&worker_set).0, "pending");
    landing::observe(&team.db, Some(&team.id)).unwrap();
    assert_eq!(team.state(&worker_set).0, "pending");
    let listed = landing::list(&team.db, &team.id).unwrap();
    assert!(listed.iter().any(|view| view.id == worker_set));
}

/// A lead's worktree removed outside the app closes only the lead's set: its workers' open
/// sets stay open at every later read and on the owner's list under either Lands setting,
/// and the owner can still close them, since no one pressed anything in the app
/// (HB-S4-LANDED).
#[test]
fn a_lead_removed_outside_the_app_leaves_its_workers_sets_open_and_listed() {
    for (lands, role) in [
        ("lead", "sub"),
        ("owner", "sub"),
        ("lead", "teammate"),
        ("owner", "teammate"),
    ] {
        let team = Team::new();
        team.lands(lands);
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.db
            .execute("UPDATE team_agents SET role=?1 WHERE id=2", [role])
            .unwrap();
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        fs::remove_dir_all(&team.lead.worktree).unwrap();
        for _ in 0..3 {
            landing::observe(&team.db, Some(&team.id)).unwrap();
        }
        let shape = format!("{lands} {role}");
        assert_eq!(team.state(&lead_set).0, "discarded", "{shape}");
        assert_eq!(team.state(&worker_set).0, "pending", "{shape}");
        assert!(team.owner_listed().contains(&worker_set), "{shape}");
        let head = team.view_head(&worker_set);
        landing::discard(&team.db, &worker_set, &head, now_ms()).unwrap();
        assert_eq!(team.state(&worker_set).0, "discarded", "{shape}");
    }
}

/// A land the owner did not press is refused while a worker below the set's agent holds
/// an open set, for the master and for the engine alike: nothing lands, nothing closes,
/// and a refusal leaves both sets as they were (HB-S4-LANDED r2).
#[test]
fn a_land_the_owner_did_not_press_is_refused_while_a_worker_set_is_open() {
    use crazyagent::activity::{ENGINE_ACTOR, MASTER_ACTOR};
    for by in [MASTER_ACTOR, ENGINE_ACTOR] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        let start = team.git(&team.project, &["rev-parse", "main"]);
        let refused = landing::accept_into_as(
            &team.db,
            &team.home,
            &lead_set,
            &team.view_head(&lead_set),
            None,
            "main",
            by,
        )
        .unwrap_err();
        assert_eq!(refused.0, landing::WORKER_OPEN, "{by}");
        assert_eq!(team.git(&team.project, &["rev-parse", "main"]), start);
        assert_eq!(team.state(&lead_set).0, "pending", "{by}");
        assert_eq!(team.state(&worker_set).0, "pending", "{by}");
    }
}

/// TEAM-3-R2-LOWS F3: a worker set held in a conflict or in an update holds back a land the
/// owner did not press as a pending one does, since each is work the owner has not seen
/// land. The state is written as a refused Accept or a stopped update leaves it.
#[test]
fn a_land_the_owner_did_not_press_is_refused_while_a_worker_set_conflicts_or_updates() {
    for held in ["conflict", "updating"] {
        let team = Team::new();
        let fix = team.worker(2, &team.lead, "fix");
        team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
        let worker_set = team.turn_end(&fix);
        team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
        let lead_set = team.turn_end(&team.lead);
        team.db
            .execute(
                "UPDATE landing_change_sets SET state=?1 WHERE id=?2",
                params![held, worker_set.as_str()],
            )
            .unwrap();
        let start = team.git(&team.project, &["rev-parse", "main"]);
        let refused = landing::accept_into_as(
            &team.db,
            &team.home,
            &lead_set,
            &team.view_head(&lead_set),
            None,
            "main",
            crazyagent::activity::ENGINE_ACTOR,
        )
        .unwrap_err();
        assert_eq!(refused.0, landing::WORKER_OPEN, "{held}");
        assert_eq!(team.git(&team.project, &["rev-parse", "main"]), start);
        assert_eq!(team.state(&lead_set).0, "pending", "{held}");
        assert_eq!(team.state(&worker_set).0, held);
    }
}
