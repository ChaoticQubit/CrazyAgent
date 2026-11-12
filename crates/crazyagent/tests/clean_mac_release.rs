use std::fs;
use std::path::{Path, PathBuf};

const FEATURE: &str = "clean-mac-gate";
const RELEASE_SOURCES: [&str; 6] = [
    "Cargo.toml",
    "Makefile",
    ".github/workflows/release.yml",
    "apps/desktop/package.json",
    "apps/desktop/src-tauri/tauri.conf.json",
    "apps/desktop/src-tauri/tauri.linux.conf.json",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn table(path: &Path) -> toml::Table {
    fs::read_to_string(path).unwrap().parse().unwrap()
}

fn names_feature(value: &toml::Value) -> bool {
    value.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.as_str().is_some_and(|item| item.contains(FEATURE)))
    })
}

/// No release build can carry the clean-Mac gate's system-root remap: the engine crate's
/// default and every other feature leave it out, the desktop app's dependency on the engine
/// never turns it on, and no release workflow, script or bundle config names it or builds
/// with every feature. Only `scripts/gate/clean-mac.sh` builds with it.
#[test]
fn no_release_build_enables_the_clean_mac_gate_remap() {
    let repo = repo();
    let engine = table(&repo.join("crates/crazyagent/Cargo.toml"));
    if let Some(features) = engine.get("features").and_then(toml::Value::as_table) {
        for (name, members) in features {
            if name != FEATURE {
                assert!(!names_feature(members), "feature {name} turns on {FEATURE}");
            }
        }
    }
    let desktop = table(&repo.join("apps/desktop/src-tauri/Cargo.toml"));
    let dependency = desktop
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("crazyagent"))
        .expect("the desktop app depends on the engine crate");
    assert!(
        !dependency.get("features").is_some_and(names_feature),
        "the desktop app turns on {FEATURE}"
    );
    let mut sources: Vec<PathBuf> = RELEASE_SOURCES.iter().map(|path| repo.join(path)).collect();
    for entry in fs::read_dir(repo.join("scripts/release")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".mjs") && !name.ends_with(".test.mjs") {
            sources.push(path);
        }
    }
    for source in sources {
        let text = fs::read_to_string(&source).unwrap();
        assert!(
            !text.contains(FEATURE),
            "{} names {FEATURE}",
            source.display()
        );
        assert!(
            !text.contains("--all-features"),
            "{} builds with every feature",
            source.display()
        );
    }
}
