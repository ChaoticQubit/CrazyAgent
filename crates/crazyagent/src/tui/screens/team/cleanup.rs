use crate::protocol::ProjectId;
use crate::team::AgentRole;
use crate::team::cleanup::FinishedAgent;
use crate::tui::client::OwnerEvent;
use crate::tui::client::cleanup::{CleanupEvent, CleanupQuery, CleanupReply};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::Flow;
use crate::tui::screens::app_setup::size;
use crate::tui::screens::new_project::harness_name;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{cut, styled, wrap};
use crate::tui::theme::Role;
use ratatui::text::Line;

enum Stage {
    Closed,
    Reading,
    Confirm(Vec<FinishedAgent>),
    Removing(Vec<FinishedAgent>),
}

/// The Team screen's cleanup of finished agents: `c` reads the agents whose worktree and
/// folder can go, a box lists them with their size, `y` removes them all and `n` or Esc
/// keeps them. Input: keys and cleanup replies. Output: Cleanup requests and one message
/// with the outcome.
pub(super) struct Cleanup {
    stage: Stage,
}

/// The space the agents hold on disk, "at least" when the engine stopped counting one at
/// its bound. Input: the agents. Output: the size words.
pub(super) fn space(agents: &[FinishedAgent]) -> String {
    let bytes = agents
        .iter()
        .fold(0u64, |sum, agent| sum.saturating_add(agent.bytes));
    let floor = if agents.iter().all(|agent| agent.counted_all) {
        ""
    } else {
        "at least "
    };
    format!("{floor}{}", size(bytes))
}

fn agents_word(count: usize) -> String {
    format!(
        "{count} finished {}",
        if count == 1 { "agent" } else { "agents" }
    )
}

/// One plain sentence for a refused cleanup. Input: the code. Output: the sentence.
pub(super) fn words(code: &str) -> &'static str {
    match code {
        "cleanup_agent_has_task" => "An agent has a task waiting for it again.",
        "cleanup_agent_not_found" => "An agent is no longer in this project.",
        "cleanup_agent_running" => "An agent started again.",
        "cleanup_changes_open" => "An agent has changes waiting for review.",
        "cleanup_changes_restorable" => "An agent has discarded changes you can still restore.",
        "cleanup_commits_unlanded" => "An agent has commits that have not landed.",
        "cleanup_folder_failed" => {
            "The worktree is gone but the agent's folder could not be removed. Try again."
        }
        "cleanup_worktree_dirty" => {
            "An agent's working copy has changes that are not saved anywhere else."
        }
        "cleanup_worktree_invalid" => {
            "An agent's working copy is not where the engine recorded it."
        }
        "cleanup_unavailable" | "engine_support_not_ready" => {
            "Cleanup is not available in this engine yet."
        }
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine connection is not ready yet."
        }
        other => crate::config::plain_problem(other).unwrap_or("The engine could not do this."),
    }
}

impl Cleanup {
    /// Cleanup with nothing read and no box open.
    pub(super) fn new() -> Self {
        Self {
            stage: Stage::Closed,
        }
    }

    /// Takes a key first while the box is open or for `c`. Inputs: the key, the project and
    /// the context. Output: the flow when cleanup took the key, None to leave it to the tree.
    pub(super) fn key(&mut self, key: &Key, project: &ProjectId, ctx: &mut Ctx) -> Option<Flow> {
        match (&self.stage, key) {
            (Stage::Closed, Key::Char('c')) => {
                self.stage = Stage::Reading;
                ctx.request(Request::Cleanup(Box::new(CleanupQuery::Finished {
                    project: project.clone(),
                })));
            }
            (Stage::Closed, _) => return None,
            (Stage::Reading | Stage::Confirm(_), Key::Char('n') | Key::Esc) => {
                self.stage = Stage::Closed;
            }
            (Stage::Confirm(agents), Key::Char('y')) => {
                let agents = agents.clone();
                ctx.request(Request::Cleanup(Box::new(CleanupQuery::Remove {
                    project: project.clone(),
                    agents: agents.iter().map(|agent| agent.id).collect(),
                })));
                self.stage = Stage::Removing(agents);
            }
            _ => {}
        }
        Some(Flow::Handled)
    }

    /// Applies the reply to the request this cleanup sent for the project; replies for
    /// another project, or that arrive after the box closed, are left alone.
    /// Inputs: the arrival, the project and the context.
    pub(super) fn arrive(&mut self, arrival: &Arrival, project: &ProjectId, ctx: &mut Ctx) {
        let Arrival::Owner(event) = arrival else {
            return;
        };
        let OwnerEvent::Cleanup(event) = event.as_ref() else {
            return;
        };
        let CleanupEvent { query, result } = event.as_ref();
        match (query, &self.stage) {
            (CleanupQuery::Finished { project: asked }, Stage::Reading) if asked == project => {
                self.stage = Stage::Closed;
                match result {
                    Ok(CleanupReply::Finished(agents)) if agents.is_empty() => ctx.say(
                        Message::success("Nothing to clean up.", "No finished agent can go now."),
                    ),
                    Ok(CleanupReply::Finished(agents)) => {
                        self.stage = Stage::Confirm(agents.clone());
                    }
                    Ok(CleanupReply::Removed(_)) => {}
                    Err(code) => ctx.say(Message::error(
                        "Could not read finished agents.",
                        words(code),
                    )),
                }
            }
            (CleanupQuery::Remove { project: asked, .. }, Stage::Removing(agents))
                if asked == project =>
            {
                let freed = space(agents);
                self.stage = Stage::Closed;
                match result {
                    Ok(CleanupReply::Removed(removed)) => ctx.say(Message::success(
                        &format!("Removed {}.", agents_word(removed.len())),
                        &format!("Freed {freed}."),
                    )),
                    Ok(CleanupReply::Finished(_)) => {}
                    Err(code) => ctx.say(Message::warning(
                        "Cleanup kept an agent and stopped there.",
                        words(code),
                    )),
                }
            }
            _ => {}
        }
    }

    /// Says whether the cleanup box is open, so it takes every key.
    pub(super) fn open(&self) -> bool {
        !matches!(self.stage, Stage::Closed)
    }

    /// The keys the open cleanup box offers.
    pub(super) fn hints(&self) -> Vec<Hint> {
        match self.stage {
            Stage::Closed | Stage::Removing(_) => Vec::new(),
            Stage::Reading => vec![hint("Esc", "Close")],
            Stage::Confirm(_) => vec![hint("y", "Remove"), hint("n", "Keep")],
        }
    }

    /// The box lines while it is open: the agents to remove with role, harness and size, as
    /// many as fit after the explanation and the question, the rest counted in one "and N
    /// more" row, then what removal does and the question, always the last line.
    /// Inputs: the line width, the box's inner height and the context. Output: the title and
    /// at most `room` lines (one when `room` is zero), or None while closed.
    pub(super) fn lines(
        &self,
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Option<(String, Vec<Line<'static>>)> {
        let theme = &ctx.theme;
        let agents = match &self.stage {
            Stage::Closed => return None,
            Stage::Reading => {
                return Some((
                    "Clean up".into(),
                    vec![ctx.loading("Reading finished agents...")],
                ));
            }
            Stage::Confirm(agents) | Stage::Removing(agents) => agents,
        };
        let title = format!("Clean up {} ({})", agents_word(agents.len()), space(agents));
        let question = match self.stage {
            Stage::Removing(_) => ctx.loading("Removing..."),
            _ => styled(
                format!("y Remove and free {}   n Keep them", space(agents)),
                Role::Warning,
                theme,
            ),
        };
        let mut tail = vec![Line::default()];
        tail.extend(
            wrap(
                "Each agent's working copy and folder are deleted. Landed work stays in your project. The engine checks each one again first and keeps any that changed.",
                limit,
            )
            .into_iter()
            .map(|text| styled(text, Role::Muted, theme)),
        );
        tail.push(Line::default());
        if tail.len() + 2 > room {
            tail.clear();
        }
        tail.push(question);
        let free = room.saturating_sub(tail.len());
        let mut lines = Vec::new();
        let mut shown = 0;
        for (index, agent) in agents.iter().enumerate() {
            let role = match agent.role {
                AgentRole::Master => "master",
                AgentRole::Sub => "sub-agent",
            };
            let row = wrap(
                &format!(
                    "{}  {role}, {}  {}",
                    super::plain(&agent.label).replace('\n', " "),
                    harness_name(&agent.harness, None),
                    space(std::slice::from_ref(agent))
                ),
                limit,
            );
            let more = usize::from(index + 1 < agents.len());
            if lines.len() + row.len() + more > free {
                break;
            }
            lines.extend(row.into_iter().map(Line::raw));
            shown += 1;
        }
        let rest = &agents[shown..];
        if !rest.is_empty() && lines.len() < free {
            lines.push(Line::raw(cut(
                &format!("and {} more ({})", rest.len(), space(rest)),
                limit,
                ctx.glyphs.ellipsis,
            )));
        }
        lines.extend(tail);
        assert!(lines.len() <= room.max(1));
        Some((title, lines))
    }
}
