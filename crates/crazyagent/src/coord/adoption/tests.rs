use super::*;
use serde_json::json;

const NOW: i64 = 1_800_000_000_000;

fn database() -> (tempfile::TempDir, Connection, ProjectId) {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let project = ProjectId::new("adopt").unwrap();
    let body = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: project.clone(),
        spec: crate::protocol::ProjectSpec {
            name: "Adopt".into(),
            repositories: vec![],
        },
    })
    .unwrap();
    db.execute(
        "INSERT INTO projects(id, body) VALUES(?1, ?2)",
        params![project.as_str(), body],
    )
    .unwrap();
    (root, db, project)
}

/// One team agent row: its role, status, whether it runs with tools, when it started and
/// whether its launch fires no hooks.
fn agent_row(
    db: &Connection,
    project: &ProjectId,
    status: &str,
    tools: bool,
    created: i64,
    hookless: bool,
) -> AgentId {
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
         VALUES (?1,'sub','claude-code',?2,?3,?4,?4,?5)",
        params![project.as_str(), tools, status, created, hookless],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn call(db: &Connection, agent: AgentId, name: &str, input: &Value, times: usize) {
    let tool = tool_key(name, input).unwrap();
    for _ in 0..times {
        crate::coord::record_tool(db, agent, &tool, NOW).unwrap();
    }
    if let Some(skill) = skill_key(name, input) {
        for _ in 0..times {
            crate::coord::record_tool(db, agent, &skill, NOW).unwrap();
        }
    }
}

fn row(view: &Adoption, agent: AgentId) -> &AgentAdoption {
    view.agents.iter().find(|row| row.agent == agent).unwrap()
}

#[test]
fn tool_key_names_mcp_shell_graphify_and_skill_calls() {
    let key = |name: &str, input: Value| tool_key(name, &input);
    assert_eq!(
        key("mcp__context-mode__ctx_execute", json!({})).as_deref(),
        Some("ctx_execute")
    );
    assert_eq!(
        key(
            "mcp__plugin_context-mode_context-mode__ctx_batch_execute",
            json!({})
        )
        .as_deref(),
        Some("ctx_batch_execute")
    );
    assert_eq!(
        key("mcp__graphify__query_graph", json!({})).as_deref(),
        Some("graphify")
    );
    for (name, input) in [
        (
            "Bash",
            json!({"command": "graphify query \"who calls land\""}),
        ),
        (
            "Bash",
            json!({"command": "cd /repo && /usr/local/bin/graphify update ."}),
        ),
        ("exec_command", json!({"cmd": "graphify path A B"})),
        ("shell_command", json!({"command": "(graphify explain x)"})),
        (
            "shell",
            json!({"command": ["bash", "-lc", "graphify query x"]}),
        ),
        (
            "local_shell_call",
            json!({"command": ["sh", "-c", "true; graphify query y"]}),
        ),
    ] {
        assert_eq!(
            key(name, input.clone()).as_deref(),
            Some("graphify"),
            "{input}"
        );
    }
    assert_eq!(
        key(
            "Bash",
            json!({"command": "cat ~/.claude/skills/tdd/SKILL.md"})
        )
        .as_deref(),
        Some("Skill")
    );
    assert_eq!(
        key("Bash", json!({"command": "graphifyish query x"})).as_deref(),
        Some("Bash")
    );
    assert_eq!(
        key("Bash", json!({"command": "ls -la"})).as_deref(),
        Some("Bash")
    );
    assert_eq!(key("Edit", json!({})).as_deref(), Some("Edit"));
    assert_eq!(
        key("apply_patch", json!({})).as_deref(),
        Some("apply_patch")
    );
    assert_eq!(key("bad name", json!({})), None);
    assert_eq!(key("", json!({})), None);
    assert_eq!(key(&"x".repeat(TOOL_BYTES_MAX + 1), json!({})), None);
    assert_eq!(
        key(&"x".repeat(TOOL_BYTES_MAX), json!({})).map(|key| key.len()),
        Some(TOOL_BYTES_MAX)
    );
    let long = format!("{} graphify query x", "a".repeat(COMMAND_BYTES_MAX));
    assert_eq!(
        key("Bash", json!({ "command": long })).as_deref(),
        Some("Bash"),
        "only the first COMMAND_BYTES_MAX bytes of a command are read"
    );
}

#[test]
fn skill_key_names_claude_and_codex_skills() {
    let key = |name: &str, input: Value| skill_key(name, &input);
    assert_eq!(
        key("Skill", json!({"skill": "tdd"})).as_deref(),
        Some("skill:tdd")
    );
    assert_eq!(
        key("Skill", json!({"skill": "superpowers:brainstorming"})).as_deref(),
        Some("skill:superpowers:brainstorming")
    );
    assert_eq!(
        key(
            "Bash",
            json!({"command": "cat /Users/o/.claude/skills/grill-me/SKILL.md"})
        )
        .as_deref(),
        Some("skill:grill-me")
    );
    assert_eq!(
        key(
            "exec_command",
            json!({"cmd": "sed -n 1,80p '/h/.codex/skills/tiger-style/SKILL.md'"})
        )
        .as_deref(),
        Some("skill:tiger-style")
    );
    assert_eq!(
        key(
            "shell",
            json!({"command": ["bash", "-lc", "cat skills/caveman/SKILL.md | head"]})
        )
        .as_deref(),
        Some("skill:caveman")
    );
    assert_eq!(key("Skill", json!({"skill": "bad name"})), None);
    assert_eq!(key("Skill", json!({})), None);
    assert_eq!(key("Skill", json!({"skill": ""})), None);
    assert_eq!(
        key(
            "Skill",
            json!({"skill": "x".repeat(SKILL_NAME_BYTES_MAX + 1)})
        ),
        None
    );
    assert_eq!(
        key("Skill", json!({"skill": "x".repeat(SKILL_NAME_BYTES_MAX)})).map(|key| key.len()),
        Some(TOOL_BYTES_MAX)
    );
    assert_eq!(
        key(
            "Read",
            json!({"file_path": "/h/.claude/skills/tdd/SKILL.md"})
        ),
        None
    );
    assert_eq!(key("Bash", json!({"command": "ls skills/"})), None);
}

#[test]
fn adoption_flags_follow_q18_rules() {
    let (_root, db, project) = database();
    let long_ago = NOW - UNGROUNDED_MS;
    let edit = json!({});
    let both = agent_row(&db, &project, "working", true, long_ago, false);
    call(&db, both, "Edit", &edit, 1);

    let grounded = agent_row(&db, &project, "working", true, long_ago, false);
    crate::coord::start_work(
        &db,
        &project,
        grounded,
        "Fix login".into(),
        vec!["src".into()],
        NOW,
    )
    .unwrap();
    call(&db, grounded, "Edit", &edit, 2);
    call(&db, grounded, "mcp__graphify__query_graph", &json!({}), 1);

    let context = agent_row(&db, &project, "working", true, long_ago, false);
    call(
        &db,
        context,
        "mcp__context-mode__ctx_execute",
        &json!({}),
        1,
    );

    let fresh = agent_row(
        &db,
        &project,
        "working",
        true,
        NOW - UNGROUNDED_MS + 1,
        false,
    );
    call(&db, fresh, "Write", &edit, 1);

    let stopped = agent_row(&db, &project, "stopped", true, long_ago, false);
    call(&db, stopped, "MultiEdit", &edit, 1);

    let toolless = agent_row(&db, &project, "working", false, long_ago, false);

    let idle = agent_row(&db, &project, "idle", true, long_ago, false);

    let bare = agent_row(&db, &project, "working", true, long_ago, true);
    call(&db, bare, "Edit", &edit, 3);

    let view = view(&db, &project, &[], NOW).unwrap();
    let flags = |agent: AgentId| row(&view, agent).flags.clone();
    assert_eq!(
        flags(both),
        [Flag::EditsWithoutWork, Flag::Ungrounded],
        "edits with no declared work, and ten busy minutes with no graph or context call"
    );
    assert_eq!(
        flags(grounded),
        Vec::<Flag>::new(),
        "declared work and a graph query"
    );
    assert_eq!(
        flags(context),
        Vec::<Flag>::new(),
        "a context-mode call grounds it"
    );
    assert_eq!(
        flags(fresh),
        [Flag::EditsWithoutWork],
        "under ten minutes is not ungrounded yet"
    );
    assert_eq!(
        flags(stopped),
        [Flag::EditsWithoutWork],
        "a stopped agent keeps its edit flag but is not ungrounded"
    );
    assert_eq!(
        flags(toolless),
        Vec::<Flag>::new(),
        "an agent without tools is never ungrounded"
    );
    assert_eq!(
        flags(idle),
        Vec::<Flag>::new(),
        "an idle agent that called nothing is not busy"
    );
    assert_eq!(
        flags(bare),
        [Flag::EditsWithoutWork, Flag::Ungrounded],
        "a hookless agent raises the same flags as one with hooks (OQ-8)"
    );
    assert!(row(&view, bare).hookless);
    assert!(!row(&view, both).hookless);
    assert!(row(&view, both).active && !row(&view, stopped).active);
}

fn notes(db: &Connection, agent: AgentId) -> Vec<String> {
    let mut statement = db
        .prepare(
            "SELECT body FROM coord_messages WHERE from_kind='engine' AND to_kind='agent'
             AND to_agent=?1 ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([agent], |found| found.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn adoption_tick_sends_one_note_per_flag_only_under_ten_minute_nudges() {
    let (_root, db, project) = database();
    let long_ago = NOW - UNGROUNDED_MS;
    let flagged = agent_row(&db, &project, "working", true, long_ago, false);
    call(&db, flagged, "Edit", &json!({}), 1);
    let bare = agent_row(&db, &project, "working", true, long_ago, true);
    call(&db, bare, "Edit", &json!({}), 1);
    let stopped = agent_row(&db, &project, "stopped", true, long_ago, false);
    call(&db, stopped, "Edit", &json!({}), 1);

    assert_eq!(
        crate::coord::settings::CoordSettings::default().nudges,
        Nudges::Off,
        "nudges default to off (DEC-109)"
    );
    tick(&db, &project, NOW).unwrap();
    assert!(
        notes(&db, flagged).is_empty(),
        "a project that never set nudges sends nothing"
    );
    let off = crate::coord::settings::CoordSettings {
        nudges: Nudges::Off,
        ..Default::default()
    };
    crate::coord::settings::write(&db, &project, &off).unwrap();
    tick(&db, &project, NOW).unwrap();
    assert!(notes(&db, flagged).is_empty(), "nudges off sends nothing");
    assert!(
        notes(&db, bare).is_empty(),
        "nudges off reminds no hookless agent either"
    );
    let view_off = view(&db, &project, &[], NOW).unwrap();
    assert_eq!(view_off.nudges, Nudges::Off);
    assert_eq!(
        row(&view_off, flagged).flags,
        [Flag::EditsWithoutWork, Flag::Ungrounded],
        "the flags still show with nudges off"
    );

    let on = crate::coord::settings::CoordSettings {
        nudges: Nudges::TenMinutes,
        ..Default::default()
    };
    crate::coord::settings::write(&db, &project, &on).unwrap();
    tick(&db, &project, NOW).unwrap();
    assert_eq!(
        notes(&db, flagged),
        [
            Flag::EditsWithoutWork.reminder(),
            Flag::Ungrounded.reminder()
        ]
    );
    tick(&db, &project, NOW + 60_000).unwrap();
    assert_eq!(notes(&db, flagged).len(), 2, "one reminder per flag, ever");
    assert_eq!(
        notes(&db, bare),
        [
            Flag::EditsWithoutWork.reminder(),
            Flag::Ungrounded.reminder()
        ],
        "a hookless agent is reminded once per flag like any other (OQ-8)"
    );
    assert!(
        notes(&db, stopped).is_empty(),
        "a stopped agent is not reminded"
    );
    let view_on = view(&db, &project, &[], NOW).unwrap();
    assert_eq!(
        row(&view_on, flagged).reminded,
        [Flag::EditsWithoutWork, Flag::Ungrounded]
    );
}

/// Under the FirstEdit nudges (DEC-111 R4) the engine reminds each running tools-on agent
/// without the hook path, a Codex agent or a launch that fires no hooks, once its counted
/// calls hold a file edit and no code-graph query, however often it ticks, and sends no
/// flag note. A Claude Code agent with hooks gets its reminder from the hook instead.
#[test]
fn first_edit_nudges_remind_each_agent_without_hooks_once() {
    let (_root, db, project) = database();
    let long_ago = NOW - UNGROUNDED_MS;
    let codex = |tools: bool| {
        let id = agent_row(&db, &project, "working", tools, long_ago, false);
        db.execute("UPDATE team_agents SET harness='codex' WHERE id=?1", [id])
            .unwrap();
        id
    };
    let hooked = agent_row(&db, &project, "working", true, long_ago, false);
    call(&db, hooked, "Edit", &json!({}), 1);
    let bare = agent_row(&db, &project, "working", true, long_ago, true);
    call(&db, bare, "Edit", &json!({}), 1);
    let edited = codex(true);
    call(&db, edited, "apply_patch", &json!({}), 2);
    let grounded = codex(true);
    call(
        &db,
        grounded,
        "exec_command",
        &json!({"cmd": "graphify query parser"}),
        1,
    );
    call(&db, grounded, "apply_patch", &json!({}), 1);
    let reading = codex(true);
    call(&db, reading, "exec_command", &json!({"cmd": "ls"}), 1);
    let toolless = codex(false);
    call(&db, toolless, "apply_patch", &json!({}), 1);
    let stopped = agent_row(&db, &project, "stopped", true, long_ago, true);
    call(&db, stopped, "Edit", &json!({}), 1);

    let first_edit = crate::coord::settings::CoordSettings {
        nudges: Nudges::FirstEdit,
        ..Default::default()
    };
    crate::coord::settings::write(&db, &project, &first_edit).unwrap();
    assert_eq!(
        view(&db, &project, &[], NOW).unwrap().nudges,
        Nudges::FirstEdit
    );
    tick(&db, &project, NOW).unwrap();
    tick(&db, &project, NOW + 60_000).unwrap();
    for reminded in [edited, bare] {
        assert_eq!(
            notes(&db, reminded),
            [FIRST_EDIT_REMINDER],
            "agent {reminded} is reminded once"
        );
    }
    for quiet in [hooked, grounded, reading, toolless, stopped] {
        assert!(
            notes(&db, quiet).is_empty(),
            "agent {quiet}: {:?}",
            notes(&db, quiet)
        );
    }
}

#[test]
fn view_counts_every_enabled_skill_with_zero() {
    let (_root, db, project) = database();
    let agent = agent_row(&db, &project, "working", true, NOW, false);
    call(&db, agent, "Skill", &json!({"skill": "tdd"}), 2);
    call(
        &db,
        agent,
        "Skill",
        &json!({"skill": "superpowers:grill-me"}),
        1,
    );
    call(
        &db,
        agent,
        "Bash",
        &json!({"command": "cat /h/.claude/skills/other-one/SKILL.md"}),
        1,
    );
    call(&db, agent, "mcp__graphify__query_graph", &json!({}), 1);
    call(&db, agent, "mcp__context-mode__ctx_execute", &json!({}), 2);
    call(&db, agent, "mcp__context-mode__ctx_search", &json!({}), 1);
    let enabled = [
        "zeta".to_owned(),
        "tdd".into(),
        "grill-me".into(),
        "bad name".into(),
    ];
    let view = view(&db, &project, &enabled, NOW).unwrap();
    assert_eq!(
        view.skills,
        ["grill-me", "tdd", "zeta"],
        "sorted, invalid names left out"
    );
    let shown = row(&view, agent);
    assert_eq!(
        shown.skills,
        [1, 2, 0],
        "every enabled skill, zero included"
    );
    assert_eq!(
        shown.other_skills,
        [SkillCount {
            name: "other-one".into(),
            count: 1
        }]
    );
    assert_eq!((shown.graphify, shown.context_mode), (1, 3));
    let column = |name: &str| {
        let at = view.counted.iter().position(|entry| entry == name).unwrap();
        shown.counts[at]
    };
    assert_eq!(column("ctx_*"), 3);
    assert_eq!(column("graphify"), 1);
    assert_eq!(column("Skill"), 4, "the harness's own Skill count");
    assert_eq!(column("start_work"), 0);
}

#[test]
fn view_keeps_newest_agents_within_budget() {
    let (_root, db, project) = database();
    let skills: Vec<String> = (0..200).map(|n| format!("skill-number-{n:03}")).collect();
    let mut ids = Vec::new();
    for n in 0..AGENTS_SHOWN_MAX + 2 {
        let agent = agent_row(&db, &project, "working", true, NOW, false);
        for other in 0..OTHER_SKILLS_MAX {
            let name = format!("{}{n}-{other}", "o".repeat(SKILL_NAME_BYTES_MAX - 8));
            crate::coord::record_tool(&db, agent, &format!("{SKILL_PREFIX}{name}"), NOW).unwrap();
        }
        ids.push(agent);
    }
    let view = view(&db, &project, &skills, NOW).unwrap();
    assert_eq!(view.skills.len(), SKILLS_SHOWN_MAX.min(200));
    let bytes = serde_json::to_vec(&view).unwrap().len();
    assert!(bytes <= VIEW_BYTES_MAX, "{bytes}");
    let row = serde_json::to_vec(&view.agents[0]).unwrap().len() + 1;
    assert!(
        view.agents
            .iter()
            .all(|agent| serde_json::to_vec(agent).unwrap().len() + 1 == row),
        "rows of one shape encode to one size"
    );
    let header = serde_json::to_vec(&Adoption {
        agents: Vec::new(),
        hidden: 0,
        ..view.clone()
    })
    .unwrap()
    .len();
    let fits = (VIEW_BYTES_MAX - header) / row;
    assert!(
        fits < AGENTS_SHOWN_MAX,
        "the byte budget, not the row cap, decides here"
    );
    assert_eq!(
        view.agents.len(),
        fits,
        "rows fill the budget until the next one would pass it"
    );
    assert_eq!(
        usize::try_from(view.hidden).unwrap(),
        AGENTS_SHOWN_MAX - fits,
        "every newer agent the budget left out is counted"
    );
    assert_eq!(
        view.agents.len() + usize::try_from(view.hidden).unwrap(),
        AGENTS_SHOWN_MAX,
        "only the newest AGENTS_SHOWN_MAX are read"
    );
    let shown: Vec<AgentId> = view.agents.iter().map(|row| row.agent).collect();
    let newest: Vec<AgentId> = ids.iter().rev().take(shown.len()).copied().collect();
    assert_eq!(shown, newest, "newest first, none skipped");
    assert!(
        view.agents
            .iter()
            .all(|row| row.skills.len() == view.skills.len())
    );
    assert!(
        view.agents
            .iter()
            .all(|row| row.other_skills.len() == OTHER_SKILLS_MAX)
    );
}

/// A Claude agent whose launch fires no hooks gets the counts, flags and TenMinutes
/// reminders of one with hooks (OQ-8): the engine counts its calls from its transcript, so
/// its habits are as known as any other agent's.
#[test]
fn a_hookless_agent_gets_the_counts_flags_and_reminders_of_one_with_hooks() {
    let directory = tempfile::tempdir().unwrap();
    let db = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let project = ProjectId::new("adoption-hooks").unwrap();
    db.execute(
        "INSERT INTO projects(id, body) VALUES (?1, '{}')",
        [project.as_str()],
    )
    .unwrap();
    let agent = |hookless: bool| -> AgentId {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
             VALUES (?1,'sub','claude-code',1,'working',0,0,?2)",
            params![project.as_str(), hookless],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO coord_tool_use(agent,tool,count,last)
             VALUES (?1,'Edit',3,1),(?1,'Skill',2,1),(?1,'start_work',1,1)",
            [id],
        )
        .unwrap();
        id
    };
    let hooked = agent(false);
    let hookless = agent(true);
    let now = UNGROUNDED_MS + 1;

    let view = view(&db, &project, &[], now).unwrap();
    let row = |id: AgentId| {
        view.agents
            .iter()
            .find(|agent| agent.agent == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        row(hooked).flags,
        vec![Flag::EditsWithoutWork, Flag::Ungrounded],
        "an agent with hooks that edits without work for 10 minutes ungrounded raises both"
    );
    let bare = row(hookless);
    assert!(bare.hookless);
    assert_eq!(
        bare.flags,
        row(hooked).flags,
        "a launch without hooks raises the same flags"
    );
    assert_eq!(
        bare.counts,
        row(hooked).counts,
        "a launch without hooks shows the same counts"
    );
    assert_eq!(bare.counts, [1, 0, 0, 0, 0, 0, 0, 2]);
    assert!(!row(hooked).hookless);

    let nudges = crate::coord::settings::CoordSettings {
        nudges: Nudges::TenMinutes,
        ..Default::default()
    };
    crate::coord::settings::write(&db, &project, &nudges).unwrap();
    tick(&db, &project, now).unwrap();
    let notes = |id: AgentId| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE to_agent=?1 AND kind='note'",
            [id],
            |found| found.get(0),
        )
        .unwrap()
    };
    assert_eq!(notes(hooked), 2, "one reminder per flag");
    assert_eq!(
        notes(hookless),
        2,
        "an agent without hooks gets one reminder per flag too"
    );
}

#[test]
fn a_tool_call_is_also_counted_in_its_hour() {
    let (_root, db, project) = database();
    let agent = agent_row(&db, &project, "running", true, NOW, false);
    const HOUR: i64 = 3_600_000;
    crate::coord::record_tool(&db, agent, "Edit", 2 * HOUR + 5).unwrap();
    crate::coord::record_tool(&db, agent, "Edit", 2 * HOUR + 9).unwrap();
    crate::coord::record_tool(&db, agent, "Edit", 3 * HOUR).unwrap();
    let rows: Vec<(i64, i64)> = db
        .prepare("SELECT hour_ms,count FROM coord_tool_hours WHERE agent=?1 ORDER BY hour_ms")
        .unwrap()
        .query_map([agent], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![(2 * HOUR, 2), (3 * HOUR, 1)]);
    let counted: i64 = db
        .query_row("SELECT rows FROM coord_tool_hours_rows", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(counted, 2, "a repeat in its hour adds no row to the cap");
}

#[test]
fn past_its_caps_a_tool_call_is_left_out_of_the_chart_only() {
    let (_root, db, project) = database();
    let agent = agent_row(&db, &project, "running", true, NOW, false);
    const HOUR: i64 = 3_600_000;
    for index in 0..crate::coord::TOOLS_PER_HOUR_MAX {
        crate::coord::record_tool(&db, agent, &format!("tool-{index}"), HOUR).unwrap();
    }
    crate::coord::record_tool(&db, agent, "one-more", HOUR).unwrap();
    let held: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM coord_tool_hours WHERE agent=?1",
            [agent],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(held, crate::coord::TOOLS_PER_HOUR_MAX);
    crate::coord::record_tool(&db, agent, "tool-0", HOUR).unwrap();
    let repeated: i64 = db
        .query_row(
            "SELECT count FROM coord_tool_hours WHERE tool='tool-0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repeated, 2, "a tool the hour already holds still counts");
    db.execute(
        "UPDATE coord_tool_hours_rows SET rows=?1",
        [crate::coord::TOOL_HOURS_MAX],
    )
    .unwrap();
    crate::coord::record_tool(&db, agent, "tool-0", 2 * HOUR).unwrap();
    let later: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM coord_tool_hours WHERE hour_ms=?1",
            [2 * HOUR],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(later, 0);
    let counted = |tool: &str| -> i64 {
        db.query_row(
            "SELECT count FROM coord_tool_use WHERE agent=?1 AND tool=?2",
            rusqlite::params![agent, tool],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(counted("one-more"), 1, "past the caps it still counts");
    assert_eq!(counted("tool-0"), 3, "the call past the row cap counts");
}
