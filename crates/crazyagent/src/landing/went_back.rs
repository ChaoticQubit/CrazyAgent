#![cfg_attr(not(unix), allow(dead_code))]

/// Why a regeneration rewrote nothing: no safe place to run the tools, a tool failed or ran
/// out of time, the owner hid a folder during the run, the clash is not lockfiles alone, the
/// owner's merge settings cover the paths, a tool changed other files, the agent's work
/// moved before the commit was kept, or a reason the owner is not told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    Setup,
    Tool,
    Hidden,
    NotLockfileOnly,
    MergeSettings,
    WroteOutside,
    Moved,
    Other,
}

impl Why {
    /// The word the refusal log line names. Input: the reason. Output: the word.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Why::Setup => "setup",
            Why::Tool => "tool",
            Why::Hidden => "hidden",
            Why::NotLockfileOnly => "not_lockfile_only",
            Why::MergeSettings => "merge_settings",
            Why::WroteOutside => "wrote_outside",
            Why::Moved => "moved",
            Why::Other => "other",
        }
    }
}

/// The most bytes one lockfile path takes in the owner's note.
const NOTE_PATH_BYTES_MAX: usize = 120;

/// A regeneration that rewrote nothing: why, the tool it names and the clashing lockfiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) why: Why,
    pub(crate) tool: &'static str,
    pub(crate) files: Vec<String>,
}

/// The owner's note that a set they heard waits in Changes went back to its agent, then
/// why its lockfiles were not rewritten, naming as many of them as fit, each with control
/// characters replaced and cut to `NOTE_PATH_BYTES_MAX`. Inputs: the agent's label and the
/// refusal, if one. Output: the note, within `team::NOTE_BYTES_MAX`.
pub(crate) fn went_back_note(agent: &str, refusal: Option<&Refusal>) -> String {
    let agent = crate::team::bound_text(agent, crate::agents::LABEL_BYTES_MAX);
    let note = format!(
        "The edits from {agent} went back to it to take in newer work, so they are not waiting for you in Changes yet."
    );
    let Some(refusal) = refusal.filter(|refusal| !refusal.files.is_empty()) else {
        return note;
    };
    let Some(empty) = because(refusal, "", &agent) else {
        return note;
    };
    let room = crate::team::NOTE_BYTES_MAX - note.len() - 1 - empty.len();
    let tail_max = " and 16 more".len();
    assert!(room >= NOTE_PATH_BYTES_MAX + tail_max);
    let mut files = String::new();
    let mut shown = 0_usize;
    for path in &refusal.files {
        let name = crate::team::bound_text(
            &path.replace(char::is_control, "\u{fffd}"),
            NOTE_PATH_BYTES_MAX,
        );
        let gap = if shown == 0 { "" } else { ", " };
        if files.len() + gap.len() + name.len() + tail_max > room {
            break;
        }
        files = format!("{files}{gap}{name}");
        shown += 1;
    }
    let more = refusal.files.len() - shown;
    if more > 0 {
        files = format!("{files} and {more} more");
    }
    let line = format!(
        "{note} {}",
        because(refusal, &files, &agent).unwrap_or_default()
    );
    assert!(line.len() <= crate::team::NOTE_BYTES_MAX);
    line
}

/// The sentence on why a regeneration rewrote nothing. Inputs: the refusal, the named
/// lockfiles and the agent's label. Output: the sentence, or None for a reason the owner is
/// not told.
fn because(refusal: &Refusal, files: &str, agent: &str) -> Option<String> {
    let tool = refusal.tool;
    let (were, them) = match refusal.files.len() {
        1 => ("was", "it"),
        _ => ("were", "them"),
    };
    Some(match refusal.why {
        Why::Setup => format!(
            "{files} could not be updated automatically because a safe place to run {tool} could not be set up, so {agent} will fix {them} itself."
        ),
        Why::Tool => format!(
            "{files} could not be updated automatically because {tool} failed or took too long, so {agent} will fix {them} itself."
        ),
        Why::Hidden => format!(
            "{files} {were} not updated automatically because you hid a folder while {tool} was running, so nothing it wrote was kept."
        ),
        Why::NotLockfileOnly => format!(
            "The clash with the newer work is in more than {files}, so {agent} will sort it out itself."
        ),
        Why::MergeSettings => format!(
            "{files} {were} not updated automatically because your repository has its own merge settings for {them}, so {agent} will fix {them} itself."
        ),
        Why::WroteOutside => format!(
            "{files} {were} not updated automatically because {tool} also changed other files, so nothing it wrote was kept."
        ),
        Why::Moved => format!(
            "{agent}'s work changed while {files} {were} being updated, so that update was dropped and {agent} will redo it."
        ),
        Why::Other => return None,
    })
}
