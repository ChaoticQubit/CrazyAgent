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

#[test]
fn generation_fails_closed_without_rustfmt() {
    let missing = std::ffi::OsStr::new("/nonexistent/capgen-test/rustfmt");
    let refused = crate::rustfmt_with(missing, "mod.rs", "pub mod host;\n").unwrap_err();
    assert_eq!(refused.code, "capgen_rustfmt_missing");
    assert_eq!(refused.file, "mod.rs");
    assert!(
        refused.detail.contains("rustup component add rustfmt"),
        "{}",
        refused.detail
    );
}

/// Generates the test module from the fixture `.cap`. Output: its text.
fn fixture_module() -> String {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixture/fixture.cap"))
            .expect("fixture.cap reads");
    let file = crazyagent_cap::parse(&source).expect("fixture.cap parses");
    rust_test_module("fixture", &file).expect("fixture.cap generates")
}

/// Returns the derive line written above one generated item. Inputs: the module text and
/// the item's head, such as `pub struct Pair`. Output: the derive line.
fn derive_of<'t>(module: &'t str, head: &str) -> &'t str {
    let at = module
        .find(&format!("{head} {{"))
        .expect("the item is generated");
    module[..at]
        .lines()
        .rev()
        .find(|line| line.starts_with("#[derive("))
        .expect("the item has a derive line")
}

#[test]
fn the_fixture_generates_the_checked_in_test_module() {
    let checked_in =
        fs::read_to_string(sdk("rust/src/fixture_generated.rs")).expect("the test module reads");
    assert_eq!(fixture_module(), checked_in);
}

#[test]
fn derives_follow_the_fields() {
    let module = fixture_module();
    assert_eq!(
        derive_of(&module, "pub struct Header"),
        "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]"
    );
    assert_eq!(
        derive_of(&module, "pub struct Pair"),
        "#[derive(Clone, Copy, PartialEq, Eq)]"
    );
    assert_eq!(
        derive_of(&module, "pub struct Keyed"),
        "#[derive(Clone, PartialEq, Eq)]"
    );
    assert_eq!(
        derive_of(&module, "pub struct Secret"),
        "#[derive(Clone, Copy, PartialEq, Eq, Hash)]"
    );
    assert_eq!(
        derive_of(&module, "pub enum Method"),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]"
    );
    for (head, denies) in [
        ("pub struct Header", true),
        ("pub struct Pair", false),
        ("pub struct Keyed", false),
    ] {
        let at = module
            .find(&format!("{head} {{"))
            .expect("the item is generated");
        let block = module[..at]
            .rsplit("\n\n")
            .next()
            .expect("the item's block");
        assert_eq!(block.contains("deny_unknown_fields"), denies, "{head}");
    }
}

#[test]
fn wrapped_types_use_alloc_and_core_paths() {
    let module = fixture_module();
    for line in [
        "    pub headers: alloc::vec::Vec<Header>,\n",
        "    pub body: core::option::Option<alloc::vec::Vec<u8>>,\n",
        "    pub timeout_ms: core::option::Option<u32>,\n",
    ] {
        assert!(module.contains(line), "{line}");
    }
}

#[test]
fn bound_checks_are_written_only_where_a_field_is_bounded() {
    let module = fixture_module();
    for present in [
        "pub fn check_send_input(",
        "pub fn check_send_output(",
        "pub fn check_reveal_output(",
        "pub fn check_pair_input(",
        "pub fn check_count_input(",
    ] {
        assert!(module.contains(present), "{present}");
    }
    for absent in [
        "check_reveal_input",
        "check_pair_output",
        "check_count_output",
    ] {
        assert!(!module.contains(absent), "{absent}");
    }
    let settings = generated_from_caps()
        .into_iter()
        .find(|(path, _)| path == "settings.rs")
        .expect("settings.rs")
        .1;
    assert!(!settings.contains("check_"), "{settings}");
    assert!(!settings.contains("alloc::"), "{settings}");
}

#[test]
fn settings_changes_only_by_the_caller_and_the_tier() {
    let settings = generated_from_caps()
        .into_iter()
        .find(|(path, _)| path == "settings.rs")
        .expect("settings.rs")
        .1;
    assert!(settings.contains(
        "    fn save_quit(&self, caller: &'static str, settings: QuitSettings) -> Result<(), Refusal>;\n"
    ));
    assert!(settings.contains("    tier: Tier::Kernel,\n"));
    assert!(settings.contains(
        "pub fn save_quit(host: &dyn Host, settings: QuitSettings) -> Result<(), Refusal> {\n    host.settings_save_quit(settings)\n}\n"
    ));
    assert!(settings.contains(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(deny_unknown_fields)]\npub struct QuitSettings {"
    ));
}

#[test]
fn caller_is_a_reserved_input_name() {
    assert_eq!(
        refused(&[(
            "s",
            "capability s.a(caller: u16) -> unit tier: t1 needs: s.w"
        )]),
        "capgen_reserved_name"
    );
}

#[test]
fn a_bounded_type_named_twice_is_written_once() {
    let source = "type Zeta { t: text }\ntype Alpha { zeta: Zeta, more: list<Zeta> }\n\
                  capability s.a(alpha: Alpha) -> Zeta tier: t1 needs: s.w";
    let module = crate::rust::test_module("s", &crazyagent_cap::parse(source).expect("parses"))
        .expect("generates");
    assert_eq!(module.matches("fn fits_zeta(").count(), 1, "{module}");
    assert_eq!(module.matches("fn fits_alpha(").count(), 1, "{module}");
}

#[test]
fn a_capability_named_like_a_generated_check_is_refused() {
    assert_eq!(
        refused(&[(
            "s",
            "type R { b: bytes }\ncapability s.fits_r() -> R tier: t1 needs: s.w"
        )]),
        "capgen_name_collision"
    );
    assert_eq!(
        refused(&[(
            "s",
            "capability s.a(x: text) -> unit tier: t1 needs: s.w\ncapability s.check_a_input() -> unit tier: t1 needs: s.w"
        )]),
        "capgen_name_collision"
    );
}

fn chain(leaf: &str) -> String {
    let mut source = format!("type T0 {{ a: {leaf} }}\n");
    for level in 1..=40 {
        let below = level - 1;
        source.push_str(&format!("type T{level} {{ a: T{below}, b: T{below} }}\n"));
    }
    source.push_str("capability s.a(t: T40) -> unit tier: t1 needs: s.w");
    source
}

fn generated_within(source: String, seconds: u64) -> String {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let file = crazyagent_cap::parse(&source).expect("parses");
        let module = crate::rust::test_module("s", &file).expect("generates");
        let _ = sender.send(module);
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(seconds))
        .expect("a 41-type chain generates in linear work, not 2^40")
}

#[test]
fn a_deep_chain_of_shared_types_generates_without_blowing_up() {
    let plain = generated_within(chain("u16"), 30);
    assert!(
        derive_of(&plain, "pub struct T40").contains("Copy"),
        "{plain}"
    );
    assert!(
        derive_of(&plain, "pub struct T40").contains("Debug"),
        "{plain}"
    );
    assert!(!plain.contains("fn fits_t40("), "{plain}");
    assert!(!plain.contains("fn check_a_input("), "{plain}");
    let bounded = generated_within(chain("text"), 30);
    assert!(
        !derive_of(&bounded, "pub struct T40").contains("Copy"),
        "{bounded}"
    );
    assert_eq!(bounded.matches("fn fits_t40(").count(), 1, "{bounded}");
    assert_eq!(bounded.matches("fn fits_t0(").count(), 1, "{bounded}");
    assert!(bounded.contains("fn check_a_input("), "{bounded}");
}
