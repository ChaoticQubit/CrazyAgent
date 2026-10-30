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
        Vec::<Flag>::new(),
        "a hookless agent raises no flag"
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
fn adoption_tick_sends_one_note_per_flag_only_while_nudges_on() {
    let (_root, db, project) = database();
    let long_ago = NOW - UNGROUNDED_MS;
    let flagged = agent_row(&db, &project, "working", true, long_ago, false);
    call(&db, flagged, "Edit", &json!({}), 1);
    let bare = agent_row(&db, &project, "working", true, long_ago, true);
    call(&db, bare, "Edit", &json!({}), 1);
    let stopped = agent_row(&db, &project, "stopped", true, long_ago, false);
    call(&db, stopped, "Edit", &json!({}), 1);

    let off = crate::coord::settings::CoordSettings {
        nudges: false,
        ..Default::default()
    };
    crate::coord::settings::write(&db, &project, &off).unwrap();
    tick(&db, &project, NOW).unwrap();
    assert!(notes(&db, flagged).is_empty(), "nudges off sends nothing");
    let view_off = view(&db, &project, &[], NOW).unwrap();
    assert!(!view_off.nudges);
    assert_eq!(
        row(&view_off, flagged).flags,
        [Flag::EditsWithoutWork, Flag::Ungrounded],
        "the flags still show with nudges off"
    );

    crate::coord::settings::write(&db, &project, &Default::default()).unwrap();
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
    assert!(
        notes(&db, bare).is_empty(),
        "a hookless agent is never reminded"
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
    assert!(view.hidden > 0, "the byte budget left some out");
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
