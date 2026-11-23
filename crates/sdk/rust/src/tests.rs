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

#[test]
fn the_sdk_re_exports_no_ambient_power() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("lib.rs")];
    for entry in fs::read_dir(src.join("generated")).expect("generated reads") {
        let path = entry.expect("entry reads").path();
        if !path.is_dir() {
            files.push(path);
            continue;
        }
        for inner in fs::read_dir(&path).expect("a generated subfolder reads") {
            files.push(inner.expect("entry reads").path());
        }
    }
    assert!(files.len() >= 7);
    for file in files {
        let text = fs::read_to_string(&file).expect("source reads");
        for banned in [
            "pub use std",
            "pub use core",
            "pub use alloc",
            "pub use serde_json",
            "pub use ::std",
            "pub use ::core",
            "pub use ::alloc",
            "pub extern crate",
        ] {
            assert!(!text.contains(banned), "{} has {banned}", file.display());
        }
    }
}

#[test]
fn the_host_and_free_functions_take_no_caller() {
    let generated = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/generated");
    let host = fs::read_to_string(generated.join("host.rs")).expect("host.rs reads");
    assert!(host.contains(
        "    fn settings_save_quit(&self, settings: settings::QuitSettings) -> Result<(), Refusal>;\n"
    ));
    let settings = fs::read_to_string(generated.join("settings.rs")).expect("settings.rs reads");
    assert!(settings.contains(
        "pub fn save_quit(host: &dyn Host, settings: QuitSettings) -> Result<(), Refusal> {\n"
    ));
    assert!(!host.contains("caller"));
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
