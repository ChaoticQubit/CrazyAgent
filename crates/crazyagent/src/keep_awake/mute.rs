use super::journal::{Entry, Journal};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// The journal kind of the sound state Keep awake muted while the lid was closed
/// (DEC-167 Task 17).
pub const KIND: &str = "keep_awake_mute";

const SINK_BYTES_MAX: usize = 256;
#[cfg(target_os = "macos")]
const OSASCRIPT: &str = "/usr/bin/osascript";
#[cfg(unix)]
const READ_MUTED: &str = "output muted of (get volume settings)";

/// The program that reads and sets the sound: osascript on macOS, wpctl or pactl on
/// Linux, each at the path given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    Osascript(PathBuf),
    Wpctl(PathBuf),
    Pactl(PathBuf),
}

impl Tool {
    /// The tool's name as the journal keeps it. Output: osascript, wpctl or pactl.
    fn name(&self) -> &'static str {
        match self {
            Self::Osascript(_) => "osascript",
            Self::Wpctl(_) => "wpctl",
            Self::Pactl(_) => "pactl",
        }
    }

    /// The program file. Output: its absolute path.
    #[cfg(unix)]
    fn program(&self) -> &Path {
        match self {
            Self::Osascript(program) | Self::Wpctl(program) | Self::Pactl(program) => program,
        }
    }
}

/// The sound state before Keep awake muted it: muted or not, the tool that read it and
/// the output it was read on (none for osascript, the node id for wpctl, the sink name
/// for pactl).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    muted: bool,
    tool: String,
    sink: String,
}

fn error(code: &str, problem: impl std::fmt::Display) -> EngineError {
    assert!(code.starts_with("keep_awake_mute"));
    EngineError(format!("{code}: {problem}"))
}

/// Tells whether an output name is one this module passes on: at most 256 bytes, no space
/// or control character. Input: the name. Output: true when it may be used.
fn plain(sink: &str) -> bool {
    sink.len() <= SINK_BYTES_MAX && !sink.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Finds one Linux sound tool by its program name through the one tool resolver, in the
/// system's own folders only. Input: the name. Output: its program, or none.
#[cfg(target_os = "linux")]
fn found(name: &str) -> Option<PathBuf> {
    let folders = ["/usr/bin", "/bin", "/usr/local/bin"]
        .map(PathBuf::from)
        .to_vec();
    let places = crate::tools::Places::new(folders, None);
    let resolved = crate::tools::resolve(name, &places).ok()?;
    (resolved.kind == crate::tools::Kind::Native).then_some(resolved.program)
}

/// The sound tool this computer has: osascript on macOS; wpctl, else pactl, on Linux;
/// none on Windows (not available until the K0-7 probe shows a call without
/// administrator). Output: the tool, or none.
pub fn platform() -> Option<Tool> {
    #[cfg(target_os = "macos")]
    return Path::new(OSASCRIPT)
        .is_file()
        .then(|| Tool::Osascript(OSASCRIPT.into()));
    #[cfg(target_os = "linux")]
    return found("wpctl")
        .map(Tool::Wpctl)
        .or_else(|| found("pactl").map(Tool::Pactl));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}

/// The tool a journaled sound state was read with, found again on this computer, so a
/// restore after a crash uses the same tool. Input: a `KIND` entry. Output: the tool, or
/// `keep_awake_mute_tool` when it is gone.
pub fn tool_for(entry: &Entry) -> Result<Tool, EngineError> {
    let saved = saved(entry)?;
    let tool = platform().filter(|tool| tool.name() == saved.tool);
    #[cfg(target_os = "linux")]
    let tool = tool.or_else(|| {
        if saved.tool == "pactl" {
            found("pactl").map(Tool::Pactl)
        } else {
            None
        }
    });
    tool.ok_or_else(|| error("keep_awake_mute_tool", saved.tool))
}

/// Reads a journaled sound state. Input: a `KIND` entry. Output: the state, or
/// `keep_awake_mute_entry_invalid`.
fn saved(entry: &Entry) -> Result<Saved, EngineError> {
    assert_eq!(entry.kind, KIND);
    let saved: Saved = serde_json::from_str(&entry.restore)
        .map_err(|problem| error("keep_awake_mute_entry_invalid", problem))?;
    if !plain(&saved.sink) {
        return Err(error("keep_awake_mute_entry_invalid", "output name"));
    }
    Ok(saved)
}

/// Runs the sound tool once with a fixed system PATH and, for wpctl and pactl, the
/// owner's runtime folder, which is where their sound server listens. Inputs: the tool
/// and its arguments. Output: what it printed, or `keep_awake_mute_tool` when it failed.
#[cfg(unix)]
fn ask(tool: &Tool, arguments: &[&str]) -> Result<String, EngineError> {
    assert!(tool.program().is_absolute());
    let mut command = std::process::Command::new(tool.program());
    command
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/");
    if !matches!(tool, Tool::Osascript(_)) {
        let uid = rustix::process::getuid().as_raw();
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|runtime| !runtime.is_empty())
            .unwrap_or_else(|| format!("/run/user/{uid}").into());
        command.env("XDG_RUNTIME_DIR", runtime);
        if let Some(server) = std::env::var_os("PULSE_SERVER") {
            command.env("PULSE_SERVER", server);
        }
    }
    super::freeze::run(&mut command).ok_or_else(|| error("keep_awake_mute_tool", tool.name()))
}

/// Reads the sound state now. Input: the tool. Output: muted or not and the output it was
/// read on, none for an output that cannot be muted, or why it could not be read.
#[cfg(unix)]
fn read(tool: &Tool) -> Result<Option<(bool, String)>, EngineError> {
    let state = match tool {
        Tool::Osascript(_) => match ask(tool, &["-e", READ_MUTED])?.trim() {
            "true" => Some((true, String::new())),
            "false" => Some((false, String::new())),
            _ => None,
        },
        Tool::Wpctl(_) => {
            let Ok(text) = ask(tool, &["inspect", "@DEFAULT_AUDIO_SINK@"]) else {
                return Ok(None);
            };
            let id = text
                .lines()
                .next()
                .and_then(|line| line.trim().strip_prefix("id "))
                .and_then(|rest| rest.split(',').next())
                .filter(|id| (1..=10).contains(&id.len()))
                .filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()));
            let Some(id) = id else {
                return Ok(None);
            };
            let muted = ask(tool, &["get-volume", id])?.contains("[MUTED]");
            Some((muted, id.to_owned()))
        }
        Tool::Pactl(_) => {
            let Ok(text) = ask(tool, &["get-default-sink"]) else {
                return Ok(None);
            };
            let sink = text.trim().to_owned();
            if sink.is_empty() || !plain(&sink) {
                return Ok(None);
            }
            let muted = match ask(tool, &["get-sink-mute", &sink])?.trim() {
                "Mute: yes" => true,
                "Mute: no" => false,
                other => return Err(error("keep_awake_mute_unread", other)),
            };
            Some((muted, sink))
        }
    };
    assert!(state.as_ref().is_none_or(|(_, sink)| plain(sink)));
    Ok(state)
}

/// Sets the sound muted or not on one output. Inputs: the tool, the output and the state.
/// Output: Ok, or why it could not be set.
#[cfg(unix)]
fn set(tool: &Tool, sink: &str, muted: bool) -> Result<(), EngineError> {
    assert!(plain(sink));
    let flag = if muted { "1" } else { "0" };
    match tool {
        Tool::Osascript(_) => {
            let script = if muted {
                "set volume with output muted"
            } else {
                "set volume without output muted"
            };
            ask(tool, &["-e", script])?;
        }
        Tool::Wpctl(_) => {
            ask(tool, &["set-mute", sink, flag])?;
        }
        Tool::Pactl(_) => {
            ask(tool, &["set-sink-mute", sink, flag])?;
        }
    }
    Ok(())
}

/// Mutes the sound: reads the current state, journals it synced, then mutes. Inputs: the
/// lane journal and the tool. Output: true when an entry was journaled, false for an
/// output that cannot be muted, or why the sound could not be read or set.
pub fn mute(journal: &mut Journal, tool: &Tool) -> Result<bool, EngineError> {
    #[cfg(unix)]
    {
        let Some((muted, sink)) = read(tool)? else {
            return Ok(false);
        };
        let saved = Saved {
            muted,
            tool: tool.name().into(),
            sink,
        };
        let restore = serde_json::to_string(&saved).expect("a sound state is plain data");
        journal.record(Entry {
            kind: KIND.into(),
            restore,
        })?;
        set(tool, &saved.sink, true)?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = (journal, tool);
        Err(error(
            "keep_awake_mute_unavailable",
            "Not available on Windows",
        ))
    }
}

/// Puts back the journaled sound state, so an owner who was muted stays muted. Inputs: a
/// `KIND` entry and the tool. Output: Ok, or why it could not be put back.
pub fn restore(entry: &Entry, tool: &Tool) -> Result<(), EngineError> {
    let saved = saved(entry)?;
    if saved.tool != tool.name() {
        return Err(error("keep_awake_mute_tool", saved.tool));
    }
    #[cfg(unix)]
    {
        set(tool, &saved.sink, saved.muted)
    }
    #[cfg(not(unix))]
    {
        Err(error(
            "keep_awake_mute_unavailable",
            "Not available on Windows",
        ))
    }
}
