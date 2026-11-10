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
use crate::tui::seam::{Arrival, OWNER_BUSY, Request};
use crate::tui::text::{cut, styled, wrap};
use crate::tui::theme::Role;
use ratatui::text::Line;

const REMOVES_TEXT: &str = "Each agent's worktree, its code checkout and build files, is deleted. Their conversations, history and logs stay, and their landed work stays in your project. The engine checks each one again first and keeps any that changed.";
const STAYS_TEXT: &str = "Their conversations, history and logs stay.";

enum Stage {
    Closed,
    Reading,
    Confirm {
        agents: Vec<FinishedAgent>,
        held: Vec<FinishedAgent>,
    },
    Removing(Vec<FinishedAgent>),
}

/// The Team screen's cleanup of finished agents' worktrees: `c` reads the finished agents
/// that still hold one, a box lists the ones whose worktree can go with its size and each
/// one a check keeps with its reason, `y` removes those worktrees and `n` or Esc keeps them.
/// The agents' conversations, history and logs stay. Input: keys and cleanup replies.
/// Output: Cleanup requests and one message with the outcome.
pub(super) struct Cleanup {
    stage: Stage,
}

/// The space the agents' worktrees hold on disk, "at least" when the engine stopped
/// counting one at its bound. Input: the agents. Output: the size words.
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

/// Why a check keeps a finished agent's worktree, as the box shows it after "<agent> stays: ",
/// for each code the removal answers with. Input: the code. Output: the reason, or one naming
/// the code when this build has no words for it.
pub(super) fn kept_reason(code: &str) -> String {
    match code {
        "cleanup_agent_has_task" => "a task is waiting for it",
        "cleanup_agent_not_found" => "it is no longer in this project",
        "cleanup_agent_running" => "it is the master this project starts again",
        "cleanup_changes_open" => "its changes wait for your review",
        "cleanup_changes_restorable" => "its discarded changes can still be restored",
        "cleanup_commits_unlanded" => "it has commits that have not landed",
        "cleanup_refs_unlanded" => {
            "its worktree has a stash, branch or tag that is not saved anywhere else"
        }
        "cleanup_worktree_dirty" => "its worktree has changes that are not saved anywhere else",
        "cleanup_worktree_invalid" => "its worktree is not where the engine recorded it",
        "cleanup_worktree_leftover" => {
            "git left files in its worktree that CrazyAgent could not delete"
        }
        "cleanup_worktree_unreadable" => "git could not read its worktree",
        other => return format!("a check keeps it ({other})"),
    }
    .to_owned()
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
        "cleanup_refs_unlanded" => {
            "An agent's working copy has a stash, branch or tag that is not saved anywhere else."
        }
        "cleanup_worktree_dirty" => {
            "An agent's working copy has changes that are not saved anywhere else."
        }
        "cleanup_worktree_invalid" => {
            "An agent's working copy is not where the engine recorded it."
        }
        "cleanup_worktree_leftover" => {
            "Git removed an agent's worktree but left files the engine could not delete."
        }
        "cleanup_worktree_unreadable" => {
            "Git could not read an agent's working copy, for example because of a leftover lock file."
        }
        "cleanup_outcome_unknown" => {
            "The engine did not answer in time and may still be working. Press c to read the list again."
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

/// One agent's row in the box: an agent whose worktree goes with its role, harness and size,
/// or one a check keeps with its reason. Input: the agent. Output: the row text.
fn agent_row(agent: &FinishedAgent) -> String {
    let label = super::plain(&agent.label).replace('\n', " ");
    if let Some(code) = &agent.kept {
        return format!("{label} stays: {}", kept_reason(code));
    }
    let role = match agent.role {
        AgentRole::Master => "master",
        AgentRole::Sub => "sub-agent",
        AgentRole::Teammate => "teammate",
    };
    format!(
        "{label}  {role}, {}  {}",
        harness_name(&agent.harness, None),
        space(std::slice::from_ref(agent))
    )
}

impl Cleanup {
    /// Cleanup with nothing read and no box open.
    pub(super) fn new() -> Self {
        Self {
            stage: Stage::Closed,
        }
    }

    /// Takes a key first while the box is open or for `c`; `y` does nothing while no
    /// worktree can go. Inputs: the key, the project and the context. Output: the flow when
    /// cleanup took the key, None to leave it to the tree.
    pub(super) fn key(&mut self, key: &Key, project: &ProjectId, ctx: &mut Ctx) -> Option<Flow> {
        match (&self.stage, key) {
            (Stage::Closed, Key::Char('c')) => {
                self.stage = Stage::Reading;
                ctx.request(Request::Cleanup(Box::new(CleanupQuery::Finished {
                    project: project.clone(),
                })));
            }
            (Stage::Closed, _) => return None,
            (Stage::Reading | Stage::Confirm { .. }, Key::Char('n') | Key::Esc) => {
                self.stage = Stage::Closed;
            }
            (Stage::Confirm { agents, .. }, Key::Char('y')) if !agents.is_empty() => {
                let agents = agents.clone();
                assert!(agents.iter().all(|agent| agent.kept.is_none()));
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

    /// Opens the box for a read: the agents whose worktree can go and the ones a check keeps,
    /// or one message when no finished agent holds a worktree. Inputs: the agents the engine
    /// listed and the context.
    fn listed(&mut self, listed: &[FinishedAgent], ctx: &mut Ctx) {
        if listed.is_empty() {
            ctx.say(Message::success(
                "Nothing to clean up.",
                "No finished agent holds a worktree now.",
            ));
            return;
        }
        let (held, agents): (Vec<FinishedAgent>, Vec<FinishedAgent>) = listed
            .iter()
            .cloned()
            .partition(|agent| agent.kept.is_some());
        assert_eq!(held.len() + agents.len(), listed.len());
        self.stage = Stage::Confirm { agents, held };
    }

    /// Says what a removal did: whose worktrees went and the space freed, how many went and
    /// why one stayed when it stopped part way, or why it removed none. Inputs: the agents the
    /// box removed, the engine's answer and the context.
    fn removed(agents: &[FinishedAgent], result: &Result<CleanupReply, String>, ctx: &mut Ctx) {
        let gone: Vec<FinishedAgent> = match result {
            Ok(CleanupReply::Removed(removal)) => agents
                .iter()
                .filter(|agent| removal.agents.contains(&agent.id))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        let kept = agents.len().saturating_sub(gone.len());
        let worktrees = if gone.len() == 1 {
            "worktree"
        } else {
            "worktrees"
        };
        match result {
            Ok(CleanupReply::Removed(removal)) => match removal.kept.as_deref() {
                None => ctx.say(Message::success(
                    &format!("Removed the worktrees of {}.", agents_word(gone.len())),
                    &format!("Freed {}. {STAYS_TEXT}", space(&gone)),
                )),
                Some(code) => ctx.say(Message::warning(
                    &format!("Removed {} {worktrees}, kept {kept}:", gone.len()),
                    words(code),
                )),
            },
            Ok(CleanupReply::Finished(_)) => {}
            Err(code) if code == "cleanup_outcome_unknown" => ctx.say(Message::warning(
                "Could not confirm what cleanup did.",
                words(code),
            )),
            Err(code) if code == OWNER_BUSY => {
                ctx.say(Message::warning("Nothing was removed.", words(code)))
            }
            Err(code) => ctx.say(Message::warning(
                "Cleanup kept an agent and stopped there.",
                words(code),
            )),
        }
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
                    Ok(CleanupReply::Finished(listed)) => self.listed(listed, ctx),
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
                let agents = agents.clone();
                self.stage = Stage::Closed;
                Self::removed(&agents, result, ctx);
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
        match &self.stage {
            Stage::Closed | Stage::Removing(_) => Vec::new(),
            Stage::Confirm { agents, .. } if agents.is_empty() => vec![hint("Esc", "Close")],
            Stage::Reading => vec![hint("Esc", "Close")],
            Stage::Confirm { .. } => vec![hint("y", "Remove"), hint("n", "Keep")],
        }
    }

    /// The lines under the agents: what removal does and the question, always the last line,
    /// or only the question when the box has no room for the rest or no worktree can go.
    /// Inputs: the agents whose worktree goes, the line width, the box's inner height and the
    /// context. Output: the lines, at least the question.
    fn tail(
        &self,
        agents: &[FinishedAgent],
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let question = match self.stage {
            Stage::Removing(_) => ctx.loading("Removing..."),
            _ if agents.is_empty() => styled("Nothing can go now.   Esc Close", Role::Muted, theme),
            _ => styled(
                format!("y Remove and free {}   n Keep them", space(agents)),
                Role::Warning,
                theme,
            ),
        };
        let mut tail = vec![Line::default()];
        if !agents.is_empty() {
            tail.extend(
                wrap(REMOVES_TEXT, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            tail.push(Line::default());
        }
        if tail.len() + 2 > room {
            tail.clear();
        }
        tail.push(question);
        assert!(!tail.is_empty());
        tail
    }

    /// The box lines while it is open: the agents whose worktree goes with role, harness and
    /// size, then each agent a check keeps with its reason, as many as fit after the
    /// explanation and the question, the rest counted in one "and N more" row, then what
    /// removal does and the question, always the last line.
    /// Inputs: the line width, the box's inner height and the context. Output: the title and
    /// at most `room` lines (one when `room` is zero), or None while closed.
    pub(super) fn lines(
        &self,
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Option<(String, Vec<Line<'static>>)> {
        let (agents, held): (&[FinishedAgent], &[FinishedAgent]) = match &self.stage {
            Stage::Closed => return None,
            Stage::Reading => {
                return Some((
                    "Clean up".into(),
                    vec![ctx.loading("Reading finished agents...")],
                ));
            }
            Stage::Confirm { agents, held } => (agents, held),
            Stage::Removing(agents) => (agents, &[]),
        };
        let title = if agents.is_empty() {
            "Finished agents".to_owned()
        } else {
            format!(
                "Worktrees of {} ({})",
                agents_word(agents.len()),
                space(agents)
            )
        };
        let tail = self.tail(agents, limit, room, ctx);
        let free = room.saturating_sub(tail.len());
        let listed: Vec<&FinishedAgent> = agents.iter().chain(held).collect();
        let mut lines = Vec::new();
        let mut shown = 0;
        for (index, agent) in listed.iter().enumerate() {
            let row = wrap(&agent_row(agent), limit);
            let more = usize::from(index + 1 < listed.len());
            if lines.len() + row.len() + more > free {
                break;
            }
            if agent.kept.is_some() {
                lines.extend(
                    row.into_iter()
                        .map(|text| styled(text, Role::Muted, &ctx.theme)),
                );
            } else {
                lines.extend(row.into_iter().map(Line::raw));
            }
            shown += 1;
        }
        let rest = &listed[shown..];
        if !rest.is_empty() && lines.len() < free {
            let going: Vec<FinishedAgent> = rest
                .iter()
                .filter(|agent| agent.kept.is_none())
                .map(|agent| (**agent).clone())
                .collect();
            let more = if going.is_empty() {
                format!("and {} more that stay", rest.len())
            } else {
                format!("and {} more ({})", rest.len(), space(&going))
            };
            lines.push(Line::raw(cut(&more, limit, ctx.glyphs.ellipsis)));
        }
        lines.extend(tail);
        assert!(lines.len() <= room.max(1));
        Some((title, lines))
    }
}
