use super::settings::connection_lost;
use super::*;
use crate::engine::owner::{ProcessText, SavedText};

#[cfg(all(test, unix))]
mod tests;

/// One Process request the TUI sends.
/// Reads or saves the app-wide or one project's instructions, reads a seed for a preview
/// (the default process or one file the owner named), reads what one team agent received at
/// its last launch (none for the newest master), reads or saves one project's agent
/// instruction settings, reads its workflow adoption, lists or confirms the one-time
/// import of its own instruction files, lists the moved files still waiting for a commit or
/// commits that move (DEC-106 and its addendum). A save names the version it read and, only
/// for "Keep mine" after a stale reply, the older version the owner opened.
/// Runs on the owner worker; the reply arrives as `OwnerEvent::Process` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ProcessQuery {
    Read {
        project: Option<ProjectId>,
    },
    Save {
        project: Option<ProjectId>,
        text: String,
        version: u64,
        opened: Option<u64>,
    },
    ReadFile {
        path: std::path::PathBuf,
    },
    Default,
    Agent {
        project: ProjectId,
        agent: Option<i64>,
    },
    Settings {
        project: ProjectId,
    },
    SaveSettings {
        project: ProjectId,
        settings: crate::engine::owner::InstructionSettings,
        version: Option<u64>,
    },
    Adoption {
        project: ProjectId,
    },
    PendingImport {
        project: ProjectId,
    },
    ConfirmImport {
        project: ProjectId,
    },
    Uncommitted {
        project: ProjectId,
    },
    CommitImport {
        project: ProjectId,
    },
}

/// What one Process request returned: a seed text, the saved text with its version, what a
/// save did (saved with what it comes to beside what a Claude master takes, or stale with
/// the newer text), what one team agent received at its last launch, one project's agent
/// instruction settings as stored with their version, its workflow adoption, the
/// instruction files an import would move or moved, the moved paths still waiting for a
/// commit with the checked-out branch, or that commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ProcessReply {
    Text(String),
    Current(ProcessText),
    Saved(SavedText),
    Agent(Box<crate::engine::owner::AgentInstructions>),
    Settings(crate::engine::owner::InstructionView),
    Adoption(Box<crate::engine::owner::Adoption>),
    Files(Vec<crate::engine::owner::ImportFile>),
    Uncommitted(crate::engine::owner::UncommittedImport),
    Committed(crate::engine::owner::ImportCommit),
}

/// One Process reply for the screens.
/// Holds the query it answers and its reply, or a bounded plain code.
/// A screen applies it only when the query is the one it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct ProcessEvent {
    pub query: ProcessQuery,
    pub result: Result<ProcessReply, String>,
}

impl ProcessEvent {
    /// The reply for a request the worker could not run because the trusted connection is gone.
    /// Takes the query.
    /// Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: ProcessQuery) -> Self {
        Self::failed(query, "trusted_connection_unavailable")
    }

    /// The reply for a request that failed before it reached the engine.
    /// Takes the query and the problem code.
    /// Returns the event for that query.
    pub(in crate::tui) fn failed(query: ProcessQuery, code: &str) -> Self {
        Self {
            query,
            result: Err(code.to_owned()),
        }
    }
}

impl Connection {
    /// Queues one Process request on the paired owner worker.
    /// Takes the query.
    /// Returns an error while another native request is pending.
    pub(in crate::tui) fn process(&mut self, query: ProcessQuery) -> Result<(), String> {
        self.owner_job(Job::Process(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one Process request through the trusted owner controller.
    /// Takes the query.
    /// Returns the event; a transport failure detaches the connection so the shell reconnects.
    pub(super) async fn process(&mut self, query: ProcessQuery) -> ProcessEvent {
        let mut lost = false;
        let result = self.process_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        ProcessEvent { query, result }
    }

    async fn process_reply(
        &mut self,
        query: ProcessQuery,
        lost: &mut bool,
    ) -> Result<ProcessReply, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or("trusted_connection_unavailable")?;
        let problem = |error: crate::engine::EngineError| {
            if connection_lost(&error.0) {
                *lost = true;
            }
            bounded_problem(&error.0)
        };
        match query {
            ProcessQuery::Read { project } => controller
                .process_text(project)
                .await
                .map(ProcessReply::Current),
            ProcessQuery::Save {
                project,
                text,
                version,
                opened,
            } => controller
                .save_process_text(project, text, version, opened)
                .await
                .map(ProcessReply::Saved),
            ProcessQuery::ReadFile { path } => controller
                .read_instruction_file(path)
                .await
                .map(ProcessReply::Text),
            ProcessQuery::Default => controller.default_process().await.map(ProcessReply::Text),
            ProcessQuery::Agent { project, agent } => controller
                .agent_instructions(project, agent)
                .await
                .map(|view| ProcessReply::Agent(Box::new(view))),
            ProcessQuery::Settings { project } => controller
                .instruction_settings(project)
                .await
                .map(ProcessReply::Settings),
            ProcessQuery::SaveSettings {
                project,
                settings,
                version,
            } => controller
                .save_instruction_settings(project, settings, version)
                .await
                .map(ProcessReply::Settings),
            ProcessQuery::Adoption { project } => controller
                .adoption(project)
                .await
                .map(|view| ProcessReply::Adoption(Box::new(view))),
            ProcessQuery::PendingImport { project } => controller
                .instruction_import(project)
                .await
                .map(ProcessReply::Files),
            ProcessQuery::ConfirmImport { project } => controller
                .confirm_instruction_import(project)
                .await
                .map(ProcessReply::Files),
            ProcessQuery::Uncommitted { project } => controller
                .uncommitted_instruction_import(project)
                .await
                .map(ProcessReply::Uncommitted),
            ProcessQuery::CommitImport { project } => controller
                .commit_instruction_import(project)
                .await
                .map(ProcessReply::Committed),
        }
        .map_err(problem)
    }
}
