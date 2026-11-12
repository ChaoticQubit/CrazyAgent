#![cfg(unix)]

#[path = "engine_support/config.rs"]
mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::notifications::{
    Banners, NoticeAction, NoticeFilter, NoticeKind, NoticeQuery, NoticeReport, NoticeTarget,
};
use crazyagent_desktop::bridge::notify::{bannered, notice, one_item};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 3] = [
    "desktop_notify_notice",
    "desktop_notification_target",
    "desktop_system_notifications",
];

#[test]
fn notification_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/notify.ts");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("async fn {command}(")),
            "{command} binding"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} handler"
        );
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&permission).count(),
            1,
            "{command} capability"
        );
        assert_eq!(
            window.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} window"
        );
    }
    assert!(
        !capability.contains("notification:"),
        "the window gets no notification plugin command; the app shows them itself"
    );
}

/// The parameters of one command binding in `lib.rs`, one per line as rustfmt writes them.
/// Input: the command name. Output: each parameter line, trimmed.
fn parameters(command: &str) -> Vec<String> {
    let bindings = include_str!("../src/lib.rs");
    let start = bindings
        .find(&format!("async fn {command}("))
        .unwrap_or_else(|| panic!("{command} binding"));
    let rest = &bindings[start..];
    let signature = &rest[..rest.find(") -> ").expect("a return type")];
    signature
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_window_names_only_the_store_item() {
    assert_eq!(
        parameters("desktop_notify_notice"),
        [
            "webview: Webview,",
            "state: State<'_, NativeState>,",
            "notices: State<'_, Notices>,",
            "id: i64,"
        ]
    );
}

/// Sends one change to the store and expects it applied. Inputs: the bridge, the change and
/// a fresh draft generation. Output: none, or the bridge failure.
async fn applied(
    bridge: &mut NativeBridge,
    action: NoticeAction,
    generation: u64,
) -> Result<(), Box<dyn Error>> {
    let receipt = bridge.notification_action(action, generation).await?;
    assert!(
        matches!(receipt.outcome, ActionOutcome::Applied),
        "{receipt:?}"
    );
    Ok(())
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let report = NoticeReport::AppUpdateReady {
        version: "9.1.0".into(),
    };
    applied(&mut bridge, NoticeAction::Report { report }, 1).await?;
    let newest = NoticeQuery {
        filter: NoticeFilter::All,
        project_id: None,
        kind: None,
        fold: false,
        before: None,
        after: None,
        limit: 1,
    };
    let id = bridge.notifications(newest).await?.newest;
    assert!(id >= 1);

    let item = bannered(bridge.notifications(one_item(id)?).await?, id)
        .expect("a new unread App update ready item the engine marks for a banner");
    assert_eq!(item.kind, NoticeKind::AppUpdateReady);
    assert_eq!(item.target, NoticeTarget::Updates);
    let shown = notice(None, &item);
    assert_eq!(shown.title, "App update ready");
    assert_eq!(
        shown.body, "CrazyAgent 9.1.0 is ready to install.",
        "the text comes from the engine's item"
    );
    assert_eq!(
        bannered(bridge.notifications(one_item(id + 1)?).await?, id + 1),
        None,
        "an id the store does not hold is never notified"
    );

    let mut settings = bridge.notification_settings().await?;
    settings.banners = Banners::Off;
    applied(
        &mut bridge,
        NoticeAction::SaveSettings {
            settings: settings.clone(),
        },
        2,
    )
    .await?;
    assert_eq!(
        bannered(bridge.notifications(one_item(id)?).await?, id),
        None,
        "macOS notifications Off sends none, whatever the window asks"
    );
    settings.banners = Banners::All;
    applied(&mut bridge, NoticeAction::SaveSettings { settings }, 3).await?;
    assert!(bannered(bridge.notifications(one_item(id)?).await?, id).is_some());
    applied(&mut bridge, NoticeAction::Read { ids: vec![id] }, 4).await?;
    assert_eq!(
        bannered(bridge.notifications(one_item(id)?).await?, id),
        None,
        "an item already read is never notified"
    );
    Ok(())
}

#[tokio::test]
async fn a_notification_reads_its_store_item_from_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-n-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
