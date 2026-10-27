use crate::engine::EngineError;
use crate::engine::owner::failure;
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
/// and the release process, each headed by who planned it and when, with hidden characters
/// shown escaped so no agent reading the context sees text the owner cannot.
/// Takes the plan, the harness label and the date. Returns the roadmap and release process.
pub fn render(plan: &Plan, harness: &str, date: &str) -> (String, String) {
    let visible = crate::memory::visible_text;
    let mut roadmap = format!(
        "{ROADMAP_HEADING}\n\nPlanned by {harness} on {date} from the brief and the research. \
         Times are estimates until the team's pace is measured.\n\n{}\n\n## Open questions\n\n",
        visible(&plan.roadmap)
    );
    if plan.open_points.is_empty() {
        roadmap.push_str("None.\n");
    }
    for point in &plan.open_points {
        roadmap.push_str(&format!("- {}\n", visible(point)));
    }
    let release = format!(
        "{RELEASE_HEADING}\n\nPlanned by {harness} on {date}.\n\n{}\n",
        visible(&plan.release_process)
    );
    assert!(roadmap.starts_with(ROADMAP_HEADING) && release.starts_with(RELEASE_HEADING));
    (roadmap, release)
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
    let date = crate::research::runner::utc_date(std::time::SystemTime::now());
    let (roadmap, release) = render(&plan, harness, &date);
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
                let question = asked
                    .iter()
                    .find(|question| question.id == item.id)
                    .map(|question| question.question.as_str());
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

/// One answer as the roadmap lists it: the question id and text on one line, then the owner's
/// answer or that the owner dismissed it, line breaks folded and hidden characters escaped.
/// Inputs: the id, the question text when still listed and the answer. Output: the entry.
fn answer_line(id: i64, question: Option<&str>, answer: Option<&str>) -> String {
    let flat = |text: &str| {
        crate::memory::visible_text(&text.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let question = question.map_or_else(String::new, |text| format!(" {}", flat(text)));
    let outcome = answer.map_or_else(
        || "The owner dismissed it.".to_owned(),
        |answer| format!("Owner: {}", flat(answer)),
    );
    format!("- Question {id}:{question}\n  {outcome}\n")
}

/// Adds answer entries to a roadmap under `ANSWERS_HEADING`, adding the heading once and
/// skipping an entry whose question id the roadmap already lists, so a retried write never
/// repeats one. Inputs: the roadmap, none when missing, and the entries. Output: the new text.
fn with_answers(roadmap: Option<&str>, entries: &[String]) -> String {
    let mut text = roadmap.unwrap_or("# Roadmap\n").to_owned();
    if !text.lines().any(|line| line == ANSWERS_HEADING) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n{ANSWERS_HEADING}\n\n"));
    }
    let answers = text.find(&format!("\n{ANSWERS_HEADING}\n")).unwrap_or(0);
    for entry in entries {
        let marker = entry.split(':').next().unwrap_or(entry);
        let listed = text[answers..]
            .lines()
            .any(|line| line.split(':').next() == Some(marker));
        if !listed {
            text.push_str(entry);
        }
    }
    assert!(text.lines().any(|line| line == ANSWERS_HEADING));
    text
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

/// Writes each named document into `<state>/context/` through a 0644 partial file that is
/// synced, then renamed over the old name, so a link at either name is replaced, never followed.
/// Takes the state folder and the file names with their text. Returns `UNWRITABLE` on failure.
#[cfg(unix)]
fn write_beside_brief(state: &Path, documents: &[(&str, &str)]) -> Result<(), &'static str> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    let unwritable = |_| UNWRITABLE;
    let context = context_folder(state)?;
    for (name, body) in documents {
        let partial = format!(".{name}.partial");
        let _ = rustix::fs::unlinkat(&context, partial.as_str(), AtFlags::empty());
        let created = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let mut file = std::fs::File::from(
            rustix::fs::openat(
                &context,
                partial.as_str(),
                created | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
            )
            .map_err(unwritable)?,
        );
        let written = file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all());
        if written.is_err() {
            let _ = rustix::fs::unlinkat(&context, partial.as_str(), AtFlags::empty());
            return Err(UNWRITABLE);
        }
        rustix::fs::renameat(&context, partial.as_str(), &context, *name).map_err(unwritable)?;
    }
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
