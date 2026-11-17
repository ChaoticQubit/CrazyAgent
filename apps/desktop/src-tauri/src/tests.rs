use super::*;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};
use tauri::webview::{InvokeRequest, WebviewBuilder};
use tauri::{LogicalPosition, LogicalSize, Webview};

struct Page(Webview<MockRuntime>);

impl AsRef<Webview<MockRuntime>> for Page {
    fn as_ref(&self) -> &Webview<MockRuntime> {
        &self.0
    }
}

fn ask(page: &Webview<MockRuntime>, command: &str, args: Value) -> Result<(), Value> {
    get_ipc_response(
        &Page(page.clone()),
        InvokeRequest {
            cmd: command.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|_| ())
}

/// DEC-134 addendum 1: the app replaces an engine on its own only when the engine's
/// version is older than the app's. A dev build sorts below its stable; an equal, newer or
/// unreadable version is never replaced, so the app never downgrades an engine.
#[test]
fn only_an_engine_older_than_the_app_is_replaced_on_its_own() {
    let app = "2.4.1-dev.202611161323";
    assert!(engine_older("2.4.1-dev.202611150900", app));
    assert!(engine_older("2.4.0", app));
    assert!(engine_older(app, "2.4.1"));
    assert!(!engine_older(app, app));
    assert!(!engine_older("2.4.1", app));
    assert!(!engine_older("2.4.1-dev.202611170000", app));
    for unreadable in ["", "x", "unknown", "2.4.1\n\u{1b}[2J"] {
        assert!(!engine_older(unreadable, app), "{unreadable:?}");
        assert!(!engine_older(app, unreadable), "{unreadable:?}");
    }
}

/// DEC-134 addendum 1: one app run replaces an older engine at most once, so a swap that
/// leaves the mismatch shows the restart view instead of restarting the engine again.
#[test]
fn an_app_run_replaces_an_older_engine_at_most_once() {
    let taken = AtomicBool::new(false);
    assert!(first_replacement(&taken));
    assert!(!first_replacement(&taken));
    assert!(!first_replacement(&taken));
    assert!(taken.load(Ordering::SeqCst));
}

/// DEC-134 addendum 1: a replacement that succeeded is told by the next connect reply
/// and by no later one. A local signal stands in for the app's, which the bridge rows
/// that connect read in parallel.
#[test]
fn a_finished_replacement_is_told_by_one_connect_reply() {
    let untold = AtomicBool::new(true);
    assert!(take_signal(&untold));
    assert!(!take_signal(&untold));
    untold.store(true, Ordering::SeqCst);
    assert!(take_signal(&untold));
    assert!(!untold.load(Ordering::SeqCst));
}

/// #111 F1: the launch swaps an older engine only while the login service is installed
/// and running; an older engine with no service, or with a service that runs nothing,
/// keeps today's mismatch view, and an engine of this app's version is never swapped.
#[test]
fn only_an_older_engine_the_login_service_runs_is_replaced_at_launch() {
    let app = "2.4.1-dev.202611161323";
    let older = "2.4.1-dev.202611150900";
    let service = |installed: bool, running: bool| crazyagent::service::Status {
        installed,
        running,
        pid: running.then_some(4242),
    };
    assert!(swap_on_launch(older, app, &service(true, true)));
    assert!(
        !swap_on_launch(older, app, &service(false, false)),
        "an older engine started by hand, with no login service, is left alone"
    );
    assert!(
        !swap_on_launch(older, app, &service(true, false)),
        "an installed service that runs nothing did not start the engine that answered"
    );
    assert!(
        !swap_on_launch(older, app, &service(false, true)),
        "a loaded job without its definition cannot be replaced through the service"
    );
    assert!(!swap_on_launch(app, app, &service(true, true)));
    assert!(!swap_on_launch("2.4.2", app, &service(true, true)));
}

/// UI-5-F3: the close walls command refuses a wall of another project and admits the main
/// window and the project's own wall; close_walls closes every wall of the removed project
/// and no other window, and every wall once main is gone. The mock runtime keeps a closed
/// window listed, as it runs no event loop, so the walls a close reached are read from
/// close_walls's own answer.
#[test]
fn a_removal_closes_every_wall_of_its_project_and_only_those_and_another_project_s_wall_is_refused()
{
    let app = mock_builder()
        .manage(ide::IdeViews::default())
        .invoke_handler(admit_pages(tauri::generate_handler![
            desktop_close_project_walls,
        ]))
        .build(mock_context(noop_assets()))
        .unwrap();
    let page = |label: &str| {
        WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
            .build()
            .unwrap()
    };
    let (one, other) = (
        ProjectId::new("project-one").unwrap(),
        ProjectId::new("project-other").unwrap(),
    );
    let own = [
        bridge::terminal::wall_label(&one, 1),
        bridge::terminal::wall_label(&one, 2),
    ];
    let other_wall = bridge::terminal::wall_label(&other, 1);
    let main = page("main");
    let own_page = page(&own[0]);
    let _ = page(&own[1]);
    let other_page = page(&other_wall);
    let close = json!({ "id": one.as_str() });
    let refused = ask(
        other_page.as_ref(),
        "desktop_close_project_walls",
        close.clone(),
    )
    .unwrap_err();
    assert_eq!(refused["code"], "window_denied");
    for admitted in [&own_page, &main] {
        assert_eq!(
            ask(
                admitted.as_ref(),
                "desktop_close_project_walls",
                close.clone()
            ),
            Ok(()),
            "{}",
            admitted.label()
        );
    }
    let sorted = |mut labels: Vec<String>| {
        labels.sort();
        labels
    };
    assert_eq!(
        sorted(close_walls(app.handle(), Some(&one))),
        sorted(own.to_vec()),
        "every wall of the removed project closes, and no other window"
    );
    assert_eq!(
        sorted(close_walls(app.handle(), None)),
        sorted(vec![own[0].clone(), own[1].clone(), other_wall.clone()]),
        "once main is gone every wall closes, and main is not one"
    );
}

#[test]
fn a_wall_title_shows_hidden_characters_as_their_escapes_and_stays_bounded() {
    assert_eq!(
        wall_name("Lark\u{202e}spur\u{200b} \u{e0041}Booking"),
        "Lark\\u202espur\\u200b \\u{e0041}Booking"
    );
    assert_eq!(wall_name("Lark\n\tspur\u{1}"), "Larkspur");
    assert_eq!(wall_name("\u{200b}"), "\\u200b");
    assert_eq!(wall_name(" \u{1} "), "Project");
    assert_eq!(wall_name(&wall_name("Lark\u{202e}spur")), "Lark\\u202espur");
    assert_eq!(
        wall_name("Larkspur \u{2764}\u{fe0f}"),
        "Larkspur \u{2764}\u{fe0f}"
    );
    let long = wall_name(&"\u{200b}x".repeat(WALL_NAME_CHARS_MAX * 4));
    assert_eq!(long.chars().count(), WALL_NAME_CHARS_MAX);
}

#[test]
fn the_app_page_keeps_its_commands_and_the_ide_page_gets_none_once_the_ide_is_in_the_window() {
    let app = mock_builder()
        .manage(NativeState(BridgeQueue::new(NativeBridge::new(None))))
        .manage(ide::IdeViews::default())
        .invoke_handler(admit_pages(tauri::generate_handler![
            desktop_ide_place,
            desktop_ide_hide,
            desktop_ide_show,
        ]))
        .build(mock_context(noop_assets()))
        .unwrap();
    let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    let ide_page = main
        .as_ref()
        .window()
        .add_child(
            WebviewBuilder::new("ide-70-1", WebviewUrl::default()),
            LogicalPosition::new(0, 0),
            LogicalSize::new(400, 300),
        )
        .unwrap();
    let project = json!({ "projectId": "project-one" });
    let placed = json!({
        "projectId": "project-one",
        "bounds": { "x": 0.0, "y": 80.0, "width": 900.0, "height": 600.0, "viewport": 720.0 },
    });
    let calls = [
        ("desktop_ide_place", &placed),
        ("desktop_ide_hide", &project),
        ("desktop_ide_show", &placed),
    ];
    for (command, args) in calls {
        assert_eq!(
            ask(main.as_ref(), command, args.clone()),
            Ok(()),
            "{command} from the app page",
        );
        let refused = ask(&ide_page, command, args.clone()).unwrap_err();
        assert_eq!(
            refused["code"], "window_denied",
            "{command} from the IDE page",
        );
    }
}

#[test]
fn the_main_window_mirrors_its_theme_choice_and_a_wall_or_a_stray_word_changes_nothing() {
    let app = mock_builder()
        .manage(theme::Mirrored::default())
        .invoke_handler(admit_pages(tauri::generate_handler![desktop_set_theme]))
        .build(mock_context(noop_assets()))
        .unwrap();
    let page = |label: &str| {
        WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
            .build()
            .unwrap()
    };
    let main = page("main");
    let wall = page("terminals-0123456789abcdef-1");
    let set = |page: &Webview<MockRuntime>, choice: &str| {
        ask(page, "desktop_set_theme", json!({ "choice": choice }))
    };
    let dark = theme::Mirror {
        choice: theme::ThemeChoice::Dark,
        icon: tauri::Theme::Dark,
    };
    assert_eq!(set(main.as_ref(), "dark"), Ok(()));
    assert_eq!(theme::mirrored(app.handle()), Some(dark));
    let refused = set(wall.as_ref(), "light").unwrap_err();
    assert_eq!(refused["code"], "window_denied");
    assert!(set(main.as_ref(), "Light").is_err());
    assert!(set(main.as_ref(), "blue").is_err());
    assert_eq!(theme::mirrored(app.handle()), Some(dark));
}

/// v2.5 S3b: the engine watch is the main window's; a terminal wall that asks for it is
/// refused before its channel is kept, so no watch starts for it.
#[test]
fn only_the_main_window_starts_the_engine_watch() {
    let app = mock_builder()
        .manage(Watching::default())
        .invoke_handler(admit_pages(tauri::generate_handler![desktop_watch]))
        .build(mock_context(noop_assets()))
        .unwrap();
    let wall =
        WebviewWindowBuilder::new(&app, "terminals-0123456789abcdef-1", WebviewUrl::default())
            .build()
            .unwrap();
    let asked = json!({ "changes": "__CHANNEL__:7" });
    let refused = ask(wall.as_ref(), "desktop_watch", asked).unwrap_err();
    assert_eq!(refused["code"], "window_denied");
    assert!(
        app.state::<Watching>().listen(Channel::new(|_| Ok(()))),
        "the refused wall started no watch"
    );
}

#[test]
fn every_native_command_refuses_any_page_the_app_did_not_make() {
    let reached = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let spy = {
        let reached = Arc::clone(&reached);
        move |invoke: Invoke<MockRuntime>| {
            reached
                .lock()
                .unwrap()
                .push(invoke.message.command().to_owned());
            invoke.resolver.resolve(());
            true
        }
    };
    let app = mock_builder()
        .invoke_handler(admit_pages(spy))
        .build(mock_context(noop_assets()))
        .unwrap();
    let page = |label: &str| {
        WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
            .build()
            .unwrap()
    };
    let main = page("main");
    let wall = page("terminals-0123456789abcdef-1");
    let ide_page = main
        .as_ref()
        .window()
        .add_child(
            WebviewBuilder::new("ide-70-1", WebviewUrl::default()),
            LogicalPosition::new(0, 0),
            LogicalSize::new(400, 300),
        )
        .unwrap();
    let strangers: Vec<Webview<MockRuntime>> = ["stranger", "Main", "terminals-zz-1"]
        .map(|label| page(label).as_ref().clone())
        .into_iter()
        .chain([ide_page])
        .collect();
    assert!(NATIVE_COMMANDS.contains(&"desktop_connect"));
    assert!(NATIVE_COMMANDS.contains(&"desktop_ide_hide"));
    let added_later = ["desktop_a_command_added_later"];
    for command in NATIVE_COMMANDS.iter().chain(&added_later) {
        for stranger in &strangers {
            let refused = ask(stranger, command, json!({})).unwrap_err();
            assert_eq!(
                refused["code"],
                "window_denied",
                "{command} from {}",
                stranger.label(),
            );
        }
    }
    assert_eq!(
        *reached.lock().unwrap(),
        Vec::<String>::new(),
        "no command ran for a page the app did not make",
    );
    for command in NATIVE_COMMANDS {
        assert_eq!(ask(main.as_ref(), command, json!({})), Ok(()), "{command}");
        assert_eq!(ask(wall.as_ref(), command, json!({})), Ok(()), "{command}");
    }
    assert_eq!(reached.lock().unwrap().len(), NATIVE_COMMANDS.len() * 2);
}
