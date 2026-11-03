use super::*;
use crate::setup::fetch::{entry_folder, skill_folders};
use crate::setup::pack::{PackSource, default_pack};
use std::os::unix::fs::PermissionsExt;

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pack")
}

fn walk(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn private() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn slashes(line: &str) -> bool {
    line.match_indices("//").any(|(at, _)| {
        line[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
    })
}

fn hashed(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "#" || trimmed.starts_with("# ") || line.contains(" # ")
}

fn comments(path: &str, text: &str) -> Vec<String> {
    let script = [".html", ".cjs", ".js", ".css"]
        .iter()
        .any(|ext| path.ends_with(ext));
    let mut fence: Option<String> = None;
    let mut found = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if path.ends_with(".md") && trimmed.starts_with("```") {
            fence = match fence {
                Some(_) => None,
                None => Some(trimmed.trim_start_matches('`').trim().to_owned()),
            };
            continue;
        }
        let lang = fence.as_deref();
        let c_like = lang.map_or(script, |lang| {
            ["html", "css", "js", "javascript", "ts", "typescript"].contains(&lang)
        });
        let hash = lang.is_some_and(|lang| ["", "bash", "sh", "python", "yaml"].contains(&lang));
        let comment = line.contains("<!--")
            || (c_like && (line.contains("/*") || slashes(line)))
            || (hash && hashed(line))
            || (lang == Some("mermaid") && trimmed.starts_with("%%"));
        if comment {
            found.push(format!("{path}:{}: {trimmed}", number + 1));
        }
    }
    found
}

#[test]
fn the_comment_check_finds_each_kind_and_leaves_code_alone() {
    let flagged = |path: &str, text: &str| comments(path, text).len();
    assert_eq!(flagged("a.html", "<p>\n<!-- note -->\n</p>"), 1);
    assert_eq!(flagged("a.html", "a{color:red}/* note */"), 1);
    assert_eq!(
        flagged("a.cjs", "#!/usr/bin/env node\nlet a = 1; // note"),
        1
    );
    assert_eq!(flagged("a.cjs", "const u = \"https://x.invalid/a//b\";"), 0);
    assert_eq!(flagged("a.md", "<!-- note -->\n# Title"), 1);
    assert_eq!(
        flagged("a.md", "```bash\n# note\nls '*/.git/*' # why\n```"),
        2
    );
    assert_eq!(flagged("a.md", "```\nx = 1  # note\n```"), 1);
    assert_eq!(flagged("a.md", "```mermaid\n%% note\nflowchart LR\n```"), 1);
    assert_eq!(
        flagged("a.md", "```html\n<script>f(){/* note */}</script>\n```"),
        1
    );
    assert_eq!(
        flagged(
            "a.md",
            "Globs like `src/**/*.rs` and #tags.\n```text\n# heading\n```"
        ),
        0
    );
}

#[test]
fn no_bundled_pack_file_carries_a_comment() {
    let found: Vec<String> = FILES
        .iter()
        .flat_map(|(path, bytes)| {
            comments(
                path,
                std::str::from_utf8(bytes).expect("pack files are text"),
            )
        })
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn the_compiled_files_are_exactly_the_shipped_skill_files() {
    let root = assets();
    let mut found = Vec::new();
    walk(&root.join("skills"), &mut found);
    let mut on_disk: Vec<String> = found
        .iter()
        .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    on_disk.sort();
    let compiled: Vec<String> = FILES.iter().map(|(path, _)| (*path).to_owned()).collect();
    assert_eq!(compiled, on_disk);
    for (path, bytes) in FILES {
        assert_eq!(std::fs::read(root.join(path)).unwrap(), *bytes, "{path}");
    }
    assert!(root.join("LICENSE").is_file());
}

#[test]
fn extracting_writes_private_files_and_replaces_an_older_copy() {
    let setup = private();
    let bundled = extract(setup.path()).unwrap();
    assert_eq!(bundled, setup.path().join("bundled"));
    for (path, bytes) in FILES {
        let file = bundled.join(path);
        assert_eq!(std::fs::read(&file).unwrap(), *bytes);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{path}");
    }
    let stale = bundled.join("skills/stale.md");
    std::fs::write(&stale, "old").unwrap();
    assert_eq!(extract(setup.path()).unwrap(), bundled);
    assert!(!stale.exists());
    let hidden: Vec<String> = std::fs::read_dir(setup.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
}

#[test]
fn every_bundled_manifest_entry_finds_its_skills_in_the_bundle() {
    let setup = private();
    let bundled = extract(setup.path()).unwrap();
    let manifest = default_pack().unwrap();
    let mut checked = 0;
    for entry in &manifest.entries {
        let PackSource::Bundled { path } = &entry.source else {
            continue;
        };
        assert_eq!(entry.license, "Apache-2.0", "{}", entry.id);
        assert_eq!(path, &format!("skills/{}", entry.id));
        let folder = entry_folder(setup.path(), entry, Some(bundled.as_path())).unwrap();
        assert!(!skill_folders(&folder).unwrap().is_empty(), "{}", entry.id);
        checked += 1;
    }
    assert_eq!(checked, 10);
}

#[test]
fn the_default_pack_bundles_the_grill_skill() {
    let manifest = default_pack().unwrap();
    let grill = manifest
        .entries
        .iter()
        .find(|entry| entry.id == "grill")
        .expect("the default pack lists the grill skill");
    assert!(grill.on_by_default);
    assert_eq!(grill.license, "Apache-2.0");
    assert_eq!(
        grill.source,
        PackSource::Bundled {
            path: "skills/grill".into()
        }
    );
    let (_, bytes) = FILES
        .iter()
        .find(|(path, _)| *path == "skills/grill/SKILL.md")
        .expect("the grill skill is compiled into the engine");
    let text = std::str::from_utf8(bytes).unwrap();
    let (name, description) = crate::plugins::skill_folder::frontmatter(text).unwrap();
    assert_eq!(name, "grill");
    assert!(
        description.contains("one question at a time"),
        "{description}"
    );
    assert!(text.contains(&format!("`{}` tool", crate::team::tool::ASK_TOOL)));
    assert_eq!(crate::questions::OPTIONS_MAX, 6);
    assert!(text.contains("at most 6 short one-line ones"));
    assert!(text.contains(&format!("`{}` tool", crate::team::tool::RECORD_TOOL)));
    assert!(!text.contains("ask_owner"));
    assert!(text.contains("`crazyagent-team`"));
    assert!(text.contains("`recommended`"));
    assert!(!text.contains('\u{2014}'));
    let setup = private();
    let bundled = extract(setup.path()).unwrap();
    let folder = entry_folder(setup.path(), grill, Some(bundled.as_path())).unwrap();
    assert_eq!(skill_folders(&folder).unwrap().len(), 1);
}

#[test]
fn each_bundled_path_has_its_own_fingerprint() {
    let tiger = digest("skills/tiger-style");
    assert_eq!(tiger, digest("skills/tiger-style/"));
    assert_ne!(tiger, digest("skills/linear-method"));
    assert_ne!(digest("skills/to-features"), digest("skills/to-feature"));
    assert_eq!(tiger.len(), 64);
}

#[test]
fn the_shipped_hook_files_unpack_privately_replace_an_older_copy_and_name_their_entry() {
    let setup = private();
    assert_eq!(hook_folder(setup.path(), "graphify"), None);
    let unpacked = extract_hooks(setup.path()).unwrap();
    assert_eq!(unpacked, setup.path().join("bundled-hooks"));
    assert_eq!(HOOK_FILES.len(), 3);
    for (path, bytes) in HOOK_FILES {
        let file = unpacked.join(path);
        assert_eq!(std::fs::read(&file).unwrap(), *bytes, "{path}");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{path}");
    }
    let stale = unpacked.join("hooks/graphify/hooks/stale.sh");
    std::fs::write(&stale, "old").unwrap();
    assert_eq!(extract_hooks(setup.path()).unwrap(), unpacked);
    assert!(!stale.exists());
    assert_eq!(
        hook_folder(setup.path(), "graphify"),
        Some(unpacked.join("hooks/graphify"))
    );
    for id in ["caveman", "", "graphify/hooks", "../graphify"] {
        assert_eq!(hook_folder(setup.path(), id), None, "{id:?}");
    }
    assert!(setup.path().join("bundled").symlink_metadata().is_err());
    let read = crate::plugins::hook_folder::read(&unpacked.join("hooks/graphify")).unwrap();
    assert_eq!(read.plugin, "graphify");
    let hooks: Vec<(&str, Option<&str>)> = read
        .hooks
        .iter()
        .map(|hook| (hook.event.as_str(), hook.matcher.as_deref()))
        .collect();
    assert_eq!(
        hooks,
        [
            ("PreToolUse", Some("Bash")),
            ("PreToolUse", Some("Read|Glob"))
        ]
    );
}

#[test]
fn no_shipped_hook_file_carries_a_comment() {
    let mut found = Vec::new();
    for (path, bytes) in HOOK_FILES {
        let text = std::str::from_utf8(bytes).expect("hook files are text");
        found.extend(comments(path, text));
        if path.ends_with(".sh") {
            for (number, line) in text.lines().enumerate() {
                let shebang = number == 0 && line.starts_with("#!");
                if !shebang && (line.trim_start().starts_with('#') || hashed(line)) {
                    found.push(format!("{path}:{}: {line}", number + 1));
                }
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
    let root = assets();
    let mut on_disk = Vec::new();
    walk(&root.join("hooks"), &mut on_disk);
    let mut on_disk: Vec<String> = on_disk
        .iter()
        .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    on_disk.sort();
    let compiled: Vec<String> = HOOK_FILES
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    assert_eq!(compiled, on_disk);
}

fn reminder(folder: &Path, mode: &str, input: &str) -> (bool, String) {
    use std::io::Write;
    let setup = private();
    let unpacked = extract_hooks(setup.path()).unwrap();
    let script = unpacked.join("hooks/graphify/hooks/graphify-reminder.sh");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg(&script)
        .arg(mode)
        .current_dir(folder)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn the_graphify_reminder_nudges_only_searches_and_source_reads_where_a_graph_exists() {
    let with_graph = private();
    std::fs::create_dir(with_graph.path().join("graphify-out")).unwrap();
    std::fs::write(with_graph.path().join("graphify-out/graph.json"), "{}").unwrap();
    let without = private();
    let search = r#"{"tool_input":{"command":"rg foo src"}}"#;
    let listing = r#"{"tool_input":{"command":"ls -la"}}"#;
    let source = r#"{"tool_input":{"file_path":"/p/src/main.rs"}}"#;
    let graph_page = r#"{"tool_input":{"file_path":"/p/graphify-out/wiki/x.md"}}"#;
    let image = r#"{"tool_input":{"file_path":"/p/logo.png"}}"#;
    let (ok, nudge) = reminder(with_graph.path(), "search", search);
    assert!(ok);
    let parsed: serde_json::Value = serde_json::from_str(nudge.trim()).unwrap();
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(
        parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("graphify query")
    );
    let (ok, nudge) = reminder(with_graph.path(), "read", source);
    assert!(ok);
    assert!(nudge.contains("Give every sub-agent and teammate the same rule"));
    for (folder, mode, input) in [
        (with_graph.path(), "search", listing),
        (with_graph.path(), "read", graph_page),
        (with_graph.path(), "read", image),
        (with_graph.path(), "other", search),
        (without.path(), "search", search),
        (without.path(), "read", source),
    ] {
        assert_eq!(
            reminder(folder, mode, input),
            (true, String::new()),
            "{mode} {input}"
        );
    }
}

#[test]
fn a_hookless_folder_with_a_bad_name_reads_as_having_no_hooks_before_its_name_is_checked() {
    let parent = private();
    let bad = parent.path().join(".bad");
    std::fs::create_dir(&bad).unwrap();
    assert_eq!(
        crate::plugins::hook_folder::read(&bad),
        Err("hooks_none_found")
    );
    std::fs::create_dir(bad.join("hooks")).unwrap();
    std::fs::write(
        bad.join("hooks/hooks.json"),
        HOOK_FILES
            .iter()
            .find(|(path, _)| path.ends_with("hooks.json"))
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        crate::plugins::hook_folder::read(&bad),
        Err("hooks_plugin_manifest_missing")
    );
    std::fs::create_dir(bad.join(".claude-plugin")).unwrap();
    std::fs::write(bad.join(".claude-plugin/plugin.json"), r#"{"name":".bad"}"#).unwrap();
    assert_eq!(
        crate::plugins::hook_folder::read(&bad),
        Err("hook_plugin_name_invalid")
    );
}

#[test]
fn an_entrys_hooks_come_from_the_shipped_folder_else_a_real_cached_checkout_never_a_link() {
    let setup = private();
    let manifest = default_pack().unwrap();
    let caveman = manifest
        .entries
        .iter()
        .find(|entry| entry.id == "caveman")
        .unwrap();
    let PackSource::Git { repo, commit, .. } = &caveman.source else {
        panic!("caveman is a git entry");
    };
    let checkout = crate::setup::fetch::checkout_folder(setup.path(), repo, commit);
    assert_eq!(
        crate::setup::fetch::plugin_folder(setup.path(), caveman),
        None
    );
    let elsewhere = private();
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &checkout).unwrap();
    assert_eq!(
        crate::setup::fetch::plugin_folder(setup.path(), caveman),
        None
    );
    std::fs::remove_file(&checkout).unwrap();
    std::fs::create_dir(&checkout).unwrap();
    assert_eq!(
        crate::setup::fetch::plugin_folder(setup.path(), caveman),
        Some(checkout)
    );

    let graphify = manifest
        .entries
        .iter()
        .find(|entry| entry.id == "graphify")
        .unwrap();
    extract_hooks(setup.path()).unwrap();
    assert_eq!(
        crate::setup::fetch::plugin_folder(setup.path(), graphify),
        Some(setup.path().join("bundled-hooks/hooks/graphify"))
    );
}
