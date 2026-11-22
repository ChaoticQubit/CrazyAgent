use std::path::PathBuf;

use super::*;

/// Names a folder of the sdk tree. Input: its path below `crates/sdk`. Output: the folder.
fn sdk(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

/// Parses `.cap` sources named by their stems. Input: stems with sources.
/// Output: the stems with their parsed files; panics on a grammar refusal.
fn parsed<'s>(sources: &[(&'s str, &str)]) -> Vec<(&'s str, File)> {
    sources
        .iter()
        .map(|(stem, source)| (*stem, crazyagent_cap::parse(source).expect("parses")))
        .collect()
}

/// Generates the SDK from the checked-in `.cap` files. Output: the generated files.
fn generated_from_caps() -> Vec<(String, String)> {
    let loaded = load(&sdk("caps")).expect("the caps load");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    rust(&borrowed).expect("the caps generate")
}

/// Runs capgen over sources that must be refused. Input: stems with sources.
/// Output: the refusal code; panics when capgen accepts them.
fn refused(sources: &[(&str, &str)]) -> &'static str {
    match rust(&parsed(sources)) {
        Ok(files) => panic!("generated, expected a refusal: {files:?}"),
        Err(error) => error.code,
    }
}

#[test]
fn settings_cap_generates_the_checked_in_files() {
    let files = generated_from_caps();
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec!["host.rs", "mod.rs", "settings.rs"]
    );
    assert_eq!(
        drift(&sdk("rust/src/generated"), &files),
        Vec::<String>::new()
    );
}

#[test]
fn generation_is_deterministic() {
    assert_eq!(generated_from_caps(), generated_from_caps());
}

#[test]
fn a_renamed_field_changes_the_output() {
    let source = fs::read_to_string(sdk("caps/settings.cap")).expect("settings.cap reads");
    let renamed = source.replace("hours: u16", "hour_count: u16");
    assert_ne!(renamed, source);
    let files = rust(&parsed(&[("settings", &renamed)])).expect("generates");
    assert_eq!(
        drift(&sdk("rust/src/generated"), &files),
        vec!["settings.rs".to_owned()]
    );
}

#[test]
fn a_rust_reserved_name_is_refused() {
    for source in [
        "type A { type: u16 }",
        "type Self { a: u16 }",
        "type Option { a: u16 }",
        "type Host { a: u16 }",
        "enum Mode { gen }",
        "enum Result { a }",
        "type Serialize { a: u16 }",
        "capability s.match() -> unit tier: t1 needs: s.w",
        "capability s.a(async: u16) -> unit tier: t1 needs: s.w",
    ] {
        assert_eq!(
            refused(&[("s", source)]),
            "capgen_reserved_name",
            "{source}"
        );
    }
    assert_eq!(
        refused(&[("crate", "type A { a: u16 }")]),
        "capgen_reserved_name"
    );
    assert_eq!(
        refused(&[("host", "type A { a: u16 }")]),
        "capgen_reserved_name"
    );
}

#[test]
fn two_capabilities_mapping_to_one_method_are_refused() {
    assert_eq!(
        refused(&[
            ("a", "capability a.b_c() -> unit tier: t1 needs: a.w"),
            ("a_b", "capability a_b.c() -> unit tier: t1 needs: a.w"),
        ]),
        "capgen_name_collision"
    );
}

#[test]
fn a_capability_outside_its_file_is_refused() {
    assert_eq!(
        refused(&[(
            "settings",
            "capability other.save() -> unit tier: t1 needs: a.w"
        )]),
        "capgen_name_outside_file"
    );
}

#[test]
fn mod_rs_lists_every_capability_once_in_file_order() {
    let files = rust(&parsed(&[
        (
            "beta",
            "capability beta.two() -> unit tier: t2 needs: beta.w\ncapability beta.three() -> unit tier: t1 needs: beta.w",
        ),
        ("alpha", "capability alpha.one() -> unit tier: t1 needs: alpha.r"),
    ]))
    .expect("generates");
    let module = &files
        .iter()
        .find(|(path, _)| path == "mod.rs")
        .expect("mod.rs")
        .1;
    assert!(
        module.contains(
            "pub const CAPABILITIES: [crate::Capability; 3] = [alpha::ONE, beta::TWO, beta::THREE];\n"
        ),
        "{module}"
    );
}

/// Generates `mod.rs` from one `.cap` file and returns its `CAPABILITIES` item, from the
/// const line to the end. Inputs: the stem and the source. Output: the item text.
fn capabilities_item(stem: &str, source: &str) -> String {
    let files = rust(&parsed(&[(stem, source)])).expect("generates");
    let module = &files
        .iter()
        .find(|(path, _)| path == "mod.rs")
        .expect("mod.rs")
        .1;
    let start = module.find("pub const CAPABILITIES").expect("the list");
    assert!(!module.contains("rustfmt::skip"), "{module}");
    module[start..].to_owned()
}

#[test]
fn the_capability_list_takes_the_layout_rustfmt_gives_it() {
    assert_eq!(
        capabilities_item(
            "settings",
            "capability settings.save_quit() -> unit tier: t1 needs: settings.write\n\
             capability settings.save_tabs() -> unit tier: t1 needs: settings.write\n\
             capability settings.save_home() -> unit tier: t1 needs: settings.write",
        ),
        "pub const CAPABILITIES: [crate::Capability; 3] = [\n    settings::SAVE_QUIT,\n    settings::SAVE_TABS,\n    settings::SAVE_HOME,\n];\n"
    );
    assert_eq!(
        capabilities_item(
            "processes",
            "capability processes.spawn_and_wait_for() -> unit tier: t1 needs: processes.run\n\
             capability processes.output_of_command() -> unit tier: t1 needs: processes.run",
        ),
        "pub const CAPABILITIES: [crate::Capability; 2] =\n    [processes::SPAWN_AND_WAIT_FOR, processes::OUTPUT_OF_COMMAND];\n"
    );
}
