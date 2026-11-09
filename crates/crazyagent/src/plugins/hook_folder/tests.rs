use super::*;
use std::path::PathBuf;

fn plugin(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    fs::create_dir_all(folder.join("hooks")).unwrap();
    fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
    fs::write(
        folder.join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"{name}"}}"#),
    )
    .unwrap();
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
fn a_hooks_folder_without_plugin_json_is_refused_with_a_plain_reason() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().join("a-folder-name-short-enough");
    fs::create_dir_all(folder.join("hooks")).unwrap();
    fs::write(
        folder.join("hooks").join("hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .unwrap();
    assert_eq!(
        read(&folder).map(|found| found.plugin),
        Err("hooks_plugin_manifest_missing")
    );
    fs::create_dir(folder.join(".claude-plugin")).unwrap();
    fs::write(
        folder.join(".claude-plugin/plugin.json"),
        r#"{"description":"no name"}"#,
    )
    .unwrap();
    assert_eq!(
        read(&folder).map(|found| found.plugin),
        Err("hook_plugin_name_invalid")
    );
    fs::write(
        folder.join(".claude-plugin/plugin.json"),
        r#"{"name":"guard"}"#,
    )
    .unwrap();
    assert_eq!(
        read(&folder).map(|found| found.plugin),
        Ok("guard".to_owned())
    );
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

#[test]
fn a_stored_hook_one_file_past_the_launch_bound_is_refused_and_the_tally_stays_at_the_bound() {
    let parent = tempfile::tempdir().unwrap();
    let stored = parent.path().join("stored");
    let half = LAUNCH_FILES_MAX / 2;
    let counts = [half, LAUNCH_FILES_MAX - half + 1];
    assert!(counts.iter().all(|count| *count < ENTRIES_MAX));
    assert_eq!(counts.iter().sum::<usize>(), LAUNCH_FILES_MAX + 1);
    for (name, count) in ["a", "b"].into_iter().zip(counts) {
        let inner = stored.join(name);
        fs::create_dir_all(&inner).unwrap();
        for at in 0..count {
            fs::write(inner.join(format!("{at}.txt")), "x").unwrap();
        }
    }
    let mut tally = Tally::default();
    assert_eq!(
        copy_stored(&stored, &parent.path().join("past"), &mut tally),
        Err("hooks_too_large")
    );
    assert_eq!(tally.files, LAUNCH_FILES_MAX);
    fs::remove_file(stored.join("b/0.txt")).unwrap();
    let mut tally = Tally::default();
    let launch = parent.path().join("launch");
    copy_stored(&stored, &launch, &mut tally).unwrap();
    assert_eq!(tally.files, LAUNCH_FILES_MAX);
    assert!(launch.join("b/1.txt").is_file());
    let next = parent.path().join("next");
    fs::create_dir(&next).unwrap();
    fs::write(next.join("one.txt"), "x").unwrap();
    assert_eq!(
        copy_stored(&next, &parent.path().join("next-copy"), &mut tally),
        Err("hooks_too_large")
    );
    assert_eq!(tally.files, LAUNCH_FILES_MAX);
}

/// SECAUDIT hook_folder copy_entry path-walk-follows-swapped-parent: a subfolder swapped for
/// a link while it is copied never brings files from behind that link into the store.
#[test]
fn a_subfolder_swapped_for_a_link_during_the_copy_brings_in_nothing_from_outside() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let parent = tempfile::tempdir().unwrap();
    let raced = plugin(parent.path(), "raced");
    let sub = raced.join("hooks/sub");
    let outside = parent.path().join("outside");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(&outside).unwrap();
    for index in 0..16 {
        let name = format!("f{index:02}");
        fs::write(sub.join(&name), vec![b'a'; 16 * 1024]).unwrap();
        fs::write(outside.join(&name), vec![b'z'; 16 * 1024]).unwrap();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let parked = parent.path().join("parked");
        let (sub, outside) = (sub.clone(), outside.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                fs::rename(&sub, &parked).unwrap();
                std::os::unix::fs::symlink(&outside, &sub).unwrap();
                std::thread::sleep(std::time::Duration::from_micros(50));
                fs::remove_file(&sub).unwrap();
                fs::rename(&parked, &sub).unwrap();
            }
        })
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut outside_bytes = false;
    for attempt in 0..2000 {
        if std::time::Instant::now() >= until || outside_bytes {
            break;
        }
        let destination = parent.path().join(format!("copy-{attempt}"));
        if super::copy(&raced, &destination, &[]).is_ok() {
            let copied = destination.join("hooks/sub");
            outside_bytes = fs::read_dir(&copied).is_ok_and(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|entry| fs::read(entry.path()).is_ok_and(|bytes| bytes.contains(&b'z')))
            });
        }
        let _ = fs::remove_dir_all(&destination);
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    assert!(
        !outside_bytes,
        "a copy held bytes from behind the swapped link"
    );
}

/// SECAUDIT hook_folder copy_entry path-walk-follows-swapped-parent: a plugin folder that
/// became a link after it was read is refused and leaves no copy.
#[test]
fn a_plugin_folder_swapped_for_a_link_before_the_copy_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let real = plugin(parent.path(), "real");
    fs::write(real.join("hooks/run.sh"), "x").unwrap();
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let copied = parent.path().join("copy");
    assert_eq!(copy(&link, &copied, &[]), Err("skill_folder_not_found"));
    assert!(fs::symlink_metadata(&copied).is_err());
}

/// The walk's entry bound: a folder at the bound copies, one entry past it is refused.
#[test]
fn a_folder_at_the_entry_bound_copies_and_one_past_it_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let crowded = |name: &str, links: usize| {
        let folder = plugin(parent.path(), name);
        for index in 0..links {
            std::os::unix::fs::symlink("missing", folder.join(format!("hooks/link-{index}")))
                .unwrap();
        }
        folder
    };
    let at = crowded("at", ENTRIES_MAX);
    copy(&at, &parent.path().join("at-copy"), &[]).unwrap();
    let past = crowded("past", ENTRIES_MAX + 1);
    let copied = parent.path().join("past-copy");
    assert_eq!(copy(&past, &copied, &[]), Err("hooks_too_large"));
    assert!(fs::symlink_metadata(&copied).is_err());
}

/// The walk's depth bound: a file at the bound copies, one a folder deeper is refused.
#[test]
fn a_file_at_the_depth_bound_copies_and_one_deeper_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let nested = |name: &str, folders: usize| {
        let root = parent.path().join(name);
        let mut path = root.clone();
        for index in 0..folders {
            path = path.join(format!("d{index}"));
        }
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("file"), "x").unwrap();
        root
    };
    let mut tally = Tally::default();
    let at = nested("at", DEPTH_MAX - 1);
    copy_stored(&at, &parent.path().join("at-copy"), &mut tally).unwrap();
    let past = nested("past", DEPTH_MAX);
    assert_eq!(
        copy_stored(&past, &parent.path().join("past-copy"), &mut tally),
        Err("hooks_too_large")
    );
}
