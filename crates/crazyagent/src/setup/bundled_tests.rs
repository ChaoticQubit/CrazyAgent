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
    assert_eq!(checked, 9);
}

#[test]
fn each_bundled_path_has_its_own_fingerprint() {
    let tiger = digest("skills/tiger-style");
    assert_eq!(tiger, digest("skills/tiger-style/"));
    assert_ne!(tiger, digest("skills/linear-method"));
    assert_ne!(digest("skills/to-features"), digest("skills/to-feature"));
    assert_eq!(tiger.len(), 64);
}
