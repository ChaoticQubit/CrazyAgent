use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerResult;
use crate::memory::library::{LibraryAction, MemoryScope, NOTE_BYTES_MAX};
use crate::tui::app::App;
use std::os::unix::fs::PermissionsExt;

async fn round_trip(app: &mut App, native: &mut NativeOwner) -> LibraryView {
    let query = app.take_setup_query().unwrap();
    assert!(matches!(query, SetupQuery::Library(_)));
    let event = native.project_setup(query).await;
    app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
    match &app.setup_view {
        Some((SetupQuery::Library(_), SetupView::Library(view))) => view.clone(),
        other => panic!("memory view required, got {other:?}"),
    }
}

#[tokio::test]
async fn tui_adds_lists_and_deletes_global_memory_through_the_real_engine() {
    let mut limited = App::new();
    assert_eq!(
        limited
            .request_library_change(LibraryAction::Add {
                scope: MemoryScope::Global,
                text: "Remember this".into(),
            })
            .unwrap_err(),
        "Memory changes disabled in limited mode"
    );

    let root = tempfile::Builder::new()
        .prefix("ca-tui-mem-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(root.path().join("e.sock"), Duration::from_secs(2));
    let (_, access) = native.attach().await;
    let mut app = App::new();
    app.apply_owner(access).unwrap();
    assert!(app.owner_ready);

    assert_eq!(
        app.request_library_change(LibraryAction::Add {
            scope: MemoryScope::Global,
            text: "x".repeat(NOTE_BYTES_MAX + 1),
        })
        .unwrap_err(),
        "Keep the memory under 2000 characters."
    );
    assert_eq!(
        app.request_library_change(LibraryAction::SaveSelection {
            project_id: crate::protocol::ProjectId::new("tui-mem").unwrap(),
            path: "context/brief.md\n".into(),
            text: "Selected words".into(),
        })
        .unwrap_err(),
        "context_path_invalid"
    );
    assert!(app.take_setup_query().is_none());
    assert!(
        app.request_library(LibraryQuery::Change {
            action: LibraryAction::Delete {
                memory_id: RequestId::new("note-x").unwrap(),
            },
            generation: 1,
        })
        .is_err(),
        "changes go through request_library_change"
    );

    app.request_library_change(LibraryAction::Add {
        scope: MemoryScope::Global,
        text: "Use metric units in every recipe.".into(),
    })
    .unwrap();
    assert_eq!(
        round_trip(&mut app, &mut native).await,
        LibraryView::Changed(OwnerResult::Applied)
    );
    app.request_library(LibraryQuery::Page {
        scope: MemoryScope::Global,
        search: Some("METRIC".into()),
        before: None,
    })
    .unwrap();
    let LibraryView::Page(page) = round_trip(&mut app, &mut native).await else {
        panic!("page required");
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].text, "Use metric units in every recipe.");
    let id = page.items[0].id.clone();

    app.request_library(LibraryQuery::View {
        memory_id: id.clone(),
    })
    .unwrap();
    assert_eq!(
        round_trip(&mut app, &mut native).await,
        LibraryView::View(Some(page.items[0].clone()))
    );
    app.request_library_change(LibraryAction::Delete {
        memory_id: id.clone(),
    })
    .unwrap();
    assert_eq!(
        round_trip(&mut app, &mut native).await,
        LibraryView::Changed(OwnerResult::Applied)
    );
    app.request_library(LibraryQuery::View { memory_id: id })
        .unwrap();
    assert_eq!(
        round_trip(&mut app, &mut native).await,
        LibraryView::View(None)
    );
    app.request_library(LibraryQuery::Page {
        scope: MemoryScope::Global,
        search: None,
        before: None,
    })
    .unwrap();
    let LibraryView::Page(page) = round_trip(&mut app, &mut native).await else {
        panic!("page required");
    };
    assert!(page.items.is_empty());
    assert_eq!(page.next, None);

    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
