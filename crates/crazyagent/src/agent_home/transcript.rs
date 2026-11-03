use super::Harness;
use crate::usage::capture::{Format, open_record, records};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub const RECORDS_READ_MAX: usize = 4;
pub const RECORD_BYTES_MAX: u64 = 8 * 1024 * 1024;
pub const LOADED_MAX: usize = 16;
pub const USED_MAX: usize = 32;
pub const PATH_BYTES_MAX: usize = 512;
pub const SKILL_BYTES_MAX: usize = 128;
pub const VIEW_BYTES_MAX: usize =
    2 * (LOADED_MAX * (PATH_BYTES_MAX + 64) + USED_MAX * (SKILL_BYTES_MAX + 8));
const AGENTS_HEADING: &str = "# AGENTS.md instructions for ";
const SKILL_OPENING: &str = "<skill>\n<name>";

const _: () = assert!(VIEW_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX / 2);

/// One instruction file an agent's harness reported loading: the file name shown in a
/// list, such as `CLAUDE.md`, and where it was. Codex names the folder whose `AGENTS.md`
/// chain it loaded rather than each file, so its place is that folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadedFile {
    pub name: String,
    pub path: String,
}

/// What an agent's harness reported in its own newest session records: how many records
/// were read, the instruction files it loaded and the skills it invoked, each once in the
/// order first seen. Never holds any file content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub records: usize,
    pub loaded: Vec<LoadedFile>,
    pub used: Vec<String>,
}

impl Observed {
    fn load(&mut self, name: &str, path: &str) {
        let known = self
            .loaded
            .iter()
            .any(|file| file.name == name && file.path == path);
        if self.loaded.len() < LOADED_MAX && shown(path, PATH_BYTES_MAX) && !known {
            self.loaded.push(LoadedFile {
                name: name.to_owned(),
                path: path.to_owned(),
            });
        }
    }

    fn invoke(&mut self, skill: &str) {
        let named = shown(skill, SKILL_BYTES_MAX)
            && skill
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b":-_.".contains(&byte));
        if self.used.len() < USED_MAX && named && !self.used.iter().any(|used| used == skill) {
            self.used.push(skill.to_owned());
        }
    }
}

fn shown(text: &str, bytes_max: usize) -> bool {
    !text.is_empty() && text.len() <= bytes_max && !text.chars().any(char::is_control)
}

/// Reads what an agent's harness wrote about itself in the agent's private home: Claude
/// Code's `projects/**/<session>.jsonl` or Codex's `sessions/**/rollout-*.jsonl`, the
/// newest `RECORDS_READ_MAX`, the first `RECORD_BYTES_MAX` bytes of each, never following
/// a link. Inputs: the home and the harness. Output: the files and skills it reported.
pub fn observed(home: &Path, harness: Harness) -> Observed {
    let format = match harness {
        Harness::Claude => Format::Claude,
        Harness::Codex => Format::Codex,
    };
    let mut seen = Observed::default();
    for path in records(home, format).into_iter().take(RECORDS_READ_MAX) {
        let Some((file, _, _)) = open_record(&path) else {
            continue;
        };
        seen.records += 1;
        let mut reader = BufReader::new(file.take(RECORD_BYTES_MAX));
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            match harness {
                Harness::Claude => claude_line(text, &mut seen),
                Harness::Codex => codex_line(text, &mut seen),
            }
        }
    }
    assert!(seen.records <= RECORDS_READ_MAX);
    assert!(seen.loaded.len() <= LOADED_MAX && seen.used.len() <= USED_MAX);
    seen
}

/// Takes one Claude Code record line: an `instructions` attachment names the files it
/// loaded, and an assistant `Skill` tool call names the skill it invoked.
fn claude_line(text: &str, seen: &mut Observed) {
    if !text.contains("\"instructions\"") && !text.contains("\"Skill\"") {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    match value["type"].as_str() {
        Some("attachment") if value["attachment"]["type"] == "instructions" => {
            for file in value["attachment"]["files"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let Some(path) = file["path"].as_str() else {
                    continue;
                };
                if let Some(name) = Path::new(path).file_name().and_then(|name| name.to_str()) {
                    seen.load(name, path);
                }
            }
        }
        Some("assistant") => {
            for item in value["message"]["content"].as_array().into_iter().flatten() {
                if item["type"] == "tool_use"
                    && item["name"] == "Skill"
                    && let Some(skill) = item["input"]["skill"].as_str()
                {
                    seen.invoke(skill);
                }
            }
        }
        _ => {}
    }
}

/// Takes one Codex rollout line: a user message that opens with its `AGENTS.md`
/// heading names the folder whose instructions it loaded, and one that opens with its
/// selected skill fragment names the skill.
fn codex_line(text: &str, seen: &mut Observed) {
    if !text.contains("AGENTS.md instructions for ") && !text.contains("<skill>") {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let payload = &value["payload"];
    if value["type"] != "response_item" || payload["type"] != "message" || payload["role"] != "user"
    {
        return;
    }
    for item in payload["content"].as_array().into_iter().flatten() {
        let Some(text) = item["text"].as_str() else {
            continue;
        };
        if let Some(rest) = text.strip_prefix(AGENTS_HEADING) {
            seen.load("AGENTS.md", rest.lines().next().unwrap_or_default());
        } else if let Some((skill, _)) = text
            .strip_prefix(SKILL_OPENING)
            .and_then(|rest| rest.split_once("</name>"))
        {
            seen.invoke(skill);
        }
    }
}

#[cfg(test)]
mod tests;
