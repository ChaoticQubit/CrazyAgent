use super::*;
use std::path::PathBuf;

const SETUP: &str = "/engine/setup";
const HOME: &str = "/home/mira";
const COMMIT: &str = "2ce8aa15d05bd57213e6f746f953aafb7ded7a8b";
const BUMPED: &str = "0000000000000000000000000000000000000001";
const SUPERPOWERS: &str = "https://github.com/obra/superpowers";

fn entry(id: &str, source: PackSource) -> PackManifestEntry {
    let mut entry = default_pack().unwrap().entries.remove(0);
    entry.id = id.into();
    entry.source = source;
    entry
}

fn git(id: &str, repo: &str, path: Option<&str>) -> PackManifestEntry {
    entry(
        id,
        PackSource::Git {
            repo: repo.into(),
            commit: COMMIT.into(),
            path: path.map(str::to_owned),
        },
    )
}

fn checkout(repo: &str, commit: &str) -> PathBuf {
    Path::new(SETUP)
        .join("pack")
        .join(format!("{}-{commit}", repo_key(repo)))
}

fn of(entries: &[PackManifestEntry], source: &Path) -> String {
    label(
        Path::new(SETUP),
        Some(Path::new(HOME)),
        entries,
        &source.display().to_string(),
    )
}

#[test]
fn a_skill_in_a_pinned_checkout_names_its_entry_even_after_a_pin_bump() {
    let entries = [git("superpowers", SUPERPOWERS, Some("skills"))];
    for commit in [COMMIT, BUMPED] {
        let source = checkout(SUPERPOWERS, commit).join("skills/brainstorming");
        assert_eq!(of(&entries, &source), "Default pack: superpowers");
    }
    let outside = checkout(SUPERPOWERS, COMMIT).join("docs/brainstorming");
    assert_eq!(of(&entries, &outside), ADDED_BY_YOU);
    let other = checkout("https://github.com/obra/other", COMMIT).join("skills/brainstorming");
    assert_eq!(of(&entries, &other), ADDED_BY_YOU);
    let no_commit = Path::new(SETUP)
        .join("pack")
        .join(format!("{}-", repo_key(SUPERPOWERS)))
        .join("skills/brainstorming");
    assert_eq!(of(&entries, &no_commit), ADDED_BY_YOU);
    let no_dash = Path::new(SETUP)
        .join("pack")
        .join(repo_key(SUPERPOWERS))
        .join("skills/brainstorming");
    assert_eq!(of(&entries, &no_dash), ADDED_BY_YOU);
}

#[test]
fn an_entry_without_a_path_holds_its_whole_checkout() {
    let repo = "https://github.com/safishamsi/graphify";
    let entries = [git("graphify", repo, None)];
    assert_eq!(
        of(&entries, &checkout(repo, COMMIT).join("graphify")),
        "Default pack: graphify"
    );
    assert_eq!(
        of(&entries, &checkout(repo, COMMIT)),
        "Default pack: graphify"
    );
}

#[test]
fn the_entry_with_the_longest_path_wins_and_paths_match_whole_folders() {
    let repo = "https://github.com/mattpocock/skills";
    let entries = [
        git("skills-all", repo, Some("skills")),
        git("grill-me", repo, Some("skills/productivity/grill-me")),
    ];
    let root = checkout(repo, COMMIT);
    let grill = root.join("skills/productivity/grill-me");
    assert_eq!(of(&entries, &grill), "Default pack: grill-me");
    let reversed = [entries[1].clone(), entries[0].clone()];
    assert_eq!(of(&reversed, &grill), "Default pack: grill-me");
    assert_eq!(
        of(&entries, &root.join("skills/productivity/grill-me-too")),
        "Default pack: skills-all"
    );
    assert_eq!(of(&entries, &root.join("skills-extra/notes")), ADDED_BY_YOU);
}

#[test]
fn a_bundled_skill_names_its_entry() {
    let entries = [entry(
        "software-practices",
        PackSource::Bundled {
            path: "skills/software-practices".into(),
        },
    )];
    let bundled = Path::new(SETUP).join("bundled");
    assert_eq!(
        of(&entries, &bundled.join("skills/software-practices/code-review")),
        "Default pack: software-practices"
    );
    assert_eq!(
        of(
            &entries,
            &bundled.join("skills/software-practices-old/code-review")
        ),
        ADDED_BY_YOU
    );
    let unbundled = Path::new(SETUP).join("pack/skills/software-practices/code-review");
    assert_eq!(of(&entries, &unbundled), ADDED_BY_YOU);
}

#[test]
fn imports_name_the_harness_folder_they_came_from() {
    let home = Path::new(HOME);
    assert_eq!(
        of(&[], &home.join(".claude/skills/notes")),
        "Imported from Claude Code"
    );
    assert_eq!(
        of(&[], &home.join(".codex/skills/graph")),
        "Imported from Codex"
    );
    assert_eq!(of(&[], &home.join(".claude/skills-old/notes")), ADDED_BY_YOU);
    assert_eq!(of(&[], &home.join("Skills/notes")), ADDED_BY_YOU);
    assert_eq!(of(&[], Path::new("/picked/notes")), ADDED_BY_YOU);
    let unknown_home = label(
        Path::new(SETUP),
        None,
        &[],
        &home.join(".claude/skills/notes").display().to_string(),
    );
    assert_eq!(unknown_home, ADDED_BY_YOU);
}

#[test]
fn a_pack_folder_wins_over_an_import_folder() {
    let entries = [git("superpowers", SUPERPOWERS, Some("skills"))];
    let setup = Path::new(HOME).join(".claude/skills/setup");
    let source = setup
        .join("pack")
        .join(format!("{}-{COMMIT}", repo_key(SUPERPOWERS)))
        .join("skills/brainstorming");
    let named = label(
        &setup,
        Some(Path::new(HOME)),
        &entries,
        &source.display().to_string(),
    );
    assert_eq!(named, "Default pack: superpowers");
}

#[test]
fn a_source_that_climbs_out_with_dot_dot_is_added_by_you() {
    let entries = [git("superpowers", SUPERPOWERS, Some("skills"))];
    let climbed = checkout(SUPERPOWERS, COMMIT).join("skills/../../../../picked/notes");
    assert_eq!(of(&entries, &climbed), ADDED_BY_YOU);
    let imported = Path::new(HOME).join(".claude/skills/../../picked");
    assert_eq!(of(&entries, &imported), ADDED_BY_YOU);
}

#[test]
fn every_shipped_entry_names_the_folder_it_installs_into() {
    let entries = pack_entries();
    assert!(!entries.is_empty());
    for entry in entries {
        let folder = match &entry.source {
            PackSource::Git { repo, commit, path } => {
                let root = checkout(repo, commit);
                match path {
                    Some(path) => root.join(path),
                    None => root,
                }
            }
            PackSource::Bundled { path } => Path::new(SETUP).join("bundled").join(path),
        };
        assert_eq!(
            of(entries, &folder.join("a-skill")),
            format!("Default pack: {}", entry.id),
            "{folder:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_pack_folder_is_the_one_the_installer_checks_out() {
    let setup = Path::new(SETUP);
    assert_eq!(
        crate::setup::fetch::checkout_folder(setup, SUPERPOWERS, COMMIT),
        checkout(SUPERPOWERS, COMMIT)
    );
    let key = repo_key(SUPERPOWERS);
    assert_eq!(key.len(), 16);
    assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
}
