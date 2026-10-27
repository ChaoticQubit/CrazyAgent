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
    run(root, None, Action::Read { project }, None)
}

fn save(
    root: &Path,
    project: Option<ProjectId>,
    text: &str,
) -> Result<Option<String>, EngineError> {
    run(root, None, Action::Save { project }, Some(text.into()))
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
        let text = run(&root, Some(&home), action, None).unwrap().unwrap();
        assert!(!text.contains("OWNER-GLOBAL-RULE"));
    }
    assert_eq!(crate::agent_home::read_app(&root).unwrap(), "");

    let imported = run(&root, Some(&home), Action::ImportGlobal, None).unwrap();
    assert_eq!(imported.as_deref(), Some("OWNER-GLOBAL-RULE\n"));
    assert_eq!(
        crate::agent_home::read_app(&root).unwrap(),
        "",
        "an import is a preview; only a save stores it"
    );

    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        run(&root, Some(&empty), Action::ImportGlobal, None)
            .unwrap_err()
            .0,
        "process_import_missing"
    );
    assert_eq!(
        run(&root, None, Action::ImportGlobal, None).unwrap_err().0,
        "process_import_missing"
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
    assert_eq!(
        run(&root, Some(&home), Action::ImportGlobal, None)
            .unwrap()
            .as_deref(),
        Some("LINKED-RULE\n")
    );
    fs::write(dotfiles.join("AGENTS.md"), "a".repeat(TEXT_BYTES_MAX + 1)).unwrap();
    assert_eq!(
        run(&root, Some(&home), Action::ImportGlobal, None)
            .unwrap_err()
            .0,
        "process_text_too_large"
    );
}

#[test]
fn the_default_process_is_plain_bounded_and_keeps_the_owner_rules() {
    let (_directory, root) = root();
    let text = run(&root, None, Action::Default, None).unwrap().unwrap();
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
        "Never add a Co-Authored-By trailer",
        "Never put a secret",
        "asks the master",
        "escalates to the owner only for security, destructive or irreversible actions, spending, and product or scope decisions",
    ] {
        assert!(text.contains(rule), "the default process misses: {rule}");
    }
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
        run(&root, None, Action::Save { project: None }, None)
            .unwrap_err()
            .0,
        "process_request_invalid"
    );
    assert_eq!(
        run(&root, None, Action::Default, Some("x".into()))
            .unwrap_err()
            .0,
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
        }
    );

    let given = AgentProject {
        data: root.clone(),
        id: project.clone(),
        folder: Some(folder.clone()),
    };
    let sub_home = launched_home(&root, sub);
    prepare(&sub_home, Harness::Codex, &given, "# Sub rules\n").unwrap();
    record_skills(&sub_home, &["tdd", "code-review"]).unwrap();
    assert_eq!(
        agent_instructions(&root, &project, Some(sub)).unwrap(),
        AgentInstructions {
            agent: sub,
            harness: "codex".into(),
            launched: true,
            text: "# Sub rules\n".into(),
            skills: vec!["tdd".into(), "code-review".into()],
        }
    );

    let master_home = launched_home(&root, master);
    prepare(&master_home, Harness::Claude, &given, "# Master rules\n").unwrap();
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
    let found: std::collections::BTreeSet<&str> = [
        include_str!("../../../agent_home.rs"),
        include_str!("process.rs"),
    ]
    .into_iter()
    .flat_map(refusals)
    .collect();
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
