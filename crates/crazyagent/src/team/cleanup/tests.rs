use super::*;
use crate::cleanup_settings::{CleanupMode, CleanupSettings};
use crate::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
use std::process::{Command, Stdio};

const NOW: i64 = 1_800_000_000_000;

/// One project repository with a state folder, an owner home with no global git config,
/// and an engine root whose database holds the team and change set stores.
struct Case {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    team: PathBuf,
    db: Connection,
    project: ProjectId,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: standard output.
fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        std::fs::create_dir(repo.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        git_in(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git_in(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git_in(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        write(&repo.join("README"), b"one\n");
        write(&repo.join(".gitignore"), b"/.crazyagent/\ntarget/\n");
        git_in(&home, &repo, &["add", "README", ".gitignore"]);
        git_in(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let db = crate::landing::open(&engine).unwrap();
        super::super::initialize(&db).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, "project-1", &repo);
        Self {
            _tmp: tmp,
            home,
            repo,
            team: engine.join("team"),
            db,
            project: ProjectId::new("project-1").unwrap(),
        }
    }

    fn worktree(&self, id: i64) -> PathBuf {
        crate::projects_home::state_folder(&self.repo)
            .join("worktrees")
            .join(format!("agent-{id}"))
    }

    fn git_dir(&self, id: i64) -> PathBuf {
        self.repo.join(".git/worktrees").join(format!("agent-{id}"))
    }

    fn folder(&self, id: i64) -> PathBuf {
        self.team.join("agents").join(format!("agent-{id}"))
    }

    fn profile(&self, id: i64) -> PathBuf {
        self.team.join("profiles").join(format!("agent-{id}.sb"))
    }

    /// Adds one team agent as the runtime does: its worktree under the project's state
    /// folder with its start recorded, its agent folder and sandbox profile, and its rows.
    /// Inputs: the id, its role and its status.
    fn agent(&self, id: i64, role: &str, status: &str) {
        let worktree = self.worktree(id);
        git_in(
            &self.home,
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        self.rows(id, role, status, &self.git_dir(id));
    }

    /// Adds one Own branch team agent as the runtime does: its own clone under the
    /// project's state folder with its start recorded, its folders and its rows.
    /// Inputs: the id and its status.
    fn clone_agent(&self, id: i64, status: &str) {
        let worktree = self.worktree(id);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        crate::landing::add_agent_clone(&self.repo, &worktree).unwrap();
        self.rows(id, "sub", status, &crate::agent_git::git_dir(&worktree));
    }

    /// Records an agent whose worktree exists: its start, its agent folder and sandbox
    /// profile, and its team rows. Inputs: the id, its role, its status and git folder.
    fn rows(&self, id: i64, role: &str, status: &str, git_dir: &Path) {
        let worktree = self.worktree(id);
        crate::landing::record_worktree_start(&self.db, &self.repo, &worktree, git_dir).unwrap();
        write(
            &self.folder(id).join("home/projects/x/session.jsonl"),
            b"{}\n",
        );
        std::fs::create_dir_all(self.folder(id).join("tmp")).unwrap();
        write(&self.profile(id), b"(version 1)\n");
        let stopped = matches!(status, "stopped" | "failed").then_some(NOW - 1000);
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms,stopped_ms)
                 VALUES (?1,?2,?3,'claude-code',0,'claude-main',?4,?5,?5,?6)",
                params![id, self.project.as_str(), role, status, NOW - 5000, stopped],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    self.project.as_str(),
                    self.repo.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
    }

    /// Points an agent's worktree record somewhere else, as a database row an older engine
    /// or a tamper wrote would, with that worktree's start recorded so nothing but where it
    /// sits keeps it. Inputs: the agent, the repository, the worktree and its git folder.
    fn repoint(&self, id: i64, repository: &Path, worktree: &Path, git_dir: &Path) {
        crate::landing::record_worktree_start(&self.db, repository, worktree, git_dir).unwrap();
        let changed = self
            .db
            .execute(
                "UPDATE team_worktrees SET repository=?2, worktree=?3, git_dir=?4 WHERE agent=?1",
                params![
                    id,
                    repository.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    fn in_agent(&self, id: i64, args: &[&str]) -> String {
        git_in(&self.home, &self.worktree(id), args)
    }

    /// Commits a new file in an agent's worktree as the agent does. Inputs: the agent and
    /// the file name.
    fn agent_commit(&self, id: i64, name: &str) {
        write(&self.worktree(id).join(name), name.as_bytes());
        self.in_agent(id, &["add", "-A"]);
        self.in_agent(
            id,
            &[
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-q",
                "-m",
                name,
            ],
        );
    }

    fn recorded_git_dir(&self, id: i64) -> PathBuf {
        self.db
            .query_row(
                "SELECT git_dir FROM team_worktrees WHERE agent=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .into()
    }

    /// Lands an agent's change set the way the owner's Accept does. Input: the set.
    fn accept(&self, id: &crate::landing::ChangeSetId) {
        let head = self.head_of(id);
        let outcome = crate::landing::accept(&self.db, &self.home, id, &head, None).unwrap();
        assert!(matches!(
            outcome,
            crate::landing::AcceptOutcome::Landed { .. }
        ));
    }

    fn base_known(&self, id: i64) -> bool {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM landing_bases WHERE worktree=?1",
                [self.worktree(id).to_str().unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    /// Ends a turn in an agent's worktree the way the runtime does, opening or adding to
    /// its change set. Input: the agent. Output: the change set.
    fn turn_end(&self, id: i64) -> crate::landing::ChangeSetId {
        let workspace = Workspace {
            project: self.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: id.to_string(),
                label: format!("Agent {id}"),
            },
            repository: self.repo.clone(),
            worktree: self.worktree(id),
            git_dir: self.recorded_git_dir(id),
        };
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("turn-{id}"),
            prompt_line: "Edit the readme".into(),
        };
        crate::landing::record_turn_end(&self.db, &workspace, &turn)
            .unwrap()
            .unwrap()
    }

    fn head_of(&self, id: &crate::landing::ChangeSetId) -> String {
        crate::landing::list(&self.db, &self.project)
            .unwrap()
            .into_iter()
            .find(|view| &view.id == id)
            .unwrap()
            .head
    }

    fn listed(&self) -> Vec<i64> {
        finished(&self.db, &self.project, NOW)
            .unwrap()
            .iter()
            .filter(|agent| agent.kept.is_none())
            .map(|agent| agent.id)
            .collect()
    }

    /// The agents a check keeps, newest first, with the code it keeps each with.
    fn held(&self) -> Vec<(i64, String)> {
        finished(&self.db, &self.project, NOW)
            .unwrap()
            .into_iter()
            .filter_map(|agent| agent.kept.map(|code| (agent.id, code)))
            .collect()
    }

    /// Says whether an agent's history is all still there: its row, its folder with its
    /// harness conversation and its sandbox profile.
    fn history_kept(&self, id: i64) -> bool {
        let row: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM team_agents WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        row == 1
            && self
                .folder(id)
                .join("home/projects/x/session.jsonl")
                .is_file()
            && self.profile(id).is_file()
    }

    fn stopped_at(&self, id: i64, stopped_ms: Option<i64>) {
        let changed = self
            .db
            .execute(
                "UPDATE team_agents SET stopped_ms=?2 WHERE id=?1",
                params![id, stopped_ms],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Records a finished agent with a task waiting for it, which every check keeps before it
    /// reads any worktree, so it needs no git. Input: the id.
    fn waiting_on_a_task(&self, id: i64) {
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms,stopped_ms)
                 VALUES (?1,?2,'sub','claude-code',0,'claude-main','stopped',?3,?3,?3)",
                params![id, self.project.as_str(), NOW - 5000],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    self.project.as_str(),
                    self.repo.to_str().unwrap(),
                    self.worktree(id).to_str().unwrap(),
                    self.git_dir(id).to_str().unwrap()
                ],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms)
                 VALUES (?1,'master','claude-code',0,'Resolve','queued',?2,?3)",
                params![self.project.as_str(), id, NOW],
            )
            .unwrap();
    }

    fn sweep(&self, mode: CleanupMode, days: u16, after: i64, now_ms: i64) -> Swept {
        sweep(&self.db, &CleanupSettings { mode, days }, after, now_ms).unwrap()
    }

    fn known(&self, id: i64) -> bool {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM team_worktrees WHERE agent=?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    fn kept(&self, id: i64) -> bool {
        self.known(id) && self.worktree(id).is_dir() && self.folder(id).is_dir()
    }

    fn remove(&self, agents: &[i64]) -> Removal {
        remove(&self.db, &self.project, agents, NOW).unwrap()
    }

    fn retiring(&self) -> Vec<i64> {
        let mut statement = self
            .db
            .prepare("SELECT agent FROM team_retiring ORDER BY agent")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn removed(agents: &[i64]) -> Removal {
    Removal {
        agents: agents.to_vec(),
        kept: None,
    }
}

fn refused(code: &str) -> Removal {
    Removal::refused(code)
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn swept(removed: &[i64], after: i64) -> Swept {
    Swept {
        removed: removed.to_vec(),
        after,
        failures: Vec::new(),
    }
}

/// DEC-123: a removal takes only the worktree, the code checkout and its build files, and
/// its size is the worktree's alone; the agent's folder with its conversations, its sandbox
/// profile, its rows and its kept conversation stay as history.
#[test]
fn a_clean_finished_agent_is_listed_with_its_worktree_size_and_loses_only_its_worktree() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("target/build.bin"), &[7; 64 * 1024]);
    write(
        &case.folder(1).join("home/transcript.bin"),
        &[7; 512 * 1024],
    );
    case.agent(2, "master", "failed");
    write(&case.worktree(2).join("README"), b"one\ntwo\n");
    let landed = case.turn_end(2);
    let head = case.head_of(&landed);
    let outcome = crate::landing::accept(&case.db, &case.home, &landed, &head, None).unwrap();
    assert!(matches!(
        outcome,
        crate::landing::AcceptOutcome::Landed { .. }
    ));
    case.agent(3, "master", "stopped");
    for id in [1, 3] {
        case.db
            .execute(
                "INSERT INTO team_agent_conversations(agent,project,harness,connection,conversation,record)
                 VALUES (?1,?2,'codex','connection-1','conversation-1','/tmp/record.jsonl')",
                params![id, case.project.as_str()],
            )
            .unwrap();
    }
    let listed = finished(&case.db, &case.project, NOW).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|agent| (agent.id, agent.kept.as_deref()))
            .collect::<Vec<_>>(),
        [(3, Some("cleanup_agent_running")), (2, None), (1, None)]
    );
    assert!(listed[2].bytes >= 64 * 1024 && listed[2].counted_all);
    assert!(
        listed[2].bytes < 512 * 1024,
        "the size counts the worktree alone, never the agent's folder: {}",
        listed[2].bytes
    );
    assert_eq!(listed[1].role, AgentRole::Master);
    assert!(!listed[2].label.is_empty());
    assert_eq!(case.remove(&[1, 2]), removed(&[1, 2]));
    assert_eq!(
        case.remove(&[3]),
        refused("cleanup_agent_running"),
        "the master the runtime starts again keeps its worktree and home"
    );
    assert!(case.kept(3));
    let listing = git_in(&case.home, &case.repo, &["worktree", "list", "--porcelain"]);
    for id in [1, 2] {
        assert!(!case.worktree(id).exists());
        assert!(!case.git_dir(id).exists());
        assert!(!listing.contains(&format!("agent-{id}")));
        assert!(case.history_kept(id), "agent {id} keeps its history");
        assert!(!case.known(id));
    }
    assert!(case.folder(1).join("home/transcript.bin").is_file());
    let conversations: Vec<i64> = case
        .db
        .prepare("SELECT agent FROM team_agent_conversations ORDER BY agent")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        conversations,
        [1, 3],
        "a removed agent keeps its kept conversation"
    );
    assert!(case.retiring().is_empty());
    assert_eq!(
        git_in(&case.home, &case.repo, &["status", "--porcelain"]),
        ""
    );
    assert!(case.listed().is_empty());
}

/// DEC-123: an agent a check keeps stays listed with the code the Clean up button would
/// answer for it, and counts no size, while an agent still working is not listed at all.
#[test]
fn an_agent_a_check_keeps_is_listed_with_the_code_the_button_answers() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("README"), b"edited\n");
    case.agent(2, "sub", "stopped");
    case.agent_commit(2, "unlanded.txt");
    case.agent(3, "sub", "failed");
    write(&case.worktree(3).join("README"), b"pending\n");
    case.turn_end(3);
    case.agent(4, "sub", "stopped");
    case.agent(5, "master", "stopped");
    case.agent(6, "sub", "working");
    let listed = finished(&case.db, &case.project, NOW).unwrap();
    let codes: Vec<(i64, Option<&str>)> = listed
        .iter()
        .map(|agent| (agent.id, agent.kept.as_deref()))
        .collect();
    assert_eq!(
        codes,
        [
            (5, Some("cleanup_agent_running")),
            (4, None),
            (3, Some("cleanup_changes_open")),
            (2, Some("cleanup_commits_unlanded")),
            (1, Some("cleanup_worktree_dirty")),
        ]
    );
    for agent in listed.iter().filter(|agent| agent.kept.is_some()) {
        assert_eq!((agent.bytes, agent.counted_all), (0, true), "{}", agent.id);
    }
    for (id, code) in case.held() {
        assert_eq!(case.remove(&[id]), refused(&code), "agent {id}");
        assert!(case.kept(id), "agent {id}");
    }
}

/// DEC-123: a removed agent keeps its history but can never be woken into a worktree that is
/// gone: its worktree record goes, which every revive reads first, and the master's list of
/// finished agents it may wake no longer offers it.
#[test]
fn a_removed_agent_keeps_its_history_and_is_never_offered_to_wake() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    let wakeable = || -> Vec<i64> {
        crate::team::events::finished_agents(&case.db, &case.project, NOW)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    };
    assert_eq!(wakeable(), [2, 1]);
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(case.history_kept(1) && !case.known(1));
    assert_eq!(wakeable(), [2], "a removed agent is not offered to wake");
    assert!(case.kept(2));
}

/// DEC-123, After N days: the sweep removes the worktree of each finished agent past its wait,
/// an agent with no finish time counting as past it, and keeps the rest, a check keeping one
/// never stopping the others; once the wait is over the next pass takes the one that waited.
#[test]
fn the_sweep_after_n_days_removes_only_agents_past_their_wait() {
    let case = Case::new();
    for id in 1..=4 {
        case.agent(id, "sub", "stopped");
    }
    case.stopped_at(1, Some(NOW - 3 * DAY_MS - 1));
    case.stopped_at(2, Some(NOW - 3 * DAY_MS + 60_000));
    case.stopped_at(3, None);
    case.stopped_at(4, Some(NOW - 10 * DAY_MS));
    write(&case.worktree(4).join("notes.txt"), b"never added\n");
    assert_eq!(
        case.sweep(CleanupMode::AfterDays, 3, 0, NOW),
        swept(&[1, 3], 0)
    );
    for id in [1, 3] {
        assert!(!case.worktree(id).exists() && !case.known(id), "agent {id}");
        assert!(case.history_kept(id), "agent {id} keeps its history");
    }
    assert!(case.kept(2), "agent 2 has not waited 3 days");
    assert!(case.kept(4), "a check keeps agent 4");
    assert_eq!(case.held(), [(4, "cleanup_worktree_dirty".to_owned())]);
    assert_eq!(
        case.sweep(CleanupMode::AfterDays, 3, 0, NOW + DAY_MS),
        swept(&[2], 0)
    );
    assert!(case.kept(4));
}

/// DEC-123, Once its work lands: the sweep removes a finished agent's worktree as soon as
/// nothing in it is unlanded, with no wait, and keeps one whose commits never landed.
#[test]
fn the_sweep_once_landed_removes_a_landed_agent_at_once() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    case.agent_commit(2, "unlanded.txt");
    case.agent(3, "sub", "stopped");
    write(&case.worktree(3).join("README"), b"landed\n");
    let landed = case.turn_end(3);
    case.accept(&landed);
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 30, 0, NOW),
        swept(&[1, 3], 0)
    );
    assert!(case.kept(2));
    assert_eq!(case.held(), [(2, "cleanup_commits_unlanded".to_owned())]);
    assert!(case.history_kept(1) && case.history_kept(3));
}

/// DEC-123, Only when I click Clean up: the sweep removes nothing, however long an agent
/// finished, and the button still does.
#[test]
fn the_sweep_set_to_manual_removes_nothing() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.stopped_at(1, Some(NOW - 400 * DAY_MS));
    case.agent(2, "sub", "stopped");
    case.stopped_at(2, None);
    assert_eq!(case.sweep(CleanupMode::Manual, 1, 0, NOW), swept(&[], 0));
    assert!(case.kept(1) && case.kept(2));
    assert_eq!(case.listed(), [2, 1]);
    assert_eq!(case.remove(&[1, 2]), removed(&[1, 2]));
}

/// A sweep pass removes at most `RECLAIMABLE_MAX` worktrees and says where it stopped, and
/// the next pass, starting there, takes the rest and starts over.
#[test]
fn a_sweep_pass_removes_at_most_its_bound_and_the_next_pass_takes_the_rest() {
    let case = Case::new();
    let last = i64::try_from(RECLAIMABLE_MAX).unwrap() + 1;
    for id in 1..=last {
        case.agent(id, "sub", "stopped");
    }
    let first = case.sweep(CleanupMode::OnceLanded, 1, 0, NOW);
    let bound: Vec<i64> = (1..last).collect();
    assert_eq!(first, swept(&bound, last - 1));
    assert!(case.kept(last));
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, first.after, NOW),
        swept(&[last], 0)
    );
    assert!(!case.worktree(last).exists());
}

/// A sweep pass reads at most `CANDIDATES_MAX` agents, so agents a check keeps fill a pass
/// without starving a newer one: the next pass starts after them and removes it.
#[test]
fn agents_a_check_keeps_never_hold_the_sweep_back_from_a_newer_one() {
    let case = Case::new();
    let held = i64::try_from(CANDIDATES_MAX).unwrap();
    for id in 1..=held {
        case.waiting_on_a_task(id);
    }
    case.agent(held + 1, "sub", "stopped");
    let first = case.sweep(CleanupMode::OnceLanded, 1, 0, NOW);
    assert_eq!(first, swept(&[], held));
    assert!(
        case.kept(held + 1),
        "the newer agent waits for the next pass"
    );
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, first.after, NOW),
        swept(&[held + 1], 0)
    );
    assert!(!case.worktree(held + 1).exists());
}

/// DEC-123: the code graph's copy at the top of a linked worktree is never the agent's work,
/// so the agent is listed and its worktree removed with the copy in it, while any other
/// untracked file still keeps it as dirty with the copy left in place.
#[test]
fn a_graph_copy_never_keeps_a_linked_worktree_but_another_untracked_file_does() {
    let case = Case::new();
    for id in [1, 2] {
        case.agent(id, "sub", "stopped");
        write(
            &case.worktree(id).join("graphify-out/graph.json"),
            b"{\"nodes\": []}\n",
        );
    }
    write(&case.worktree(2).join("notes.txt"), b"never added\n");
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.held(), [(2, "cleanup_worktree_dirty".to_owned())]);
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(!case.worktree(1).exists() && !case.git_dir(1).exists());
    assert!(case.history_kept(1));
    assert_eq!(case.remove(&[2]), refused("cleanup_worktree_dirty"));
    assert!(case.worktree(2).join("graphify-out/graph.json").is_file());
    assert!(case.kept(2));
}

/// DEC-123: the code graph's copy in an agent's own clone is never its work either: the
/// owner repository's git leaves it out of the clone's untracked files and the size check
/// reads only the landed tip's files, so the clone is removed with it.
#[test]
fn a_graph_copy_never_keeps_an_own_clone() {
    let case = Case::new();
    case.clone_agent(1, "stopped");
    write(
        &case.worktree(1).join("graphify-out/graph.json"),
        b"{\"nodes\": []}\n",
    );
    write(
        &case.worktree(1).join("graphify-out/cache/big.bin"),
        &[7; 4096],
    );
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(!case.worktree(1).exists() && !case.base_known(1));
    assert!(case.history_kept(1));
}

/// DEC-123 rv-code L2: when the project tracks its graph folder, a file left inside it is the
/// agent's work like any other untracked file, so the list, the removal and the sweep all
/// keep the agent as dirty, and no list offers an agent a removal then refuses.
#[test]
fn a_file_left_inside_a_tracked_graph_folder_keeps_the_agent_in_the_list_the_removal_and_the_sweep()
{
    let case = Case::new();
    write(
        &case.repo.join("graphify-out/graph.json"),
        b"{\"nodes\": []}\n",
    );
    git_in(&case.home, &case.repo, &["add", "graphify-out/graph.json"]);
    git_in(
        &case.home,
        &case.repo,
        &["commit", "-q", "-m", "Track the graph"],
    );
    for id in [1, 2] {
        case.agent(id, "sub", "stopped");
    }
    write(
        &case.worktree(2).join("graphify-out/extra.json"),
        b"left behind\n",
    );
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.held(), [(2, "cleanup_worktree_dirty".to_owned())]);
    assert_eq!(case.remove(&[2]), refused("cleanup_worktree_dirty"));
    assert!(case.kept(2));
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1], 0)
    );
    assert!(case.worktree(2).join("graphify-out/extra.json").is_file());
    assert!(case.kept(2));
}

/// DEC-123 rv-secreview L1: a worktree record the database points anywhere but straight
/// inside the project's own worktrees folder, or at another repository, is never removed:
/// the list keeps it, a removal refuses it and the sweep leaves it, its folder untouched.
#[test]
fn a_record_pointing_outside_the_projects_worktrees_folder_or_repository_is_never_removed() {
    let case = Case::new();
    for id in [1, 2, 3] {
        case.agent(id, "sub", "stopped");
    }
    let outside = case.repo.parent().unwrap().join("outside").join("agent-2");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    git_in(
        &case.home,
        &case.repo,
        &[
            "worktree",
            "move",
            case.worktree(2).to_str().unwrap(),
            outside.to_str().unwrap(),
        ],
    );
    case.repoint(2, &case.repo, &outside, &case.git_dir(2));
    let other = case.repo.parent().unwrap().join("other");
    std::fs::create_dir(&other).unwrap();
    git_in(
        &case.home,
        &other,
        &["init", "-q", "--template=", "-b", "main"],
    );
    git_in(
        &case.home,
        &other,
        &[
            "-c",
            "user.name=Other Owner",
            "-c",
            "user.email=other@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Start",
        ],
    );
    let foreign = crate::projects_home::state_folder(&other)
        .join("worktrees")
        .join("agent-3");
    git_in(
        &case.home,
        &other,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            foreign.to_str().unwrap(),
            "HEAD",
        ],
    );
    case.repoint(3, &other, &foreign, &other.join(".git/worktrees/agent-3"));
    let invalid = "cleanup_worktree_invalid".to_owned();
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.held(), [(3, invalid.clone()), (2, invalid)]);
    assert_eq!(case.remove(&[2]), refused("cleanup_worktree_invalid"));
    assert_eq!(case.remove(&[3]), refused("cleanup_worktree_invalid"));
    assert_eq!(
        case.sweep(CleanupMode::OnceLanded, 1, 0, NOW),
        swept(&[1], 0)
    );
    assert!(outside.join("README").is_file());
    assert!(foreign.join(".git").is_file());
    assert!(case.known(2) && case.known(3));
    assert!(case.retiring().is_empty());
}

/// The owner's list of finished agents to remove names one whose name a running sub-agent
/// shares by its number only (DEC-111 Q19), since that name reaches the running one; an
/// unshared name shows as the engine names the agent everywhere else.
#[test]
fn a_finished_agent_sharing_a_running_agent_s_name_is_offered_by_number_only() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    case.agent(3, "sub", "working");
    for (id, persona, handle) in [
        (1, "Ada", None),
        (2, "Bo", Some("docs")),
        (3, "ada", Some("charts")),
    ] {
        case.db
            .execute(
                "INSERT INTO team_names(agent,project,persona,handle) VALUES (?1,?2,?3,?4)",
                params![id, case.project.as_str(), persona, handle],
            )
            .unwrap();
    }
    let listed = finished(&case.db, &case.project, NOW).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|agent| (agent.id, agent.label.as_str()))
            .collect::<Vec<_>>(),
        [(2, "Bo (@docs)"), (1, "Sub-agent 1")]
    );
}

#[test]
fn unlanded_or_live_work_keeps_the_agent_and_says_why() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("README"), b"edited\n");
    case.agent(2, "sub", "stopped");
    write(&case.worktree(2).join("notes.txt"), b"new\n");
    case.agent(3, "sub", "stopped");
    write(&case.worktree(3).join("README"), b"committed\n");
    case.in_agent(3, &["commit", "-q", "-am", "Agent commit"]);
    case.agent(4, "sub", "stopped");
    write(&case.worktree(4).join("README"), b"pending\n");
    case.turn_end(4);
    case.agent(5, "sub", "failed");
    write(&case.worktree(5).join("README"), b"discarded\n");
    let discarded = case.turn_end(5);
    let head = case.head_of(&discarded);
    crate::landing::discard(&case.db, &discarded, &head, u64::try_from(NOW).unwrap()).unwrap();
    case.agent(6, "master", "idle");
    case.agent(7, "sub", "stopped");
    case.db
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms)
             VALUES (?1,'master','claude-code',0,'Resolve','queued',7,?2)",
            params![case.project.as_str(), NOW],
        )
        .unwrap();
    case.agent(8, "sub", "working");
    assert!(case.listed().is_empty());
    let refusals = [
        (1, "cleanup_worktree_dirty"),
        (2, "cleanup_worktree_dirty"),
        (3, "cleanup_commits_unlanded"),
        (4, "cleanup_changes_open"),
        (5, "cleanup_changes_restorable"),
        (6, "cleanup_agent_running"),
        (7, "cleanup_agent_has_task"),
        (8, "cleanup_agent_running"),
    ];
    for (id, code) in refusals {
        assert_eq!(case.remove(&[id]), refused(code), "agent {id}");
        assert!(case.kept(id), "agent {id} keeps its worktree and folder");
    }
    assert_eq!(case.in_agent(1, &["status", "--porcelain"]), " M README\n");
}

#[test]
fn a_change_after_the_read_is_caught_inside_the_removal_and_nothing_is_removed() {
    let case = Case::new();
    for id in [1, 2, 3] {
        case.agent(id, "sub", "stopped");
    }
    assert_eq!(case.listed(), [3, 2, 1]);
    write(
        &case.worktree(2).join("late.txt"),
        b"written after the read\n",
    );
    assert_eq!(case.remove(&[1, 2]), refused("cleanup_worktree_dirty"));
    assert!(case.kept(1) && case.kept(2));
    case.db
        .execute("UPDATE team_agents SET status='starting' WHERE id=3", [])
        .unwrap();
    assert_eq!(case.remove(&[1, 3]), refused("cleanup_agent_running"));
    assert!(case.kept(1) && case.kept(3));
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        remove(&case.db, &other, &[1], NOW).unwrap(),
        refused("cleanup_agent_not_found")
    );
    let over = i64::try_from(RECLAIMABLE_MAX).unwrap() + 1;
    for agents in [vec![], vec![1, 1], (1..=over).collect::<Vec<i64>>()] {
        assert_eq!(case.remove(&agents), refused("cleanup_request_invalid"));
    }
    assert!(case.kept(1) && case.kept(2) && case.kept(3));
    assert!(case.retiring().is_empty());
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert_eq!(case.listed(), Vec::<i64>::new());
}

#[test]
fn an_older_master_is_listed_but_the_one_the_runtime_starts_again_is_not() {
    let case = Case::new();
    case.agent(1, "master", "stopped");
    case.agent(2, "sub", "stopped");
    case.agent(3, "master", "failed");
    assert_eq!(case.listed(), [2, 1]);
    assert_eq!(case.remove(&[3]), refused("cleanup_agent_running"));
    case.db
        .execute("UPDATE team_agents SET harness='codex' WHERE id=1", [])
        .unwrap();
    assert_eq!(
        case.listed(),
        [2],
        "the newest ended master of each harness stays"
    );
    assert_eq!(case.remove(&[1]), refused("cleanup_agent_running"));
    assert!(case.kept(1) && case.kept(3));
}

#[cfg(target_os = "macos")]
#[test]
fn a_removal_that_stops_part_way_says_what_it_removed_and_offers_the_rest_again() {
    let case = Case::new();
    for id in [1, 2, 3] {
        case.agent(id, "sub", "stopped");
    }
    let stuck = case.worktree(2).join("target/stuck.bin");
    write(&stuck, b"locked\n");
    let unlocked = Unlocked(stuck.clone());
    assert!(chflags("uchg", &stuck));
    assert_eq!(
        case.remove(&[1, 2, 3]),
        Removal {
            agents: vec![1],
            kept: Some("cleanup_worktree_leftover".into()),
        }
    );
    assert!(!case.worktree(1).exists() && case.history_kept(1));
    assert!(stuck.exists() && !case.git_dir(2).exists());
    assert!(!case.known(2), "no revive finds the removed worktree");
    assert_eq!(
        case.retiring(),
        [2],
        "the agent left part way stays offered"
    );
    assert!(case.kept(3), "the agent not reached keeps its record");
    assert_eq!(case.listed(), [3, 2]);
    assert!(chflags("nouchg", &stuck));
    assert_eq!(case.remove(&[2]), removed(&[2]));
    assert!(!case.worktree(2).exists() && case.history_kept(2));
    assert!(case.retiring().is_empty());
    assert_eq!(case.listed(), [3]);
    drop(unlocked);
}

#[test]
fn a_claim_an_engine_stop_interrupted_is_offered_again_or_handed_back_when_it_changed() {
    let case = Case::new();
    for id in [1, 2] {
        case.agent(id, "sub", "stopped");
    }
    initialize(&case.db).unwrap();
    assert!(
        claim(&case.db, &case.project, &[1, 2], NOW)
            .unwrap()
            .is_ok()
    );
    assert!(!case.known(1) && !case.known(2));
    write(
        &case.worktree(2).join("after.txt"),
        b"written after the stop\n",
    );
    assert_eq!(case.listed(), [1]);
    assert!(case.known(2), "a changed worktree gets its record back");
    assert_eq!(case.retiring(), [1]);
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(!case.worktree(1).exists() && case.history_kept(1));
    assert!(case.retiring().is_empty());
    assert!(case.kept(2));
}

/// A removal leaves the agent's folder alone, read-only folders and links in it included;
/// deleting its project then removes those folders without following a link out.
#[test]
fn read_only_folders_are_removed_without_following_a_link_out() {
    use std::os::unix::fs::PermissionsExt;
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    let outside = case.home.join("outside");
    write(&outside.join("keep.txt"), b"the owner's\n");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = case.folder(1).join("home/cache/module");
    write(&cache.join("lib.go"), b"package lib\n");
    std::os::unix::fs::symlink(&outside, case.folder(1).join("home/cache/link")).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(
        case.folder(1).join("home/cache"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let closed = case.folder(1).join("tmp/closed");
    write(&closed.join("inner/file"), b"x\n");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(
        cache.join("lib.go").is_file(),
        "a removal keeps the agent's folder"
    );
    remove_agent_files(&case.team, 1).unwrap();
    assert!(!case.folder(1).exists());
    let mode = std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "the link's target keeps its mode");
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"the owner's\n"
    );
}

/// A master stored under a harness alias ("claude") is the same harness as one stored under
/// its catalog entry ("claude-code"): only the newest of the two stays for the runtime to
/// start again, as the runtime's own lookup picks it, and the older one is listed and removed.
#[test]
fn an_older_master_under_a_harness_alias_is_listed_and_removed() {
    let case = Case::new();
    case.agent(1, "master", "stopped");
    case.agent(3, "master", "failed");
    case.db
        .execute("UPDATE team_agents SET harness='claude' WHERE id=1", [])
        .unwrap();
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.remove(&[3]), refused("cleanup_agent_running"));
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(!case.known(1) && case.kept(3));
}

#[test]
fn an_agent_git_cannot_read_keeps_its_own_code_and_leaves_the_others_removable() {
    let case = Case::new();
    for id in [1, 2, 3] {
        case.agent(id, "sub", "stopped");
    }
    write(&case.git_dir(2).join("index.lock"), b"");
    std::fs::File::options()
        .write(true)
        .open(case.worktree(2).join("README"))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
        .unwrap();
    assert_eq!(
        case.listed(),
        [3, 1],
        "the agent git stops on is left out, the others stay listed"
    );
    assert_eq!(case.remove(&[1, 2]), refused("cleanup_worktree_unreadable"));
    assert!(case.kept(1) && case.kept(2));
    assert!(case.retiring().is_empty());
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(claim(&case.db, &case.project, &[2], NOW).unwrap().is_ok());
    assert_eq!(case.listed(), [3]);
    assert!(
        case.known(2),
        "a claimed agent git stops on gets its record back"
    );
    assert!(case.retiring().is_empty());
    std::fs::remove_file(case.git_dir(2).join("index.lock")).unwrap();
    assert_eq!(case.listed(), [3, 2]);
    assert_eq!(case.remove(&[2, 3]), removed(&[2, 3]));
    assert!(case.listed().is_empty());
}

#[test]
fn a_worktree_git_removes_only_part_way_is_finished_without_following_a_link_out() {
    use std::os::unix::fs::PermissionsExt;
    let case = Case::new();
    for id in [1, 2] {
        case.agent(id, "sub", "stopped");
    }
    let outside = case.home.join("outside");
    write(&outside.join("keep.txt"), b"the owner's\n");
    let cache = case.worktree(1).join("target/cache/module");
    write(&cache.join("lib.go"), b"package lib\n");
    std::os::unix::fs::symlink(&outside, case.worktree(1).join("target/cache/link")).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(case.listed(), [2, 1]);
    assert_eq!(case.remove(&[1, 2]), removed(&[1, 2]));
    let listing = git_in(&case.home, &case.repo, &["worktree", "list", "--porcelain"]);
    for id in [1, 2] {
        assert!(!case.worktree(id).exists() && !case.git_dir(id).exists());
        assert!(!listing.contains(&format!("agent-{id}")));
        assert!(case.history_kept(id) && !case.known(id));
    }
    let bases: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM landing_bases WHERE worktree=?1",
            [case.worktree(1).to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bases, 0);
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"the owner's\n"
    );
    assert!(case.retiring().is_empty());
    assert!(case.listed().is_empty());
}

/// CEIL-L1: an older-layout project's cleanup of agent 7 is cut off after its claim, so the
/// record waits in `team_retiring` while the worktree and its `gitdir:` `.git` stay in
/// `<project>/worktrees`. Once the project gains a `.crazyagent` folder, the IDE's git
/// ceiling must still keep that older root, as it does for the live record.
#[test]
fn an_agent_whose_cleanup_stopped_after_its_claim_keeps_the_older_root_in_the_git_ceiling() {
    let case = Case::new();
    let engine = case.team.parent().unwrap().to_path_buf();
    let state = case.repo.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    std::fs::remove_dir(&state).unwrap();
    let older = || crate::ide::workspace::older_agents(&engine, &case.project, &case.repo);
    assert!(!older().unwrap(), "no engine-made worktree yet");
    case.agent(7, "sub", "stopped");
    let worktree = case.repo.join("worktrees").join("agent-7");
    assert!(worktree.join(".git").is_file());
    std::fs::create_dir(&state).unwrap();
    assert_eq!(crate::projects_home::state_folder(&case.repo), state);
    assert!(older().unwrap(), "the live record in the older root counts");

    initialize(&case.db).unwrap();
    assert!(claim(&case.db, &case.project, &[7], NOW).unwrap().is_ok());
    assert_eq!(case.retiring(), [7]);
    assert!(!case.known(7));
    assert!(
        worktree.join(".git").is_file(),
        "the claim alone removes nothing"
    );
    assert!(
        older().unwrap(),
        "a record waiting in team_retiring keeps the older root in the ceiling"
    );
}

/// A finished Own branch agent whose commits all landed, or that made none, is listed and
/// removed: the engine decides through its own fetch of the clone's branch, runs no git
/// in the clone and deletes the clone folder, its base record and its folders
/// (CLONE-CLEAN-1).
#[test]
fn a_finished_clone_agent_with_everything_landed_is_removed_without_git_in_its_clone() {
    use std::os::unix::fs::PermissionsExt;
    let case = Case::new();
    case.clone_agent(1, "stopped");
    case.agent_commit(1, "feature.txt");
    let landed = case.turn_end(1);
    case.accept(&landed);
    case.clone_agent(2, "failed");
    let marker = case.home.join("fsmonitor-ran");
    let hook = case.home.join("fsmonitor.sh");
    write(
        &hook,
        format!("#!/bin/sh\necho ran >> '{}'\nexit 1\n", marker.display()).as_bytes(),
    );
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    for id in [1, 2] {
        case.in_agent(id, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    }
    let mut listed = case.listed();
    listed.sort_unstable();
    assert_eq!(listed, [1, 2]);
    let base_refs = || {
        git_in(
            &case.home,
            &case.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/crazyagent/base",
            ],
        )
    };
    assert_eq!(
        base_refs(),
        "refs/crazyagent/base/agent-1\nrefs/crazyagent/base/agent-2\n"
    );
    assert_eq!(case.remove(&[1, 2]), removed(&[1, 2]));
    assert_eq!(base_refs(), "", "each removed clone's base ref is gone");
    for id in [1, 2] {
        assert!(!case.worktree(id).exists(), "agent {id}'s clone is gone");
        assert!(case.history_kept(id), "agent {id} keeps its history");
        assert!(!case.known(id) && !case.base_known(id));
    }
    assert!(!marker.exists(), "git ran in an agent's clone");
    assert_eq!(
        git_in(
            &case.home,
            &case.repo,
            &["for-each-ref", "refs/crazyagent/incoming"]
        ),
        ""
    );
    assert_eq!(
        git_in(&case.home, &case.repo, &["show", "main:feature.txt"]),
        "feature.txt"
    );
    assert!(case.retiring().is_empty());
    assert!(case.listed().is_empty());
}

/// A finished Own branch agent with commits that never landed keeps its clone with the
/// same plain reason as a linked worktree, both before any turn end and after a landed
/// set, and one with a set waiting keeps it for that set (CLONE-CLEAN-1).
#[test]
fn a_finished_clone_agent_with_unlanded_commits_is_kept_with_the_plain_reason() {
    let case = Case::new();
    case.clone_agent(1, "stopped");
    case.agent_commit(1, "never-ended.txt");
    case.clone_agent(2, "stopped");
    case.agent_commit(2, "landed.txt");
    let landed = case.turn_end(2);
    case.accept(&landed);
    case.agent_commit(2, "after.txt");
    case.clone_agent(3, "stopped");
    case.agent_commit(3, "pending.txt");
    case.turn_end(3);
    assert!(case.listed().is_empty());
    for (id, code) in [
        (1, "cleanup_commits_unlanded"),
        (2, "cleanup_commits_unlanded"),
        (3, "cleanup_changes_open"),
    ] {
        assert_eq!(case.remove(&[id]), refused(code), "agent {id}");
        assert!(
            case.kept(id) && case.base_known(id),
            "agent {id} keeps its clone"
        );
    }
    assert!(case.retiring().is_empty());
    assert_eq!(
        case.in_agent(1, &["log", "-1", "--format=%s"]),
        "never-ended.txt\n"
    );
}

/// A removal batch with one clone that holds unlanded commits keeps every agent and hands
/// back every claim, so the others are removed on their own straight after (RV-DS-61
/// rule, CLONE-CLEAN-1).
#[test]
fn a_batch_with_one_unlanded_clone_keeps_it_and_leaves_the_others_removable() {
    let case = Case::new();
    case.clone_agent(1, "stopped");
    case.clone_agent(2, "stopped");
    case.agent_commit(2, "unlanded.txt");
    case.agent(3, "sub", "stopped");
    assert_eq!(case.remove(&[1, 2, 3]), refused("cleanup_commits_unlanded"));
    for id in [1, 2, 3] {
        assert!(case.kept(id), "agent {id} is kept with the batch");
    }
    assert!(case.retiring().is_empty());
    let mut listed = case.listed();
    listed.sort_unstable();
    assert_eq!(listed, [1, 3]);
    assert_eq!(case.remove(&[1, 3]), removed(&[1, 3]));
    assert!(case.kept(2));
    assert!(!case.worktree(1).exists() && !case.worktree(3).exists());
}

/// A finished Own branch agent whose clone holds an uncommitted edit or a new untracked
/// file keeps it with the linked layout's reason, read with the owner repository's git and
/// a private index, never the clone's own git folder, while a clean landed clone with only
/// ignored files is removed and no private index is left behind (Q-W-LAND-20).
#[test]
fn a_finished_clone_agent_with_uncommitted_or_untracked_files_is_kept_as_dirty() {
    let case = Case::new();
    case.clone_agent(1, "stopped");
    write(
        &case.worktree(1).join("README"),
        b"edited, never committed\n",
    );
    case.clone_agent(2, "stopped");
    write(&case.worktree(2).join("notes.txt"), b"never added\n");
    case.clone_agent(3, "stopped");
    case.agent_commit(3, "landed.txt");
    let landed = case.turn_end(3);
    case.accept(&landed);
    write(&case.worktree(3).join("target/build.bin"), &[7; 1024]);
    for id in [1, 2] {
        assert_eq!(
            case.remove(&[id]),
            refused("cleanup_worktree_dirty"),
            "agent {id}"
        );
        assert!(case.kept(id) && case.base_known(id), "agent {id}");
    }
    assert_eq!(case.in_agent(1, &["status", "--porcelain"]), " M README\n");
    assert_eq!(
        case.in_agent(2, &["status", "--porcelain"]),
        "?? notes.txt\n"
    );
    let mut listed = case.listed();
    listed.sort_unstable();
    assert_eq!(listed, [3]);
    assert_eq!(case.remove(&[3]), removed(&[3]));
    assert!(!case.worktree(3).exists() && !case.base_known(3));
    let left: Vec<_> = std::fs::read_dir(case.repo.join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("crazyagent-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A finished Own branch agent whose clone holds a ref besides its branch keeps it, read as
/// data from the clone's git folder with no git run and no link followed: a stash, a side
/// branch with a commit, a tag, a detached HEAD or a reftable store keep it as
/// `cleanup_refs_unlanded` and a ref that is a link as `cleanup_worktree_unreadable`, while
/// a clone whose branch git packed is removed without following a link out of it (CC-1).
#[test]
fn a_finished_clone_agent_with_refs_only_its_clone_holds_is_kept_with_the_plain_reason() {
    let case = Case::new();
    let git_dir = |id| crate::agent_git::git_dir(&case.worktree(id));
    for id in 1..=7 {
        case.clone_agent(id, "stopped");
    }
    write(
        &case.worktree(1).join("README"),
        b"stashed, never committed\n",
    );
    case.in_agent(
        1,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "stash",
            "-q",
        ],
    );
    let branch = crate::agent_git::branch(&case.worktree(2)).unwrap();
    case.in_agent(2, &["checkout", "-q", "-b", "side"]);
    case.agent_commit(2, "side.txt");
    case.in_agent(2, &["checkout", "-q", &branch]);
    case.in_agent(3, &["tag", "v1"]);
    case.in_agent(4, &["checkout", "-q", "--detach"]);
    std::fs::create_dir(git_dir(5).join("reftable")).unwrap();
    let outside_ref = case.home.join("outside-ref");
    write(
        &outside_ref,
        case.in_agent(6, &["rev-parse", "HEAD"]).as_bytes(),
    );
    std::os::unix::fs::symlink(&outside_ref, git_dir(6).join("refs/heads/linked")).unwrap();
    let packed = crate::agent_git::branch(&case.worktree(7)).unwrap();
    case.in_agent(7, &["pack-refs", "--all"]);
    assert!(!git_dir(7).join("refs/heads").join(&packed).exists());
    assert!(
        std::fs::read_to_string(git_dir(7).join("packed-refs"))
            .unwrap()
            .contains(&format!(" refs/heads/{packed}\n"))
    );
    let outside = case.home.join("outside/kept.txt");
    write(&outside, b"not the agent's\n");
    std::os::unix::fs::symlink(outside.parent().unwrap(), git_dir(7).join("outside")).unwrap();
    let listed = case.listed();
    let got: Vec<Removal> = (1..=7).map(|id| case.remove(&[id])).collect();
    let mut expected = vec![refused("cleanup_refs_unlanded"); 5];
    expected.extend([refused("cleanup_worktree_unreadable"), removed(&[7])]);
    assert_eq!(got, expected);
    assert_eq!(listed, [7]);
    for id in 1..=6 {
        assert!(
            case.kept(id) && case.base_known(id),
            "agent {id} keeps its clone"
        );
    }
    assert_eq!(case.in_agent(1, &["stash", "list"]).lines().count(), 1);
    assert_eq!(
        case.in_agent(2, &["log", "-1", "--format=%s", "side"]),
        "side.txt\n"
    );
    assert!(!case.worktree(7).exists() && !case.base_known(7));
    assert_eq!(std::fs::read(&outside).unwrap(), b"not the agent's\n");
    assert!(outside_ref.is_file());
    assert!(case.retiring().is_empty());
}

/// A finished Own branch agent that left a huge sparse file at a tracked path keeps its
/// clone as dirty within seconds: the engine compares each tracked file's size with the
/// landed tip before git hashes any of them under the repository hold, so the file is
/// never read (CCS-1).
#[test]
fn a_clone_with_a_huge_file_at_a_tracked_path_is_kept_as_dirty_without_hashing_it() {
    let case = Case::new();
    case.clone_agent(1, "stopped");
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(case.worktree(1).join("README"))
        .unwrap()
        .set_len(8 << 30)
        .unwrap();
    let started = std::time::Instant::now();
    let kept = case.remove(&[1]);
    let elapsed = started.elapsed();
    assert_eq!(
        (kept, elapsed < std::time::Duration::from_secs(20)),
        (refused("cleanup_worktree_dirty"), true),
        "{elapsed:?}"
    );
    assert!(case.kept(1) && case.base_known(1));
}

/// A removed native lead keeps its own and its teammates' profile and the records of its
/// teammates' socket name and server pid, as history (DEC-123); deleting its project takes
/// them all (rv-audit TTDL F2, PLC c PC-L1), and an agent left in place keeps its own.
#[test]
fn a_removed_native_lead_keeps_its_teammates_records_until_its_project_is_deleted() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    let teammates = |id: i64| {
        [
            crate::team::runtime::agent_teammates_profile(&case.team, id),
            crate::team::runtime::agent_teammates_socket_name(&case.team, id),
            crate::team::runtime::agent_teammates_server_pid(&case.team, id),
        ]
    };
    for id in [1, 2] {
        for file in teammates(id) {
            write(&file, b"written by the engine\n");
        }
    }
    assert_eq!(case.remove(&[1]), removed(&[1]));
    for file in teammates(1).into_iter().chain([case.profile(1)]) {
        assert!(file.exists(), "{} went with the worktree", file.display());
    }
    remove_agent_files(&case.team, 1).unwrap();
    assert!(!case.profile(1).exists() && !case.folder(1).exists());
    for file in teammates(1) {
        assert!(!file.exists(), "{} was left behind", file.display());
    }
    for file in teammates(2).into_iter().chain([case.profile(2)]) {
        assert!(file.exists(), "{} went with another agent", file.display());
    }
}

/// Sets or clears the user immutable flag on one file with the system's `chflags`.
/// Inputs: the flag name and the file. Output: whether `chflags` succeeded.
#[cfg(target_os = "macos")]
fn chflags(flag: &str, file: &Path) -> bool {
    crate::spawn_gate::status(
        Command::new("/usr/bin/chflags")
            .arg(flag)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|done| done.success())
}

/// Clears the user immutable flag on a test file when dropped, so the temporary folder
/// can always be deleted, whatever the test asserted. Holds the file's path.
#[cfg(target_os = "macos")]
struct Unlocked(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Unlocked {
    fn drop(&mut self) {
        let _ = chflags("nouchg", &self.0);
    }
}

/// When git drops an agent's worktree record but leaves a file the engine cannot delete,
/// the agent stays offered for removal with its record waiting, and a later removal
/// finishes once the file can go, instead of refusing it as an invalid worktree for good
/// (RVD-C-N1).
#[cfg(target_os = "macos")]
#[test]
fn a_leftover_the_engine_cannot_delete_keeps_the_agent_offered_and_a_later_removal_finishes_it() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    let stuck = case.worktree(1).join("target/stuck.bin");
    write(&stuck, b"locked\n");
    let unlocked = Unlocked(stuck.clone());
    assert!(chflags("uchg", &stuck));
    assert_eq!(case.listed(), [1]);
    assert_eq!(case.remove(&[1]), refused("cleanup_worktree_leftover"));
    assert!(!case.git_dir(1).exists() && stuck.exists());
    assert_eq!(case.listed(), [1], "the agent stays offered for removal");
    assert_eq!(
        case.retiring(),
        [1],
        "its record waits instead of going back"
    );
    assert_eq!(
        case.remove(&[1]),
        refused("cleanup_worktree_leftover"),
        "the file is still locked"
    );
    assert_eq!(case.retiring(), [1]);
    assert!(chflags("nouchg", &stuck));
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(!case.worktree(1).exists() && case.history_kept(1) && !case.known(1));
    assert!(case.retiring().is_empty() && case.listed().is_empty());
    drop(unlocked);
}

/// What a Delete everything would lose, read before anything stops: uncommitted edits,
/// unlanded commits and an open change set each keep their plain reason, a running agent's
/// edits count too, a clean agent is not listed, and an agent's own clone whose git config
/// names an fsmonitor and a filter never runs either while it is read (DEC-118).
#[test]
fn a_removal_preview_names_each_agent_s_unlanded_work_without_running_git_in_a_clone() {
    use std::os::unix::fs::PermissionsExt;
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("README"), b"edited\n");
    case.agent(2, "sub", "stopped");
    write(&case.worktree(2).join("README"), b"committed\n");
    case.in_agent(2, &["commit", "-q", "-am", "Agent commit"]);
    case.agent(3, "sub", "stopped");
    write(&case.worktree(3).join("README"), b"pending\n");
    case.turn_end(3);
    case.agent(4, "sub", "working");
    write(&case.worktree(4).join("notes.txt"), b"new\n");
    case.agent(5, "sub", "stopped");
    case.clone_agent(6, "stopped");
    let marker = case.home.join("hook-ran");
    let hook = case.home.join("hook.sh");
    write(
        &hook,
        format!("#!/bin/sh\necho ran >> '{}'\nexit 1\n", marker.display()).as_bytes(),
    );
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    case.in_agent(6, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    case.in_agent(6, &["config", "filter.mark.clean", hook.to_str().unwrap()]);
    write(&case.worktree(6).join(".gitattributes"), b"* filter=mark\n");
    write(&case.worktree(6).join("README"), b"edited in the clone\n");

    let losses = removal_loss(&case.db, &case.project, NOW).unwrap();
    let got: Vec<(i64, &str)> = losses
        .iter()
        .map(|loss| (loss.id, loss.code.as_str()))
        .collect();
    assert_eq!(
        got[..got.len().min(4)],
        [
            (1, "cleanup_worktree_dirty"),
            (2, "cleanup_commits_unlanded"),
            (3, "cleanup_changes_open"),
            (4, "cleanup_worktree_dirty"),
        ],
        "{got:?}"
    );
    assert!(
        matches!(
            got.get(4),
            Some((6, "cleanup_worktree_dirty" | "cleanup_worktree_unreadable"))
        ),
        "the clone's edits are named or said to be unreadable: {got:?}"
    );
    assert_eq!(got.len(), 5, "a clean agent loses nothing: {got:?}");
    assert!(losses.iter().all(|loss| !loss.label.is_empty()));
    assert!(
        !marker.exists(),
        "no fsmonitor or filter ran in an agent's clone"
    );
    for id in [1, 2, 3, 4, 6] {
        assert!(case.kept(id), "a preview changes nothing for agent {id}");
    }
}
