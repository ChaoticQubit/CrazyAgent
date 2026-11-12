use std::fs;
use std::path::{Path, PathBuf};

const FEATURE: &str = "clean-mac-gate";
const ENGINE_MANIFEST: &str = "crates/crazyagent/Cargo.toml";
const DESKTOP_MANIFEST: &str = "apps/desktop/src-tauri/Cargo.toml";
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

/// The files a release build reads that could name the gate feature: the release
/// workflow, scripts and bundle configs. Input: the repo folder. Output: the files.
fn sources(repo: &Path) -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = RELEASE_SOURCES.iter().map(|path| repo.join(path)).collect();
    for entry in fs::read_dir(repo.join("scripts/release")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".mjs") && !name.ends_with(".test.mjs") {
            sources.push(path);
        }
    }
    sources
}

/// Everything that would let a release build carry the clean-Mac gate's system-root remap:
/// another engine feature or the desktop app's engine dependency turning it on, or a
/// release source naming it or building with every feature. Input: the repo folder.
/// Output: one line per problem, empty when no release build can turn the remap on.
fn release_problems(repo: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let engine = table(&repo.join(ENGINE_MANIFEST));
    if let Some(features) = engine.get("features").and_then(toml::Value::as_table) {
        for (name, members) in features {
            if name != FEATURE && names_feature(members) {
                problems.push(format!("engine feature {name} turns on {FEATURE}"));
            }
        }
    }
    let desktop = table(&repo.join(DESKTOP_MANIFEST));
    match desktop
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("crazyagent"))
    {
        None => problems.push("the desktop app no longer depends on the engine crate".into()),
        Some(dependency) if dependency.get("features").is_some_and(names_feature) => {
            problems.push(format!(
                "the desktop app's engine dependency turns on {FEATURE}"
            ));
        }
        Some(_) => {}
    }
    for source in sources(repo) {
        let text = fs::read_to_string(&source).unwrap();
        if text.contains(FEATURE) {
            problems.push(format!("{} names {FEATURE}", source.display()));
        }
        if text.contains("--all-features") {
            problems.push(format!("{} builds with every feature", source.display()));
        }
    }
    problems
}

/// No release build can carry the clean-Mac gate's system-root remap: the engine crate's
/// default and every other feature leave it out, the desktop app never turns it on, and no
/// release workflow, script or bundle config names it or builds with every feature. Only
/// `scripts/gate/clean-mac.sh` builds with it.
#[test]
fn no_release_build_enables_the_clean_mac_gate_remap() {
    let problems = release_problems(&repo());
    assert!(problems.is_empty(), "{problems:#?}");
}

/// A desktop feature that turns on the engine's gate feature, kept in the desktop's default
/// features, puts the remap in every desktop build, and the engine with it when cargo builds
/// both together; the release row reports it. Built on a copy of this repo's release files
/// with only the desktop manifest changed.
#[test]
fn a_desktop_feature_that_turns_on_the_gate_fails_the_release_row() {
    let real = repo();
    let copy = tempfile::tempdir().unwrap();
    let mut files = sources(&real);
    files.extend([real.join(ENGINE_MANIFEST), real.join(DESKTOP_MANIFEST)]);
    for file in files {
        let at = copy.path().join(file.strip_prefix(&real).unwrap());
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::copy(&file, &at).unwrap();
    }
    assert!(release_problems(copy.path()).is_empty());
    let manifest = copy.path().join(DESKTOP_MANIFEST);
    let mut desktop: toml_edit::DocumentMut =
        fs::read_to_string(&manifest).unwrap().parse().unwrap();
    desktop["features"]["gate"] = toml_edit::value(toml_edit::Array::from_iter([format!(
        "crazyagent/{FEATURE}"
    )]));
    desktop["features"]["default"]
        .as_array_mut()
        .expect("the desktop app has default features")
        .push("gate");
    fs::write(&manifest, desktop.to_string()).unwrap();
    let problems = release_problems(copy.path());
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains(DESKTOP_MANIFEST) && problem.contains(FEATURE)),
        "{problems:#?}"
    );
}
