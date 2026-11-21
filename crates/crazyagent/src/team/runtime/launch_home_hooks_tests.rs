use super::*;

/// RV-DS241-137 code r2 F1: a native lead's teammates' home is reset as an agent home is at
/// each launch, so a crate source a teammate planted in its cargo home where the owner has
/// none, a package npx installed and a tool config no list names are gone, while the links
/// to the lead's team and task folders and Claude Code's answered config stay.
#[cfg(unix)]
#[test]
fn a_native_leads_teammates_home_is_reset_at_each_launch() {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let owner = root.join("owner");
    std::fs::create_dir_all(&owner).unwrap();
    let homes = ToolHomes::at(&owner);
    let home = root.join("agent/home");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mates = prepare_mates_home(&home, &homes, &project).unwrap();
    let planted = [
        mates.join(".cargo/registry/src/index/x-1.0.0/build.rs"),
        mates.join(".npm/_npx/0/node_modules/x/index.js"),
        mates.join(".bunfig.toml"),
    ];
    for path in &planted {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "planted\n").unwrap();
    }

    let again = prepare_mates_home(&home, &homes, &project).unwrap();

    assert_eq!(again, mates);
    let left: Vec<&PathBuf> = planted
        .iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect();
    assert!(left.is_empty(), "the teammates' home kept {left:?}");
    for folder in crate::sandbox::CLAUDE_TEAM_SHARED {
        assert_eq!(
            std::fs::read_link(mates.join(folder)).unwrap(),
            home.join(folder)
        );
    }
    assert!(mates.join(".claude.json").is_file());
}

/// SECRET-PARSE-COPIES R2, leader 16:05: a Codex agent whose launch left no readable hook key
/// list, as one an earlier CrazyAgent started, fails closed. The engine never asks Codex about
/// keys it could not read nor counts them as trusted; it keeps asking until `HOOK_TRUST_WAIT`
/// and then keeps a notice that says why in plain words. The other answers keep their texts.
#[test]
fn a_codex_agent_with_no_readable_hook_key_list_is_never_taken_as_trusted_and_is_told_why() {
    use crate::coord::settings::{
        CODEX_HOOKS_LIST_UNREADABLE, CODEX_HOOKS_UNCHECKED, CODEX_HOOKS_UNLISTED,
        CODEX_HOOKS_UNTRUSTED,
    };
    use crate::launch_context::HookKeysUnread::{Absent, Unreadable};
    let unasked = |_: &[String]| -> Result<bool, &'static str> {
        panic!("Codex was asked about hook keys the engine could not read")
    };
    for unread in [Absent, Unreadable] {
        assert_eq!(hook_trust(Err(unread), unasked, false), HookTrust::Later);
    }
    assert_eq!(
        hook_trust(Err(Absent), unasked, true),
        HookTrust::Notice(CODEX_HOOKS_UNLISTED),
        "a missing key list says why once the wait is over"
    );
    assert_eq!(
        hook_trust(Err(Unreadable), unasked, true),
        HookTrust::Notice(CODEX_HOOKS_LIST_UNREADABLE),
        "a key list that is there but unreadable is not blamed on an earlier CrazyAgent (#166 N0)"
    );
    assert!(
        !CODEX_HOOKS_LIST_UNREADABLE.contains("before this update")
            && CODEX_HOOKS_LIST_UNREADABLE.contains("replaced or damaged")
            && CODEX_HOOKS_LIST_UNREADABLE.contains("until the agent restarts")
            && !CODEX_HOOKS_LIST_UNREADABLE.contains('_'),
        "{CODEX_HOOKS_LIST_UNREADABLE}"
    );
    assert!(
        CODEX_HOOKS_UNLISTED.contains("started before this update")
            && CODEX_HOOKS_UNLISTED.contains("until the agent restarts")
            && !CODEX_HOOKS_UNLISTED.contains('_'),
        "the notice names the cause and the way out in plain words: {CODEX_HOOKS_UNLISTED}"
    );
    assert_eq!(
        hook_trust(Ok(Vec::new()), unasked, true),
        HookTrust::Settled
    );
    let keys = vec!["/h/config.toml:pre_tool_use:0:0".to_owned()];
    let expected = keys.clone();
    let asked = |answer: Result<bool, &'static str>| {
        let expected = expected.clone();
        move |given: &[String]| {
            assert_eq!(given, expected.as_slice());
            answer
        }
    };
    assert_eq!(
        hook_trust(Ok(keys.clone()), asked(Ok(true)), false),
        HookTrust::Settled
    );
    assert_eq!(
        hook_trust(Ok(keys.clone()), asked(Ok(false)), false),
        HookTrust::Notice(CODEX_HOOKS_UNTRUSTED)
    );
    assert_eq!(
        hook_trust(
            Ok(keys.clone()),
            asked(Err("codex_bridge_peer_unknown")),
            false
        ),
        HookTrust::Later
    );
    assert_eq!(
        hook_trust(Ok(keys), asked(Err("codex_bridge_peer_unknown")), true),
        HookTrust::Notice(CODEX_HOOKS_UNCHECKED)
    );
}

/// A project stored under the data volume's long name of its folder, with a hidden folder
/// saved in that name too, still launches an agent: its sandbox is built on the short name,
/// the only one Seatbelt matches, so the project stays readable, the worktree writable and
/// the hidden folder closed under either name (V243-FIRMLINK-LAUNCH-ROOT).
#[cfg(target_os = "macos")]
#[test]
fn a_project_stored_under_the_long_name_launches_an_agent_on_the_short_one() {
    let mut fixture = Fixture::new(1);
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let long = |path: &Path| PathBuf::from(format!("/System/Volumes/Data{}", path.display()));
    let project = fixture.base.join("project");
    assert!(long(&project).is_dir());
    std::fs::write(project.join("owner.txt"), "owner\n").unwrap();
    let drafts = project.join("drafts");
    std::fs::create_dir(&drafts).unwrap();
    std::fs::write(drafts.join("plan.txt"), "plan\n").unwrap();
    fixture.setup.folder = long(&project).to_str().unwrap().into();
    fixture
        .connection
        .execute(
            "UPDATE project_setups SET body=?2 WHERE project=?1",
            params![PROJECT, serde_json::to_string(&fixture.setup).unwrap()],
        )
        .unwrap();
    crate::sandbox::hidden_paths::save(&fixture.connection, PROJECT, &[long(&drafts)]).unwrap();
    let access = fixture.access.clone();
    let runtime = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap();
    let account = TeamAccount {
        connection: reference(ENDPOINT),
        reach: TeamReach::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        },
    };
    assert_eq!(
        fixture.agent(AgentRole::Sub, "codex", AgentStatus::Starting),
        1
    );
    let plan = runtime
        .prepare(
            &fixture.setup,
            1,
            AgentRole::Sub,
            "codex",
            true,
            &account,
            false,
            crate::agent_git::Layout::Linked,
            false,
        )
        .unwrap_or_else(|cause| panic!("a long-stored project must launch an agent: {cause}"))
        .1;
    assert!(plan.working_dir.starts_with(&project));
    let quoted = |path: &Path| format!("'{}'", path.display());
    let script = format!(
        "(cat {owner} >/dev/null) 2>/dev/null && echo owner=read || echo owner=refused\n\
         (cat {plan} >/dev/null) 2>/dev/null && echo plan=read || echo plan=refused\n\
         (cat {long_plan} >/dev/null) 2>/dev/null && echo long_plan=read || echo long_plan=refused\n\
         (echo mine > mine.txt) 2>/dev/null && echo wrote || echo write=refused\n",
        owner = quoted(&project.join("owner.txt")),
        plan = quoted(&drafts.join("plan.txt")),
        long_plan = quoted(&long(&drafts).join("plan.txt")),
    );
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&plan.sandbox_profile)
            .args(["/bin/sh", "-c", &script])
            .current_dir(&plan.working_dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", runtime.agent_folder(1).join("home"))
            .env("TMPDIR", runtime.agent_folder(1).join("tmp"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "owner=read\nplan=refused\nlong_plan=refused\nwrote\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(plan.working_dir.join("mine.txt")).unwrap(),
        "mine\n"
    );
}

/// How many ended tasks, answered owner messages and stopped agents the idle-cost row puts
/// in front of the tick's active project read.
const HISTORY_ROWS: i64 = 300;
/// The most SQLite steps that read may add over that history: far below one step per old
/// row, so any arm that walks the history fails.
const HISTORY_STEPS_SLACK: i32 = 32;

/// Idle cost (V243-ENGINE-IDLE-CPU): every tick reads which projects to step. Over a
/// history of delivered ended tasks, answered owner messages and stopped agents the read
/// takes as many SQLite steps as over none, so an idle tick does not grow with history.
#[test]
fn the_active_projects_read_does_not_grow_with_team_history() {
    let fixture = Fixture::new(1);
    let before = active_projects_steps(&fixture.connection);
    for _ in 0..HISTORY_ROWS {
        fixture
            .connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,delivered,created_ms)
                 VALUES (?1,'master','codex',0,'Old','done',1,1)",
                [PROJECT],
            )
            .unwrap();
        fixture
            .connection
            .execute(
                "INSERT INTO team_messages(project,author,text,state,created_ms)
                 VALUES (?1,'owner','Old','answered',1)",
                [PROJECT],
            )
            .unwrap();
        fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    }
    let after = active_projects_steps(&fixture.connection);
    assert!(
        after <= before + HISTORY_STEPS_SLACK,
        "active project steps grew from {before} to {after}"
    );
}

/// Runs the tick's active project read to its end and counts its SQLite virtual machine
/// steps. Input: the engine database. Output: the step count.
fn active_projects_steps(connection: &Connection) -> i32 {
    let mut statement = connection.prepare(ACTIVE_PROJECTS_SQL).unwrap();
    let mut rows = statement.query([]).unwrap();
    while rows.next().unwrap().is_some() {}
    drop(rows);
    let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
    assert!(steps > 0);
    steps
}

/// Idle cost (V243-ENGINE-IDLE-CPU): the tick follows the IDE workspaces only when the
/// database changed since it last did, so an idle tick runs none of their reads, while a
/// write by another connection or by the runtime makes the next tick follow them again.
#[test]
fn ide_workspaces_are_followed_only_after_the_database_changes() {
    let fixture = Fixture::new(1);
    let mut runtime = fixture.runtime();
    let write = "INSERT INTO team_messages(project,author,text,state,created_ms)
                 VALUES (?1,'owner','Seen','answered',1)";
    assert!(runtime.follow_due().unwrap(), "the first tick follows");
    runtime.tick().unwrap();
    runtime.tick().unwrap();
    assert!(
        !runtime.follow_due().unwrap(),
        "an idle tick leaves nothing to follow"
    );
    fixture.connection.execute(write, [PROJECT]).unwrap();
    assert!(runtime.follow_due().unwrap(), "another connection wrote");
    runtime.tick().unwrap();
    assert!(
        !runtime.follow_due().unwrap(),
        "the tick followed that write"
    );
    runtime.connection.execute(write, [PROJECT]).unwrap();
    assert!(runtime.follow_due().unwrap(), "the runtime wrote");
    runtime.shutdown().unwrap();
}

/// Idle cost (V243-ENGINE-IDLE-CPU, rv-code2 F1): another connection's commit that lands
/// while the tick follows the IDE workspaces, after the follow read its rows, keeps the
/// next tick due, and that tick's follow leaves nothing due again.
#[test]
fn a_commit_during_the_workspace_follow_is_followed_on_the_next_tick() {
    let fixture = Fixture::new(1);
    let mut runtime = fixture.runtime();
    let write = "INSERT INTO team_messages(project,author,text,state,created_ms)
                 VALUES (?1,'owner','Seen','answered',1)";
    runtime.tick().unwrap();
    runtime.tick().unwrap();
    assert!(
        !runtime.follow_due().unwrap(),
        "an idle tick leaves nothing to follow"
    );
    fixture.connection.execute(write, [PROJECT]).unwrap();
    runtime
        .follow_workspaces(|db, root| {
            crate::ide::workspace::follow(db, root)?;
            fixture.connection.execute(write, [PROJECT]).unwrap();
            Ok(())
        })
        .unwrap();
    assert!(
        runtime.follow_due().unwrap(),
        "a commit during the follow stays due"
    );
    runtime
        .follow_workspaces(crate::ide::workspace::follow)
        .unwrap();
    assert!(!runtime.follow_due().unwrap(), "the next follow caught it");
    runtime.shutdown().unwrap();
}

/// Idle cost (V243-ENGINE-IDLE-CPU): with no queued task, dispatch reads none of the
/// settings only its per-task loop uses (caps, open recovery runs, Reuse idle, Keep warm)
/// on each tick. A caps table that cannot be read shows it: an idle dispatch succeeds.
#[test]
fn dispatch_with_nothing_queued_reads_no_per_task_settings() {
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime();
    fixture
        .connection
        .execute_batch("DROP TABLE team_caps")
        .unwrap();
    let idle = team.dispatch(&fixture.setup, &fixture.access, &BTreeSet::new());
    assert!(idle.is_ok(), "an idle dispatch read the caps: {idle:?}");
    team.shutdown().unwrap();
}
