use super::new_project::harness_name;
use super::new_project_view::{Look, Row, wrap_words};
use crate::team::{AgentStatus, EdgeState, GraphNode, NodeKind, TeamGraph};
use crate::tui::ctx::Load;
use ratatui::style::Style;

/// Content width from which the Conversation tab shows the team panel beside the master.
pub(in crate::tui) const TEAM_FROM: usize = 100;
/// Width of the team panel in cells.
pub(in crate::tui) const TEAM_SIDE: usize = 32;
/// Cells between the master conversation and the team panel.
pub(in crate::tui) const TEAM_GAP: usize = 2;
/// What the team panel says while the team has no sub-agents.
pub(in crate::tui) const TEAM_EMPTY: &str =
    "No sub-agents yet. The master starts them when it delegates.";
const ROWS_PER_AGENT: usize = 4;

/// The team panel beside the master conversation: a heading, then per sub-agent its status
/// glyph and name, its harness and status, and its newest task, live agents first. Every row
/// is cut to the panel width with an ellipsis; agents that do not fit end in a "more" line.
/// Takes the latest team graph read, the panel height and the look. Returns at most `height`
/// rows, each exactly `TEAM_SIDE` cells wide.
pub(in crate::tui) fn activity_lines(
    graph: &Load<Box<TeamGraph>>,
    height: usize,
    look: &Look,
) -> Vec<Row> {
    let mut lines = vec![Row::of("Team", look.heading)];
    match graph {
        Load::Idle | Load::Loading => lines.push(Row::of("Loading the team.", look.muted)),
        Load::NotReady => lines.push(Row::of("Not ready yet.", look.muted)),
        Load::Failed(_) => lines.push(Row::of("Could not load the team.", look.error)),
        Load::Ready(graph) => agent_lines(graph, height, look, &mut lines),
    }
    let rows: Vec<Row> = lines
        .into_iter()
        .take(height)
        .map(|row| row.pad(TEAM_SIDE))
        .collect();
    assert!(rows.len() <= height && rows.iter().all(|row| row.width() == TEAM_SIDE));
    rows
}

fn agent_lines(graph: &TeamGraph, height: usize, look: &Look, lines: &mut Vec<Row>) {
    let mut agents: Vec<&GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Agent)
        .collect();
    agents.sort_by(|a, b| {
        live(b)
            .cmp(&live(a))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.id.cmp(&b.id))
    });
    if agents.is_empty() {
        lines.extend(
            wrap_words(TEAM_EMPTY, TEAM_SIDE)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        return;
    }
    let room = height.saturating_sub(lines.len() + 1);
    let fits = if agents.len() * ROWS_PER_AGENT <= room + 1 {
        agents.len()
    } else {
        room.saturating_sub(1) / ROWS_PER_AGENT
    };
    for agent in &agents[..fits] {
        let (glyph, style) = status_glyph(agent.status, look);
        lines.push(Row::default());
        lines.push(Row::of(look.glyph(glyph), style).push(format!(" {}", agent.label), look.fg));
        let word = agent
            .status_label
            .clone()
            .or_else(|| agent.status.map(|status| status.label().to_owned()))
            .unwrap_or_default();
        lines.push(Row::of(
            format!("  {}, {word}", harness_name(&agent.harness, None)),
            look.muted,
        ));
        lines.push(Row::of(
            format!("  {}", task_line(graph, agent)),
            look.muted,
        ));
    }
    if fits < agents.len() {
        lines.push(Row::of(
            format!("+{} more. Press t for the team.", agents.len() - fits),
            look.muted,
        ));
    }
}

fn live(node: &&GraphNode) -> bool {
    node.status.is_some_and(AgentStatus::live)
}

fn status_glyph(status: Option<AgentStatus>, look: &Look) -> (&'static str, Style) {
    match status {
        Some(AgentStatus::Starting | AgentStatus::Working) => ("\u{25b8}", look.running),
        Some(AgentStatus::Idle) => ("\u{25cc}", look.warning),
        Some(AgentStatus::Failed) => ("\u{2717}", look.error),
        Some(AgentStatus::Stopped) | None => ("\u{2298}", look.muted),
    }
}

/// The task line of one sub-agent: its newest live task, else its newest ended one led by
/// Done or Failed, as a one-line title. Takes the graph and the agent. Returns the line, or
/// "No task yet." when it has none.
fn task_line(graph: &TeamGraph, agent: &GraphNode) -> String {
    let live = |state: EdgeState| matches!(state, EdgeState::Sent | EdgeState::Running);
    let Some(edge) = graph
        .edges
        .iter()
        .filter(|edge| edge.to == agent.id)
        .max_by_key(|edge| (live(edge.state), edge.task_id))
    else {
        return "No task yet.".to_owned();
    };
    let title = super::team::title(&edge.text);
    match edge.state {
        EdgeState::Done => format!("Done: {title}"),
        EdgeState::Failed => format!("Failed: {title}"),
        EdgeState::Sent | EdgeState::Running => title,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::ProjectId;
    use crate::team::{GraphEdge, TaskSource};

    fn agent(id: &str, label: &str, status: AgentStatus) -> GraphNode {
        GraphNode {
            id: id.into(),
            kind: NodeKind::Agent,
            parent: Some("group-codex".into()),
            label: label.into(),
            harness: "codex".into(),
            status: Some(status),
            status_label: None,
            account: None,
            agents: None,
            terminal: None,
            questions: None,
        }
    }

    fn edge(task_id: i64, to: &str, state: EdgeState, text: &str) -> GraphEdge {
        GraphEdge {
            from: "master".into(),
            to: to.into(),
            task_id,
            source: TaskSource::Master,
            text: text.into(),
            state,
            reply: None,
        }
    }

    fn team(agents: Vec<GraphNode>, edges: Vec<GraphEdge>) -> Load<Box<TeamGraph>> {
        Load::Ready(Box::new(TeamGraph {
            project_id: ProjectId::new("tidepool").unwrap(),
            nodes: agents,
            edges,
        }))
    }

    fn text(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| row.text().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn the_team_panel_lists_live_agents_first_with_harness_status_and_newest_task() {
        let graph = team(
            vec![
                agent("team-3", "Linus (@usage)", AgentStatus::Stopped),
                agent(
                    "team-1",
                    "Ada Lovelace (@onboarding-fix-for-reused-ids)",
                    AgentStatus::Working,
                ),
            ],
            vec![
                edge(1, "team-1", EdgeState::Done, "Old task"),
                edge(
                    2,
                    "team-1",
                    EdgeState::Running,
                    "Fix\tthe onboarding run name collision\nmore",
                ),
                edge(3, "team-3", EdgeState::Done, "Write the usage page"),
            ],
        );
        let rows = activity_lines(&graph, 20, &Look::plain());
        assert!(rows.iter().all(|row| row.width() == TEAM_SIDE));
        assert_eq!(
            text(&rows),
            [
                "Team",
                "",
                "▸ Ada Lovelace (@onboarding-fix…",
                "  Codex, Working",
                "  Fix the onboarding run name c…",
                "",
                "⊘ Linus (@usage)",
                "  Codex, Stopped",
                "  Done: Write the usage page",
            ]
        );
    }

    #[test]
    fn the_team_panel_says_when_there_are_no_sub_agents_or_no_read() {
        let look = Look::plain();
        assert_eq!(
            text(&activity_lines(&team(Vec::new(), Vec::new()), 10, &look)),
            [
                "Team",
                "No sub-agents yet. The master",
                "starts them when it delegates."
            ]
        );
        assert_eq!(
            text(&activity_lines(&Load::Loading, 10, &look)),
            ["Team", "Loading the team."]
        );
        assert_eq!(
            text(&activity_lines(&Load::NotReady, 10, &look)),
            ["Team", "Not ready yet."]
        );
        assert_eq!(text(&activity_lines(&Load::Loading, 1, &look)), ["Team"]);
    }

    #[test]
    fn agents_that_do_not_fit_end_in_a_more_line_within_the_height() {
        let agents = (1..=6)
            .map(|id| {
                agent(
                    &format!("team-{id}"),
                    &format!("agent-{id}"),
                    AgentStatus::Idle,
                )
            })
            .collect();
        let rows = activity_lines(&team(agents, Vec::new()), 12, &Look::plain());
        let shown = text(&rows);
        assert!(rows.len() <= 12);
        assert_eq!(
            shown
                .iter()
                .filter(|row| row.starts_with("◌ agent-"))
                .count(),
            2
        );
        assert_eq!(shown.last().unwrap(), "+4 more. Press t for the team.");
    }
}
