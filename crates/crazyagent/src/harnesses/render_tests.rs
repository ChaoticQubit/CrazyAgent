use super::*;
use crate::harnesses::manifest::{KIND, MISSING, UNKNOWN_KEY, VALUE, parse};
use crate::plugins::{LaunchSecret, SecretValue, ServerSpec};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

const SMALL: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

const BOTH: &str = "mcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/agent/mcp.json\"\ninstructions:\n  render: \"file\"\n  path: \"{home}/agent/AGENTS.md\"\n";

fn renders_of(blocks: &str) -> Result<Renders, (&'static str, String)> {
    let manifest = parse(format!("{SMALL}{blocks}").as_bytes()).unwrap();
    read(&manifest).map_err(|refused| (refused.code, refused.message))
}

/// A listed launch's private home as T18 lays it out, `<agent home>/sample/account-1`,
/// each folder 0700. Output: the agent home's guard and the private home.
fn private_home() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let base = folder.path().canonicalize().unwrap();
    let home = base.join("sample").join("account-1");
    std::fs::create_dir_all(&home).unwrap();
    for path in [base.clone(), base.join("sample"), home.clone()] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    (folder, home)
}

fn files_server() -> LaunchServer {
    LaunchServer {
        name: "files".into(),
        spec: ServerSpec::Stdio {
            command: "/usr/bin/files".into(),
            args: vec!["--stdio".into()],
        },
        secrets: vec![LaunchSecret {
            name: "FILES_TOKEN".into(),
            value: SecretValue::new(b"s3cret".to_vec()),
        }],
    }
}

/// Task 18: a manifest's `mcp` and `instructions` blocks write the launch's MCP servers, in
/// the shape the render names, and its instruction text where the harness reads them, each
/// file 0600 in a 0700 folder of the private home.
#[test]
fn an_mcp_block_and_an_instructions_block_write_their_files_inside_the_private_home() {
    let (_home, home) = private_home();
    let renders = renders_of(BOTH);

    let written = renders.as_ref().map_err(Clone::clone).and_then(|renders| {
        write(renders, &home, &[files_server()], "Work in small steps.\n")
            .map_err(|problem| ("launch", problem.0))
    });

    let mcp = home.join("agent").join("mcp.json");
    let instructions = home.join("agent").join("AGENTS.md");
    assert_eq!(written, Ok(vec![mcp.clone(), instructions.clone()]));
    let declared = std::fs::read_to_string(&mcp)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    assert_eq!(
        declared,
        Some(serde_json::json!({
            "mcpServers": {
                "files": {
                    "type": "stdio",
                    "command": "/usr/bin/files",
                    "args": ["--stdio"],
                    "env": { "FILES_TOKEN": "s3cret" }
                }
            }
        }))
    );
    assert_eq!(
        std::fs::read_to_string(&instructions).ok().as_deref(),
        Some("Work in small steps.\n")
    );
    for (path, mode) in [
        (&mcp, 0o600),
        (&instructions, 0o600),
        (&home.join("agent"), 0o700),
    ] {
        let found = std::fs::symlink_metadata(path).map(|found| found.mode() & 0o7777);
        assert_eq!(found.ok(), Some(mode), "{}", path.display());
    }
}

/// Task 18: a render never writes through a link planted in the private home: a linked
/// folder is refused, and a link at a file's own name is replaced by the file, so what
/// either link named stays as it was.
#[test]
fn a_render_never_writes_through_a_link_planted_in_the_private_home() {
    let outside = tempfile::tempdir().unwrap();
    let kept = outside.path().join("kept.json");
    std::fs::write(&kept, "kept").unwrap();
    let renders = renders_of(BOTH).unwrap_or_default();

    let (_linked_home, linked_home) = private_home();
    std::os::unix::fs::symlink(outside.path(), linked_home.join("agent")).unwrap();
    let refused = write(&renders, &linked_home, &[files_server()], "text");

    let (_home, home) = private_home();
    std::fs::create_dir(home.join("agent")).unwrap();
    std::fs::set_permissions(home.join("agent"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&kept, home.join("agent").join("mcp.json")).unwrap();
    let replaced = write(&renders, &home, &[files_server()], "text");

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_account_home_unsafe".to_owned())
    );
    assert!(replaced.is_ok(), "{replaced:?}");
    let written = std::fs::symlink_metadata(home.join("agent").join("mcp.json"));
    assert!(written.is_ok_and(|written| written.is_file()));
    assert_eq!(std::fs::read_to_string(&kept).ok().as_deref(), Some("kept"));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
}

/// Task 18: the render blocks take only the kinds CrazyAgent writes, a path that is a file
/// inside the private home, the keys they have and one file per path; each refusal names
/// the setting.
#[test]
fn render_blocks_refuse_unknown_kinds_paths_outside_the_home_and_unknown_keys() {
    let inside = "this must be {home} followed by a file inside it, each name only ASCII letters, digits, ., _ and -.";
    let cases = [
        (
            "mcp:\n  render: \"yaml-servers\"\n  path: \"{home}/mcp.yaml\"\n",
            KIND,
            "mcp.render: CrazyAgent has no MCP render named yaml-servers.".to_owned(),
        ),
        (
            "instructions:\n  render: \"folder\"\n  path: \"{home}/rules\"\n",
            KIND,
            "instructions.render: CrazyAgent has no instructions render named folder.".to_owned(),
        ),
        (
            "mcp:\n  path: \"{home}/mcp.json\"\n",
            MISSING,
            "mcp.render: this setting is needed.".to_owned(),
        ),
        (
            "mcp:\n  render: \"json-mcp-servers\"\n",
            MISSING,
            "mcp.path: this setting is needed.".to_owned(),
        ),
        (
            "mcp:\n  render: \"json-mcp-servers\"\n  path: \"/tmp/mcp.json\"\n",
            VALUE,
            format!("mcp.path: {inside}"),
        ),
        (
            "mcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/../mcp.json\"\n",
            VALUE,
            format!("mcp.path: {inside}"),
        ),
        (
            "instructions:\n  render: \"file\"\n  path: \"{home}\"\n",
            VALUE,
            format!("instructions.path: {inside}"),
        ),
        (
            "mcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/mcp.json\"\n  merge: true\n",
            UNKNOWN_KEY,
            "mcp.merge: this key is not one the mcp block has.".to_owned(),
        ),
        (
            "mcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/a.json\"\ninstructions:\n  render: \"file\"\n  path: \"{home}/a.json\"\n",
            VALUE,
            "instructions.path: mcp.path already names this file.".to_owned(),
        ),
    ];
    for (blocks, code, sentence) in cases {
        assert_eq!(renders_of(blocks).err(), Some((code, sentence)), "{blocks}");
    }
    assert_eq!(
        renders_of(BOTH),
        Ok(Renders {
            mcp: Some(Render {
                kind: McpRender::JsonMcpServers,
                path: "agent/mcp.json".into(),
            }),
            instructions: Some(Render {
                kind: InstructionsRender::File,
                path: "agent/AGENTS.md".into(),
            }),
        })
    );
}

/// Task 18: a render path never names a file or folder CrazyAgent keeps in the private home
/// (its launch state, the home lock and the key file), in any letter case, as the folder may
/// not tell cases apart, so a render can never replace the lock or the key.
#[test]
fn a_render_path_never_names_a_file_crazyagent_keeps_in_the_home() {
    let kept = "CrazyAgent keeps its own files there, so a manifest cannot.";
    for (key, render, path) in [
        ("mcp", "json-mcp-servers", "{home}/crazyagent/mcp.json"),
        ("mcp", "json-mcp-servers", "{home}/crazyagent.lock"),
        ("instructions", "file", "{home}/.crazyagent-key"),
        ("instructions", "file", "{home}/CrazyAgent.LOCK"),
    ] {
        let blocks = format!("{key}:\n  render: \"{render}\"\n  path: \"{path}\"\n");
        assert_eq!(
            renders_of(&blocks).err(),
            Some((VALUE, format!("{key}.path: {kept}"))),
            "{path}"
        );
    }
}

/// T18 r2 F1 (rv-code4): a render path names its folders and its file in ASCII letters,
/// digits, `.`, `_` and `-` only. A macOS folder also folds Unicode letter case, so a name
/// holding the Kelvin sign would still open the home lock or the key file, and an accented
/// name could open another spelling of itself.
#[test]
fn a_render_path_holds_only_ascii_names_so_no_unicode_case_fold_reaches_an_engine_file() {
    let inside = "this must be {home} followed by a file inside it, each name only ASCII letters, digits, ., _ and -.";
    for (key, render, path) in [
        ("instructions", "file", "{home}/crazyagent.loc\u{212a}"),
        ("instructions", "file", "{home}/.crazyagent-\u{212a}ey"),
        (
            "mcp",
            "json-mcp-servers",
            "{home}/crazyagent\u{212a}/mcp.json",
        ),
        ("mcp", "json-mcp-servers", "{home}/agent/caf\u{e9}.json"),
    ] {
        let blocks = format!("{key}:\n  render: \"{render}\"\n  path: \"{path}\"\n");
        assert_eq!(
            renders_of(&blocks).err(),
            Some((VALUE, format!("{key}.path: {inside}"))),
            "{path}"
        );
    }
}

/// T18 r2 F1 (rv-code4): an MCP path and an instruction path that differ only in letter
/// case name one file on a macOS folder, so the second is refused instead of being written
/// over the first and leaving the harness with no servers.
#[test]
fn two_render_paths_that_differ_only_in_letter_case_are_refused_as_one_file() {
    let blocks = "mcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/agent/A.json\"\ninstructions:\n  render: \"file\"\n  path: \"{home}/Agent/a.JSON\"\n";

    assert_eq!(
        renders_of(blocks).err(),
        Some((
            VALUE,
            "instructions.path: mcp.path already names this file.".to_owned()
        ))
    );
}

/// Task 18: every bundled manifest's render blocks read, so no launch of a bundled harness
/// is refused for its own manifest.
#[test]
fn every_bundled_manifest_render_blocks_read() {
    for harness in crate::harnesses::registry::registry().all() {
        let found = read(harness.manifest());
        assert!(found.is_ok(), "{}: {:?}", harness.manifest().id, found);
    }
}

/// Task 18: a value filled into a template stays one string value of the template's format,
/// whatever quotes, backslashes or line ends it holds; braces that are not a placeholder are
/// plain text, and a placeholder with no value is refused.
#[test]
fn a_template_value_is_quoted_for_its_format() {
    let model = "a\"b\\c\nd\u{1}e";
    let values = [("model", model), ("relay_url", "http://127.0.0.1:4100/v1")];

    let json = fill(
        "{\"model\": \"{model}\", \"base\": \"{relay_url}\", \"other\": {}}",
        Format::Json,
        &values,
    );
    let toml = fill("model = \"{model}\"\n", Format::Toml, &values);
    let yaml = fill("model: \"{model}\"\n", Format::Yaml, &values);
    let missing = fill("model: \"{session}\"\n", Format::Yaml, &values);

    let json = json
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    assert_eq!(
        json,
        Some(serde_json::json!({
            "model": model,
            "base": "http://127.0.0.1:4100/v1",
            "other": {}
        }))
    );
    let toml = toml
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|document| document.get("model")?.as_str().map(str::to_owned));
    assert_eq!(toml.as_deref(), Some(model));
    assert_eq!(
        yaml.ok().as_deref(),
        Some("model: \"a\\\"b\\\\c\\nd\\u0001e\"\n")
    );
    assert_eq!(
        missing.err().map(|problem| problem.0),
        Some("launch_template_value_missing".to_owned())
    );
}
