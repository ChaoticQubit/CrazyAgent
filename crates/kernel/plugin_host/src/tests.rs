use std::path::PathBuf;

use crate::Refused;
use crate::manifest::{self, MANIFEST_BYTES_MAX, Manifest, PLUGIN_ID_MAX};

/// Writes one `plugin.toml` into a fresh folder of its own. Inputs: a name unique per row
/// and the text. Output: the folder.
fn plugin_dir(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("plugin-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the folder is made");
    std::fs::write(dir.join("plugin.toml"), text).expect("plugin.toml is written");
    dir
}

/// A manifest text with the five keys and one id. Input: the id. Output: the text.
fn manifest_text(id: &str) -> String {
    format!(
        "plugin = \"{id}\"\ntier = \"t2\"\nkind = \"agent-quit\"\nversion = 1\npermissions = [\"settings.write\"]\n"
    )
}

#[test]
fn a_valid_manifest_reads_its_five_keys() {
    let dir = plugin_dir("valid", &manifest_text("agent-quit"));
    assert_eq!(
        manifest::read(&dir),
        Ok(Manifest {
            plugin: "agent-quit".to_owned(),
            tier: "t2".to_owned(),
            kind: "agent-quit".to_owned(),
            version: 1,
            permissions: vec!["settings.write".to_owned()],
        })
    );
}

#[test]
fn an_unknown_key_is_refused_with_its_line() {
    let text = format!("{}extra = true\n", manifest_text("agent-quit"));
    let dir = plugin_dir("unknown-key", &text);
    assert_eq!(
        manifest::read(&dir),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 6,
        })
    );
}

#[test]
fn a_manifest_over_its_byte_bound_is_refused() {
    let text = manifest_text("agent-quit");
    let fits = format!("{text}{}", " ".repeat(MANIFEST_BYTES_MAX - text.len()));
    assert!(manifest::read(&plugin_dir("bound-fits", &fits)).is_ok());
    let over = format!("{fits} ");
    assert_eq!(
        manifest::read(&plugin_dir("bound-over", &over)),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 1,
        })
    );
    assert_eq!(
        manifest::read(&std::env::temp_dir().join("plugin-host-no-such-folder")),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 1,
        })
    );
}

#[test]
fn a_plugin_id_outside_the_folder_rule_is_refused() {
    let longest = "a".repeat(PLUGIN_ID_MAX);
    assert!(manifest::read(&plugin_dir("id-longest", &manifest_text(&longest))).is_ok());
    for (row, id) in [
        ("id-upper", "Agent"),
        ("id-under", "agent_quit"),
        ("id-dot", "agent.quit"),
        ("id-long", &"a".repeat(PLUGIN_ID_MAX + 1)),
    ] {
        assert_eq!(
            manifest::read(&plugin_dir(row, &manifest_text(id))),
            Err(Refused {
                code: "plugin_id_invalid",
                line: 1,
            }),
            "{id}"
        );
    }
}

#[test]
fn plugin_host_depends_on_no_sdk_or_engine_crate() {
    let cargo: toml::Table = include_str!("../Cargo.toml")
        .parse()
        .expect("Cargo.toml parses");
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let names: Vec<&str> = cargo
            .get(section)
            .and_then(toml::Value::as_table)
            .map(|table| table.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let allowed: &[&str] = if section == "dependencies" {
            &["toml"]
        } else {
            &[]
        };
        assert_eq!(names, allowed, "{section}");
    }
}
