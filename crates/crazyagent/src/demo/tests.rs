use super::*;
use std::process::Command;

const POOLS: &str = r#"
[model_access]
accounts = ["account-codex", "account-claude", "account-rcli"]
[[model_access.connections]]
id = "m-codex"
provider = "provider-codex"
harness = "codex"
protocol = "native"
account_ref = "account-codex"
endpoint = { kind = "native" }
model = { id = "native-default", author = "author-codex" }
auth = { method = "native_managed", profile_ref = "profile-codex" }
[[model_access.connections]]
id = "m-claude"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "native-default", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
[[model_access.connections]]
id = "m-rcli"
provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "account-rcli"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "local-model", author = "openai-compatible" }
auth = { method = "none" }
[[model_access.pools]]
id = "pool-codex"
members = ["m-codex"]
projects = []
roles = []
fallback_allowed = false
[[model_access.pools]]
id = "pool-claude"
members = ["m-claude"]
projects = []
roles = []
fallback_allowed = false
[[model_access.pools]]
id = "pool-rcli"
members = ["m-rcli"]
projects = []
roles = []
fallback_allowed = false
"#;

fn config() -> EffectiveConfig {
    crate::config::resolve_config_bytes(POOLS, "").unwrap()
}

fn pool(id: &str) -> CredentialRef {
    CredentialRef::new(id).unwrap()
}

fn database(root: &Path) -> Connection {
    let db = Connection::open(root.join("engine.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE projects(id TEXT PRIMARY KEY);
         CREATE TABLE native_auth_operations(profile_key TEXT, generation INTEGER, phase TEXT);
         CREATE TABLE master_sessions(id TEXT PRIMARY KEY, project TEXT);
         CREATE TABLE master_turns(session TEXT, ordinal INTEGER, input INTEGER);
         CREATE TABLE team_agents(project TEXT, role TEXT, created_ms INTEGER);
         CREATE TABLE landing_change_sets(project TEXT, state TEXT, created_ms INTEGER);
         CREATE TABLE team_messages(id INTEGER PRIMARY KEY, project TEXT);
         CREATE TABLE team_notifications(id INTEGER PRIMARY KEY, project TEXT, kind TEXT,
             text TEXT, created_ms INTEGER, seen_ms INTEGER);",
    )
    .unwrap();
    crate::questions::initialize(&db).unwrap();
    initialize(&db).unwrap();
    db
}

fn git(root: &Path, home: &Path, arguments: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(arguments)
        .current_dir(root)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn staged(root: &Path) -> std::path::PathBuf {
    let staging = root.join("staging");
    let home = root.join("git-home");
    std::fs::create_dir(&staging).unwrap();
    std::fs::create_dir(&home).unwrap();
    crate::research::runner::ensure_repository(&staging, &home).unwrap();
    crate::projects_home::prepare_layout(&staging).unwrap();
    assert!(crate::projects_home::exclude_from_git(&staging).unwrap());
    staging
}

#[test]
fn the_seed_is_a_small_web_page_with_a_node_test_that_needs_nothing_installed_or_fetched() {
    let paths: Vec<&str> = SEED.iter().map(|(path, _)| *path).collect();
    assert_eq!(
        paths,
        [
            "index.html",
            "style.css",
            "app.js",
            "test/app.test.js",
            "README.md"
        ]
    );
    for (path, text) in SEED {
        assert!(!text.is_empty() && text.len() < 4096, "{path} is small");
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "{path} fetches nothing"
        );
        assert!(
            !text.contains("<!--") && !text.contains("/*"),
            "{path} has no comments"
        );
    }
    let test = SEED[3].1;
    for line in test.lines().filter(|line| line.contains("require(")) {
        assert!(
            line.contains("\"node:") || line.contains("\"../app.js\""),
            "the test needs only Node and the page: {line}"
        );
    }
    assert!(SEED[0].1.contains("<script src=\"app.js\"></script>"));
    assert!(SEED[4].1.contains("node --test"));
    assert!(GOAL.contains("node --test") && GOAL.contains("no dependencies"));
}

#[test]
fn seeding_commits_the_page_on_the_start_commit_with_no_remote_and_leaves_no_git_home() {
    let temp = tempfile::tempdir().unwrap();
    let staging = staged(temp.path());
    seed(&staging).unwrap();
    let home = temp.path().join("git-home");
    assert_eq!(
        git(&staging, &home, &["log", "--format=%s"]),
        "Add the demo web page\nStart project\n"
    );
    assert_eq!(git(&staging, &home, &["remote"]), "");
    let mut tracked: Vec<String> = git(&staging, &home, &["ls-files"])
        .lines()
        .map(String::from)
        .collect();
    tracked.sort();
    assert_eq!(
        tracked,
        [
            "README.md",
            "app.js",
            "index.html",
            "style.css",
            "test/app.test.js"
        ]
    );
    assert_eq!(git(&staging, &home, &["status", "--porcelain"]), "");
    let state = crate::projects_home::state_folder(&staging);
    assert!(!state.join("demo-git-home").exists());
}

#[test]
fn seeding_refuses_a_file_already_there_and_never_replaces_it() {
    let temp = tempfile::tempdir().unwrap();
    let staging = staged(temp.path());
    std::fs::write(staging.join("index.html"), "OWNER_PAGE_4471").unwrap();
    assert_eq!(seed(&staging), Err("demo_seed_failed"));
    assert_eq!(
        std::fs::read_to_string(staging.join("index.html")).unwrap(),
        "OWNER_PAGE_4471"
    );
    let state = crate::projects_home::state_folder(&staging);
    assert!(!state.join("demo-git-home").exists());
}

#[test]
fn a_demo_is_a_plain_new_project_with_the_picked_pool_and_only_the_demo_id() {
    let id = ProjectId::new(PROJECT_ID).unwrap();
    if !cfg!(target_os = "macos") {
        assert_eq!(
            create_action(&id, &pool("pool-codex")).unwrap_err().0,
            "demo_platform_unqualified"
        );
        return;
    }
    let action = create_action(&id, &pool("pool-codex")).unwrap();
    let SetupAction::Create {
        project_id,
        spec,
        setup,
    } = action
    else {
        panic!("a demo is a Create");
    };
    assert_eq!(project_id.as_str(), PROJECT_ID);
    assert_eq!(spec.name, NAME);
    assert!(spec.repositories.is_empty());
    assert!(setup.documents.is_empty() && setup.research.is_empty());
    assert_eq!(setup.pool, Some(pool("pool-codex")));
    assert_eq!(
        setup.team, None,
        "New Project's own team default picks the master"
    );
    let other = ProjectId::new("project-other").unwrap();
    assert_eq!(
        create_action(&other, &pool("pool-codex")).unwrap_err().0,
        "demo_project_id_invalid"
    );
}

#[test]
fn a_pool_offers_the_demo_only_with_a_signed_in_account_for_its_master_and_never_on_rcli_alone() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = database(temp.path());
    let config = config();
    let tx = db.transaction().unwrap();
    let why = |id: &str| pool_unavailable(&tx, &config, &pool(id)).unwrap();
    let sign_in = |profile: &[u8]| {
        tx.execute(
            "INSERT INTO native_auth_operations VALUES(?1,1,'authenticated_observed')",
            [crate::memory::hash(profile)],
        )
        .unwrap();
    };
    assert_eq!(why("pool-codex"), Some("demo_pool_not_ready"), "signed out");
    assert_eq!(
        why("pool-claude"),
        Some("demo_pool_not_ready"),
        "signed out"
    );
    assert_eq!(why("pool-rcli"), Some("demo_harness_unavailable"));
    assert_eq!(why("pool-gone"), Some("demo_pool_missing"));
    sign_in(b"profile-codex");
    assert_eq!(
        why("pool-codex"),
        None,
        "a signed-in Codex account runs the master"
    );
    assert_eq!(why("pool-claude"), Some("demo_pool_not_ready"));
    sign_in(b"profile-claude");
    assert_eq!(
        why("pool-claude"),
        None,
        "a signed-in Claude account runs the master"
    );
    assert_eq!(why("pool-rcli"), Some("demo_harness_unavailable"));
}

#[test]
fn the_demo_reads_each_step_from_its_own_rows_and_a_new_demo_never_inherits_a_deleted_ones() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = database(temp.path());
    let config = config();
    let tx = db.transaction().unwrap();
    let empty = view(&tx, &config).unwrap();
    assert_eq!(empty.project_id, None);
    assert_eq!(empty.created_ms, None);
    assert_eq!(empty.steps, DemoSteps::default());
    assert_eq!(empty.goal, GOAL);
    assert_eq!(empty.pools.len(), 3);
    assert!(!exists(&tx).unwrap());
    let id = ProjectId::new(PROJECT_ID).unwrap();
    tx.execute("INSERT INTO projects VALUES(?1)", [PROJECT_ID])
        .unwrap();
    record(&tx, &id, 1_000).unwrap();
    assert!(exists(&tx).unwrap());
    let made = view(&tx, &config).unwrap();
    assert_eq!(
        made.project_id.as_ref().map(ProjectId::as_str),
        Some(PROJECT_ID)
    );
    assert_eq!(made.created_ms, Some(1_000));
    assert_eq!(
        made.steps,
        DemoSteps {
            created: true,
            ..DemoSteps::default()
        }
    );
    tx.execute_batch(
        "INSERT INTO master_sessions VALUES('s1','crazyagent-demo');
         INSERT INTO master_turns VALUES('s1',1,7);
         INSERT INTO team_agents VALUES('crazyagent-demo','master',1000);",
    )
    .unwrap();
    let sent = view(&tx, &config).unwrap().steps;
    assert!(
        sent.goal_sent && !sent.agent_started,
        "the master alone is not a team agent"
    );
    tx.execute_batch(
        "INSERT INTO team_agents VALUES('crazyagent-demo','agent',1500);
         INSERT INTO landing_change_sets VALUES('other-project','accepted',1500);
         INSERT INTO landing_change_sets VALUES('crazyagent-demo','pending',1500);",
    )
    .unwrap();
    let working = view(&tx, &config).unwrap().steps;
    assert!(working.agent_started && !working.change_set_accepted);
    tx.execute(
        "UPDATE landing_change_sets SET state='accepted' WHERE project=?1",
        [PROJECT_ID],
    )
    .unwrap();
    assert!(view(&tx, &config).unwrap().steps.change_set_accepted);
    tx.execute("DELETE FROM projects WHERE id=?1", [PROJECT_ID])
        .unwrap();
    let gone = view(&tx, &config).unwrap();
    assert_eq!(gone.project_id, None);
    assert_eq!(gone.steps, DemoSteps::default());
    record(&tx, &id, 2_000).unwrap();
    tx.execute("INSERT INTO projects VALUES(?1)", [PROJECT_ID])
        .unwrap();
    let again = view(&tx, &config).unwrap();
    assert_eq!(again.created_ms, Some(2_000));
    assert_eq!(
        again.steps,
        DemoSteps {
            created: true,
            ..DemoSteps::default()
        },
        "rows Delete everything kept belong to the deleted demo"
    );
    tx.execute_batch(
        "INSERT INTO master_sessions VALUES('s2','crazyagent-demo');
         INSERT INTO master_turns VALUES('s2',1,8);
         INSERT INTO team_agents VALUES('crazyagent-demo','agent',2000);",
    )
    .unwrap();
    let fresh = view(&tx, &config).unwrap().steps;
    assert!(fresh.goal_sent && fresh.agent_started && !fresh.change_set_accepted);
}

fn goal_sent(db: &mut Connection, config: &EffectiveConfig) -> bool {
    let tx = db.transaction().unwrap();
    view(&tx, config).unwrap().steps.goal_sent
}

#[test]
fn a_goal_typed_into_the_live_master_terminal_ticks_goal_sent_but_engine_words_and_older_rows_do_not()
 {
    let temp = tempfile::tempdir().unwrap();
    let mut db = database(temp.path());
    let config = config();
    let id = ProjectId::new(PROJECT_ID).unwrap();
    db.execute("INSERT INTO projects VALUES(?1)", [PROJECT_ID])
        .unwrap();
    let tx = db.transaction().unwrap();
    record(&tx, &id, 1_000).unwrap();
    tx.commit().unwrap();
    assert!(
        !goal_sent(&mut db, &config),
        "a database with no master conversation yet still reads"
    );
    crate::master_sessions::mark_live_turn(
        &db,
        &id,
        "live-1",
        "claude",
        Some(&crate::master_sessions::Fed::Engine(
            "The owner answered the demo question.".into(),
        )),
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_messages(session,seq,project,speaker,mode,harness,text,at_ms,source,source_key)
         VALUES('kept',1,?1,'owner','live','claude','An older goal.',900,'terminal','t1'),
               ('kept',2,?1,'master','live','claude','Done.',1500,'terminal','r2')",
        [PROJECT_ID],
    )
    .unwrap();
    assert!(
        !goal_sent(&mut db, &config),
        "engine words, the master's own words and owner words a deleted demo left are no goal"
    );
    crate::master_sessions::mark_owner_turn(&db, &id, "live-1", "claude").unwrap();
    assert!(
        goal_sent(&mut db, &config),
        "the owner's own turn typed into the live master terminal is the goal sent"
    );
}

#[test]
fn a_demo_that_would_start_wider_than_sandboxed_stops_at_the_assertion() {
    use crate::sandbox::policy::{AccessGrant, AgentAccess, AgentPolicy};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let temp = tempfile::tempdir().unwrap();
    let db = database(temp.path());
    let id = demo_id().unwrap();
    let trips = || catch_unwind(AssertUnwindSafe(|| assert_confined(&db, &id))).is_err();
    assert_confined(&db, &id).unwrap();
    for access in [AgentAccess::OwnBranch, AgentAccess::Full] {
        let policy = AgentPolicy {
            access,
            ..AgentPolicy::default()
        };
        crate::sandbox::policy::save(&db, PROJECT_ID, &policy).unwrap();
        assert!(trips(), "{access:?}");
    }
    let granted = AgentPolicy {
        grants: vec![AccessGrant::TempFolder],
        ..AgentPolicy::default()
    };
    crate::sandbox::policy::save(&db, PROJECT_ID, &granted).unwrap();
    assert!(trips(), "a grant");
    crate::sandbox::policy::save(&db, PROJECT_ID, &AgentPolicy::default()).unwrap();
    assert_confined(&db, &id).unwrap();
    let folder = [std::path::PathBuf::from("/Users/owner/notes")];
    crate::sandbox::extra_paths::save(&db, PROJECT_ID, &folder).unwrap();
    assert!(trips(), "a read folder");
    crate::sandbox::extra_paths::save(&db, PROJECT_ID, &[]).unwrap();
    crate::sandbox::hidden_paths::save(&db, PROJECT_ID, &folder).unwrap();
    assert!(trips(), "a hidden folder");
}

#[test]
fn the_planted_question_is_the_engines_own_once_and_its_answer_ticks_the_step() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = database(temp.path());
    db.execute("INSERT INTO projects VALUES(?1)", [PROJECT_ID])
        .unwrap();
    let first = plant_question(&db).unwrap();
    assert_eq!(
        plant_question(&db).unwrap(),
        first,
        "asked once while it waits"
    );
    let (asker, source, question, status): (String, String, String, String) = db
        .query_row(
            "SELECT asker,source,question,status FROM questions_log WHERE id=?1",
            [i64::try_from(first).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (
            asker.as_str(),
            source.as_str(),
            question.as_str(),
            status.as_str()
        ),
        ("engine:demo", "engine", QUESTION, "pending")
    );
    assert_eq!(crate::questions::asker_label(&asker), "the demo");
    db.execute(
        "UPDATE questions_log SET status='answered' WHERE id=?1",
        [i64::try_from(first).unwrap()],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    record(&tx, &ProjectId::new(PROJECT_ID).unwrap(), 1_000).unwrap();
    assert!(view(&tx, &config()).unwrap().steps.question_answered);
}

#[test]
fn every_demo_refusal_the_dialog_names_is_listed_once() {
    for code in ["project_exists", "home_destination_exists", "pool_missing"] {
        assert!(CODES.contains(&refusal(code.into()).as_str()), "{code}");
    }
    assert_eq!(refusal("projects_home_unset".into()), "projects_home_unset");
    let unique: std::collections::BTreeSet<&str> = CODES.into_iter().collect();
    assert_eq!(unique.len(), CODES.len());
}
