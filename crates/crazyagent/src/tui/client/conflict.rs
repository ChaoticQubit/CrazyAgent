use crate::storage::versioned::LastWriter;

/// The most lines of the text that landed a conflict shows; the rest are counted.
pub(in crate::tui) const LINES_SHOWN_MAX: usize = 6;

/// The two choices a conflict offers, as their keys read.
pub(in crate::tui) const CHOICES: &str =
    "Ctrl-R Reload: use the saved text. Ctrl-Y Keep mine: save yours over it.";

/// A save the engine refused as stale, held so the owner sees what landed before anything is
/// written: the version the save named, the newer version with its text, and who saved it.
/// The owner then reloads the newer text or keeps their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Conflict {
    pub opened: u64,
    pub version: u64,
    pub current: String,
    pub writer: LastWriter,
}

impl Conflict {
    /// The sentence naming who changed the item since the owner opened it.
    pub fn heading(&self) -> String {
        format!(
            "Changed by {} since you opened it.",
            writer_name(&self.writer)
        )
    }

    /// The versions Keep mine saves with: over the newer version shown, naming the version
    /// the owner opened so the engine records the owner's choice. Output: (version, opened).
    pub fn keep_mine(&self) -> (u64, Option<u64>) {
        (self.version, Some(self.opened))
    }
}

/// Names who saved a shared item's current version. Input: the writer. Output: "you in
/// another window", "Agent <id>" for a team agent, another agent's own name, or "another
/// save" when nobody is recorded.
pub(in crate::tui) fn writer_name(writer: &LastWriter) -> String {
    match writer {
        LastWriter::Owner => "you in another window".to_owned(),
        LastWriter::Nobody => "another save".to_owned(),
        LastWriter::Agent(agent) => match agent.team_agent() {
            Some(id) => format!("Agent {id}"),
            None => agent.as_str().to_owned(),
        },
    }
}
