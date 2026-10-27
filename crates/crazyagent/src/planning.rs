use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::project_setup::CONTEXT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::questions::{Ask, Step};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::Path;

pub mod prompt;

/// The roadmap a planning job writes beside the brief, relative to the context folder's parent.
pub const ROADMAP: &str = "context/roadmap.md";
/// The release process a planning job writes beside the brief.
pub const RELEASE_PROCESS: &str = "context/release-process.md";
/// The most open points one plan raises as owner questions, the questions log's per-asker cap.
pub const OPEN_POINTS_MAX: usize = 8;
/// The largest open point raised as a question, the questions log's question bound.
pub const OPEN_POINT_BYTES_MAX: usize = crate::questions::QUESTION_BYTES_MAX;
/// The largest roadmap the owner answers are added to.
pub const ROADMAP_BYTES_MAX: usize = 4 * 1024 * 1024;
/// The heading the owner's answers to planning questions are listed under in the roadmap.
pub const ANSWERS_HEADING: &str = "## Owner answers";
/// The largest agent reply a plan is read from.
pub const REPLY_BYTES_MAX: usize = 512 * 1024;
/// The job failure when the reply has no roadmap.
pub const NO_ROADMAP: &str = "Agent returned no roadmap";
/// The job failure when the reply has no release process.
pub const NO_RELEASE_PROCESS: &str = "Agent returned no release process";
/// The job failure when the documents cannot be written beside the brief.
pub const UNWRITABLE: &str = "Could not save the roadmap";
/// The job failure when the plan's questions could not be logged.
pub const QUESTIONS_UNLOGGED: &str = "Could not log the roadmap's open questions";
/// The job failure when the research reports a plan names as its sources cannot be read.
pub const SOURCES_UNREADABLE: &str = "Could not read the research reports";
/// The words opening both planning documents, naming who wrote them.
pub const PROVENANCE_MARK: &str = "Written by the CrazyAgent engine";
/// The job failure when the reply is larger than `REPLY_BYTES_MAX`.
pub const REPLY_TOO_LARGE: &str = "Agent output exceeded the limit";

/// The SQL condition, over a `research_jobs` row, that holds a roadmap planning job back while
/// any other job of its project still waits or runs; every other job passes.
pub(crate) const AFTER_RESEARCH: &str = "(research_jobs.kind <> 'roadmap_planning' OR NOT EXISTS (SELECT 1 FROM research_jobs AS earlier WHERE earlier.project = research_jobs.project AND earlier.kind <> 'roadmap_planning' AND earlier.status IN ('queued','needs_pool','running')))";

const QUESTION_CONTEXT: &str = "Raised by the roadmap planning job. The roadmap in context/roadmap.md lists it under Open questions, and your answer is added there.";
const ROADMAP_FILE: &str = "roadmap.md";
const RELEASE_FILE: &str = "release-process.md";
const ROADMAP_HEADING: &str = "# Roadmap";
const RELEASE_HEADING: &str = "# Release process";
const OPEN_HEADING: &str = "# Open questions";
const OPEN_QUESTIONS_HEADING: &str = "## Open questions";
const ENGINE_SECTIONS: [&str; 4] = [
    "roadmap",
    "release process",
    "open questions",
    "owner answers",
];

/// A planning agent's reply split into its parts: the roadmap body, the release process body
/// and every open point in order, each as the agent wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub roadmap: String,
    pub release_process: String,
    pub open_points: Vec<String>,
}

/// The part of a planning reply a line belongs to while it is split.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Preface,
    Roadmap,
    Release,
    Open,
}

impl Plan {
    /// The open points raised as owner questions: hidden characters shown escaped, then the
    /// first `OPEN_POINTS_MAX` within `OPEN_POINT_BYTES_MAX`. Every point stays in the roadmap.
    /// Takes the plan. Returns the question texts in order.
    pub fn questions(&self) -> Vec<String> {
        let questions: Vec<String> = self
            .open_points
            .iter()
            .map(|point| crate::memory::visible_text(point.as_str()))
            .filter(|point| point.len() <= OPEN_POINT_BYTES_MAX)
            .take(OPEN_POINTS_MAX)
            .collect();
        assert!(questions.len() <= OPEN_POINTS_MAX);
        questions
    }
}

/// Splits a planning agent's reply at its `# Roadmap`, `# Release process` and
/// `# Open questions` lines, matched whole and without case; text before the first is
/// dropped and a repeated heading continues its section. Open points are `- `, `* ` or
/// `1. ` lines, a following plain line continuing the point; "None" is no point.
/// Takes the reply. Returns the plan, or the job failure when the roadmap or release
/// process is empty.
pub fn parse(reply: &str) -> Result<Plan, &'static str> {
    let mut section = Section::Preface;
    let (mut roadmap, mut release) = (Vec::new(), Vec::new());
    let mut open_points: Vec<String> = Vec::new();
    for line in reply.lines() {
        let heading = line.trim();
        let next = [
            (ROADMAP_HEADING, Section::Roadmap),
            (RELEASE_HEADING, Section::Release),
            (OPEN_HEADING, Section::Open),
        ]
        .into_iter()
        .find(|(text, _)| heading.eq_ignore_ascii_case(text));
        if let Some((_, next)) = next {
            section = next;
            continue;
        }
        match section {
            Section::Preface => {}
            Section::Roadmap => roadmap.push(line),
            Section::Release => release.push(line),
            Section::Open => {
                if let Some(point) = bullet(heading) {
                    open_points.push(point.to_owned());
                } else if let Some(last) = open_points.last_mut()
                    && !heading.is_empty()
                {
                    last.push(' ');
                    last.push_str(heading);
                }
            }
        }
    }
    open_points.retain(|point| {
        let point = point.trim().trim_end_matches('.');
        !point.is_empty() && !point.eq_ignore_ascii_case("none")
    });
    let roadmap = roadmap.join("\n").trim().to_owned();
    let release_process = release.join("\n").trim().to_owned();
    if roadmap.is_empty() {
        return Err(NO_ROADMAP);
    }
    if release_process.is_empty() {
        return Err(NO_RELEASE_PROCESS);
    }
    Ok(Plan {
        roadmap,
        release_process,
        open_points,
    })
}

/// The text of a list line: after `- `, `* ` or a number with `. `.
/// Takes a trimmed line. Returns the point text, or none for a plain line.
fn bullet(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return Some(rest.trim());
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    line[digits..].strip_prefix(". ").map(str::trim)
}

/// Renders the two documents: the roadmap with every open point under "Open questions",
/// and the release process, each headed by its provenance (engine-written from an agent's
/// reply, its sources, not reviewed by the owner), with hidden characters shown escaped so no
/// agent reading the context sees text the owner cannot, and every agent line that could pass
/// for engine text escaped by `inert`.
/// Takes the plan, the harness label, the date and the research report paths the plan was made
/// from. Returns the roadmap and release process.
pub fn render(plan: &Plan, harness: &str, date: &str, sources: &[String]) -> (String, String) {
    let visible = crate::memory::visible_text;
    let body = |text: &str| inert(&visible(text));
    let provenance = provenance(harness, date, sources);
    let mut roadmap = format!(
        "{ROADMAP_HEADING}\n\n{provenance}\n\n{}\n\n{OPEN_QUESTIONS_HEADING}\n\n",
        body(&plan.roadmap)
    );
    if plan.open_points.is_empty() {
        roadmap.push_str("None.\n");
    }
    for point in &plan.open_points {
        roadmap.push_str(&format!("- {}\n", body(point)));
    }
    let release = format!(
        "{RELEASE_HEADING}\n\n{provenance}\n\n{}\n",
        body(&plan.release_process)
    );
    assert!(roadmap.starts_with(ROADMAP_HEADING) && release.starts_with(RELEASE_HEADING));
    (roadmap, release)
}

/// Escapes every line of an agent-written body that could pass for engine text: a heading,
/// ATX or setext at any level, named like an engine section; a line naming the engine as its
/// writer; and a listed `Question <n>:` answer marker. A backslash goes before the line's
/// first mark, so the engine finds its own sections by structure and a reader sees plain text.
/// Takes the body with hidden characters already escaped. Returns the body.
fn inert(body: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = Vec::with_capacity(lines.len());
    for (at, line) in lines.iter().enumerate() {
        let underlined = at > 0 && engine_section(lines[at - 1].trim()) && underline(line);
        if engine_like(line) || underlined {
            let text = line.trim_start();
            out.push(format!("{}\\{text}", &line[..line.len() - text.len()]));
        } else {
            out.push((*line).to_owned());
        }
    }
    let body = out.join("\n");
    assert!(body.split('\n').all(|line| !engine_like(line)));
    body
}

/// Whether a line could pass for engine text: an ATX heading named like an engine section,
/// a line opening with the provenance mark, quoted or not, or a listed `Question <n>:` marker.
/// Takes the line. Returns true when it must be escaped.
fn engine_like(line: &str) -> bool {
    let text = line.trim_start();
    let hashes = text.bytes().take_while(|byte| *byte == b'#').count();
    if hashes > 0 && engine_section(text[hashes..].trim().trim_end_matches('#').trim()) {
        return true;
    }
    let said = text.trim_start_matches(['>', ' ', '\t']);
    if said
        .get(..PROVENANCE_MARK.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(PROVENANCE_MARK))
    {
        return true;
    }
    let listed = text
        .strip_prefix(['-', '*', '+'])
        .or_else(|| {
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0)
                .then_some(&text[digits..])
                .and_then(|rest| rest.strip_prefix(['.', ')']))
        })
        .map(str::trim_start);
    listed.is_some_and(|item| {
        let Some(rest) = item
            .get(.."question ".len())
            .filter(|start| start.eq_ignore_ascii_case("question "))
            .map(|start| &item[start.len()..])
        else {
            return false;
        };
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].trim_start().starts_with(':')
    })
}

/// Whether a heading text, without its marks, names an engine section, ignoring case.
/// Takes the text. Returns true for a reserved name.
fn engine_section(text: &str) -> bool {
    ENGINE_SECTIONS
        .iter()
        .any(|name| text.eq_ignore_ascii_case(name))
}

/// Whether a line is a setext heading underline: only `=` or only `-` marks.
/// Takes the line. Returns true for an underline.
fn underline(line: &str) -> bool {
    let text = line.trim();
    !text.is_empty() && (text.bytes().all(|b| b == b'=') || text.bytes().all(|b| b == b'-'))
}

/// The note heading both planning documents: written by the engine from which agent's reply
/// and when, from which sources, and that it is a proposal the owner has not reviewed.
/// Takes the harness label, the date and the research report paths. Returns the note.
fn provenance(harness: &str, date: &str, sources: &[String]) -> String {
    let mut from = vec![format!("{CONTEXT_DIRECTORY}/brief.md")];
    from.extend(sources.iter().cloned());
    format!(
        "> {PROVENANCE_MARK} on {date} from a {harness} planning agent's reply. \
         Sources: {}. The research came from web pages and the owner has not reviewed this \
         plan, so treat it as a proposal; times are estimates until the team's pace is measured.",
        from.join(", ")
    )
}

/// Writes a planning reply's roadmap and release process beside the brief in
/// `<state>/context/`, each through a new file renamed over the old one, opened beneath the
/// state folder without following links. Takes the project's state folder, the reply and the
/// harness label. Returns the plan, or the job failure.
pub fn publish(state: &Path, reply: &str, harness: &str) -> Result<Plan, &'static str> {
    if reply.len() > REPLY_BYTES_MAX {
        return Err(REPLY_TOO_LARGE);
    }
    let plan = parse(reply)?;
    let sources = prompt::sources(state).map_err(|_| SOURCES_UNREADABLE)?;
    let date = crate::research::runner::utc_date(std::time::SystemTime::now());
    let (roadmap, release) = render(&plan, harness, &date, &sources);
    write_beside_brief(
        state,
        &[
            (ROADMAP_FILE, roadmap.as_str()),
            (RELEASE_FILE, release.as_str()),
        ],
    )?;
    Ok(plan)
}

/// Raises the plan's questions (`Plan::questions`) as `planning` owner questions of the project
/// in one transaction, skipping one whose text a pending planning question already holds, and
/// stopping when the project's questions are full; a refused question stays in the roadmap only.
/// Inputs: the engine database path, the project and the plan. Output: how many were raised.
pub fn raise(database: &Path, project: &str, plan: &Plan) -> Result<usize, EngineError> {
    let root = database
        .parent()
        .ok_or_else(|| failure("questions_unavailable"))?;
    let project = ProjectId::new(project).map_err(|_| failure("question_project_unknown"))?;
    let db = crate::questions::open(root)?;
    let pending: Vec<String> = crate::questions::list(&db, &project, true, None)?
        .into_iter()
        .filter(|question| question.source == crate::questions::Source::Planning)
        .map(|question| question.question)
        .collect();
    let now = now_ms();
    let tx = Transaction::new_unchecked(&db, TransactionBehavior::Immediate)
        .map_err(|_| failure("questions_unavailable"))?;
    let mut raised = 0;
    for question in plan.questions() {
        if pending.contains(&question) {
            continue;
        }
        let ask = Ask {
            question,
            options: Vec::new(),
            recommended: None,
            context: Some(QUESTION_CONTEXT.to_owned()),
        };
        match crate::questions::raise(&tx, &project, Step::Planning, &ask, now)? {
            Ok(_) => raised += 1,
            Err("questions_full") => break,
            Err(_) => {}
        }
    }
    tx.commit().map_err(|_| failure("questions_unavailable"))?;
    assert!(raised <= OPEN_POINTS_MAX);
    Ok(raised)
}

/// Adds the owner's answers to planning questions to each project's roadmap under
/// `ANSWERS_HEADING`, taking each settled question once, so every agent reading the context
/// sees what the owner decided. The research service calls it every tick.
/// Input: the engine database path. Output: how many answers were added, or the first failure
/// after trying every project.
pub fn record_answers(database: &Path) -> Result<usize, EngineError> {
    let root = database
        .parent()
        .ok_or_else(|| failure("questions_unavailable"))?;
    let db = crate::questions::open(root)?;
    let mut recorded = 0;
    let mut first = Ok(());
    for project in crate::questions::untaken(&db, Step::Planning)? {
        match record_project(&db, &project) {
            Ok(count) => recorded += count,
            Err(error) => first = first.and(Err(error)),
        }
    }
    first.map(|()| recorded)
}

/// Takes one project's settled planning questions and adds them to its roadmap in the same
/// transaction, which commits only once the roadmap is written; a project whose setup is gone
/// has its answers taken and dropped. Inputs: the engine connection and the project.
/// Output: how many answers were taken.
fn record_project(db: &Connection, project: &ProjectId) -> Result<usize, EngineError> {
    let asked = crate::questions::list(db, project, false, None)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure("questions_unavailable"))?;
    let taken = crate::questions::take(&tx, project, Step::Planning)?;
    if taken.is_empty() {
        return Ok(0);
    }
    if let Some(stored) = crate::project_setup::load_setup(&tx, project)? {
        let lines: Vec<String> = taken
            .iter()
            .map(|item| {
                let question = asked.iter().find(|question| question.id == item.id);
                answer_line(item.id, question, item.answer.as_deref())
            })
            .collect();
        let state = stored.state_folder();
        let roadmap = read_beside_brief(&state, ROADMAP_FILE).map_err(failure)?;
        let roadmap = with_answers(roadmap.as_deref(), &lines);
        write_beside_brief(&state, &[(ROADMAP_FILE, roadmap.as_str())]).map_err(failure)?;
    }
    tx.commit().map_err(|_| failure("questions_unavailable"))?;
    Ok(taken.len())
}

/// One answer as the roadmap lists it: the question id and text on one line, then the answer
/// labelled by who gave it, or that the owner dismissed it, line breaks folded and hidden
/// characters escaped. Inputs: the id, the question as listed before it was taken, and the
/// answer, none when dismissed. Output: the entry.
fn answer_line(
    id: i64,
    question: Option<&crate::questions::Question>,
    answer: Option<&str>,
) -> String {
    use crate::questions::AnsweredBy;
    let flat = |text: &str| {
        crate::memory::visible_text(&text.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let asked = question.map_or_else(String::new, |asked| format!(" {}", flat(&asked.question)));
    let outcome = match (answer, question.and_then(|asked| asked.answered_by)) {
        (None, _) => "The owner dismissed it.".to_owned(),
        (Some(answer), Some(AnsweredBy::Owner)) => format!("Owner: {}", flat(answer)),
        (Some(answer), Some(AnsweredBy::Master)) => {
            format!("Recorded by the master from chat: {}", flat(answer))
        }
        (Some(answer), Some(AnsweredBy::Terminal)) => {
            format!("Answered in the master's terminal: {}", flat(answer))
        }
        (Some(answer), None) => format!("Answer: {}", flat(answer)),
    };
    format!("- Question {id}:{asked}\n  {outcome}\n")
}

/// Adds answer entries to the engine's answers section of a roadmap: the first
/// `ANSWERS_HEADING` line after the engine's last `## Open questions` line, added at the end
/// when there is none, so a heading the agent's text holds is never taken for it. An entry
/// whose `Question <n>:` marker the section already lists is skipped, so a retried write
/// never repeats one. Inputs: the roadmap, none when missing, and the entries. Output: the text.
fn with_answers(roadmap: Option<&str>, entries: &[String]) -> String {
    let mut text = roadmap.unwrap_or(ROADMAP_HEADING).to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let lines = lines_at(&text);
    let open = lines
        .iter()
        .rfind(|(_, line)| *line == OPEN_QUESTIONS_HEADING)
        .map_or(0, |(at, _)| *at);
    let found = lines
        .iter()
        .find(|(at, line)| *at >= open && *line == ANSWERS_HEADING)
        .map(|(at, _)| *at);
    let answers = found.unwrap_or_else(|| {
        text.push('\n');
        let at = text.len();
        text.push_str(&format!("{ANSWERS_HEADING}\n\n"));
        at
    });
    for entry in entries {
        let marker = entry.split_inclusive(':').next().unwrap_or(entry);
        if !text[answers..].lines().any(|line| line.starts_with(marker)) {
            text.push_str(entry);
        }
    }
    assert!(answers >= open && text[answers..].starts_with(ANSWERS_HEADING));
    text
}

/// Each line of a text with the byte offset it starts at, its line break left off.
/// Takes the text. Returns the offsets and lines in order.
fn lines_at(text: &str) -> Vec<(usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n')
        .map(|line| {
            let start = at;
            at += line.len();
            (start, line.trim_end_matches('\n'))
        })
        .collect()
}

fn now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(elapsed).unwrap_or(i64::MAX)
}

/// Opens `<state>/context` as a directory handle, refusing a link at either name.
/// Takes the state folder. Returns the handle or `UNWRITABLE`.
#[cfg(unix)]
fn context_folder(state: &Path) -> Result<rustix::fd::OwnedFd, &'static str> {
    use rustix::fs::{Mode, OFlags};
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = rustix::fs::open(state, directory, Mode::empty()).map_err(|_| UNWRITABLE)?;
    let context = crate::project_setup::CONTEXT_DIRECTORY;
    rustix::fs::openat(&folder, context, directory, Mode::empty()).map_err(|_| UNWRITABLE)
}

/// Reads one document beside the brief without following a link at its name, at most
/// `ROADMAP_BYTES_MAX` bytes. Takes the state folder and file name. Returns the text, none
/// when missing, or `UNWRITABLE` for a link, a non-regular file, an oversized or unreadable one.
#[cfg(unix)]
fn read_beside_brief(state: &Path, name: &str) -> Result<Option<String>, &'static str> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read;
    let context = context_folder(state)?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match rustix::fs::openat(&context, name, flags, Mode::empty()) {
        Ok(file) => std::fs::File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(UNWRITABLE),
    };
    let metadata = file.metadata().map_err(|_| UNWRITABLE)?;
    if !metadata.is_file() || metadata.len() > ROADMAP_BYTES_MAX as u64 {
        return Err(UNWRITABLE);
    }
    let mut text = String::new();
    file.take(ROADMAP_BYTES_MAX as u64)
        .read_to_string(&mut text)
        .map_err(|_| UNWRITABLE)?;
    Ok(Some(text))
}

/// Refuses to read planning documents where no directory-relative no-follow open exists.
/// Takes the state folder and name, both unused. Returns `UNWRITABLE`.
#[cfg(not(unix))]
fn read_beside_brief(_state: &Path, _name: &str) -> Result<Option<String>, &'static str> {
    Err(UNWRITABLE)
}

/// Writes the named documents into `<state>/context/` as a pair: each target must be absent,
/// a regular file or a link; every document goes to a 0644 partial file that is synced, and
/// only once all are written are they renamed over their names, so a link at a name is
/// replaced, never followed, and a failed write leaves the old documents and no partials.
/// Takes the state folder and the file names with their text. Returns `UNWRITABLE` on failure.
#[cfg(unix)]
fn write_beside_brief(state: &Path, documents: &[(&str, &str)]) -> Result<(), &'static str> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::io::Write;
    let context = context_folder(state)?;
    for (name, _) in documents {
        match rustix::fs::statat(&context, *name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat)
                if matches!(
                    FileType::from_raw_mode(stat.st_mode),
                    FileType::RegularFile | FileType::Symlink
                ) => {}
            Err(rustix::io::Errno::NOENT) => {}
            _ => return Err(UNWRITABLE),
        }
    }
    let partials: Vec<String> = documents
        .iter()
        .map(|(name, _)| format!(".{name}.partial"))
        .collect();
    let discard = |partials: &[String]| {
        for partial in partials {
            let _ = rustix::fs::unlinkat(&context, partial.as_str(), AtFlags::empty());
        }
    };
    discard(&partials);
    for (partial, (_, body)) in partials.iter().zip(documents) {
        let created = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let opened = rustix::fs::openat(
            &context,
            partial.as_str(),
            created | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        );
        let written = opened.map_err(std::io::Error::from).and_then(|file| {
            let mut file = std::fs::File::from(file);
            file.write_all(body.as_bytes())
                .and_then(|()| file.sync_all())
        });
        if written.is_err() {
            discard(&partials);
            return Err(UNWRITABLE);
        }
    }
    for (index, (partial, (name, _))) in partials.iter().zip(documents).enumerate() {
        if rustix::fs::renameat(&context, partial.as_str(), &context, *name).is_err() {
            discard(&partials[index..]);
            return Err(UNWRITABLE);
        }
    }
    assert_eq!(partials.len(), documents.len());
    Ok(())
}

/// Refuses to write planning documents where no directory-relative no-follow open exists.
/// Takes the state folder and documents, both unused. Returns `UNWRITABLE`.
#[cfg(not(unix))]
fn write_beside_brief(_state: &Path, _documents: &[(&str, &str)]) -> Result<(), &'static str> {
    Err(UNWRITABLE)
}

#[cfg(all(test, unix))]
mod tests;
