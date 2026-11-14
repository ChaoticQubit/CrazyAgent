use super::*;
use std::io::Write;

const NOW: i64 = 1_793_000_000_000;

fn assistant(id: &str, input: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "message": {"id": id, "usage": {
                "input_tokens": input, "output_tokens": 1,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
            }}
        })
    )
}

fn append(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

#[test]
fn the_master_counts_as_its_projects_master_and_an_agent_without_an_account_is_unattributed() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER, hookless INTEGER NOT NULL DEFAULT 0);
         INSERT INTO team_agents VALUES (1,'alpha','master','claude-code','claude-a','idle',NULL,0);
         INSERT INTO team_agents VALUES (2,'alpha','sub_agent','claude-code',NULL,'working',NULL,0);
         INSERT INTO team_agents VALUES (3,'beta','master','claude-code','claude-a','stopped',0,0);",
    )
    .unwrap();
    for (id, input) in [(1, 50), (2, 7), (3, 1000)] {
        append(
            &team.join(format!("agents/agent-{id}/home/projects/f/s{id}.jsonl")),
            &assistant(&format!("m{id}"), input),
        );
    }
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    usage.step(NOW).unwrap();
    let rows: Vec<(String, String, String, String, i64)> = db
        .prepare(
            "SELECT kind,project,account,agent,SUM(input) FROM usage_tokens
             GROUP BY kind,project,account,agent ORDER BY agent",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "master".into(),
                "alpha".into(),
                "claude-a".into(),
                "master-alpha".into(),
                50
            ),
            (
                "agent".into(),
                "alpha".into(),
                String::new(),
                "team-2".into(),
                7
            ),
        ]
    );
}

#[test]
fn a_live_agent_on_a_harness_without_a_reader_is_marked_not_reported() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER, hookless INTEGER NOT NULL DEFAULT 0);
         INSERT INTO team_agents VALUES (1,'alpha','master','claude-code','claude-a','idle',NULL,0);
         INSERT INTO team_agents VALUES (2,'alpha','sub_agent','opencode',NULL,'working',NULL,0);
         INSERT INTO team_agents VALUES (3,'beta','sub_agent','gemini-cli',NULL,'stopped',0,0);",
    )
    .unwrap();
    append(
        &team.join("agents/agent-1/home/projects/f/s1.jsonl"),
        &assistant("m1", 50),
    );
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    usage.step(NOW).unwrap();
    usage.step(NOW + 1).unwrap();
    let report = crate::usage::report::report(&db, None, None, 0, NOW + 2, &[]).unwrap();
    assert_eq!(report.total.input, 50);
    assert_eq!(report.not_reported_by, ["OpenCode"]);
    assert_eq!(
        report
            .agents
            .iter()
            .map(|row| (row.key.as_str(), row.not_reported_by.as_deref()))
            .collect::<Vec<_>>(),
        [("master-alpha", None), ("team-2", Some("OpenCode"))],
        "a long-stopped agent is not scanned, so it is not marked"
    );
    let marks: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_unreported", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marks, 1, "two steps in one hour keep one mark");
}

#[test]
fn a_step_that_panics_ends_only_that_step_and_the_next_step_still_counts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER, hookless INTEGER NOT NULL DEFAULT 0);
         INSERT INTO team_agents VALUES (4,'alpha','sub_agent','claude-code',NULL,'working',NULL,0);
         CREATE TABLE held (n INTEGER);",
    )
    .unwrap();
    append(
        &team.join("agents/agent-4/home/projects/f/s4.jsonl"),
        &assistant("m4", 9),
    );
    let mut usage = Usage::open(
        &root.join("engine.db"),
        &team,
        Box::new(|| None),
        BTreeMap::new(),
    )
    .unwrap();
    let panicked = guarded(|| {
        let tx = usage.db.unchecked_transaction().map_err(storage)?;
        tx.execute("INSERT INTO held (n) VALUES (1)", [])
            .map_err(storage)?;
        panic!("a step broke an invariant");
    });
    assert_eq!(panicked.as_deref(), Some("usage_step_panicked"));
    let held: i64 = db
        .query_row("SELECT COUNT(*) FROM held", [], |row| row.get(0))
        .unwrap();
    assert_eq!(held, 0);
    assert_eq!(guarded(|| usage.step(NOW)), None);
    let counted: i64 = db
        .query_row(
            "SELECT SUM(input) FROM usage_tokens WHERE agent='team-4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(counted, 9);
    assert_eq!(
        guarded(|| Err(failure("usage_unavailable"))).as_deref(),
        Some("usage_unavailable")
    );
}

#[test]
fn a_step_that_keeps_panicking_is_logged_once_and_waits_longer_each_time() {
    let mut pace = Pace::default();
    let panicked = || Some(STEP_PANICKED.to_owned());
    let unavailable = || Some("usage_unavailable".to_owned());
    assert_eq!(pace.after(None), (Vec::new(), SCAN_EVERY));
    assert_eq!(
        pace.after(guarded(|| panic!("a step broke an invariant"))),
        (
            vec!["usage: usage_step_panicked".to_owned()],
            SCAN_EVERY * 2
        )
    );
    let pauses: Vec<Duration> = (0..10)
        .map(|_| {
            let (lines, pause) = pace.after(panicked());
            assert!(lines.is_empty(), "{lines:?}");
            pause
        })
        .collect();
    assert_eq!(
        pauses[..3],
        [SCAN_EVERY * 4, SCAN_EVERY * 8, SCAN_EVERY * 16]
    );
    assert!(pauses.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(pauses.last(), Some(&PANIC_PAUSE_MAX));
    assert_eq!(
        pace.after(None),
        (
            vec!["usage: 11 steps in a row panicked before this one".to_owned()],
            SCAN_EVERY
        )
    );

    assert_eq!(
        pace.after(unavailable()),
        (vec!["usage: usage_unavailable".to_owned()], SCAN_EVERY)
    );
    assert_eq!(pace.after(unavailable()), (Vec::new(), SCAN_EVERY));
    assert_eq!(
        pace.after(panicked()),
        (
            vec!["usage: usage_step_panicked".to_owned()],
            SCAN_EVERY * 2
        )
    );
    assert_eq!(
        pace.after(unavailable()),
        (vec!["usage: usage_unavailable".to_owned()], SCAN_EVERY),
        "one panic needs no count line"
    );
    for _ in 0..u32::BITS + 2 {
        pace.after(panicked());
    }
    assert_eq!(pace.after(panicked()).1, PANIC_PAUSE_MAX);
}

#[test]
fn a_panicking_step_kind_is_counted_marked_working_again_and_forgotten_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch(
        "CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT, role TEXT, harness TEXT,
            connection TEXT, status TEXT, stopped_ms INTEGER);",
    )
    .unwrap();
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    for at in [NOW, NOW + 5_000] {
        let problem = counted_step(
            &mut usage,
            |usage| {
                usage.doing = UsageStep::Team;
                panic!("a team scan broke an invariant");
            },
            at,
        );
        assert_eq!(problem.as_deref(), Some("usage_step_panicked"));
    }
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 10_000), NOW + 10_000),
        None
    );
    assert_eq!(
        usage.doing,
        UsageStep::Recount,
        "a whole step ends in its last part"
    );
    let report = crate::usage::report::report(&db, None, None, 0, NOW + 11_000, &[]).unwrap();
    assert_eq!(
        report.health,
        [crate::usage::health::StepHealth {
            step: UsageStep::Team,
            panics: 2,
            last_ms: NOW + 5_000,
            recovered_ms: Some(NOW + 10_000),
        }],
        "a later step that completes the kind marks it working again and keeps the count"
    );
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 15_000), NOW + 15_000),
        None
    );
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].recovered_ms,
        Some(NOW + 10_000),
        "the first time it worked again is kept"
    );
    let problem = counted_step(
        &mut usage,
        |usage| {
            usage.doing = UsageStep::Team;
            panic!("the team scan broke again");
        },
        NOW + 20_000,
    );
    assert_eq!(problem.as_deref(), Some("usage_step_panicked"));
    assert_eq!(
        crate::usage::health::health(&db).unwrap(),
        [crate::usage::health::StepHealth {
            step: UsageStep::Team,
            panics: 3,
            last_ms: NOW + 20_000,
            recovered_ms: None,
        }],
        "a new panic makes the kind failing again"
    );
    assert_eq!(
        counted_step(&mut usage, |usage| usage.step(NOW + 25_000), NOW + 25_000),
        None
    );
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].recovered_ms,
        Some(NOW + 25_000)
    );
    drop(usage);
    Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    assert!(
        crate::usage::health::health(&db).unwrap().is_empty(),
        "a new engine run starts with no counted panics"
    );
}

#[test]
fn a_recount_panic_is_charged_to_the_recount_and_the_live_step_goes_on() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    let mut usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    usage
        .guarded_recount(|_| panic!("the recount broke an invariant"), NOW)
        .unwrap();
    assert_eq!(usage.doing, UsageStep::Recount);
    let health = crate::usage::health::health(&db).unwrap();
    assert_eq!(
        (health[0].step, health[0].panics, health[0].recovered_ms),
        (UsageStep::Recount, 1, None)
    );
    usage.guarded_recount(|_| Ok(()), NOW + 5_000).unwrap();
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].recovered_ms,
        Some(NOW + 5_000)
    );
}

#[test]
fn an_older_health_table_takes_the_recount_step_after_an_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    db.execute_batch(
        "DROP TABLE usage_health;
         CREATE TABLE usage_health (step TEXT PRIMARY KEY CHECK(step IN ('poll','team','runs')),
            panics INTEGER NOT NULL CHECK(panics >= 1), last_ms INTEGER NOT NULL,
            recovered_ms INTEGER CHECK(recovered_ms IS NULL OR recovered_ms >= last_ms));",
    )
    .unwrap();
    drop(db);
    let db = crate::usage::open(root.path()).unwrap();
    record_panic(&db, UsageStep::Recount, NOW).unwrap();
    assert_eq!(
        crate::usage::health::health(&db).unwrap()[0].step,
        UsageStep::Recount
    );
}

fn no_access() -> ModelAccess {
    ModelAccess {
        accounts: Vec::new(),
        connections: Vec::new(),
        pools: Vec::new(),
    }
}

#[test]
fn a_removed_projects_research_run_is_recounted_under_its_removed_name() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    let home = root.join("research/run-1/home");
    append(&home.join("projects/f/s1.jsonl"), &assistant("m1", 30));
    db.execute(
        "INSERT INTO usage_runs (home,kind,project,account,agent,harness,started_ms)
         VALUES (?1,'research','alpha','acct','research-1','claude-code',?2)",
        rusqlite::params![home.to_string_lossy(), NOW],
    )
    .unwrap();
    db.execute(
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,input,output,cache_read,cache_write)
         VALUES (3600000,'research','alpha','','acct','research-1','claude-code','s1',400,1,0,0)",
        [],
    )
    .unwrap();
    crate::usage::forget_project(&db, "alpha").unwrap();
    let usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    for _ in 0..16 {
        usage.recount(&no_access(), NOW).unwrap();
    }
    let status = recount::status(&db).unwrap().unwrap();
    assert_eq!((status.old.input, status.new.input), (400, 30));
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (1, 0)
    );
}

#[test]
fn a_team_table_that_cannot_be_read_does_not_start_the_recount() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    let db = crate::usage::open(&root).unwrap();
    db.execute_batch("CREATE TABLE team_agents (id INTEGER PRIMARY KEY);")
        .unwrap();
    let usage = Usage::open(&database, &team, Box::new(|| None), BTreeMap::new()).unwrap();
    assert!(usage.recount(&no_access(), NOW).is_err());
    assert!(!recount::started(&db).unwrap());
}

/// A limit an agent reports asks for one confirm poll (SECAUDIT-RATELIMIT-FORGE, design row
/// 3): the next step polls every account although ten minutes have not passed, an ask within
/// the minute after it adds no poll, and an ask a minute later polls again.
#[test]
fn a_confirm_ask_makes_the_next_step_poll_and_asks_within_the_minute_add_none() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let mut usage = Usage::open(
        &root.join("engine.db"),
        &root.join("team"),
        Box::new(|| None),
        BTreeMap::new(),
    )
    .unwrap();
    usage.step(NOW).unwrap();
    assert_eq!(usage.polled_ms, Some(NOW));
    usage.step(NOW + 5_000).unwrap();
    assert_eq!(
        usage.polled_ms,
        Some(NOW),
        "no ask, no poll inside ten minutes"
    );
    assert!(request_confirm(&usage.db, NOW + 6_000).unwrap());
    usage.step(NOW + 10_000).unwrap();
    assert_eq!(
        usage.polled_ms,
        Some(NOW + 10_000),
        "the ask makes the next step poll"
    );
    assert!(!request_confirm(&usage.db, NOW + 20_000).unwrap());
    usage.step(NOW + 25_000).unwrap();
    assert_eq!(
        usage.polled_ms,
        Some(NOW + 10_000),
        "an ask within the minute adds no poll"
    );
    assert!(request_confirm(&usage.db, NOW + 6_000 + CONFIRM_EVERY_MS).unwrap());
    usage.step(NOW + 70_000).unwrap();
    assert_eq!(usage.polled_ms, Some(NOW + 70_000));
}

#[test]
fn a_harness_installed_after_engine_start_is_found_only_through_the_engines_lookup() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let database = root.join("engine.db");
    let team = root.join("team");
    drop(crate::usage::open(&root).unwrap());
    let codex = root.join("bin").join("codex");
    let given = root.join("given-claude");
    let usage = Usage::open(
        &database,
        &team,
        Box::new(|| None),
        BTreeMap::from([("claude-code", given.clone())]),
    )
    .unwrap();
    assert_eq!(usage.harness("codex"), None);
    assert_eq!(usage.harness("claude-code"), Some(given.clone()));
    let pinned = codex.clone();
    let usage = usage.with_harness_lookup(Arc::new(move |harness: &str| {
        (harness == "codex").then(|| pinned.clone())
    }));
    assert_eq!(usage.harness("codex"), Some(codex));
    assert_eq!(usage.harness("claude-code"), Some(given));
}

#[test]
fn an_open_pools_page_between_polls_makes_only_the_detail_reads() {
    let polled = Some(NOW - 60_000);
    assert_eq!(
        poll_plan(polled, None, true, NOW),
        PollPlan::DetailOnly,
        "a detail read between polls never polls every account again (LE-L1)"
    );
    assert_eq!(poll_plan(polled, None, false, NOW), PollPlan::Nothing);
    assert_eq!(
        poll_plan(Some(NOW - POLL_EVERY_MS), None, true, NOW),
        PollPlan::Every { detail: true }
    );
    assert_eq!(
        poll_plan(Some(NOW - POLL_EVERY_MS), None, false, NOW),
        PollPlan::Every { detail: false }
    );
    assert_eq!(
        poll_plan(None, None, false, NOW),
        PollPlan::Every { detail: false }
    );
    assert_eq!(
        poll_plan(polled, Some(NOW - 1), true, NOW),
        PollPlan::Every { detail: true },
        "an owner refresh polls every account, with the detail read riding along"
    );
    assert_eq!(
        poll_plan(polled, Some(NOW - 120_000), false, NOW),
        PollPlan::Nothing
    );
}

/// Each source picks the detail call's User-Agent: through Claude Code and as Claude Code
/// both name Claude Code's version (installed, else pinned) while no Claude Code version can
/// report free resets itself, and "Don't read" makes no detail call.
#[test]
fn each_claude_free_resets_source_picks_the_detail_user_agent_or_none() {
    use super::spend::ResetsSource;
    for source in [ResetsSource::ThroughClaudeCode, ResetsSource::AsClaudeCode] {
        assert_eq!(
            detail_agent(source, Some("2.1.330")).as_deref(),
            Some("claude-cli/2.1.330 (external, cli)"),
            "{source:?}"
        );
        assert_eq!(
            detail_agent(source, None).as_deref(),
            Some("claude-cli/2.1.322 (external, cli)"),
            "{source:?}"
        );
    }
    assert_eq!(detail_agent(ResetsSource::Off, Some("2.1.330")), None);
    assert_eq!(detail_agent(ResetsSource::Off, None), None);
}

/// "Don't read" forgets the stored free resets of every Claude account and only those, so
/// spend and the limited pool notice never act on an old count.
#[test]
fn dont_read_forgets_the_stored_claude_free_resets_and_keeps_codex() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    let account = |id: &str, harness: &str| ModelConnection {
        id: CredentialRef::new(id).unwrap(),
        provider: CredentialRef::new("vendor").unwrap(),
        harness: CredentialRef::new(harness).unwrap(),
        protocol: CredentialRef::new("native").unwrap(),
        account_ref: CredentialRef::new(id).unwrap(),
        endpoint: ModelEndpoint::Native {},
        model: crate::config::ModelReference {
            id: "e2e-model".into(),
            author: CredentialRef::new("vendor").unwrap(),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: CredentialRef::new(id).unwrap(),
        },
    };
    let access = ModelAccess {
        accounts: vec![
            CredentialRef::new("claude-a").unwrap(),
            CredentialRef::new("codex-a").unwrap(),
        ],
        connections: vec![account("claude-a", "claude"), account("codex-a", "codex")],
        pools: Vec::new(),
    };
    let two = Extras {
        resets: Field::Known(extras::Resets {
            count: 2,
            kinds: Vec::new(),
            next_expiry_ms: None,
        }),
        credits: Field::NotAsked,
    };
    for connection in &access.connections {
        extras::record_extras(&db, &connection.id, &two, NOW).unwrap();
    }
    forget_claude_resets(&db, &access).unwrap();
    let claude = extras::extras_of(&db, &access.connections[0].id, NOW).unwrap();
    assert_eq!(claude.resets, None);
    assert_eq!(claude.resets_unread, None);
    let codex = extras::extras_of(&db, &access.connections[1].id, NOW).unwrap();
    assert_eq!(codex.resets.map(|resets| resets.count), Some(2));
}

/// The detail call names the installed Claude Code, found under the engine's harness id
/// `claude-code`: its `--version` is read once per executable and again after the executable
/// changes, Don't read never asks it, and an executable that prints no version names the
/// pinned one (DEC-154 addendum, secreview r1 F1).
#[cfg(unix)]
#[test]
fn the_detail_user_agent_names_the_installed_claude_code_and_reads_it_again_after_an_update() {
    use super::spend::ResetsSource;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let team = root.path().join("team");
    std::fs::create_dir(&team).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    let runs = bin.join("runs");
    let install = |printed: &str, at: std::time::SystemTime| {
        let script = format!(
            "#!/bin/sh\necho x >> '{}'\necho '{printed}'\n",
            runs.display()
        );
        std::fs::write(&claude, script).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&claude)
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    let ran = || std::fs::read_to_string(&runs).map_or(0, |text| text.lines().count());
    let start = std::time::SystemTime::now();
    install("2.1.330 (Claude Code)", start);
    let mut usage = Usage::open(
        &root.path().join("engine.db"),
        &team,
        Box::new(|| None),
        BTreeMap::from([("claude-code", claude.clone())]),
    )
    .unwrap();
    for source in [ResetsSource::ThroughClaudeCode, ResetsSource::AsClaudeCode] {
        assert_eq!(
            usage.detail_user_agent(source).as_deref(),
            Some("claude-cli/2.1.330 (external, cli)"),
            "{source:?}"
        );
    }
    assert_eq!(ran(), 1, "one executable is asked once");
    assert_eq!(usage.detail_user_agent(ResetsSource::Off), None);
    assert_eq!(ran(), 1, "Don't read never asks");
    install("2.1.331 (Claude Code)", start + Duration::from_secs(60));
    assert_eq!(
        usage
            .detail_user_agent(ResetsSource::ThroughClaudeCode)
            .as_deref(),
        Some("claude-cli/2.1.331 (external, cli)")
    );
    assert_eq!(ran(), 2, "an updated executable is asked again");
    install("Claude Code", start + Duration::from_secs(120));
    assert_eq!(
        usage
            .detail_user_agent(ResetsSource::AsClaudeCode)
            .as_deref(),
        Some("claude-cli/2.1.322 (external, cli)")
    );
}
