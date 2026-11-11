#![cfg(unix)]

mod e2e_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{
    InstructionSettings, OwnerClientKind, OwnerResult, ProcessText, SavedText,
};
use crazyagent::instruction_files::{EditDelivery, Mode};
use crazyagent::protocol::ProjectId;
use crazyagent::storage::versioned::{self, LastWriter};
use e2e_support::{Engine, Owner, profile, scratch};
use std::time::Duration;

const ATTACH_TIME: Duration = Duration::from_secs(10);
const DESKTOP_TEXT: &str = "Desktop rule: run the tests before every commit.\n";
const TUI_TEXT: &str = "TUI rule: keep each change set small.\n";
const LATE_TEXT: &str = "Desktop rule typed on the screen opened before Keep mine.\n";

fn shown(text: &str, version: u64) -> ProcessText {
    ProcessText {
        text: text.into(),
        version,
    }
}

async fn desktop(engine: &Engine) -> OwnerController {
    OwnerController::attach(engine.path(), OwnerClientKind::Desktop, ATTACH_TIME)
        .await
        .unwrap()
}

fn recorded_forces(engine: &Engine) -> Vec<versioned::Force> {
    let connection = rusqlite::Connection::open_with_flags(
        engine.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    versioned::forces(&tx, &versioned::process_text_item(None)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_from_an_older_screen_is_refused_until_the_owner_reloads_or_keeps_theirs() {
    let mut engine = Engine::start("ca-av2-state-");
    let desk = desktop(&engine).await;
    let tui = Owner::attach(&engine).await;

    let opened = desk.process_text(None).await.unwrap();
    assert_eq!(
        opened,
        shown("", 0),
        "nobody saved the app instructions yet"
    );
    assert_eq!(
        tui.controller.process_text(None).await.unwrap(),
        opened,
        "the desktop and the TUI open the same version"
    );

    let saved = desk
        .save_process_text(None, DESKTOP_TEXT.into(), opened.version, None)
        .await
        .unwrap();
    assert!(
        matches!(saved, SavedText::Saved { version: 1, .. }),
        "the first save lands as version 1: {saved:?}"
    );

    let refused = tui
        .controller
        .save_process_text(None, TUI_TEXT.into(), opened.version, None)
        .await
        .unwrap();
    assert_eq!(
        refused,
        SavedText::Stale {
            version: opened.version,
            current: shown(DESKTOP_TEXT, 1),
            writer: LastWriter::Owner,
        },
        "the TUI save from the screen opened before the desktop save is refused with the newer text and who saved it"
    );
    assert_eq!(
        tui.controller.process_text(None).await.unwrap(),
        shown(DESKTOP_TEXT, 1),
        "Reload shows the desktop text; the refused save changed nothing"
    );
    assert!(
        recorded_forces(&engine).is_empty(),
        "a refused save records no Keep mine"
    );

    let kept = tui
        .controller
        .save_process_text(None, TUI_TEXT.into(), 1, Some(opened.version))
        .await
        .unwrap();
    assert!(
        matches!(kept, SavedText::Saved { version: 2, .. }),
        "Keep mine saves over the version the conflict showed: {kept:?}"
    );
    let late = desk
        .save_process_text(None, LATE_TEXT.into(), 1, None)
        .await
        .unwrap();
    assert_eq!(
        late,
        SavedText::Stale {
            version: 1,
            current: shown(TUI_TEXT, 2),
            writer: LastWriter::Owner,
        },
        "the desktop screen that saved version 1 is now the stale one"
    );
    assert_eq!(
        desk.save_process_text(None, LATE_TEXT.into(), 2, Some(2))
            .await
            .err()
            .map(|error| error.0),
        Some("process_request_invalid".into()),
        "Keep mine must name an older version than the one it saves over"
    );

    let forces = recorded_forces(&engine);
    assert_eq!(forces.len(), 1, "one Keep mine is recorded: {forces:?}");
    assert_eq!(
        (
            forces[0].replaced_version,
            forces[0].replaced_writer.clone(),
            forces[0].version
        ),
        (1, LastWriter::Owner, 2),
        "the record names the version Keep mine replaced and the version it became"
    );

    drop(desk);
    drop(tui);
    engine.restart();
    let desk = desktop(&engine).await;
    assert_eq!(
        desk.process_text(None).await.unwrap(),
        shown(TUI_TEXT, 2),
        "the kept text and its version survive an engine restart"
    );
    assert_eq!(
        desk.save_process_text(None, LATE_TEXT.into(), 1, None)
            .await
            .unwrap(),
        SavedText::Stale {
            version: 1,
            current: shown(TUI_TEXT, 2),
            writer: LastWriter::Owner,
        },
        "a screen opened before the restart is still refused after it"
    );
    assert_eq!(
        recorded_forces(&engine).len(),
        1,
        "the Keep mine record survives the restart"
    );
    drop(desk);
    engine.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_agent_settings_save_is_refused_and_the_re_read_settings_save() {
    let mut engine = Engine::start("ca-av2-settings-");
    let mut tui = Owner::attach(&engine).await;
    assert_eq!(tui.save_profile(profile()).await, OwnerResult::Applied);
    let disk = scratch("ca-av2-settings-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    assert_eq!(tui.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        tui.create_project("versioned-shop", "Versioned shop").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("versioned-shop").unwrap();
    let desk = desktop(&engine).await;

    let opened = desk.instruction_settings(project.clone()).await.unwrap();
    assert_eq!(
        opened.settings,
        InstructionSettings::default(),
        "a new project starts on the default agent settings"
    );
    assert_eq!(
        tui.controller
            .instruction_settings(project.clone())
            .await
            .unwrap(),
        opened
    );
    for scope in [None, Some(project.clone())] {
        assert!(matches!(
            desk.save_process_text(scope, TUI_TEXT.into(), 0, None)
                .await
                .unwrap(),
            SavedText::Saved { version: 1, .. }
        ));
    }

    let quiet = InstructionSettings {
        mode: Mode::ProjectOnly,
        transcripts: false,
        edits: EditDelivery::Badge,
        ..Default::default()
    };
    let saved = desk
        .save_instruction_settings(project.clone(), quiet, Some(opened.version))
        .await
        .unwrap();
    assert_eq!(saved.settings, quiet);
    assert_eq!(saved.version, opened.version + 1);

    let both = InstructionSettings {
        mode: Mode::AppOnly,
        transcripts: true,
        edits: EditDelivery::Restart,
        ..Default::default()
    };
    assert_eq!(
        tui.controller
            .save_instruction_settings(project.clone(), both, Some(opened.version))
            .await
            .err()
            .map(|error| error.0),
        Some(versioned::STALE_CODE.into()),
        "the TUI save from before the desktop save is refused"
    );
    assert_eq!(
        tui.controller
            .instruction_settings(project.clone())
            .await
            .unwrap(),
        saved,
        "the refused save left the desktop's settings in place"
    );

    let resaved = tui
        .controller
        .save_instruction_settings(project.clone(), both, Some(saved.version))
        .await
        .unwrap();
    assert_eq!(resaved.settings, both);
    assert_eq!(resaved.version, saved.version + 1);
    assert_eq!(
        desk.instruction_settings(project).await.unwrap(),
        resaved,
        "the desktop reads what the TUI saved after re-reading"
    );
    drop(desk);
    drop(tui);
    engine.stop();
}
