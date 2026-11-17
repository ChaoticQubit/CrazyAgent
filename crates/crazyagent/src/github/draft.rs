use super::publish;
use crate::engine::EngineError;
use std::collections::VecDeque;
use std::path::Path;

#[cfg(all(test, target_os = "macos"))]
mod draft_tests;

pub(super) const BASELINE: [&str; 4] = [".env*", "*.pem", "*.key", ".DS_Store"];
pub(super) const NOT_ASKED: &str = "github_publish_draft_not_asked";
pub(super) const STOPPED: &str = "github_publish_draft_stopped";
const LISTED_MAX: usize = 2000;
const DEPTH_MAX: usize = 6;
const DRAFT_LINES_MAX: usize = 200;
const PROMPT_IGNORE_LINES_MAX: usize = 200;
const SKIPPED: [&str; 2] = [".git", ".crazyagent"];
const INSTRUCTION: &str = "You draft the .gitignore of a software project about to be \
     published to GitHub. Below are the names of the project's files and folders, folders \
     ending in /, and the lines its .gitignore holds now; you get no file contents. Answer \
     with .gitignore patterns only, one per line, and nothing else: no prose, no comments, \
     no code fences, no negations. List what must never be pushed and is not ignored yet: \
     secrets and credential files, env files, editor and operating system files, \
     dependency folders, build output, logs and caches. Never list a pattern that would \
     hide the project's own source code or documentation.";
const LEFT_OUT: &str = "(more names were left out)";

/// Lists the names in a project for the agent that drafts its .gitignore, folder by folder
/// in name order, folders ending in `/`. No link is followed and nothing is opened but
/// folders; `.git`, `.crazyagent` and names holding a control character or a backslash
/// are left out, and so is what lies past `LISTED_MAX` names or `DEPTH_MAX` levels.
/// Input: the project folder. Output: the names and whether some were cut, or
/// `github_publish_failed` when the folder itself cannot be read.
pub(super) fn listing(project: &Path) -> Result<(Vec<String>, bool), EngineError> {
    let mut listed: Vec<String> = Vec::new();
    let mut cut = false;
    let mut folders: VecDeque<(String, usize)> = VecDeque::from([(String::new(), 0)]);
    while let Some((prefix, depth)) = folders.pop_front() {
        let read = match std::fs::read_dir(project.join(&prefix)) {
            Ok(read) => read,
            Err(_) if depth == 0 => return Err(EngineError("github_publish_failed".into())),
            Err(_) => {
                cut = true;
                continue;
            }
        };
        let mut entries: Vec<(String, bool)> = Vec::new();
        for entry in read {
            let Ok(entry) = entry else {
                cut = true;
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let unlisted = name
                .chars()
                .any(|character| character.is_control() || character == '\\');
            if unlisted || (depth == 0 && SKIPPED.contains(&name.as_str())) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                cut = true;
                continue;
            };
            entries.push((name, kind.is_dir()));
        }
        entries.sort();
        for (name, folder) in entries {
            if listed.len() == LISTED_MAX {
                return Ok((listed, true));
            }
            if folder {
                let path = format!("{prefix}{name}/");
                if depth + 1 < DEPTH_MAX {
                    folders.push_back((path.clone(), depth + 1));
                } else {
                    cut = true;
                }
                listed.push(path);
            } else {
                listed.push(format!("{prefix}{name}"));
            }
        }
    }
    assert!(listed.len() <= LISTED_MAX);
    Ok((listed, cut))
}

/// Builds the prompt the drafting agent gets: the fixed instruction, the project's names,
/// a line when some were left out, and the first `PROMPT_IGNORE_LINES_MAX` lines of its
/// .gitignore. No file's contents go in. Inputs: the names, whether some were cut and the
/// .gitignore text, if there is one. Output: the prompt.
pub(super) fn prompt(listed: &[String], cut: bool, existing: Option<&str>) -> String {
    let mut text = format!("{INSTRUCTION}\n\nNames in the project:\n");
    for name in listed {
        text.push_str(name);
        text.push('\n');
    }
    if cut {
        text.push_str(LEFT_OUT);
        text.push('\n');
    }
    text.push_str("\nLines the .gitignore holds now:\n");
    for line in existing
        .unwrap_or_default()
        .lines()
        .take(PROMPT_IGNORE_LINES_MAX)
    {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// Reads the patterns out of the drafting agent's reply, which is untrusted, since the
/// project's names reach its prompt. A line is kept only when it is one plain pattern:
/// not blank, no comment, no negation (by last match it could bring back `.env*` or
/// `/.crazyagent/`), no catch-all made of `*`, `?` and `/` only, no code fence, no
/// whitespace or backslash inside, and valid for a .gitignore. Input: the reply. Output:
/// the first `DRAFT_LINES_MAX` such patterns, each once.
pub(super) fn reply_lines(reply: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        let catch_all = line
            .chars()
            .all(|character| matches!(character, '*' | '?' | '/'));
        let unplain = line.starts_with(['#', '!', '`'])
            || line.contains('\\')
            || line.chars().any(char::is_whitespace);
        if catch_all || unplain {
            continue;
        }
        let line = line.to_owned();
        if kept.contains(&line) || !publish::lines_valid(std::slice::from_ref(&line)) {
            continue;
        }
        kept.push(line);
        if kept.len() == DRAFT_LINES_MAX {
            break;
        }
    }
    assert!(kept.len() <= DRAFT_LINES_MAX);
    kept
}

/// Joins the baseline and the agent's patterns into the lines a publish proposes: the
/// baseline first, then the agent's in its order, each once, leaving out every line the
/// project's .gitignore holds already, so the owner sees exactly what would be added.
/// Inputs: the agent's patterns and the .gitignore text, if there is one. Output: the lines.
pub(super) fn merged(agent: &[String], existing: Option<&str>) -> Vec<String> {
    let present: Vec<&str> = existing
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for line in BASELINE.into_iter().chain(agent.iter().map(String::as_str)) {
        if !present.contains(&line) && !lines.iter().any(|kept| kept == line) {
            lines.push(line.to_owned());
        }
    }
    lines
}
