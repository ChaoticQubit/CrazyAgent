use super::settings::connection_lost;
use super::*;

#[cfg(all(test, unix))]
mod tests;

/// One Process request the TUI Process page sends.
/// Reads or saves the app-wide or one project's instructions, or reads a seed for a preview.
/// Runs on the owner worker; the reply arrives as `OwnerEvent::Process` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ProcessQuery {
    Read {
        project: Option<ProjectId>,
    },
    Save {
        project: Option<ProjectId>,
        text: String,
    },
    ImportGlobal,
    Default,
}

/// One Process reply for the screens.
/// Holds the query it answers and the text read (empty for a save), or a bounded plain code.
/// The Process page applies it only when the query is the one it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct ProcessEvent {
    pub query: ProcessQuery,
    pub result: Result<String, String>,
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
        let result = self.process_text(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        ProcessEvent { query, result }
    }

    async fn process_text(
        &mut self,
        query: ProcessQuery,
        lost: &mut bool,
    ) -> Result<String, String> {
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
            ProcessQuery::Read { project } => controller.process_text(project).await,
            ProcessQuery::Save { project, text } => controller
                .save_process_text(project, text)
                .await
                .map(|()| String::new()),
            ProcessQuery::ImportGlobal => controller.import_global_instructions().await,
            ProcessQuery::Default => controller.default_process().await,
        }
        .map_err(problem)
    }
}
