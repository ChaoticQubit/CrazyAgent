use super::*;
use crate::agent_git::Layout;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::team::runtime::{TeamLimits, TeamRuntime};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::Arc;

const PROJECT: &str = "spawn-once";

struct Case {
    _root: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    team: Connection,
    tmux: Option<PathBuf>,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-spawn-once-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let team = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&team).unwrap();
        let owner = team.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&team).unwrap();
        crate::project_setup::initialize(&team).unwrap();
        let repo = base.join("project");
        std::fs::create_dir_all(repo.join(".crazyagent/worktrees")).unwrap();
        std::fs::write(repo.join("README.md"), "spawn once\n").unwrap();
        git(&repo, &["init", "-q", "--template=", "-b", "main"]);
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "Start"]);
        Self {
            _root: root,
            base,
            repo,
            team,
            tmux: None,
        }
    }

    fn agent(&self) -> i64 {
        self.team
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'local-a','starting',1,1)",
                [PROJECT],
            )
            .unwrap();
        self.team.last_insert_rowid()
    }

    fn worktree(&self, id: i64) -> PathBuf {
        self.repo
            .join(".crazyagent/worktrees")
            .join(format!("agent-{id}"))
    }

    fn add(&self, id: i64, checkout: bool) {
        let path = self.worktree(id);
        let mut args = vec!["worktree", "add", "-q", "--detach"];
        if !checkout {
            args.push("--no-checkout");
        }
        args.extend([path.to_str().unwrap(), "HEAD"]);
        git(&self.repo, &args);
    }

    fn record(&self, id: i64) {
        let [repository, worktree, git_dir] =
            super::super::agent_git_dir(&self.repo, &self.worktree(id), Layout::Linked).unwrap();
        self.team
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![id, PROJECT, repository, worktree, git_dir],
            )
            .unwrap();
    }

    fn listed(&self, id: i64) -> bool {
        let line = format!("worktree {}\n", self.worktree(id).display());
        git(&self.repo, &["worktree", "list", "--porcelain"]).contains(&line)
    }

    fn entry(&self, id: i64) -> bool {
        self.repo
            .join(".git/worktrees")
            .join(format!("agent-{id}"))
            .exists()
    }

    fn ledger(&self) -> Connection {
        crate::ledger::open(&self.base).unwrap()
    }

    fn outcomes(&self, kind: &str) -> Vec<(String, Option<String>)> {
        let ledger = self.ledger();
        let mut statement = ledger
            .prepare("SELECT action_key,outcome FROM ledger_actions WHERE kind=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn pending(&self, kind: ActionKind) -> usize {
        crate::ledger::pending(&self.ledger(), kind).unwrap().len()
    }

    fn runtime(&self) -> TeamRuntime {
        TeamRuntime::open(
            &self.base.join("engine.db"),
            &self.base.join("team"),
            BTreeMap::new(),
            Box::new(|| None),
            TeamLimits::default(),
        )
        .unwrap()
    }

    fn host(&mut self) -> Arc<dyn Host> {
        let tmux = installed(&self.base).expect("tmux must be installed on this machine");
        self.tmux = Some(tmux.clone());
        Arc::new(Tmux::open(Some(tmux), &self.base.join(FOLDER)))
    }

    fn pane(&self, host: &Arc<dyn Host>, id: i64) {
        let file = self.base.join(format!("session-{id}.jsonl"));
        std::fs::write(&file, "").unwrap();
        let command = InteractiveCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::ClaudeSession { file },
        };
        host.spawn(&AgentId::team(id).unwrap(), &command).unwrap();
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if let Some(tmux) = &self.tmux {
            let _ = crate::spawn_gate::status(
                Command::new(tmux)
                    .arg("-S")
                    .arg(self.base.join(FOLDER).join(SOCKET))
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }
}

/// Runs git as the owner would in a test: no global or system config, in `dir`.
/// Inputs: the folder and the arguments. Output: standard output.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "user.name=Spawn Once",
                "-c",
                "user.email=spawn-once@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
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

fn key(id: i64) -> String {
    format!("team-{id}")
}

fn ended(id: i64, outcome: &str) -> (String, Option<String>) {
    (key(id), Some(outcome.to_owned()))
}

fn shown(id: i64) -> (String, Option<String>) {
    (key(id), None)
}

fn unsettled(refused: Result<(), String>, sentence: &str) {
    let refused = refused.unwrap_err();
    assert!(refused.starts_with(sentence), "{refused}");
    assert!(refused.contains("ledger_pending"), "{refused}");
}

impl Case {
    fn dismiss(&self, kind: ActionKind, id: i64) {
        let ledger = self.ledger();
        let notice = crate::ledger::pending(&ledger, kind)
            .unwrap()
            .into_iter()
            .find(|action| action.key == key(id))
            .unwrap();
        assert!(
            notice.payload.contains("\"unknown\":true"),
            "{}",
            notice.payload
        );
        let shown = crate::ledger::dismiss(&ledger, kind, notice.id.get()).unwrap();
        assert_eq!(shown, Outcome::Unknown);
    }
}

#[test]
fn worktree_create_interrupted_is_completed_or_cleaned_never_duplicated() {
    let case = Case::new();
    let partial = case.agent();
    begin_worktree(&case.base, partial, &case.repo, &case.worktree(partial)).unwrap();
    case.add(partial, false);
    let recorded = case.agent();
    begin_worktree(&case.base, recorded, &case.repo, &case.worktree(recorded)).unwrap();
    case.add(recorded, true);
    case.record(recorded);
    let nothing = case.agent();
    begin_worktree(&case.base, nothing, &case.repo, &case.worktree(nothing)).unwrap();
    let lost = case.agent();
    begin_worktree(&case.base, lost, &case.repo, &case.worktree(lost)).unwrap();
    case.add(lost, false);
    std::fs::remove_dir_all(case.worktree(lost)).unwrap();
    let foreign = case.agent();
    begin_worktree(&case.base, foreign, &case.repo, &case.worktree(foreign)).unwrap();
    std::fs::create_dir(case.worktree(foreign)).unwrap();
    std::fs::write(case.worktree(foreign).join("notes.txt"), "kept\n").unwrap();
    unsettled(
        begin_worktree(&case.base, partial, &case.repo, &case.worktree(partial)),
        "An earlier add of this agent's worktree is still being checked",
    );
    assert_eq!(case.pending(ActionKind::WorktreeCreate), 5);

    drop(case.runtime());
    assert!(!case.worktree(partial).exists());
    assert!(!case.listed(partial) && !case.entry(partial));
    assert!(case.worktree(recorded).join("README.md").is_file());
    assert!(case.listed(recorded) && case.entry(recorded));
    assert!(!case.worktree(nothing).exists() && !case.listed(nothing));
    assert!(!case.listed(lost) && !case.entry(lost));
    assert_eq!(
        std::fs::read_to_string(case.worktree(foreign).join("notes.txt")).unwrap(),
        "kept\n"
    );
    let settled = vec![
        ended(partial, "failed"),
        ended(recorded, "done"),
        ended(nothing, "failed"),
        ended(lost, "failed"),
        ended(foreign, "unknown"),
        shown(foreign),
    ];
    assert_eq!(case.outcomes("worktree_create"), settled);
    assert_eq!(case.pending(ActionKind::WorktreeCreate), 1);

    drop(case.runtime());
    settle(&case.team, &case.base, foreign, false);
    assert_eq!(case.outcomes("worktree_create"), settled);
    assert!(case.listed(recorded));
    assert!(case.worktree(foreign).join("notes.txt").is_file());
    unsettled(
        begin_worktree(&case.base, foreign, &case.repo, &case.worktree(foreign)),
        "An earlier add of this agent's worktree has an unknown outcome. Check its folder, then dismiss it in Recovery",
    );

    begin_worktree(&case.base, partial, &case.repo, &case.worktree(partial)).unwrap();
    case.add(partial, true);
    settle(&case.team, &case.base, partial, false);
    assert!(!case.worktree(partial).exists() && !case.listed(partial) && !case.entry(partial));
    begin_worktree(&case.base, partial, &case.repo, &case.worktree(partial)).unwrap();
    case.add(partial, true);
    case.record(partial);
    settle(&case.team, &case.base, partial, false);
    assert!(case.listed(partial));
    let mut expected = settled;
    expected.extend([ended(partial, "failed"), ended(partial, "done")]);
    assert_eq!(case.outcomes("worktree_create"), expected);
    assert_eq!(case.pending(ActionKind::WorktreeCreate), 1);

    case.dismiss(ActionKind::WorktreeCreate, foreign);
    assert_eq!(case.pending(ActionKind::WorktreeCreate), 0);
    expected[5] = ended(foreign, "unknown");
    assert_eq!(case.outcomes("worktree_create"), expected);
    assert!(case.worktree(foreign).join("notes.txt").is_file());
}

/// A worktree `.git` pointer or a `.git/worktrees` entry `gitdir` that is a link is never
/// read through (VS-1): a pointer link that names the agent's own entry leaves the
/// worktree as foreign, and an entry whose `gitdir` link points back is never taken for the
/// worktree's entry, so neither the folder, the entry nor the link targets are removed.
#[test]
fn a_worktree_pointer_reached_through_a_link_is_never_read() {
    let case = Case::new();
    let id = case.agent();
    let worktree = case.worktree(id);
    let entry = case
        .repo
        .join(".git")
        .join("worktrees")
        .join(format!("agent-{id}"));
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&entry).unwrap();
    let back = case.base.join("gitdir");
    std::fs::write(&back, format!("{}\n", worktree.join(".git").display())).unwrap();
    let pointer = case.base.join("pointer");
    std::fs::write(&pointer, format!("gitdir: {}\n", entry.display())).unwrap();
    std::os::unix::fs::symlink(&pointer, worktree.join(".git")).unwrap();
    std::os::unix::fs::symlink(&back, entry.join("gitdir")).unwrap();
    let intent = WorktreeIntent {
        team_agent: id,
        repository: case.repo.clone(),
        worktree: worktree.clone(),
        unknown: false,
    };
    assert_eq!(remove(&intent), Err("team_worktree_foreign"));
    assert!(worktree.join(".git").is_symlink());
    assert!(pointer.is_file());

    std::fs::remove_dir_all(&worktree).unwrap();
    assert_eq!(remove(&intent), Ok(()));
    assert!(
        entry.join("gitdir").is_symlink(),
        "an entry whose gitdir is a link is never taken as the worktree's own"
    );
    assert!(back.is_file());
}

#[test]
#[should_panic(expected = "an agent worktree sits at worktrees/agent-<id> in its checkout")]
fn a_worktree_outside_its_agent_folder_is_never_recorded() {
    let case = Case::new();
    let id = case.agent();
    let _ = begin_worktree(&case.base, id, &case.repo, &case.repo.join("src"));
}

#[test]
fn agent_spawn_interrupted_does_not_start_a_second_harness() {
    let mut case = Case::new();
    let host = case.host();
    let running = case.agent();
    begin_spawn(&case.base, running, true).unwrap();
    case.pane(&host, running);
    let never = case.agent();
    begin_spawn(&case.base, never, true).unwrap();
    let piped = case.agent();
    begin_spawn(&case.base, piped, false).unwrap();
    let checking = "An earlier start of this agent is still being checked";
    let noticed = "An earlier start of this agent has an unknown outcome. Check the agent, then dismiss it in Recovery";
    unsettled(begin_spawn(&case.base, running, true), checking);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 3);
    let team = AgentId::team(running).unwrap();
    assert!(host.alive().unwrap().contains(&team));

    let headless = case.runtime();
    assert_eq!(case.pending(ActionKind::AgentSpawn), 3);
    unsettled(begin_spawn(&case.base, running, true), checking);
    unsettled(begin_spawn(&case.base, piped, false), noticed);
    let runtime = headless.with_terminal(host.clone()).unwrap();
    assert!(!host.alive().unwrap().contains(&team));
    let settled = vec![
        ended(running, "unknown"),
        ended(never, "failed"),
        ended(piped, "unknown"),
        shown(piped),
        shown(running),
    ];
    assert_eq!(case.outcomes("agent_spawn"), settled);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 2);
    drop(runtime);

    unsettled(begin_spawn(&case.base, running, true), noticed);
    settle(&case.team, &case.base, running, false);
    spawned(&case.base, piped, Outcome::Failed);
    drop(case.runtime().with_terminal(host.clone()).unwrap());
    assert_eq!(case.outcomes("agent_spawn"), settled);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 2);
    case.dismiss(ActionKind::AgentSpawn, running);
    case.dismiss(ActionKind::AgentSpawn, piped);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 0);
    let mut settled = settled;
    settled[3] = ended(piped, "unknown");
    settled[4] = ended(running, "unknown");
    begin_spawn(&case.base, running, true).unwrap();
    spawned(&case.base, running, Outcome::Done);
    settled.push(ended(running, "done"));

    let ready = case.agent();
    begin_spawn(&case.base, ready, true).unwrap();
    spawned(&case.base, ready, Outcome::Done);
    spawned(&case.base, ready, Outcome::Failed);
    let unhosted = case.agent();
    begin_spawn(&case.base, unhosted, true).unwrap();
    settle(&case.team, &case.base, unhosted, false);
    let started = case.agent();
    begin_spawn(&case.base, started, true).unwrap();
    settle(&case.team, &case.base, started, true);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 1);
    spawned(&case.base, started, Outcome::Failed);
    begin_spawn(&case.base, ready, true).unwrap();
    spawned(&case.base, ready, Outcome::Done);
    let mut expected = settled;
    expected.extend([
        ended(ready, "done"),
        ended(unhosted, "failed"),
        ended(started, "failed"),
        ended(ready, "done"),
    ]);
    assert_eq!(case.outcomes("agent_spawn"), expected);
    assert_eq!(case.pending(ActionKind::AgentSpawn), 0);
}
