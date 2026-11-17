use super::*;

/// PA1213 security F1: a Codex launch never lifts hook review. Its `config.toml` pins the
/// trust hash of each hook the engine wrote, under the key Codex 0.167.1 reads
/// (`<canonical home>/config.toml:<label>:<group>:<handler>`), so a hook an agent adds in
/// its worktree stays held for review. The hashes match vectors proven live on Codex
/// 0.167.1 with a stand-in home, and Codex's matcher and timeout normalizing is kept.
#[test]
fn a_codex_launch_pins_only_its_own_hooks_and_never_bypasses_hook_trust() {
    let pinned = |event: &str, matcher: Option<&str>, timeout: Option<u32>| {
        codex_hook_hash(&codex_hook(event, matcher, "echo hi".to_owned(), timeout).unwrap())
    };
    assert_eq!(
        pinned("PreToolUse", Some("apply_patch|Bash"), Some(15)),
        "sha256:fb1e14c710eaa3bd057e6917169ae3318e54399b7bc942693b8595fdb7ccabb3"
    );
    assert_eq!(
        pinned("SessionStart", None, None),
        "sha256:a245b9b8493f2be82aea06c373ef42709b417c1b701a54e5ab76ab728777f4ad"
    );
    assert_eq!(
        pinned("Stop", Some("ignored"), None),
        "sha256:f2aa169c7a1cf9b7cc9b61ce5ecec0315e0f7d13ac35935b1aa74440f9a839f8"
    );
    assert!(codex_hook("Notification", None, "echo hi".to_owned(), None).is_none());
    let end = codex_hook("SessionEnd", None, "echo hi".to_owned(), Some(30)).unwrap();
    assert_eq!(end.timeout, 3);

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let engine = root.join("bin").join("crazyagent");
    let codex = private_home(&root, "codex");
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_relay(&engine, &root.join("mailbox-codex"))
        .install("codex", &codex, true)
        .unwrap();
    let text = fs::read_to_string(codex.join("config.toml")).unwrap();
    assert!(!text.contains("bypass"), "{text}");
    let config: toml::Table = text.parse().unwrap();
    let hooks = config["hooks"].as_table().unwrap();
    let group = &hooks["PreToolUse"].as_array().unwrap()[0];
    let handler = &group["hooks"].as_array().unwrap()[0];
    let written = CodexHook {
        event: "PreToolUse",
        label: "pre_tool_use",
        matcher: group["matcher"].as_str().map(str::to_owned),
        command: handler["command"].as_str().unwrap().to_owned(),
        timeout: u32::try_from(handler["timeout"].as_integer().unwrap()).unwrap(),
    };
    let key = format!(
        "{}:pre_tool_use:0:0",
        codex.canonicalize().unwrap().join("config.toml").display()
    );
    let state = hooks["state"].as_table().unwrap();
    assert_eq!(state.keys().collect::<Vec<_>>(), [&key]);
    assert_eq!(
        state[&key]["trusted_hash"].as_str(),
        Some(codex_hook_hash(&written).as_str())
    );
    assert_eq!(
        codex_pinned_keys(&codex),
        Ok(vec![key]),
        "the engine reads back the keys it asks Codex about (PA1213R2-F2)"
    );
    assert_eq!(
        codex_pinned_keys(&root.join("no-home")),
        Err(HookKeysUnread::Absent)
    );
    drop(installed);
}

/// SECRET-PARSE-COPIES R2: the engine reads back a Codex launch's pinned hook keys from the
/// launch's own key list, never by parsing `config.toml`, whose MCP env and header tables
/// hold secrets that a parse tree would copy into memory no wipe reaches. An edited or
/// missing `config.toml` changes nothing, and a home with no key list is unreadable even
/// when its `config.toml` pins keys.
#[test]
fn a_codex_launch_s_pinned_keys_come_from_its_own_key_list_never_from_its_config_text() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let engine = root.join("bin").join("crazyagent");
    let codex = private_home(&root, "codex");
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_relay(&engine, &root.join("mailbox-codex"))
        .install("codex", &codex, true)
        .unwrap();
    let key = format!(
        "{}:pre_tool_use:0:0",
        codex.canonicalize().unwrap().join("config.toml").display()
    );
    assert_eq!(codex_pinned_keys(&codex), Ok(vec![key.clone()]));
    let planted = r#"[hooks.state."planted:pre_tool_use:0:0"]
trusted_hash = "sha256:00"

[mcp_servers.tools.env]
TOKEN = "secret-value"
"#;
    let config = codex.join("config.toml");
    fs::write(&config, planted).unwrap();
    assert_eq!(
        codex_pinned_keys(&codex),
        Ok(vec![key.clone()]),
        "an edited config.toml is never parsed for the keys"
    );
    fs::remove_file(&config).unwrap();
    assert_eq!(
        codex_pinned_keys(&codex),
        Ok(vec![key]),
        "the keys need no config.toml at all"
    );
    let bare = private_home(&root, "bare");
    fs::write(bare.join("config.toml"), planted).unwrap();
    assert_eq!(
        codex_pinned_keys(&bare),
        Err(HookKeysUnread::Absent),
        "a home with no key list has none, never read from its config.toml"
    );
    drop(installed);
}

/// #166 N0 and N1: a Codex home tells a missing hook key list, as an agent an earlier
/// CrazyAgent started has, apart from one it cannot read. A link in the list's place is not
/// followed, and a list past `CODEX_HOOK_KEYS_BYTES_MAX` or not a list of strings is
/// unreadable, never a guess at keys.
#[test]
fn a_missing_codex_hook_key_list_is_told_apart_from_a_linked_oversized_or_malformed_one() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = private_home(&root, "codex");
    assert_eq!(codex_pinned_keys(&home), Err(HookKeysUnread::Absent));
    fs::create_dir(home.join(STATE)).unwrap();
    assert_eq!(codex_pinned_keys(&home), Err(HookKeysUnread::Absent));
    let list = home.join(STATE).join(CODEX_HOOK_KEYS);
    let planted = root.join("planted.json");
    fs::write(&planted, r#"["planted:pre_tool_use:0:0"]"#).unwrap();
    std::os::unix::fs::symlink(&planted, &list).unwrap();
    assert_eq!(
        codex_pinned_keys(&home),
        Err(HookKeysUnread::Unreadable),
        "a link in the list's place is never followed"
    );
    fs::remove_file(&list).unwrap();
    fs::write(&list, r#"{"keys": ["k:pre_tool_use:0:0"]}"#).unwrap();
    assert_eq!(
        codex_pinned_keys(&home),
        Err(HookKeysUnread::Unreadable),
        "a list that is not a list of strings"
    );
    let bound = usize::try_from(CODEX_HOOK_KEYS_BYTES_MAX).unwrap();
    fs::write(&list, format!("[\"{}\"]", "k".repeat(bound))).unwrap();
    assert_eq!(
        codex_pinned_keys(&home),
        Err(HookKeysUnread::Unreadable),
        "a well-formed list past the bound"
    );
    fs::write(&list, r#"["k:pre_tool_use:0:0"]"#).unwrap();
    assert_eq!(
        codex_pinned_keys(&home),
        Ok(vec!["k:pre_tool_use:0:0".to_owned()])
    );
}

/// KEYLIST-OPEN-NONBLOCK (#168 F1): a FIFO an agent put in the hook key list's place is
/// unreadable at once. Opening it never waits for a writer, so the team tick that asks for
/// the keys is never held by an agent's home.
#[test]
fn a_fifo_in_the_codex_hook_key_list_s_place_is_unreadable_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = private_home(&root, "codex");
    fs::create_dir(home.join(STATE)).unwrap();
    let made = crate::spawn_gate::collected(
        std::process::Command::new("/usr/bin/mkfifo").arg(home.join(STATE).join(CODEX_HOOK_KEYS)),
    )
    .unwrap();
    assert!(made.status.success(), "{made:?}");
    let (sender, receiver) = std::sync::mpsc::channel();
    let asked = home.clone();
    std::thread::spawn(move || sender.send(codex_pinned_keys(&asked)));
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(Err(HookKeysUnread::Unreadable)),
        "a FIFO with no writer never holds the caller"
    );
}
