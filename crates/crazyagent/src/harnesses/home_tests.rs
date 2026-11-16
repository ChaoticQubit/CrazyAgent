use super::*;
use crate::harnesses::manifest::{PLACEHOLDER, TYPE, UNKNOWN_KEY, VALUE, parse};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

const SMALL: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

fn home_of(block: &str) -> Result<Home, (&'static str, String)> {
    let manifest = parse(format!("{SMALL}home:\n{block}").as_bytes()).unwrap();
    read(&manifest).map_err(|refused| (refused.code, refused.message))
}

fn private_base() -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let path = base.path().canonicalize().unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (base, path)
}

/// Task 18: a manifest's home block gives the harness its environment, each value with the
/// private home filled in, and HOME itself only when the block asks for it.
#[test]
fn a_home_block_gives_its_environment_with_the_home_filled_and_home_only_when_asked() {
    let home = Path::new("/private/tmp/agent/sample/account-1");
    let asked = home_of(
        "  env:\n    SAMPLE_DIR: \"{home}/agent\"\n    SAMPLE_TELEMETRY: \"0\"\n  set_home: true\n",
    );
    let plain = home_of("  env:\n    SAMPLE_DIR: \"{home}\"\n");

    assert_eq!(
        asked.map(|found| found.environment(home)),
        Ok(vec![
            (
                OsString::from("SAMPLE_DIR"),
                OsString::from("/private/tmp/agent/sample/account-1/agent"),
            ),
            (OsString::from("SAMPLE_TELEMETRY"), OsString::from("0")),
            (OsString::from("HOME"), home.as_os_str().to_owned()),
        ])
    );
    assert_eq!(
        plain.map(|found| found.environment(home)),
        Ok(vec![(
            OsString::from("SAMPLE_DIR"),
            home.as_os_str().to_owned()
        )])
    );
}

/// Task 18: a home block may not set what the engine sets or what decides which code runs,
/// fill in anything but `{home}`, point outside the private home or carry a key it does not
/// have; each refusal is one sentence that names the setting.
#[test]
fn a_home_block_refuses_names_values_and_keys_the_engine_owns() {
    let owned = "CrazyAgent sets this itself, so a manifest cannot.";
    let outside = "{home} must start the value and be followed only by a folder inside it.";
    let cases = [
        (
            "  colour: \"red\"\n",
            UNKNOWN_KEY,
            "home.colour: this key is not one the home block has.".to_owned(),
        ),
        (
            "  set_home: \"yes\"\n",
            TYPE,
            "home.set_home: this must be true or false.".to_owned(),
        ),
        (
            "  env:\n    - \"PATH\"\n",
            TYPE,
            "home.env: this must be a map of names to text in quotes.".to_owned(),
        ),
        (
            "  env:\n    PATH: \"/tmp\"\n",
            VALUE,
            format!("home.env.PATH: {owned}"),
        ),
        (
            "  env:\n    HOME: \"{home}\"\n",
            VALUE,
            format!("home.env.HOME: {owned}"),
        ),
        (
            "  env:\n    DYLD_INSERT_LIBRARIES: \"/tmp/x.dylib\"\n",
            VALUE,
            format!("home.env.DYLD_INSERT_LIBRARIES: {owned}"),
        ),
        (
            "  env:\n    CRAZYAGENT_HOME: \"{home}\"\n",
            VALUE,
            format!("home.env.CRAZYAGENT_HOME: {owned}"),
        ),
        (
            "  env:\n    NODE_OPTIONS: \"--require /tmp/x.js\"\n",
            VALUE,
            format!("home.env.NODE_OPTIONS: {owned}"),
        ),
        (
            "  env:\n    lower_case: \"1\"\n",
            VALUE,
            "home.env.lower_case: a name is capital letters, digits and _, starting with a letter."
                .to_owned(),
        ),
        (
            "  env:\n    SAMPLE_LEVEL: 1\n",
            TYPE,
            "home.env.SAMPLE_LEVEL: this must be text in quotes.".to_owned(),
        ),
        (
            "  env:\n    SAMPLE_ID: \"{session}\"\n",
            PLACEHOLDER,
            "home.env.SAMPLE_ID: only {home} is filled in here.".to_owned(),
        ),
        (
            "  env:\n    SAMPLE_DIR: \"{home}/../out\"\n",
            VALUE,
            format!("home.env.SAMPLE_DIR: {outside}"),
        ),
        (
            "  env:\n    SAMPLE_DIR: \"/tmp{home}\"\n",
            VALUE,
            format!("home.env.SAMPLE_DIR: {outside}"),
        ),
    ];
    for (block, code, sentence) in cases {
        assert_eq!(home_of(block).err(), Some((code, sentence)), "{block}");
    }
}

/// Task 18: every bundled manifest's home block reads, so no launch of a bundled harness is
/// refused for its own manifest.
#[test]
fn every_bundled_manifest_home_block_reads() {
    for harness in crate::harnesses::registry::registry().all() {
        let found = read(harness.manifest());
        assert!(found.is_ok(), "{}: {:?}", harness.manifest().id, found);
    }
}

/// Task 18: each account gets its own private home under the harness's canonical id, every
/// folder 0700, and a link planted where a folder of the home belongs is refused, never
/// followed.
#[test]
fn a_private_home_is_a_0700_folder_per_harness_and_account_and_never_follows_a_link() {
    let (_base, base) = private_base();
    let first = CredentialRef::new("account-1").unwrap();
    let second = CredentialRef::new("account-2").unwrap();
    let planted = CredentialRef::new("account-3").unwrap();

    let made = prepare(&base, "claude", &first);
    let other = prepare(&base, "claude-code", &second);

    let harness = base.join("claude-code");
    assert_eq!(made.ok(), Some(harness.join("account-1")));
    assert_eq!(other.ok(), Some(harness.join("account-2")));
    for folder in [
        &harness,
        &harness.join("account-1"),
        &harness.join("account-2"),
    ] {
        let found = std::fs::symlink_metadata(folder).map(|found| (found.is_dir(), found.mode()));
        assert_eq!(
            found.ok().map(|(dir, mode)| (dir, mode & 0o7777)),
            Some((true, 0o700)),
            "{}",
            folder.display()
        );
    }
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), harness.join("account-3")).unwrap();
    let refused = prepare(&base, "claude-code", &planted);
    assert_eq!(
        refused.err().map(|refused| refused.0),
        Some("launch_account_home_unsafe".to_owned())
    );
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    assert_eq!(
        prepare(&base, "no-such-harness", &first)
            .err()
            .map(|refused| refused.0),
        Some("launch_harness_unsupported".to_owned())
    );
}
