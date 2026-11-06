use super::settings::connection_lost;
use super::*;
use crate::ide::bridge::reveal::RevealTarget;
use crate::tui::frame::Message;

#[cfg(all(test, unix))]
mod tests;

/// The plain line shown when no desktop IDE is open for the project.
pub(in crate::tui) const IDE_NOT_RUNNING: &str = "The desktop IDE isn't open for this project.";
const NOT_RUNNING_HINT: &str = "Open the project in the CrazyAgent app, then its IDE tab.";

/// One "open in IDE" request the TUI sends: the project and what to show. Runs on the
/// owner worker; the reply arrives as `OwnerEvent::Reveal` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct RevealQuery {
    pub project: ProjectId,
    pub target: RevealTarget,
}

/// What the engine answered an "open in IDE" request with: shown, or a bounded plain code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct RevealEvent {
    pub query: RevealQuery,
    pub result: Result<(), String>,
}

impl RevealEvent {
    /// The reply for a request the worker could not run because the trusted connection is
    /// gone. Takes the query. Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: RevealQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }

    /// The line the TUI shows for this reply, whichever screen is open.
    /// Takes the event. Returns the message.
    pub(in crate::tui) fn message(&self) -> Message {
        let code = match &self.result {
            Ok(()) => return Message::success("Opened in the desktop IDE.", ""),
            Err(code) => code.as_str(),
        };
        let (lead, rest) = match code {
            "ide_not_running" => (IDE_NOT_RUNNING, NOT_RUNNING_HINT),
            "ide_reveal_unanswered" => (
                "The desktop IDE didn't show it.",
                "Check the project's IDE tab in the CrazyAgent app, then try again.",
            ),
            "ide_reveal_not_listed" => (
                "That agent's folder isn't in the IDE.",
                "Show all agents in the IDE, then try again.",
            ),
            "ide_reveal_missing" | "changes_not_found" => {
                ("That file or folder is not there any more.", "")
            }
            "ide_reveal_outside" | "ide_reveal_invalid" => (
                "CrazyAgent opens only files inside the project's folders.",
                "",
            ),
            "ide_reveal_busy" => ("The IDE is still showing earlier files.", "Try again."),
            "trusted_connection_unavailable" | "controller_disconnected" => {
                ("The engine connection is not ready yet.", "Try again.")
            }
            _ => ("Could not open it in the desktop IDE.", ""),
        };
        Message::error(lead, rest)
    }
}

impl Connection {
    /// Queues one "open in IDE" request on the paired owner worker.
    /// Takes the query. Returns an error while another native request is pending.
    pub(in crate::tui) fn reveal(&mut self, query: RevealQuery) -> Result<(), String> {
        self.owner_job(Job::Reveal(Box::new(query)))
    }
}

impl NativeOwner {
    /// Runs one "open in IDE" request through the trusted owner controller.
    /// Takes the query. Returns the event; a transport failure detaches the connection so
    /// the shell reconnects.
    pub(super) async fn reveal(&mut self, query: RevealQuery) -> RevealEvent {
        let mut lost = false;
        let result = self.reveal_reply(query.clone(), &mut lost).await;
        if lost {
            self.detach();
        }
        RevealEvent { query, result }
    }

    async fn reveal_reply(&mut self, query: RevealQuery, lost: &mut bool) -> Result<(), String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or("trusted_connection_unavailable")?;
        controller
            .reveal_in_ide(query.project, query.target)
            .await
            .map(|_| ())
            .map_err(|error| {
                if connection_lost(&error.0) {
                    *lost = true;
                }
                bounded_problem(&error.0)
            })
    }
}
