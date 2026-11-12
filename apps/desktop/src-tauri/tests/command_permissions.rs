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

#[test]
fn every_bound_command_is_listed_in_the_build_and_granted_to_main_or_for_the_wall_only_ones_to_the_wall()
 {
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
