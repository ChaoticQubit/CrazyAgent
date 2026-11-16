use super::*;
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::storage::versioned::LastWriter;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Barrier;

/// Makes a temporary engine folder whose `engine.db` is in WAL and written in it, as an engine
/// leaves it. The macOS system SQLite refuses a writer at once in a rollback journal while a
/// read-only connection holds a read, and refuses read-only connections on a WAL file never
/// written in WAL. No engine meets either: it refuses a store that does not take WAL
/// (`database_wal_refused`). Output: the folder's guard and its canonical path.
fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let mode: String = database
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    database
        .execute_batch("CREATE TABLE written_in_wal(x); DROP TABLE written_in_wal;")
        .unwrap();
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
    match run(root, Action::Read { project }, None)? {
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
    match run(root, action, Some(text.into()))? {
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
        outside: false,
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
        let text = answered(run(&root, action, None).unwrap());
        assert!(!text.contains("OWNER-GLOBAL-RULE"));
    }
    assert_eq!(crate::agent_home::read_app(&root).unwrap(), "");

    let path = home.join(".claude/CLAUDE.md");
    let imported = answered(run(&root, Action::ReadFile { path }, None).unwrap());
    assert_eq!(imported, "OWNER-GLOBAL-RULE\n");
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
        answered(run(&root, Action::ReadFile { path: path.clone() }, None).unwrap()),
        "LINKED-RULE\n"
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
    let text = answered(run(&root, Action::Default, None).unwrap());
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
        run(
            &root,
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
        run(&root, Action::Default, Some("x".into())).unwrap_err().0,
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
    match run(root, action, None)? {
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
    match run(root, action, Some(text.into()))? {
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
        crate::agent_home::render(
            "App rule.",
            "Project rule.",
            crate::agent_home::Repeated::LeaveOut,
        ),
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

#[test]
fn the_store_under_test_runs_the_journal_mode_the_engine_keeps_it_in() {
    let (_directory, root) = root();
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let mode: String = database
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

/// Saves made at once from six screens each name the version they read: a save lands only
/// on the version it read and reads back what it stored at the next one, every other save
/// is refused as stale, and the stored version counts exactly the saves that landed (Q35).
#[test]
fn saves_at_once_land_only_on_the_version_they_read() {
    use crate::instruction_files::{EditDelivery, Mode};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, None, "App rule.\n", 0).unwrap();
    save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    let options: Vec<InstructionSettings> = [Mode::ProjectOnly, Mode::AppOnly, Mode::Both]
        .into_iter()
        .flat_map(|mode| [true, false].map(|transcripts| (mode, transcripts)))
        .zip(
            [
                EditDelivery::NextMessage,
                EditDelivery::Restart,
                EditDelivery::Badge,
            ]
            .into_iter()
            .cycle(),
        )
        .map(|((mode, transcripts), edits)| InstructionSettings {
            mode,
            transcripts,
            edits,
            ..Default::default()
        })
        .collect();
    assert_eq!(options.len(), 6);
    let start = Barrier::new(options.len());
    let landed: u64 = std::thread::scope(|scope| {
        let saves: Vec<_> = options
            .iter()
            .map(|&settings| {
                let (root, project, start) = (&root, &project, &start);
                scope.spawn(move || {
                    start.wait();
                    let mut landed = 0_u64;
                    for _ in 0..100 {
                        let read = instruction_settings(root, project, None).unwrap();
                        match instruction_settings(
                            root,
                            project,
                            Some((settings, Some(read.version))),
                        ) {
                            Ok(saved) => {
                                assert_eq!(
                                    saved,
                                    InstructionView {
                                        settings,
                                        version: read.version + 1,
                                        repeats: 0,
                                    }
                                );
                                landed += 1;
                            }
                            Err(refused) => assert_eq!(refused.0, versioned::STALE_CODE),
                        }
                    }
                    landed
                })
            })
            .collect();
        saves
            .into_iter()
            .map(|save| {
                save.join()
                    .expect("every save lands on the version it read or is refused as stale")
            })
            .sum()
    });
    assert!(
        landed >= 1,
        "a save on the version no other save moved always lands"
    );
    let now = instruction_settings(&root, &project, None).unwrap();
    assert_eq!(now.version, landed);
    assert!(options.contains(&now.settings));
}

/// A project's agents get each line the app text already has once, the settings read counts
/// those lines, and the project's Keep choice gives both texts whole again; a save reports
/// the size of the text agents then get (DEC-129 item 5, DEC-132 Q7).
#[test]
fn a_project_leaves_out_lines_the_app_text_has_until_its_setting_says_keep() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    let own = "- Use TDD.\n- No em dash.\n- Run make check.\n";
    save(&root, None, "- Use TDD.\n- No em dash.\n", 0).unwrap();
    let sized = |saved: SavedText| match saved {
        SavedText::Saved {
            size: Some(size), ..
        } => size.bytes as usize,
        other => panic!("{other:?}"),
    };
    let left_out = sized(save(&root, Some(project.clone()), own, 0).unwrap());
    let fresh = instruction_settings(&root, &project, None).unwrap();
    assert_eq!(fresh.settings.repeated, Repeated::LeaveOut);
    assert_eq!(fresh.repeats, 2);
    let given = given_text(&root, &project).unwrap();
    assert_eq!(given.matches("- Use TDD.").count(), 1);
    assert!(given.ends_with("## Project instructions\n\n- Run make check.\n"));
    assert_eq!(left_out, given.len());
    let keep = InstructionSettings {
        repeated: Repeated::Keep,
        ..fresh.settings
    };
    let kept = instruction_settings(&root, &project, Some((keep, Some(fresh.version)))).unwrap();
    assert_eq!((kept.settings.repeated, kept.repeats), (Repeated::Keep, 2));
    let given = given_text(&root, &project).unwrap();
    assert_eq!(given.matches("- Use TDD.").count(), 2);
    assert_eq!(
        sized(save(&root, Some(project.clone()), own, 1).unwrap()),
        given.len()
    );
    assert!(given.len() > left_out);
    let alone = InstructionSettings {
        mode: Mode::ProjectOnly,
        ..fresh.settings
    };
    let alone = instruction_settings(&root, &project, Some((alone, Some(kept.version)))).unwrap();
    assert_eq!(
        alone.repeats, 0,
        "a project that gets one text has no repeat"
    );
    assert_eq!(
        given_text(&root, &project).unwrap(),
        format!("# Instructions from CrazyAgent\n\n## Project instructions\n\n{own}")
    );
}

/// A save from an older version is refused as stale and leaves the stored settings and
/// version as they were; a save naming no version is refused while the stored settings are
/// readable and replaces them, at the next version, once they are not (Q35).
#[test]
fn a_stale_settings_save_changes_nothing_and_only_unreadable_settings_take_a_save_with_no_version()
{
    use crate::instruction_files::{EditDelivery, Mode};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, None, "App rule.\n", 0).unwrap();
    save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    let fresh = instruction_settings(&root, &project, None).unwrap();
    assert_eq!(
        fresh,
        InstructionView {
            settings: InstructionSettings::default(),
            version: 0,
            repeats: 0,
        }
    );
    let none = InstructionSettings {
        mode: Mode::ProjectOnly,
        transcripts: false,
        edits: EditDelivery::Restart,
        ..Default::default()
    };
    let saved = instruction_settings(&root, &project, Some((none, Some(0)))).unwrap();
    assert_eq!(
        saved,
        InstructionView {
            settings: none,
            version: 1,
            repeats: 0,
        }
    );
    let user = InstructionSettings {
        mode: Mode::AppOnly,
        transcripts: true,
        edits: EditDelivery::Badge,
        ..Default::default()
    };
    assert_eq!(
        instruction_settings(&root, &project, Some((user, Some(0))))
            .unwrap_err()
            .0,
        versioned::STALE_CODE
    );
    assert_eq!(
        instruction_settings(&root, &project, Some((user, None)))
            .unwrap_err()
            .0,
        versioned::STALE_CODE,
        "readable settings are never replaced by a save that read no version"
    );
    assert_eq!(instruction_settings(&root, &project, None).unwrap(), saved);

    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    connection
        .execute(
            "UPDATE agent_instruction_settings SET mode='bogus' WHERE project=?1",
            [project.as_str()],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        instruction_settings(&root, &project, None).unwrap_err().0,
        "instruction_settings_invalid"
    );
    assert_eq!(
        instruction_settings(&root, &project, Some((user, None))).unwrap(),
        InstructionView {
            settings: user,
            version: 2,
            repeats: 0,
        },
        "unreadable settings are replaced at the next version"
    );
    assert_eq!(
        instruction_settings(&root, &project, Some((none, None)))
            .unwrap_err()
            .0,
        versioned::STALE_CODE
    );
}

#[test]
fn a_save_the_store_keeps_busy_past_its_wait_saves_nothing_says_so_and_lands_when_saved_again() {
    use crate::instruction_files::{EditDelivery, Mode};
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    let agent = team_agent(&root, project.as_str(), "sub", "codex");
    let settings = InstructionSettings {
        mode: Mode::AppOnly,
        transcripts: true,
        edits: EditDelivery::Badge,
        ..Default::default()
    };
    assert_ne!(settings, InstructionSettings::default());
    let save_settings = || -> Result<InstructionView, EngineError> {
        let action = Action::SaveSettings {
            project: project.clone(),
            settings,
            version: Some(0),
        };
        match run(&root, action, None)? {
            Answer::Settings(view) => Ok(view),
            _ => panic!("a settings save answers the settings"),
        }
    };
    let writer = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        save(&root, None, "App rule.\n", 0).unwrap_err().0,
        "process_save_unavailable"
    );
    assert_eq!(
        agent_save(&root, &project, agent, "Only touch the parser.\n", 0, None)
            .unwrap_err()
            .0,
        "process_save_unavailable"
    );
    assert_eq!(
        save_settings().unwrap_err().0,
        "instruction_settings_save_unavailable"
    );
    writer.execute_batch("ROLLBACK").unwrap();

    assert_eq!(
        read(&root, None).unwrap(),
        shown("", 0),
        "nothing was saved"
    );
    assert_eq!(agent_read(&root, &project, agent).unwrap(), shown("", 0));
    assert_eq!(
        instruction_settings(&root, &project, None).unwrap(),
        InstructionView {
            settings: InstructionSettings::default(),
            version: 0,
            repeats: 0,
        }
    );
    let saved = save(&root, None, "App rule.\n", 0).unwrap();
    assert!(
        matches!(saved, SavedText::Saved { version: 1, .. }),
        "saving again lands: {saved:?}"
    );
    assert_eq!(
        agent_save(&root, &project, agent, "Only touch the parser.\n", 0, None).unwrap(),
        SavedText::Saved {
            version: 1,
            size: None
        }
    );
    assert_eq!(
        save_settings().unwrap(),
        InstructionView {
            settings,
            version: 1,
            repeats: 0,
        }
    );
}

#[cfg(unix)]
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
        repeated: crate::agent_home::Repeated::LeaveOut,
    };
    let text = |text: &str| crate::agent_home::Given {
        text: text.into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    let sub_home = launched_home(&root, sub);
    prepare(
        &sub_home,
        Harness::Codex,
        &given,
        &text("# Sub rules\n"),
        crate::agent_home::Caches::default(),
    )
    .unwrap();
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
        crate::agent_home::Caches::default(),
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
        repeated: crate::agent_home::Repeated::LeaveOut,
    };
    prepare(
        &home,
        Harness::Claude,
        &launch,
        &given,
        crate::agent_home::Caches::default(),
    )
    .unwrap();
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
        repeated: crate::agent_home::Repeated::LeaveOut,
    };
    let given = Given {
        text: "# Rules\n".into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    prepare(
        &home,
        Harness::Codex,
        &launch,
        &given,
        crate::agent_home::Caches::default(),
    )
    .unwrap();
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

fn given(root: &Path, project: &ProjectId) -> Result<String, EngineError> {
    run(
        root,
        Action::Given {
            project: project.clone(),
        },
        None,
    )
    .map(answered)
}

/// What the Process page shows as what agents read is what a launch gives them now: the
/// engine's own render for the project's mode, with its title and headings. A part the
/// engine cannot read refuses the read with that part's code, so the page never shows the
/// rest as all of it.
#[test]
fn the_text_agents_get_is_the_engines_render_for_the_projects_mode() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, None, "App rule.\n", 0).unwrap();
    save(&root, Some(project.clone()), "  Project rule.\n\n", 0).unwrap();
    let both = given(&root, &project).unwrap();
    assert_eq!(
        both,
        crate::agent_home::render(
            "App rule.\n",
            "  Project rule.\n\n",
            crate::agent_home::Repeated::LeaveOut
        )
    );
    assert!(
        both.starts_with("# Instructions from CrazyAgent\n") && both.contains("Project rule.\n"),
        "{both}"
    );
    let mut version = instruction_settings(&root, &project, None).unwrap().version;
    for (mode, app, own) in [
        (Mode::ProjectOnly, "", "  Project rule.\n\n"),
        (Mode::AppOnly, "App rule.\n", ""),
        (Mode::Both, "App rule.\n", "  Project rule.\n\n"),
    ] {
        let settings = InstructionSettings {
            mode,
            ..InstructionSettings::default()
        };
        version = instruction_settings(&root, &project, Some((settings, Some(version))))
            .unwrap()
            .version;
        assert_eq!(
            given(&root, &project).unwrap(),
            crate::agent_home::render(app, own, crate::agent_home::Repeated::LeaveOut),
            "{mode:?}"
        );
    }
    let app = root.join(crate::agent_home::APP_FILE);
    fs::remove_file(&app).unwrap();
    fs::write(root.join("elsewhere.md"), "Other rule.\n").unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere.md"), &app).unwrap();
    assert_eq!(given(&root, &project).unwrap_err().0, "process_text_unsafe");
    let unknown = ProjectId::new("not-set-up").unwrap();
    assert_eq!(
        given(&root, &unknown).unwrap_err().0,
        "process_project_unknown"
    );
}

/// The refusal a save gets when the project's agents would get no instructions after it.
const NONE_GIVEN_CODE: &str = "process_instructions_would_be_empty";

/// Saves one project's Delivery mode at the version read, keeping its other settings.
/// Inputs: the engine root, the project and the mode. Output: what the save answered.
fn switch(
    root: &Path,
    project: &ProjectId,
    mode: crate::instruction_files::Mode,
) -> Result<InstructionView, EngineError> {
    let read = instruction_settings(root, project, None).unwrap();
    let settings = InstructionSettings {
        mode,
        ..read.settings
    };
    instruction_settings(root, project, Some((settings, Some(read.version))))
}

/// A Delivery switch after which the project's agents would get no instructions at all is
/// refused by the engine, whichever client sends it, and changes nothing: Project only on
/// an empty project file, App only on an empty app text. A change that keeps the mode, and
/// a switch to a mode whose text is there, still save.
#[test]
fn a_delivery_switch_that_leaves_agents_no_instructions_is_refused() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    let fresh = instruction_settings(&root, &project, None).unwrap();
    let quiet = InstructionSettings {
        transcripts: !fresh.settings.transcripts,
        ..fresh.settings
    };
    let kept = instruction_settings(&root, &project, Some((quiet, Some(fresh.version)))).unwrap();
    assert_eq!(kept.settings, quiet, "a change that keeps the mode saves");

    save(&root, None, "App rule.\n", 0).unwrap();
    let before = instruction_settings(&root, &project, None).unwrap();
    let given_before = given(&root, &project).unwrap();
    assert_eq!(
        given_before,
        crate::agent_home::render("App rule.\n", "", crate::agent_home::Repeated::LeaveOut)
    );
    assert_eq!(
        switch(&root, &project, Mode::ProjectOnly),
        Err(EngineError(NONE_GIVEN_CODE.into())),
        "Project only on an empty project file"
    );
    assert_eq!(instruction_settings(&root, &project, None).unwrap(), before);
    assert_eq!(given(&root, &project).unwrap(), given_before);

    save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    assert_eq!(
        switch(&root, &project, Mode::ProjectOnly)
            .unwrap()
            .settings
            .mode,
        Mode::ProjectOnly,
        "Project only saves once the project file has text"
    );
    assert!(
        matches!(
            save(&root, None, "", 1),
            Ok(SavedText::Saved { version: 2, .. })
        ),
        "an app text no agent of the project reads may be cleared"
    );
    let before = instruction_settings(&root, &project, None).unwrap();
    assert_eq!(
        switch(&root, &project, Mode::AppOnly),
        Err(EngineError(NONE_GIVEN_CODE.into())),
        "App only on an empty app text"
    );
    assert_eq!(instruction_settings(&root, &project, None).unwrap(), before);
    assert_eq!(
        given(&root, &project).unwrap(),
        crate::agent_home::render("", "Project rule.\n", crate::agent_home::Repeated::LeaveOut)
    );
}

/// An app text save after which a project's agents would get no instructions at all is
/// refused and keeps the saved text and its version: a blank app text under App only, or
/// under Both on an empty project file. With the project's own text there, the app text
/// may be cleared.
#[test]
fn an_app_text_save_that_leaves_agents_no_instructions_is_refused() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, None, "App rule.\n", 0).unwrap();
    switch(&root, &project, Mode::AppOnly).unwrap();
    for blank in ["", " \n\n"] {
        assert_eq!(
            save(&root, None, blank, 1),
            Err(EngineError(NONE_GIVEN_CODE.into())),
            "App only: {blank:?}"
        );
    }
    assert_eq!(read(&root, None).unwrap(), shown("App rule.\n", 1));

    switch(&root, &project, Mode::Both).unwrap();
    assert_eq!(
        save(&root, None, "", 1),
        Err(EngineError(NONE_GIVEN_CODE.into())),
        "Both on an empty project file"
    );
    assert_eq!(read(&root, None).unwrap(), shown("App rule.\n", 1));
    assert_eq!(
        given(&root, &project).unwrap(),
        crate::agent_home::render("App rule.\n", "", crate::agent_home::Repeated::LeaveOut)
    );

    save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    assert!(
        matches!(
            save(&root, None, "", 1),
            Ok(SavedText::Saved { version: 2, .. })
        ),
        "with the project's own text the app text may be cleared"
    );
    assert_eq!(
        given(&root, &project).unwrap(),
        crate::agent_home::render("", "Project rule.\n", crate::agent_home::Repeated::LeaveOut)
    );
}

/// A project text save after which the project's agents would get no instructions at all
/// is refused by the engine, which knows the mode even when the page could not read it: a
/// blank text under Project only, or under Both with an empty app text. A project text the
/// mode does not give may be cleared.
#[test]
fn a_project_text_save_that_leaves_agents_no_instructions_is_refused() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, Some(project.clone()), "Project rule.\n", 0).unwrap();
    switch(&root, &project, Mode::ProjectOnly).unwrap();
    assert_eq!(
        save(&root, Some(project.clone()), "", 1),
        Err(EngineError(NONE_GIVEN_CODE.into())),
        "Project only"
    );
    assert_eq!(
        read(&root, Some(project.clone())).unwrap(),
        shown("Project rule.\n", 1)
    );

    switch(&root, &project, Mode::Both).unwrap();
    assert_eq!(
        save(&root, Some(project.clone()), "\n", 1),
        Err(EngineError(NONE_GIVEN_CODE.into())),
        "Both with an empty app text"
    );
    assert_eq!(
        given(&root, &project).unwrap(),
        crate::agent_home::render("", "Project rule.\n", crate::agent_home::Repeated::LeaveOut)
    );

    save(&root, None, "App rule.\n", 0).unwrap();
    switch(&root, &project, Mode::AppOnly).unwrap();
    assert!(
        matches!(
            save(&root, Some(project.clone()), "", 1),
            Ok(SavedText::Saved { version: 2, .. })
        ),
        "App only does not give the project text, so it may be cleared"
    );
    assert_eq!(
        given(&root, &project).unwrap(),
        crate::agent_home::render("App rule.\n", "", crate::agent_home::Repeated::LeaveOut)
    );
}

/// Reads the plain codes a source file refuses with, before its tests, whichever of
/// `#[cfg(test)]` or `#[cfg(all(test, ...))]` gates them. Input: the file text. Output:
/// each `process_` code once.
fn refusals(source: &str) -> std::collections::BTreeSet<&str> {
    let end = ["#[cfg(test)]", "#[cfg(all(test"]
        .into_iter()
        .filter_map(|gate| source.find(gate))
        .min()
        .unwrap_or(source.len());
    let code = &source[..end];
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

#[cfg(unix)]
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
    assert_eq!(
        view.nudges,
        crate::coord::settings::Nudges::Off,
        "nudges are off by default (DEC-111 Q4)"
    );
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
            nudges: crate::coord::settings::Nudges::TenMinutes,
            ..Default::default()
        };
        crate::coord::settings::write(&connection, &project, &on).unwrap();
    }
    assert_eq!(
        adoption(&root, &project).unwrap().nudges,
        crate::coord::settings::Nudges::TenMinutes,
        "the view shows nudges the owner turned on"
    );

    assert_eq!(
        adoption(&root, &unknown).unwrap_err().0,
        "process_project_unknown"
    );
}

fn cleanup_project(root: &Path) -> ProjectId {
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    with_project(root, &folder)
}

fn never(_: &str) -> Result<String, &'static str> {
    panic!("no text may reach a model here");
}

#[test]
fn a_project_proposal_holds_both_texts_keeps_the_version_and_writes_nothing() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, None, "- Use TDD.\n", 0).unwrap();
    let own = "- Use TDD.\n- Run make check.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let before = current_text(&root, Some(&project)).unwrap();
    let mut prompts = Vec::new();
    let proposed = propose(&root, Some(&project), None, |prompt| {
        prompts.push(prompt.to_owned());
        Ok("Done.\n~~~\n- Run make check.\n~~~\n".to_owned())
    })
    .unwrap();
    assert_eq!(proposed.text, "- Run make check.\n");
    assert_eq!(proposed.version, before.version);
    let had = "The app instructions already have this line.";
    let found = vec![Finding::new(FindingKind::Duplicate, vec![1], had)];
    assert_eq!(proposed.findings, found, "the check rides along");
    assert_eq!(prompts.len(), 1, "one model run per proposal");
    assert!(
        prompts[0].contains("App instructions file, for comparison only:\n~~~\n- Use TDD.\n~~~\n")
    );
    assert!(prompts[0].contains(&format!(
        "The project instructions file to rewrite:\n~~~\n{own}~~~\n"
    )));
    assert_eq!(
        current_text(&root, Some(&project)).unwrap(),
        before,
        "a proposal saves nothing"
    );
    assert_eq!(current_text(&root, None).unwrap().text, "- Use TDD.\n");
}

#[test]
fn a_project_whose_agents_get_its_file_alone_never_sends_the_app_text() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, None, "- App secret rule.\n", 0).unwrap();
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    let fresh = instruction_settings(&root, &project, None).unwrap();
    let alone = InstructionSettings {
        mode: Mode::ProjectOnly,
        ..fresh.settings
    };
    instruction_settings(&root, &project, Some((alone, Some(fresh.version)))).unwrap();
    let mut seen = String::new();
    propose(&root, Some(&project), None, |prompt| {
        seen = prompt.to_owned();
        Ok("~~~\n- Run make check.\n~~~".to_owned())
    })
    .unwrap();
    assert!(!seen.contains("App secret rule"));
    assert!(!seen.contains("repeat the app instructions file"));
}

#[test]
fn with_the_app_wide_setting_off_a_project_proposal_never_sends_the_app_text() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, None, "- App secret rule.\n", 0).unwrap();
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    let off = AppInstructionSettings {
        edit_with_ai: false,
    };
    let propose_once = |root: &Path| {
        let mut seen = String::new();
        let proposed = propose(root, Some(&project), None, |prompt| {
            seen = prompt.to_owned();
            Ok("~~~\n- Run make check.\n~~~".to_owned())
        })
        .unwrap();
        assert_eq!(proposed.text, "- Run make check.\n");
        seen
    };
    let on = propose_once(&root);
    assert!(
        on.contains("- App secret rule.") && on.contains("- Run make check."),
        "On, a project whose agents get both texts compares them: {on}"
    );
    assert_eq!(app_settings(&root, Some(off)).unwrap(), off);
    let seen = propose_once(&root);
    assert!(
        !seen.contains("App secret rule"),
        "Off, the app text is sent to no model: {seen}"
    );
    assert!(!seen.contains("repeat the app instructions file"), "{seen}");
    assert!(seen.contains("- Run make check."), "{seen}");
    assert!(
        instruction_settings(&root, &project, None)
            .unwrap()
            .settings
            .edit_with_ai,
        "the project's own setting stays on and still governs its editor"
    );
}

#[test]
fn the_app_proposal_needs_the_picked_pool_and_a_project_takes_none() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let pool = CredentialRef::new("pool-a").unwrap();
    save(&root, None, "- Use TDD.\n- Use TDD.\n", 0).unwrap();
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    assert_eq!(
        propose(&root, None, None, never).unwrap_err().0,
        "instructions_cleanup_pool_needed"
    );
    assert_eq!(
        propose(&root, Some(&project), Some(&pool), never)
            .unwrap_err()
            .0,
        "instructions_cleanup_pool_not_asked"
    );
    let mut seen = String::new();
    let proposed = propose(&root, None, Some(&pool), |prompt| {
        seen = prompt.to_owned();
        Ok("~~~\n- Use TDD.\n~~~\n".to_owned())
    })
    .unwrap();
    assert_eq!(proposed.text, "- Use TDD.\n");
    assert_eq!(proposed.version, current_text(&root, None).unwrap().version);
    assert!(
        seen.contains("The app instructions file to rewrite:\n~~~\n- Use TDD.\n- Use TDD.\n~~~\n")
    );
    assert!(
        !seen.contains("Run make check"),
        "the app run never holds a project's text"
    );
}

#[test]
fn edit_with_ai_off_refuses_its_own_scope_before_any_text_reaches_a_model() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let pool = CredentialRef::new("pool-a").unwrap();
    let reply = |_: &str| Ok("~~~\n- Kept.\n~~~".to_owned());
    save(&root, None, "- Use TDD.\n", 0).unwrap();
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    assert_eq!(
        app_settings(&root, None).unwrap(),
        AppInstructionSettings { edit_with_ai: true },
        "on until the owner turns it off"
    );
    let fresh = instruction_settings(&root, &project, None).unwrap();
    assert!(fresh.settings.edit_with_ai);
    assert!(propose(&root, None, Some(&pool), reply).is_ok(), "app On");
    assert!(
        propose(&root, Some(&project), None, reply).is_ok(),
        "project On"
    );

    let off = AppInstructionSettings {
        edit_with_ai: false,
    };
    assert_eq!(app_settings(&root, Some(off)).unwrap(), off);
    assert_eq!(app_settings(&root, None).unwrap(), off);
    assert_eq!(
        propose(&root, None, Some(&pool), never).unwrap_err().0,
        "instructions_cleanup_off",
        "app Off"
    );
    assert!(
        propose(&root, Some(&project), None, reply).is_ok(),
        "the app-wide Off leaves a project's own setting alone"
    );

    let on = AppInstructionSettings { edit_with_ai: true };
    assert_eq!(app_settings(&root, Some(on)).unwrap(), on);
    let project_off = InstructionSettings {
        edit_with_ai: false,
        ..fresh.settings
    };
    let stored =
        instruction_settings(&root, &project, Some((project_off, Some(fresh.version)))).unwrap();
    assert!(!stored.settings.edit_with_ai);
    assert_eq!(
        propose(&root, Some(&project), None, never).unwrap_err().0,
        "instructions_cleanup_off",
        "project Off"
    );
    assert!(
        propose(&root, None, Some(&pool), reply).is_ok(),
        "a project's Off leaves the app editor alone"
    );
}

#[test]
fn an_empty_file_a_failed_run_and_an_unusable_reply_change_nothing() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    assert_eq!(
        propose(&root, Some(&project), None, never).unwrap_err().0,
        "instructions_cleanup_empty"
    );
    save(&root, None, "- Use TDD.\n", 0).unwrap();
    save(&root, Some(project.clone()), " \n\n", 0).unwrap();
    assert_eq!(
        propose(&root, Some(&project), None, never).unwrap_err().0,
        "instructions_cleanup_empty"
    );
    let version = current_text(&root, Some(&project)).unwrap().version;
    save(&root, Some(project.clone()), "- Run make check.\n", version).unwrap();
    let before = current_text(&root, Some(&project)).unwrap();
    for (ending, code) in [
        (Err(cleanup::FAILED), "instructions_cleanup_failed"),
        (Err(cleanup::TIMED_OUT), "instructions_cleanup_timed_out"),
        (Err(cleanup::BUSY), "instructions_cleanup_busy"),
        (Err(cleanup::NO_ACCOUNT), "instructions_cleanup_no_account"),
        (Err(cleanup::PAUSED), "instructions_cleanup_paused"),
        (
            Err(cleanup::PROJECT_POOL_MISSING),
            "instructions_cleanup_project_pool_missing",
        ),
        (
            Err(cleanup::POOL_UNKNOWN),
            "instructions_cleanup_pool_unknown",
        ),
        (
            Err(cleanup::UNAVAILABLE),
            "instructions_cleanup_unavailable",
        ),
        (
            Err(cleanup::PROVIDER_UNREACHABLE),
            "instructions_cleanup_provider_unreachable",
        ),
        (
            Err(cleanup::REMOTE_PROVIDER_UNREACHABLE),
            "instructions_cleanup_remote_provider_unreachable",
        ),
        (Err(cleanup::NO_NETWORK), "instructions_cleanup_no_network"),
        (
            Ok("I would rather not."),
            "instructions_cleanup_no_proposal",
        ),
        (Ok("~~~\n\n~~~"), "instructions_cleanup_no_proposal"),
    ] {
        let refused = propose(&root, Some(&project), None, |_| ending.map(str::to_owned));
        assert_eq!(refused.unwrap_err().0, code);
        assert!(
            PROCESS_CODES.contains(&code),
            "{code} reaches the owner as itself"
        );
    }
    let large = format!("~~~\n{}\n~~~", "x".repeat(TEXT_BYTES_MAX + 1));
    assert_eq!(
        propose(&root, Some(&project), None, |_| Ok(large.clone()))
            .unwrap_err()
            .0,
        "instructions_cleanup_too_large"
    );
    for code in [
        cleanup::OFF,
        cleanup::EMPTY,
        cleanup::POOL_NEEDED,
        cleanup::POOL_NOT_ASKED,
    ] {
        assert!(PROCESS_CODES.contains(&code));
    }
    assert_eq!(current_text(&root, Some(&project)).unwrap(), before);
}

#[test]
fn accepting_a_proposal_made_before_a_newer_save_answers_stale_and_keeps_the_newer_text() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, Some(project.clone()), "- Old.\n- Old.\n", 0).unwrap();
    let proposed = propose(&root, Some(&project), None, |_| {
        Ok("~~~\n- Old.\n~~~".to_owned())
    })
    .unwrap();
    let newer = "- Old.\n- Old.\n- Added in another window.\n";
    save(&root, Some(project.clone()), newer, proposed.version).unwrap();
    match save(
        &root,
        Some(project.clone()),
        &proposed.text,
        proposed.version,
    )
    .unwrap()
    {
        SavedText::Stale { current, .. } => assert_eq!(current.text, newer),
        other => panic!("{other:?}"),
    }
    assert_eq!(current_text(&root, Some(&project)).unwrap().text, newer);
    let again = propose(&root, Some(&project), None, |_| {
        Ok("~~~\n- Old.\n- Added in another window.\n~~~".to_owned())
    })
    .unwrap();
    match save(&root, Some(project.clone()), &again.text, again.version).unwrap() {
        SavedText::Saved { .. } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        current_text(&root, Some(&project)).unwrap().text,
        "- Old.\n- Added in another window.\n"
    );
}

fn check_prompt(root: &Path, project: Option<&ProjectId>) -> String {
    let pool = CredentialRef::new("pool-a").unwrap();
    let picked = project.is_none().then_some(&pool);
    let mut seen = String::new();
    propose(root, project, picked, |prompt| {
        seen = prompt.to_owned();
        Ok("~~~\n- Kept.\n~~~\n".to_owned())
    })
    .unwrap();
    assert!(!seen.is_empty());
    seen
}

fn set_master(root: &Path, project: &ProjectId, master: &str) {
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let mut setup = crate::project_setup::load_setup(&connection, project)
        .unwrap()
        .unwrap();
    setup.team.master = master.into();
    let body = serde_json::to_string(&setup).unwrap();
    let changed = connection
        .execute(
            "UPDATE project_setups SET body=?2 WHERE project=?1",
            [project.as_str(), body.as_str()],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

fn with_plugins(root: &Path, skill: &str, server: &str) -> tempfile::TempDir {
    use crate::plugins::{Scope, ServerSpec};
    use crate::storage::plugins::{Command, ResultValue, execute, initialize};
    let mut connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    let picked = tempfile::tempdir().unwrap();
    let folder = picked.path().join(skill);
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: {skill}\ndescription: The {skill} skill.\n---\n"),
    )
    .unwrap();
    execute(
        &mut connection,
        root,
        Command::AddSkill {
            scope: Scope::Global,
            source: folder.display().to_string(),
        },
    )
    .unwrap();
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec!["--stdio".into()],
    };
    let ResultValue::Server(view) = execute(
        &mut connection,
        root,
        Command::AddServer {
            scope: Scope::Global,
            name: server.into(),
            spec: spec.clone(),
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    execute(
        &mut connection,
        root,
        Command::Approve { id: view.id, spec },
    )
    .unwrap();
    picked
}

#[test]
fn the_edit_names_a_text_over_what_a_claude_master_takes_as_over_budget() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    let short = check_prompt(&root, Some(&project));
    assert!(!short.contains("- over_budget"), "a short text fits");
    let mut long = String::new();
    let mut rule = 0;
    while long.len() <= super::super::master_host_launch::CLAUDE_PROCESS_BYTES_MAX {
        rule += 1;
        long.push_str(&format!("- Rule {rule} keeps the build green.\n"));
    }
    assert!(long.len() <= TEXT_BYTES_MAX);
    let version = current_text(&root, Some(&project)).unwrap().version;
    save(&root, Some(project.clone()), &long, version).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(seen.contains("- over_budget, whole file: Agents get "));
    assert!(seen.contains("; a Claude master takes at most "));
    assert!(
        !seen.contains("- duplicate"),
        "numbered rules repeat nothing"
    );
}

#[test]
fn the_edit_names_a_file_reference_the_project_folder_does_not_have() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    fs::create_dir_all(root.join("project/docs")).unwrap();
    fs::write(root.join("project/docs/kept.md"), "kept\n").unwrap();
    let own = "- Read `docs/kept.md` first.\n- Read `docs/missing.md` too.\n\
               - Never read `../outside.md`.\n- See `docs/kept.md:12` for the rule.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(seen.contains(
        "- broken_reference, line 2: `docs/missing.md` is not a file or folder in this project.\n"
    ));
    assert!(!seen.contains("`docs/kept.md` is not"), "{seen}");
    assert!(!seen.contains("`docs/kept.md:12` is not"), "{seen}");
    assert!(!seen.contains("`../outside.md` is not"), "{seen}");
    save(&root, None, "- Read `docs/missing.md` too.\n", 0).unwrap();
    let app = check_prompt(&root, None);
    assert!(
        !app.contains("- broken_reference"),
        "the app text has no project folder to check files in: {app}"
    );
}

#[test]
fn the_edit_names_a_skill_its_agents_do_not_get() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let _picked = with_plugins(&root, "grill-me", "docs");
    let own = "- Use the `grill-me` skill first.\n- Use the `ghost-skill` skill after.\n\
               - Name `plain-word` in prose.\n- Use the `superpowers:grill-me` skill.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(
        seen.contains(
            "- broken_reference, line 2: `ghost-skill` is not a skill these agents get.\n"
        )
    );
    assert!(!seen.contains("`grill-me` is not"), "{seen}");
    assert!(!seen.contains("`superpowers:grill-me` is not"), "{seen}");
    assert!(!seen.contains("`plain-word`"), "{seen}");
    save(&root, None, own, 0).unwrap();
    let app = check_prompt(&root, None);
    assert!(
        app.contains(
            "- broken_reference, line 2: `ghost-skill` is not a skill these agents get.\n"
        )
    );
    assert!(!app.contains("`grill-me` is not"), "{app}");
}

#[test]
fn the_edit_names_a_tool_whose_mcp_server_its_agents_do_not_get() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let _picked = with_plugins(&root, "grill-me", "docs");
    let own = "- Search with `mcp__docs__search` first.\n\
               - Search with `mcp__ghost__search` after.\n\
               - Claim work with `mcp__crazyagent-team__start_work`.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(seen.contains(
        "- broken_reference, line 2: `mcp__ghost__search` names the MCP server ghost, which these \
         agents do not get.\n"
    ));
    assert!(!seen.contains("`mcp__docs__search` names"), "{seen}");
    let team = "`mcp__crazyagent-team__start_work` names";
    assert!(!seen.contains(team), "{seen}");
}

#[test]
fn the_edit_names_a_command_this_computer_does_not_have() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let own = "- Run `sh -c true` before you commit.\n- Run `no-such-tool-dec172 --check` after.\n\
               - Run `cd docs` to start.\n- Run `nosuchtool` alone.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(seen.contains(
        "- broken_reference, line 2: `no-such-tool-dec172` is not a command on this computer.\n"
    ));
    assert!(!seen.contains("`sh` is not"), "{seen}");
    assert!(
        !seen.contains("`cd` is not"),
        "a shell builtin is no program: {seen}"
    );
    assert!(
        !seen.contains("`nosuchtool` is not"),
        "one word alone may name anything: {seen}"
    );
}

#[test]
fn the_edit_names_repeated_lines_and_lines_the_app_text_has_only_when_it_may_send_it() {
    use crate::instruction_files::Mode;
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, None, "- Run make check.\n", 0).unwrap();
    let own = "- Keep commits small.\n- Run make check.\n- Keep commits small.\n";
    save(&root, Some(project.clone()), own, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    let repeated = "- duplicate, lines 1, 3: Line 3 repeats line 1.\n";
    assert!(seen.contains(repeated), "{seen}");
    assert!(
        seen.contains("- duplicate, line 2: The app instructions already have this line.\n"),
        "{seen}"
    );
    let fresh = instruction_settings(&root, &project, None).unwrap();
    let alone = InstructionSettings {
        mode: Mode::ProjectOnly,
        ..fresh.settings
    };
    instruction_settings(&root, &project, Some((alone, Some(fresh.version)))).unwrap();
    let alone = check_prompt(&root, Some(&project));
    assert!(alone.contains("- duplicate, lines 1, 3: Line 3 repeats line 1.\n"));
    assert!(
        !alone.contains("The app instructions already have"),
        "no app line is named in a run that may not see the app text: {alone}"
    );
}

#[test]
fn the_edit_asks_the_model_for_contradictions_and_rules_said_twice_as_finding_lines() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    let text = "- Always run tests.\n- Never run tests.\n";
    save(&root, Some(project.clone()), text, 0).unwrap();
    let seen = check_prompt(&root, Some(&project));
    assert!(seen.contains("rules that contradict each other"), "{seen}");
    let wanted = "FINDING {\"kind\":\"contradiction\",\"lines\":[";
    assert!(seen.contains(wanted), "{seen}");
    assert!(seen.contains("the kind \"duplicate\""), "{seen}");
}

#[test]
fn a_project_edit_runs_only_on_a_master_harness_that_can_run_it() {
    let (_directory, root) = root();
    let project = cleanup_project(&root);
    save(&root, Some(project.clone()), "- Run make check.\n", 0).unwrap();
    let reply = |_: &str| Ok("~~~\n- Run make check.\n~~~\n".to_owned());
    for master in ["codex", "claude", "claude-code"] {
        set_master(&root, &project, master);
        assert!(
            propose(&root, Some(&project), None, reply).is_ok(),
            "{master} runs it"
        );
    }
    set_master(&root, &project, "pi");
    assert_eq!(
        propose(&root, Some(&project), None, reply).unwrap_err().0,
        "instructions_cleanup_master_harness"
    );
    assert!(PROCESS_CODES.contains(&"instructions_cleanup_master_harness"));
}

#[test]
fn only_a_proposal_request_is_served_past_the_owner_exchange() {
    let body = |action: Action| {
        serde_json::to_vec(&Envelope {
            process: action,
            text_bytes: None,
        })
        .unwrap()
    };
    let pool = CredentialRef::new("pool-a").unwrap();
    let project = ProjectId::new("process-1").unwrap();
    let propose = body(Action::Propose {
        project: None,
        pool: Some(pool),
    });
    assert_eq!(
        String::from_utf8(propose.clone()).unwrap(),
        r#"{"process":{"kind":"propose","project":null,"pool":"pool-a"},"text_bytes":null}"#
    );
    assert!(deferred(&propose));
    assert!(deferred(&body(Action::Propose {
        project: Some(project.clone()),
        pool: None
    })));
    for quick in [
        Action::AppSettings,
        Action::SaveAppSettings {
            settings: AppInstructionSettings {
                edit_with_ai: false,
            },
        },
        Action::Read { project: None },
        Action::Given { project },
    ] {
        assert!(!deferred(&body(quick)));
    }
    assert!(!deferred(b"{}"));
    assert!(!deferred(&vec![b' '; REQUEST_BYTES_MAX + 1]));
    assert!(
        !deferred(
            br#"{"process":{"kind":"propose","project":null,"pool":"pool-a"},"text_bytes":4}"#
        ),
        "a proposal carries no text"
    );
    assert_eq!(
        PROPOSE_TIMEOUT,
        cleanup::WALL_TIME + Duration::from_secs(30)
    );
}

/// SA vid 7 (leader 09:47): the import Process confirms takes the project text as it is, so
/// text in a `.crazyagent/instructions.md` the project's git tracks, here the owner's with
/// the Imported heading taken off, stays first and unlabelled in the file after the import,
/// while agents still get it labelled, as Process never saved it (SA vid 7 residual).
#[test]
fn process_import_never_labels_the_project_text() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    fs::write(folder.join(".crazyagent/instructions.md"), "OWNER-TEXT-7\n").unwrap();
    fs::write(folder.join("CLAUDE.md"), "REPO-RULE-7\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-f", ".crazyagent/instructions.md", "CLAUDE.md"][..],
        &[
            "-c",
            "user.name=Repo",
            "-c",
            "user.email=repo@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-q",
            "-m",
            "carried",
        ][..],
    ] {
        let done = crate::spawn_gate::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&folder)
                .args(args),
        )
        .unwrap();
        assert!(done.status.success(), "{done:?}");
    }
    let project = with_project(&root, &folder);
    crate::instruction_files::import(&project_folder(&root, &project).unwrap()).unwrap();
    assert_eq!(
        crate::agent_home::read_project(&folder).unwrap(),
        "OWNER-TEXT-7\n\n### Imported from CLAUDE.md\n\nREPO-RULE-7\n"
    );
    assert!(read(&root, Some(project.clone())).unwrap().outside);
    let shown = given(&root, &project).unwrap();
    let heading = crate::instruction_files::CARRIED_HEADING;
    assert!(
        shown.contains(&format!("{heading}\n\nOWNER-TEXT-7\n")),
        "text Process never saved keeps its label after an import (SA vid 7 residual): {shown}"
    );
}

/// SA vid 7 residual (leader 14:22 A): once the project file holds text Process did not
/// save, as the owner's own git or an accepted change set leaves it, agents get that text
/// under the Imported heading with its headings demoted, until the owner saves it in Process.
#[test]
fn project_text_changed_after_a_process_save_reaches_agents_labelled_until_saved_again() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    save(&root, Some(project.clone()), "Owner rule.\n", 0).unwrap();
    let saved = given(&root, &project).unwrap();
    assert!(
        saved.ends_with("## Project instructions\n\nOwner rule.\n"),
        "{saved}"
    );
    let pulled = "## Pulled\nRule.\n";
    fs::write(folder.join(".crazyagent/instructions.md"), pulled).unwrap();
    let heading = crate::instruction_files::CARRIED_HEADING;
    let label = format!("## Project instructions\n\n{heading}\n\n##### Pulled\nRule.\n");
    let changed = given(&root, &project).unwrap();
    assert!(changed.ends_with(&label), "{changed}");
    save(&root, Some(project.clone()), pulled, 1).unwrap();
    let kept = given(&root, &project).unwrap();
    assert!(
        kept.ends_with("## Project instructions\n\n## Pulled\nRule.\n"),
        "{kept}"
    );
}

/// SA vid 7 residual (leader 14:22 A): Process says when the project file holds text it did
/// not save there last, in a read and in a stale reply, never for the app text, and a save
/// that lands clears it.
#[test]
fn process_says_when_the_project_text_changed_outside_until_a_save_lands() {
    let (_directory, root) = root();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let project = with_project(&root, &folder);
    assert!(!read(&root, Some(project.clone())).unwrap().outside);
    fs::write(folder.join(".crazyagent/instructions.md"), "Pulled rule.\n").unwrap();
    assert!(read(&root, Some(project.clone())).unwrap().outside);
    fs::write(root.join(crate::agent_home::APP_FILE), "App rule.\n").unwrap();
    assert!(!read(&root, None).unwrap().outside, "never the app text");
    let refused = save(&root, Some(project.clone()), "Mine.\n", 5).unwrap();
    let SavedText::Stale { current, .. } = refused else {
        panic!("a save at another version is stale");
    };
    assert!(current.outside, "{current:?}");
    assert_eq!(current.text, "Pulled rule.\n");
    save(&root, Some(project.clone()), "Pulled rule.\n", 0).unwrap();
    assert!(!read(&root, Some(project)).unwrap().outside);
}

/// H-import: the harness instruction sources travel as two owner read requests, the list and
/// the read by harness id (never a path), the refusal of a source agents can write is a
/// public code, and a list as long as the registry can be at the largest names fits a frame.
#[test]
fn the_harness_instruction_sources_are_owner_read_requests_that_fit_one_frame() {
    assert_eq!(read_kind(&Action::Sources), Some("process.sources"));
    assert_eq!(
        read_kind(&Action::ReadSource {
            harness: "standin-pi".into()
        }),
        Some("process.read_source")
    );
    assert!(PROCESS_CODES.contains(&"process_import_agents_can_write"));

    let envelope = Envelope {
        process: Action::ReadSource {
            harness: "standin-pi".into(),
        },
        text_bytes: None,
    };
    let back: Envelope = serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();

    assert!(matches!(back.process, Action::ReadSource { harness } if harness == "standin-pi"));

    let largest = InstructionSource {
        harness: "h".repeat(crate::harnesses::manifest::NAME_BYTES_MAX),
        name: "n".repeat(crate::harnesses::manifest::SENTENCE_BYTES_MAX),
        path: "p".repeat(crate::harnesses::manifest::SENTENCE_BYTES_MAX),
        bytes: u64::MAX,
    };
    let reply = Reply::Sources {
        sources: vec![largest; crate::harnesses::registry::HARNESSES_MAX],
    };

    assert!(encode_frame(&reply).is_ok());
}

/// H-import, Q4: through the owner transport the bundled registry offers no harness home
/// instruction file and refuses a read for Claude Code or Codex, so no request reads a file
/// in the owner's home until a bundled manifest names one with the owner's yes.
#[test]
fn the_bundled_registry_offers_no_harness_instruction_source_through_the_transport() {
    let (_directory, root) = root();

    let listed = run(&root, Action::Sources, None);

    assert!(
        matches!(listed, Ok(Answer::Sources(ref sources)) if sources.is_empty()),
        "{listed:?}"
    );
    for harness in ["claude-code", "codex"] {
        let refused = run(
            &root,
            Action::ReadSource {
                harness: harness.into(),
            },
            None,
        );
        assert_eq!(
            refused.err().map(|refused| refused.0),
            Some("process_request_invalid".to_owned()),
            "{harness}"
        );
    }
}
