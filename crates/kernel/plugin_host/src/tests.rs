use std::path::PathBuf;

use crate::Refused;
use crate::manifest::{
    self, Bounded, CAPABILITIES_MAX, MANIFEST_BYTES_MAX, Manifest, PLUGIN_ID_MAX, read_bounded,
};

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
            net: None,
            essential: false,
            capabilities: Vec::new(),
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
/// `package` a renamed entry names, else the key. A `workspace = true` entry is read from
/// the root's `[workspace.dependencies]` entry of the same key, and an empty crate stands
/// for a key the root does not hold. Inputs: the member text and the root text. Output:
/// (table, key, crate), in table then key order.
fn dependency_names(text: &str, root: &str) -> Vec<(String, String, String)> {
    const SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let cargo: toml::Table = text.parse().expect("Cargo.toml parses");
    let root: toml::Table = root.parse().expect("root Cargo.toml parses");
    let inherited = root
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table);
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
                    let from_root =
                        entry.get("workspace").and_then(toml::Value::as_bool) == Some(true);
                    let entry = if from_root {
                        inherited.and_then(|table| table.get(key))
                    } else {
                        Some(entry)
                    };
                    let krate = match entry {
                        None => String::new(),
                        Some(entry) => entry.get("package").map_or_else(
                            || key.clone(),
                            |package| package.as_str().unwrap_or("").to_owned(),
                        ),
                    };
                    names.push((format!("{prefix}{section}"), key.clone(), krate));
                }
            }
        }
    }
    names
}

/// The crates plugin_host may depend on, each in the one table it may sit in, sorted: no
/// SDK crate and no engine crate. rustix is the unix no-follow open of manifests and assets;
/// crazyagent-win-sandbox is the Windows private open of the plugins folder; tempfile is for
/// test rows.
const ALLOWED: [(&str, &str); 10] = [
    ("dependencies", "crazyagent-kernel-common"),
    ("dependencies", "crazyagent-kernel-security"),
    ("dependencies", "rusqlite"),
    ("dependencies", "serde"),
    ("dependencies", "serde_json"),
    ("dependencies", "toml"),
    ("dependencies", "url"),
    ("dev-dependencies", "tempfile"),
    ("target.cfg(unix).dependencies", "rustix"),
    ("target.cfg(windows).dependencies", "crazyagent-win-sandbox"),
];

/// The entries of a `Cargo.toml` text whose crate is not on [`ALLOWED`] in its table,
/// or whose key names another crate than the one behind it. Input: the text. Output: the
/// (table, key, crate) of each.
fn outside_allowlist(text: &str, root: &str) -> Vec<(String, String, String)> {
    dependency_names(text, root)
        .into_iter()
        .filter(|(table, key, krate)| {
            key != krate || !ALLOWED.contains(&(table.as_str(), krate.as_str()))
        })
        .collect()
}

#[test]
fn plugin_host_depends_on_no_sdk_or_engine_crate() {
    let text = include_str!("../Cargo.toml");
    let root = include_str!("../../../../Cargo.toml");
    let outside = outside_allowlist(text, root);
    assert!(outside.is_empty(), "{outside:?}");
    let mut direct: Vec<(String, String)> = dependency_names(text, root)
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
        dependency_names(text, ""),
        vec![
            pair("dependencies", "toml"),
            pair("target.cfg(unix).dependencies", "crazyagent-sdk"),
            pair("target.cfg(windows).dev-dependencies", "crazyagent"),
        ]
    );
    assert_eq!(outside_allowlist(text, "").len(), 2);
}

/// A renamed entry is judged by the crate behind it, so an allowed key
/// cannot carry an SDK or engine crate, and a `workspace = true` entry by its root name.
#[test]
fn the_allowlist_judges_the_crate_behind_a_renamed_entry() {
    let renamed = "[dependencies]\nserde = { package = \"crazyagent-sdk\", path = \"x\" }\n";
    assert_eq!(
        outside_allowlist(renamed, ""),
        vec![(
            "dependencies".to_owned(),
            "serde".to_owned(),
            "crazyagent-sdk".to_owned()
        )]
    );
    let workspace =
        "[dependencies]\ntoml = { workspace = true }\ncrazyagent = { workspace = true }\n";
    let root = "[workspace.dependencies]\ntoml = \"1\"\ncrazyagent = { path = \"x\" }\n";
    assert_eq!(
        outside_allowlist(workspace, root),
        vec![(
            "dependencies".to_owned(),
            "crazyagent".to_owned(),
            "crazyagent".to_owned()
        )]
    );
    let odd = "[dependencies]\nserde = { package = 1 }\n";
    assert_eq!(outside_allowlist(odd, "").len(), 1);
}

/// A `workspace = true` entry is judged by the crate its root entry names, so an allowed
/// key cannot carry an SDK or engine crate through a rename in the root table, and an
/// entry the root does not hold is reported.
#[test]
fn the_allowlist_judges_the_crate_behind_an_inherited_entry() {
    let member = "[dependencies]\ntoml = { workspace = true }\nserde = { workspace = true }\n";
    let root = "[workspace]\nmembers = []\n\n[workspace.dependencies]\ntoml = { package = \"crazyagent-sdk\", version = \"1\" }\n";
    assert_eq!(
        outside_allowlist(member, root),
        vec![
            ("dependencies".to_owned(), "serde".to_owned(), String::new()),
            (
                "dependencies".to_owned(),
                "toml".to_owned(),
                "crazyagent-sdk".to_owned()
            ),
        ]
    );
    let plain = "[workspace.dependencies]\ntoml = \"1\"\nserde = { version = \"1\" }\n";
    assert!(outside_allowlist(member, plain).is_empty());
}

/// A manifest text that asks for `net.request` and holds one `[net]` section. Input: the
/// section body after `[net]`. Output: the text.
fn net_text(net: &str) -> String {
    format!(
        "plugin = \"github\"\ntier = \"t2\"\nkind = \"connector\"\nversion = 1\npermissions = [\"net.request\"]\n\n[net]\n{net}"
    )
}

/// The refusal every `[net]` value rule answers, at the line of `[net]`.
fn not_allowed(text: &str) -> Result<Manifest, Refused> {
    let line = text.lines().position(|line| line == "[net]").unwrap() + 1;
    Err(Refused {
        code: "plugin_manifest_not_allowed",
        line,
    })
}

/// Checks that one `[net]` body is refused. Input: the body.
fn refused_net(net: &str) {
    let text = net_text(net);
    assert_eq!(manifest::parse(&text), not_allowed(&text), "{net}");
}

#[test]
fn a_valid_net_section_reads_its_hosts_and_captures() {
    let text = net_text(
        "hosts = [\"github.com\", \"api.github.com\", \"ghe.example:8443\", \"127.0.0.1:8080\", \"[::1]:9000\", \"localhost\"]\n\n[net.capture]\n\"github.com\" = [\"access_token\", \"device_code\"]\n",
    );
    let dir = plugin_dir("net-valid", &text);
    let read = manifest::read(&dir).unwrap();
    assert_eq!(
        read.net,
        Some(manifest::Net {
            hosts: [
                "github.com",
                "api.github.com",
                "ghe.example:8443",
                "127.0.0.1:8080",
                "[::1]:9000",
                "localhost",
            ]
            .map(str::to_owned)
            .to_vec(),
            capture: vec![(
                "github.com".to_owned(),
                vec!["access_token".to_owned(), "device_code".to_owned()],
            )],
            send: Vec::new(),
        })
    );
    assert_eq!(
        manifest::read(&plugin_dir("net-none", &manifest_text("agent-quit")))
            .unwrap()
            .net,
        None
    );
}

#[test]
fn net_is_required_with_net_request() {
    let text = "plugin = \"github\"\ntier = \"t2\"\nkind = \"connector\"\nversion = 1\npermissions = [\"net.request\"]\n";
    assert_eq!(
        manifest::parse(text),
        Err(Refused {
            code: "plugin_net_without_permission",
            line: 5,
        })
    );
}

#[test]
fn net_without_net_request_is_refused() {
    let text = format!(
        "{}\n[net]\nhosts = [\"github.com\"]\n",
        manifest_text("agent-quit")
    );
    assert_eq!(
        manifest::parse(&text),
        Err(Refused {
            code: "plugin_net_without_permission",
            line: 7,
        })
    );
}

#[test]
fn an_unknown_net_key_or_type_is_refused() {
    refused_net("hosts = [\"github.com\"]\nports = [443]\n");
    refused_net("hosts = \"github.com\"\n");
    refused_net("hosts = [443]\n");
    refused_net("hosts = []\n");
    refused_net("capture = {}\n");
    refused_net("hosts = [\"github.com\"]\ncapture = [\"access_token\"]\n");
    refused_net("hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = \"access_token\"\n");
    refused_net("hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [1]\n");
    let text = "plugin = \"github\"\ntier = \"t2\"\nkind = \"connector\"\nversion = 1\npermissions = [\"net.request\"]\nnet = true\n";
    assert_eq!(
        manifest::parse(text),
        Err(Refused {
            code: "plugin_manifest_not_allowed",
            line: 6,
        })
    );
}

#[test]
fn an_upper_case_host_is_refused() {
    refused_net("hosts = [\"GitHub.com\"]\n");
}

#[test]
fn a_host_with_a_scheme_is_refused() {
    refused_net("hosts = [\"https://github.com\"]\n");
}

#[test]
fn a_host_with_a_path_is_refused() {
    refused_net("hosts = [\"github.com/login\"]\n");
    refused_net("hosts = [\"github.com?x\"]\n");
}

#[test]
fn a_wildcard_host_is_refused() {
    refused_net("hosts = [\"*.github.com\"]\n");
    refused_net("hosts = [\"*\"]\n");
}

#[test]
fn a_host_with_userinfo_is_refused() {
    refused_net("hosts = [\"user@github.com\"]\n");
    refused_net("hosts = [\"user:pass@github.com\"]\n");
}

#[test]
fn a_non_loopback_ip_host_is_refused() {
    for host in [
        "10.0.0.1",
        "140.82.112.4",
        "169.254.169.254",
        "[fe80::1]",
        "[2606:4700::1111]",
        "0.0.0.0",
        "127.1",
    ] {
        refused_net(&format!("hosts = [\"{host}\"]\n"));
    }
}

#[test]
fn a_host_with_a_bad_port_or_label_is_refused() {
    for host in [
        "github.com:0",
        "github.com:65536",
        "github.com:08",
        "github.com:",
        ":443",
        "-github.com",
        "github-.com",
        "github..com",
        ".github.com",
        "git_hub.com",
        "github.com.",
    ] {
        refused_net(&format!("hosts = [\"{host}\"]\n"));
    }
}

#[test]
fn a_duplicate_host_is_refused() {
    refused_net("hosts = [\"github.com\", \"github.com\"]\n");
}

#[test]
fn a_host_over_253_bytes_is_refused() {
    let label = "a".repeat(63);
    let fits = format!("{label}.{label}.{label}.{}", "a".repeat(61));
    assert_eq!(fits.len(), manifest::HOST_BYTES_MAX);
    assert!(manifest::parse(&net_text(&format!("hosts = [\"{fits}\"]\n"))).is_ok());
    refused_net(&format!("hosts = [\"{fits}a\"]\n"));
    refused_net(&format!("hosts = [\"{}.com\"]\n", "a".repeat(64)));
}

#[test]
fn more_than_sixteen_hosts_are_refused() {
    let hosts = |count: usize| {
        (0..count)
            .map(|index| format!("\"h{index}.example\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    assert!(
        manifest::parse(&net_text(&format!(
            "hosts = [{}]\n",
            hosts(manifest::HOSTS_MAX)
        )))
        .is_ok()
    );
    refused_net(&format!("hosts = [{}]\n", hosts(manifest::HOSTS_MAX + 1)));
}

#[test]
fn a_capture_key_outside_hosts_is_refused() {
    refused_net(
        "hosts = [\"github.com\"]\n\n[net.capture]\n\"api.github.com\" = [\"access_token\"]\n",
    );
}

#[test]
fn a_duplicate_capture_field_is_refused() {
    refused_net(
        "hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [\"access_token\", \"access_token\"]\n",
    );
    refused_net("hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [\"access token\"]\n");
    refused_net("hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [\"\"]\n");
}

#[test]
fn more_than_four_capture_fields_are_refused() {
    assert!(manifest::parse(&net_text("hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [\"a\", \"b\", \"c\", \"d\"]\n")).is_ok());
    refused_net(
        "hosts = [\"github.com\"]\n\n[net.capture]\n\"github.com\" = [\"a\", \"b\", \"c\", \"d\", \"e\"]\n",
    );
}

#[test]
fn a_hex_or_octal_ip_host_is_refused() {
    for host in [
        "0xa9fea9fe",
        "0xa9.0xfe.0xa9.0xfe",
        "0251.0376.0251.0376",
        "169.254.43518",
        "2852039166",
        "1.2.3.0x4",
        "0xa9fea9fe:443",
    ] {
        refused_net(&format!("hosts = [\"{host}\"]\n"));
    }
    assert!(
        manifest::parse(&net_text(
            "hosts = [\"0xa9.example\", \"api2.example\", \"127.0.0.1\"]\n"
        ))
        .is_ok()
    );
}

/// `essential` marks a code plugin the app relies on: true or false reads as written, any
/// other value is refused at its line, and an embedding plugin, the only kind a user adds,
/// is refused for holding it at all.
#[test]
fn the_essential_key_marks_a_code_plugin_and_is_refused_elsewhere() {
    for (value, essential) in [("true", true), ("false", false)] {
        let text = format!("{}essential = {value}\n", manifest_text("agent-quit"));
        assert_eq!(
            manifest::parse(&text).map(|read| read.essential),
            Ok(essential),
            "{value}"
        );
    }
    for value in ["1", "\"yes\""] {
        let text = format!("{}essential = {value}\n", manifest_text("agent-quit"));
        assert_eq!(
            manifest::parse(&text),
            Err(Refused {
                code: "plugin_manifest_invalid",
                line: 6,
            }),
            "{value}"
        );
    }
    let embedding = "plugin = \"near\"\ntier = \"t2\"\nkind = \"embedding\"\nversion = 1\npermissions = []\nessential = true\n";
    assert_eq!(
        manifest::parse(embedding),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 6,
        })
    );
}

/// A `[net]` body with two hosts, one capture row and the given `[net.send]` lines.
fn with_send(send: &str) -> String {
    format!(
        "hosts = [\"github.com\", \"api.github.com\"]\n\n[net.capture]\n\"github.com\" = [\"access_token\", \"device_code\"]\n\n[net.send]\n{send}"
    )
}

#[test]
fn a_valid_send_row_reads_its_hosts_and_keep_as() {
    let text = net_text(&with_send(
        "access_token = { hosts = [\"api.github.com\"], keep_as = \"token\" }\ndevice_code = { hosts = [\"api.github.com\"] }\n",
    ));
    let read = manifest::parse(&text).unwrap().net.unwrap();
    assert_eq!(
        read.send,
        vec![
            manifest::NetSend {
                field: "access_token".to_owned(),
                hosts: vec!["api.github.com".to_owned()],
                keep_as: Some("token".to_owned()),
            },
            manifest::NetSend {
                field: "device_code".to_owned(),
                hosts: vec!["api.github.com".to_owned()],
                keep_as: None,
            },
        ]
    );
}

#[test]
fn a_send_row_for_an_uncaptured_field_is_refused() {
    refused_net(&with_send(
        "refresh_token = { hosts = [\"api.github.com\"] }\n",
    ));
}

#[test]
fn a_send_host_outside_hosts_is_refused() {
    refused_net(&with_send(
        "access_token = { hosts = [\"uploads.github.com\"] }\n",
    ));
}

#[test]
fn an_empty_send_host_list_is_refused() {
    refused_net(&with_send("access_token = { hosts = [] }\n"));
}

#[test]
fn a_keep_as_the_store_refuses_is_refused_at_read() {
    for name in ["a.", "token."] {
        refused_net(&with_send(&format!(
            "access_token = {{ hosts = [\"api.github.com\"], keep_as = \"{name}\" }}\n"
        )));
    }
    for name in ["nul", "con", "nul.token"] {
        let text = net_text(&with_send(&format!(
            "access_token = {{ hosts = [\"api.github.com\"], keep_as = \"{name}\" }}\n"
        )));
        assert!(manifest::parse(&text).is_ok(), "{name}");
    }
}

#[test]
fn a_duplicate_send_host_is_refused() {
    refused_net(&with_send(
        "access_token = { hosts = [\"api.github.com\", \"api.github.com\"] }\n",
    ));
}

#[test]
fn a_reused_or_refused_keep_as_is_refused() {
    refused_net(&with_send(
        "access_token = { hosts = [\"api.github.com\"], keep_as = \"token\" }\ndevice_code = { hosts = [\"api.github.com\"], keep_as = \"token\" }\n",
    ));
    for name in ["", "Token", ".token", "a..b", "a b"] {
        refused_net(&with_send(&format!(
            "access_token = {{ hosts = [\"api.github.com\"], keep_as = \"{name}\" }}\n"
        )));
    }
    refused_net(&with_send(&format!(
        "access_token = {{ hosts = [\"api.github.com\"], keep_as = \"{}\" }}\n",
        "t".repeat(manifest::KEEP_AS_BYTES_MAX + 1)
    )));
}

#[test]
fn an_unknown_send_key_or_type_is_refused() {
    refused_net(&with_send(
        "access_token = { hosts = [\"api.github.com\"], to = [\"github.com\"] }\n",
    ));
    refused_net(&with_send("access_token = [\"api.github.com\"]\n"));
    refused_net(&with_send(
        "access_token = { hosts = [\"api.github.com\"], keep_as = 1 }\n",
    ));
}

/// `capabilities` lists the capability stems a code plugin serves: absent reads empty, a
/// list reads in order, and up to [`CAPABILITIES_MAX`] stems fit.
#[test]
fn the_capabilities_key_reads_the_served_stems_in_order() {
    assert_eq!(
        manifest::parse(&manifest_text("agent-quit")).map(|read| read.capabilities),
        Ok(Vec::<String>::new())
    );
    let text = format!(
        "{}capabilities = [\"projects\", \"model\"]\n",
        manifest_text("agent-quit")
    );
    assert_eq!(
        manifest::parse(&text).map(|read| read.capabilities),
        Ok(vec!["projects".to_owned(), "model".to_owned()])
    );
    let most: Vec<String> = (0..CAPABILITIES_MAX)
        .map(|index| format!("\"s{index}\""))
        .collect();
    let text = format!(
        "{}capabilities = [{}]\n",
        manifest_text("agent-quit"),
        most.join(", ")
    );
    assert_eq!(
        manifest::parse(&text).map(|read| read.capabilities.len()),
        Ok(CAPABILITIES_MAX)
    );
}

/// A `capabilities` value that is not a list of distinct snake stems, or holds more than
/// [`CAPABILITIES_MAX`], is refused at its line; a misspelt key is still an unknown key.
#[test]
fn the_capabilities_key_refuses_anything_but_distinct_snake_stems() {
    let over: Vec<String> = (0..=CAPABILITIES_MAX)
        .map(|index| format!("\"s{index}\""))
        .collect();
    let over = format!("[{}]", over.join(", "));
    for value in [
        "\"projects\"",
        "[1]",
        "[\"Projects\"]",
        "[\"projects.list\"]",
        "[\"\"]",
        "[\"projects\", \"projects\"]",
        over.as_str(),
    ] {
        let text = format!("{}capabilities = {value}\n", manifest_text("agent-quit"));
        assert_eq!(
            manifest::parse(&text),
            Err(Refused {
                code: "plugin_manifest_invalid",
                line: 6,
            }),
            "{value}"
        );
    }
    let misspelt = format!(
        "{}capability = [\"projects\"]\n",
        manifest_text("agent-quit")
    );
    assert_eq!(
        manifest::parse(&misspelt),
        Err(Refused {
            code: "plugin_manifest_invalid",
            line: 6,
        })
    );
}
