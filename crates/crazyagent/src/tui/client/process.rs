use super::settings::connection_lost;
use super::*;
use crate::engine::owner::{ProcessText, SavedText};

#[cfg(all(test, unix))]
mod tests;

/// One Process request the TUI sends.
/// Reads or saves the app-wide or one project's instructions, reads a seed for a preview,
/// or reads what one team agent received at its last launch (none for the newest master).
/// A save names the version it read and, only for "Keep mine" after a stale reply, the
/// older version the owner opened.
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
    ImportGlobal,
    Default,
    Agent {
        project: ProjectId,
        agent: Option<i64>,
    },
}

/// What one Process request returned: a seed text, the saved text with its version, what a
/// save did (saved, or stale with the newer text), or what one team agent received at its
/// last launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ProcessReply {
    Text(String),
    Current(ProcessText),
    Saved(SavedText),
    Agent(Box<crate::engine::owner::AgentInstructions>),
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
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
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
            ProcessQuery::ImportGlobal => controller
                .import_global_instructions()
                .await
                .map(ProcessReply::Text),
            ProcessQuery::Default => controller.default_process().await.map(ProcessReply::Text),
            ProcessQuery::Agent { project, agent } => controller
                .agent_instructions(project, agent)
                .await
                .map(|view| ProcessReply::Agent(Box::new(view))),
        }
        .map_err(problem)
    }
}
