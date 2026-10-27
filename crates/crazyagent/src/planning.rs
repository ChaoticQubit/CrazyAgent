use std::path::Path;

pub mod prompt;

/// The roadmap a planning job writes beside the brief, relative to the context folder's parent.
pub const ROADMAP: &str = "context/roadmap.md";
/// The release process a planning job writes beside the brief.
pub const RELEASE_PROCESS: &str = "context/release-process.md";
/// The most open points one plan raises as owner questions, the questions log's per-asker cap.
pub const OPEN_POINTS_MAX: usize = 8;
/// The largest open point raised as a question, the questions log's question bound.
pub const OPEN_POINT_BYTES_MAX: usize = 2048;
/// The largest agent reply a plan is read from.
pub const REPLY_BYTES_MAX: usize = 512 * 1024;
/// The job failure when the reply has no roadmap.
pub const NO_ROADMAP: &str = "Agent returned no roadmap";
/// The job failure when the reply has no release process.
pub const NO_RELEASE_PROCESS: &str = "Agent returned no release process";
/// The job failure when the documents cannot be written beside the brief.
pub const UNWRITABLE: &str = "Could not save the roadmap";
/// The job failure when the reply is larger than `REPLY_BYTES_MAX`.
pub const REPLY_TOO_LARGE: &str = "Agent output exceeded the limit";

/// The SQL condition, over a `research_jobs` row, that holds a roadmap planning job back while
/// any other job of its project still waits or runs; every other job passes.
pub(crate) const AFTER_RESEARCH: &str = "(research_jobs.kind <> 'roadmap_planning' OR NOT EXISTS (SELECT 1 FROM research_jobs AS earlier WHERE earlier.project = research_jobs.project AND earlier.kind <> 'roadmap_planning' AND earlier.status IN ('queued','needs_pool','running')))";

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

/// Writes each named document into `<state>/context/` through a 0644 partial file that is
/// synced, then renamed over the old name, so a link at either name is replaced, never followed.
/// Takes the state folder and the file names with their text. Returns `UNWRITABLE` on failure.
#[cfg(unix)]
fn write_beside_brief(state: &Path, documents: &[(&str, &str)]) -> Result<(), &'static str> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    let unwritable = |_| UNWRITABLE;
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = rustix::fs::open(state, directory, Mode::empty()).map_err(unwritable)?;
    let context = crate::project_setup::CONTEXT_DIRECTORY;
    let context =
        rustix::fs::openat(&folder, context, directory, Mode::empty()).map_err(unwritable)?;
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
