use std::fs;
use std::path::{Path, PathBuf};

use crazyagent_cap::File;

use crate::{CapgenError, drift, load, rust};

/// Names a folder of the sdk tree. Input: its path below `crates/sdk`. Output: the folder.
fn sdk(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

/// Generates the Rust SDK of `.cap` sources named by their stems. Input: stems with
/// sources. Output: the files, or the refusal; panics on a grammar refusal.
fn rust_of(sources: &[(&str, &str)]) -> Result<Vec<(String, String)>, CapgenError> {
    let parsed: Vec<(&str, File)> = sources
        .iter()
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
        .collect();
    rust(&parsed)
}

/// Generates the Rust SDK of one checked-in caps folder. Input: its path below
/// `crates/sdk`. Output: the files.
fn rust_of_folder(path: &str) -> Vec<(String, String)> {
    let loaded = load(&sdk(path)).expect("the caps load");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    rust(&borrowed).expect("the caps generate")
}

/// Makes a fresh folder under the system temp folder for one row.
/// Input: a name unique to the row. Output: the empty folder.
fn scratch(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("capgen-wire-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(folder.join("envelope")).expect("the scratch folder is made");
    folder
}

#[test]
fn drift_covers_the_envelope_subfolder() {
    let folder = scratch("drift");
    fs::write(folder.join("mod.rs"), "a").expect("writes");
    fs::write(folder.join("envelope/kept.rs"), "b").expect("writes");
    fs::write(folder.join("envelope/changed.rs"), "old").expect("writes");
    fs::write(folder.join("envelope/extra.rs"), "x").expect("writes");
    let files = vec![
        ("mod.rs".to_owned(), "a".to_owned()),
        ("envelope/changed.rs".to_owned(), "new".to_owned()),
        ("envelope/kept.rs".to_owned(), "b".to_owned()),
        ("envelope/missing.rs".to_owned(), "m".to_owned()),
    ];
    assert_eq!(
        drift(&folder, &files),
        vec![
            "envelope/changed.rs".to_owned(),
            "envelope/extra.rs".to_owned(),
            "envelope/missing.rs".to_owned(),
        ]
    );
    let flat = vec![("mod.rs".to_owned(), "a".to_owned())];
    assert_eq!(
        drift(&folder, &flat),
        vec![
            "envelope/changed.rs".to_owned(),
            "envelope/extra.rs".to_owned(),
            "envelope/kept.rs".to_owned(),
        ]
    );
    fs::remove_dir_all(&folder).expect("the scratch folder is removed");
}

#[test]
fn a_handle_is_still_refused_in_an_event() {
    let local = "handle S\ncapability net.a() -> unit tier: t1 needs: net.w emits: E { s: S }";
    assert_eq!(
        crazyagent_cap::parse_as("net", local).unwrap_err().code,
        "cap_handle_in_event"
    );
    let foreign = rust_of(&[
        (
            "net",
            "capability net.a() -> unit tier: t1 needs: net.w emits: E { s: secrets.Secret }",
        ),
        ("secrets", "handle Secret"),
    ]);
    assert_eq!(foreign.unwrap_err().code, "capgen_handle_in_event");
}

#[test]
fn wire_inputs_carry_no_caller_field() {
    for name in ["plugin", "caller", "host"] {
        let source = format!("capability net.send({name}: text) -> unit tier: t1 needs: net.send");
        let refusal = rust_of(&[("net", &source)]).unwrap_err();
        assert_eq!(refusal.code, "capgen_reserved_name", "{name}");
        assert_eq!(refusal.detail, name);
    }
    rust_of(&[(
        "net",
        "type T { plugin: text }\ncapability net.send(t: T) -> unit tier: t1 needs: net.send",
    )])
    .expect("a type field may be named plugin");
    for folder in ["caps", "capgen/fixture_wire"] {
        let files = rust_of_folder(folder);
        let envelopes: Vec<&(String, String)> = files
            .iter()
            .filter(|(path, _)| path.starts_with("envelope/"))
            .collect();
        assert!(envelopes.len() >= 2, "{folder}");
        for (path, text) in envelopes {
            for field in ["pub plugin:", "pub caller:", "pub host:"] {
                assert!(!text.contains(field), "{folder} {path} has {field}");
            }
        }
    }
}

#[test]
fn the_stem_envelope_is_refused() {
    let refusal = rust_of(&[("envelope", "enum M { a }")]).unwrap_err();
    assert_eq!(refusal.code, "capgen_reserved_name");
    assert_eq!(refusal.detail, "envelope");
}

#[test]
fn fixture_wire_generates_the_checked_in_rust() {
    let files = rust_of_folder("capgen/fixture_wire");
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "envelope/mod.rs",
            "envelope/wire.rs",
            "host.rs",
            "mod.rs",
            "modes.rs",
            "wire.rs"
        ]
    );
    assert_eq!(
        drift(&sdk("rust/src/fixture_wire"), &files),
        Vec::<String>::new()
    );
    let envelope = &files[1].1;
    assert!(
        envelope.contains("pub values: alloc::vec::Vec<core::option::Option<crate::wire::U64>>,"),
        "{envelope}"
    );
    assert!(envelope.contains("pub struct StoreInput {"), "{envelope}");
    assert!(envelope.contains("pub struct Blob {"), "{envelope}");
    assert!(!envelope.contains("pub struct Plain {"), "{envelope}");
}

#[test]
fn a_record_holding_a_handle_gets_a_mirror_rebuilt_through_from_kernel() {
    let files = rust_of(&[
        (
            "net",
            "type Ring { keys: list<secrets.Secret>, port: u16 }\ncapability net.send(ring: Ring) -> option<secrets.Secret> tier: t1 needs: net.send",
        ),
        ("secrets", "handle Secret"),
    ])
    .expect("generates");
    assert!(!files.iter().any(|(path, _)| path == "envelope/secrets.rs"));
    let (_, net) = files
        .iter()
        .find(|(path, _)| path == "envelope/net.rs")
        .expect("net has an envelope");
    for fragment in [
        "pub keys: alloc::vec::Vec<crate::wire::Id>,",
        "pub port: u16,",
        ".map(|item0| crate::wire::Id(item0.kernel_id()))",
        ".map(|item0| super::super::secrets::Secret::from_kernel(item0.0))",
        "pub struct SendOutput(pub core::option::Option<crate::wire::Id>);",
    ] {
        assert!(net.contains(fragment), "{fragment}\n{net}");
    }
}

/// The `.cap` source of the row that names a type `From`: a mirrored record, an enum and a
/// capability that takes and returns the record, so every conversion kind is written.
const FROM_CAP: &str = "type From { at: u64, data: bytes, items: list<u64> }\n\
                                   capability conv.round(value: From, more: option<From>) -> From \
                                   tier: t1 needs: conv.use\n\
                                   capability conv.many(values: list<From>) -> list<From> \
                                   tier: t1 needs: conv.use \
                                   emits: FromSaved { value: From, at: u64 }";

/// Finds each `::from(` call in generated text that does not name `core::convert::From`.
/// Input: the text. Output: the lines holding a bare call or a bare `impl From<`.
fn bare_from(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            let bare_call = line.match_indices("::from(").any(|(at, _)| {
                let before = &line[..at];
                !before.ends_with(">") && !before.ends_with("core::convert::From")
            });
            bare_call || line.contains("impl From<")
        })
        .collect()
}

#[test]
fn a_cap_type_named_from_still_compiles() {
    let files = rust_of(&[("conv", FROM_CAP)]).expect("a type named From generates");
    let envelope = files
        .iter()
        .find(|(path, _)| path == "envelope/conv.rs")
        .expect("the envelope is written");
    assert!(envelope.1.contains("pub struct From {"), "{}", envelope.1);
    assert!(
        envelope.1.contains("pub struct FromSavedWire {"),
        "{}",
        envelope.1
    );
    for (path, text) in &files {
        assert_eq!(bare_from(text), Vec::<&str>::new(), "{path}");
    }
    for (path, text) in rust_of_folder("caps")
        .iter()
        .chain(rust_of_folder("capgen/fixture_wire").iter())
    {
        assert_eq!(bare_from(text), Vec::<&str>::new(), "{path}");
    }
    assert_eq!(
        bare_from("impl From<A> for B {}\nlet s = alloc::string::String::from(x);\n").len(),
        2
    );
    assert!(
        bare_from("<T as core::convert::From<_>>::from(x)\ncore::convert::From::from(x)\n")
            .is_empty()
    );
}
