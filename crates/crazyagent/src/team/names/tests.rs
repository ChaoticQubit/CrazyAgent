use super::*;
use rusqlite::params;
use std::collections::BTreeSet;

fn engine() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::team::initialize(&connection).unwrap();
    (directory, connection)
}

fn give_role(
    connection: &Connection,
    project: &ProjectId,
    task: i64,
    role: &str,
    expected: u64,
) -> Result<u64, versioned::Stale> {
    let tx = connection.unchecked_transaction().unwrap();
    let done = set_task_role(&tx, project, task, role, expected).unwrap();
    tx.commit().unwrap();
    done
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn agent(connection: &Connection, project: &str, role: &str, status: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,?2,'claude-code',1,?3,0,0)",
            params![project, role, status],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn task(connection: &Connection, project: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'master','claude-code',1,'Fix the tests','queued',0)",
            [project],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn stop(connection: &Connection, agent: i64) {
    connection
        .execute(
            "UPDATE team_agents SET status='stopped' WHERE id=?1",
            [agent],
        )
        .unwrap();
}

fn stored_name(connection: &Connection, agent: i64) -> Option<AgentName> {
    connection
        .query_row(
            "SELECT persona,handle FROM team_names WHERE agent=?1",
            [agent],
            |row| {
                Ok(AgentName {
                    persona: row.get(0)?,
                    handle: row.get(1)?,
                })
            },
        )
        .optional()
        .unwrap()
}

#[test]
fn personas_are_distinct_plain_words_that_outnumber_a_project_s_agents() {
    let distinct: BTreeSet<String> = PERSONAS
        .iter()
        .map(|persona| persona.to_ascii_lowercase())
        .collect();
    assert_eq!(distinct.len(), PERSONAS.len());
    assert!(PERSONAS.len() > crate::terminal::PANES_PER_PROJECT_MAX);
    for persona in PERSONAS {
        assert!(persona.chars().all(|c| c.is_ascii_alphabetic()));
        assert!(handle(persona).is_some_and(|handle| handle != MASTER_HANDLE));
    }
}

#[test]
fn every_live_sub_agent_gets_a_distinct_persona_that_stays_for_its_life() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let ids: Vec<i64> = (0..crate::terminal::PANES_PER_PROJECT_MAX)
        .map(|_| agent(&connection, "alpha", "sub", "working"))
        .collect();
    let personas: Vec<String> = ids
        .iter()
        .map(|id| assign_persona(&connection, &alpha, *id).unwrap())
        .collect();
    let distinct: BTreeSet<&String> = personas.iter().collect();
    assert_eq!(distinct.len(), ids.len());
    for (id, persona) in ids.iter().zip(&personas) {
        assert_eq!(&assign_persona(&connection, &alpha, *id).unwrap(), persona);
        assert_eq!(&stored_name(&connection, *id).unwrap().persona, persona);
    }
    let other = agent(&connection, "beta", "sub", "working");
    assert_eq!(
        assign_persona(&connection, &project("beta"), other).unwrap(),
        PERSONAS[0]
    );
}

#[test]
fn a_new_agent_prefers_a_persona_no_agent_of_the_project_ever_held() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let first = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_persona(&connection, &alpha, first).unwrap(),
        PERSONAS[0]
    );
    stop(&connection, first);
    let second = agent(&connection, "alpha", "sub", "starting");
    assert_eq!(
        assign_persona(&connection, &alpha, second).unwrap(),
        PERSONAS[1]
    );
    connection
        .execute(
            "UPDATE team_agents SET status='starting' WHERE id=?1",
            [first],
        )
        .unwrap();
    assert_eq!(
        assign_persona(&connection, &alpha, first).unwrap(),
        PERSONAS[0]
    );
}

#[test]
fn a_revived_agent_whose_persona_a_live_agent_now_holds_gets_another() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let first = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_persona(&connection, &alpha, first).unwrap(),
        PERSONAS[0]
    );
    stop(&connection, first);
    let second = agent(&connection, "alpha", "sub", "working");
    connection
        .execute(
            "INSERT INTO team_names(agent,project,persona) VALUES (?1,'alpha',?2)",
            params![second, PERSONAS[0]],
        )
        .unwrap();
    assert_eq!(
        assign_persona(&connection, &alpha, second).unwrap(),
        PERSONAS[0]
    );
    connection
        .execute("UPDATE team_agents SET status='idle' WHERE id=?1", [first])
        .unwrap();
    let revived = assign_persona(&connection, &alpha, first).unwrap();
    assert_ne!(revived, PERSONAS[0]);
    assert_eq!(stored_name(&connection, first).unwrap().persona, revived);
}

#[test]
fn a_persona_is_refused_for_the_master_or_an_agent_of_another_project() {
    let (_directory, connection) = engine();
    let master = agent(&connection, "alpha", "master", "working");
    let sub = agent(&connection, "beta", "sub", "working");
    assert!(assign_persona(&connection, &project("alpha"), master).is_err());
    assert!(assign_persona(&connection, &project("alpha"), sub).is_err());
    assert!(stored_name(&connection, master).is_none());
}

#[test]
fn handles_become_short_kebab_case_or_nothing() {
    assert_eq!(handle("onboarding-fix").as_deref(), Some("onboarding-fix"));
    assert_eq!(
        handle(" @Onboarding Fix ").as_deref(),
        Some("onboarding-fix")
    );
    assert_eq!(
        handle("fix_the.tests/now").as_deref(),
        Some("fix-the-tests-now")
    );
    assert_eq!(handle("--a  --  b--").as_deref(), Some("a-b"));
    assert_eq!(handle("2fa").as_deref(), Some("2fa"));
    assert_eq!(handle("café ünïcode").as_deref(), Some("caf-ncode"));
    assert_eq!(handle("@"), None);
    assert_eq!(handle("!!! ???"), None);
    assert_eq!(handle(""), None);
    let long = handle(&"word-".repeat(40)).unwrap();
    assert!(long.len() <= HANDLE_CHARS_MAX);
    assert!(!long.ends_with('-'));
    assert_eq!(long, "word-word-word-word-word-word-wo");
}

/// A handle never reads as an agent number a master names agents by (RV-DS-62): bare digits,
/// `team-<n>`, `sub-agent-<n>` and `<harness>-<n>` are refused as roles and renames, and a
/// taken role is never suffixed into one.
#[test]
fn a_handle_never_reads_as_an_agent_number() {
    for numbered in [
        "7",
        "@7",
        "team-7",
        "Team 7",
        "sub-agent-7",
        "Sub-agent 7",
        "codex-7",
        "@Codex-7",
        "claude-code-12",
    ] {
        assert_eq!(handle(numbered), None, "{numbered}");
    }
    for kept in [
        "onboarding-fix-2",
        "task-1",
        "v2",
        "2fa",
        "codex",
        "team",
        "7up",
    ] {
        assert!(handle(kept).is_some(), "{kept}");
    }
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let first = agent(&connection, "alpha", "sub", "working");
    let second = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_role(&connection, &alpha, first, "codex").unwrap(),
        Ok("codex".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, second, "codex").unwrap(),
        Err("team_handle_taken"),
        "codex-2 would read as agent 2"
    );
    assert_eq!(
        assign_role(&connection, &alpha, second, "7").unwrap(),
        Err("team_handle_invalid")
    );
    assert_eq!(stored_name(&connection, second).unwrap().handle, None);
}

/// A handle kept in a numbered form from before such handles were refused (RV-DS-62) is
/// cleared when the engine opens its database (PD-1): agent 3 kept as `Ada (@7)` would read
/// that way in every roster while `@7` now reaches agent 7. Its label is then its persona
/// alone; every other handle stays.
#[test]
fn a_numbered_handle_kept_from_before_is_cleared_when_the_engine_opens() {
    let (directory, connection) = engine();
    let mut legacy = Vec::new();
    for (persona, handle) in [
        ("Ada", "7"),
        ("Grace", "codex-9"),
        ("Alan", "team-4"),
        ("Edsger", "sub-agent-5"),
    ] {
        let id = agent(&connection, "alpha", "sub", "working");
        connection
            .execute(
                "INSERT INTO team_names(agent,project,persona,handle) VALUES (?1,'alpha',?2,?3)",
                params![id, persona, handle],
            )
            .unwrap();
        legacy.push((id, persona));
    }
    let kept = agent(&connection, "alpha", "sub", "working");
    connection
        .execute(
            "INSERT INTO team_names(agent,project,persona,handle) VALUES (?1,'alpha','Barbara','onboarding-fix-2')",
            [kept],
        )
        .unwrap();
    drop(connection);
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::team::initialize(&connection).unwrap();
    for (id, persona) in legacy {
        assert_eq!(
            stored_name(&connection, id),
            Some(AgentName {
                persona: persona.into(),
                handle: None
            }),
            "{persona}"
        );
        assert_eq!(
            label(AgentRole::Sub, id, name(&connection, id).unwrap().as_ref()),
            persona
        );
    }
    assert_eq!(
        label(
            AgentRole::Sub,
            kept,
            name(&connection, kept).unwrap().as_ref()
        ),
        "Barbara (@onboarding-fix-2)"
    );
}

#[test]
fn a_role_handle_is_unique_among_the_project_s_live_agents() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let first = agent(&connection, "alpha", "sub", "working");
    let second = agent(&connection, "alpha", "sub", "working");
    let third = agent(&connection, "alpha", "sub", "working");
    let elsewhere = agent(&connection, "beta", "sub", "working");
    assert_eq!(
        assign_role(&connection, &alpha, first, "Fix Tests").unwrap(),
        Ok("fix-tests".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, first, "fix-tests").unwrap(),
        Ok("fix-tests".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, second, "@fix-tests").unwrap(),
        Ok("fix-tests-2".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, third, "fix-tests").unwrap(),
        Ok("fix-tests-3".into())
    );
    assert_eq!(
        assign_role(&connection, &project("beta"), elsewhere, "fix-tests").unwrap(),
        Ok("fix-tests".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, third, "master").unwrap(),
        Ok("master-2".into())
    );
    assert_eq!(
        assign_role(&connection, &alpha, third, "???").unwrap(),
        Err("team_handle_invalid")
    );
    assert_eq!(
        stored_name(&connection, third).unwrap().handle.as_deref(),
        Some("master-2")
    );
    stop(&connection, first);
    assert_eq!(
        assign_role(&connection, &alpha, third, "fix-tests").unwrap(),
        Ok("fix-tests".into())
    );
    assert!(stored_name(&connection, first).is_some_and(|name| !name.persona.is_empty()));
}

#[test]
fn a_long_role_keeps_its_suffix_within_the_handle_bound() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let role = "a".repeat(HANDLE_CHARS_MAX);
    let first = agent(&connection, "alpha", "sub", "working");
    let second = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_role(&connection, &alpha, first, &role).unwrap(),
        Ok(role.clone())
    );
    let suffixed = assign_role(&connection, &alpha, second, &role)
        .unwrap()
        .unwrap();
    assert_eq!(suffixed.len(), HANDLE_CHARS_MAX);
    assert!(suffixed.ends_with("-2"));
}

#[test]
fn a_task_role_becomes_the_handle_and_a_new_job_without_one_gets_its_task_handle() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let sub = agent(&connection, "alpha", "sub", "working");
    let named = task(&connection, "alpha");
    give_role(&connection, &alpha, named, "Onboarding fix", 0).unwrap();
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, named).unwrap(),
        Some("onboarding-fix".into())
    );
    assert_eq!(
        stored_name(&connection, sub).unwrap().handle.as_deref(),
        Some("onboarding-fix")
    );
    let plain = task(&connection, "alpha");
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, plain).unwrap(),
        Some(format!("task-{plain}"))
    );
    let pinned = task(&connection, "alpha");
    connection
        .execute(
            "INSERT INTO team_task_pins(task,project,agent) VALUES (?1,'alpha',?2)",
            params![pinned, sub],
        )
        .unwrap();
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, pinned).unwrap(),
        Some(format!("task-{plain}"))
    );
    let ignored = task(&connection, "alpha");
    give_role(&connection, &alpha, ignored, "!!!", 0).unwrap();
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, ignored).unwrap(),
        Some(format!("task-{ignored}"))
    );
}

#[test]
fn task_roles_of_tasks_that_no_longer_exist_are_dropped() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let gone = task(&connection, "alpha");
    give_role(&connection, &alpha, gone, "old-job", 0).unwrap();
    connection
        .execute("DELETE FROM team_tasks WHERE id=?1", [gone])
        .unwrap();
    let kept = task(&connection, "alpha");
    give_role(&connection, &alpha, kept, "new-job", 0).unwrap();
    let roles: Vec<i64> = connection
        .prepare("SELECT task FROM team_task_roles ORDER BY task")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(roles, vec![kept]);
}

#[test]
fn the_owner_renames_a_live_sub_agent_s_handle() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let first = agent(&connection, "alpha", "sub", "working");
    let second = agent(&connection, "alpha", "sub", "idle");
    let master = agent(&connection, "alpha", "master", "working");
    assign_role(&connection, &alpha, first, "fix-tests")
        .unwrap()
        .unwrap();
    assert_eq!(
        rename(&connection, &alpha, second, "@docs-pass").unwrap(),
        Ok("docs-pass".into())
    );
    assert_eq!(
        stored_name(&connection, second).unwrap().handle.as_deref(),
        Some("docs-pass")
    );
    assert_eq!(
        rename(&connection, &alpha, second, "fix-tests").unwrap(),
        Err("team_handle_taken")
    );
    assert_eq!(
        rename(&connection, &alpha, second, "master").unwrap(),
        Err("team_handle_taken")
    );
    for bad in [
        "Docs Pass",
        "docs_pass",
        "-docs",
        "docs-",
        "",
        "@",
        &"a".repeat(33),
    ] {
        assert_eq!(
            rename(&connection, &alpha, second, bad).unwrap(),
            Err("team_handle_invalid"),
            "{bad}"
        );
    }
    assert_eq!(
        rename(&connection, &alpha, master, "lead").unwrap(),
        Err("team_agent_unknown")
    );
    assert_eq!(
        rename(&connection, &project("beta"), first, "lead").unwrap(),
        Err("team_agent_unknown")
    );
    assert_eq!(
        rename(&connection, &alpha, 9999, "lead").unwrap(),
        Err("team_agent_unknown")
    );
    assert_eq!(
        stored_name(&connection, second).unwrap().handle.as_deref(),
        Some("docs-pass")
    );
}

/// A handle shaped like an agent's id form, `team-7`, `<harness>-7` or `7`, is refused from
/// the owner and never given from a role, so no handle can stand for another agent's id.
#[test]
fn no_handle_takes_the_shape_of_an_agent_id() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let sub = agent(&connection, "alpha", "sub", "working");
    for shaped in [
        "team-7",
        "@team-7",
        "7",
        "codex-7",
        "claude-code-12",
        "team-07",
    ] {
        assert_eq!(
            rename(&connection, &alpha, sub, shaped).unwrap(),
            Err("team_handle_invalid"),
            "{shaped}"
        );
    }
    for kept in ["task-7", "team-7a", "codex"] {
        assert_eq!(
            rename(&connection, &alpha, sub, kept).unwrap(),
            Ok(kept.to_owned()),
            "{kept}"
        );
    }
    assert_eq!(
        assign_role(&connection, &alpha, sub, "Team 7").unwrap(),
        Err("team_handle_invalid")
    );
    let delegated = task(&connection, "alpha");
    give_role(&connection, &alpha, delegated, "Team 7", 0).unwrap();
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, delegated).unwrap(),
        Some(format!("task-{delegated}"))
    );
    let other = agent(&connection, "alpha", "sub", "idle");
    assign_role(&connection, &alpha, other, "codex")
        .unwrap()
        .unwrap();
    assert_eq!(
        assign_role(&connection, &alpha, sub, "codex").unwrap(),
        Err("team_handle_taken"),
        "a taken role never falls back to an id form such as codex-2"
    );
}

#[test]
fn resolve_finds_a_live_agent_by_handle_or_persona_within_its_project() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let master = agent(&connection, "alpha", "master", "idle");
    let sub = agent(&connection, "alpha", "sub", "working");
    let persona = assign_persona(&connection, &alpha, sub).unwrap();
    assign_role(&connection, &alpha, sub, "onboarding-fix")
        .unwrap()
        .unwrap();
    let elsewhere = agent(&connection, "beta", "sub", "working");
    assign_role(&connection, &project("beta"), elsewhere, "docs")
        .unwrap()
        .unwrap();
    for text in [
        "@onboarding-fix",
        " @Onboarding-Fix ",
        "onboarding-fix",
        persona.as_str(),
        &persona.to_ascii_uppercase(),
    ] {
        assert_eq!(
            resolve(&connection, &alpha, text).unwrap(),
            Some(sub),
            "{text}"
        );
    }
    assert_eq!(
        resolve(&connection, &alpha, "@master").unwrap(),
        Some(master)
    );
    assert_eq!(
        resolve(&connection, &alpha, "Master").unwrap(),
        Some(master)
    );
    for text in [
        "@docs",
        "docs",
        "@nobody",
        "",
        "@",
        "Zelda",
        &"x".repeat(200),
    ] {
        assert_eq!(resolve(&connection, &alpha, text).unwrap(), None, "{text}");
    }
    stop(&connection, sub);
    assert_eq!(
        resolve(&connection, &alpha, "@onboarding-fix").unwrap(),
        None
    );
    assert_eq!(resolve(&connection, &alpha, &persona).unwrap(), None);
    stop(&connection, master);
    assert_eq!(resolve(&connection, &alpha, "@master").unwrap(), None);
}

#[test]
fn labels_read_persona_role_harness_and_status() {
    let named = AgentName {
        persona: "Ada".into(),
        handle: Some("onboarding-fix".into()),
    };
    assert_eq!(
        label(AgentRole::Sub, 7, Some(&named)),
        "Ada (@onboarding-fix)"
    );
    let unassigned = AgentName {
        persona: "Grace".into(),
        handle: None,
    };
    assert_eq!(label(AgentRole::Sub, 7, Some(&unassigned)), "Grace");
    assert_eq!(label(AgentRole::Sub, 7, None), "Sub-agent 7");
    assert_eq!(label(AgentRole::Master, 1, Some(&named)), "Master");
    assert_eq!(
        line("Ada (@onboarding-fix)", "claude-code", "Working"),
        "Ada (@onboarding-fix) \u{b7} Claude Code \u{b7} working"
    );
    assert_eq!(
        line("Grace", "codex", "Waiting for work"),
        "Grace \u{b7} Codex \u{b7} waiting for work"
    );
    assert_eq!(
        line("Sub-agent 3", "mystery-tool", "Stopped"),
        "Sub-agent 3 \u{b7} mystery-tool \u{b7} stopped"
    );
}

fn stored_role(connection: &Connection, task: i64) -> Option<String> {
    connection
        .query_row(
            "SELECT role FROM team_task_roles WHERE task=?1",
            [task],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn stale_task_role_write_is_refused() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let master = agent(&connection, "alpha", "master", "working");
    let delegated = task(&connection, "alpha");
    assert_eq!(
        give_role(&connection, &alpha, delegated, "Onboarding fix", 0),
        Ok(1),
        "the master names the role of a task it just created at version 0"
    );
    let by_master = versioned::LastWriter::Agent(AgentId::team(master).unwrap());
    for version in [0, 4] {
        assert_eq!(
            give_role(&connection, &alpha, delegated, "Release notes", version),
            Err(versioned::Stale {
                version,
                current: 1,
                writer: by_master.clone(),
            }),
            "a role write from version {version} never replaces version 1"
        );
    }
    assert_eq!(
        stored_role(&connection, delegated).as_deref(),
        Some("Onboarding fix"),
        "the stale write changed nothing"
    );
    assert_eq!(
        give_role(&connection, &alpha, delegated, "Release notes", 1),
        Ok(2),
        "after a re-read the redo lands"
    );
    assert_eq!(
        stored_role(&connection, delegated).as_deref(),
        Some("Release notes")
    );

    connection
        .execute("DELETE FROM team_tasks WHERE id=?1", [delegated])
        .unwrap();
    let next = task(&connection, "alpha");
    assert!(next > delegated, "task ids are never reused");
    assert_eq!(give_role(&connection, &alpha, next, "Docs", 0), Ok(1));
    assert_eq!(stored_role(&connection, delegated), None);
    assert_eq!(
        versioned::current(&connection, &versioned::task_role_item(delegated))
            .unwrap()
            .version,
        0,
        "a gone task's role forgets its version"
    );

    let beta = project("beta");
    let unmastered = task(&connection, "beta");
    assert_eq!(give_role(&connection, &beta, unmastered, "Docs", 0), Ok(1));
    assert_eq!(
        versioned::current(&connection, &versioned::task_role_item(unmastered))
            .unwrap()
            .writer,
        versioned::LastWriter::Owner,
        "without a recorded master the owner's turn wrote it"
    );
}

/// A woken sub-agent gets its names back (WK-R2): its persona, and its role handle while it
/// is free; when a live agent took the handle while it was stopped, the woken agent gets the
/// first free suffix and the live one keeps its own, so no two live agents share a handle.
#[test]
fn a_woken_agent_reclaims_its_names_and_never_shares_a_live_handle() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let ada = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_role(&connection, &alpha, ada, "onboarding-fix").unwrap(),
        Ok("onboarding-fix".into())
    );
    let persona = stored_name(&connection, ada).unwrap().persona;
    stop(&connection, ada);
    let newer = agent(&connection, "alpha", "sub", "working");
    assert_eq!(
        assign_role(&connection, &alpha, newer, "onboarding-fix").unwrap(),
        Ok("onboarding-fix".into())
    );
    assert_eq!(
        resolve_finished(&connection, &alpha, "@onboarding-fix").unwrap(),
        Some(ada)
    );
    assert_eq!(
        resolve(&connection, &alpha, "@onboarding-fix").unwrap(),
        Some(newer)
    );

    connection
        .execute(
            "UPDATE team_agents SET status='starting' WHERE id=?1",
            [ada],
        )
        .unwrap();
    assert_eq!(
        reclaim(&connection, &alpha, ada).unwrap(),
        AgentName {
            persona: persona.clone(),
            handle: Some("onboarding-fix-2".into()),
        }
    );
    assert_eq!(
        stored_name(&connection, newer).unwrap().handle.as_deref(),
        Some("onboarding-fix"),
        "the live agent keeps the handle its master knows"
    );
    assert_eq!(
        resolve(&connection, &alpha, "@onboarding-fix-2").unwrap(),
        Some(ada)
    );
    assert_eq!(
        resolve(&connection, &alpha, "@onboarding-fix").unwrap(),
        Some(newer)
    );

    stop(&connection, newer);
    stop(&connection, ada);
    connection
        .execute(
            "UPDATE team_agents SET status='starting' WHERE id=?1",
            [newer],
        )
        .unwrap();
    assert_eq!(
        reclaim(&connection, &alpha, newer)
            .unwrap()
            .handle
            .as_deref(),
        Some("onboarding-fix"),
        "a free handle is kept as it was"
    );
}

/// The handle registry keeps a handle pointing at the agent that last held it (F16): a
/// sub-agent reused for a later job, or renamed by the owner, is still reached by its
/// earlier handle, live or finished, and a new holder of a handle takes it over.
#[test]
fn an_earlier_handle_reaches_its_agent_after_a_new_job_or_a_rename_until_another_takes_it() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let sub = agent(&connection, "alpha", "sub", "working");
    let first = task(&connection, "alpha");
    give_role(&connection, &alpha, first, "classifier-storage", 0).unwrap();
    assign_task_role(&connection, &alpha, sub, first).unwrap();
    let second = task(&connection, "alpha");
    give_role(&connection, &alpha, second, "storage-api", 0).unwrap();
    assert_eq!(
        assign_task_role(&connection, &alpha, sub, second).unwrap(),
        Some("storage-api".into())
    );
    assert_eq!(
        resolve(&connection, &alpha, "@classifier-storage").unwrap(),
        Some(sub),
        "the earlier job's handle still reaches the reused agent"
    );
    assert_eq!(
        rename(&connection, &alpha, sub, "store").unwrap(),
        Ok("store".into())
    );
    for earlier in ["@storage-api", "@classifier-storage", "storage-api"] {
        assert_eq!(
            resolve(&connection, &alpha, earlier).unwrap(),
            Some(sub),
            "{earlier}"
        );
    }
    assert_eq!(
        resolve(&connection, &project("beta"), "@storage-api").unwrap(),
        None,
        "the registry never reaches into another project"
    );
    stop(&connection, sub);
    assert_eq!(resolve(&connection, &alpha, "@storage-api").unwrap(), None);
    assert_eq!(
        resolve_finished(&connection, &alpha, "@storage-api").unwrap(),
        Some(sub),
        "a finished agent is found by an earlier handle for a wake"
    );
    let newer = agent(&connection, "alpha", "sub", "working");
    assign_role(&connection, &alpha, newer, "storage-api")
        .unwrap()
        .unwrap();
    assert_eq!(
        resolve(&connection, &alpha, "@storage-api").unwrap(),
        Some(newer)
    );
    stop(&connection, newer);
    assert_eq!(
        resolve_finished(&connection, &alpha, "@storage-api").unwrap(),
        Some(newer),
        "the newest holder of a handle takes it over"
    );
    let holder: i64 = connection
        .query_row(
            "SELECT agent FROM team_handles WHERE project='alpha' AND handle='storage-api'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(holder, newer);
}

/// A handle the master gave a task that has not started yet names that task, and stops
/// doing so once the task starts (F16, DM "@classifier-storage not recognized").
#[test]
fn a_waiting_task_s_role_names_that_task_until_it_starts() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let queued = task(&connection, "alpha");
    give_role(&connection, &alpha, queued, "Classifier storage", 0).unwrap();
    for name in ["@classifier-storage", "classifier-storage", " @Classifier-Storage "] {
        assert_eq!(
            waiting_task(&connection, &alpha, name).unwrap(),
            Some(queued),
            "{name}"
        );
    }
    for name in ["@other", "", "@", &"x".repeat(200)] {
        assert_eq!(waiting_task(&connection, &alpha, name).unwrap(), None, "{name}");
    }
    assert_eq!(
        waiting_task(&connection, &project("beta"), "@classifier-storage").unwrap(),
        None
    );
    connection
        .execute(
            "UPDATE team_tasks SET status='running' WHERE id=?1",
            [queued],
        )
        .unwrap();
    assert_eq!(
        waiting_task(&connection, &alpha, "@classifier-storage").unwrap(),
        None
    );
}

/// A retired handle is one of a finished sub-agent whose worktree is gone and that no live
/// agent holds; the listing is bounded (F10, F16).
#[test]
fn retired_handles_are_finished_agents_without_a_worktree_that_no_live_agent_holds() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let worktree = |agent: i64| {
        connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,'alpha','.','.','.git')",
                [agent],
            )
            .unwrap();
    };
    let gone = agent(&connection, "alpha", "sub", "working");
    assign_role(&connection, &alpha, gone, "core-policy")
        .unwrap()
        .unwrap();
    stop(&connection, gone);
    let kept = agent(&connection, "alpha", "sub", "working");
    assign_role(&connection, &alpha, kept, "docs").unwrap().unwrap();
    worktree(kept);
    stop(&connection, kept);
    let running = agent(&connection, "alpha", "sub", "working");
    assign_role(&connection, &alpha, running, "web").unwrap().unwrap();
    assert_eq!(
        retired(&connection, &alpha).unwrap(),
        vec![("core-policy".to_owned(), gone)]
    );
    let retaken = agent(&connection, "alpha", "sub", "working");
    assign_role(&connection, &alpha, retaken, "core-policy")
        .unwrap()
        .unwrap();
    assert!(
        retired(&connection, &alpha).unwrap().is_empty(),
        "a handle a live agent holds again is not retired"
    );
    assert!(retired(&connection, &project("beta")).unwrap().is_empty());
}

/// The registry keeps at most `HANDLES_KEPT_MAX` handles per project, the newest, and none
/// of an agent that no longer exists.
#[test]
fn the_handle_registry_keeps_its_newest_handles_within_the_bound() {
    let (_directory, connection) = engine();
    let alpha = project("alpha");
    let sub = agent(&connection, "alpha", "sub", "working");
    for at in 0..=HANDLES_KEPT_MAX {
        assign_role(&connection, &alpha, sub, &format!("job-{at}a"))
            .unwrap()
            .unwrap();
    }
    let count = |connection: &Connection| -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM team_handles WHERE project='alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(count(&connection), HANDLES_KEPT_MAX as i64);
    assert_eq!(
        resolve(&connection, &alpha, &format!("@job-{HANDLES_KEPT_MAX}a")).unwrap(),
        Some(sub)
    );
    assert_eq!(resolve(&connection, &alpha, "@job-0a").unwrap(), None);
    let other = agent(&connection, "alpha", "sub", "working");
    connection
        .execute("DELETE FROM team_agents WHERE id=?1", [sub])
        .unwrap();
    assign_role(&connection, &alpha, other, "fresh").unwrap().unwrap();
    assert_eq!(count(&connection), 1, "rows of a deleted agent are dropped");
}
