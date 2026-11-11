use super::move_project::PATH_COLUMNS;
use super::{
    AddMode, AddOverride, Asked, Finished, Registered, finish, forget, move_holds, pending, plan,
};
use crate::engine::EngineError;
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const PROJECT: &str = "app";
const EXCHANGE: i64 = 7;
const SEAL: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Runs git in `dir` with no global or system configuration and a fixed author. Inputs:
/// the folder and the arguments. Output: whether git succeeded and its trimmed standard
/// output.
fn git_try(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Move Test",
                "-c",
                "user.email=move@example.invalid",
                "-C",
            ])
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(output.stdout.len() <= 65_536);
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = git_try(dir, args);
    assert!(ok, "git {args:?} failed in {}", dir.display());
    out
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// One project kept where the owner made it, outside the projects home, with an engine
/// database beside the home: the temp folder, the home, the engine data folder, its
/// database, the project's repository and the open connection.
struct Case {
    _root: tempfile::TempDir,
    home: PathBuf,
    engine: PathBuf,
    database: PathBuf,
    repo: PathBuf,
    db: Connection,
}

impl Case {
    /// A committed repository at `<temp>/work/app`, registered and set up as project `app`,
    /// with every engine table made. Output: the case.
    fn new() -> Self {
        Self::at(|base| base.join("work"))
    }

    /// The same project with its repository made in the folder `parent` picks. Input: the
    /// choice from the temp base. Output: the case.
    fn at(parent: impl Fn(&Path) -> PathBuf) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("CrazyAgent");
        let engine = base.join("engine");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&engine).unwrap();
        let folder = parent(&base);
        std::fs::create_dir_all(&folder).unwrap();
        let repo = folder.join(PROJECT);
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README"), b"one\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-m", "first"]);
        let database = engine.join("engine.db");
        let db = Connection::open(&database).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::cleanup::initialize(&db).unwrap();
        drop(crate::landing::open(&engine).unwrap());
        drop(crate::recovery::open(&engine).unwrap());
        super::save(&db, &home).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: PROJECT.into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "codex".into(),
                master_tools: false,
                sub_harnesses: vec![],
                min_agents: 1,
                max_agents: 16,
            },
            documents: vec![],
            folder: text(&repo).into(),
            created_ms: 1,
            revision: 1,
        };
        let project = serde_json::json!({
            "project_id": PROJECT,
            "spec": {"name": PROJECT, "repositories": [text(&repo)]},
        });
        db.execute(
            "INSERT INTO projects(id, body) VALUES (?1,?2)",
            params![PROJECT, project.to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
        Self {
            _root: root,
            home,
            engine,
            database,
            repo,
            db,
        }
    }

    fn target(&self) -> PathBuf {
        self.home.join(PROJECT)
    }

    fn worktree(&self, agent: i64) -> PathBuf {
        self.repo
            .join(".crazyagent")
            .join("worktrees")
            .join(format!("agent-{agent}"))
    }

    /// Records a team agent with its copy as the runtime does. Inputs: the agent id, its
    /// status and the copy's git folder.
    fn record_agent(&self, agent: i64, status: &str, git_dir: &Path) {
        let worktree = self.worktree(agent);
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms)
                 VALUES (?1,?2,'team','codex',1,?3,?4,0,0)",
                params![agent, PROJECT, text(&worktree), status],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    PROJECT,
                    text(&self.repo),
                    text(&worktree),
                    text(git_dir)
                ],
            )
            .unwrap();
    }

    /// Gives a stopped agent its own clone inside the project folder (DEC-102), recorded in
    /// the team tables. Input: the agent id. Output: the clone's worktree folder.
    fn own_clone(&self, agent: i64) -> PathBuf {
        let worktree = self.worktree(agent);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        crate::landing::add_agent_clone(&self.repo, &worktree).unwrap();
        self.record_agent(agent, "stopped", &crate::agent_git::git_dir(&worktree));
        worktree
    }

    /// Gives a stopped agent a linked git worktree inside the project folder, recorded in
    /// the team tables with its admin folder. Input: the agent id. Output: the worktree.
    fn linked_worktree(&self, agent: i64) -> PathBuf {
        let worktree = self.worktree(agent);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let branch = format!("crazyagent/agent-{agent}");
        git(
            &self.repo,
            &["worktree", "add", "-b", &branch, text(&worktree)],
        );
        let admin = self
            .repo
            .join(".git")
            .join("worktrees")
            .join(format!("agent-{agent}"));
        assert!(admin.is_dir());
        self.record_agent(agent, "stopped", &admin);
        worktree
    }

    /// Writes one row with the project's paths into every other table of the inventory, as
    /// the engine's own writers leave them. Input: the agent whose copy the rows name.
    fn fill_inventory(&self, agent: i64) {
        let repo = text(&self.repo);
        let worktree = self.worktree(agent);
        let git_dir = crate::agent_git::git_dir(&worktree);
        let (worktree, git_dir) = (text(&worktree), text(&git_dir));
        let retired = self.worktree(99);
        self.db
            .execute(
                "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
                 VALUES (?1,?2,'pool','done','done','{}',1,1)",
                params![PROJECT, repo],
            )
            .unwrap();
        for table in ["onboarding_saved_drafts", "onboarding_saved_overviews"] {
            self.db
                .execute(
                    &format!("INSERT INTO {table}(repo,seal) VALUES (?1,?2)"),
                    params![repo, SEAL],
                )
                .unwrap();
        }
        self.db
            .execute(
                "INSERT INTO team_retiring(agent,project,repository,worktree,git_dir,since_ms)
                 VALUES (99,?1,?2,?3,?4,1)",
                params![
                    PROJECT,
                    repo,
                    text(&retired),
                    text(&crate::agent_git::git_dir(&retired))
                ],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,worktree,git_dir,
                 base,head,state,last_outcome,turn,prompt_line,files,added,removed,conflicts,created_ms,updated_ms)
                 VALUES ('set-1',?1,'sub','7','Agent 7',?2,?3,?4,'base','head','pending','succeeded','turn-1',
                 'Add a page',1,1,0,'[]',1,1)",
                params![PROJECT, repo, worktree, git_dir],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO landing_bases(worktree,base) VALUES (?1,'base')",
                [worktree],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO landing_agent_branches(worktree,repository,branch) VALUES (?1,?2,'agent/agent-7')",
                params![worktree, repo],
            )
            .unwrap();
        crate::landing::owner_edit::record(&self.db, Path::new(worktree), "README", 1).unwrap();
        self.db
            .execute(
                "INSERT INTO recovery_runs(agent,project,kind,harness,at_ms,turn,cwd,state)
                 VALUES (?1,?2,'sub','codex',1,1,?3,'stopped')",
                params![agent, PROJECT, worktree],
            )
            .unwrap();
        crate::sandbox::extra_paths::save(&self.db, PROJECT, &[self.repo.join("docs")]).unwrap();
        crate::sandbox::hidden_paths::save(&self.db, PROJECT, &[self.repo.join("secret")]).unwrap();
    }

    /// Records one research job of the project. Input: its status.
    fn research(&self, status: &str) {
        self.db
            .execute(
                "INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms)
                 VALUES (?1,'market_research','codex',1,?2,1)",
                params![PROJECT, status],
            )
            .unwrap();
    }

    fn registered(&self) -> Vec<Registered> {
        vec![Registered {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: PROJECT.into(),
            folder: self.stored_folder(),
            repositories: vec![self.stored_folder()],
            onboarding: false,
        }]
    }

    fn stored_folder(&self) -> PathBuf {
        let body: String = self
            .db
            .query_row(
                "SELECT body FROM project_setups WHERE project=?1",
                [PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        let stored: StoredSetup = serde_json::from_str(&body).unwrap();
        stored.project_folder()
    }

    fn declared(&self) -> Vec<String> {
        let body: String = self
            .db
            .query_row("SELECT body FROM projects WHERE id=?1", [PROJECT], |row| {
                row.get(0)
            })
            .unwrap();
        let stored: crate::protocol::StoredProject = serde_json::from_str(&body).unwrap();
        stored.spec.repositories
    }

    /// Plans the move in its own write, as the storage actor does. Inputs: Move or Copy and
    /// the owner's overrides. Output: the plan, with the pending row committed, or the
    /// refusal with nothing written.
    fn plan(
        &self,
        mode: AddMode,
        overrides: &[AddOverride],
    ) -> Result<super::ProjectMove, EngineError> {
        let project = ProjectId::new(PROJECT).unwrap();
        let tx = self.db.unchecked_transaction().unwrap();
        let planned = plan(
            &tx,
            &self.database,
            &self.engine,
            &self.registered(),
            &Asked {
                project: &project,
                mode,
                overrides,
                request: "request-7",
                exchange: EXCHANGE,
            },
        )?;
        tx.commit().unwrap();
        Ok(planned)
    }

    /// Finishes the pending move in its own write. Input: whether the disk work reported
    /// success. Output: what the finish found.
    fn finish(&self, verified: bool) -> Option<Finished> {
        let tx = self.db.unchecked_transaction().unwrap();
        let finished = finish(&tx, |pending| pending.exchange == EXCHANGE, verified).unwrap();
        tx.commit().unwrap();
        finished
    }

    /// The whole move as the engine runs it across the owner transport and the storage
    /// actor, in one call: plan, disk work with no wait for the team, then the record, or
    /// the settle of a move whose work failed. Output: what the finish found, or the
    /// refusal.
    fn move_project(
        &self,
        mode: AddMode,
        overrides: &[AddOverride],
    ) -> Result<Finished, EngineError> {
        let planned = self.plan(mode, overrides)?;
        match planned.run_with(Duration::ZERO) {
            Ok(()) => Ok(self.finish(true).expect("the planned move is pending")),
            Err(e) => {
                assert_eq!(self.finish(false), Some(Finished::Left));
                Err(e)
            }
        }
    }

    fn rows_under(&self, table: &str, column: &str, folder: &Path) -> i64 {
        self.db
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table}
                     WHERE {column}=?1 OR substr({column},1,length(?1)+1)=?1||'/'"
                ),
                [text(folder)],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Every text cell of the database that still names `folder`, as `table.column`.
    fn leftovers(&self, folder: &Path) -> Vec<String> {
        let tables: Vec<String> = self
            .db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(tables.len() > 20);
        let mut found = Vec::new();
        for table in tables {
            let columns: Vec<String> = self
                .db
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            for column in columns {
                let named: i64 = self
                    .db
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM \"{table}\"
                             WHERE typeof(\"{column}\")='text' AND instr(\"{column}\", ?1) > 0"
                        ),
                        [text(folder)],
                        |row| row.get(0),
                    )
                    .unwrap();
                if named > 0 {
                    found.push(format!("{table}.{column}"));
                }
            }
        }
        found
    }
}

/// A Move takes the folder into the home, points the setup, the declared repository and
/// every inventory column at the new place, leaves no text cell naming the old folder, and
/// gives the agent's own clone the moved project's object store.
#[test]
fn moving_a_project_kept_in_place_updates_every_stored_folder_and_repoints_its_agent_clones() {
    let case = Case::new();
    let origin = case.repo.clone();
    let target = case.target();
    case.own_clone(7);
    case.fill_inventory(7);
    case.research("needs_pool");
    for (table, column) in PATH_COLUMNS {
        assert!(
            case.rows_under(table, column, &origin) > 0,
            "{table}.{column} has no row to move"
        );
    }
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap(),
        Finished::Moved { repointed: 1 }
    );
    assert!(std::fs::symlink_metadata(&origin).is_err());
    assert!(target.join("README").is_file());
    assert_eq!(pending(&case.db).unwrap(), None);
    assert_eq!(case.stored_folder(), target);
    assert_eq!(case.declared(), vec![text(&target).to_owned()]);
    for (table, column) in PATH_COLUMNS {
        assert_eq!(
            case.rows_under(table, column, &origin),
            0,
            "{table}.{column}"
        );
        assert!(
            case.rows_under(table, column, &target) > 0,
            "{table}.{column}"
        );
    }
    assert_eq!(case.leftovers(&origin), Vec::<String>::new());
    let clone = target.join(".crazyagent").join("worktrees").join("agent-7");
    let alternates = std::fs::read_to_string(
        clone
            .join(".git")
            .join("objects")
            .join("info")
            .join("alternates"),
    )
    .unwrap();
    assert_eq!(alternates, format!("{}/.git/objects\n", text(&target)));
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&target, &["rev-parse", "HEAD"])
    );
}

/// A project whose folder is already inside the projects home is refused, with nothing
/// recorded and nothing moved.
#[test]
fn a_project_already_inside_the_home_is_refused() {
    let case = Case::at(|base| base.join("CrazyAgent"));
    assert!(case.repo.starts_with(&case.home));
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "move_project_already_home"
    );
    assert_eq!(pending(&case.db).unwrap(), None);
    assert!(case.repo.join("README").is_file());
}

/// Nothing moves while the project works: a master session holding its own working copy,
/// an agent that runs with the team not stopped, and an agent still running past the wait
/// after Stop all each refuse, and the folder and its stored paths stay.
#[test]
fn a_project_with_a_running_agent_is_refused_and_nothing_moves() {
    let case = Case::new();
    let origin = case.repo.clone();
    case.research("running");
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "move_project_busy"
    );
    case.db
        .execute("UPDATE research_jobs SET status='queued'", [])
        .unwrap();
    case.db
        .execute(
            "INSERT INTO worktree_leases(task,repository,path,body) VALUES ('task-1',?1,?2,?3)",
            params![
                text(&origin),
                text(&case.engine.join("lease-1")),
                r#"{"lease":{"state":"Ready"}}"#
            ],
        )
        .unwrap();
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "move_project_master_session"
    );
    case.db.execute("DELETE FROM worktree_leases", []).unwrap();
    case.record_agent(4, "working", &case.worktree(4).join(".git"));
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "move_project_busy"
    );
    assert_eq!(pending(&case.db).unwrap(), None);
    case.db
        .execute(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,1)",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "move_project_busy"
    );
    assert_eq!(pending(&case.db).unwrap(), None);
    assert!(origin.join("README").is_file());
    assert!(std::fs::symlink_metadata(case.target()).is_err());
    assert_eq!(case.stored_folder(), origin);
    case.db
        .execute("UPDATE team_agents SET status='stopped' WHERE id=4", [])
        .unwrap();
    assert!(matches!(
        case.move_project(AddMode::Move, &[]).unwrap(),
        Finished::Moved { .. }
    ));
    assert_eq!(case.stored_folder(), case.target());
}

/// A commit an agent made in its own clone and that has not landed is still there after
/// the move, on the same branch, and its parent is read from the moved project's store.
#[test]
fn an_unlanded_commit_in_an_agent_clone_is_kept_after_the_move() {
    let case = Case::new();
    let clone = case.own_clone(7);
    std::fs::write(clone.join("page.txt"), b"work\n").unwrap();
    git(&clone, &["add", "page.txt"]);
    git(&clone, &["commit", "-m", "unlanded work"]);
    let head = git(&clone, &["rev-parse", "HEAD"]);
    let branch = git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let first = git(&case.repo, &["rev-parse", "HEAD"]);
    assert_ne!(head, first);
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap(),
        Finished::Moved { repointed: 1 }
    );
    let moved = case
        .target()
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-7");
    assert_eq!(git(&moved, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&moved, &["rev-parse", "--abbrev-ref", "HEAD"]), branch);
    assert_eq!(git(&moved, &["rev-parse", "HEAD~1"]), first);
    assert_eq!(git(&moved, &["log", "-1", "--format=%s"]), "unlanded work");
    assert_eq!(git(&moved, &["show", "HEAD~1:README"]), "one");
    assert_eq!(
        std::fs::read_to_string(moved.join("page.txt")).unwrap(),
        "work\n"
    );
}

/// A Copy leaves the old folder whole, points the project at the copy, and gives the
/// copied agent clone the copy's object store while the old clone keeps the old one.
#[test]
fn a_copy_leaves_the_old_folder_and_points_the_project_at_the_copy() {
    let case = Case::new();
    let origin = case.repo.clone();
    let target = case.target();
    let old_clone = case.own_clone(7);
    std::fs::write(old_clone.join("page.txt"), b"work\n").unwrap();
    git(&old_clone, &["add", "page.txt"]);
    git(&old_clone, &["commit", "-m", "unlanded work"]);
    let head = git(&old_clone, &["rev-parse", "HEAD"]);
    assert_eq!(
        case.move_project(AddMode::Copy, &[]).unwrap(),
        Finished::Moved { repointed: 1 }
    );
    assert!(origin.join("README").is_file());
    assert!(target.join("README").is_file());
    assert_eq!(case.stored_folder(), target);
    assert_eq!(case.declared(), vec![text(&target).to_owned()]);
    assert_eq!(case.rows_under("team_worktrees", "git_dir", &origin), 0);
    assert_eq!(case.rows_under("team_worktrees", "git_dir", &target), 1);
    let new_clone = target.join(".crazyagent").join("worktrees").join("agent-7");
    let alternates = |clone: &Path| {
        std::fs::read_to_string(
            clone
                .join(".git")
                .join("objects")
                .join("info")
                .join("alternates"),
        )
        .unwrap()
    };
    assert_eq!(
        alternates(&new_clone),
        format!("{}/.git/objects\n", text(&target))
    );
    assert_eq!(
        alternates(&old_clone),
        format!("{}/.git/objects\n", text(&origin))
    );
    assert_eq!(git(&new_clone, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&old_clone, &["rev-parse", "HEAD"]), head);
}

/// An agent's linked git worktree inside the folder follows a Move: both of its git
/// pointer files name the new places, so git works in it and the repository lists it.
#[test]
fn a_linked_agent_worktree_inside_the_folder_follows_a_move() {
    let case = Case::new();
    let target = case.target();
    let worktree = case.linked_worktree(8);
    std::fs::write(worktree.join("page.txt"), b"work\n").unwrap();
    git(&worktree, &["add", "page.txt"]);
    git(&worktree, &["commit", "-m", "unlanded work"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap(),
        Finished::Moved { repointed: 1 }
    );
    let moved = target.join(".crazyagent").join("worktrees").join("agent-8");
    assert_eq!(git(&moved, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&moved, &["rev-parse", "--show-toplevel"]), text(&moved));
    assert_eq!(git(&moved, &["status", "--porcelain"]), "");
    let listed = git(&target, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(&format!("worktree {}", text(&moved))));
    assert!(!listed.contains(text(&case.repo)));
    assert_eq!(
        case.rows_under(
            "team_worktrees",
            "git_dir",
            &target.join(".git").join("worktrees")
        ),
        1
    );
}

/// A Copy is refused while an agent works in a linked git worktree of the repository,
/// since the copy would leave that work with the original.
#[test]
fn a_copy_is_refused_while_an_agent_works_in_a_linked_worktree() {
    let case = Case::new();
    case.linked_worktree(8);
    assert_eq!(
        case.move_project(AddMode::Copy, &[]).unwrap_err().0,
        "move_project_copy_worktrees"
    );
    assert_eq!(pending(&case.db).unwrap(), None);
    assert!(std::fs::symlink_metadata(case.target()).is_err());
    assert_eq!(case.stored_folder(), case.repo);
}

/// A Move whose record never arrived is finished from what is on disk; a Copy in the same
/// place is not, since a copy cut short looks the same, and the project stays where it was.
#[test]
fn a_move_whose_record_never_came_is_finished_from_disk_and_a_copy_is_not() {
    let moved = Case::new();
    moved.own_clone(7);
    let planned = moved.plan(AddMode::Move, &[]).unwrap();
    planned.run_with(Duration::ZERO).unwrap();
    assert_eq!(moved.stored_folder(), moved.repo);
    assert_eq!(moved.finish(false), Some(Finished::Moved { repointed: 1 }));
    assert_eq!(moved.stored_folder(), moved.target());
    assert_eq!(moved.finish(false), None);

    let copied = Case::new();
    let planned = copied.plan(AddMode::Copy, &[]).unwrap();
    planned.run_with(Duration::ZERO).unwrap();
    assert_eq!(copied.finish(false), Some(Finished::Left));
    assert_eq!(copied.stored_folder(), copied.repo);
    assert_eq!(pending(&copied.db).unwrap(), None);

    let never = Case::new();
    never.plan(AddMode::Move, &[]).unwrap();
    assert_eq!(never.finish(false), Some(Finished::Left));
    assert_eq!(never.stored_folder(), never.repo);
    assert!(never.repo.join("README").is_file());
}

/// A planned move holds launches in the project's folder and refuses a second move until
/// it is finished or forgotten; a move of another exchange is not forgotten.
#[test]
fn a_pending_project_move_holds_the_folder_and_refuses_a_second_move() {
    let case = Case::new();
    let other = case.home.join("other");
    assert!(!move_holds(&case.db, &case.repo).unwrap());
    case.plan(AddMode::Move, &[]).unwrap();
    let recorded = pending(&case.db).unwrap().unwrap();
    assert_eq!(recorded.origin, case.repo);
    assert_eq!(recorded.target, case.target());
    assert_eq!(recorded.mode, AddMode::Move);
    assert!(move_holds(&case.db, &case.repo).unwrap());
    assert!(move_holds(&case.db, &case.worktree(7)).unwrap());
    assert!(!move_holds(&case.db, &other).unwrap());
    assert_eq!(
        case.plan(AddMode::Move, &[]).unwrap_err().0,
        "move_project_in_progress"
    );
    let tx = case.db.unchecked_transaction().unwrap();
    forget(&tx, EXCHANGE + 1).unwrap();
    tx.commit().unwrap();
    assert!(move_holds(&case.db, &case.repo).unwrap());
    let tx = case.db.unchecked_transaction().unwrap();
    forget(&tx, EXCHANGE).unwrap();
    tx.commit().unwrap();
    assert!(!move_holds(&case.db, &case.repo).unwrap());
    assert_eq!(pending(&case.db).unwrap(), None);
}

/// A folder another project also uses is refused in both modes, and a Move of a
/// repository with a git worktree kept outside the folder needs the owner's choice.
#[test]
fn a_shared_folder_is_refused_and_outside_worktrees_need_the_owners_choice() {
    let case = Case::new();
    let outside = case.repo.parent().unwrap().join("side");
    git(
        &case.repo,
        &["worktree", "add", "-b", "side", text(&outside)],
    );
    assert_eq!(
        case.move_project(AddMode::Move, &[]).unwrap_err().0,
        "home_move_worktrees"
    );
    assert!(case.repo.join("README").is_file());
    assert!(matches!(
        case.move_project(AddMode::Move, &[AddOverride::Worktrees])
            .unwrap(),
        Finished::Moved { .. }
    ));
    assert_eq!(case.stored_folder(), case.target());

    let shared = Case::new();
    let project = ProjectId::new(PROJECT).unwrap();
    let mut registered = shared.registered();
    registered.push(Registered {
        project_id: ProjectId::new("second").unwrap(),
        name: "second".into(),
        folder: shared.repo.join("packages"),
        repositories: vec![],
        onboarding: false,
    });
    for mode in [AddMode::Move, AddMode::Copy] {
        let tx = shared.db.unchecked_transaction().unwrap();
        let refused = plan(
            &tx,
            &shared.database,
            &shared.engine,
            &registered,
            &Asked {
                project: &project,
                mode,
                overrides: &[],
                request: "request-7",
                exchange: EXCHANGE,
            },
        )
        .unwrap_err();
        assert_eq!(refused.0, "home_move_project");
    }
}
