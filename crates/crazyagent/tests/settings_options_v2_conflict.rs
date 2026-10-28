#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::{ProcessText, SavedText};
use crazyagent::storage::versioned::{self, Force, LastWriter};
use e2e_support::{Engine, Owner};
use rusqlite::{Connection, OpenFlags};

const THEIRS: &str = "Run the buoy simulator before every commit.";
const MINE: &str = "Write a failing test first, then the smallest fix.";
const EDITED: &str = "Run the buoy simulator before every commit.\nKeep commits small.";

/// What reached the instructions file the engine gives every agent, read from the running
/// engine's data folder. Input: the engine. Output: the text.
fn given(engine: &Engine) -> String {
    crazyagent::agent_home::read_app(engine.path()).unwrap()
}

/// The owner "Keep mine" records the engine kept for the app-wide instructions, read from
/// the running engine's database without writing to it. Input: the engine. Output: the
/// records, oldest first.
fn kept(engine: &Engine) -> Vec<Force> {
    let db = Connection::open_with_flags(
        engine.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tx = db.unchecked_transaction().unwrap();
    versioned::forces(&tx, &versioned::process_text_item(None)).unwrap()
}

/// Opens the instructions in one window, lets another window save over them, then saves
/// the owner's own text from the version first opened, which the engine refuses as stale
/// with the other window's text. Input: the attached owner. Output: the stale reply's
/// current text and version, as the conflict screen shows them.
async fn conflict(owner: &Owner) -> ProcessText {
    let opened = owner.controller.process_text(None).await.unwrap();
    assert_eq!(
        opened,
        ProcessText {
            text: String::new(),
            version: 0
        }
    );
    let other = owner
        .controller
        .save_process_text(None, THEIRS.into(), opened.version, None)
        .await
        .unwrap();
    assert!(
        matches!(other, SavedText::Saved { version: 1, .. }),
        "{other:?}"
    );
    let stale = owner
        .controller
        .save_process_text(None, MINE.into(), opened.version, None)
        .await
        .unwrap();
    match stale {
        SavedText::Stale {
            current, writer, ..
        } => {
            assert_eq!(writer, LastWriter::Owner);
            assert_eq!(
                current,
                ProcessText {
                    text: THEIRS.into(),
                    version: 1
                }
            );
            current
        }
        other => panic!("a save from an older version must be refused stale: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_keep_mine_writes_the_owner_text_agents_receive_and_records_it() {
    let mut engine = Engine::start("ca-v2opt-keep-");
    let owner = Owner::attach(&engine).await;
    let current = conflict(&owner).await;
    assert_eq!(given(&engine), THEIRS, "the refused save changed nothing");
    assert!(kept(&engine).is_empty());

    let saved = owner
        .controller
        .save_process_text(None, MINE.into(), current.version, Some(0))
        .await
        .unwrap();
    assert!(
        matches!(saved, SavedText::Saved { version: 2, .. }),
        "{saved:?}"
    );
    assert_eq!(given(&engine), MINE, "agents now receive the owner's text");
    let records = kept(&engine);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        (
            records[0].replaced_version,
            &records[0].replaced_writer,
            records[0].version
        ),
        (1, &LastWriter::Owner, 2),
        "Keep mine is recorded with the version it replaced"
    );

    engine.restart();
    let owner = Owner::attach(&engine).await;
    assert_eq!(
        owner.controller.process_text(None).await.unwrap(),
        ProcessText {
            text: MINE.into(),
            version: 2
        }
    );
    assert_eq!(given(&engine), MINE);
    assert_eq!(kept(&engine).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_reload_keeps_the_newer_text_and_records_no_keep_mine() {
    let engine = Engine::start("ca-v2opt-reload-");
    let owner = Owner::attach(&engine).await;
    conflict(&owner).await;

    let reloaded = owner.controller.process_text(None).await.unwrap();
    assert_eq!(
        reloaded,
        ProcessText {
            text: THEIRS.into(),
            version: 1
        },
        "Reload shows the text that landed"
    );
    assert_eq!(given(&engine), THEIRS, "agents keep the newer text");
    assert!(kept(&engine).is_empty(), "Reload forces nothing");

    let saved = owner
        .controller
        .save_process_text(None, EDITED.into(), reloaded.version, None)
        .await
        .unwrap();
    assert!(
        matches!(saved, SavedText::Saved { version: 2, .. }),
        "{saved:?}"
    );
    assert_eq!(given(&engine), EDITED, "an edit on the reloaded text lands");
    assert!(
        kept(&engine).is_empty(),
        "a save from the reloaded version is no Keep mine"
    );
}
