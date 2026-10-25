use crazyagent::setup::pack::{PackGroup, PackKind, PackManifest, PackSource, default_pack, parse};
use std::path::Path;

const DEFAULT_ON: [&str; 18] = [
    "graphify",
    "superpowers",
    "software-practices",
    "ponytail",
    "tiger-style",
    "grill-me",
    "linear-method",
    "to-releases",
    "to-features",
    "to-linear-projects",
    "security-audit",
    "taste-skill",
    "html-explainer",
    "deep-docs",
    "caveman",
    "i-have-adhd",
    "context-mode",
    "headroom",
];

fn shipped_text() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/setup/pack.toml");
    std::fs::read_to_string(path).unwrap()
}

fn shipped() -> PackManifest {
    default_pack().unwrap()
}

fn broken(old: &str, new: &str) -> String {
    let text = shipped_text();
    assert_eq!(
        text.matches(old).count(),
        1,
        "anchor must occur once: {old}"
    );
    text.replacen(old, new, 1)
}

fn code(text: &str) -> String {
    parse(text).unwrap_err().0
}

fn git(manifest: &PackManifest, id: &str) -> (String, String, Option<String>) {
    let entry = manifest
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .unwrap();
    let PackSource::Git { repo, commit, path } = &entry.source else {
        panic!("{id} must come from a pinned git source");
    };
    (repo.clone(), commit.clone(), path.clone())
}

#[test]
fn the_shipped_pack_holds_every_grill_entry_with_its_default() {
    let manifest = shipped();
    assert_eq!(manifest.version, 1);
    let ids = manifest
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    let mut expected = DEFAULT_ON.to_vec();
    expected.extend(["serena", "coderabbit", "linear"]);
    expected.sort_unstable();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, expected);
    for entry in &manifest.entries {
        let optional = ["serena", "coderabbit", "linear"].contains(&entry.id.as_str());
        assert_eq!(entry.on_by_default, !optional, "{}", entry.id);
        assert_eq!(entry.group == PackGroup::Optional, optional, "{}", entry.id);
        assert!(!entry.license.is_empty(), "{}", entry.id);
        assert!(!entry.install.is_empty(), "{}", entry.id);
    }
    let graphify = &manifest.entries[0];
    assert_eq!(graphify.id, "graphify");
    assert_eq!(graphify.group, PackGroup::Graph);
    assert!(graphify.on_by_default);
    assert_eq!(
        manifest
            .entries
            .iter()
            .filter(|entry| entry.group == PackGroup::Graph)
            .count(),
        1
    );
    let servers = manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == PackKind::McpServer)
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(servers, ["context-mode", "headroom", "serena", "linear"]);
}

#[test]
fn the_shipped_pack_pins_public_sources_to_full_commits() {
    let manifest = shipped();
    for entry in &manifest.entries {
        match &entry.source {
            PackSource::Git { repo, commit, path } => {
                assert!(repo.starts_with("https://github.com/"), "{}", entry.id);
                assert_eq!(commit.len(), 40, "{}", entry.id);
                assert!(
                    commit
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "{}",
                    entry.id
                );
                assert!(
                    path.as_deref()
                        .is_none_or(|path| !path.starts_with('/') && !path.contains("..")),
                    "{}",
                    entry.id
                );
            }
            PackSource::Bundled { path } => {
                assert!(path.starts_with("skills/"), "{}", entry.id);
                assert!(!path.contains(".."), "{}", entry.id);
            }
        }
    }
    assert_eq!(
        git(&manifest, "grill-me"),
        (
            "https://github.com/mattpocock/skills".into(),
            "24fe0ef7737efae15c87225755e9f6f5965e4888".into(),
            Some("skills/productivity/grill-me".into())
        )
    );
    assert_eq!(
        git(&manifest, "graphify").1,
        "53efaf89b68190d367feb73f9ef5dba15899377c"
    );
    let bundled = manifest
        .entries
        .iter()
        .filter(|entry| matches!(entry.source, PackSource::Bundled { .. }))
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        bundled,
        [
            "software-practices",
            "tiger-style",
            "linear-method",
            "to-releases",
            "to-features",
            "to-linear-projects",
            "security-audit",
            "html-explainer",
            "deep-docs",
        ]
    );
}

#[test]
fn the_file_on_disk_is_the_manifest_compiled_into_the_engine() {
    assert_eq!(parse(&shipped_text()).unwrap(), shipped());
}

#[test]
fn broken_pins_licenses_and_ids_are_refused_with_their_code() {
    let cases = [
        (
            broken(
                "commit = \"24fe0ef7737efae15c87225755e9f6f5965e4888\"",
                "commit = \"24fe0ef\"",
            ),
            "pack_entry_commit_invalid",
        ),
        (
            broken(
                "commit = \"24fe0ef7737efae15c87225755e9f6f5965e4888\"",
                "commit = \"24FE0EF7737EFAE15C87225755E9F6F5965E4888\"",
            ),
            "pack_entry_commit_invalid",
        ),
        (
            broken("id = \"ponytail\"", "id = \"superpowers\""),
            "pack_entry_id_duplicate",
        ),
        (
            broken("id = \"ponytail\"", "id = \"Pony Tail\""),
            "pack_entry_id_invalid",
        ),
        (
            broken(
                "repo = \"https://github.com/mattpocock/skills\"",
                "repo = \"http://github.com/mattpocock/skills\"",
            ),
            "pack_entry_repo_invalid",
        ),
        (
            broken(
                "repo = \"https://github.com/mattpocock/skills\"",
                "repo = \"https://token@github.com/mattpocock/skills\"",
            ),
            "pack_entry_repo_invalid",
        ),
        (
            broken(
                "path = \"skills/productivity/grill-me\"",
                "path = \"skills/../../outside\"",
            ),
            "pack_entry_path_invalid",
        ),
        (
            broken(
                "path = \"skills/tiger-style\"",
                "path = \"/Users/someone/tiger-style\"",
            ),
            "pack_entry_path_invalid",
        ),
        (
            broken("version = 1", "version = 2"),
            "pack_manifest_version",
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(code(&text), expected);
    }
}

#[test]
fn a_missing_license_or_wrong_default_is_refused() {
    let shipped = shipped_text();
    let first = shipped.find("license = \"MIT\"").unwrap();
    let blank = format!(
        "{}license = \"\"{}",
        &shipped[..first],
        &shipped[first + "license = \"MIT\"".len()..]
    );
    assert_eq!(code(&blank), "pack_entry_license_missing");
    let dropped = format!(
        "{}{}",
        &shipped[..first],
        &shipped[first + "license = \"MIT\"\n".len()..]
    );
    assert_eq!(code(&dropped), "pack_manifest_unreadable");
    let serena = shipped.find("id = \"serena\"").unwrap();
    let off = serena + shipped[serena..].find("on_by_default = false").unwrap();
    let switched_on = format!(
        "{}on_by_default = true{}",
        &shipped[..off],
        &shipped[off + "on_by_default = false".len()..]
    );
    assert_eq!(code(&switched_on), "pack_entry_default_invalid");
}

#[test]
fn unknown_fields_empty_packs_and_oversized_text_are_refused() {
    assert_eq!(
        code(&broken(
            "version = 1",
            "version = 1\nmirror = \"elsewhere\""
        )),
        "pack_manifest_unreadable"
    );
    assert_eq!(code("version = 1\nentries = []\n"), "pack_manifest_size");
    assert_eq!(code(&" ".repeat(64 * 1024 + 1)), "pack_manifest_size");
    assert_eq!(code("not toml at all ["), "pack_manifest_unreadable");
}
