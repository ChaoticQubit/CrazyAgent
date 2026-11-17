use super::Scenario;
use super::landing::{Plugin, Seeded, debug, lines, no_plugins, seeded, text};
use crazyagent::agent_git::Layout;
use crazyagent::launch_context;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The largest file whose text a launch capture keeps; a larger or non-UTF-8 file is kept
/// as its byte count.
const TEXT_BYTES_MAX: u64 = 64 * 1024;

/// The harnesses a pack is installed for, each with tools off and on.
const HARNESSES: [&str; 2] = ["claude-code", "codex"];

/// Lists the pack scenarios. Output: the scenarios in run order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "packs",
        name,
        capture,
    };
    vec![
        scenario("none", none),
        scenario("skill-folder", skill_folder),
        scenario("hook-folder", hook_folder),
        scenario("mcp-stdio-approved", mcp_stdio_approved),
        scenario("mcp-added-not-approved", mcp_added_not_approved),
    ]
}

/// A project with no plugin. Input: unused. Output: see `launches`.
fn none(_scratch: &Path) -> Result<Value, String> {
    launches(&seeded("ca-p4-none-", no_plugins, None)?)
}

/// A project with one enabled skill folder. Input: unused. Output: see `launches`.
fn skill_folder(_scratch: &Path) -> Result<Value, String> {
    let captured = launches(&seeded(
        "ca-p4-skill-",
        |disk| {
            let folder = disk.join("skills").join("tide-notes");
            fs::create_dir_all(&folder).map_err(text)?;
            fs::write(
            folder.join("SKILL.md"),
            "---\nname: tide-notes\ndescription: Writes tide table notes.\n---\nRead the tide table, then write one note per tide.\n",
        )
        .map_err(text)?;
            Ok(vec![Plugin::Skill(folder)])
        },
        None,
    )?)?;
    present(&captured, "tide-notes", "the enabled skill")?;
    Ok(captured)
}

/// A project with one enabled hooks plugin folder. Input: unused. Output: see `launches`.
fn hook_folder(_scratch: &Path) -> Result<Value, String> {
    let captured = launches(&seeded(
        "ca-p4-hooks-",
        |disk| {
            let folder = disk.join("tide-hooks");
            fs::create_dir_all(folder.join(".claude-plugin")).map_err(text)?;
            fs::create_dir_all(folder.join("hooks")).map_err(text)?;
            fs::write(
                folder.join(".claude-plugin").join("plugin.json"),
                "{\"name\": \"tide-hooks\"}\n",
            )
            .map_err(text)?;
            fs::write(
            folder.join("hooks").join("hooks.json"),
            "{\"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"/bin/echo checked\"}]}]}}\n",
        )
        .map_err(text)?;
            Ok(vec![Plugin::Hooks(folder)])
        },
        None,
    )?)?;
    present(&captured, "/bin/echo checked", "the enabled hook's command")?;
    Ok(captured)
}

/// A project with one approved stdio MCP server. Input: unused. Output: see `launches`.
fn mcp_stdio_approved(_scratch: &Path) -> Result<Value, String> {
    let captured = launches(&seeded(
        "ca-p4-mcp-",
        |_disk| {
            Ok(vec![Plugin::Server {
                name: "tide-server",
                command: "/bin/cat",
                approve: true,
            }])
        },
        None,
    )?)?;
    present(&captured, "tide-server", "the approved server")?;
    if !captured["read_paths"]
        .as_array()
        .is_some_and(|paths| paths.contains(&json!("/bin/cat")))
    {
        return Err(
            "the approved server's command is not among the tools-on read paths".to_owned(),
        );
    }
    Ok(captured)
}

/// A project with one stdio MCP server added but never approved. Input: unused.
/// Output: see `launches`.
fn mcp_added_not_approved(_scratch: &Path) -> Result<Value, String> {
    launches(&seeded(
        "ca-p4-mcp-added-",
        |_disk| {
            Ok(vec![Plugin::Server {
                name: "tide-server",
                command: "/bin/cat",
                approve: false,
            }])
        },
        None,
    )?)
}

/// Fails when a plugin the owner switched on never reaches a launch, so a fixture the engine
/// took but does not launch with cannot pass as a pack capture. Inputs: the capture, a text
/// only that plugin writes and what it is. Output: nothing, or which plugin is missing.
fn present(captured: &Value, needle: &str, what: &str) -> Result<(), String> {
    let shown = serde_json::to_string(&captured["launches"]).map_err(text)?;
    if shown.contains(needle) {
        Ok(())
    } else {
        Err(format!(
            "{what} reaches no launch: no launch file or argument names {needle}"
        ))
    }
}

/// Captures what leaves the engine for each launch of the seeded project: per harness and
/// tools switch the harness arguments, the denied tools and every file written into the
/// agent home, and the paths a tools-on sandbox must let the agent read. The Seatbelt text
/// itself is not captured here: only a real launch shows it, and that row comes after P2's
/// launch helper lands. Input: the seeded project. Output: the launches and the read paths.
fn launches(seeded: &Seeded) -> Result<Value, String> {
    let db = Connection::open_with_flags(
        seeded.engine.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(text)?;
    let context =
        launch_context::for_project(&db, &seeded.project, Layout::Clone).map_err(debug)?;
    let mut launches = Vec::new();
    for harness in HARNESSES {
        for tools in [false, true] {
            let home = private_folder(seeded, &format!("home-{harness}-{}", switch(tools)))?;
            let installed = context.install(harness, &home, tools).map_err(debug)?;
            let arguments: Vec<String> = installed
                .arguments()
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            launches.push(json!({
                "harness": harness,
                "tools": tools,
                "arguments": arguments,
                "denied_tools": installed.denied_tools(),
                "files": written(&home, &home)?,
            }));
        }
    }
    let read_paths: Vec<String> = context
        .read_paths()
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok(json!({
        "launches": launches,
        "read_paths": read_paths,
    }))
}

/// Names a tools switch for a folder name. Input: the switch. Output: `on` or `off`.
fn switch(tools: bool) -> &'static str {
    if tools { "on" } else { "off" }
}

/// Makes a new 0700 folder on the seeded scratch disk, resolved to its real path.
/// Inputs: the seeded project and the folder name. Output: the folder.
fn private_folder(seeded: &Seeded, name: &str) -> Result<std::path::PathBuf, String> {
    let folder = seeded.disk.path().join(name);
    fs::create_dir(&folder).map_err(text)?;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).map_err(text)?;
    folder.canonicalize().map_err(text)
}

/// Lists every file a launch wrote below a folder, sorted, with its text, its byte count
/// when it is large or not UTF-8, or its link target. Inputs: the agent home and the folder
/// being read. Output: one entry per file, its path relative to the home.
fn written(home: &Path, folder: &Path) -> Result<Vec<Value>, String> {
    let mut entries: Vec<_> = fs::read_dir(folder)
        .map_err(text)?
        .collect::<Result<_, _>>()
        .map_err(text)?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(home)
            .map_err(text)?
            .to_string_lossy()
            .into_owned();
        let kind = fs::symlink_metadata(&path).map_err(text)?;
        if kind.file_type().is_symlink() {
            let target = fs::read_link(&path).map_err(text)?;
            files.push(json!({"path": relative, "link": target.to_string_lossy()}));
        } else if kind.is_dir() {
            files.extend(written(home, &path)?);
        } else {
            let bytes = fs::read(&path).map_err(text)?;
            let shown = (kind.len() <= TEXT_BYTES_MAX)
                .then(|| String::from_utf8(bytes.clone()).ok())
                .flatten();
            files.push(match shown {
                Some(shown) => json!({"path": relative, "text": lines(&shown)}),
                None => json!({"path": relative, "bytes": bytes.len()}),
            });
        }
    }
    Ok(files)
}
