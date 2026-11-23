use std::path::PathBuf;

use super::*;

/// Names a folder of the repository from capgen's own folder. Input: its path from the
/// repository root. Output: the folder.
fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(path)
}

/// Loads the checked-in `.cap` files. Output: each stem with its parsed file.
fn caps() -> Vec<(String, File)> {
    load(&repo("crates/sdk/caps")).expect("the caps load")
}

/// Borrows loaded files as the emitters take them. Input: the loaded files. Output: the
/// stems borrowed with cloned files.
fn borrowed(loaded: &[(String, File)]) -> Vec<(&str, File)> {
    loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect()
}

/// A fresh plugins folder under the system temp folder holding the given manifests, plus a
/// stray file at each level. Inputs: a name unique to the row and (tier folder, plugin
/// folder, plugin.toml text) triples. Output: the folder.
fn plugins(name: &str, manifests: &[(&str, &str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "capgen-permissions-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the plugins folder is made");
    fs::write(root.join(".DS_Store"), "x").expect("the stray file writes");
    for (tier, folder, text) in manifests {
        let dir = root.join(tier).join(folder);
        fs::create_dir_all(&dir).expect("the plugin folder is made");
        fs::write(root.join(tier).join(".DS_Store"), "x").expect("the stray file writes");
        fs::write(dir.join("plugin.toml"), text).expect("the manifest writes");
    }
    root
}

/// A plugin.toml text. Inputs: the id, the tier word and the permissions list text.
/// Output: the text.
fn manifest(plugin: &str, tier: &str, permissions: &str) -> String {
    format!(
        "plugin = \"{plugin}\"\ntier = \"{tier}\"\nkind = \"setting\"\nversion = 1\npermissions = {permissions}\n"
    )
}

/// Runs the permission emitter over the checked-in caps and a plugins folder that must be
/// refused. Input: the folder. Output: the refusal.
fn refused(plugins: &Path) -> CapgenError {
    let loaded = caps();
    match permissions(&borrowed(&loaded), plugins) {
        Ok(files) => panic!("generated, expected a refusal: {files:?}"),
        Err(error) => error,
    }
}

#[test]
fn permissions_rs_equals_capgen_output() {
    let loaded = caps();
    let files = permissions(&borrowed(&loaded), &repo("plugins")).expect("the tables generate");
    assert_eq!(
        files.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(),
        ["permissions.rs"]
    );
    assert_eq!(
        drift(&repo("crates/kernel/plugin_host/src/generated"), &files),
        Vec::<String>::new()
    );
}

#[test]
fn the_permission_table_lists_every_capability() {
    let loaded = caps();
    let files = permissions(&borrowed(&loaded), &repo("plugins")).expect("the tables generate");
    let text = &files[0].1;
    let mut names: Vec<&str> = loaded
        .iter()
        .flat_map(|(_, file)| file.capabilities.iter().map(|c| c.name.as_str()))
        .collect();
    names.sort_unstable();
    let listed: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("name: \""))
        .map(|rest| rest.trim_end_matches("\","))
        .collect();
    assert_eq!(listed, names);
    assert!(text.contains(&format!("pub const PERMISSIONS: [Permission; {}]", names.len())));
    assert!(text.contains("name: \"settings.save_quit\",\n        tier: Tier::Kernel,\n        needs: \"settings.write\",\n        emits: &[\"AgentQuitSaved\"],"));
    assert!(text.contains("pub const BUNDLED_GRANTS: [BundledGrant; 1]"));
    assert!(text.contains("plugin: \"agent-quit\",\n        tier: Tier::T2,\n        permissions: &[\"settings.write\"],"));
}

#[test]
fn bundled_grants_are_sorted_by_plugin_id_whatever_the_folders() {
    let root = plugins(
        "sorted",
        &[
            ("t3", "b-plugin", &manifest("b-plugin", "t3", "[]")),
            ("t1", "c-plugin", &manifest("c-plugin", "t1", "[\"secrets.find\"]")),
            ("t2", "a-plugin", &manifest("a-plugin", "t2", "[\"settings.write\"]")),
        ],
    );
    let loaded = caps();
    let files = permissions(&borrowed(&loaded), &root).expect("the tables generate");
    let text = &files[0].1;
    let ids: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("plugin: \""))
        .map(|rest| rest.trim_end_matches("\","))
        .collect();
    assert_eq!(ids, ["a-plugin", "b-plugin", "c-plugin"]);
    assert!(text.contains("pub const BUNDLED_GRANTS: [BundledGrant; 3]"));
    assert!(text.contains("tier: Tier::T3,\n        permissions: &[],"));
    fs::remove_dir_all(&root).expect("the folder is removed");
}

#[test]
fn a_bundled_plugin_outside_its_tier_folder_is_refused() {
    let root = plugins("tier", &[("t1", "p", &manifest("p", "t2", "[]"))]);
    let error = refused(&root);
    assert_eq!(error.code, "capgen_plugin_tier");
    assert!(error.file.ends_with("t1/p/plugin.toml"), "{error:?}");
    fs::remove_dir_all(&root).expect("the folder is removed");
}

#[test]
fn a_bundled_plugin_named_unlike_its_folder_is_refused() {
    let root = plugins("folder", &[("t2", "p", &manifest("q", "t2", "[]"))]);
    assert_eq!(refused(&root).code, "capgen_plugin_folder");
    fs::remove_dir_all(&root).expect("the folder is removed");
}

#[test]
fn a_folder_outside_the_tier_folders_is_refused() {
    let root = plugins("kernel", &[("kernel", "p", &manifest("p", "t2", "[]"))]);
    assert_eq!(refused(&root).code, "capgen_plugin_tier");
    fs::remove_dir_all(&root).expect("the folder is removed");
}

#[test]
fn a_manifest_refusal_names_its_file_and_line() {
    let root = plugins("invalid", &[("t2", "p", &manifest("p", "t4", "[]"))]);
    let error = refused(&root);
    assert_eq!(error.code, "plugin_manifest_invalid");
    assert!(error.file.ends_with("t2/p/plugin.toml"), "{error:?}");
    assert_eq!(error.detail, "line 2");
    fs::remove_dir_all(&root).expect("the folder is removed");
}
