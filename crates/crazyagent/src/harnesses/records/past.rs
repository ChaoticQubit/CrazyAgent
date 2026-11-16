use super::super::drivers::rules::{self, Matched};
use super::super::manifest::{self, Fact, Manifest, RecordKind, Refused, Rule};
use super::super::transcript::{Speaker, claude_message, codex_message};
use super::json_files::Pattern;
use super::json_rewrite::DOCUMENT_BYTES_MAX;
use super::markdown_history::{SESSION_HEADER, USER_HEADING};
use crate::adapters::{LINE_BYTES_MAX, clip_text};
use crate::app_plugins::yaml::Value as Yaml;
use crate::model_gateway::relay::{TOKEN_HEX, TOKEN_PREFIX, is_relay_token};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The most past conversation records one import reads.
pub const RECORDS_MAX: usize = 4096;
/// The most folder entries, and names looked up, one import's search of the owner's folders
/// takes.
pub const ENTRIES_SCANNED_MAX: usize = 65_536;
/// The most bytes one import reads of one record; a longer record is read up to here.
pub const RECORD_BYTES_MAX: u64 = 64 * 1024 * 1024;
/// The most messages one imported conversation keeps.
pub const MESSAGES_MAX: usize = 4096;
/// The most bytes one import reads in all.
pub const IMPORT_BYTES_MAX: u64 = 256 * 1024 * 1024;
/// A record that could not be opened as a plain file below the owner's folders.
pub const UNREADABLE: &str = "import_record_unreadable";
/// A whole-document record larger than one import reads of a record.
pub const TOO_LARGE: &str = "import_record_too_large";
/// A record with no message whose lines or document could not be read as JSON or text.
pub const BROKEN: &str = "import_record_broken";
/// A record that could be read and holds no message.
pub const EMPTY: &str = "import_record_empty";
/// What stands in imported text for a relay token, `crazyagent-relay-` and 64 lowercase hex
/// digits, which a tool the harness ran may have printed into a conversation.
pub const RELAY_REDACTED: &str = "crazyagent-relay-[redacted]";
const KEY: &str = "conversations";
const SETTING: &str = "import.conversations";
const RULES: &str = "terminal.record.rules";

/// How much one import reads: records, folder entries scanned, bytes of one record, bytes of
/// one line, messages of one conversation and bytes in all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    records: usize,
    entries: usize,
    record_bytes: u64,
    line_bytes: usize,
    messages: usize,
    total_bytes: u64,
}

impl Bounds {
    const DEFAULT: Self = Self {
        records: RECORDS_MAX,
        entries: ENTRIES_SCANNED_MAX,
        record_bytes: RECORD_BYTES_MAX,
        line_bytes: LINE_BYTES_MAX,
        messages: MESSAGES_MAX,
        total_bytes: IMPORT_BYTES_MAX,
    };
}

/// One message of an imported conversation: its place in the conversation from 0, who said
/// it and its text, clipped to `FIELD_BYTES_MAX`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub ordinal: usize,
    pub speaker: Speaker,
    pub text: String,
}

/// One past conversation read from one record: the record's place below the owner's home,
/// the working folder the record names, its last change, its messages in order, and whether
/// a bound clipped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub record: PathBuf,
    pub cwd: Option<String>,
    pub modified: SystemTime,
    pub messages: Vec<Message>,
    pub clipped: bool,
}

/// One record an import passed over: its place below the owner's home and the reason code,
/// which [`sentence`] says in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub record: PathBuf,
    pub reason: &'static str,
}

/// What one import read: the conversations and the skipped records, each in record order,
/// and whether a bound on records, folder entries or bytes in all stopped it early.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub conversations: Vec<Conversation>,
    pub skipped: Vec<Skipped>,
    pub cut: bool,
}

/// How the lines of a JSON lines record give messages: Claude Code session lines, Codex
/// rollout lines, or objects read by the manifest's record rules.
enum Said {
    Claude,
    Codex,
    Ruled(Vec<Rule>),
}

/// How one harness's records give messages, by the record kind its manifest names.
enum Kind {
    Lines(Said),
    Document(Vec<Rule>),
    Markdown,
}

/// What one record gave so far: its messages in order, the working folder it names,
/// whether a bound clipped it, whether a line or its document could not be read, and the
/// bytes read.
#[derive(Debug, Default)]
struct Taken {
    messages: Vec<(Speaker, String)>,
    cwd: Option<String>,
    clipped: bool,
    broken: bool,
    bytes: u64,
}

/// How reading one record ended: with what it gave, skipped for a reason, or stopped
/// because the bytes left in all are fewer than its whole document.
enum Outcome {
    Read(Taken),
    Skipped(&'static str),
    Cut,
}

/// Reads the past conversations of one harness from the owner's own harness folders, only
/// reading them, by the records its manifest's `import.conversations` pattern matches.
/// Inputs: the manifest and the absolute owner home. Output: what was read, nothing without
/// the key, or a refusal that names the setting.
pub fn import(manifest: &Manifest, owner_home: &Path) -> Result<Imported, Refused> {
    import_within(manifest, owner_home, &Bounds::DEFAULT)
}

/// [`import`] within the given bounds. The import block is checked as the registry checks
/// it, the records are listed in order of place without following a link, and each is
/// opened as itself, read only. Inputs: the manifest, the absolute owner home and the
/// bounds. Output: as [`import`].
fn import_within(
    manifest: &Manifest,
    owner_home: &Path,
    bounds: &Bounds,
) -> Result<Imported, Refused> {
    assert!(owner_home.is_absolute());
    crate::skills::import::check(manifest)?;
    let text = match manifest.block("import").and_then(|import| import.get(KEY)) {
        None | Some(Yaml::Null) => return Ok(Imported::default()),
        Some(Yaml::Str(text)) => text,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                format!("{SETTING}: this must be text in quotes."),
            );
        }
    };
    let kind = match manifest.record {
        RecordKind::ClaudeSession => Kind::Lines(Said::Claude),
        RecordKind::CodexRollouts => Kind::Lines(Said::Codex),
        RecordKind::JsonlAppend => Kind::Lines(Said::Ruled(record_rules(manifest)?)),
        RecordKind::JsonRewrite => Kind::Document(record_rules(manifest)?),
        RecordKind::MarkdownHistory => Kind::Markdown,
        RecordKind::SqlitePoll | RecordKind::Screen | RecordKind::Unavailable => {
            return refuse(
                manifest::KIND,
                format!(
                    "{SETTING}: this harness's terminal record keeps no past conversations \
                     CrazyAgent reads back."
                ),
            );
        }
    };
    let pattern = Pattern::owned(owner_home, text)?;
    let (listed, limit_reached) = pattern.listed(bounds.entries);
    let mut imported = Imported {
        cut: limit_reached || listed.len() > bounds.records,
        ..Imported::default()
    };
    let mut total = 0;
    for (record, modified) in listed.into_iter().take(bounds.records) {
        if total >= bounds.total_bytes {
            imported.cut = true;
            break;
        }
        let budget = bounds.record_bytes.min(bounds.total_bytes - total);
        let outcome = match pattern.open(&record) {
            Some((file, _)) => read(&kind, file, budget, bounds),
            None => Outcome::Skipped(UNREADABLE),
        };
        let taken = match outcome {
            Outcome::Read(taken) => taken,
            Outcome::Skipped(reason) => {
                imported.skipped.push(Skipped { record, reason });
                continue;
            }
            Outcome::Cut => {
                imported.cut = true;
                break;
            }
        };
        assert!(taken.bytes <= budget);
        total += taken.bytes;
        if taken.messages.is_empty() {
            let reason = if taken.broken { BROKEN } else { EMPTY };
            imported.skipped.push(Skipped { record, reason });
            continue;
        }
        imported
            .conversations
            .push(conversation(record, modified, taken));
    }
    assert!(total <= bounds.total_bytes);
    assert!(imported.conversations.len() + imported.skipped.len() <= bounds.records);
    Ok(imported)
}

/// Reads the rule set `terminal.record.rules` names. Input: the manifest. Output: the
/// rules, or a refusal that names `terminal.record.rules` or the refused rule.
fn record_rules(manifest: &Manifest) -> Result<Vec<Rule>, Refused> {
    let written = manifest
        .block("terminal")
        .and_then(|terminal| terminal.get("record"))
        .and_then(|record| record.get("rules"));
    match written {
        None | Some(Yaml::Null) => refuse(
            manifest::MISSING,
            format!("{RULES}: this setting is needed to import past conversations."),
        ),
        Some(Yaml::Str(set)) => rules::read(manifest, set),
        Some(_) => refuse(
            manifest::TYPE,
            format!("{RULES}: this must be text in quotes."),
        ),
    }
}

/// Reads one opened record, at most `budget` bytes of it. Inputs: how its kind gives
/// messages, the file, the budget and the bounds. Output: what it gave, marked clipped when
/// the record is longer than the budget, or why it was skipped or stopped.
fn read(kind: &Kind, file: File, budget: u64, bounds: &Bounds) -> Outcome {
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let most = bounds.messages;
    let mut taken = match kind {
        Kind::Document(rules) => return document(rules, file, length, budget, bounds),
        Kind::Lines(said) => lines_of(file, budget, bounds.line_bytes, |taken, line| {
            taken.json(said, line, most)
        }),
        Kind::Markdown => {
            let mut open = None;
            let mut taken = lines_of(file, budget, bounds.line_bytes, |taken, line| {
                taken.markdown(line, &mut open, most)
            });
            taken.push(open, most);
            taken
        }
    };
    taken.clipped |= length > budget;
    Outcome::Read(taken)
}

/// Reads a record line by line, at most `budget` bytes in all and each line at most
/// `line_bytes` bytes; a longer line is passed over to its end and marks the record clipped,
/// a blank line is passed over, and a last line the budget cut is not kept and marks it
/// clipped. Inputs: the file, the budget, the line bound and what to do with each line.
/// Output: what it gave.
fn lines_of(
    file: File,
    budget: u64,
    line_bytes: usize,
    mut each: impl FnMut(&mut Taken, &[u8]),
) -> Taken {
    let mut taken = Taken::default();
    let mut reader = BufReader::new(file.take(budget));
    let mut line = Vec::new();
    loop {
        line.clear();
        let longest = line_bytes as u64 + 1;
        let Ok(count) = reader.by_ref().take(longest).read_until(b'\n', &mut line) else {
            break;
        };
        if count == 0 {
            break;
        }
        taken.bytes += count as u64;
        let ended = line.last() == Some(&b'\n');
        if !ended && line.len() > line_bytes {
            taken.clipped = true;
            match reader.skip_until(b'\n') {
                Ok(skipped) => taken.bytes += skipped as u64,
                Err(_) => break,
            }
            continue;
        }
        if !ended && taken.bytes == budget {
            taken.clipped = true;
            break;
        }
        let text = line.strip_suffix(b"\n").unwrap_or(&line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if !text.trim_ascii().is_empty() {
            each(&mut taken, text);
        }
    }
    assert!(taken.bytes <= budget);
    taken
}

/// Reads a record that is one JSON array, whose object entries the record rules read once
/// its relay tokens are redacted. Inputs: the rules, the file, its length, the bytes left for
/// it and the bounds. Output: what it gave; skipped as too large past `DOCUMENT_BYTES_MAX` or
/// the record bound, as unreadable, or stopped when fewer bytes are left in all.
fn document(rules: &[Rule], file: File, length: u64, budget: u64, bounds: &Bounds) -> Outcome {
    let most = (DOCUMENT_BYTES_MAX as u64).min(bounds.record_bytes);
    if length > most {
        return Outcome::Skipped(TOO_LARGE);
    }
    if length > budget {
        return Outcome::Cut;
    }
    let mut bytes = Vec::new();
    if file.take(most + 1).read_to_end(&mut bytes).is_err() {
        return Outcome::Skipped(UNREADABLE);
    }
    let read = bytes.len() as u64;
    if read > most {
        return Outcome::Skipped(TOO_LARGE);
    }
    if read > budget {
        return Outcome::Cut;
    }
    let mut taken = Taken {
        bytes: read,
        ..Taken::default()
    };
    let parsed = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&redacted(text)).ok());
    match parsed {
        Some(Value::Array(entries)) => {
            for entry in entries.iter().filter(|entry| entry.is_object()) {
                let said = rules::matched(rules, entry).and_then(spoken);
                taken.push(said, bounds.messages);
            }
        }
        _ => taken.broken = true,
    }
    Outcome::Read(taken)
}

impl Taken {
    /// Keeps one message, unless the conversation already holds `most`, which clips it.
    /// Inputs: the message, if any, and the bound.
    fn push(&mut self, said: Option<(Speaker, String)>, most: usize) {
        let Some(said) = said else {
            return;
        };
        if self.messages.len() < most {
            self.messages.push(said);
        } else {
            self.clipped = true;
        }
        assert!(self.messages.len() <= most);
    }

    /// Reads one JSON line: a line that is not JSON marks the record broken; otherwise the
    /// working folder the record names is kept from its first line that has one, and the
    /// message it gives is kept. A line the record rules read has its relay tokens redacted
    /// first, so their clip never cuts one. Inputs: how the lines give messages, the line
    /// and the bound on messages.
    fn json(&mut self, said: &Said, line: &[u8], most: usize) {
        let parsed = match said {
            Said::Ruled(_) => std::str::from_utf8(line)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&redacted(text)).ok()),
            Said::Claude | Said::Codex => serde_json::from_slice::<Value>(line).ok(),
        };
        let Some(value) = parsed else {
            self.broken = true;
            return;
        };
        let message = match said {
            Said::Claude => {
                if self.cwd.is_none() {
                    self.cwd = value["cwd"].as_str().map(str::to_owned);
                }
                claude_message(&value)
            }
            Said::Codex => {
                if self.cwd.is_none() && value["type"] == "session_meta" {
                    self.cwd = value["payload"]["cwd"].as_str().map(str::to_owned);
                }
                codex_message(&value)
            }
            Said::Ruled(rules) => rules::matched(rules, &value).and_then(spoken),
        };
        self.push(message, most);
    }

    /// Reads one Markdown chat history line: a `#### ` line is typed text, a session header
    /// ends the open message, a `>` note gives nothing, and any other line is reply text.
    /// Lines of one speaker in a row join into one message. A line that is not UTF-8 marks
    /// the record broken. Inputs: the line, the open message and the bound on messages.
    fn markdown(&mut self, line: &[u8], open: &mut Option<(Speaker, String)>, most: usize) {
        let Ok(text) = std::str::from_utf8(line) else {
            self.broken = true;
            return;
        };
        if text.starts_with(SESSION_HEADER) {
            self.push(open.take(), most);
            return;
        }
        let (speaker, part) = if text == USER_HEADING || text.starts_with("#### ") {
            (Speaker::Input, text[USER_HEADING.len()..].trim_start())
        } else if text.starts_with('>') {
            return;
        } else {
            (Speaker::Agent, text)
        };
        if part.is_empty() {
            return;
        }
        match open {
            Some((now, kept)) if *now == speaker => {
                kept.push('\n');
                kept.push_str(part);
            }
            _ => {
                let done = open.replace((speaker, part.to_owned()));
                self.push(done, most);
            }
        }
    }
}

/// The message one record rule found: a typed prompt from a `turn_started` rule's text, a
/// reply from a `text` rule's. Input: what the rule found. Output: who said what, or none
/// for another fact or no text.
fn spoken(found: Matched) -> Option<(Speaker, String)> {
    let speaker = match found.fact {
        Fact::TurnStarted => Speaker::Input,
        Fact::Text => Speaker::Agent,
        _ => return None,
    };
    found.text.map(|text| (speaker, text))
}

/// Builds one conversation from what its record gave: each text with its relay tokens
/// redacted and then clipped, each message numbered from 0 in order. Inputs: the record's
/// place, its last change and what it gave. Output: the conversation.
fn conversation(record: PathBuf, modified: SystemTime, taken: Taken) -> Conversation {
    let mut clipped = taken.clipped;
    let messages = taken
        .messages
        .into_iter()
        .enumerate()
        .map(|(ordinal, (speaker, text))| {
            let whole = redacted(&text);
            let text = clip_text(&whole);
            clipped |= text.len() != whole.len();
            Message {
                ordinal,
                speaker,
                text,
            }
        })
        .collect();
    Conversation {
        record,
        cwd: taken.cwd.as_deref().map(redacted),
        modified,
        messages,
        clipped,
    }
}

/// Replaces each relay token, `crazyagent-relay-` and 64 lowercase hex digits, in one
/// imported text with `RELAY_REDACTED`, whatever follows it; a shorter or upper-case run
/// stays as written. Input: the text. Output: the text, never longer.
fn redacted(text: &str) -> String {
    let length = TOKEN_PREFIX.len() + TOKEN_HEX;
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(TOKEN_PREFIX) {
        kept.push_str(&rest[..at]);
        let found = &rest[at..];
        if found.get(..length).is_some_and(is_relay_token) {
            kept.push_str(RELAY_REDACTED);
            rest = &found[length..];
        } else {
            kept.push_str(TOKEN_PREFIX);
            rest = &found[TOKEN_PREFIX.len()..];
        }
    }
    kept.push_str(rest);
    assert!(kept.len() <= text.len());
    kept
}

/// The plain sentence of one skip reason. Input: the code. Output: the sentence, or none for
/// a code that is not a skip reason.
pub fn sentence(code: &str) -> Option<&'static str> {
    match code {
        UNREADABLE => Some(
            "This record could not be opened as a plain file in the harness's folders, so it \
             was skipped.",
        ),
        TOO_LARGE => Some("This record is larger than CrazyAgent reads, so it was skipped."),
        BROKEN => Some("Nothing in this record reads as a conversation, so it was skipped."),
        EMPTY => Some("This record holds no messages, so it was skipped."),
        _ => None,
    }
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

#[cfg(all(test, unix))]
#[path = "past_tests.rs"]
mod tests;
