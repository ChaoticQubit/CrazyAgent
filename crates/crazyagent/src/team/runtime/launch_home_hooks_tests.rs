use super::*;

/// RV-DS241-137 code r2 F1: a native lead's teammates' home is reset as an agent home is at
/// each launch, so a crate source a teammate planted in its cargo home where the owner has
/// none, a package npx installed and a tool config no list names are gone, while the links
/// to the lead's team and task folders and Claude Code's answered config stay.
#[cfg(unix)]
#[test]
fn a_native_leads_teammates_home_is_reset_at_each_launch() {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let owner = root.join("owner");
    std::fs::create_dir_all(&owner).unwrap();
    let homes = ToolHomes::at(&owner);
    let home = root.join("agent/home");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mates = prepare_mates_home(&home, &homes, &project).unwrap();
    let planted = [
        mates.join(".cargo/registry/src/index/x-1.0.0/build.rs"),
        mates.join(".npm/_npx/0/node_modules/x/index.js"),
        mates.join(".bunfig.toml"),
    ];
    for path in &planted {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "planted\n").unwrap();
    }

    let again = prepare_mates_home(&home, &homes, &project).unwrap();

    assert_eq!(again, mates);
    let left: Vec<&PathBuf> = planted
        .iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect();
    assert!(left.is_empty(), "the teammates' home kept {left:?}");
    for folder in crate::sandbox::CLAUDE_TEAM_SHARED {
        assert_eq!(
            std::fs::read_link(mates.join(folder)).unwrap(),
            home.join(folder)
        );
    }
    assert!(mates.join(".claude.json").is_file());
}

/// SECRET-PARSE-COPIES R2, leader 16:05: a Codex agent whose launch left no readable hook key
/// list, as one an earlier CrazyAgent started, fails closed. The engine never asks Codex about
/// keys it could not read nor counts them as trusted; it keeps asking until `HOOK_TRUST_WAIT`
/// and then keeps a notice that says why in plain words. The other answers keep their texts.
#[test]
fn a_codex_agent_with_no_readable_hook_key_list_is_never_taken_as_trusted_and_is_told_why() {
    use crate::coord::settings::{
        CODEX_HOOKS_LIST_UNREADABLE, CODEX_HOOKS_UNCHECKED, CODEX_HOOKS_UNLISTED,
        CODEX_HOOKS_UNTRUSTED,
    };
    use crate::launch_context::HookKeysUnread::{Absent, Unreadable};
    let unasked = |_: &[String]| -> Result<bool, &'static str> {
        panic!("Codex was asked about hook keys the engine could not read")
    };
    for unread in [Absent, Unreadable] {
        assert_eq!(hook_trust(Err(unread), unasked, false), HookTrust::Later);
    }
    assert_eq!(
        hook_trust(Err(Absent), unasked, true),
        HookTrust::Notice(CODEX_HOOKS_UNLISTED),
        "a missing key list says why once the wait is over"
    );
    assert_eq!(
        hook_trust(Err(Unreadable), unasked, true),
        HookTrust::Notice(CODEX_HOOKS_LIST_UNREADABLE),
        "a key list that is there but unreadable is not blamed on an earlier CrazyAgent (#166 N0)"
    );
    assert!(
        !CODEX_HOOKS_LIST_UNREADABLE.contains("before this update")
            && CODEX_HOOKS_LIST_UNREADABLE.contains("replaced or damaged")
            && CODEX_HOOKS_LIST_UNREADABLE.contains("until the agent restarts")
            && !CODEX_HOOKS_LIST_UNREADABLE.contains('_'),
        "{CODEX_HOOKS_LIST_UNREADABLE}"
    );
    assert!(
        CODEX_HOOKS_UNLISTED.contains("started before this update")
            && CODEX_HOOKS_UNLISTED.contains("until the agent restarts")
            && !CODEX_HOOKS_UNLISTED.contains('_'),
        "the notice names the cause and the way out in plain words: {CODEX_HOOKS_UNLISTED}"
    );
    assert_eq!(
        hook_trust(Ok(Vec::new()), unasked, true),
        HookTrust::Settled
    );
    let keys = vec!["/h/config.toml:pre_tool_use:0:0".to_owned()];
    let expected = keys.clone();
    let asked = |answer: Result<bool, &'static str>| {
        let expected = expected.clone();
        move |given: &[String]| {
            assert_eq!(given, expected.as_slice());
            answer
        }
    };
    assert_eq!(
        hook_trust(Ok(keys.clone()), asked(Ok(true)), false),
        HookTrust::Settled
    );
    assert_eq!(
        hook_trust(Ok(keys.clone()), asked(Ok(false)), false),
        HookTrust::Notice(CODEX_HOOKS_UNTRUSTED)
    );
    assert_eq!(
        hook_trust(
            Ok(keys.clone()),
            asked(Err("codex_bridge_peer_unknown")),
            false
        ),
        HookTrust::Later
    );
    assert_eq!(
        hook_trust(Ok(keys), asked(Err("codex_bridge_peer_unknown")), true),
        HookTrust::Notice(CODEX_HOOKS_UNCHECKED)
    );
}
