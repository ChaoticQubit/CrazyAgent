//! Edit with AI's instruction check (DEC-172): what CrazyAgent itself can prove about one
//! instructions text before a model rewrites it, lines said twice and references to files,
//! skills, MCP servers and commands its agents do not have, and the shape every finding
//! takes on the wire, the model's own findings included. It reads no file's content and
//! writes nothing.

use crate::agent_home::{TEXT_BYTES_MAX, repeated_line_numbers, repeats_within};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The most findings one proposal carries, the engine's and the model's together.
pub const FINDINGS_MAX: usize = 64;
/// The most lines one finding names.
pub const LINES_PER_FINDING_MAX: usize = 8;
/// The longest note of one finding, in bytes.
pub const NOTE_BYTES_MAX: usize = 240;
/// The most code spans of one text read as references.
pub const REFERENCES_MAX: usize = 64;
/// The longest code span read as a reference, in bytes.
pub const NAME_BYTES_MAX: usize = 255;
/// The longest line whose code spans are read, in bytes.
const LINE_BYTES_MAX: usize = 2048;
/// The characters that make a span a pattern, a variable or a URL rather than a path.
const NOT_IN_PATHS: &str = "*?[]{}<>|\"'\\:$%#=";

const _: () = assert!(
    FINDINGS_MAX * (2 * NOTE_BYTES_MAX + LINES_PER_FINDING_MAX * 12 + 96) + 256
        <= crate::protocol::FRAME_BYTES_MAX
);
const _: () = assert!(NAME_BYTES_MAX < LINE_BYTES_MAX);

/// Shell words that name no program a resolver could find.
const BUILTINS: &[&str] = &[
    "alias", "bg", "break", "case", "cd", "command", "continue", "do", "done", "echo", "elif",
    "else", "esac", "eval", "exec", "exit", "export", "false", "fg", "fi", "for", "function",
    "hash", "if", "jobs", "kill", "local", "popd", "printf", "pushd", "pwd", "read", "return",
    "set", "shift", "source", "test", "then", "time", "trap", "true", "type", "ulimit", "umask",
    "unalias", "unset", "wait", "while",
];

/// What a finding is about: a text longer than a Claude master takes, a line said twice, a
/// reference to something these agents do not have, or rules that contradict each other.
/// On the wire it is the snake case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    OverBudget,
    Duplicate,
    BrokenReference,
    Contradiction,
}

impl FindingKind {
    /// Names the kind as the wire and the model prompt spell it. Input: the kind. Output:
    /// the snake case name, the same one serde writes.
    pub fn name(self) -> &'static str {
        match self {
            Self::OverBudget => "over_budget",
            Self::Duplicate => "duplicate",
            Self::BrokenReference => "broken_reference",
            Self::Contradiction => "contradiction",
        }
    }

    /// Names the kind for the owner, the way the TUI and the desktop show it above a
    /// finding's lines and note. Input: the kind. Output: the words, starting with a capital
    /// letter.
    pub fn label(self) -> &'static str {
        match self {
            Self::OverBudget => "Over budget",
            Self::Duplicate => "Duplicate",
            Self::BrokenReference => "Broken reference",
            Self::Contradiction => "Contradiction",
        }
    }
}

/// One thing the check found in a saved instructions text: its kind, the lines it covers,
/// counted from 1 in the text the proposal was made from and none for the whole file, and a
/// short plain note with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub kind: FindingKind,
    pub lines: Vec<u32>,
    pub note: String,
}

impl Finding {
    /// Makes a finding with a clean note. Inputs: the kind, at most `LINES_PER_FINDING_MAX`
    /// lines in rising order, none for the whole file, and a note that is not blank once
    /// clean. Output: the finding.
    pub fn new(kind: FindingKind, lines: Vec<u32>, note: &str) -> Self {
        assert!(lines.len() <= LINES_PER_FINDING_MAX);
        assert!(lines.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(lines.first().is_none_or(|line| *line >= 1));
        let note = clean_note(note);
        assert!(!note.is_empty());
        Self { kind, lines, note }
    }
}

/// Names the lines a finding covers, for the model prompt and the TUI: "whole file",
/// "line 3" or "lines 2, 5". Input: the lines, none for the whole file. Output: the words,
/// lower case.
pub fn lines_label(lines: &[u32]) -> String {
    match lines {
        [] => "whole file".into(),
        [line] => format!("line {line}"),
        _ => {
            let numbers: Vec<String> = lines.iter().map(u32::to_string).collect();
            format!("lines {}", numbers.join(", "))
        }
    }
}

/// Whether a character hides or reorders text when shown: a control character, a zero-width
/// mark, a direction override or isolate, or a byte order mark. Input: the character.
/// Output: the answer.
fn hidden(character: char) -> bool {
    let code = u32::from(character);
    character.is_control()
        || (0x200B..=0x200F).contains(&code)
        || (0x202A..=0x202E).contains(&code)
        || (0x2066..=0x2069).contains(&code)
        || code == 0xFEFF
}

/// Makes a note safe to show in a terminal and to send to a model: line breaks and tabs turn
/// into spaces, characters that hide or reorder text go, runs of spaces fold into one, and
/// it is cut to `NOTE_BYTES_MAX` bytes at a character boundary. Input: the note. Output: the
/// clean note, empty when nothing is left.
pub fn clean_note(note: &str) -> String {
    let mut clean = String::with_capacity(note.len().min(NOTE_BYTES_MAX));
    for character in note.chars() {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if hidden(character) || (character == ' ' && (clean.is_empty() || clean.ends_with(' '))) {
            continue;
        }
        if clean.len() + character.len_utf8() > NOTE_BYTES_MAX {
            break;
        }
        clean.push(character);
    }
    let clean = clean.trim_end().to_owned();
    assert!(clean.len() <= NOTE_BYTES_MAX && !clean.chars().any(hidden));
    clean
}

/// Finds lines said twice: each plain line that repeats an earlier one in the same place,
/// and, when the run may see the app text, each line the app text already has, both by the
/// rule a joined text uses to leave repeated lines out. Inputs: the text and the app text,
/// none when the run may not see it. Output: the findings, in line order.
pub fn repeats(text: &str, app: Option<&str>) -> Vec<Finding> {
    assert!(text.len() <= TEXT_BYTES_MAX);
    let number = |line: usize| u32::try_from(line).expect("a text of at most 64 KB");
    let mut found: Vec<(u32, Finding)> = Vec::new();
    for (later, earlier) in repeats_within(text) {
        let note = format!("Line {later} repeats line {earlier}.");
        let lines = vec![number(earlier), number(later)];
        let finding = Finding::new(FindingKind::Duplicate, lines, &note);
        found.push((number(later), finding));
    }
    let in_app = app.map_or_else(Vec::new, |app| repeated_line_numbers(app, text));
    for line in in_app {
        let note = "The app instructions already have this line.";
        let finding = Finding::new(FindingKind::Duplicate, vec![number(line)], note);
        found.push((number(line), finding));
    }
    found.sort_by_key(|(line, _)| *line);
    found.into_iter().map(|(_, finding)| finding).collect()
}

/// What the check knows of the agents a text is written for. A fact it could not read is
/// none, and the references it would judge by that fact are left out, never guessed:
/// `folder` is the project's canonical folder, none for the app text, and `program` answers
/// whether a command is found on this computer, none while that is not known yet.
pub struct Known<'a> {
    pub folder: Option<&'a Path>,
    pub skills: Option<&'a [String]>,
    pub servers: Option<&'a [String]>,
    pub program: &'a dyn Fn(&str) -> Option<bool>,
}

/// What one code span of a text names, as far as its shape tells: a relative path, a skill
/// name, the MCP server of a tool, or the program that leads a command line. Each holds the
/// part of the span the check looks up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference<'a> {
    File(&'a str),
    Skill(&'a str),
    Tool(&'a str),
    Command(&'a str),
}

/// Finds references to files, skills, MCP servers and commands these agents do not have.
/// Only code spans in backticks outside code blocks are read, so plain words never count,
/// and a span's shape decides what it may name. A name missing on several lines is one
/// finding. Inputs: the text and what the check knows. Output: the findings, in the order
/// their first line comes, from at most `REFERENCES_MAX` spans.
pub fn broken(text: &str, known: &Known<'_>) -> Vec<Finding> {
    assert!(text.len() <= TEXT_BYTES_MAX);
    assert!(known.folder.is_none_or(Path::is_absolute));
    let mut found: Vec<(String, Vec<u32>)> = Vec::new();
    let mut judged = 0;
    let mut open: Option<(u8, usize)> = None;
    'lines: for (index, line) in text.lines().enumerate() {
        let number = u32::try_from(index + 1).expect("a text of at most 64 KB");
        match (open, fence(line)) {
            (Some((mark, length)), Some((closing, run, rest)))
                if closing == mark && run >= length && rest.is_empty() =>
            {
                open = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, Some((mark, run, _))) => {
                open = Some((mark, run));
                continue;
            }
            (None, None) => {}
        }
        if line.len() > LINE_BYTES_MAX {
            continue;
        }
        let skill_line = line.to_ascii_lowercase().contains("skill");
        for span in spans(line) {
            let Some(reference) = reference(span, skill_line) else {
                continue;
            };
            if judged == REFERENCES_MAX {
                break 'lines;
            }
            judged += 1;
            let Some(note) = missing(span, reference, known) else {
                continue;
            };
            match found.iter_mut().find(|(seen, _)| *seen == note) {
                Some((_, lines)) => {
                    if lines.len() < LINES_PER_FINDING_MAX && lines.last() != Some(&number) {
                        lines.push(number);
                    }
                }
                None => found.push((note, vec![number])),
            }
        }
    }
    assert!(judged <= REFERENCES_MAX && found.len() <= REFERENCES_MAX);
    found
        .into_iter()
        .map(|(note, lines)| Finding::new(FindingKind::BrokenReference, lines, &note))
        .collect()
}

/// Reads a code fence line: up to three spaces, then three or more backticks or tildes.
/// Input: the line. Output: the fence mark, its length and what follows it, trimmed, or
/// none for a line that is no fence.
fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let mark = *trimmed.as_bytes().first()?;
    if mark != b'`' && mark != b'~' {
        return None;
    }
    let length = trimmed.bytes().take_while(|byte| *byte == mark).count();
    (length >= 3).then(|| (mark, length, trimmed[length..].trim()))
}

/// Lists the code spans of one line: the text between a run of backticks and the next run
/// of the same length, trimmed; a run with no match opens nothing. Input: the line, at most
/// `LINE_BYTES_MAX` bytes. Output: the spans, in order.
fn spans(line: &str) -> Vec<&str> {
    assert!(line.len() <= LINE_BYTES_MAX);
    let bytes = line.as_bytes();
    let run_at = |at: usize| {
        let rest = &bytes[at..];
        rest.iter().take_while(|byte| **byte == b'`').count()
    };
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let run = run_at(at);
        let start = at + run;
        let mut close = start;
        let mut end = None;
        while close < bytes.len() {
            if bytes[close] != b'`' {
                close += 1;
                continue;
            }
            let length = run_at(close);
            if length == run {
                end = Some(close);
                break;
            }
            close += length;
        }
        match end {
            Some(end) => {
                found.push(line[start..end].trim());
                at = end + run;
            }
            None => at = start,
        }
    }
    assert!(found.len() <= bytes.len() / 2);
    found
}

/// Whether a byte may stand in a skill or MCP server name: a lower case letter, a digit, a
/// dash or an underscore. Input: the byte. Output: the answer.
fn name_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
}

/// Whether a part of a span reads as a skill or MCP server name: it starts with a letter or
/// a digit and holds only name bytes. Input: the part. Output: the answer.
fn named(part: &str) -> bool {
    let first = part.bytes().next();
    first.is_some_and(|byte| byte.is_ascii_alphanumeric()) && part.bytes().all(name_byte)
}

/// Whether a word reads as a program name: a lower case letter first, then lower case
/// letters, digits, dots, underscores, pluses or dashes. Input: the word. Output: the
/// answer.
fn program_name(word: &str) -> bool {
    let first = word.bytes().next();
    let rest = |byte: u8| name_byte(byte) || byte == b'.' || byte == b'+';
    first.is_some_and(|byte| byte.is_ascii_lowercase()) && word.bytes().all(rest)
}

/// Reads what a code span names from its shape alone: `mcp__<server>__<tool>` a tool, a
/// span with spaces a command line led by a program, a path with a slash a file, and on a
/// line that says "skill" a bare or `namespace:` name a skill. Inputs: the span and whether
/// its line says "skill". Output: the reference, none for a span it cannot tell.
fn reference(span: &str, skill_line: bool) -> Option<Reference<'_>> {
    if span.is_empty() || span.len() > NAME_BYTES_MAX {
        return None;
    }
    if let Some(rest) = span.strip_prefix("mcp__") {
        let server = rest.split("__").next()?;
        return named(server).then_some(Reference::Tool(server));
    }
    if span.contains(char::is_whitespace) {
        let program = span.split_whitespace().next()?;
        let shaped = program_name(program) && !BUILTINS.contains(&program);
        return shaped.then_some(Reference::Command(program));
    }
    if span.contains('/') {
        return path(span).map(Reference::File);
    }
    match span.split_once(':') {
        Some((space, name)) if skill_line && named(space) && named(name) => {
            Some(Reference::Skill(name))
        }
        None if skill_line && named(span) => Some(Reference::Skill(span)),
        _ => None,
    }
}

/// Reads a relative path out of a span: a leading `./` and a `:line` or `:line:column` end
/// drop off; an absolute or home path, a URL, a pattern, a variable or a path with `..` is
/// none. Input: the span. Output: the path.
fn path(span: &str) -> Option<&str> {
    let mut path = span.strip_prefix("./").unwrap_or(span);
    for _ in 0..2 {
        if let Some((head, tail)) = path.rsplit_once(':')
            && !tail.is_empty()
            && tail.bytes().all(|byte| byte.is_ascii_digit())
        {
            path = head;
        }
    }
    let odd = |character: char| NOT_IN_PATHS.contains(character);
    let rooted = path.starts_with(['/', '~', '@', '-']);
    let plain = !path.is_empty() && !rooted && !path.contains(odd);
    (plain && path.split('/').all(|part| part != "..")).then_some(path)
}

/// Whether a file name ends in an extension: a dot after at least one other character,
/// then one to eight letters or digits. Input: the name. Output: the answer.
fn has_extension(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let short = (1..=8).contains(&extension.len());
    !stem.is_empty() && short && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Judges one reference against what the check knows. A file is judged only under the
/// project's folder, and only when its first part is there or, for a first part with no
/// dot, its last part has an extension, so a branch name such as `feat/x` never counts; a
/// file whose real place is outside the folder is not judged. Inputs: the span, what it
/// names and what the check knows. Output: the note for a missing one, else none.
fn missing(span: &str, reference: Reference<'_>, known: &Known<'_>) -> Option<String> {
    match reference {
        Reference::Tool(server) => {
            let servers = known.servers?;
            if servers.iter().any(|known| known == server) {
                return None;
            }
            Some(format!(
                "`{span}` names the MCP server {server}, which these agents do not get."
            ))
        }
        Reference::Skill(name) => {
            let skills = known.skills?;
            if skills.iter().any(|known| known == name || known == span) {
                return None;
            }
            Some(format!("`{span}` is not a skill these agents get."))
        }
        Reference::Command(program) => {
            if (known.program)(program) != Some(false) {
                return None;
            }
            Some(format!("`{program}` is not a command on this computer."))
        }
        Reference::File(relative) => {
            let folder = known.folder?;
            let first = relative.split('/').next()?;
            let there = !first.is_empty() && std::fs::symlink_metadata(folder.join(first)).is_ok();
            let last = relative.trim_end_matches('/').rsplit('/').next()?;
            if !there && (first.contains('.') || !has_extension(last)) {
                return None;
            }
            match folder.join(relative).canonicalize() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let note = format!("`{span}` is not a file or folder in this project.");
                    Some(note)
                }
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests;
