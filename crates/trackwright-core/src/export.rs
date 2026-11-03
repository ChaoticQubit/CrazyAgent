use crate::criteria;
use crate::host::{Host, Paths};
use crate::import;
use crate::model::{
    Actor, Cycle, CycleStatus, Evidence, OpKind, ProjectKey, ReleaseVersion, ShortId, Ticket, codes,
};
use crate::ops::{self, Cx, Failure};
use crate::store::{self, StoreError};
use rusqlite::{Row, Transaction};
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The first words of every export. `write` replaces only a file that opens with them.
pub const EXPORT_HEADER: &str = "# Trackwright export: ";
/// The largest export the core builds or writes.
pub const EXPORT_BYTES_MAX: usize = 16 * 1024 * 1024;
/// File names that harnesses load as agent instructions. An export never takes one, in any
/// case and in any folder a host allows, because its text holds agent-written titles.
pub const INSTRUCTION_FILE_NAMES: [&str; 6] = [
    "CLAUDE.md",
    "CLAUDE.local.md",
    "AGENTS.md",
    "AGENTS.override.md",
    "GEMINI.md",
    "copilot-instructions.md",
];

const MARKS: &str = "\\`*_[]<>&~|#!";
const FENCE_TILDES_MIN: usize = 3;
const TEMPORARY_TRIES_MAX: u32 = 16;

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

pub(crate) const OPEN_TICKETS_OF_PROJECT: &str = "SELECT number FROM tk_tickets \
     WHERE project = ?1 AND status NOT IN ('done', 'cancelled') ORDER BY number";
pub(crate) const CYCLES_OF_PROJECT: &str = "SELECT id, project, version_id, number, starts_day, \
     ends_day, status, version FROM tk_cycles WHERE project = ?1 ORDER BY number";
pub(crate) const LINK_TARGETS_OF_TICKET: &str =
    "SELECT kind, target FROM tk_links WHERE ticket = ?1 ORDER BY kind, target";

struct Plan {
    versions: Vec<ReleaseVersion>,
    cycles: Vec<Cycle>,
}

impl Plan {
    fn read(tx: &Transaction<'_>, project: i64) -> Result<Self, StoreError> {
        let cycles = store::prepare(tx, CYCLES_OF_PROJECT)?
            .query_map([project], cycle_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(cycles.iter().all(|cycle| cycle.project == project));
        Ok(Self {
            versions: import::versions(tx, project)?,
            cycles,
        })
    }

    fn version(&self, id: Option<i64>) -> Option<(usize, &ReleaseVersion)> {
        let id = id?;
        let found = self
            .versions
            .iter()
            .enumerate()
            .find(|(_, version)| version.id == id);
        assert!(
            found.is_some(),
            "a ticket's version is one of its project's"
        );
        found
    }

    fn cycle(&self, id: Option<i64>) -> Option<(usize, &Cycle)> {
        let id = id?;
        let found = self
            .cycles
            .iter()
            .enumerate()
            .find(|(_, cycle)| cycle.id == id);
        assert!(found.is_some(), "a ticket's cycle is one of its project's");
        found
    }

    fn rank(&self, ticket: &Ticket) -> (usize, usize, u32) {
        let version = self
            .version(ticket.version_id)
            .map_or(usize::MAX, |(at, _)| at);
        let cycle = self.cycle(ticket.cycle_id).map_or(usize::MAX, |(at, _)| at);
        (version, cycle, ticket.short_id.number())
    }

    fn version_heading(&self, ticket: &Ticket) -> String {
        match self.version(ticket.version_id) {
            None => "\n## No version\n".to_owned(),
            Some((_, version)) => format!(
                "\n## Version {} ({}, {})\n",
                inline(&version.name),
                version.status,
                version
                    .target_day
                    .as_deref()
                    .map_or("no target".to_owned(), |day| format!(
                        "target {}",
                        inline(day)
                    ))
            ),
        }
    }

    fn cycle_heading(&self, ticket: &Ticket) -> String {
        match self.cycle(ticket.cycle_id) {
            None => "\n### No cycle\n".to_owned(),
            Some((_, cycle)) => format!(
                "\n### Cycle {} ({} to {}, {})\n",
                cycle.number,
                inline(&cycle.starts_day),
                inline(&cycle.ends_day),
                cycle.status
            ),
        }
    }

    fn place(&self, ticket: &Ticket) -> String {
        let version = self
            .version(ticket.version_id)
            .map_or("no version".to_owned(), |(_, version)| {
                format!("version {}", inline(&version.name))
            });
        let cycle = self
            .cycle(ticket.cycle_id)
            .map_or("no cycle".to_owned(), |(_, cycle)| {
                format!("cycle {}", cycle.number)
            });
        format!("- {version}, {cycle}\n")
    }
}

/// Renders a project's open tickets as Markdown, by version (in roadmap order, then none),
/// cycle (by number, then none) and ticket number, each with its criteria, checks, latest
/// evidence and links. The text holds no time, escapes every stored text and starts no line
/// with one, so the same state always gives the same bytes and stored text never shapes
/// the document. Inputs: the transaction and the project. Output: the
/// text, or `project_unknown` or `export_too_large`.
pub fn project(tx: &Transaction<'_>, key: &ProjectKey) -> Result<String, StoreError> {
    let owner = store::project(tx, key)?;
    let plan = Plan::read(tx, owner.id)?;
    let numbers = store::prepare(tx, OPEN_TICKETS_OF_PROJECT)?
        .query_map([owner.id], |row| row.get::<_, u32>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tickets = Vec::with_capacity(numbers.len());
    for number in numbers {
        tickets.push(store::ticket(tx, &ShortId::new(key.clone(), number)?)?);
    }
    tickets.sort_by_key(|ticket| plan.rank(ticket));
    let mut out = format!(
        "{EXPORT_HEADER}project {key}\n\nOpen tickets of {}, by version and cycle. The tracker \
         writes this file and never reads it back.\n",
        inline(&owner.name)
    );
    let mut group = None;
    for ticket in &tickets {
        assert!(!ticket.status.is_closed());
        let at = (ticket.version_id, ticket.cycle_id);
        if group.map(|(version, _)| version) != Some(ticket.version_id) {
            out.push_str(&plan.version_heading(ticket));
        }
        if group != Some(at) {
            out.push_str(&plan.cycle_heading(ticket));
        }
        group = Some(at);
        out.push_str(&format!(
            "\n#### {}: {}\n\n",
            ticket.short_id,
            inline(&ticket.title)
        ));
        out.push_str(&facts(ticket));
        out.push_str(&proof(tx, ticket)?);
        out.push_str(&links(tx, ticket)?);
        bounded(&out)?;
    }
    Ok(out)
}

/// Renders one ticket of any status as Markdown: its title, facts, version and cycle, body,
/// criteria with checks and latest evidence, links and close reason, with the body and the
/// reason fenced and every other stored text escaped. Inputs: the transaction and the
/// ticket. Output: the text, or `ticket_unknown` or `export_too_large`.
pub fn ticket(tx: &Transaction<'_>, id: &ShortId) -> Result<String, StoreError> {
    let ticket = store::ticket(tx, id)?;
    let plan = Plan::read(tx, ticket.project)?;
    assert_eq!(ticket.closed_by.is_some(), ticket.status.is_closed());
    let mut out = format!(
        "{EXPORT_HEADER}ticket {}\n\n{}: {}\n\n",
        ticket.short_id,
        ticket.short_id,
        inline(&ticket.title)
    );
    out.push_str(&facts(&ticket));
    out.push_str(&plan.place(&ticket));
    out.push_str(&format!(
        "- created by {}\n",
        inline(ticket.created_by.as_str())
    ));
    if let Some(by) = &ticket.closed_by {
        out.push_str(&format!(
            "- closed as {} by {}\n",
            ticket.status,
            inline(by.as_str())
        ));
    }
    if !ticket.body.is_empty() {
        out.push_str("\n## Body\n\n");
        out.push_str(&fenced(&ticket.body));
    }
    let criteria = proof(tx, &ticket)?;
    if !criteria.is_empty() {
        out.push_str("\n## Criteria\n\n");
        out.push_str(&criteria);
    }
    let links = links(tx, &ticket)?;
    if !links.is_empty() {
        out.push_str("\n## Links\n\n");
        out.push_str(&links);
    }
    if let Some(reason) = &ticket.close_reason {
        out.push_str("\n## Close reason\n\n");
        out.push_str(&fenced(reason));
    }
    bounded(&out)?;
    Ok(out)
}

/// Writes an export to the `.md` file the host confines the requested path to, through a
/// private temporary file renamed over it, so a link at the path is never followed. Refuses
/// a path the host refuses, an agent instruction file name, a path that is not `.md`, a
/// folder that is a link or missing, a link or folder at the path, and a file there that is
/// not an export. Inputs: the
/// transaction, the host, the actor, the project, the requested path and the export.
/// Output: Ok, or `export_path_refused`, the host's refusal or `store_failed`.
pub fn write(
    tx: &Transaction<'_>,
    host: &impl Paths,
    actor: &Actor,
    key: &ProjectKey,
    requested: &Path,
    text: &str,
) -> Result<(), StoreError> {
    assert!(text.starts_with(EXPORT_HEADER), "only an export is written");
    let path = host.resolve_path(tx, key, actor, OpKind::Export, requested)?;
    if instruction_file(&path) {
        return Err(path_refused(
            "an export never takes the name of an agent instruction file",
        ));
    }
    write_file(&path, text)
}

fn instruction_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            INSTRUCTION_FILE_NAMES
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))
        })
}

fn write_file(path: &Path, text: &str) -> Result<(), StoreError> {
    assert!(text.starts_with(EXPORT_HEADER), "only an export is written");
    assert!(text.len() <= EXPORT_BYTES_MAX);
    if !import::markdown(path) {
        return Err(path_refused("an export is written to a .md file"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| path_refused("the export path names no file"))?;
    let folder = match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => Path::new("."),
    };
    let listed = fs::symlink_metadata(folder)
        .map_err(|_| path_refused("the export folder does not exist"))?;
    if !listed.file_type().is_dir() {
        return Err(path_refused("the export folder is a link or not a folder"));
    }
    match fs::symlink_metadata(path) {
        Ok(listed) => {
            if !listed.file_type().is_file() {
                return Err(path_refused(
                    "the export path is a link or not a plain file",
                ));
            }
            if !is_export(path, &listed)? {
                return Err(path_refused(
                    "the file at the export path is not an export; only an export is replaced",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed("checked", &error)),
    }
    let (staged, mut file) = temporary(folder, name)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let written = written.and_then(|()| fs::rename(&staged, path));
    if let Err(error) = written {
        let removed = fs::remove_file(&staged);
        let what = if removed.is_ok() {
            "written"
        } else {
            "written, and its temporary file remains"
        };
        return Err(failed(what, &error));
    }
    Ok(())
}

fn facts(ticket: &Ticket) -> String {
    let assignee = ticket
        .assignee
        .as_ref()
        .map_or("nobody".to_owned(), |actor| inline(actor.as_str()));
    format!(
        "- {}, {}, priority {}, assigned to {assignee}\n",
        ticket.kind,
        ticket.status,
        ticket.priority.get()
    )
}

fn proof(tx: &Transaction<'_>, ticket: &Ticket) -> Result<String, StoreError> {
    let mut out = String::new();
    for criterion in criteria::criteria_of(tx, ticket.id)? {
        assert_eq!(criterion.ticket, ticket.id);
        out.push_str(&format!(
            "- criterion {}, {}: {}\n",
            criterion.position,
            criterion.state,
            inline(&criterion.text)
        ));
        for check in criteria::checks_of(tx, criterion.id)? {
            assert_eq!(check.criterion, criterion.id);
            let latest = criteria::latest_of(tx, check.id)?;
            out.push_str(&format!(
                "  - {} check {}: {}\n",
                check.kind,
                inline(&check.reference),
                evidence(latest.as_ref())
            ));
        }
    }
    Ok(out)
}

fn evidence(latest: Option<&Evidence>) -> String {
    let Some(latest) = latest else {
        return "no evidence".to_owned();
    };
    let recorded = format!(
        "{} recorded by {}",
        latest.result,
        inline(latest.recorded_by.as_str())
    );
    if latest.reference.is_empty() {
        return recorded;
    }
    format!("{recorded}: {}", inline(&latest.reference))
}

fn links(tx: &Transaction<'_>, ticket: &Ticket) -> Result<String, StoreError> {
    let mut out = String::new();
    let mut statement = store::prepare(tx, LINK_TARGETS_OF_TICKET)?;
    let rows = statement.query_map([ticket.id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (kind, target) = row?;
        out.push_str(&format!("- link {}: {}\n", inline(&kind), inline(&target)));
    }
    Ok(out)
}

fn inline(text: &str) -> String {
    assert!(
        !text.contains(['\n', '\r']),
        "a one-line field holds no line break"
    );
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for character in text.chars() {
        if MARKS.contains(character) {
            out.push('\\');
        }
        out.push(character);
    }
    assert!(out.len() >= text.len());
    out
}

fn fenced(text: &str) -> String {
    let longest = text
        .split(|character: char| character != '~')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "~".repeat(longest.max(FENCE_TILDES_MIN - 1) + 1);
    assert!(fence.len() >= FENCE_TILDES_MIN);
    assert!(fence.len() > longest);
    let mut out = format!("{fence}\n{text}");
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
    out
}

fn bounded(out: &str) -> Result<(), StoreError> {
    if out.len() <= EXPORT_BYTES_MAX {
        return Ok(());
    }
    Err(store::refusal(
        codes::EXPORT_TOO_LARGE,
        format!("the export is over {EXPORT_BYTES_MAX} bytes; export one ticket at a time"),
    ))
}

fn is_export(path: &Path, listed: &fs::Metadata) -> Result<bool, StoreError> {
    let file = fs::File::open(path).map_err(|error| failed("checked", &error))?;
    let opened = file.metadata().map_err(|error| failed("checked", &error))?;
    if !import::same_file(listed, &opened) {
        return Err(path_refused("the export path changed while it was checked"));
    }
    let mut start = Vec::with_capacity(EXPORT_HEADER.len());
    file.take(EXPORT_HEADER.len() as u64)
        .read_to_end(&mut start)
        .map_err(|error| failed("checked", &error))?;
    assert!(start.len() <= EXPORT_HEADER.len());
    Ok(start == EXPORT_HEADER.as_bytes())
}

fn temporary(folder: &Path, name: &OsStr) -> Result<(PathBuf, fs::File), StoreError> {
    for _ in 0..TEMPORARY_TRIES_MAX {
        let mut temporary = OsString::from(".");
        temporary.push(name);
        temporary.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = folder.join(temporary);
        match create_private(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failed("created", &error)),
        }
    }
    Err(store::refusal(
        codes::STORE_FAILED,
        "no free temporary name beside the export",
    ))
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn cycle_row(row: &Row<'_>) -> rusqlite::Result<Cycle> {
    Ok(Cycle {
        id: row.get(0)?,
        project: row.get(1)?,
        version_id: row.get(2)?,
        number: row.get(3)?,
        starts_day: row.get(4)?,
        ends_day: row.get(5)?,
        status: store::named(row, 6, CycleStatus::parse)?,
        version: row.get(7)?,
    })
}

fn path_refused(message: &str) -> StoreError {
    store::refusal(codes::EXPORT_PATH_REFUSED, message)
}

fn failed(what: &str, error: &io::Error) -> StoreError {
    store::refusal(
        codes::STORE_FAILED,
        format!("the export could not be {what}: {}", error.kind()),
    )
}

/// Runs `export` for dispatch: the project's open tickets, or the one ticket `id` names,
/// written to `path` when given and carried in the reply otherwise, so an export longer
/// than one reply needs a path. It reads the store and writes no event, so even with a path
/// it is a read: the file is the host's, confined by its Paths, and a rewrite replaces it
/// whole. Input: the context. Output: the text, or the path written and its length.
pub(crate) fn export<H: Host>(cx: &Cx<'_, '_, H>) -> Result<ops::Outcome, Failure> {
    let key = cx.project();
    let text = match cx.args.get::<&ShortId>("id") {
        Some(id) => ticket(cx.tx, id)?,
        None => project(cx.tx, key)?,
    };
    assert!(text.starts_with(EXPORT_HEADER));
    let data = match cx.args.get::<&str>("path") {
        Some(requested) => {
            write(cx.tx, cx.host, cx.actor, key, Path::new(requested), &text)?;
            json!({"path": requested, "bytes": text.len()})
        }
        None => ops::whole(
            cx,
            json!({"text": text}),
            codes::EXPORT_TOO_LARGE,
            "give a path to write the export to a file",
        )?,
    };
    Ok(ops::Outcome {
        data,
        next: Vec::new(),
        cursor: None,
    })
}
