use super::registry::Registry;

const SAMPLE: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

/// Task 18: an installed harness older than its manifest's `version.minimum` is too old to
/// drive, so both apps show "needs X or newer" with its Update line; the floor itself, a
/// newer version, an unknown version or one that is not plain numbers is never too old, and
/// a harness with no floor never is.
#[test]
fn an_installed_harness_below_its_manifest_floor_is_too_old_and_nothing_else_is() {
    let floored = format!("{SAMPLE}version:\n  minimum: \"2.4.0\"\n");
    let open = SAMPLE.replace("sample", "open");
    let built = Registry::build(&[("sample", floored.as_str()), ("open", open.as_str())]);
    let built = built.ok();
    let sample = built.as_ref().and_then(|registry| registry.get("sample"));
    let open = built.as_ref().and_then(|registry| registry.get("open"));

    let answers: Vec<bool> = [
        Some("2.3.9"),
        Some("1.0"),
        Some("2.4.0"),
        Some("2.10.0"),
        None,
    ]
    .into_iter()
    .chain([Some("nightly")])
    .map(|installed| sample.is_some_and(|harness| super::too_old(harness, installed)))
    .collect();

    assert_eq!(answers, [true, true, false, false, false, false]);
    assert!(open.is_some_and(|harness| !super::too_old(harness, Some("0.0.1"))));
    assert!(sample.is_some_and(|harness| harness.minimum_version() == Some("2.4.0")));
}

/// Task 18: the floor a versions row names is the manifest's own, for every bundled harness.
#[test]
fn every_bundled_harness_names_its_manifest_floor() {
    for harness in super::registry::registry().all() {
        let id = harness.manifest().id.as_str();
        assert_eq!(super::run_min(id), harness.minimum_version(), "{id}");
    }
}
