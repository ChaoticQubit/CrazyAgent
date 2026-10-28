use super::*;
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::storage::versioned::LastWriter;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Barrier;

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    rusqlite::Connection::open(root.join("engine.db")).unwrap();
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

fn read(root: &Path, project: Option<ProjectId>) -> Result<ProcessText, EngineError> {
    match run(root, None, Action::Read { project }, None)? {
        Answer::Current(current) => Ok(current),
        _ => panic!("a read answers the text and its version"),
    }
}

fn save(
    root: &Path,
    project: Option<ProjectId>,
    text: &str,
    version: u64,
) -> Result<SavedText, EngineError> {
    keep(root, project, text, version, None)
}

fn keep(
    root: &Path,
    project: Option<ProjectId>,
    text: &str,
    version: u64,
    opened: Option<u64>,
) -> Result<SavedText, EngineError> {
    let action = Action::Save {
        project,
        version,
        opened,
    };
    match run(root, None, action, Some(text.into()))? {
        Answer::Saved(saved) => Ok(saved),
        _ => panic!("a save answers saved or stale"),
    }
}

fn answered(answer: Answer) -> String {
    match answer {
        Answer::Text(text) => text,
        Answer::Current(current) => current.text,
        _ => panic!("the request answers a text"),
    }
}

fn shown(text: &str, version: u64) -> ProcessText {
    ProcessText {
        text: text.into(),
        version,
    }
}

#[test]
fn the_app_and_a_project_text_are_saved_and_read_through_the_protocol() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);

    assert_eq!(read(&root, None).unwrap(), shown("", 0));
    let saved = save(&root, None, "App rule.\n", 0).unwrap();
    assert!(
        matches!(
            saved,
            SavedText::Saved {
                version: 1,
                size: Some(_)
            }
        ),
        "{saved:?}"
    );
    assert_eq!(read(&root, None).unwrap(), shown("App rule.\n", 1));

    assert_eq!(read(&root, Some(project.clone())).unwrap(), shown("", 0));
    let saved = save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    assert!(
        matches!(
            saved,
            SavedText::Saved {
                version: 1,
                size: Some(_)
            }
        ),
        "{saved:?}"
    );
    assert_eq!(
        fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "Project rule.\n"
    );
    assert_eq!(
        read(&root, Some(project)).unwrap(),
        shown("Project rule.\n", 1)
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
        let text = answered(run(&root, Some(&home), action, None).unwrap());
        assert!(!text.contains("OWNER-GLOBAL-RULE"));
    }
    assert_eq!(crate::agent_home::read_app(&root).unwrap(), "");

    let imported = answered(run(&root, Some(&home), Action::ImportGlobal, None).unwrap());
    assert_eq!(imported, "OWNER-GLOBAL-RULE\n");
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
        answered(run(&root, Some(&home), Action::ImportGlobal, None).unwrap()),
        "LINKED-RULE\n"
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
    let text = answered(run(&root, None, Action::Default, None).unwrap());
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
        run(
            &root,
            None,
            Action::Save {
                project: None,
                version: 0,
                opened: None
            },
            None
        )
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
        save(&root, None, &"a".repeat(TEXT_BYTES_MAX + 1), 0)
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

fn kept_over(root: &Path, project: Option<&ProjectId>) -> Vec<crate::storage::versioned::Force> {
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    crate::storage::versioned::forces(&tx, &crate::storage::versioned::process_text_item(project))
        .unwrap()
}

#[test]
fn stale_process_text_save_is_refused() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);

    for scope in [None, Some(project.clone())] {
        let opened = read(&root, scope.clone()).unwrap();
        assert_eq!(opened, shown("", 0));
        let saved = save(&root, scope.clone(), "Desktop rule.\n", opened.version).unwrap();
        assert!(
            matches!(
                saved,
                SavedText::Saved {
                    version: 1,
                    size: Some(_)
                }
            ),
            "{saved:?}"
        );
        assert_eq!(
            save(&root, scope.clone(), "TUI rule.\n", opened.version).unwrap(),
            SavedText::Stale {
                version: 0,
                current: shown("Desktop rule.\n", 1),
                writer: LastWriter::Owner,
            },
            "the second client is shown the text saved since it opened"
        );
        assert_eq!(
            read(&root, scope.clone()).unwrap(),
            shown("Desktop rule.\n", 1),
            "a refused save changes nothing"
        );
        assert!(kept_over(&root, scope.as_ref()).is_empty());

        for opened in [1, 2] {
            assert_eq!(
                keep(&root, scope.clone(), "TUI rule.\n", 1, Some(opened))
                    .unwrap_err()
                    .0,
                "process_request_invalid",
                "Keep mine is only for a version newer than the one opened"
            );
        }
        let kept = keep(&root, scope.clone(), "TUI rule.\n", 1, Some(0)).unwrap();
        assert!(
            matches!(
                kept,
                SavedText::Saved {
                    version: 2,
                    size: Some(_)
                }
            ),
            "Keep mine writes over the version the conflict showed: {kept:?}"
        );
        assert_eq!(read(&root, scope.clone()).unwrap(), shown("TUI rule.\n", 2));
        let records = kept_over(&root, scope.as_ref());
        assert_eq!(records.len(), 1, "Keep mine is recorded");
        assert_eq!(
            (
                records[0].replaced_version,
                &records[0].replaced_writer,
                records[0].version
            ),
            (1, &LastWriter::Owner, 2)
        );
        assert!(
            matches!(
                keep(&root, scope.clone(), "Late rule.\n", 1, Some(0)).unwrap(),
                SavedText::Stale {
                    version: 1,
                    current: ProcessText { version: 2, .. },
                    ..
                }
            ),
            "Keep mine never writes over a version it was not shown"
        );
        assert_eq!(kept_over(&root, scope.as_ref()).len(), 1);
    }
    assert_eq!(crate::agent_home::read_app(&root).unwrap(), "TUI rule.\n");
    assert_eq!(
        fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "TUI rule.\n"
    );

    let racers = 6;
    let start = Barrier::new(racers);
    let results: Vec<SavedText> = std::thread::scope(|threads| {
        let runs: Vec<_> = (0..racers)
            .map(|index| {
                let (root, start) = (&root, &start);
                threads.spawn(move || {
                    start.wait();
                    save(root, None, &format!("Racer {index}.\n"), 2).unwrap()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    let won: Vec<usize> = (0..racers)
        .filter(|index| matches!(results[*index], SavedText::Saved { version: 3, .. }))
        .collect();
    assert_eq!(won.len(), 1, "two saves never both win: {results:?}");
    let winner = format!("Racer {}.\n", won[0]);
    for (index, result) in results.iter().enumerate() {
        if index != won[0] {
            assert_eq!(
                result,
                &SavedText::Stale {
                    version: 2,
                    current: shown(&winner, 3),
                    writer: LastWriter::Owner,
                },
                "every other save is shown the winner's text"
            );
        }
    }
    assert_eq!(read(&root, None).unwrap(), shown(&winner, 3));

    fs::remove_dir_all(folder.join(".crazyagent")).unwrap();
    assert_eq!(
        save(&root, Some(project.clone()), "Lost rule.\n", 2)
            .unwrap_err()
            .0,
        "process_project_folder_missing"
    );
    assert_eq!(
        read(&root, Some(project)).unwrap(),
        shown("", 2),
        "a save whose file could not be written keeps the version"
    );
}

fn agent_read(root: &Path, project: &ProjectId, agent: i64) -> Result<ProcessText, EngineError> {
    let action = Action::AgentText {
        project: project.clone(),
        agent,
    };
    match run(root, None, action, None)? {
        Answer::Current(current) => Ok(current),
        _ => panic!("an agent text read answers the text and its version"),
    }
}

fn agent_save(
    root: &Path,
    project: &ProjectId,
    agent: i64,
    text: &str,
    version: u64,
    opened: Option<u64>,
) -> Result<SavedText, EngineError> {
    let action = Action::SaveAgentText {
        project: project.clone(),
        agent,
        version,
        opened,
    };
    match run(root, None, action, Some(text.into()))? {
        Answer::Saved(saved) => Ok(saved),
        _ => panic!("an agent text save answers saved or stale"),
    }
}

#[test]
fn stale_agent_instructions_save_is_refused() {
    use crate::storage::{agent_text, versioned};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    let agent = team_agent(&root, project.as_str(), "sub", "codex");
    let elsewhere = team_agent(&root, "process-2", "sub", "codex");

    assert_eq!(agent_read(&root, &project, agent).unwrap(), shown("", 0));
    let parser = "Only touch the parser.\n";
    assert_eq!(
        agent_save(&root, &project, agent, parser, 0, None).unwrap(),
        SavedText::Saved {
            version: 1,
            size: None
        }
    );
    assert_eq!(
        agent_save(&root, &project, agent, "Write the docs.\n", 0, None).unwrap(),
        SavedText::Stale {
            version: 0,
            current: shown(parser, 1),
            writer: LastWriter::Owner,
        },
        "a save from a screen opened before the last save is shown that save"
    );
    assert_eq!(
        agent_read(&root, &project, agent).unwrap(),
        shown(parser, 1)
    );
    assert_eq!(
        agent_save(&root, &project, agent, "Write the docs.\n", 1, Some(1))
            .unwrap_err()
            .0,
        "process_request_invalid"
    );
    assert_eq!(
        agent_save(&root, &project, agent, "Write the docs.\n", 1, Some(0)).unwrap(),
        SavedText::Saved {
            version: 2,
            size: None
        },
        "Keep mine writes over the version the conflict showed"
    );
    let item = versioned::agent_instructions_item(&project, agent);
    {
        let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let records = versioned::forces(&tx, &item).unwrap();
        assert_eq!(
            (
                records.len(),
                records[0].replaced_version,
                records[0].version
            ),
            (1, 1, 2),
            "Keep mine is recorded"
        );
    }

    let mut connection = versioned::open(&root).unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let from_agent = versioned::Writer::Agent(crate::terminal::AgentId::team(agent).unwrap());
    assert_eq!(
        agent_text::save(&tx, &project, agent, "Agent edit.\n", 1, &from_agent),
        Err(versioned::Refusal::Stale(versioned::Stale {
            version: 1,
            current: 2,
            writer: LastWriter::Owner,
        })),
        "an agent writing over a version it did not read is refused"
    );
    assert_eq!(
        agent_text::read(&tx, &project, agent).unwrap(),
        ("Write the docs.\n".to_owned(), 2)
    );
    drop(tx);

    for (asked, code) in [
        (elsewhere, "process_agent_unknown"),
        (agent + 100, "process_agent_unknown"),
        (0, "process_request_invalid"),
    ] {
        assert_eq!(agent_read(&root, &project, asked).unwrap_err().0, code);
        assert_eq!(
            agent_save(&root, &project, asked, "x", 0, None)
                .unwrap_err()
                .0,
            code
        );
    }
    assert_eq!(
        agent_save(
            &root,
            &project,
            agent,
            &"a".repeat(TEXT_BYTES_MAX + 1),
            2,
            None
        )
        .unwrap_err()
        .0,
        "process_text_too_large"
    );
    assert_eq!(
        agent_read(&root, &project, agent).unwrap(),
        shown("Write the docs.\n", 2),
        "a refused save changes nothing"
    );

    let given = crate::agent_home::with_agent(
        crate::agent_home::render("App rule.", "Project rule."),
        "Write the docs.\n",
    );
    let (app, own, mine) = (
        given.find("App rule.").unwrap(),
        given.find("Project rule.").unwrap(),
        given.find("Write the docs.").unwrap(),
    );
    assert!(
        app < own && own < mine,
        "the agent's own text comes last: {given}"
    );
    assert!(given.contains("## Instructions for this agent"));

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    agent_text::forget_project(&tx, &project).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        agent_read(&root, &project, agent).unwrap(),
        shown("", 0),
        "a removed project leaves no agent text or version behind"
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
