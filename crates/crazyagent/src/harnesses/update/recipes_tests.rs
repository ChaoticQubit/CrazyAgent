use super::*;
use crate::app_plugins::yaml;

const GUIDE: &str = "guide: \"https://example.com/install\"\n";
const PACKAGE_SHAPE: &str = "a package name of lower-case letters, digits and @ / . _ -";

fn block(text: &str) -> &'static Value {
    Box::leak(Box::new(yaml::parse(text).unwrap()))
}

fn refusal(text: &str) -> String {
    read("sample", block(text))
        .err()
        .unwrap_or_else(|| panic!("{text:?} is not refused"))
}

fn own(rest: &str) -> String {
    format!("own:\n  root: \".sample/versions\"\n{rest}{GUIDE}")
}

fn pointer(rest: &str) -> String {
    own(&format!(
        "  args:\n    - \"update\"\n  pointer:\n    url: \"https://example.com/latest\"\n{rest}"
    ))
}

#[test]
fn every_bundled_harness_but_rcli_has_a_recipe_in_catalog_order() {
    let ids: Vec<&str> = recipes().iter().map(|recipe| recipe.id).collect();
    let listed: Vec<&str> = crate::harnesses::specs()
        .iter()
        .map(|spec| spec.id)
        .filter(|id| *id != "rcli")
        .collect();
    assert_eq!(ids, listed);
    assert_eq!(ids.len(), 17);
    assert!(recipe("rcli").is_none());
    assert!(recipe("no-such-harness").is_none());
    let updaters: Vec<&str> = recipes()
        .iter()
        .filter(|recipe| recipe.own.is_some())
        .map(|recipe| recipe.id)
        .collect();
    assert_eq!(updaters, ["codex", "claude-code"]);
    for recipe in recipes() {
        assert!(recipe.brews.len() <= BREWS_MAX, "{}", recipe.id);
        assert!(recipe.guide.len() <= TEXT_BYTES_MAX, "{}", recipe.id);
    }
}

#[test]
fn a_full_update_block_reads_every_setting() {
    let recipe = read(
        "sample",
        block(concat!(
            "own:\n  root: \".sample/versions\"\n  args:\n    - \"self\"\n    - \"update\"\n",
            "  pointer:\n    url: \"https://example.com/latest\"\n    format: \"json-tag\"\n",
            "    field: \"tag_name\"\n    prefix: \"v\"\n    bytes_max: 512\n",
            "formulae:\n  - \"sample\"\nknown_formulae:\n  - \"sample-old\"\n",
            "npm:\n  scope: \"@sample\"\n  name: \"cli\"\n  package: \"@sample/cli\"\n",
            "  spec: \"@sample/cli@latest\"\n  flags:\n    - \"--ignore-scripts\"\n",
            "guide: \"https://example.com/install\"\n",
        )),
    )
    .unwrap();
    assert_eq!(recipe.id, "sample");
    assert_eq!(recipe.guide, "https://example.com/install");
    let updater = recipe.own.as_ref().unwrap();
    assert_eq!(updater.root, ".sample/versions");
    assert_eq!(updater.args, ["self", "update"]);
    assert_eq!(
        updater.pointer,
        Pointer {
            url: "https://example.com/latest",
            format: PointerFormat::JsonTag {
                field: "tag_name",
                prefix: "v",
            },
            bytes_max: 512,
        }
    );
    assert_eq!(recipe.brews.len(), 2);
    assert!(recipe.brews[0].kind == BrewKind::Formula && recipe.brews[0].install);
    assert_eq!(recipe.brews[0].name, "sample");
    assert!(recipe.brews[1].kind == BrewKind::Formula && !recipe.brews[1].install);
    assert_eq!(recipe.brews[1].name, "sample-old");
    let npm = recipe.npm.as_ref().unwrap();
    assert_eq!(
        (npm.scope, npm.name, npm.package, npm.spec),
        ("@sample", "cli", "@sample/cli", "@sample/cli@latest")
    );
    assert_eq!(npm.flags, ["--ignore-scripts"]);
}

#[test]
fn the_smallest_update_block_is_a_guide_and_its_defaults_are_empty() {
    let recipe = read("sample", block(GUIDE)).unwrap();
    assert!(recipe.own.is_none() && recipe.npm.is_none() && recipe.brews.is_empty());
    let plain = read("sample", block(&pointer("    format: \"plain-version\"\n"))).unwrap();
    let updater = plain.own.as_ref().unwrap();
    assert_eq!(updater.pointer.format, PointerFormat::PlainVersion);
    assert_eq!(updater.pointer.bytes_max, POINTER_BYTES_MAX);
    let unscoped = read(
        "sample",
        block(&format!(
            "casks:\n  - \"sample\"\nknown_casks:\n  - \"sample@beta\"\nnpm:\n  name: \"sample\"\n  package: \"sample\"\n  spec: \"sample\"\n{GUIDE}"
        )),
    )
    .unwrap();
    let npm = unscoped.npm.as_ref().unwrap();
    assert_eq!(npm.scope, "");
    assert!(npm.flags.is_empty());
    assert!(unscoped.brews[0].kind == BrewKind::Cask && unscoped.brews[0].install);
    assert!(unscoped.brews[1].kind == BrewKind::Cask && !unscoped.brews[1].install);
    assert_eq!(unscoped.brews[1].name, "sample@beta");
}

#[test]
fn a_key_a_recipe_does_not_have_or_a_missing_guide_is_refused() {
    assert_eq!(
        refusal(&format!("colour: \"red\"\n{GUIDE}")),
        "update.colour: this key is not one a recipe has."
    );
    assert_eq!(
        refusal("formulae:\n  - \"sample\"\n"),
        "update.guide: this setting is needed."
    );
    assert_eq!(
        refusal("guide: \"http://example.com/install\"\n"),
        "update.guide: http://example.com/install is not an https address without spaces."
    );
    assert_eq!(
        refusal("guide: \"https://example.com/a b\"\n"),
        "update.guide: https://example.com/a b is not an https address without spaces."
    );
    assert_eq!(
        read("sample", Box::leak(Box::new(Value::Str("brew".to_owned())))).err(),
        Some("update: this must be a block of settings.".to_owned())
    );
    assert_eq!(
        refusal(&format!("npm:\n  title: \"x\"\n{GUIDE}")),
        "update.npm.title: this key is not one a recipe has."
    );
    assert_eq!(
        refusal(&own("  shell: \"sh\"\n")),
        "update.own.shell: this key is not one a recipe has."
    );
}

#[test]
fn a_homebrew_name_that_could_be_read_as_an_option_or_is_not_plain_is_refused() {
    for name in [
        "--force",
        "-s",
        "Sample",
        "sample cli",
        "sample;id",
        "$(id)",
    ] {
        assert_eq!(
            refusal(&format!("formulae:\n  - \"{name}\"\n{GUIDE}")),
            format!("update.formulae: {name} is not {PACKAGE_SHAPE}."),
            "{name}"
        );
        assert_eq!(
            refusal(&format!("known_casks:\n  - \"{name}\"\n{GUIDE}")),
            format!("update.known_casks: {name} is not {PACKAGE_SHAPE}."),
            "{name}"
        );
    }
    assert_eq!(
        refusal(&format!(
            "formulae:\n  - \"sample\"\ncasks:\n  - \"sample\"\n{GUIDE}"
        )),
        "update.casks: sample is listed twice."
    );
    assert_eq!(
        refusal(&format!(
            "formulae:\n  - \"a\"\n  - \"b\"\n  - \"c\"\ncasks:\n  - \"d\"\n  - \"e\"\n{GUIDE}"
        )),
        "update: a recipe lists at most 4 Homebrew names."
    );
    assert_eq!(
        refusal(&format!(
            "formulae:\n  - \"a\"\n  - \"b\"\n  - \"c\"\n  - \"d\"\n  - \"e\"\n{GUIDE}"
        )),
        "update.formulae: this must be a list of at most 4 texts."
    );
    assert_eq!(
        refusal(&format!("formulae: \"sample\"\n{GUIDE}")),
        "update.formulae: this must be a list of at most 4 texts."
    );
}

#[test]
fn an_npm_package_spec_or_flag_that_is_not_plain_is_refused() {
    let npm =
        |rest: &str| format!("npm:\n  name: \"cli\"\n  package: \"@sample/cli\"\n{rest}{GUIDE}");
    assert_eq!(
        refusal(&npm("")),
        "update.npm.spec: this setting is needed."
    );
    assert_eq!(
        refusal(&npm("  spec: \"@sample/cli; id\"\n")),
        format!("update.npm.spec: @sample/cli; id is not {PACKAGE_SHAPE}.")
    );
    assert_eq!(
        refusal(&npm("  spec: \"--global\"\n")),
        format!("update.npm.spec: --global is not {PACKAGE_SHAPE}.")
    );
    assert_eq!(
        refusal(&npm("  spec: \"@sample/cli\"\n  scope: \"@Sample\"\n")),
        format!("update.npm.scope: @Sample is not {PACKAGE_SHAPE}.")
    );
    for flag in ["-g", "--", "--Global", "--prefix /tmp", "global"] {
        assert_eq!(
            refusal(&npm(&format!(
                "  spec: \"@sample/cli\"\n  flags:\n    - \"{flag}\"\n"
            ))),
            format!(
                "update.npm.flags: {flag} is not an option that starts with -- and holds lower-case letters, digits, - and =."
            ),
            "{flag}"
        );
    }
    assert_eq!(
        refusal(&npm(
            "  spec: \"@sample/cli\"\n  flags:\n    - \"--a\"\n    - \"--b\"\n"
        )),
        "update.npm.flags: this must be a list of at most 1 texts."
    );
    assert_eq!(
        refusal(&format!("npm:\n  package: \"@sample/cli\"\n{GUIDE}")),
        "update.npm.name: this setting is needed."
    );
}

#[test]
fn an_updater_outside_the_home_folder_or_with_a_pointer_that_is_not_https_is_refused() {
    assert_eq!(
        refusal(&own("")),
        "update.own.args: this setting is needed."
    );
    assert_eq!(
        refusal(&own("  args:\n    - \"update\"\n")),
        "update.own.pointer: this setting is needed."
    );
    assert_eq!(
        refusal(&own("  args:\n    - \"--force\"\n")),
        "update.own.args: --force is not a word of lower-case letters, digits and dashes."
    );
    for root in ["../versions", "/opt/sample", "a//b", "a/./b"] {
        assert_eq!(
            refusal(&format!(
                "own:\n  root: \"{root}\"\n  args:\n    - \"update\"\n{GUIDE}"
            )),
            format!("update.own.root: {root} is not a folder under the home folder."),
            "{root}"
        );
    }
    let format = "update.own.pointer.format: this must be plain-version, or json-tag with a field and a prefix.";
    assert_eq!(refusal(&pointer("    format: \"xml\"\n")), format);
    assert_eq!(refusal(&pointer("    format: \"json-tag\"\n")), format);
    assert_eq!(
        refusal(&pointer(
            "    format: \"plain-version\"\n    field: \"tag_name\"\n"
        )),
        format
    );
    assert_eq!(
        refusal(&pointer("")),
        "update.own.pointer.format: this setting is needed."
    );
    let bytes = format!(
        "update.own.pointer.bytes_max: this must be a whole number from 1 to {POINTER_BYTES_MAX}."
    );
    for value in ["0", "-1", "262145", "\"64\"", "1.5"] {
        assert_eq!(
            refusal(&pointer(&format!(
                "    format: \"plain-version\"\n    bytes_max: {value}\n"
            ))),
            bytes,
            "{value}"
        );
    }
    assert_eq!(
        refusal(&own(
            "  args:\n    - \"update\"\n  pointer:\n    url: \"http://example.com/latest\"\n    format: \"plain-version\"\n"
        )),
        "update.own.pointer.url: http://example.com/latest is not an https address without spaces."
    );
    assert_eq!(
        refusal(&pointer(
            "    format: \"json-tag\"\n    field: \"tag.name\"\n    prefix: \"v\"\n"
        )),
        "update.own.pointer.field: tag.name is not a field name of lower-case letters, digits and _."
    );
}
