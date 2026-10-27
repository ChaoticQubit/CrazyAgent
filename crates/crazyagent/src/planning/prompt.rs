use crate::project_setup::{CONTEXT_DIRECTORY, ResearchKind};
use std::io::{self, Read};
use std::path::Path;

/// The most brief text the prompt carries.
pub const BRIEF_BYTES_MAX: u64 = 32 * 1024;
/// The most text of one research report the prompt carries; the agent is told where the rest is.
pub const REPORT_EXCERPT_BYTES_MAX: u64 = 16 * 1024;

const TASK: &str = "\nTask:\n\
- Split the work into versions V1, V2 up to Vn. V1 is the smallest release that is useful on its own.\n\
- For each version give its goal, its scope (what is in and what is left out), what it needs from earlier versions and an estimated time in working days or weeks.\n\
- Add a cut order: what to drop first when a version runs late.\n\
- Describe the release process: branches and review, the tests that must pass before a release, version numbers, packaging and distribution for the target platforms, rollback, and who approves a release.\n\
- List every point you could not decide from the brief and the research as a question for the owner.\n\
\nReply format: exactly these three top-level headings, in this order.\n\
# Roadmap\n\
(one ## heading per version)\n\
# Release process\n\
# Open questions\n\
(one \"- \" line per question, or \"- None\" when nothing is unclear)\n\
\nRules:\n\
- Work only inside your current folder; do not change files anywhere else.\n\
- Use only ## and deeper headings inside a section.\n\
- Say when an estimate or a fact is a guess.\n";

/// Builds the planning agent's prompt from the project's brief and every research report found
/// beside it, each read without following links and cut to its bound with a pointer to the rest.
/// Takes the project's state folder. Returns the prompt, or a read failure other than a missing file.
pub fn prompt(state: &Path) -> io::Result<String> {
    let context = state.join(CONTEXT_DIRECTORY);
    let mut prompt = String::from(
        "You are the planning agent for the project described below. Read the brief and the research, then plan the project's versions and how each version is released.\n\nProject brief:\n",
    );
    match excerpt(&context.join("brief.md"), BRIEF_BYTES_MAX)? {
        Some((text, _)) if !text.trim().is_empty() => {
            prompt.push_str(text.trim());
            prompt.push('\n');
        }
        _ => prompt.push_str("(The owner has not written a brief yet.)\n"),
    }
    let mut reports = 0;
    for kind in ResearchKind::ALL {
        let path = kind.report_path();
        let Some((text, cut)) = excerpt(&state.join(&path), REPORT_EXCERPT_BYTES_MAX)? else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if reports == 0 {
            prompt.push_str("\nResearch reports:\n");
        }
        reports += 1;
        prompt.push_str(&format!(
            "\n## {} ({path})\n\n{}\n",
            kind.label(),
            text.trim()
        ));
        if cut {
            prompt.push_str(&format!(
                "\n(The report continues in {}.)\n",
                state.join(&path).display()
            ));
        }
    }
    if reports == 0 {
        prompt.push_str("\nNo research reports were written for this project.\n");
    }
    prompt.push_str(TASK);
    assert!(reports <= ResearchKind::ALL.len());
    Ok(prompt)
}

/// Reads the start of a regular file that is not a link, cut at a character boundary within
/// `max` bytes. Takes the path and the bound. Returns the text and whether any of the file was
/// left out, none when the file is missing, a link or not a regular file, or the read failure.
fn excerpt(path: &Path, max: u64) -> io::Result<Option<(String, bool)>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    let file = open_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    let read = bytes.len();
    bytes.truncate(read.min(max as usize));
    let valid = match std::str::from_utf8(&bytes) {
        Ok(text) => text.len(),
        Err(error) => error.valid_up_to(),
    };
    bytes.truncate(valid);
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    assert!(text.len() as u64 <= max);
    Ok(Some((text, valid < read)))
}

/// Opens a file for reading, refusing a link at its name.
/// Takes the path. Returns the open file or the failure.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Opens a file for reading where no no-follow open exists; the caller checked it is no link.
/// Takes the path. Returns the open file or the failure.
#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}
