use std::fs;
use std::path::Path;

use crate::settings::{LeftRunning, OnQuit, QuitSettings};

#[test]
fn quit_settings_json_matches_the_engine_body() {
    let settings = QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    };
    let json = serde_json::to_string(&settings).expect("serializes");
    assert_eq!(
        json,
        r#"{"on_quit":"stop","left_running":"never","hours":12}"#
    );
    assert_eq!(
        serde_json::from_str::<QuitSettings>(&json).expect("reads back"),
        settings
    );
    let kept = QuitSettings {
        on_quit: OnQuit::KeepRunning,
        left_running: LeftRunning::AfterHours,
        hours: 24,
    };
    assert_eq!(
        serde_json::to_string(&kept).expect("serializes"),
        r#"{"on_quit":"keep_running","left_running":"after_hours","hours":24}"#
    );
}

#[test]
fn quit_settings_refuses_an_unknown_field() {
    let json = r#"{"on_quit":"stop","left_running":"never","hours":12,"extra":1}"#;
    assert!(serde_json::from_str::<QuitSettings>(json).is_err());
}

#[test]
fn the_save_quit_capability_has_its_declared_facts() {
    assert_eq!(
        crate::settings::SAVE_QUIT,
        crate::Capability {
            name: "settings.save_quit",
            tier: crate::Tier::Kernel,
            needs: "settings.write",
        }
    );
}

/// The std modules that hand a plugin power over the machine.
const AMBIENT: [&str; 4] = ["fs", "net", "process", "env"];

/// The crate roots no SDK file may re-export with `pub use`.
const ROOTS: [&str; 4] = ["std", "core", "alloc", "serde_json"];

/// Splits Rust source into tokens: identifiers (a raw `r#` prefix dropped), `::`, and single
/// punctuation characters. Comments, string, byte-string and char literals are skipped, so
/// a path spelled in a docstring or a literal is never a hit. Input: the source.
/// Output: the tokens in order.
fn rust_tokens(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let before = at;
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        if c.is_whitespace() {
            at += 1;
        } else if c == '/' && next == Some('/') {
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while at < chars.len() {
                if chars[at] == '/' && chars.get(at + 1) == Some(&'*') {
                    depth += 1;
                    at += 2;
                } else if chars[at] == '*' && chars.get(at + 1) == Some(&'/') {
                    depth -= 1;
                    at += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    at += 1;
                }
            }
        } else if let Some(end) = string_end(&chars, at) {
            at = end;
        } else if c == '\'' || (c == 'b' && next == Some('\'')) {
            let quote = if c == 'b' { at + 1 } else { at };
            if chars.get(quote + 1) == Some(&'\\') {
                at = quote + 3;
                while at < chars.len() && chars[at] != '\'' {
                    at += 1;
                }
                at += 1;
            } else if chars.get(quote + 2) == Some(&'\'') {
                at = quote + 3;
            } else {
                at = quote + 1;
            }
        } else if c.is_alphabetic() || c == '_' {
            let mut end = at;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            let word: String = chars[at..end].iter().collect();
            at = end;
            if word == "r" && chars.get(at) == Some(&'#') {
                at += 1;
                continue;
            }
            tokens.push(word);
        } else if c == ':' && next == Some(':') {
            tokens.push("::".to_owned());
            at += 2;
        } else {
            tokens.push(c.to_string());
            at += 1;
        }
        assert!(at > before, "the scan moves on");
    }
    tokens
}

/// Where a string or byte-string literal starting at a position ends: `"..."`, `b"..."`,
/// `r"..."`, `r#"..."#` and `br#"..."#`. Inputs: the source characters and the position.
/// Output: the position just past the literal, or `None` when none starts there.
fn string_end(chars: &[char], at: usize) -> Option<usize> {
    let mut quote = at;
    if chars.get(quote) == Some(&'b') {
        quote += 1;
    }
    let raw = chars.get(quote) == Some(&'r');
    if raw {
        quote += 1;
    }
    let mut hashes = 0;
    while raw && chars.get(quote + hashes) == Some(&'#') {
        hashes += 1;
    }
    if chars.get(quote + hashes) != Some(&'"') {
        return None;
    }
    if !raw && quote != at && chars[at] != 'b' {
        return None;
    }
    let mut end = quote + hashes + 1;
    while end < chars.len() {
        if !raw && chars[end] == '\\' {
            end += 2;
            continue;
        }
        if chars[end] == '"' && (0..hashes).all(|offset| chars.get(end + 1 + offset) == Some(&'#'))
        {
            return Some(end + 1 + hashes);
        }
        end += 1;
    }
    Some(chars.len())
}

/// Finds the first way a Rust source could hand a plugin ambient power: a path through
/// `std::fs`, `std::net`, `std::process` or `std::env` (a brace list, an alias or a glob
/// included), a glob of `std` itself, `std` or `std::{self}` aliased, `extern crate std`
/// or `pub extern crate`, a `#[path]` module, `include!`, or a `pub use` of a crate root.
/// Input: the source. Output: the disguise found, or `None`.
fn ambient_power(source: &str) -> Option<String> {
    let tokens = rust_tokens(source);
    let at = |index: usize| tokens.get(index).map(String::as_str).unwrap_or("");
    for index in 0..tokens.len() {
        let token = at(index);
        if token == "std" && at(index + 1) == "as" {
            return Some("std aliased".to_owned());
        }
        if token == "std" && at(index + 1) == "::" {
            let module = at(index + 2);
            if AMBIENT.contains(&module) {
                return Some(format!("std::{module}"));
            }
            if module == "*" {
                return Some("std::*".to_owned());
            }
            if module == "{" {
                let mut depth = 0usize;
                for inner in index + 2..tokens.len() {
                    match at(inner) {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        word if AMBIENT.contains(&word) || word == "self" || word == "*" => {
                            return Some(format!("std::{{{word}}}"));
                        }
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
        if token == "extern" && at(index + 1) == "crate" {
            if at(index + 2) == "std" {
                return Some("extern crate std".to_owned());
            }
            if index > 0 && at(index - 1) == "pub" {
                return Some("pub extern crate".to_owned());
            }
        }
        if token == "#" {
            let open = if at(index + 1) == "!" {
                index + 2
            } else {
                index + 1
            };
            if at(open) == "[" && at(open + 1) == "path" {
                return Some("#[path]".to_owned());
            }
        }
        if token == "include" && at(index + 1) == "!" {
            return Some("include!".to_owned());
        }
    }
    for index in 0..tokens.len() {
        if at(index) == "use" && index > 0 && at(index - 1) == "pub" {
            let mut root = index + 1;
            while matches!(at(root), "::" | "{") {
                root += 1;
            }
            if ROOTS.contains(&at(root)) {
                return Some(format!("pub use {}", at(root)));
            }
        }
    }
    None
}

/// Lists the SDK's shipped Rust files: every `.rs` under `src`, its subfolders included,
/// except the test rows and the `cfg(test)` fixture trees. Inputs: the folder and its depth.
/// Output: the files grow.
fn shipped_sources(folder: &Path, depth: usize, files: &mut Vec<std::path::PathBuf>) {
    assert!(depth <= 3, "the SDK tree is shallow");
    let mut entries: Vec<_> = fs::read_dir(folder)
        .unwrap_or_else(|error| panic!("{}: {error}", folder.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("{}: {error}", folder.display()))
                .path()
        })
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with("fixture") || name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        if path.is_dir() {
            shipped_sources(&path, depth + 1, files);
        } else if name.ends_with(".rs") {
            files.push(path);
        }
    }
}

#[test]
fn the_sdk_re_exports_no_ambient_power() {
    let mut files = Vec::new();
    shipped_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        0,
        &mut files,
    );
    assert!(files.len() >= 9, "{files:?}");
    assert!(files.iter().any(|file| file.ends_with("src/lib.rs")));
    assert!(
        files
            .iter()
            .any(|file| file.ends_with("generated/envelope/settings.rs"))
    );
    for file in files {
        let text =
            fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        assert_eq!(ambient_power(&text), None, "{}", file.display());
    }
}

#[test]
fn the_ambient_power_scan_catches_each_disguise() {
    for (source, found) in [
        ("pub use std::fs;", "std::fs"),
        ("pub use {std::fs};", "std::fs"),
        ("pub use std::{fs, net};", "std::{fs}"),
        (
            "pub use std::{io::Read, process::Command};",
            "std::{process}",
        ),
        ("pub use std::fs as files;", "std::fs"),
        ("use std::*;\npub use fs::File;", "std::*"),
        ("use std::{io, *};\npub type F = fs::File;", "std::{*}"),
        ("pub use ::std::env;", "std::env"),
        ("use std::fs as inner;\npub use inner::*;", "std::fs"),
        ("use std::{self as s};\npub use s::net::*;", "std::{self}"),
        ("use std as s;\npub use s::net;", "std aliased"),
        ("pub type File = std::fs::File;", "std::fs"),
        (
            "pub fn run() -> std::process::Child { todo!() }",
            "std::process",
        ),
        ("pub use r#std::fs;", "std::fs"),
        ("extern crate std as s;", "extern crate std"),
        ("pub extern crate alloc;", "pub extern crate"),
        ("#[path = \"x.rs\"]\npub mod x;", "#[path]"),
        ("include!(\"x.rs\");", "include!"),
        ("pub use core::fmt;", "pub use core"),
        ("pub use alloc::{string::String};", "pub use alloc"),
        ("pub use serde_json::Value;", "pub use serde_json"),
    ] {
        assert_eq!(ambient_power(source).as_deref(), Some(found), "{source}");
    }
    for clean in [
        "extern crate alloc;\nuse alloc::string::String;",
        "/// std::fs is never re-exported.\npub struct S;",
        "/* std::process */ pub const C: &str = \"std::fs\";",
        "pub const R: &str = r#\"std::env \"quoted\"\"#;",
        "fn f<'de>(c: char) -> bool { c == 'a' || c == '\\'' }",
        "pub use generated::host::Host;",
        "use core::fmt;",
    ] {
        assert_eq!(ambient_power(clean), None, "{clean}");
    }
}

#[test]
fn the_host_and_free_functions_take_no_caller() -> Result<(), Box<dyn std::error::Error>> {
    let generated = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/generated");
    let read = |name: &str| {
        let path = generated.join(name);
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))
    };
    let host = read("host.rs")?;
    assert!(host.contains(
        "    fn settings_save_quit(&self, settings: settings::QuitSettings) -> Result<(), Refusal>;\n"
    ));
    let settings = read("settings.rs")?;
    assert!(settings.contains(
        "pub fn save_quit(host: &dyn Host, settings: QuitSettings) -> Result<(), Refusal> {\n"
    ));
    assert!(!host.contains("caller"));
    Ok(())
}

#[test]
fn the_bounds_have_their_declared_values() {
    assert_eq!(crate::TEXT_BYTES_MAX, 8_192);
    assert_eq!(crate::BYTES_MAX, 4_194_304);
    assert_eq!(crate::LIST_ITEMS_MAX, 64);
}

mod bounds {
    use crate::Refusal;
    use crate::fixture_generated::{
        Envelope, Header, Method, Reply, Request, Secret, check_count_input, check_reveal_output,
        check_send_input, check_send_output, check_wrap_input,
    };

    /// A request whose url holds `url` bytes, with `headers` headers and a body of `body`
    /// bytes. Output: the request.
    fn request(url: usize, headers: usize, body: Option<usize>) -> Request {
        Request {
            method: Method::Get,
            url: "u".repeat(url),
            headers: (0..headers)
                .map(|_| Header {
                    name: "n".into(),
                    value: "v".into(),
                })
                .collect(),
            body: body.map(|len| vec![0; len]),
            timeout_ms: None,
        }
    }

    /// The refusal of an input over its bound.
    fn too_large_input() -> Refusal {
        Refusal::Rejected {
            code: "cap_input_too_large".into(),
        }
    }

    #[test]
    fn a_text_at_its_bound_passes_and_one_byte_more_is_refused() {
        assert_eq!(
            check_send_input(&request(crate::TEXT_BYTES_MAX, 0, None)),
            Ok(())
        );
        assert_eq!(
            check_send_input(&request(crate::TEXT_BYTES_MAX + 1, 0, None)),
            Err(too_large_input())
        );
    }

    #[test]
    fn a_list_at_its_bound_passes_and_one_item_more_is_refused() {
        assert_eq!(
            check_send_input(&request(1, crate::LIST_ITEMS_MAX, None)),
            Ok(())
        );
        assert_eq!(
            check_send_input(&request(1, crate::LIST_ITEMS_MAX + 1, None)),
            Err(too_large_input())
        );
        assert_eq!(check_count_input(&[true; crate::LIST_ITEMS_MAX]), Ok(()));
        assert_eq!(
            check_count_input(&[true; crate::LIST_ITEMS_MAX + 1]),
            Err(too_large_input())
        );
    }

    #[test]
    fn a_text_inside_a_listed_type_is_checked() {
        let mut long = request(1, 1, None);
        long.headers[0].value = "v".repeat(crate::TEXT_BYTES_MAX + 1);
        assert_eq!(check_send_input(&long), Err(too_large_input()));
    }

    #[test]
    fn optional_bytes_at_their_bound_pass_and_one_byte_more_is_refused() {
        assert_eq!(
            check_send_input(&request(1, 0, Some(crate::BYTES_MAX))),
            Ok(())
        );
        assert_eq!(
            check_send_input(&request(1, 0, Some(crate::BYTES_MAX + 1))),
            Err(too_large_input())
        );
    }

    #[test]
    fn an_output_over_its_bound_is_a_failure() {
        let failed = Refusal::Failed {
            code: "cap_output_too_large".into(),
        };
        let at = Reply {
            status: 200,
            body: vec![0; crate::BYTES_MAX],
        };
        assert_eq!(check_send_output(&at), Ok(()));
        let over = Reply {
            status: 200,
            body: vec![0; crate::BYTES_MAX + 1],
        };
        assert_eq!(check_send_output(&over), Err(failed.clone()));
        assert_eq!(check_reveal_output(&None), Ok(()));
        assert_eq!(
            check_reveal_output(&Some("s".repeat(crate::TEXT_BYTES_MAX + 1))),
            Err(failed)
        );
    }

    /// An envelope whose every bounded part sits exactly at its bound.
    fn full_envelope() -> Envelope {
        Envelope {
            reply: Reply {
                status: 200,
                body: vec![0; crate::BYTES_MAX],
            },
            tags: vec!["t".repeat(crate::TEXT_BYTES_MAX); crate::LIST_ITEMS_MAX],
            extra: Some(Header {
                name: "n".repeat(crate::TEXT_BYTES_MAX),
                value: "v".into(),
            }),
            grid: vec![vec!["g".into(); crate::LIST_ITEMS_MAX]; 2],
        }
    }

    #[test]
    fn an_envelope_at_every_bound_passes_and_each_part_one_over_is_refused() {
        assert_eq!(check_wrap_input(&full_envelope()), Ok(()));
        let mut body = full_envelope();
        body.reply.body.push(0);
        let mut tag = full_envelope();
        tag.tags[3].push('t');
        let mut header = full_envelope();
        if let Some(extra) = header.extra.as_mut() {
            extra.name.push('n');
        }
        let mut row = full_envelope();
        row.grid[1].push("g".into());
        let mut cell = full_envelope();
        cell.grid[0][5] = "g".repeat(crate::TEXT_BYTES_MAX + 1);
        for (part, envelope) in [
            ("body", body),
            ("tag", tag),
            ("header", header),
            ("row", row),
            ("cell", cell),
        ] {
            assert_eq!(
                check_wrap_input(&envelope),
                Err(too_large_input()),
                "{part}"
            );
        }
    }

    #[test]
    fn a_handle_keeps_the_kernel_id() {
        let secret = Secret::from_kernel(7);
        assert_eq!(secret.kernel_id(), 7);
        assert!(secret == Secret::from_kernel(7));
        assert!(secret != Secret::from_kernel(8));
    }
}

#[test]
fn a_foreign_handle_and_enum_are_the_owning_files_types() {
    use crate::fixture_two::net::{Request, Route};
    use crate::fixture_two::secrets::{Mode, Secret};
    let secret = Secret::from_kernel(7);
    let request = Request {
        bearer: Some(secret),
        keys: vec![secret, Secret::from_kernel(8)],
        mode: Mode::Read,
    };
    let held: Secret = request.bearer.expect("the bearer is set");
    assert_eq!(held.kernel_id(), 7);
    assert_eq!(request.keys[1].kernel_id(), 8);
    let route = Route {
        mode: Mode::Write,
        port: 443,
    };
    assert_eq!(
        serde_json::to_string(&route).expect("a route serializes"),
        r#"{"mode":"write","port":443}"#
    );
}

mod wire {
    use crate::fixture_wire::envelope::wire::{Blob, StoreInput};
    use crate::fixture_wire::wire::{self as sdk_wire, Mode, Plain};
    use crate::settings::{LeftRunning, OnQuit, QuitSettings};
    use crate::wire::{Bytes, Id, U64};

    /// The shared wire fixture: the `wire.store` input as compact JSON, byte for byte the
    /// same file the TS conformance row reads.
    const SHARED: &str = include_str!("../../ts/test/wire.json");

    /// The `QuitSettings` serde bytes as the SDK wrote them before the envelopes existed.
    const QUIT_BODY: &str = r#"{"on_quit":"stop","left_running":"never","hours":12}"#;

    #[test]
    fn wire_json_decodes_and_reencodes_byte_equal() {
        let input: StoreInput = serde_json::from_str(SHARED).expect("wire.json decodes");
        let (blob, plain, at) = input.into_parts();
        assert_eq!(
            blob,
            sdk_wire::Blob {
                size: u64::MAX,
                data: vec![0, 1, 255],
                parts: vec![0, 9_007_199_254_740_993],
                tail: None,
            }
        );
        assert_eq!(
            plain,
            Plain {
                name: "x".into(),
                mode: Mode::Write,
            }
        );
        assert_eq!(at, 7);
        let again = StoreInput {
            blob: Blob::from(blob),
            plain,
            at: U64(at),
        };
        assert_eq!(
            serde_json::to_string(&again).expect("the envelope encodes"),
            SHARED
        );
    }

    #[test]
    fn wire_refuses_non_canonical_u64_and_base64() {
        for text in [
            r#""01""#,
            "7",
            r#""18446744073709551616""#,
            r#""+7""#,
            r#""""#,
        ] {
            assert!(serde_json::from_str::<U64>(text).is_err(), "{text}");
            assert!(serde_json::from_str::<Id>(text).is_err(), "{text}");
        }
        assert_eq!(
            serde_json::from_str::<U64>(r#""18446744073709551615""#).expect("u64::MAX reads"),
            U64(u64::MAX)
        );
        assert_eq!(
            serde_json::from_str::<U64>(r#""0""#).expect("0 reads"),
            U64(0)
        );
        for text in [
            r#""Zg=""#,
            r#""Zh==""#,
            r#""Zg==Zg==""#,
            r#""Z===""#,
            r#""Zm9v YmFy""#,
            "[102]",
        ] {
            assert!(serde_json::from_str::<Bytes>(text).is_err(), "{text}");
        }
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            let json = alloc::format!("\"{encoded}\"");
            assert_eq!(
                serde_json::from_str::<Bytes>(&json).expect("an RFC 4648 vector reads"),
                Bytes(plain.as_bytes().to_vec())
            );
            assert_eq!(
                serde_json::to_string(&Bytes(plain.as_bytes().to_vec())).expect("encodes"),
                json
            );
        }
        let unknown = SHARED.replacen("\"at\"", "\"plugin\":\"p\",\"at\"", 1);
        assert!(serde_json::from_str::<StoreInput>(&unknown).is_err());
    }

    #[test]
    fn sdk_type_serde_is_unchanged_by_the_wire_module() {
        let settings = QuitSettings {
            on_quit: OnQuit::Stop,
            left_running: LeftRunning::Never,
            hours: 12,
        };
        assert_eq!(
            serde_json::to_string(&settings).expect("serializes"),
            QUIT_BODY
        );
        let envelope = crate::generated::envelope::settings::SaveQuitInput { settings };
        let json = serde_json::to_string(&envelope).expect("the envelope encodes");
        assert_eq!(json, alloc::format!("{{\"settings\":{QUIT_BODY}}}"));
        let read: crate::generated::envelope::settings::SaveQuitInput =
            serde_json::from_str(&json).expect("the envelope decodes");
        assert_eq!(read.into_parts(), (settings,));
        let blob = sdk_wire::Blob {
            size: 1,
            data: vec![2],
            parts: vec![3],
            tail: None,
        };
        assert_eq!(
            serde_json::to_string(&blob).expect("the SDK blob serializes"),
            r#"{"size":1,"data":[2],"parts":[3],"tail":null}"#
        );
        let found = crate::generated::envelope::secrets::FindOutput::from(Some(
            crate::secrets::Secret::from_kernel(9),
        ));
        assert_eq!(
            serde_json::to_string(&found).expect("the output encodes"),
            r#""9""#
        );
        let forget: crate::generated::envelope::secrets::ForgetInput =
            serde_json::from_str(r#"{"secret":"9"}"#).expect("a handle id decodes");
        assert_eq!(forget.into_parts().0.kernel_id(), 9);
        assert_eq!(
            serde_json::to_string(&crate::generated::envelope::secrets::ForgetOutput::from(()))
                .expect("a unit output encodes"),
            "null"
        );
    }
}
