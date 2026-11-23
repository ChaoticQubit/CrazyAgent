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
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
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
        vec![
            "envelope/mod.rs",
            "envelope/secrets.rs",
            "envelope/settings.rs",
            "host.rs",
            "mod.rs",
            "secrets.rs",
            "settings.rs"
        ]
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
    let secrets = fs::read_to_string(sdk("caps/secrets.cap")).expect("secrets.cap reads");
    let files = rust(&parsed(&[("secrets", &secrets), ("settings", &renamed)])).expect("generates");
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
    let file = crazyagent_cap::parse_as("fixture", &source).expect("fixture.cap parses");
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
fn bound_checks_are_written_for_every_capability_in_both_directions() {
    let module = fixture_module();
    for function in ["send", "reveal", "pair", "count", "wrap", "shadow"] {
        for direction in ["input", "output"] {
            let head = format!("pub fn check_{function}_{direction}(");
            assert_eq!(module.matches(&head).count(), 1, "{head}");
        }
    }
    for unbounded in [
        "pub fn check_reveal_input(_secret: &Secret) -> Result<(), Refusal> {\n    Ok(())\n}\n",
        "pub fn check_pair_output(_output: &()) -> Result<(), Refusal> {\n    Ok(())\n}\n",
        "pub fn check_count_output(_output: &u32) -> Result<(), Refusal> {\n    Ok(())\n}\n",
    ] {
        assert!(module.contains(unbounded), "{unbounded}");
    }
    assert!(
        module.contains(
            "pub fn check_pair_input(_pair: &Pair, keyed: &Keyed) -> Result<(), Refusal> {\n"
        ),
        "{module}"
    );
    let settings = generated_from_caps()
        .into_iter()
        .find(|(path, _)| path == "settings.rs")
        .expect("settings.rs")
        .1;
    assert_eq!(settings.matches("pub fn check_").count(), 2, "{settings}");
    assert!(!settings.contains("alloc::"), "{settings}");
}

#[test]
fn settings_changes_only_by_the_caller_the_tier_and_its_checks() {
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
    assert!(settings.contains(
        "pub fn check_save_quit_input(_settings: &QuitSettings) -> Result<(), Refusal> {\n    Ok(())\n}\n"
    ));
    assert!(settings.contains(
        "pub fn check_save_quit_output(_output: &()) -> Result<(), Refusal> {\n    Ok(())\n}\n"
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
    let module =
        crate::rust::test_module("s", &crazyagent_cap::parse_as("s", source).expect("parses"))
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
    assert_eq!(
        refused(&[(
            "s",
            "capability s.a(x: u16) -> unit tier: t1 needs: s.w\ncapability s.check_a_output() -> unit tier: t1 needs: s.w"
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
        let file = crazyagent_cap::parse_as("s", &source).expect("parses");
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
    assert!(
        plain.contains("pub fn check_a_input(_t: &T40) -> Result<(), Refusal> {\n    Ok(())\n}\n"),
        "{plain}"
    );
    let bounded = generated_within(chain("text"), 30);
    assert!(
        !derive_of(&bounded, "pub struct T40").contains("Copy"),
        "{bounded}"
    );
    assert_eq!(bounded.matches("fn fits_t40(").count(), 1, "{bounded}");
    assert_eq!(bounded.matches("fn fits_t0(").count(), 1, "{bounded}");
    assert!(bounded.contains("fn check_a_input("), "{bounded}");
}

/// Generates the SDK for sources that must generate. Input: stems with sources.
/// Output: the generated files; panics on a refusal.
fn generated(sources: &[(&str, &str)]) -> Vec<(String, String)> {
    rust(&parsed(sources)).expect("generates")
}

/// The text of one generated file. Inputs: the files and the path. Output: the text.
/// Text with every run of whitespace made one space, so a check reads the same before and
/// after rustfmt wraps a line. Input: the text. Output: the flattened text.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn file_text<'f>(files: &'f [(String, String)], path: &str) -> &'f str {
    &files
        .iter()
        .find(|(name, _)| name == path)
        .expect("the file is generated")
        .1
}

#[test]
fn an_unknown_foreign_stem_or_name_is_refused() {
    assert_eq!(
        refused(&[("net", "type A { s: vault.Secret }")]),
        "capgen_unknown_type"
    );
    assert_eq!(
        refused(&[
            ("net", "type A { s: option<secrets.Missing> }"),
            ("secrets", "handle Secret"),
        ]),
        "capgen_unknown_type"
    );
}

#[test]
fn a_foreign_ref_to_a_type_is_refused() {
    assert_eq!(
        refused(&[
            ("net", "type A { k: list<secrets.Key> }"),
            ("secrets", "type Key { id: u16 }"),
        ]),
        "capgen_foreign_not_leaf"
    );
}

#[test]
fn a_foreign_handle_in_an_event_is_refused() {
    let secrets = ("secrets", "handle Secret\nenum Mode { read, write }");
    assert_eq!(
        refused(&[
            (
                "net",
                "capability net.a() -> unit tier: t1 needs: net.w emits: E { s: secrets.Secret }"
            ),
            secrets,
        ]),
        "capgen_handle_in_event"
    );
    assert_eq!(
        refused(&[
            (
                "net",
                "type W { s: option<secrets.Secret> }\ncapability net.a() -> unit tier: t1 needs: net.w emits: E { w: list<W> }"
            ),
            secrets,
        ]),
        "capgen_handle_in_event"
    );
    generated(&[
        (
            "net",
            "capability net.a() -> unit tier: t1 needs: net.w emits: E { m: secrets.Mode }",
        ),
        secrets,
    ]);
}

#[test]
fn a_type_holding_a_foreign_handle_derives_no_debug_or_serde() {
    let files = generated(&[
        (
            "net",
            "type Held { s: option<secrets.Secret> }\ntype Plain { m: secrets.Mode, n: u16 }",
        ),
        ("secrets", "handle Secret\nenum Mode { read, write }"),
    ]);
    let net = file_text(&files, "net.rs");
    assert_eq!(
        derive_of(net, "pub struct Held"),
        "#[derive(Clone, Copy, PartialEq, Eq)]"
    );
    assert_eq!(
        derive_of(net, "pub struct Plain"),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]"
    );
}

#[test]
fn a_test_module_refuses_a_foreign_ref() {
    let file = crazyagent_cap::parse_as("net", "type A { s: secrets.Secret }").expect("parses");
    assert_eq!(
        rust_test_module("net", &file).unwrap_err().code,
        "capgen_unknown_type"
    );
}

#[test]
fn a_two_file_fixture_shares_one_handle_type() {
    let loaded = load(&sdk("capgen/fixture_two")).expect("the fixture loads");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    let files = rust(&borrowed).expect("the fixture generates");
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec!["host.rs", "mod.rs", "net.rs", "secrets.rs"]
    );
    assert_eq!(
        drift(&sdk("rust/src/fixture_two"), &files),
        Vec::<String>::new()
    );
    let net = file_text(&files, "net.rs");
    assert!(
        net.contains("pub bearer: core::option::Option<super::secrets::Secret>,"),
        "{net}"
    );
    assert!(
        net.contains("pub keys: alloc::vec::Vec<super::secrets::Secret>,"),
        "{net}"
    );
    assert!(!net.contains("pub struct Secret"), "{net}");
    let host = generated(&[
        (
            "net",
            "capability net.send(s: secrets.Secret) -> option<secrets.Secret> tier: t1 needs: net.send",
        ),
        ("secrets", "handle Secret"),
    ]);
    let host = file_text(&host, "host.rs");
    assert!(host.contains("s: super::secrets::Secret"), "{host}");
    assert!(
        host.contains("Result<core::option::Option<super::secrets::Secret>, Refusal>;"),
        "{host}"
    );
}

#[test]
fn a_self_qualified_ref_parsed_without_its_stem_is_refused() {
    let file = crazyagent_cap::parse("handle S\ntype A { s: net.S }").expect("parses");
    assert_eq!(
        rust(&[("net", file)]).unwrap_err().code,
        "cap_self_qualified"
    );
}

#[test]
fn a_call_taking_a_foreign_handle_writes_its_checks_and_traits() {
    let files = generated(&[
        (
            "net",
            "type Ring { keys: list<secrets.Secret> }\ncapability net.send(keys: list<secrets.Secret>, ring: Ring, s: secrets.Secret) -> unit tier: t1 needs: net.send",
        ),
        ("secrets", "handle Secret"),
    ]);
    let net = file_text(&files, "net.rs");
    let flat_net = flat(net);
    assert!(
        flat_net.contains(&flat("keys: &[super::secrets::Secret], ring: &Ring")),
        "{net}"
    );
    assert!(
        flat_net.contains(&flat(
            "keys.len() <= crate::LIST_ITEMS_MAX && self::fits_ring(ring)"
        )),
        "{net}"
    );
    assert!(
        flat_net.contains(&flat(
            "fn fits_ring(value: &Ring) -> bool {\n    value.keys.len() <= crate::LIST_ITEMS_MAX\n}"
        )),
        "{net}"
    );
    assert!(
        flat_net.contains(&flat(
            "s: super::secrets::Secret,\n    ) -> Result<(), Refusal>;"
        )),
        "{net}"
    );
    assert_eq!(
        derive_of(net, "pub struct Ring"),
        "#[derive(Clone, PartialEq, Eq)]"
    );
}
