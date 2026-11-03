use crate::events::{self, Recorded};
use crate::host::{Authority, Host, Paths};
use crate::model::{
    Actor, OPEN_TICKETS_CREATED_PER_ACTOR_MAX, OpKind, Priority, ProjectKey, ReleaseVersion,
    Ticket, TicketKind, TicketStatus, VERSIONS_PER_PROJECT_MAX, VersionStatus, codes,
};
use crate::ops::{self, Cx, Failure};
use crate::policy;
use crate::store::{self, NewTicket, StoreError};
use crate::text::{self, TextField};
use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest roadmap or checklist file an import reads.
pub const IMPORT_BYTES_MAX: usize = 1024 * 1024;
/// The most items one preview lists.
pub const IMPORT_ITEMS_MAX: usize = 1024;
/// The most item keys one apply accepts.
pub const IMPORT_ACCEPT_MAX: usize = 256;
/// The `source` of every version an import adds.
pub const IMPORT_SOURCE: &str = "import";

const ITEM_PRIORITY: u8 = 2;
const VERSION_PARTS_MAX: usize = 3;
const VERSION_PART_DIGITS_MAX: usize = 4;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const ROADMAP_TITLE: &str = "# Roadmap";
const CHECKLIST_TITLES: [&str; 2] = ["# Release checklist", "# Release process"];
const SCOPE_LABELS: [&str; 2] = ["scope", "in scope"];
const TARGET_LABEL: &str = "target:";

const IMPORTED_NAME_BYTES_MAX: usize = VERSION_PARTS_MAX * (VERSION_PART_DIGITS_MAX + 1);
const SHORT_ID_BYTES_MAX: usize = 8 + 1 + 10;
const APPLIED_DATA_BYTES_MAX: usize = 32 + IMPORT_ACCEPT_MAX * (SHORT_ID_BYTES_MAX + 3);

const _: () = assert!(IMPORT_ACCEPT_MAX <= IMPORT_ITEMS_MAX);
const _: () = assert!(IMPORTED_NAME_BYTES_MAX <= SHORT_ID_BYTES_MAX);
const _: () = assert!(APPLIED_DATA_BYTES_MAX < 7 * 1024);
const _: () = assert!(VERSIONS_PER_PROJECT_MAX <= IMPORT_ITEMS_MAX);

macro_rules! version_select {
    () => {
        "SELECT id, project, name, position, target_day, status, source, version FROM tk_versions"
    };
}

pub(crate) const VERSIONS_OF_PROJECT: &str = concat!(
    version_select!(),
    " WHERE project = ?1 ORDER BY position, name"
);
pub(crate) const VERSION_BY_ID: &str = concat!(version_select!(), " WHERE id = ?1");
pub(crate) const VERSION_BY_NAME: &str =
    concat!(version_select!(), " WHERE project = ?1 AND name = ?2");
pub(crate) const VERSION_INSERT: &str = "INSERT INTO tk_versions(project, name, position, \
     target_day, status, source, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)";
pub(crate) const VERSION_UPDATE: &str = "UPDATE tk_versions SET position = ?3, target_day = ?4, \
     version = version + 1 WHERE id = ?1 AND version = ?2";
pub(crate) const TICKET_TITLES_OF_PROJECT: &str =
    "SELECT version_id, title FROM tk_tickets WHERE project = ?1";
pub(crate) const TICKET_VERSION_SET: &str = "UPDATE tk_tickets SET version_id = ?2, \
     updated_ms = ?3, version = version + 1 WHERE id = ?1 AND version = ?4";

/// Why a part of the file was not imported: text before the first section, a section that
/// names no version, a checklist section without a checkbox, a target that is not a
/// `YYYY-MM-DD` day, a second target, a second section for a version, or an item whose
/// text the core refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    BeforeFirstSection,
    NotAVersion,
    NoChecklistItem,
    TargetNotADay,
    SecondTarget,
    SecondSection,
    ItemRefused,
}

/// A part of the file the import leaves to a person: the section heading (empty before the
/// first section), its first and last line, why, and its text as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unparsed {
    pub heading: String,
    pub line_first: u32,
    pub line_last: u32,
    pub why: Why,
    pub text: String,
}

/// One change an import would make: a new version, a version's new position or target, or
/// a new backlog ticket in a version (none for a checklist chore).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    VersionAdd {
        name: String,
        position: u32,
        target_day: Option<String>,
    },
    VersionChange {
        name: String,
        position: u32,
        target_day: Option<String>,
        was_position: u32,
        was_target_day: Option<String>,
    },
    TicketAdd {
        version: Option<String>,
        kind: TicketKind,
        title: String,
    },
}

/// A change with its key, a hash of the change's content, so an accepted key applies
/// exactly what the preview showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub key: String,
    pub change: Change,
}

/// What an import of one file would do to a project: the items in file order, and every
/// part it did not import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preview {
    pub items: Vec<Item>,
    pub unparsed: Vec<Unparsed>,
}

/// What an apply wrote: the versions added or changed and the tickets created, in file
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub versions: Vec<ReleaseVersion>,
    pub tickets: Vec<Ticket>,
}

#[derive(Debug, Default)]
struct Parsed {
    versions: Vec<Planned>,
    chores: Vec<String>,
    unparsed: Vec<Unparsed>,
}

#[derive(Debug)]
struct Planned {
    name: String,
    position: u32,
    target_day: Option<String>,
    tickets: Vec<String>,
}

struct Section<'a> {
    heading: &'a str,
    line: u32,
    level: usize,
    body: Vec<(u32, &'a str)>,
}

/// Reads a roadmap or release checklist at the path the host confines the requested one
/// to, and lists what importing it would change, writing nothing. Inputs: the transaction,
/// the host, the actor, the project and the requested file. Output: the preview, or
/// `project_unknown`, the host's refusal or the file's (`import_path_refused`,
/// `import_too_large`, `import_not_text`, `text_hidden_characters`, `import_format_unknown`).
pub fn preview(
    tx: &Transaction<'_>,
    host: &impl Paths,
    actor: &Actor,
    key: &ProjectKey,
    requested: &Path,
) -> Result<Preview, StoreError> {
    let path = host.resolve_path(tx, key, actor, OpKind::ImportPreview, requested)?;
    preview_at(tx, key, &path)
}

fn preview_at(tx: &Transaction<'_>, key: &ProjectKey, path: &Path) -> Result<Preview, StoreError> {
    let project = store::project(tx, key)?;
    let text = read(path)?;
    let parsed = parse(&text)?;
    let items = diff(tx, project.id, &parsed)?;
    assert!(items.len() <= IMPORT_ITEMS_MAX);
    assert!(
        parsed
            .unparsed
            .is_sorted_by_key(|entry| (entry.line_first, entry.line_last))
    );
    Ok(Preview {
        items,
        unparsed: parsed.unparsed,
    })
}

/// Applies the accepted items of a fresh preview of the file in one savepoint: versions
/// first as the file orders them, each ticket after its version. The actor needs
/// `import_apply`, plus `version_upsert` for version items and `create` for tickets, and
/// the new tickets count against its open tickets like a `create`; tickets start in the
/// backlog, or where `create` puts the actor's tickets. Inputs: the transaction, the host,
/// the actor, the project, the requested file, the accepted keys and the time. Output: what
/// was written, or `import_item_unknown`, `import_item_needs_version`, `versions_full`,
/// `open_tickets_full`, the host's, the file's or the policy's refusal.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    tx: &Transaction<'_>,
    host: &(impl Authority + Paths),
    actor: &Actor,
    key: &ProjectKey,
    requested: &Path,
    accept: &[String],
    now_ms: i64,
) -> Result<Applied, StoreError> {
    if accept.len() > IMPORT_ACCEPT_MAX {
        return Err(store::refusal(
            codes::IMPORT_TOO_LARGE,
            format!("one apply accepts at most {IMPORT_ACCEPT_MAX} items"),
        ));
    }
    let project = store::project(tx, key)?;
    host.actor_may(tx, key, actor, OpKind::ImportApply, None)?;
    let path: PathBuf = host.resolve_path(tx, key, actor, OpKind::ImportApply, requested)?;
    let preview = preview_at(tx, key, &path)?;
    let wanted: BTreeSet<&str> = accept.iter().map(String::as_str).collect();
    let chosen: Vec<&Change> = preview
        .items
        .iter()
        .filter(|item| wanted.contains(item.key.as_str()))
        .map(|item| &item.change)
        .collect();
    if chosen.len() != wanted.len() {
        return Err(store::refusal(
            codes::IMPORT_ITEM_UNKNOWN,
            "an accepted key is not in this file's preview; preview the file again",
        ));
    }
    authorize(tx, host, actor, key, &chosen)?;
    placed(tx, project.id, &chosen)?;
    within_open_bound(tx, actor, &chosen)?;
    store::write(tx, || {
        let start = policy::start_status(&policy::load(tx, key)?, actor, TicketStatus::Backlog);
        let mut applied = Applied {
            versions: Vec::new(),
            tickets: Vec::new(),
        };
        for change in &chosen {
            match change {
                Change::VersionAdd {
                    name,
                    position,
                    target_day,
                } => applied.versions.push(add_version(
                    tx,
                    actor,
                    project.id,
                    name,
                    *position,
                    target_day.as_deref(),
                    now_ms,
                )?),
                Change::VersionChange {
                    name,
                    position,
                    target_day,
                    ..
                } => applied.versions.push(change_version(
                    tx,
                    actor,
                    project.id,
                    name,
                    *position,
                    target_day.as_deref(),
                    now_ms,
                )?),
                Change::TicketAdd {
                    version,
                    kind,
                    title,
                } => applied.tickets.push(add_ticket(
                    tx,
                    actor,
                    key,
                    version.as_deref(),
                    *kind,
                    start,
                    title,
                    now_ms,
                )?),
            }
        }
        assert_eq!(applied.versions.len() + applied.tickets.len(), chosen.len());
        Ok(applied)
    })
}

/// A project's versions by position, then name. Inputs: the transaction and the project's
/// row id. Output: the versions.
pub(crate) fn versions(
    tx: &Transaction<'_>,
    project: i64,
) -> Result<Vec<ReleaseVersion>, StoreError> {
    let versions = store::prepare(tx, VERSIONS_OF_PROJECT)?
        .query_map([project], version_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(versions.len() <= VERSIONS_PER_PROJECT_MAX);
    assert!(versions.iter().all(|version| version.project == project));
    Ok(versions)
}

/// Tells whether a path names a `.md` file, the only kind the core reads or writes. Input:
/// the path. Output: true for a `.md` extension in any case.
pub(crate) fn markdown(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Tells whether a file opened by path is the one its link-free listing showed, so a link
/// swapped in between is caught. Inputs: the listing and the opened file's metadata.
/// Output: true when both name the same plain file.
#[cfg(unix)]
pub(crate) fn same_file(listed: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    opened.file_type().is_file() && listed.dev() == opened.dev() && listed.ino() == opened.ino()
}

/// Tells whether a file opened by path is still a plain file. Inputs: the listing and the
/// opened file's metadata. Output: true for a plain file.
#[cfg(not(unix))]
pub(crate) fn same_file(listed: &fs::Metadata, opened: &fs::Metadata) -> bool {
    listed.file_type().is_file() && opened.file_type().is_file()
}

fn read(path: &Path) -> Result<String, StoreError> {
    if !markdown(path) {
        return Err(path_refused("an import reads a .md file"));
    }
    let listed =
        fs::symlink_metadata(path).map_err(|_| path_refused("no file to import at the path"))?;
    if !listed.file_type().is_file() {
        return Err(path_refused(
            "the import path is a link or not a plain file",
        ));
    }
    if listed.len() > IMPORT_BYTES_MAX as u64 {
        return Err(too_large());
    }
    let file = fs::File::open(path).map_err(|error| failed(&error))?;
    let opened = file.metadata().map_err(|error| failed(&error))?;
    if !same_file(&listed, &opened) {
        return Err(path_refused("the import path changed while it was read"));
    }
    let mut bytes = Vec::with_capacity(listed.len() as usize);
    file.take(IMPORT_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| failed(&error))?;
    if bytes.len() > IMPORT_BYTES_MAX {
        return Err(too_large());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| store::refusal(codes::IMPORT_NOT_TEXT, "the import file is not UTF-8"))?
        .replace("\r\n", "\n");
    if text::hidden(&text) {
        return Err(store::refusal(
            codes::TEXT_HIDDEN_CHARACTERS,
            "the import file holds characters a reader cannot see or that reorder what they see",
        ));
    }
    assert!(text.len() <= IMPORT_BYTES_MAX);
    Ok(text)
}

fn parse(text: &str) -> Result<Parsed, StoreError> {
    let lines: Vec<&str> = text.split('\n').collect();
    assert!(lines.len() <= IMPORT_BYTES_MAX + 1);
    let title = lines.iter().position(|line| !line.trim().is_empty());
    let checklist = match title.map(|at| lines[at].trim()) {
        Some(ROADMAP_TITLE) => false,
        Some(heading) if CHECKLIST_TITLES.contains(&heading) => true,
        _ => {
            return Err(store::refusal(
                codes::IMPORT_FORMAT_UNKNOWN,
                "an import file opens with \"# Roadmap\", \"# Release checklist\" or \
                 \"# Release process\"",
            ));
        }
    };
    let (preface, sections) = sections(&lines, title.map_or(0, |at| at + 1));
    let mut parsed = Parsed::default();
    if let Some(entry) = preface_entry(&preface) {
        parsed.unparsed.push(entry);
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for section in &sections {
        if section.level != 2 {
            parsed.unparsed.push(whole(section, Why::NotAVersion));
        } else if checklist {
            checklist_section(section, &mut parsed);
        } else {
            roadmap_section(section, &mut parsed, &mut seen);
        }
    }
    parsed
        .unparsed
        .sort_by_key(|entry| (entry.line_first, entry.line_last));
    Ok(parsed)
}

fn sections<'a>(lines: &[&'a str], from: usize) -> (Vec<(u32, &'a str)>, Vec<Section<'a>>) {
    let mut preface = Vec::new();
    let mut sections: Vec<Section<'a>> = Vec::new();
    for (at, &line) in lines.iter().enumerate().skip(from) {
        let number = u32::try_from(at + 1).expect("a bounded file has few lines");
        let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=2).contains(&hashes) && line[hashes..].starts_with(' ') {
            sections.push(Section {
                heading: line[hashes..].trim(),
                line: number,
                level: hashes,
                body: Vec::new(),
            });
        } else if let Some(section) = sections.last_mut() {
            section.body.push((number, line));
        } else {
            preface.push((number, line));
        }
    }
    (preface, sections)
}

fn preface_entry(preface: &[(u32, &str)]) -> Option<Unparsed> {
    let kept: Vec<(u32, &str)> = preface
        .iter()
        .copied()
        .filter(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with('>'))
        .collect();
    let (first, last) = (kept.first()?.0, kept.last()?.0);
    assert!(first <= last);
    Some(Unparsed {
        heading: String::new(),
        line_first: first,
        line_last: last,
        why: Why::BeforeFirstSection,
        text: kept
            .iter()
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

fn whole(section: &Section<'_>, why: Why) -> Unparsed {
    let written: Vec<&(u32, &str)> = section
        .body
        .iter()
        .skip_while(|(_, line)| line.trim().is_empty())
        .collect();
    let end = written
        .iter()
        .rposition(|(_, line)| !line.trim().is_empty())
        .map_or(0, |at| at + 1);
    let written = &written[..end];
    let line_last = written.last().map_or(section.line, |(number, _)| *number);
    assert!(line_last >= section.line);
    Unparsed {
        heading: section.heading.to_owned(),
        line_first: section.line,
        line_last,
        why,
        text: written
            .iter()
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn line_entry(section: &Section<'_>, number: u32, line: &str, why: Why) -> Unparsed {
    assert!(number > section.line);
    Unparsed {
        heading: section.heading.to_owned(),
        line_first: number,
        line_last: number,
        why,
        text: line.to_owned(),
    }
}

fn roadmap_section(section: &Section<'_>, parsed: &mut Parsed, seen: &mut BTreeSet<String>) {
    let Some(name) = version_name(section.heading) else {
        parsed.unparsed.push(whole(section, Why::NotAVersion));
        return;
    };
    if !seen.insert(name.to_owned()) {
        parsed.unparsed.push(whole(section, Why::SecondSection));
        return;
    }
    let position = u32::try_from(parsed.versions.len()).expect("versions are bounded");
    let mut planned = Planned {
        name: name.to_owned(),
        position,
        target_day: None,
        tickets: Vec::new(),
    };
    let (mut scope, mut targeted) = (false, false);
    for &(number, line) in &section.body {
        let trimmed = line.trim();
        if trimmed.is_empty() || line.starts_with([' ', '\t']) {
            continue;
        }
        let lowered = trimmed.to_lowercase();
        if lowered.starts_with('#') {
            let label = lowered.trim_start_matches('#').trim().trim_end_matches(':');
            scope = SCOPE_LABELS.contains(&label.trim());
            continue;
        }
        if scope && let Some(item) = bullet(line) {
            match text::check(TextField::Title, item) {
                Ok(()) if planned.tickets.iter().any(|title| title == item) => {}
                Ok(()) => planned.tickets.push(item.to_owned()),
                Err(_) => parsed
                    .unparsed
                    .push(line_entry(section, number, line, Why::ItemRefused)),
            }
            continue;
        }
        scope = false;
        if let Some(day) = lowered.strip_prefix(TARGET_LABEL) {
            let day = day.trim();
            let why = match (targeted, is_day(day)) {
                (false, true) => None,
                (false, false) => Some(Why::TargetNotADay),
                (true, _) => Some(Why::SecondTarget),
            };
            if why.is_none() {
                planned.target_day = Some(day.to_owned());
            }
            targeted = true;
            if let Some(why) = why {
                parsed.unparsed.push(line_entry(section, number, line, why));
            }
        } else if let Some(label) = lowered.strip_suffix(':') {
            scope = SCOPE_LABELS.contains(&label.trim());
        }
    }
    assert!(planned.target_day.as_deref().is_none_or(is_day));
    parsed.versions.push(planned);
}

fn checklist_section(section: &Section<'_>, parsed: &mut Parsed) {
    let mut found = false;
    for &(number, line) in &section.body {
        let Some(rest) = line
            .strip_prefix("- [")
            .or_else(|| line.strip_prefix("* ["))
        else {
            continue;
        };
        let (mark, item) = match rest.split_once("] ") {
            Some((mark, item)) if [" ", "x", "X"].contains(&mark) => (mark, item.trim()),
            _ => continue,
        };
        found = true;
        if mark != " " {
            continue;
        }
        match text::check(TextField::Title, item) {
            Ok(()) if parsed.chores.iter().any(|title| title == item) => {}
            Ok(()) => parsed.chores.push(item.to_owned()),
            Err(_) => parsed
                .unparsed
                .push(line_entry(section, number, line, Why::ItemRefused)),
        }
    }
    if !found {
        parsed.unparsed.push(whole(section, Why::NoChecklistItem));
    }
}

fn bullet(line: &str) -> Option<&str> {
    let item = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let item = item.trim();
    (!item.is_empty()).then_some(item)
}

fn version_name(heading: &str) -> Option<&str> {
    let name = heading
        .split_once(':')
        .map_or(heading, |(name, _)| name)
        .trim();
    let digits = name.strip_prefix(['V', 'v']).unwrap_or(name);
    let parts: Vec<&str> = digits.split('.').collect();
    let valid = parts.len() <= VERSION_PARTS_MAX
        && parts.iter().all(|part| {
            (1..=VERSION_PART_DIGITS_MAX).contains(&part.len())
                && part.bytes().all(|byte| byte.is_ascii_digit())
        });
    if !valid {
        return None;
    }
    assert!(text::check(TextField::VersionName, name).is_ok());
    Some(name)
}

fn is_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &text[range];
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10)) else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days_max).contains(&day)
}

fn diff(tx: &Transaction<'_>, project: i64, parsed: &Parsed) -> Result<Vec<Item>, StoreError> {
    let stored = versions(tx, project)?;
    let titles: BTreeSet<(Option<i64>, String)> = store::prepare(tx, TICKET_TITLES_OF_PROJECT)?
        .query_map([project], |row| {
            Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut changes = Vec::new();
    for planned in &parsed.versions {
        let known = stored.iter().find(|version| version.name == planned.name);
        if let Some(change) = version_change(planned, known) {
            changes.push(change);
        }
        for title in &planned.tickets {
            let exists =
                known.is_some_and(|version| titles.contains(&(Some(version.id), title.clone())));
            if !exists {
                changes.push(Change::TicketAdd {
                    version: Some(planned.name.clone()),
                    kind: TicketKind::Feature,
                    title: title.clone(),
                });
            }
        }
    }
    for title in &parsed.chores {
        if !titles.contains(&(None, title.clone())) {
            changes.push(Change::TicketAdd {
                version: None,
                kind: TicketKind::Chore,
                title: title.clone(),
            });
        }
    }
    keyed(changes)
}

fn version_change(planned: &Planned, known: Option<&ReleaseVersion>) -> Option<Change> {
    let Some(known) = known else {
        return Some(Change::VersionAdd {
            name: planned.name.clone(),
            position: planned.position,
            target_day: planned.target_day.clone(),
        });
    };
    assert_eq!(known.name, planned.name);
    let target_day = planned
        .target_day
        .clone()
        .or_else(|| known.target_day.clone());
    if known.position == planned.position && known.target_day == target_day {
        return None;
    }
    Some(Change::VersionChange {
        name: planned.name.clone(),
        position: planned.position,
        target_day,
        was_position: known.position,
        was_target_day: known.target_day.clone(),
    })
}

fn keyed(changes: Vec<Change>) -> Result<Vec<Item>, StoreError> {
    if changes.len() > IMPORT_ITEMS_MAX {
        return Err(store::refusal(
            codes::IMPORT_TOO_LARGE,
            format!("the file would make over {IMPORT_ITEMS_MAX} changes; split it"),
        ));
    }
    let items: Vec<Item> = changes
        .into_iter()
        .map(|change| Item {
            key: item_key(&change),
            change,
        })
        .collect();
    let keys: BTreeSet<&str> = items.iter().map(|item| item.key.as_str()).collect();
    if keys.len() != items.len() {
        return Err(store::refusal(
            codes::IMPORT_KEYS_COLLIDE,
            "two items of the file share a key; rename one of them",
        ));
    }
    Ok(items)
}

fn item_key(change: &Change) -> String {
    let (prefix, identity) = match change {
        Change::VersionAdd {
            name,
            position,
            target_day,
        }
        | Change::VersionChange {
            name,
            position,
            target_day,
            ..
        } => (
            "version",
            format!(
                "{name}\0{position}\0{}",
                target_day.as_deref().unwrap_or("")
            ),
        ),
        Change::TicketAdd {
            version,
            kind,
            title,
        } => (
            "ticket",
            format!("{}\0{kind}\0{title}", version.as_deref().unwrap_or("")),
        ),
    };
    let hash = identity.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    });
    let key = format!("{prefix}-{hash:016x}");
    assert!(key.len() <= 24);
    assert!(key.is_ascii());
    key
}

fn authorize(
    tx: &Transaction<'_>,
    host: &impl Authority,
    actor: &Actor,
    key: &ProjectKey,
    chosen: &[&Change],
) -> Result<(), StoreError> {
    let tickets = chosen.iter().any(|change| is_ticket(change));
    let versions = chosen.iter().any(|change| !is_ticket(change));
    assert!(chosen.is_empty() || tickets || versions);
    if versions {
        host.actor_may(tx, key, actor, OpKind::VersionUpsert, None)?;
    }
    if tickets {
        host.actor_may(tx, key, actor, OpKind::Create, None)?;
    }
    Ok(())
}

fn within_open_bound(
    tx: &Transaction<'_>,
    actor: &Actor,
    chosen: &[&Change],
) -> Result<(), StoreError> {
    let added = chosen.iter().filter(|change| is_ticket(change)).count();
    assert!(added <= IMPORT_ACCEPT_MAX);
    if added == 0 {
        return Ok(());
    }
    let open_max = OPEN_TICKETS_CREATED_PER_ACTOR_MAX as i64;
    let open: i64 = store::prepare(tx, ops::OPEN_CREATED)?
        .query_row(params![actor.as_str(), open_max], |row| row.get(0))?;
    assert!((0..=open_max).contains(&open));
    if open + added as i64 > open_max {
        return Err(store::refusal(
            codes::OPEN_TICKETS_FULL,
            format!(
                "{actor} has {open} open tickets it created and the bound is {open_max}; \
                 accept at most {} ticket items, or close some first",
                open_max - open
            ),
        ));
    }
    Ok(())
}

fn is_ticket(change: &Change) -> bool {
    matches!(change, Change::TicketAdd { .. })
}

fn placed(tx: &Transaction<'_>, project: i64, chosen: &[&Change]) -> Result<(), StoreError> {
    let stored = versions(tx, project)?;
    let added: BTreeSet<&str> = chosen
        .iter()
        .filter_map(|change| match change {
            Change::VersionAdd { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for change in chosen {
        let Change::TicketAdd {
            version: Some(name),
            ..
        } = change
        else {
            continue;
        };
        if !added.contains(name.as_str()) && !stored.iter().any(|version| &version.name == name) {
            return Err(store::refusal(
                codes::IMPORT_ITEM_NEEDS_VERSION,
                format!("a ticket of version {name} needs that version's item accepted too"),
            ));
        }
    }
    if stored.len() + added.len() > VERSIONS_PER_PROJECT_MAX {
        return Err(store::refusal(
            codes::VERSIONS_FULL,
            format!("a project holds at most {VERSIONS_PER_PROJECT_MAX} versions"),
        ));
    }
    Ok(())
}

fn add_version(
    tx: &Transaction<'_>,
    actor: &Actor,
    project: i64,
    name: &str,
    position: u32,
    target_day: Option<&str>,
    now_ms: i64,
) -> Result<ReleaseVersion, StoreError> {
    assert!(text::check(TextField::VersionName, name).is_ok());
    store::prepare(tx, VERSION_INSERT)?.execute(params![
        project,
        name,
        position,
        target_day,
        VersionStatus::Planned.as_str(),
        IMPORT_SOURCE,
    ])?;
    let added = version_by_id(tx, tx.last_insert_rowid())?;
    assert_eq!(added.name, name);
    assert_eq!(added.version, 1);
    events::record(
        tx,
        Recorded {
            project,
            ticket: None,
            op: events::VERSION_UPSERT,
            actor,
            at_ms: now_ms,
            before: None,
            after: Some(events::snapshot(&added)),
        },
    )?;
    Ok(added)
}

fn change_version(
    tx: &Transaction<'_>,
    actor: &Actor,
    project: i64,
    name: &str,
    position: u32,
    target_day: Option<&str>,
    now_ms: i64,
) -> Result<ReleaseVersion, StoreError> {
    let before = store::prepare(tx, VERSION_BY_NAME)?
        .query_row(params![project, name], version_row)
        .optional()?
        .expect("a version change names a stored version");
    let changed = store::prepare(tx, VERSION_UPDATE)?.execute(params![
        before.id,
        before.version,
        position,
        target_day
    ])?;
    assert_eq!(
        changed, 1,
        "the preview read this version in the same transaction"
    );
    let after = version_by_id(tx, before.id)?;
    assert_eq!(after.version, before.version + 1);
    events::record(
        tx,
        Recorded {
            project,
            ticket: None,
            op: events::VERSION_UPSERT,
            actor,
            at_ms: now_ms,
            before: Some(events::snapshot(&before)),
            after: Some(events::snapshot(&after)),
        },
    )?;
    Ok(after)
}

#[allow(clippy::too_many_arguments)]
fn add_ticket(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    version: Option<&str>,
    kind: TicketKind,
    status: TicketStatus,
    title: &str,
    now_ms: i64,
) -> Result<Ticket, StoreError> {
    assert!(!status.is_closed());
    let new = NewTicket {
        title: title.to_owned(),
        body: String::new(),
        kind,
        status,
        priority: Priority::new(ITEM_PRIORITY)?,
    };
    let created = store::create_ticket(tx, actor, key, &new, now_ms)?;
    let Some(name) = version else {
        return Ok(created);
    };
    let owner = store::prepare(tx, VERSION_BY_NAME)?
        .query_row(params![created.project, name], version_row)
        .optional()?
        .expect("a ticket's version is stored before the ticket");
    let changed = store::prepare(tx, TICKET_VERSION_SET)?.execute(params![
        created.id,
        owner.id,
        now_ms,
        created.version
    ])?;
    assert_eq!(changed, 1, "a ticket just created is at its first version");
    let placed = store::ticket_by_id(tx, created.id)?;
    assert_eq!(placed.version_id, Some(owner.id));
    assert_eq!(placed.version, created.version + 1);
    events::record(
        tx,
        Recorded {
            project: created.project,
            ticket: Some(created.id),
            op: events::TICKET_UPDATE,
            actor,
            at_ms: now_ms,
            before: Some(events::snapshot(&created)),
            after: Some(events::snapshot(&placed)),
        },
    )?;
    Ok(placed)
}

fn version_by_id(tx: &Transaction<'_>, id: i64) -> Result<ReleaseVersion, StoreError> {
    Ok(store::prepare(tx, VERSION_BY_ID)?.query_row([id], version_row)?)
}

fn version_row(row: &Row<'_>) -> rusqlite::Result<ReleaseVersion> {
    Ok(ReleaseVersion {
        id: row.get(0)?,
        project: row.get(1)?,
        name: row.get(2)?,
        position: row.get(3)?,
        target_day: row.get(4)?,
        status: store::named(row, 5, VersionStatus::parse)?,
        source: row.get(6)?,
        version: row.get(7)?,
    })
}

fn path_refused(message: &str) -> StoreError {
    store::refusal(codes::IMPORT_PATH_REFUSED, message)
}

fn too_large() -> StoreError {
    store::refusal(
        codes::IMPORT_TOO_LARGE,
        format!("an import file is at most {IMPORT_BYTES_MAX} bytes"),
    )
}

fn failed(error: &std::io::Error) -> StoreError {
    store::refusal(
        codes::STORE_FAILED,
        format!("the import file cannot be read: {}", error.kind()),
    )
}

/// Runs `import_preview` for dispatch. A preview longer than one reply is refused; the
/// file is split, or previewed with a larger reply bound. Input: the context. Output: the
/// preview's items and unparsed parts.
pub(crate) fn import_preview<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let requested = Path::new(cx.args.need::<&str>("path"));
    let found = preview(cx.tx, cx.host, cx.actor, cx.project(), requested)?;
    assert!(found.items.len() <= IMPORT_ITEMS_MAX);
    let data = serde_json::to_value(&found).expect("a preview is plain data");
    Ok(ops::Outcome {
        data: ops::whole(
            cx,
            data,
            codes::IMPORT_TOO_LARGE,
            "split the file, or ask with a larger reply bound",
        )?,
        next: Vec::new(),
        cursor: None,
    })
}

/// Runs `import_apply` for dispatch. The reply names each version and ticket only, so even
/// the largest apply (`IMPORT_ACCEPT_MAX` items, each the longest name or id) fits the
/// smallest reply bound; `roadmap` and `show` read the rest. Input: the context. Output:
/// the names of the versions added or changed and the ids of the tickets created, in file
/// order.
pub(crate) fn import_apply<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let requested = Path::new(cx.args.need::<&str>("path"));
    let accept = cx.args.need::<&[String]>("accept");
    let applied = apply(
        cx.tx,
        cx.host,
        cx.actor,
        cx.project(),
        requested,
        accept,
        cx.at_ms,
    )?;
    assert!(applied.versions.len() + applied.tickets.len() <= accept.len());
    assert!(accept.len() <= IMPORT_ACCEPT_MAX);
    let tickets: Vec<String> = applied
        .tickets
        .iter()
        .map(|ticket| ticket.short_id.to_string())
        .collect();
    assert!(tickets.iter().all(|id| id.len() <= SHORT_ID_BYTES_MAX));
    let versions: Vec<&str> = applied
        .versions
        .iter()
        .map(|version| {
            assert!(version.name.len() <= IMPORTED_NAME_BYTES_MAX);
            version.name.as_str()
        })
        .collect();
    let data = json!({"versions": versions, "tickets": tickets});
    assert!(APPLIED_DATA_BYTES_MAX <= cx.data_bytes_max);
    assert!(data.to_string().len() <= APPLIED_DATA_BYTES_MAX);
    Ok(ops::Outcome {
        data,
        next: Vec::new(),
        cursor: None,
    })
}
