use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::project_setup::CONTEXT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::questions::{Ask, EngineSource};
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
const _: () = assert!(OPEN_POINTS_MAX as i64 <= crate::questions::PENDING_PER_ASKER_MAX);
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

const QUESTION_CONTEXT: &str = "Raised by the roadmap planning job. The roadmap in context/roadmap.md lists it under Open questions. Your answer goes to the project's master once; the roadmap itself does not change.";
const FULL: [&str; 2] = ["questions_full", "questions_asker_full"];
const REFUSED: [&str; 2] = ["questions_ask_invalid", "questions_text_invalid"];
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
    let said = words(text.trim_start_matches(['>', ' ', '\t']));
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
        .map(words);
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

/// Whether a heading text, without its marks, names an engine section, ignoring case and
/// how many spaces or tabs part its words. Takes the text. Returns true for a reserved name.
fn engine_section(text: &str) -> bool {
    let text = words(text);
    ENGINE_SECTIONS
        .iter()
        .any(|name| text.eq_ignore_ascii_case(name))
}

/// A text's words joined by single spaces, so any run of white space reads as one space.
/// Takes the text. Returns the words.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// Raises the plan's questions (`Plan::questions`) as `engine:planning` owner questions of the
/// project; the log returns a pending planning question with the same text instead of asking
/// it twice. A question the log refuses as invalid stays in the roadmap only, and raising stops
/// once the project's or the planning step's questions are full. The owner's answer reaches
/// the project's master once as an engine note; the roadmap is never changed by it.
/// Inputs: the engine database path, the project and the plan. Output: each logged question's id.
pub fn raise(database: &Path, project: &str, plan: &Plan) -> Result<Vec<u64>, EngineError> {
    let root = database
        .parent()
        .ok_or_else(|| failure("questions_unavailable"))?;
    let project = ProjectId::new(project).map_err(|_| failure("question_project_unknown"))?;
    let db = crate::questions::open(root)?;
    let mut logged = Vec::new();
    for question in plan.questions() {
        let ask = Ask {
            question,
            options: Vec::new(),
            recommended: None,
            context: Some(QUESTION_CONTEXT.to_owned()),
        };
        match crate::questions::raise(&db, &project, &EngineSource::Planning, &ask) {
            Ok(id) => logged.push(id),
            Err(problem) if FULL.contains(&problem.0.as_str()) => break,
            Err(problem) if REFUSED.contains(&problem.0.as_str()) => {}
            Err(problem) => return Err(problem),
        }
    }
    assert!(logged.len() <= OPEN_POINTS_MAX);
    Ok(logged)
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
