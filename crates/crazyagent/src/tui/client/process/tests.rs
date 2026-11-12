use super::*;
use crate::engine::bootstrap::EngineService;
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::storage::versioned::LastWriter;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn shown(text: &str, version: u64) -> ProcessText {
    ProcessText {
        text: text.into(),
        version,
    }
}

fn private_root(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn set_up_project(root: &Path, folder: &Path) -> ProjectId {
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

fn claude_room() -> u32 {
    let room = crate::engine::claude_host::INSTRUCTIONS_BYTES_MAX as usize
        - crate::launch_context::MASTER_INSTRUCTIONS_BYTES_MAX
        - crate::instruction_files::SECTION_BYTES_MAX
        - 1;
    u32::try_from(room).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_past_what_a_claude_master_takes_is_kept_and_warns_with_the_limit() {
    let root = private_root("ca-tclaude-");
    let work = private_root("ca-tclaude-work-");
    let folder = std::fs::canonicalize(work.path()).unwrap().join("project");
    std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let project = set_up_project(root.path(), &folder);
    let app = "a".repeat(20_000);
    let own = "b".repeat(20_000);

    let Ok(ProcessReply::Saved(SavedText::Saved {
        version: 1,
        size: Some(alone),
    })) = native
        .process(ProcessQuery::Save {
            project: None,
            text: app.clone(),
            version: 0,
            opened: None,
        })
        .await
        .result
    else {
        panic!("the app save lands");
    };
    assert_eq!(
        alone.bytes as usize,
        crate::agent_home::render(&app, "", crate::agent_home::Repeated::LeaveOut).len()
    );
    assert_eq!(alone.claude_bytes_max, claude_room());
    assert_eq!(
        alone.claude_warning(),
        None,
        "20,000 bytes of app text fit a Claude master"
    );

    let Ok(ProcessReply::Saved(SavedText::Saved {
        version: 1,
        size: Some(both),
    })) = native
        .process(ProcessQuery::Save {
            project: Some(project.clone()),
            text: own.clone(),
            version: 0,
            opened: None,
        })
        .await
        .result
    else {
        panic!("a save past what a Claude master takes still lands");
    };
    assert_eq!(
        std::fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        own
    );
    let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    let launch = crate::launch_context::agent_project(&tx, &project).unwrap();
    let given = crate::agent_home::text(&launch).unwrap();
    drop(tx);
    assert_eq!(
        both.bytes as usize,
        given.text.len(),
        "the save reports exactly what the project's agents now get"
    );
    assert!(both.bytes > both.claude_bytes_max);
    let warning = both.claude_warning().expect("the save warns");
    let room = format!(
        "a Claude master takes {:.1} KB",
        f64::from(claude_room()) / 1024.0
    );
    assert!(
        warning.contains(&room)
            && warning.contains(&format!("not {:.1} KB", given.text.len() as f64 / 1024.0)),
        "{warning}"
    );

    let Ok(ProcessReply::Saved(SavedText::Saved {
        version: 2,
        size: Some(again),
    })) = native
        .process(ProcessQuery::Save {
            project: None,
            text: app.clone(),
            version: 1,
            opened: None,
        })
        .await
        .result
    else {
        panic!("the app save lands again");
    };
    assert_eq!(
        again, both,
        "an app save measures the project it now pushes past what a Claude master takes"
    );
    assert!(again.claude_warning().is_some());

    let app_file = launch.data.join(crate::agent_home::APP_FILE);
    std::fs::remove_file(&app_file).unwrap();
    std::fs::create_dir(&app_file).unwrap();
    let landed = native
        .process(ProcessQuery::Save {
            project: Some(project.clone()),
            text: own.clone(),
            version: 1,
            opened: None,
        })
        .await
        .result;
    assert_eq!(
        landed,
        Ok(ProcessReply::Saved(SavedText::Saved {
            version: 2,
            size: Some(crate::engine::owner::ProcessSaved {
                bytes: crate::agent_home::render("", &own, crate::agent_home::Repeated::LeaveOut)
                    .len() as u32,
                claude_bytes_max: claude_room(),
            }),
        })),
        "a save that landed is reported saved though the app text cannot be read"
    );
    std::fs::remove_dir(&app_file).unwrap();
    service.shutdown().await.unwrap();
}

#[test]
fn a_request_without_a_trusted_connection_answers_for_its_own_query() {
    let query = ProcessQuery::Save {
        project: None,
        text: "rule".into(),
        version: 0,
        opened: None,
    };
    let event = ProcessEvent::unavailable(query.clone());
    assert_eq!(event.query, query);
    assert_eq!(event.result, Err("trusted_connection_unavailable".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_saves_both_texts_through_the_engine_and_every_launch_reads_them() {
    let root = private_root("ca-tprocess-");
    let work = private_root("ca-tprocess-work-");
    let folder = std::fs::canonicalize(work.path()).unwrap().join("project");
    std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();

    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let project = set_up_project(root.path(), &folder);

    let read = |project: Option<ProjectId>| ProcessQuery::Read { project };
    let event = native.process(read(None)).await;
    assert_eq!(event.query, read(None));
    assert_eq!(event.result, Ok(ProcessReply::Current(shown("", 0))));
    for (scope, text, given) in [
        (None, "APP-RULE-51\n", ("APP-RULE-51\n", "")),
        (
            Some(project.clone()),
            "PROJECT-RULE-52\n",
            ("APP-RULE-51\n", "PROJECT-RULE-52\n"),
        ),
    ] {
        let save = |version| ProcessQuery::Save {
            project: scope.clone(),
            text: text.into(),
            version,
            opened: None,
        };
        assert_eq!(
            native.process(save(0)).await.result,
            Ok(ProcessReply::Saved(SavedText::Saved {
                version: 1,
                size: Some(crate::engine::owner::ProcessSaved {
                    bytes: crate::agent_home::render(
                        given.0,
                        given.1,
                        crate::agent_home::Repeated::LeaveOut
                    )
                    .len() as u32,
                    claude_bytes_max: claude_room(),
                }),
            }))
        );
        assert_eq!(
            native.process(read(scope.clone())).await.result,
            Ok(ProcessReply::Current(shown(text, 1)))
        );
        assert_eq!(
            native.process(save(0)).await.result,
            Ok(ProcessReply::Saved(SavedText::Stale {
                version: 0,
                current: shown(text, 1),
                writer: LastWriter::Owner,
            })),
            "a save from a screen opened before the last save is refused with that text"
        );
    }
    assert_eq!(
        std::fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "PROJECT-RULE-52\n"
    );

    let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    let launch = crate::launch_context::agent_project(&tx, &project).unwrap();
    let given = crate::agent_home::text(&launch).unwrap();
    let app = given
        .text
        .find("APP-RULE-51")
        .expect("a launch reads the saved app text");
    let own = given
        .text
        .find("PROJECT-RULE-52")
        .expect("a launch reads the saved project text");
    assert!(app < own);
    drop(tx);

    crate::team::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
             VALUES (?1, 'sub', 'codex', 0, 'stopped', 1, 1)",
            [project.as_str()],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    let asked = ProcessQuery::Agent {
        project: project.clone(),
        agent: Some(agent),
    };
    let Ok(ProcessReply::Agent(view)) = native.process(asked.clone()).await.result else {
        panic!("an agent before its first launch is still shown");
    };
    assert!(!view.launched);
    let home = crate::team::runtime::agent_home(&root.path().join("team"), agent);
    std::fs::create_dir_all(&home).unwrap();
    for folder in [home.parent().unwrap(), home.as_path()] {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    crate::agent_home::prepare(&home, crate::agent_home::Harness::Codex, &launch, &given).unwrap();
    crate::agent_home::record_skills(&launch.data, &home, &[]).unwrap();
    let Ok(ProcessReply::Agent(view)) = native.process(asked.clone()).await.result else {
        panic!("the launched agent's view is read through the engine");
    };
    assert_eq!(view.agent, agent);
    assert_eq!(view.harness, "codex");
    assert!(view.launched);
    assert_eq!(
        view.text, given.text,
        "the view shows exactly what the launch wrote"
    );
    assert!(
        view.skills.is_empty(),
        "a launch that installed no skill records none"
    );
    assert_eq!(view.sessions, 0, "its harness wrote no session record yet");
    let rollout = home.join("sessions/2026/10/27/rollout-2026-10-27T18-00-00-0199.jsonl");
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    let said = |text: &str| {
        serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": text}]}})
        .to_string()
    };
    std::fs::write(
        &rollout,
        format!(
            "{}\n{}\n",
            said("# AGENTS.md instructions for /w/agent\n\n<INSTRUCTIONS>\nx\n</INSTRUCTIONS>"),
            said("<skill>\n<name>systematic-debugging</name>\n<path>/s/SKILL.md</path>\n</skill>")
        ),
    )
    .unwrap();
    let Ok(ProcessReply::Agent(view)) = native.process(asked.clone()).await.result else {
        panic!("the harness report is read through the engine");
    };
    assert_eq!(view.sessions, 1);
    assert_eq!(
        view.loaded,
        vec![crate::agent_home::LoadedFile {
            name: "AGENTS.md".into(),
            path: "/w/agent".into(),
        }]
    );
    assert_eq!(view.skills_used, vec!["systematic-debugging".to_owned()]);
    std::fs::remove_file(home.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(
        folder.join(".crazyagent/instructions.md"),
        home.join("AGENTS.md"),
    )
    .unwrap();
    assert_eq!(
        native.process(asked).await.result,
        Err("agent_instructions_unsafe".into()),
        "a linked file in the agent home is refused"
    );

    let event = native.process(ProcessQuery::Default).await;
    assert_eq!(
        event.result,
        Ok(ProcessReply::Text(
            crate::agent_home::DEFAULT_PROCESS.into()
        ))
    );
    let unknown = ProjectId::new("not-set-up").unwrap();
    assert_eq!(
        native.process(read(Some(unknown))).await.result,
        Err("process_project_unknown".into())
    );
    let over = ProcessQuery::Save {
        project: None,
        text: "a".repeat(crate::agent_home::TEXT_BYTES_MAX + 1),
        version: 1,
        opened: None,
    };
    assert_eq!(
        native.process(over).await.result,
        Err("process_text_too_large".into())
    );
    assert_eq!(
        native.process(read(None)).await.result,
        Ok(ProcessReply::Current(shown("APP-RULE-51\n", 1))),
        "a refused save keeps the saved text"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_reads_and_stores_every_agent_setting_through_the_engine() {
    use crate::engine::owner::{InstructionSettings, InstructionView};
    use crate::instruction_files::Mode;
    let root = private_root("ca-tsettings-");
    let work = private_root("ca-tsettings-work-");
    let folder = std::fs::canonicalize(work.path()).unwrap().join("project");
    std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let unknown = ProjectId::new("never-set-up").unwrap();
    assert_eq!(
        native
            .process(ProcessQuery::Settings { project: unknown })
            .await
            .result,
        Err("process_project_unknown".into())
    );
    let project = set_up_project(root.path(), &folder);
    assert_eq!(
        native
            .process(ProcessQuery::Settings {
                project: project.clone()
            })
            .await
            .result,
        Ok(ProcessReply::Settings(InstructionView {
            settings: InstructionSettings::default(),
            version: 0,
            repeats: 0,
        })),
        "a project the owner never changed gets both texts and keeps transcripts"
    );
    for scope in [None, Some(project.clone())] {
        assert!(matches!(
            native
                .process(ProcessQuery::Save {
                    project: scope,
                    text: "Rule.\n".into(),
                    version: 0,
                    opened: None,
                })
                .await
                .result,
            Ok(ProcessReply::Saved(SavedText::Saved { version: 1, .. }))
        ));
    }
    let mut version = 0;
    let every = [Mode::ProjectOnly, Mode::AppOnly, Mode::Both]
        .into_iter()
        .flat_map(|mode| [false, true].map(|transcripts| (mode, transcripts)));
    for (mode, transcripts) in every {
        for edits in [
            crate::instruction_files::EditDelivery::NextMessage,
            crate::instruction_files::EditDelivery::Restart,
            crate::instruction_files::EditDelivery::Badge,
        ] {
            let settings = InstructionSettings {
                mode,
                transcripts,
                edits,
                ..Default::default()
            };
            let saved = InstructionView {
                settings,
                version: version + 1,
                repeats: u32::from(mode == Mode::Both),
            };
            assert_eq!(
                native
                    .process(ProcessQuery::SaveSettings {
                        project: project.clone(),
                        settings,
                        version: Some(version),
                    })
                    .await
                    .result,
                Ok(ProcessReply::Settings(saved))
            );
            assert_eq!(
                native
                    .process(ProcessQuery::SaveSettings {
                        project: project.clone(),
                        settings: InstructionSettings::default(),
                        version: Some(version),
                    })
                    .await
                    .result,
                Err(crate::storage::versioned::STALE_CODE.into()),
                "a save from the version before refuses as stale"
            );
            version += 1;
            assert_eq!(
                native
                    .process(ProcessQuery::Settings {
                        project: project.clone()
                    })
                    .await
                    .result,
                Ok(ProcessReply::Settings(saved))
            );
            let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
            assert_eq!(
                crate::instruction_files::load(&connection, &project),
                Ok(settings)
            );
            let tx = connection.unchecked_transaction().unwrap();
            assert_eq!(
                crate::launch_context::agent_project(&tx, &project)
                    .unwrap()
                    .mode,
                mode,
                "the next launch reads the stored choice"
            );
            drop(tx);
        }
    }
    service.shutdown().await.unwrap();
}
