use super::PROFILE_HOOKS;
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::paths::any_case;
use kernel_sandbox::profile::seatbelt::{
    agent_config_denies, clone_store_denies, engine_secrets_in, hfs_ignored_deny, launch_denies,
    regranted_denies, seatbelt_pattern,
};
use kernel_sandbox::profile::{ProfileHooks, ProjectSandbox};
use std::fs;
use std::path::Path;

/// The engine secrets name every engine secret folder the hooks list under the runtime,
/// and no engine folder the hooks leave out.
#[test]
fn the_engine_secrets_name_the_secret_folders_the_hooks_list() {
    static STAND_IN: ProfileHooks = ProfileHooks {
        engine_secret_folders: &["stand-in-secrets"],
        ..PROFILE_HOOKS
    };
    let runtime = tempfile::tempdir().unwrap();
    let secrets = engine_secrets_in(&STAND_IN, runtime.path()).unwrap();
    assert!(
        secrets.contains(&runtime.path().join("stand-in-secrets")),
        "{secrets:?}"
    );
    assert!(
        !secrets.contains(&runtime.path().join(crate::ide::FOLDER)),
        "{secrets:?}"
    );
}

/// The launch denies keep writes off the launch lock the hooks name, and name no other
/// lock.
#[test]
fn the_launch_denies_keep_writes_off_the_lock_the_hooks_name() {
    static STAND_IN: ProfileHooks = ProfileHooks {
        launch_lock: "stand-in.lock",
        ..PROFILE_HOOKS
    };
    let denies = launch_denies(&STAND_IN, Path::new("/private/tmp/agent/home"));
    assert!(denies.contains(&any_case("stand-in.lock")), "{denies}");
    assert!(
        !denies.contains(&any_case(crate::launch_context::LOCK)),
        "{denies}"
    );
}

/// The agent home's config and kept-entry denies and the
/// regranted flag and `.git` denies match a path and everything below it as one `X(/|$)`
/// regex, never with a `(/.*)` tail, which libsandbox fails to compile again in lower case
/// after a fold group with no bracket class before it.
#[test]
fn home_denies_match_at_or_below_without_an_optional_tail() {
    let home = Path::new("/private/tmp/.tmpA1b2C3/agent/home");
    let data = Path::new("/private/tmp/.tmpA1b2C3/data");
    let config = agent_config_denies(&PROFILE_HOOKS, home);
    assert!(!config.contains("(/.*)"), "{config}");
    let regranted = regranted_denies(&PROFILE_HOOKS, home, &[data]);
    assert!(!regranted.contains("(/.*)"), "{regranted}");
    let flagged = seatbelt_pattern(home);
    assert!(
        regranted.contains(&format!(
            "(deny file-write-flags file-write-acl (regex #\"^{flagged}(/|$)\"))"
        )),
        "{regranted}"
    );
    let git = any_case(".git");
    for root in [home, data] {
        let below = format!("{}/(.+/)?{git}", seatbelt_pattern(root));
        assert!(
            regranted.contains(&format!("(regex #\"^{below}(/|$)\")")),
            "{regranted}"
        );
    }
}

/// No regex of a closed-tops agent's profile, a clone's store denies or a home's config denies
/// holds a `(/.*)` tail. libsandbox fails its second, lower-case compile of one after a fold group
/// with no bracket class or wildcard before it, as `^/x/(s|S)(/.*)?$` (probed 11-17), so each is
/// one `X(/|$)` regex instead. The HFS rule alone keeps a tail, after its leading `(.*/)?`, which
/// compiles.
#[test]
fn no_profile_regex_has_a_slash_tail() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let (home, tmp, project) = (base.join("home"), base.join("tmp"), base.join("project"));
    for folder in [&home, &tmp, &project] {
        fs::create_dir_all(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .keep_project_secrets(&project, &[])
    .unwrap()
    .open_only_tops(&["notes".to_string()])
    .unwrap();
    let text = [
        sandbox.profile(),
        clone_store_denies(&base.join("clone")),
        agent_config_denies(&PROFILE_HOOKS, &home),
    ]
    .concat();
    let hfs = hfs_ignored_deny();
    assert_eq!(text.matches(hfs.as_str()).count(), 1);
    let text = text.replace(hfs.as_str(), "");
    let bodies: Vec<&str> = text
        .split("#\"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(body, _)| body))
        .collect();
    assert!(bodies.len() > 20, "{text}");
    let tailed: Vec<&&str> = bodies
        .iter()
        .filter(|body| body.contains("(/.*)"))
        .collect();
    assert!(tailed.is_empty(), "{tailed:#?}");
    let below = bodies.iter().filter(|body| body.ends_with("(/|$)")).count();
    assert!(below >= 20, "{below}");
}
