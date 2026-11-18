use std::collections::BTreeSet;

fn bound_commands(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[tauri::command]" {
            continue;
        }
        let declaration = lines
            .by_ref()
            .map(str::trim)
            .find(|line| !line.starts_with("#["))
            .unwrap();
        let name = declaration
            .split("fn ")
            .nth(1)
            .and_then(|rest| rest.split(['(', '<']).next())
            .unwrap_or_else(|| panic!("no function after a command attribute: {declaration}"));
        assert!(found.insert(name.to_owned()), "{name} bound twice");
    }
    found
}

fn quoted(source: &str, prefix: &str) -> BTreeSet<String> {
    source
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|text| text.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

/// The commands only a terminal wall window may call. The runtime refuses the main window, so
/// the main window's capability does not grant them.
const WALL_ONLY: [&str; 2] = [
    "desktop_extend_terminal_windows",
    "desktop_focus_terminal_window",
];

/// The commands only the webview terminal calls. Windows and Linux keep that terminal in 2.4.3;
/// macOS draws every terminal natively (DEC-203), so a macOS build neither builds nor grants them.
const WEBVIEW_TERMINAL: [&str; 4] = [
    "desktop_terminal_attach",
    "desktop_terminal_input",
    "desktop_terminal_resize",
    "desktop_terminal_detach",
];

#[test]
fn the_webview_terminal_commands_are_built_and_granted_only_off_macos() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/capabilities/webview-terminal.json"
    );
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let capability: serde_json::Value = serde_json::from_str(&text).unwrap();
    let granted: Vec<String> = WEBVIEW_TERMINAL
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .collect();
    assert_eq!(
        capability["platforms"],
        serde_json::json!(["linux", "windows"])
    );
    assert_eq!(capability["local"], true);
    assert_eq!(
        capability["webviews"],
        serde_json::json!(["main", "terminals-*"])
    );
    assert_eq!(capability["permissions"], serde_json::json!(granted));
    for every_os in [
        include_str!("../capabilities/main-window.json"),
        include_str!("../capabilities/terminal-wall.json"),
    ] {
        for permission in &granted {
            assert!(
                !every_os.contains(&format!("\"{permission}\"")),
                "{permission}"
            );
        }
    }
    let build = include_str!("../build.rs");
    let gated = build
        .split("const WEBVIEW_TERMINAL_COMMANDS")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .expect("build.rs names the webview terminal commands in one list");
    for command in WEBVIEW_TERMINAL {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command}"
        );
        assert!(gated.contains(&format!("\"{command}\"")), "{command}");
    }
    assert!(build.contains("if os != \"macos\""));
}

#[test]
fn every_bound_command_is_built_and_granted_to_the_window_that_calls_it() {
    let bound = bound_commands(include_str!("../src/lib.rs"));
    assert!(bound.len() > 100, "{}", bound.len());
    for hooks in [
        "desktop_plugin_hooks",
        "desktop_add_hooks",
        "desktop_set_hook_enabled",
        "desktop_import_memory",
    ] {
        assert!(bound.contains(hooks), "{hooks}");
    }
    let built = quoted(include_str!("../build.rs"), "desktop_");
    let granted: BTreeSet<String> = quoted(
        include_str!("../capabilities/main-window.json"),
        "allow-desktop-",
    )
    .into_iter()
    .chain(quoted(
        include_str!("../capabilities/webview-terminal.json"),
        "allow-desktop-",
    ))
    .map(|permission| permission["allow-".len()..].replace('-', "_"))
    .collect();
    let missing = |from: &BTreeSet<String>, to: &BTreeSet<String>| -> Vec<String> {
        from.difference(to).cloned().collect()
    };
    assert_eq!(
        missing(&bound, &built),
        Vec::<String>::new(),
        "bound, not built"
    );
    assert_eq!(
        missing(&built, &bound),
        Vec::<String>::new(),
        "built, not bound"
    );
    let wall_only: BTreeSet<String> = WALL_ONLY.iter().map(|name| (*name).to_owned()).collect();
    let for_main: BTreeSet<String> = bound.difference(&wall_only).cloned().collect();
    assert_eq!(
        missing(&wall_only, &bound),
        Vec::<String>::new(),
        "wall only, not bound"
    );
    assert_eq!(
        missing(&for_main, &granted),
        Vec::<String>::new(),
        "bound, not granted"
    );
    assert_eq!(
        missing(&granted, &for_main),
        Vec::<String>::new(),
        "granted, not for the main window"
    );
    let wall_granted: BTreeSet<String> = quoted(
        include_str!("../capabilities/terminal-wall.json"),
        "allow-desktop-",
    )
    .into_iter()
    .map(|permission| permission["allow-".len()..].replace('-', "_"))
    .collect();
    assert_eq!(
        missing(&wall_only, &wall_granted),
        Vec::<String>::new(),
        "wall only, not granted to the wall"
    );
}
