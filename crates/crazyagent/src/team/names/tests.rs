use super::*;
use rusqlite::params;
use std::collections::BTreeSet;

fn engine() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::team::initialize(&connection).unwrap();
    (directory, connection)
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
    set_task_role(&connection, &alpha, named, "Onboarding fix").unwrap();
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
    set_task_role(&connection, &alpha, ignored, "!!!").unwrap();
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
    set_task_role(&connection, &alpha, gone, "old-job").unwrap();
    connection
        .execute("DELETE FROM team_tasks WHERE id=?1", [gone])
        .unwrap();
    let kept = task(&connection, "alpha");
    set_task_role(&connection, &alpha, kept, "new-job").unwrap();
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
