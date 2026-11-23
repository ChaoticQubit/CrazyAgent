use std::path::PathBuf;

use crate::Refused;
use crate::manifest::{self, Bounded, MANIFEST_BYTES_MAX, Manifest, PLUGIN_ID_MAX, read_bounded};

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
        "plugin = \"{id}\"\ntier = \"t2\"\nkind = \"setting\"\nversion = 1\npermissions = [\"settings.write\"]\n"
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
            kind: "setting".to_owned(),
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
        ("id-nul", "nul"),
        ("id-com1", "com1"),
        ("id-dash", "-x"),
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

/// A FIFO named `plugin.toml` is refused at once, never waited on.
#[cfg(unix)]
#[test]
fn a_fifo_plugin_toml_is_refused_without_waiting() {
    let dir = plugin_dir("fifo", "");
    let file = dir.join("plugin.toml");
    std::fs::remove_file(&file).expect("the file is removed");
    let made = std::process::Command::new("mkfifo")
        .arg(&file)
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "the fifo is made");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_bounded(&file, MANIFEST_BYTES_MAX));
    });
    let read = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the read returns before the deadline instead of waiting on the fifo");
    assert_eq!(read, Bounded::Unreadable);
}

/// A `plugin.toml` that is a link is refused, even to a valid file.
#[cfg(unix)]
#[test]
fn a_linked_plugin_toml_is_refused() {
    let target = plugin_dir("link-target", &manifest_text("agent-quit"));
    let dir = plugin_dir("linked", "");
    let file = dir.join("plugin.toml");
    std::fs::remove_file(&file).expect("the file is removed");
    std::os::unix::fs::symlink(target.join("plugin.toml"), &file).expect("the link is made");
    assert_eq!(read_bounded(&file, MANIFEST_BYTES_MAX), Bounded::Unreadable);
    assert_eq!(
        manifest::read(&dir),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 1,
        })
    );
}

/// The guarded open reads a regular file byte for byte.
#[test]
fn a_regular_plugin_toml_reads_byte_equal_through_the_guarded_open() {
    let text = manifest_text("agent-quit");
    let dir = plugin_dir("regular", &text);
    assert_eq!(
        read_bounded(&dir.join("plugin.toml"), MANIFEST_BYTES_MAX),
        Bounded::Text(text)
    );
}

/// On Windows a `plugin.toml` that is a reparse point (a symbolic link) is
/// refused.
#[cfg(windows)]
#[test]
fn a_reparse_point_plugin_toml_is_refused() {
    let target = plugin_dir("reparse-target", &manifest_text("agent-quit"));
    let dir = plugin_dir("reparse", "");
    let file = dir.join("plugin.toml");
    std::fs::remove_file(&file).expect("the file is removed");
    if std::os::windows::fs::symlink_file(target.join("plugin.toml"), &file).is_err() {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink"])
            .arg(&file)
            .arg(target.join("plugin.toml"))
            .status()
            .expect("mklink runs");
        assert!(status.success(), "the reparse point is made");
    }
    assert_eq!(read_bounded(&file, MANIFEST_BYTES_MAX), Bounded::Unreadable);
}

#[test]
fn a_kind_or_permission_outside_the_character_rule_is_refused() {
    let base = manifest_text("agent-quit");
    let kinds = [
        "\\u001b[31m",
        "set\\nting",
        "agent-quit",
        "",
        "Setting",
        "9setting",
    ];
    for (index, kind) in kinds.iter().enumerate() {
        let text = base.replace("kind = \"setting\"", &format!("kind = \"{kind}\""));
        assert_eq!(
            manifest::read(&plugin_dir(&format!("kind-{index}"), &text)),
            Err(Refused {
                code: "plugin_manifest_invalid",
                line: 3,
            }),
            "{kind}"
        );
    }
    let long_kind = base.replace(
        "kind = \"setting\"",
        &format!("kind = \"{}\"", "k".repeat(65)),
    );
    assert_eq!(
        manifest::read(&plugin_dir("kind-long", &long_kind)).map_err(|refused| refused.line),
        Err(3)
    );
    let permissions = [
        "\"\"",
        "\"settings\"",
        "\"settings.\\u001b\"",
        "\"settings.wri\\nte\"",
        "\"settings-x.write\"",
        "\"Settings.write\"",
        "\"settings.write\", \"settings.write\"",
    ];
    for (index, list) in permissions.iter().enumerate() {
        let text = base.replace("[\"settings.write\"]", &format!("[{list}]"));
        assert_eq!(
            manifest::read(&plugin_dir(&format!("permission-{index}"), &text)),
            Err(Refused {
                code: "plugin_manifest_invalid",
                line: 5,
            }),
            "{list}"
        );
    }
    let long = format!("a.{}", "b".repeat(63));
    let text = base.replace("[\"settings.write\"]", &format!("[\"{long}\"]"));
    assert_eq!(
        manifest::read(&plugin_dir("permission-long", &text)).map_err(|refused| refused.line),
        Err(5)
    );
    let fits = format!("a.{}", "b".repeat(62));
    let text = base.replace(
        "[\"settings.write\"]",
        &format!("[\"{fits}\", \"net.http.get\"]"),
    );
    assert!(manifest::read(&plugin_dir("permission-fits", &text)).is_ok());
}

/// The dependencies of every dependency table in a `Cargo.toml` text, the `target.*`
/// tables included: the table, the entry's key and the crate behind it, which is the
/// `package` a renamed entry names, else the key (a `workspace = true` entry names its root
/// entry by the key). Input: the text. Output: (table, key, crate), in table then key order.
fn dependency_names(text: &str) -> Vec<(String, String, String)> {
    const SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let cargo: toml::Table = text.parse().expect("Cargo.toml parses");
    let mut tables: Vec<(String, &toml::Table)> = vec![(String::new(), &cargo)];
    if let Some(targets) = cargo.get("target").and_then(toml::Value::as_table) {
        for (cfg, table) in targets {
            if let Some(table) = table.as_table() {
                tables.push((format!("target.{cfg}."), table));
            }
        }
    }
    let mut names = Vec::new();
    for (prefix, table) in tables {
        for section in SECTIONS {
            if let Some(deps) = table.get(section).and_then(toml::Value::as_table) {
                for (key, entry) in deps {
                    let package = entry
                        .get("package")
                        .map(|package| package.as_str().unwrap_or("").to_owned());
                    names.push((
                        format!("{prefix}{section}"),
                        key.clone(),
                        package.unwrap_or_else(|| key.clone()),
                    ));
                }
            }
        }
    }
    names
}

/// The crates plugin_host may depend on, each in the one table it may sit in, sorted: no
/// SDK crate and no engine crate. rustix is the unix no-follow open of manifests and assets;
/// tempfile is for test rows.
const ALLOWED: [(&str, &str); 9] = [
    ("dependencies", "crazyagent-kernel-common"),
    ("dependencies", "crazyagent-kernel-security"),
    ("dependencies", "rusqlite"),
    ("dependencies", "serde"),
    ("dependencies", "serde_json"),
    ("dependencies", "toml"),
    ("dependencies", "url"),
    ("dev-dependencies", "tempfile"),
    ("target.cfg(unix).dependencies", "rustix"),
];

/// The entries of a `Cargo.toml` text whose crate is not on [`ALLOWED`] in its table,
/// or whose key names another crate than the one behind it. Input: the text. Output: the
/// (table, key, crate) of each.
fn outside_allowlist(text: &str) -> Vec<(String, String, String)> {
    dependency_names(text)
        .into_iter()
        .filter(|(table, key, krate)| {
            key != krate || !ALLOWED.contains(&(table.as_str(), krate.as_str()))
        })
        .collect()
}

#[test]
fn plugin_host_depends_on_no_sdk_or_engine_crate() {
    let text = include_str!("../Cargo.toml");
    let outside = outside_allowlist(text);
    assert!(outside.is_empty(), "{outside:?}");
    let mut direct: Vec<(String, String)> = dependency_names(text)
        .into_iter()
        .map(|(table, _, krate)| (table, krate))
        .collect();
    direct.sort_unstable();
    let mut allowed: Vec<(String, String)> = ALLOWED
        .iter()
        .map(|(table, krate)| ((*table).to_owned(), (*krate).to_owned()))
        .collect();
    allowed.sort_unstable();
    assert_eq!(direct, allowed);
}

#[test]
fn the_allowlist_walks_target_tables() {
    let text = "[dependencies]\ntoml = \"1\"\n\n[target.'cfg(unix)'.dependencies]\ncrazyagent-sdk = { path = \"x\" }\n\n[target.'cfg(windows)'.dev-dependencies]\ncrazyagent = { path = \"y\" }\n";
    let pair = |table: &str, name: &str| (table.to_owned(), name.to_owned(), name.to_owned());
    assert_eq!(
        dependency_names(text),
        vec![
            pair("dependencies", "toml"),
            pair("target.cfg(unix).dependencies", "crazyagent-sdk"),
            pair("target.cfg(windows).dev-dependencies", "crazyagent"),
        ]
    );
    assert_eq!(outside_allowlist(text).len(), 2);
}

/// A renamed entry is judged by the crate behind it, so an allowed key
/// cannot carry an SDK or engine crate, and a `workspace = true` entry by its root name.
#[test]
fn the_allowlist_judges_the_crate_behind_a_renamed_entry() {
    let renamed = "[dependencies]\nserde = { package = \"crazyagent-sdk\", path = \"x\" }\n";
    assert_eq!(
        outside_allowlist(renamed),
        vec![(
            "dependencies".to_owned(),
            "serde".to_owned(),
            "crazyagent-sdk".to_owned()
        )]
    );
    let workspace =
        "[dependencies]\ntoml = { workspace = true }\ncrazyagent = { workspace = true }\n";
    assert_eq!(
        outside_allowlist(workspace),
        vec![(
            "dependencies".to_owned(),
            "crazyagent".to_owned(),
            "crazyagent".to_owned()
        )]
    );
    let odd = "[dependencies]\nserde = { package = 1 }\n";
    assert_eq!(outside_allowlist(odd).len(), 1);
}
