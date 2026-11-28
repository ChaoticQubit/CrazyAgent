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
    let root =
        std::env::temp_dir().join(format!("capgen-permissions-{}-{name}", std::process::id()));
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
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
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
    let flat: String = text.split_whitespace().collect();
    assert!(flat.contains(&format!("pubconstPERMISSIONS:[Permission;{}]", names.len())));
    assert!(flat.contains(
        "name:\"settings.save_quit\",tier:Tier::Kernel,needs:\"settings.write\",emits:&[\"AgentQuitSaved\"],"
    ));
    assert!(flat.contains("pubconstBUNDLED_GRANTS:[BundledGrant;2]"));
    assert!(flat.contains(
        "plugin:\"agent-quit\",tier:Tier::T2,permissions:&[\"settings.write\"],essential:true,"
    ));
}

#[test]
fn bundled_grants_are_sorted_by_plugin_id_whatever_the_folders() {
    let root = plugins(
        "sorted",
        &[
            ("t3", "b-plugin", &manifest("b-plugin", "t3", "[]")),
            (
                "t1",
                "c-plugin",
                &manifest("c-plugin", "t1", "[\"secrets.find\"]"),
            ),
            (
                "t2",
                "a-plugin",
                &manifest("a-plugin", "t2", "[\"settings.write\"]"),
            ),
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
    let flat: String = text.split_whitespace().collect();
    assert!(flat.contains("pubconstBUNDLED_GRANTS:[BundledGrant;3]"));
    assert!(flat.contains("plugin:\"b-plugin\",tier:Tier::T3,permissions:&[],essential:false,"));
    fs::remove_dir_all(&root).expect("the folder is removed");
}

#[test]
fn a_bundled_plugin_outside_its_tier_folder_is_refused() {
    let root = plugins("tier", &[("t1", "p", &manifest("p", "t2", "[]"))]);
    let error = refused(&root);
    assert_eq!(error.code, "capgen_plugin_tier");
    assert!(
        Path::new(&error.file).ends_with("t1/p/plugin.toml"),
        "{error:?}"
    );
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
    assert!(
        Path::new(&error.file).ends_with("t2/p/plugin.toml"),
        "{error:?}"
    );
    assert_eq!(error.detail, "line 2");
    fs::remove_dir_all(&root).expect("the folder is removed");
}

/// Every essential bundled code plugin is marked so in its own `plugin.toml`, and the
/// generated table carries each file's mark: exactly `agent-quit` and `projects`.
#[test]
fn every_essential_plugin_is_marked_in_its_plugin_file() {
    use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
    let essential: Vec<&str> = BUNDLED_GRANTS
        .iter()
        .filter(|grant| grant.essential)
        .map(|grant| grant.plugin)
        .collect();
    assert_eq!(essential, ["agent-quit", "projects"]);
    for grant in &BUNDLED_GRANTS {
        let dir = ["t1", "t2", "t3"]
            .iter()
            .map(|tier| repo("plugins").join(tier).join(grant.plugin))
            .find(|dir| dir.join("plugin.toml").is_file())
            .unwrap_or_else(|| panic!("no plugin folder for {}", grant.plugin));
        let read = kernel_plugin_host::manifest::read(&dir).expect("the bundled manifest reads");
        assert_eq!(read.essential, grant.essential, "{}", grant.plugin);
    }
}

/// A plugin-tier `echo` stem with one capability of the given tier. Input: the tier word.
/// Output: the `.cap` source.
fn echo(tier: &str) -> String {
    format!("capability echo.ping(word: text) -> text\n  tier: {tier}\n  needs: echo.call\n")
}

/// A plugin.toml text that serves capability stems. Inputs: the id, the tier word and the
/// stems list text. Output: the text.
fn serving(plugin: &str, tier: &str, capabilities: &str) -> String {
    format!(
        "{}capabilities = {capabilities}\n",
        manifest(plugin, tier, "[]")
    )
}

/// Runs the permission emitter over in-test `.cap` sources and a plugins folder. Inputs: a
/// name unique to the row, the stems with sources and the manifests. Output: the result.
fn served(
    name: &str,
    sources: &[(&str, &str)],
    manifests: &[(&str, &str, &str)],
) -> Result<Vec<(String, String)>, CapgenError> {
    let files: Vec<(&str, File)> = sources
        .iter()
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
        .collect();
    let root = plugins(&format!("served-{name}"), manifests);
    let result = permissions(&files, &root);
    fs::remove_dir_all(&root).expect("the folder is removed");
    result
}

/// The code a served-stem fault is refused with. Inputs: as [`served`]. Output: the code;
/// panics when the tables generate.
fn served_code(
    name: &str,
    sources: &[(&str, &str)],
    manifests: &[(&str, &str, &str)],
) -> &'static str {
    match served(name, sources, manifests) {
        Ok(files) => panic!("generated, expected a refusal: {files:?}"),
        Err(error) => error.code,
    }
}

#[test]
fn a_served_stem_is_written_into_its_providers_grant() {
    let echo = echo("t1");
    let files = served(
        "serves",
        &[("echo", &echo)],
        &[(
            "t1",
            "echo-plugin",
            &serving("echo-plugin", "t1", "[\"echo\"]"),
        )],
    )
    .expect("the tables generate");
    let flat: String = files[0].1.split_whitespace().collect();
    assert!(
        flat.contains(
            "plugin:\"echo-plugin\",tier:Tier::T1,permissions:&[],essential:false,capabilities:&[\"echo\"],"
        ),
        "{flat}"
    );
}

#[test]
fn capgen_serves_unknown() {
    let echo = echo("t1");
    let code = served_code(
        "unknown",
        &[("echo", &echo)],
        &[("t1", "p", &serving("p", "t1", "[\"echo\", \"ghost\"]"))],
    );
    assert_eq!(code, "capgen_serves_unknown");
}

#[test]
fn capgen_served_twice() {
    let echo = echo("t1");
    let code = served_code(
        "twice",
        &[("echo", &echo)],
        &[
            ("t1", "p", &serving("p", "t1", "[\"echo\"]")),
            ("t1", "q", &serving("q", "t1", "[\"echo\"]")),
        ],
    );
    assert_eq!(code, "capgen_served_twice");
}

#[test]
fn capgen_serves_kernel() {
    let kernel = echo("kernel");
    let code = served_code(
        "kernel",
        &[("echo", &kernel)],
        &[("t1", "p", &serving("p", "t1", "[\"echo\"]"))],
    );
    assert_eq!(code, "capgen_serves_kernel");
}

#[test]
fn capgen_provider_tier() {
    let echo = echo("t1");
    let code = served_code(
        "tier",
        &[("echo", &echo)],
        &[("t2", "p", &serving("p", "t2", "[\"echo\"]"))],
    );
    assert_eq!(code, "capgen_provider_tier");
}

#[test]
fn capgen_unserved() {
    let echo = echo("t1");
    let code = served_code(
        "unserved",
        &[("echo", &echo)],
        &[("t1", "p", &manifest("p", "t1", "[]"))],
    );
    assert_eq!(code, "capgen_unserved");
}

#[test]
fn capgen_served_emits() {
    let echo = "capability echo.ping(word: text) -> text\n  tier: t1\n  needs: echo.call\n  emits: EchoPinged { word: text }\n";
    let code = served_code(
        "emits",
        &[("echo", echo)],
        &[("t1", "p", &serving("p", "t1", "[\"echo\"]"))],
    );
    assert_eq!(code, "capgen_served_emits");
}

/// A stem mixing a kernel and a plugin-tier capability is refused by the Rust checks the
/// permissions mode runs first, whether a plugin claims it or not.
#[test]
fn a_stem_mixing_kernel_and_plugin_tiers_is_refused_claimed_or_not() {
    let mixed = "capability mix.low(word: text) -> text\n  tier: t1\n  needs: mix.call\n\
                 capability mix.core(word: text) -> text\n  tier: kernel\n  needs: mix.call\n";
    assert_eq!(
        served_code(
            "mixed-claimed",
            &[("mix", mixed)],
            &[("t1", "p", &serving("p", "t1", "[\"mix\"]"))],
        ),
        "capgen_stem_mixed_tiers"
    );
    assert_eq!(
        served_code(
            "mixed-unclaimed",
            &[("mix", mixed)],
            &[("t1", "p", &manifest("p", "t1", "[]"))],
        ),
        "capgen_stem_mixed_tiers"
    );
}

/// The `.cap` sources of the served wiring rows: kernel `clock` and the served `echo` and
/// `tick`, as in `capgen/fixture_served`.
const WIRING_CAPS: [(&str, &str); 3] = [
    (
        "clock",
        "capability clock.now() -> u64\n  tier: kernel\n  needs: clock.read\n",
    ),
    (
        "echo",
        "capability echo.ping(n: u32) -> u32\n  tier: t1\n  needs: echo.call\n",
    ),
    (
        "tick",
        "capability tick.count() -> u32\n  tier: t1\n  needs: tick.read\n",
    ),
];

/// A provider `Cargo.toml` naming the package in its `[package]` table. Input: the package
/// name. Output: the manifest text.
fn cargo_manifest(package: &str) -> String {
    format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n")
}

/// Runs the served wiring emitter over [`WIRING_CAPS`] and a plugins folder whose plugin
/// folders each hold the given `Cargo.toml` text. Inputs: a name unique to the row and
/// (plugin id, served stems list text, manifest text) triples, every plugin t1. Output:
/// the result.
fn wiring(
    name: &str,
    plugins_in: &[(&str, &str, String)],
) -> Result<Vec<(String, String)>, CapgenError> {
    let files: Vec<(&str, File)> = WIRING_CAPS
        .iter()
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
        .collect();
    let texts: Vec<String> = plugins_in
        .iter()
        .map(|(plugin, stems, _)| serving(plugin, "t1", stems))
        .collect();
    let manifests: Vec<(&str, &str, &str)> = plugins_in
        .iter()
        .zip(&texts)
        .map(|((plugin, _, _), text)| ("t1", *plugin, text.as_str()))
        .collect();
    let root = plugins(&format!("wiring-{name}"), &manifests);
    for (plugin, _, text) in plugins_in {
        fs::write(root.join("t1").join(plugin).join("Cargo.toml"), text)
            .expect("the Cargo.toml writes");
    }
    let result = served_wiring(&files, &root);
    fs::remove_dir_all(&root).expect("the folder is removed");
    result
}

#[test]
fn a_fixture_plugins_folder_writes_one_field_per_served_stem_from_its_provider_crate() {
    let files = wiring(
        "fields",
        &[
            (
                "echo-plugin",
                "[\"echo\"]",
                cargo_manifest("crazyagent-plugin-echo-plugin"),
            ),
            (
                "tick-plugin",
                "[\"tick\"]",
                cargo_manifest("crazyagent-plugin-tick-plugin"),
            ),
        ],
    )
    .expect("the wiring generates");
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["served.rs"]
    );
    let flat: String = files[0].1.split_whitespace().collect();
    assert!(
        flat.contains(
            "pubfnserved()->sdk_host::generated::Served<'static>{sdk_host::generated::Served{echo:&crazyagent_plugin_echo_plugin::ECHO,tick:&crazyagent_plugin_tick_plugin::TICK,}}"
        ),
        "{flat}"
    );
    assert_eq!(flat.matches(":&crazyagent_plugin_").count(), 2);
}

#[test]
fn capgen_provider_crate() {
    let line = "name = \"crazyagent-plugin-echo-plugin\"";
    for text in [
        cargo_manifest("crazyagent-plugin-other"),
        cargo_manifest("echo-plugin"),
        cargo_manifest("crazyagent-plugin-echo-plugin-extra"),
        format!("[package]\nname = \"other\"\n[dependencies]\n{line}\n"),
        format!("[package]\nname = \"other\"\n[lib]\n{line}\n"),
        format!("{line}\n[package]\nname = \"other\"\n"),
    ] {
        let error = wiring(
            "crate",
            &[
                ("echo-plugin", "[\"echo\"]", text.clone()),
                (
                    "tick-plugin",
                    "[\"tick\"]",
                    cargo_manifest("crazyagent-plugin-tick-plugin"),
                ),
            ],
        )
        .expect_err("a provider crate named unlike its plugin is refused");
        assert_eq!(error.code, "capgen_provider_crate", "{text}");
        assert!(
            Path::new(&error.file).ends_with("t1/echo-plugin/Cargo.toml"),
            "{error:?}"
        );
    }
}

/// A provider whose `plugin.toml` id breaks the plugin id grammar is refused by the
/// manifest reader before the served wiring splices the id into a Rust path, so no
/// `served.rs` is written.
#[test]
fn capgen_served_refuses_a_bad_plugin_id() {
    let error = wiring(
        "bad-id",
        &[(
            "Echo_Plugin",
            "[\"echo\"]",
            cargo_manifest("crazyagent-plugin-Echo_Plugin"),
        )],
    )
    .expect_err("a plugin id outside the grammar is refused");
    assert_eq!(error.code, "plugin_id_invalid", "{error:?}");
    assert!(
        Path::new(&error.file).ends_with("t1/Echo_Plugin/plugin.toml"),
        "{error:?}"
    );
}

/// The engine's served wiring equals what capgen writes from the prod caps and plugins:
/// with no served stem no file is written and the folder is absent or empty; with one, the
/// folder holds exactly the generated `served.rs`.
#[test]
fn the_engine_served_wiring_equals_capgen_output() {
    let loaded = caps();
    let files = served_wiring(&borrowed(&loaded), &repo("plugins")).expect("the wiring generates");
    let folder = repo("crates/crazyagent/src/generated_served");
    if files.is_empty() {
        assert!(
            !folder.exists() || drift(&folder, &files).is_empty(),
            "{}",
            folder.display()
        );
    } else {
        assert_eq!(
            files
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["served.rs"]
        );
        assert_eq!(drift(&folder, &files), Vec::<String>::new());
    }
}

/// Every stem a bundled grant serves has exactly one field in the generated wiring, taken
/// from the crate of the plugin the grant names, and no field comes from anywhere else.
#[test]
fn every_bundled_served_stem_has_one_field_from_its_provider_crate() {
    use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
    let loaded = caps();
    let files = served_wiring(&borrowed(&loaded), &repo("plugins")).expect("the wiring generates");
    let flat: String = files
        .iter()
        .map(|(_, text)| text.split_whitespace().collect::<String>())
        .collect();
    let mut stems = 0;
    for grant in &BUNDLED_GRANTS {
        let krate = format!("crazyagent_plugin_{}", grant.plugin.replace('-', "_"));
        for stem in grant.capabilities {
            let field = format!("{stem}:&{krate}::{},", stem.to_ascii_uppercase());
            assert_eq!(flat.matches(&field).count(), 1, "{field}");
            stems += 1;
        }
    }
    assert_eq!(flat.matches(":&crazyagent_plugin_").count(), stems);
}

/// Every bundled plugin that serves a stem is the engine's dependency under its own crate
/// name, by one inline line pointing at its plugin folder with no `package =` rename, so
/// the ident the wiring names resolves to that folder's code and no other crate.
#[test]
fn every_engine_plugin_dependency_points_at_its_provider_folder() {
    use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
    let engine = fs::read_to_string(repo("crates/crazyagent/Cargo.toml")).expect("reads");
    let mut providers = 0;
    for grant in BUNDLED_GRANTS
        .iter()
        .filter(|grant| !grant.capabilities.is_empty())
    {
        let name = format!("crazyagent-plugin-{}", grant.plugin);
        let tier = format!("{:?}", grant.tier).to_ascii_lowercase();
        let want = format!(
            "{name} = {{ path = \"../../plugins/{tier}/{}\" }}",
            grant.plugin
        );
        let named: Vec<&str> = engine
            .lines()
            .map(str::trim)
            .filter(|line| line.contains(&name))
            .collect();
        assert_eq!(named, [want.as_str()], "{name}");
        providers += 1;
    }
    let served = BUNDLED_GRANTS
        .iter()
        .filter(|grant| !grant.capabilities.is_empty())
        .count();
    assert_eq!(providers, served);
}
