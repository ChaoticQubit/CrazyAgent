use std::path::{Path, PathBuf};

use crazyagent_cap::File;

use crate::{CapgenError, load, rust, ts, wit};

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

/// The text of one generated file. Inputs: the files and the path. Output: the text;
/// panics when the path is missing.
fn text<'f>(files: &'f [(String, String)], path: &str) -> &'f str {
    files
        .iter()
        .find(|(name, _)| name == path)
        .map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("{path} is generated"))
}

/// Runs every target over sources each must refuse. Input: stems with sources.
/// Output: the Rust, TS and WIT refusals, in that order; panics when a target accepts.
fn refused_by_all(sources: &[(&str, &str)]) -> [CapgenError; 3] {
    let files = parsed(sources);
    [
        rust(&files).expect_err("rust refuses"),
        ts(&files).expect_err("ts refuses"),
        wit(&files).expect_err("wit refuses"),
    ]
}

/// Writes the TS of one capgen fixture folder. Input: the folder below `capgen`.
/// Output: the generated files.
fn fixture_ts(folder: &str) -> Vec<(String, String)> {
    let loaded = load(&sdk(&format!("capgen/{folder}"))).expect("the fixture loads");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    ts(&borrowed).expect("the fixture generates")
}

#[test]
fn ts_settings_event_text_is_exact() {
    let files = ts(&parsed(&[("settings", crate::ts_tests::SETTINGS)])).expect("generates");
    let settings = text(&files, "settings.ts");
    assert!(
        settings.ends_with(crate::ts_tests::SETTINGS_EVENTS),
        "{settings}"
    );
    assert!(
        settings
            .contains("\nimport type { Capability, Host, Result, Wire } from \"../core.ts\";\n"),
        "{settings}"
    );
    assert_eq!(settings.matches("export type AgentQuitSaved =").count(), 1);
}

#[test]
fn ts_a_mapped_event_gets_a_codec() {
    let files = fixture_ts("fixture_wire");
    let wire = text(&files, "wire.ts");
    for held in [
        "export type BlobStored = { readonly at: bigint; readonly data: Uint8Array; readonly blob: Blob };\n",
        "export function decodeBlobStored(value: Wire): BlobStored {\n",
        "    throw new TypeError(\"sdk_wire_blob_stored\");\n",
        "    at: decodeU64(record.at),\n",
        "    data: decodeBytes(record.data),\n",
        "    blob: decodeBlob(record.blob),\n",
        "export type WireEvent = { readonly kind: \"BlobStored\"; readonly body: BlobStored };\n",
        "export const WIRE_EVENT_KINDS = [\"BlobStored\"] as const;\n",
        "export function decodeWireEvent(kind: string, body: Wire): WireEvent | null {\n",
        "      return { kind: \"BlobStored\", body: decodeBlobStored(body) };\n",
    ] {
        assert!(wire.contains(held), "{held:?} in {wire}");
    }
    let modes = text(&files, "modes.ts");
    assert!(!modes.contains("Event"), "{modes}");
}

#[test]
fn wit_an_event_is_a_record_in_the_types_interface() {
    let files = wit(&parsed(&[("settings", crate::wit_tests::SETTINGS)])).expect("generates");
    let sdk_wit = text(&files, "sdk.wit");
    let types = sdk_wit
        .split("interface settings-types {\n")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("settings-types is written");
    assert!(
        types.ends_with("  record agent-quit-saved {\n    settings: quit-settings,\n  }"),
        "{types}"
    );
    let functions = sdk_wit
        .split("interface settings {\n")
        .nth(1)
        .expect("settings is written");
    assert!(!functions.contains("agent-quit-saved"), "{functions}");
    let only_events = wit(&parsed(&[(
        "net",
        "capability net.send(port: u16) -> unit tier: t1 needs: net.send emits: NetSent { port: u16 }",
    )]))
    .expect("generates");
    let only_events = text(&only_events, "sdk.wit");
    assert!(
        only_events
            .contains("interface net-types {\n  record net-sent {\n    port: u16,\n  }\n}\n"),
        "{only_events}"
    );
}

#[test]
fn an_event_named_like_a_type_is_refused() {
    for (item, name) in [
        ("type Pair { a: u16 }", "Pair"),
        ("enum Pair { a }", "Pair"),
        ("handle Pair", "Pair"),
    ] {
        let source = format!(
            "{item}\ncapability x.go() -> unit tier: t1 needs: x.y emits: {name} {{ b: u16 }}"
        );
        for refusal in refused_by_all(&[("x", &source)]) {
            assert_eq!(refusal.code, "capgen_name_collision", "{item}: {refusal:?}");
            assert_eq!(refusal.file, "x.cap", "{item}");
            assert_eq!(refusal.detail, name, "{item}");
        }
    }
    let union = ts(&parsed(&[(
        "x",
        "capability x.go() -> unit tier: t1 needs: x.y emits: XEvent { b: u16 }",
    )]))
    .expect_err("ts refuses a kind named like the stem's event union");
    assert_eq!(union.code, "capgen_name_collision");
    assert_eq!(union.detail, "XEvent");
    let reserved = ts(&parsed(&[(
        "x",
        "capability x.go() -> unit tier: t1 needs: x.y emits: Promise { b: u16 }",
    )]))
    .expect_err("ts refuses a reserved kind");
    assert_eq!(reserved.code, "capgen_reserved_name");
    assert_eq!(reserved.detail, "ts: Promise");
}

#[test]
fn a_kind_emitted_twice_with_other_fields_is_refused() {
    let one_capability =
        "capability x.go() -> unit tier: t1 needs: x.y emits: E { a: u16 } emits: E { a: u16 }";
    assert_eq!(
        crazyagent_cap::parse_as("x", one_capability)
            .unwrap_err()
            .code,
        "cap_duplicate_name"
    );
    let other = "capability x.go() -> unit tier: t1 needs: x.y emits: E { a: u16 }\n\
                 capability x.run() -> unit tier: t1 needs: x.y emits: E { a: u32 }";
    for refusal in refused_by_all(&[("x", other)]) {
        assert_eq!(refusal.code, "capgen_event_shape", "{refusal:?}");
        assert_eq!(refusal.detail, "E");
    }
    let same = "capability x.go() -> unit tier: t1 needs: x.y emits: E { a: u64 }\n\
                capability x.run() -> unit tier: t1 needs: x.y emits: E { a: u64 }";
    let files = parsed(&[("x", same)]);
    let typescript = ts(&files).expect("one shape twice is one type");
    let x = text(&typescript, "x.ts");
    assert_eq!(x.matches("export type E =").count(), 1, "{x}");
    assert_eq!(x.matches("export function decodeE(").count(), 1, "{x}");
    assert_eq!(x.matches("    case \"E\":").count(), 1, "{x}");
    let rust_files = rust(&files).expect("rust generates");
    let envelope = text(&rust_files, "envelope/x.rs");
    assert_eq!(
        envelope.matches("pub struct EStored").count(),
        1,
        "{envelope}"
    );
    let wit_files = wit(&files).expect("wit generates");
    assert_eq!(text(&wit_files, "sdk.wit").matches("record e {").count(), 1);
}

#[test]
fn a_kind_emitted_by_two_stems_is_refused() {
    for (a, b) in [
        ("E { a: u16 }", "E { a: u16 }"),
        ("E { a: u16 }", "E { b: text }"),
    ] {
        let net = format!("capability net.go() -> unit tier: t1 needs: net.y emits: {a}");
        let disk = format!("capability disk.go() -> unit tier: t1 needs: disk.y emits: {b}");
        for refusal in refused_by_all(&[("net", &net), ("disk", &disk)]) {
            assert_eq!(refusal.code, "capgen_event_shape", "{refusal:?}");
            assert_eq!(refusal.detail, "E: disk, net");
        }
    }
    let loaded: Vec<(String, File)> = [
        "caps",
        "capgen/fixture",
        "capgen/fixture_two",
        "capgen/fixture_wire",
    ]
    .iter()
    .flat_map(|folder| load(&sdk(folder)).expect("the folder loads"))
    .collect();
    let mut kinds: Vec<&str> = loaded
        .iter()
        .flat_map(|(_, file)| file.capabilities.iter())
        .flat_map(|capability| capability.emits.iter().map(|event| event.kind.as_str()))
        .collect();
    let count = kinds.len();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), count, "{kinds:?}");
}
