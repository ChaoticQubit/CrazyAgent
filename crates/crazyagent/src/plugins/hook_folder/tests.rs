use super::*;
use std::path::PathBuf;

fn plugin(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    fs::create_dir_all(folder.join("hooks")).unwrap();
    folder
}

#[test]
fn inline_manifest_hooks_and_a_hooks_file_both_read_with_their_events_and_commands() {
    let parent = tempfile::tempdir().unwrap();
    let inline = plugin(parent.path(), "inline-folder");
    fs::create_dir_all(inline.join(".claude-plugin")).unwrap();
    fs::write(
        inline.join(".claude-plugin/plugin.json"),
        r#"{"name":"caveman","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"node \"$HOOK_ROOT/src/hooks/caveman-activate.js\"","timeout":30,"statusMessage":"Loading"}]}],"SubagentStart":[{"hooks":[{"type":"command","command":"node \"$HOOK_ROOT/src/hooks/caveman-activate.js\" --subagent","timeout":30}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"node \"$HOOK_ROOT/src/hooks/caveman-mode-tracker.js\""}]}]}}"#,
    )
    .unwrap();
    let found = read(&inline).unwrap();
    assert_eq!(found.plugin, "caveman");
    let events: Vec<&str> = found.hooks.iter().map(|hook| hook.event.as_str()).collect();
    assert_eq!(
        events,
        ["SessionStart", "SubagentStart", "UserPromptSubmit"]
    );
    assert_eq!(found.hooks[0].timeout, Some(30));
    assert!(found.hooks[1].command.ends_with("--subagent"));

    let file = plugin(parent.path(), "i-have-adhd");
    fs::write(
        file.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command":"node \"${CLAUDE_PLUGIN_ROOT}/hooks/always-on.mjs\"","timeout":30}]}],"Stop":[{"hooks":[{"type":"prompt","prompt":"not a command"}]}]}}"#,
    )
    .unwrap();
    let found = read(&file).unwrap();
    assert_eq!(found.plugin, "i-have-adhd");
    assert_eq!(found.hooks.len(), 1);
    assert_eq!(
        found.hooks[0].matcher.as_deref(),
        Some("startup|resume|clear|compact")
    );
    assert_eq!(found.skipped, 1);
}

#[test]
fn a_linked_hooks_file_is_refused_and_a_copy_never_follows_a_link() {
    let parent = tempfile::tempdir().unwrap();
    let real = plugin(parent.path(), "real");
    let config = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh \"${CLAUDE_PLUGIN_ROOT}/hooks/start.sh\""}]}]}}"#;
    fs::write(real.join("hooks/hooks.json"), config).unwrap();
    fs::write(real.join("hooks/start.sh"), "exit 0\n").unwrap();
    let linked = plugin(parent.path(), "linked");
    std::os::unix::fs::symlink(
        real.join("hooks/hooks.json"),
        linked.join("hooks/hooks.json"),
    )
    .unwrap();
    assert_eq!(read(&linked), Err("hooks_invalid"));

    let secret = parent.path().join("secret.txt");
    fs::write(&secret, "owner secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, real.join("hooks/secret.txt")).unwrap();
    std::os::unix::fs::symlink(parent.path(), real.join("hooks/up")).unwrap();
    let found = read(&real).unwrap();
    let copy = parent.path().join("copy");
    super::copy(&real, &copy, &found.hooks).unwrap();
    assert_eq!(
        fs::read_to_string(copy.join("hooks/start.sh")).unwrap(),
        "exit 0\n"
    );
    assert!(fs::symlink_metadata(copy.join("hooks/secret.txt")).is_err());
    assert!(fs::symlink_metadata(copy.join("hooks/up")).is_err());
}

#[test]
fn a_hook_folder_past_the_file_bound_is_refused_and_leaves_no_copy() {
    let parent = tempfile::tempdir().unwrap();
    let big = plugin(parent.path(), "big");
    fs::write(
        big.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .unwrap();
    let many = big.join("hooks/many");
    fs::create_dir(&many).unwrap();
    for at in 0..FILES_MAX {
        fs::write(many.join(format!("{at}.txt")), "x").unwrap();
    }
    let found = read(&big).unwrap();
    let copy = parent.path().join("copy");
    assert_eq!(
        super::copy(&big, &copy, &found.hooks),
        Err("hooks_too_large")
    );
    assert!(fs::symlink_metadata(&copy).is_err());
    fs::remove_file(many.join("0.txt")).unwrap();
    super::copy(&big, &copy, &found.hooks).unwrap();
    assert!(copy.join("hooks/hooks.json").is_file());
}
