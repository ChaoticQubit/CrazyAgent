use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GATE: &str = "clean-mac-gate";
const TRACE: &str = "owner-trace";
const ENGINE_MANIFEST: &str = "crates/crazyagent/Cargo.toml";
const DESKTOP_MANIFEST: &str = "apps/desktop/src-tauri/Cargo.toml";
const RELEASE_SOURCES: [&str; 8] = [
    "Cargo.toml",
    DESKTOP_MANIFEST,
    "Makefile",
    ".github/workflows/release.yml",
    ".github/workflows/ci.yml",
    "apps/desktop/package.json",
    "apps/desktop/src-tauri/tauri.conf.json",
    "apps/desktop/src-tauri/tauri.linux.conf.json",
];
const MANIFESTS_MAX: usize = 64;
const LISTING_BYTES_MAX: usize = 64 * 1024;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn table(path: &Path) -> toml::Table {
    fs::read_to_string(path).unwrap().parse().unwrap()
}

fn names_feature(value: &toml::Value, feature: &str) -> bool {
    value.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.as_str().is_some_and(|item| item.contains(feature)))
    })
}

/// The Cargo.toml files git tracks in the repo but the engine's own, which alone defines
/// the checked features: the root workspace, the desktop app and any crate that depends on the
/// engine. Folders git does not track (agent worktrees, tool state) are never read.
/// Input: the repo. Output: the manifests, relative to the repo.
fn tracked_manifests(repo: &Path) -> Vec<PathBuf> {
    let output = crazyagent::spawn_gate::output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["ls-files", "-z", "--", "*Cargo.toml"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= LISTING_BYTES_MAX);
    let manifests: Vec<PathBuf> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(std::str::from_utf8(entry).unwrap()))
        .filter(|path| {
            path.file_name() == Some(OsStr::new("Cargo.toml")) && path != Path::new(ENGINE_MANIFEST)
        })
        .collect();
    assert!(manifests.len() <= MANIFESTS_MAX, "{manifests:?}");
    manifests
}

/// The files a release build reads that could name a checked feature: the release
/// workflow and the CI workflow it calls, scripts and bundle configs, and the given
/// manifests ([`tracked_manifests`]).
/// Inputs: the repo folder and the manifests. Output: the files, each once.
fn sources(repo: &Path, manifests: &[PathBuf]) -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = RELEASE_SOURCES.iter().map(|path| repo.join(path)).collect();
    for entry in fs::read_dir(repo.join("scripts/release")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".mjs") && !name.ends_with(".test.mjs") {
            sources.push(path);
        }
    }
    sources.extend(manifests.iter().map(|manifest| repo.join(manifest)));
    sources.sort();
    sources.dedup();
    sources
}

/// Everything that would let a release build carry a test-only engine feature: another
/// engine feature or the desktop app's engine dependency turning it on, or a release source
/// naming it or building with every feature. Inputs: the repo folder, its manifests and the
/// feature. Output: one line per problem, empty when no release build can turn it on.
fn release_problems(repo: &Path, manifests: &[PathBuf], feature: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let engine = table(&repo.join(ENGINE_MANIFEST));
    if let Some(features) = engine.get("features").and_then(toml::Value::as_table) {
        for (name, members) in features {
            if name != feature && names_feature(members, feature) {
                problems.push(format!("engine feature {name} turns on {feature}"));
            }
        }
    }
    let desktop = table(&repo.join(DESKTOP_MANIFEST));
    match desktop
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("crazyagent"))
    {
        None => problems.push("the desktop app no longer depends on the engine crate".into()),
        Some(dependency)
            if dependency
                .get("features")
                .is_some_and(|features| names_feature(features, feature)) =>
        {
            problems.push(format!(
                "the desktop app's engine dependency turns on {feature}"
            ));
        }
        Some(_) => {}
    }
    for source in sources(repo, manifests) {
        let text = fs::read_to_string(&source).unwrap();
        if text.contains(feature) {
            problems.push(format!("{} names {feature}", source.display()));
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
    let repo = repo();
    let manifests = tracked_manifests(&repo);
    for manifest in ["Cargo.toml", DESKTOP_MANIFEST] {
        assert!(
            manifests.contains(&PathBuf::from(manifest)),
            "{manifest} is not read: {manifests:?}"
        );
    }
    assert!(!manifests.contains(&PathBuf::from(ENGINE_MANIFEST)));
    let problems = release_problems(&repo, &manifests, GATE);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// No release build can carry the owner trace, which prints one stderr line per owner
/// exchange: the engine crate defines it turning on nothing else, no other feature, default
/// or desktop dependency turns it on, and no release workflow, script or bundle config names
/// it or builds with every feature. Only the test farm's build turns it on.
#[test]
fn no_release_build_enables_the_owner_trace() {
    let repo = repo();
    let engine = table(&repo.join(ENGINE_MANIFEST));
    let defined = engine["features"].get(TRACE);
    assert_eq!(defined, Some(&toml::Value::Array(Vec::new())));
    let problems = release_problems(&repo, &tracked_manifests(&repo), TRACE);
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
    let manifests = tracked_manifests(&real);
    let mut files = sources(&real, &manifests);
    files.extend([real.join(ENGINE_MANIFEST), real.join(DESKTOP_MANIFEST)]);
    for file in files {
        let at = copy.path().join(file.strip_prefix(&real).unwrap());
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::copy(&file, &at).unwrap();
    }
    assert!(release_problems(copy.path(), &manifests, GATE).is_empty());
    let manifest = copy.path().join(DESKTOP_MANIFEST);
    let mut desktop: toml_edit::DocumentMut =
        fs::read_to_string(&manifest).unwrap().parse().unwrap();
    desktop["features"]["gate"] =
        toml_edit::value(toml_edit::Array::from_iter([format!("crazyagent/{GATE}")]));
    desktop["features"]["default"]
        .as_array_mut()
        .expect("the desktop app has default features")
        .push("gate");
    fs::write(&manifest, desktop.to_string()).unwrap();
    let problems = release_problems(copy.path(), &manifests, GATE);
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains(DESKTOP_MANIFEST) && problem.contains(GATE)),
        "{problems:#?}"
    );
}
