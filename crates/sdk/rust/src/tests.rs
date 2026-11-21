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
            tier: crate::Tier::T1,
            needs: "settings.write",
        }
    );
}

#[test]
fn the_sdk_re_exports_no_ambient_power() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("lib.rs")];
    for entry in fs::read_dir(src.join("generated")).expect("generated reads") {
        files.push(entry.expect("entry reads").path());
    }
    assert!(files.len() >= 4);
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
