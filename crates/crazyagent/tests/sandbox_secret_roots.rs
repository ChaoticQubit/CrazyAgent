#![cfg(unix)]

use crazyagent::sandbox::{ProjectSandbox, SECRET_ROOT_REFUSED, SandboxError, SandboxNetwork};
use std::fs;
use std::path::PathBuf;

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    runtime: PathBuf,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let runtime = root.join("runtime");
    let project = root.join("project");
    let home = runtime.join("team").join("agents").join("a").join("home");
    let tmp = runtime.join("team").join("agents").join("a").join("tmp");
    for directory in [&project, &home, &tmp] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(runtime.join("account-keys").join("inner")).unwrap();
    fs::write(runtime.join("engine.db"), "").unwrap();
    Case {
        _temp: temp,
        root,
        runtime,
        project,
        home,
        tmp,
    }
}

fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
}

fn guarded(case: &Case) -> ProjectSandbox {
    sandbox(case).guard_engine(&case.runtime).unwrap()
}

fn owner_home() -> PathBuf {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .unwrap();
    user.dir.canonicalize().unwrap_or(user.dir)
}

fn walled(result: Result<ProjectSandbox, SandboxError>) {
    match result {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("expected the secret wall refusal, got {other:?}"),
    }
}

#[test]
fn a_root_that_holds_an_owner_secret_is_refused_as_read_root_worktree_and_project() {
    let case = case();
    let home = owner_home();
    for root in [
        home.join(".config"),
        home.join(".config").join("git"),
        home.join("Library"),
    ] {
        walled(sandbox(&case).allow_read(&root));
        walled(sandbox(&case).allow_worktree(&root));
        walled(ProjectSandbox::new(
            &root,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        ));
    }
}

#[test]
fn a_root_inside_an_owner_secret_is_refused() {
    let case = case();
    let home = owner_home();
    for root in [
        home.join(".ssh"),
        home.join(".ssh").join("keys"),
        home.join(".config").join("gh").join("hosts"),
        home.join(".aws").join("cli"),
        home.join("Library").join("Keychains").join("login"),
    ] {
        walled(sandbox(&case).allow_read(&root));
        walled(sandbox(&case).allow_worktree(&root));
        walled(ProjectSandbox::new(
            &case.project,
            &root,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        ));
    }
}

#[test]
fn a_root_that_holds_engine_secrets_is_refused_in_either_order() {
    let case = case();
    walled(guarded(&case).allow_read(&case.runtime));
    walled(guarded(&case).allow_worktree(&case.runtime));
    walled(
        sandbox(&case)
            .allow_read(&case.runtime)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    walled(
        sandbox(&case)
            .allow_worktree(&case.runtime)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    walled(
        ProjectSandbox::new(
            &case.root,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .guard_engine(&case.runtime),
    );
}

#[test]
fn a_root_inside_an_engine_secret_is_refused_in_either_order() {
    let case = case();
    let inner = case.runtime.join("account-keys").join("inner");
    let keys = case.runtime.join("account-keys");
    for root in [&keys, &inner] {
        walled(guarded(&case).allow_read(root));
        walled(guarded(&case).allow_worktree(root));
        walled(
            sandbox(&case)
                .allow_read(root)
                .unwrap()
                .guard_engine(&case.runtime),
        );
    }
    walled(
        ProjectSandbox::new(
            &case.project,
            &inner,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .guard_engine(&case.runtime),
    );
}

#[test]
fn unrelated_roots_and_look_alike_names_still_pass() {
    let case = case();
    let harness = case.root.join("harness");
    let look_alike = case.runtime.join("account-keys-old");
    let sibling = case.runtime.join("research");
    let worktree = case.root.join("worktree");
    for directory in [&harness, &look_alike, &sibling, &worktree] {
        fs::create_dir_all(directory).unwrap();
    }
    let widened = guarded(&case)
        .allow_read(&harness)
        .unwrap()
        .allow_read(&look_alike)
        .unwrap()
        .allow_read(&sibling)
        .unwrap()
        .allow_worktree(&worktree)
        .unwrap();
    assert_eq!(
        widened.read_roots(),
        [harness.clone(), look_alike.clone(), sibling.clone()].as_slice()
    );
    let regrouped = sandbox(&case)
        .allow_read(&sibling)
        .unwrap()
        .allow_worktree(&worktree)
        .unwrap()
        .guard_engine(&case.runtime)
        .unwrap();
    assert_eq!(regrouped.read_roots(), [sibling].as_slice());
    let secrets = regrouped.secret_paths();
    assert!(secrets.contains(&case.runtime.join("engine.db")));
    assert!(secrets.contains(&case.runtime.join("account-keys")));
    assert!(secrets.contains(&owner_home().join(".config").join("gh")));
    for root in regrouped.writable_roots().chain(regrouped.read_roots()) {
        assert!(
            secrets
                .iter()
                .all(|secret| !root.starts_with(secret) && !secret.starts_with(root)),
            "{}",
            root.display()
        );
    }
}

#[test]
fn the_app_plugin_key_folder_is_an_engine_secret_agents_cannot_reach() {
    let case = case();
    fs::create_dir_all(case.runtime.join("plugin-keys").join("inner")).unwrap();
    let secrets = guarded(&case).secret_paths();
    assert!(secrets.contains(&case.runtime.join("plugin-keys")));
    walled(guarded(&case).allow_read(&case.runtime.join("plugin-keys").join("inner")));
}

#[test]
fn the_app_plugin_request_scratch_folder_is_an_engine_secret_agents_cannot_reach() {
    let case = case();
    fs::create_dir_all(case.runtime.join("plugin-scratch").join("inner")).unwrap();
    let secrets = guarded(&case).secret_paths();
    assert!(secrets.contains(&case.runtime.join("plugin-scratch")));
    walled(guarded(&case).allow_read(&case.runtime.join("plugin-scratch").join("inner")));
}

#[test]
fn the_mcp_plugin_secret_folder_is_an_engine_secret_no_grant_reaches() {
    let case = case();
    let store = case.runtime.join("plugins");
    let secret_folder = store.join("secrets");
    let skills = store.join("skills");
    fs::create_dir_all(&secret_folder).unwrap();
    fs::create_dir_all(&skills).unwrap();
    fs::write(secret_folder.join("mcp-ref"), "synthetic").unwrap();
    let secrets = guarded(&case).secret_paths();
    assert!(secrets.contains(&secret_folder));
    assert!(!secrets.contains(&store));
    walled(guarded(&case).allow_read(&secret_folder));
    walled(guarded(&case).allow_read(&store));
    assert_eq!(
        guarded(&case).allow_read(&skills).unwrap().read_roots(),
        [skills.clone()].as_slice()
    );
    for server_parent in [&store, &secret_folder] {
        walled(
            sandbox(&case)
                .keep_ports(&[])
                .and_then(|sandbox| sandbox.guard_engine(&case.runtime))
                .and_then(|sandbox| sandbox.allow_read(server_parent)),
        );
        walled(
            sandbox(&case)
                .allow_read(server_parent)
                .and_then(|sandbox| sandbox.guard_engine(&case.runtime)),
        );
    }
    let profile = guarded(&case).profile();
    let deny = profile
        .split("(deny file-read* file-write*")
        .nth(1)
        .expect("a secret deny block");
    assert!(
        deny.contains(&format!("(subpath \"{}\")", secret_folder.display())),
        "{profile}"
    );
}

const NEAR_PLUGIN: &str = "schema: 1
kind: embedding
name: near
title: Near
version: 1
http:
  url: http://127.0.0.1:9000/embed
  hosts: [127.0.0.1]
  body:
    input: \"{{inputs}}\"
  vectors: data.*.embedding
  dimensions: 64
";

const STUDIO_PLUGIN: &str = "schema: 1
kind: embedding
name: studio
title: Studio
version: 1
http:
  url: http://localhost:1234/v1/embeddings
  hosts: [localhost]
  body:
    input: \"{{inputs}}\"
  vectors: data.*.embedding
  dimensions: 64
";

fn plugins_folder(case: &Case, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let folder = case.runtime.join("app-plugins");
    fs::create_dir_all(&folder).unwrap();
    for (name, text) in [("near.yaml", NEAR_PLUGIN), ("studio.yaml", STUDIO_PLUGIN)] {
        fs::write(folder.join(name), text).unwrap();
        fs::set_permissions(folder.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
    folder
}

fn open_sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(&case.project, &case.home, &case.tmp, SandboxNetwork::Open).unwrap()
}

#[test]
fn a_tools_on_sandbox_keeps_every_loopback_app_plugin_port_and_fails_closed_on_an_unsafe_folder() {
    let case = case();
    plugins_folder(&case, 0o700);
    let open = open_sandbox(&case).guard_engine(&case.runtime).unwrap();
    let profile = open.profile();
    for rule in ["network-bind", "network-inbound"] {
        let line = profile
            .lines()
            .find(|line| line.starts_with(&format!("(deny {rule}")))
            .unwrap_or_else(|| panic!("no {rule} deny in {profile}"));
        for port in [1234, 9000, 11434] {
            assert!(
                line.contains(&format!("(local ip \"*:{port}\")")),
                "{rule} {port}: {line}"
            );
        }
    }
    plugins_folder(&case, 0o755);
    match open_sandbox(&case).guard_engine(&case.runtime) {
        Err(SandboxError::Invalid(reason)) => {
            assert_eq!(reason, crazyagent::sandbox::APP_PLUGIN_PORTS_UNREADABLE)
        }
        other => panic!("expected the plugin ports refusal, got {other:?}"),
    }
    guarded(&case);
}
