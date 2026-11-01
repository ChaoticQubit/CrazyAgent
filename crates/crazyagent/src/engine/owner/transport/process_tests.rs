use super::*;
use crate::project_setup::{StoredSetup, TeamPlan};
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

fn with_project(root: &Path, folder: &Path) -> ProjectId {
    let project = ProjectId::new("process-1").unwrap();
    let setup = StoredSetup {
        project_id: project.clone(),
        name: "Process".into(),
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
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1,
        revision: 1,
    };
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            [
                project.as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            ],
        )
        .unwrap();
    project
}

fn read(root: &Path, project: Option<ProjectId>) -> Result<Option<String>, EngineError> {
    run(root, Action::Read { project }, None)
}

fn save(
    root: &Path,
    project: Option<ProjectId>,
    text: &str,
) -> Result<Option<String>, EngineError> {
    run(root, Action::Save { project }, Some(text.into()))
}

#[test]
fn the_app_and_a_project_text_are_saved_and_read_through_the_protocol() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);

    assert_eq!(read(&root, None).unwrap().as_deref(), Some(""));
    assert_eq!(save(&root, None, "App rule.\n").unwrap(), None);
    assert_eq!(read(&root, None).unwrap().as_deref(), Some("App rule.\n"));

    assert_eq!(
        read(&root, Some(project.clone())).unwrap().as_deref(),
        Some("")
    );
    assert_eq!(
        save(&root, Some(project.clone()), "Project rule.\n").unwrap(),
        None
    );
    assert_eq!(
        fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "Project rule.\n"
    );
    assert_eq!(
        read(&root, Some(project)).unwrap().as_deref(),
        Some("Project rule.\n")
    );
    let unknown = ProjectId::new("not-set-up").unwrap();
    assert_eq!(
        read(&root, Some(unknown)).unwrap_err().0,
        "process_project_unknown"
    );
}

#[test]
fn the_global_instructions_are_read_only_on_the_explicit_import() {
    let (_directory, root) = root();
    let home = root.join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(home.join(".claude/CLAUDE.md"), "OWNER-GLOBAL-RULE\n").unwrap();

    for action in [Action::Read { project: None }, Action::Default] {
        let text = run(&root, action, None).unwrap().unwrap();
        assert!(!text.contains("OWNER-GLOBAL-RULE"));
    }
    assert_eq!(crate::agent_home::read_app(&root).unwrap(), "");

    let path = home.join(".claude/CLAUDE.md");
    let imported = run(&root, Action::ReadFile { path }, None).unwrap();
    assert_eq!(imported.as_deref(), Some("OWNER-GLOBAL-RULE\n"));
    assert_eq!(
        crate::agent_home::read_app(&root).unwrap(),
        "",
        "an import is a preview; only a save stores it"
    );

    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let path = empty.join(".claude/CLAUDE.md");
    assert_eq!(
        run(&root, Action::ReadFile { path }, None).unwrap_err().0,
        "process_import_missing"
    );
    let path = PathBuf::from("relative/CLAUDE.md");
    assert_eq!(
        run(&root, Action::ReadFile { path }, None).unwrap_err().0,
        "process_request_invalid"
    );
}

#[cfg(unix)]
#[test]
fn an_import_follows_the_owners_linked_global_file() {
    let (_directory, root) = root();
    let home = root.join("home");
    let dotfiles = root.join("dotfiles");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(&dotfiles).unwrap();
    fs::write(dotfiles.join("AGENTS.md"), "LINKED-RULE\n").unwrap();
    std::os::unix::fs::symlink(dotfiles.join("AGENTS.md"), home.join(".claude/CLAUDE.md")).unwrap();
    let path = home.join(".claude/CLAUDE.md");
    assert_eq!(
        run(&root, Action::ReadFile { path: path.clone() }, None)
            .unwrap()
            .as_deref(),
        Some("LINKED-RULE\n")
    );
    fs::write(dotfiles.join("AGENTS.md"), "a".repeat(TEXT_BYTES_MAX + 1)).unwrap();
    assert_eq!(
        run(&root, Action::ReadFile { path }, None).unwrap_err().0,
        "process_text_too_large"
    );
}

#[test]
fn the_default_process_is_plain_bounded_and_keeps_the_owner_rules() {
    let (_directory, root) = root();
    let text = run(&root, Action::Default, None).unwrap().unwrap();
    assert_eq!(text, crate::agent_home::DEFAULT_PROCESS);
    crate::agent_home::validate(&text).unwrap();
    assert!(!text.contains('\u{2014}'), "no em dash");
    for rule in [
        "Never create, switch, rename, delete or push a branch",
        "watch it fail",
        "Real tests only",
        "review your own diff",
        "No comments in code, except a short docstring",
        "Never use an em dash",
        "never write inside the project folder's `.git` or rewrite history",
        "Commit or leave your edits as the \"How your work lands\" section CrazyAgent gives you says",
        "The owner accepts your work in the project's Changes view",
        "Never put a secret",
        "asks the master",
        "escalates to the owner only for security, destructive or irreversible actions, spending, and product or scope decisions",
    ] {
        assert!(text.contains(rule), "the default process misses: {rule}");
    }
    assert!(
        !text.contains("never commit"),
        "an Own branch agent lands only its commits, so the seed defers to the layout rule"
    );
    for tool in [
        "graphify",
        "context-mode",
        "herdr",
        "codex-security",
        "deepsec",
    ] {
        assert!(
            !text.to_lowercase().contains(tool),
            "machine-only tool {tool}"
        );
    }
}

#[test]
fn a_mismatched_or_oversized_request_is_refused() {
    let (_directory, root) = root();
    assert_eq!(
        run(&root, Action::Save { project: None }, None)
            .unwrap_err()
            .0,
        "process_request_invalid"
    );
    assert_eq!(
        run(&root, Action::Default, Some("x".into())).unwrap_err().0,
        "process_request_invalid"
    );
    assert_eq!(
        save(&root, None, &"a".repeat(TEXT_BYTES_MAX + 1))
            .unwrap_err()
            .0,
        "process_text_too_large"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let over = u32::try_from(TEXT_BYTES_MAX + 1).unwrap();
    let mut payload: &[u8] = &[];
    assert_eq!(
        runtime
            .block_on(read_text(&mut payload, over))
            .unwrap_err()
            .0,
        "process_text_too_large"
    );
    let mut short: &[u8] = b"abc";
    assert_eq!(
        runtime.block_on(read_text(&mut short, 5)).unwrap_err().0,
        "process_request_invalid"
    );
    let mut nul: &[u8] = b"a\0b";
    assert_eq!(
        runtime.block_on(read_text(&mut nul, 3)).unwrap_err().0,
        "process_text_invalid"
    );
}

#[test]
fn saves_at_once_each_read_back_the_settings_they_stored() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(&folder).unwrap();
    let project = with_project(&root, &folder);
    let options: Vec<InstructionSettings> = [Mode::ProjectOnly, Mode::AppOnly, Mode::Both]
        .into_iter()
        .flat_map(|mode| [true, false].map(|transcripts| InstructionSettings { mode, transcripts }))
        .collect();
    assert_eq!(options.len(), 6);
    let start = std::sync::Barrier::new(options.len());
    std::thread::scope(|scope| {
        let saves: Vec<_> = options
            .iter()
            .map(|&settings| {
                let (root, project, start) = (&root, &project, &start);
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..100 {
                        assert_eq!(
                            instruction_settings(root, project, Some(settings)),
                            Ok(settings)
                        );
                    }
                })
            })
            .collect();
        for save in saves {
            save.join()
                .expect("every save reads back what it stored, whatever saves beside it");
        }
    });
    assert!(options.contains(&instruction_settings(&root, &project, None).unwrap()));
}

fn team_agent(root: &Path, project: &str, role: &str, harness: &str) -> i64 {
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::team::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
             VALUES (?1, ?2, ?3, 1, 'stopped', 1, 1)",
            [project, role, harness],
        )
        .unwrap();
    connection.last_insert_rowid()
}

#[cfg(unix)]
fn launched_home(root: &Path, id: i64) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let home = crate::team::runtime::agent_home(&root.join("team"), id);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    home
}

#[cfg(unix)]
#[test]
fn each_team_agent_shows_what_its_last_launch_left_in_its_home() {
    use crate::agent_home::{AgentProject, Harness, prepare, record_skills};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(&folder).unwrap();
    let project = with_project(&root, &folder);
    assert_eq!(
        agent_instructions(&root, &project, None).unwrap_err().0,
        "process_agent_unknown",
        "a project whose team never started has no agents"
    );
    let first_master = team_agent(&root, project.as_str(), "master", "codex");
    let master = team_agent(&root, project.as_str(), "master", "claude-code");
    let sub = team_agent(&root, project.as_str(), "sub", "codex");
    let other = team_agent(&root, "another-project", "sub", "codex");
    let unsupported = team_agent(&root, project.as_str(), "sub", "opencode");
    assert!(first_master < master);

    assert_eq!(
        agent_instructions(&root, &project, Some(sub)).unwrap(),
        AgentInstructions {
            agent: sub,
            harness: "codex".into(),
            launched: false,
            text: String::new(),
            skills: Vec::new(),
            sessions: 0,
            loaded: Vec::new(),
            skills_used: Vec::new(),
            files: Vec::new(),
        }
    );

    let given = AgentProject {
        data: root.clone(),
        id: project.clone(),
        folder: Some(folder.clone()),
        mode: crate::instruction_files::Mode::Both,
    };
    let text = |text: &str| crate::agent_home::Given {
        text: text.into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    let sub_home = launched_home(&root, sub);
    prepare(&sub_home, Harness::Codex, &given, &text("# Sub rules\n")).unwrap();
    record_skills(&root, &sub_home, &["tdd", "code-review"]).unwrap();
    assert_eq!(
        agent_instructions(&root, &project, Some(sub)).unwrap(),
        AgentInstructions {
            agent: sub,
            harness: "codex".into(),
            launched: true,
            text: "# Sub rules\n".into(),
            skills: vec!["tdd".into(), "code-review".into()],
            sessions: 0,
            loaded: Vec::new(),
            skills_used: Vec::new(),
            files: Vec::new(),
        }
    );

    let rollout = sub_home.join("sessions/2026/10/27/rollout-2026-10-27T18-00-00-0199.jsonl");
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    let said = |text: &str| {
        serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": text}]}})
        .to_string()
    };
    fs::write(
        &rollout,
        format!(
            "{}\n{}\n",
            said(&format!(
                "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\nOWNER-SECRET-78\n</INSTRUCTIONS>",
                folder.display()
            )),
            said("<skill>\n<name>tdd</name>\n<path>/skills/tdd/SKILL.md</path>\n</skill>")
        ),
    )
    .unwrap();
    let reported = agent_instructions(&root, &project, Some(sub)).unwrap();
    assert_eq!(reported.sessions, 1);
    assert_eq!(
        reported.loaded,
        vec![crate::agent_home::LoadedFile {
            name: "AGENTS.md".into(),
            path: folder.display().to_string(),
        }]
    );
    assert_eq!(reported.skills_used, vec!["tdd".to_owned()]);
    assert!(!format!("{reported:?}").contains("OWNER-SECRET-78"));

    let master_home = launched_home(&root, master);
    prepare(
        &master_home,
        Harness::Claude,
        &given,
        &text("# Master rules\n"),
    )
    .unwrap();
    let shown = agent_instructions(&root, &project, None).unwrap();
    assert_eq!(
        shown.agent, master,
        "the master card reads the newest master"
    );
    assert_eq!(shown.harness, "claude-code");
    assert_eq!(shown.text, "# Master rules\n");

    assert_eq!(
        agent_instructions(&root, &project, Some(other))
            .unwrap_err()
            .0,
        "process_agent_unknown",
        "another project's agent is never read through this project"
    );
    assert_eq!(
        agent_instructions(&root, &project, Some(unsupported))
            .unwrap_err()
            .0,
        "process_agent_unsupported"
    );
    assert_eq!(
        agent_instructions(&root, &project, Some(0)).unwrap_err().0,
        "process_request_invalid"
    );

    let secret = root.join("owner-secret");
    fs::write(&secret, "OWNER-SECRET-77").unwrap();
    fs::remove_file(sub_home.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(&secret, sub_home.join("AGENTS.md")).unwrap();
    assert_eq!(
        agent_instructions(&root, &project, Some(sub))
            .unwrap_err()
            .0,
        "agent_instructions_unsafe",
        "a link the agent planted in its home is never followed"
    );
}

#[cfg(unix)]
#[test]
fn an_agent_cannot_change_the_instruction_files_its_card_lists() {
    use crate::agent_home::{AgentProject, Given, Harness, prepare};
    use crate::instruction_files::{Mode, Record, State};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(&folder).unwrap();
    let project = with_project(&root, &folder);
    let sub = team_agent(&root, project.as_str(), "sub", "claude-code");
    let home = launched_home(&root, sub);
    let files = vec![
        Record {
            name: "CLAUDE.md".into(),
            bytes: 120,
            state: State::Given,
        },
        Record {
            name: "AGENTS.md".into(),
            bytes: 40_000,
            state: State::TooLarge,
        },
    ];
    let given = Given {
        text: "# Rules\n".into(),
        files: files.clone(),
        section_bytes: 0,
    };
    let launch = AgentProject {
        data: root.clone(),
        id: project.clone(),
        folder: Some(folder.clone()),
        mode: Mode::Both,
    };
    prepare(&home, Harness::Claude, &launch, &given).unwrap();
    assert_eq!(
        agent_instructions(&root, &project, Some(sub))
            .unwrap()
            .files,
        files
    );

    let forged = "given\t5\tCLAUDE.local.md\n";
    let mut planted = vec![home.join("crazyagent-instruction-files")];
    for entry in fs::read_dir(&home).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().is_some_and(|name| name != "CLAUDE.md") {
            planted.push(path);
        }
    }
    for path in &planted {
        fs::write(path, forged).unwrap();
    }
    let shown = agent_instructions(&root, &project, Some(sub)).unwrap();
    assert_eq!(
        shown.files, files,
        "the card lists only the engine's own record, whatever the agent writes in its home"
    );
    assert_eq!(shown.text, "# Rules\n");
}

#[cfg(unix)]
#[test]
fn an_agent_cannot_change_the_skills_its_card_lists() {
    use crate::agent_home::{AgentProject, Given, Harness, prepare, record_skills};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(&folder).unwrap();
    let project = with_project(&root, &folder);
    let sub = team_agent(&root, project.as_str(), "sub", "codex");
    let home = launched_home(&root, sub);
    let launch = AgentProject {
        data: root.clone(),
        id: project.clone(),
        folder: Some(folder.clone()),
        mode: crate::instruction_files::Mode::Both,
    };
    let given = Given {
        text: "# Rules\n".into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    prepare(&home, Harness::Codex, &launch, &given).unwrap();
    record_skills(&root, &home, &["tdd", "code-review"]).unwrap();
    let installed = vec!["tdd".to_owned(), "code-review".to_owned()];
    assert_eq!(
        agent_instructions(&root, &project, Some(sub))
            .unwrap()
            .skills,
        installed
    );

    let forged = "secret-exfiltration\n";
    let mut planted = vec![home.join("crazyagent-skills")];
    for entry in fs::read_dir(&home).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().is_some_and(|name| name != "AGENTS.md") {
            planted.push(path);
        }
    }
    for path in &planted {
        fs::write(path, forged).unwrap();
    }
    let shown = agent_instructions(&root, &project, Some(sub)).unwrap();
    assert_eq!(
        shown.skills, installed,
        "the card lists only the engine's own record, whatever the agent writes in its home"
    );
    assert_eq!(shown.text, "# Rules\n");
}

#[test]
fn a_received_payload_holds_the_rendered_bound() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let full = "x".repeat(crate::agent_home::PROCESS_BYTES_MAX);
    let mut payload = full.as_bytes();
    let bytes = u32::try_from(full.len()).unwrap();
    assert_eq!(
        runtime
            .block_on(read_payload(
                &mut payload,
                bytes,
                crate::agent_home::PROCESS_BYTES_MAX
            ))
            .unwrap()
            .len(),
        crate::agent_home::PROCESS_BYTES_MAX
    );
    let mut none: &[u8] = &[];
    assert_eq!(
        runtime
            .block_on(read_payload(
                &mut none,
                bytes + 1,
                crate::agent_home::PROCESS_BYTES_MAX
            ))
            .unwrap_err()
            .0,
        "process_text_too_large"
    );
}

/// Reads the plain codes a source file refuses with, before its tests.
/// Input: the file text. Output: each `process_` code once.
fn refusals(source: &str) -> std::collections::BTreeSet<&str> {
    let code = source.split("#[cfg(test)]").next().unwrap();
    ["\"process_", "\"agent_instructions_"]
        .into_iter()
        .flat_map(|prefix| code.match_indices(prefix))
        .filter_map(|(at, _)| {
            let rest = &code[at + 1..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect()
}

#[test]
fn process_codes_cover_every_refusal_of_the_store_and_protocol() {
    let (prefixes, found): (
        std::collections::BTreeSet<&str>,
        std::collections::BTreeSet<&str>,
    ) = [
        include_str!("../../../agent_home.rs"),
        include_str!("process.rs"),
    ]
    .into_iter()
    .flat_map(refusals)
    .partition(|literal| literal.ends_with('_'));
    assert_eq!(
        prefixes,
        std::collections::BTreeSet::from(["agent_instructions_too_large_"]),
        "the one prefix builds launch refusals, which the Process page never answers with"
    );
    assert!(found.len() >= 12);
    for code in &found {
        assert!(
            PROCESS_CODES.contains(code),
            "{code} is not in PROCESS_CODES"
        );
    }
    let mut sorted = PROCESS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, PROCESS_CODES);
}

#[test]
fn process_adoption_round_trip() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(&folder).unwrap();
    let unknown = ProjectId::new("never-set-up").unwrap();
    {
        let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
    }
    let project = with_project(&root, &folder);
    let master = team_agent(&root, project.as_str(), "master", "claude-code");
    let sub = team_agent(&root, project.as_str(), "sub", "codex");
    team_agent(&root, "another-project", "sub", "codex");
    {
        let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        for _ in 0..2 {
            crate::coord::record_tool(&connection, sub, "graphify", 5).unwrap();
        }
        crate::coord::record_tool(&connection, sub, "ctx_execute", 5).unwrap();
        crate::coord::record_tool(&connection, master, "Edit", 5).unwrap();
        crate::coord::record_tool(&connection, sub, "skill:tdd", 5).unwrap();
        let skill = |id: &str, name: &str, scope: serde_json::Value, key: &str, enabled: bool| {
            let folder = format!("plugins/skills/{id}/{name}");
            fs::create_dir_all(root.join(&folder)).unwrap();
            fs::write(
                root.join(&folder).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: The {name} rule.\n---\n"),
            )
            .unwrap();
            let body = serde_json::json!({
                "id": id,
                "name": name,
                "description": format!("The {name} rule."),
                "scope": scope,
                "enabled": enabled,
                "source": format!("/owner/skills/{name}"),
                "folder": folder,
            });
            connection
                .execute(
                    "INSERT INTO plugin_items(id,kind,scope,name,body) VALUES(?1,'skill',?2,?3,?4)",
                    rusqlite::params![id, key, name, body.to_string()],
                )
                .unwrap();
        };
        let own = serde_json::json!({"kind": "project", "project_id": project.as_str()});
        let elsewhere = serde_json::json!({"kind": "project", "project_id": "another-project"});
        skill(
            "s1",
            "zeta",
            serde_json::json!({"kind": "global"}),
            "global",
            true,
        );
        skill("s2", "tdd", own, "project:process-1", true);
        skill(
            "s3",
            "off",
            serde_json::json!({"kind": "global"}),
            "global",
            false,
        );
        skill("s4", "theirs", elsewhere, "project:another-project", true);
    }

    let view = adoption(&root, &project).unwrap();
    assert!(!view.nudges, "nudges are off by default (DEC-111 Q4)");
    assert_eq!(
        view.counted,
        crate::coord::settings::COUNTED_TOOLS_DEFAULT
            .map(str::to_owned)
            .to_vec()
    );
    assert_eq!(
        view.agents
            .iter()
            .map(|row| (row.agent, row.harness.as_str()))
            .collect::<Vec<_>>(),
        vec![(sub, "codex"), (master, "claude-code")],
        "the project's own agents, newest first"
    );
    assert_eq!(
        (view.agents[0].graphify, view.agents[0].context_mode),
        (2, 1)
    );
    assert_eq!(
        (view.agents[1].graphify, view.agents[1].context_mode),
        (0, 0)
    );
    assert_eq!(
        view.skills,
        vec!["tdd".to_owned(), "zeta".to_owned()],
        "the project's enabled skills, its own and global ones, by name"
    );
    assert_eq!(view.agents[0].skills, vec![1, 0]);
    assert_eq!(view.agents[1].skills, vec![0, 0]);
    assert!(
        view.agents
            .iter()
            .all(|row| row.counts.len() == view.counted.len())
    );

    let request: Envelope = serde_json::from_value(serde_json::json!({
        "process": {"kind": "adoption", "project": project.as_str()},
        "text_bytes": null
    }))
    .unwrap();
    assert!(
        matches!(request.process, Action::Adoption { project: ref asked } if *asked == project)
    );
    let wire = serde_json::to_vec(&Reply::Adoption {
        adoption: view.clone(),
    })
    .unwrap();
    match serde_json::from_slice::<Reply>(&wire).unwrap() {
        Reply::Adoption { adoption } => assert_eq!(adoption, view),
        _ => panic!("an adoption reply reads back as one"),
    }

    {
        let mut connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        let tx = connection.transaction().unwrap();
        let spec = crate::protocol::ProjectSpec {
            name: "Process".into(),
            repositories: vec![],
        };
        assert!(matches!(
            crate::storage::create_project(&tx, &project, &spec).unwrap(),
            crate::protocol::Outcome::Applied
        ));
        tx.commit().unwrap();
        let on = crate::coord::settings::CoordSettings {
            nudges: true,
            ..Default::default()
        };
        crate::coord::settings::write(&connection, &project, &on).unwrap();
    }
    assert!(
        adoption(&root, &project).unwrap().nudges,
        "the view shows nudges the owner turned on"
    );

    assert_eq!(
        adoption(&root, &unknown).unwrap_err().0,
        "process_project_unknown"
    );
}
