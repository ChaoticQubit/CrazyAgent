use super::super::{AgentRole, bound_text};
use super::error;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};

/// The most end rows one project keeps; the oldest go first.
pub(super) const ENDS_PER_PROJECT_MAX: i64 = 500;

/// The most characters of a problem one end row keeps.
pub(super) const END_PROBLEM_CHARS_MAX: usize = 300;

/// What the team note says when a live master's terminal ends and nobody asked it to stop.
pub(super) const TERMINAL_ENDED: &str =
    "The master's terminal ended, so the engine starts it again in its own conversation";

/// One agent terminal that ended, as its end row keeps it: who, when, why, the problem it
/// ended with, whether a turn was open and whether processes outlived its stop.
pub(super) struct End<'a> {
    pub(super) project: &'a ProjectId,
    pub(super) agent: i64,
    pub(super) role: AgentRole,
    pub(super) harness: &'a str,
    pub(super) at_ms: i64,
    pub(super) cause: &'static str,
    pub(super) problem: Option<&'a str>,
    pub(super) turn_open: bool,
    pub(super) leftovers: bool,
}

/// The codes a problem closes with when the engine itself ended the terminal at one of its
/// bounds, and the cause each end is named by.
const ENGINE_LIMITS: [(&str, &str); 2] = [
    ("launch_deadline", "engine_limit:launch_deadline"),
    (
        "launch_transcript_unavailable",
        "engine_limit:launch_transcript_unavailable",
    ),
];

/// The stop the engine asks for after a turn that ended so the agent cannot take another.
pub(super) const TURN_ENDED: &str = "engine_stop:turn_ended";

/// Names why an agent's terminal ended. Inputs: the cause the engine recorded when it asked
/// the agent to stop, if it did, whether the agent got as far as ready, and the problem it
/// ended with. Output: `owner_stop`, `rate_limited`, `engine_stop:<why>`, `takeover_failed`,
/// `engine_limit:<code>` when the problem closes with a code of [`ENGINE_LIMITS`], which also
/// names a [`TURN_ENDED`] stop that followed such a turn, `start_failed` or `ended_by_itself`.
pub(super) fn cause(
    stop: Option<&'static str>,
    ready: bool,
    problem: Option<&str>,
) -> &'static str {
    let limit = problem.and_then(engine_limit);
    match (stop, limit) {
        (Some(TURN_ENDED), Some(limit)) => limit,
        (Some(""), _) => "engine_stop:unnamed",
        (Some(why), _) => why,
        (None, _) if problem.is_some_and(|problem| problem.contains("could not be taken over")) => {
            "takeover_failed"
        }
        (None, Some(limit)) => limit,
        (None, None) if !ready && problem.is_some() => "start_failed",
        (None, None) => "ended_by_itself",
    }
}

/// Says whether a launch code is one of the engine's own bounds in [`ENGINE_LIMITS`], so a
/// turn that ended at it leaves the code on its agent. Input: the code. Output: the answer.
pub(super) fn engine_bound(code: &str) -> bool {
    ENGINE_LIMITS.iter().any(|(named, _)| *named == code)
}

/// Finds the engine bound a problem names in its closing `(code)`, as `with_code` writes
/// it. Input: the problem. Output: the `engine_limit:<code>` cause, none for any other.
fn engine_limit(problem: &str) -> Option<&'static str> {
    let code = problem.strip_suffix(')')?.rsplit_once('(')?.1;
    let &(_, limit) = ENGINE_LIMITS.iter().find(|(named, _)| *named == code)?;
    assert!(limit.ends_with(code));
    Some(limit)
}

/// Writes one end row and drops the project's oldest rows past [`ENDS_PER_PROJECT_MAX`].
/// Inputs: the connection and the end. Output: any SQLite failure.
pub(super) fn record(connection: &Connection, end: &End<'_>) -> Result<(), EngineError> {
    assert!(!end.cause.is_empty());
    assert!(end.agent > 0);
    let problem = end
        .problem
        .map(|problem| bound_text(problem, END_PROBLEM_CHARS_MAX));
    connection
        .execute(
            "INSERT INTO team_agent_ends(project,agent,role,harness,at_ms,cause,problem,turn_open,leftovers)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                end.project.as_str(),
                end.agent,
                end.role.name(),
                end.harness,
                end.at_ms,
                end.cause,
                problem,
                end.turn_open,
                end.leftovers,
            ],
        )
        .map_err(error)?;
    connection
        .execute(
            "DELETE FROM team_agent_ends WHERE project=?1 AND id <= (
                SELECT id FROM team_agent_ends WHERE project=?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            params![end.project.as_str(), ENDS_PER_PROJECT_MAX],
        )
        .map_err(error)?;
    Ok(())
}

/// The team note for a live master whose terminal ended on its own: it starts again in its
/// own conversation, and a turn it was in was cut. Inputs: the problem it ended with and
/// whether a turn was open. Output: the note.
pub(super) fn restart_line(problem: Option<&str>, turn_open: bool) -> String {
    let why = problem.map_or_else(String::new, |problem| {
        format!(" ({})", bound_text(problem, END_PROBLEM_CHARS_MAX))
    });
    let cut = if turn_open {
        ". Its last turn was cut, so send that message again if it still matters"
    } else {
        ""
    };
    format!("{TERMINAL_ENDED}{why}{cut}. Turn it off from the master view to stop it.")
}

#[cfg(test)]
mod tests {
    use super::{ENDS_PER_PROJECT_MAX, End, TURN_ENDED, cause, engine_bound, record};
    use crate::protocol::ProjectId;
    use crate::team::AgentRole;
    use rusqlite::Connection;

    #[test]
    fn each_end_names_its_cause() {
        assert_eq!(cause(Some("owner_stop"), true, None), "owner_stop");
        assert_eq!(
            cause(Some("engine_stop:idle"), true, Some("x")),
            "engine_stop:idle"
        );
        assert_eq!(cause(Some(""), true, None), "engine_stop:unnamed");
        assert_eq!(
            cause(
                None,
                true,
                Some("The agent's running terminal could not be taken over")
            ),
            "takeover_failed"
        );
        assert_eq!(cause(None, false, Some("exit 3")), "start_failed");
        assert_eq!(cause(None, false, None), "ended_by_itself");
        assert_eq!(cause(None, true, Some("exit 1")), "ended_by_itself");
    }

    /// Owner 11-11 (D): an end the engine itself made at one of its bounds is named
    /// `engine_limit:<code>`, before or after ready, never `ended_by_itself`; a pane that
    /// closed by itself, a code that is not the problem's closing one and an asked stop keep
    /// their own causes.
    #[test]
    fn an_end_at_an_engine_bound_is_named_engine_limit() {
        let closed = |code: &str| format!("The agent process closed. ({code})");
        for (code, named) in super::ENGINE_LIMITS {
            assert_eq!(cause(None, true, Some(&closed(code))), named);
            assert_eq!(cause(None, false, Some(&closed(code))), named);
            assert_eq!(named, format!("engine_limit:{code}"));
        }
        assert_eq!(
            cause(None, true, Some(&closed("launch_closed"))),
            "ended_by_itself"
        );
        assert_eq!(
            cause(
                None,
                true,
                Some("(launch_deadline) then the agent process closed")
            ),
            "ended_by_itself"
        );
        assert_eq!(
            cause(Some("owner_stop"), true, Some(&closed("launch_deadline"))),
            "owner_stop"
        );
        assert_eq!(
            cause(
                Some("engine_stop:idle"),
                true,
                Some(&closed("launch_deadline"))
            ),
            "engine_stop:idle"
        );
        assert_eq!(
            cause(
                None,
                true,
                Some("The agent's running terminal could not be taken over (launch_deadline)")
            ),
            "takeover_failed"
        );
    }

    /// L1C r1 F1: a sub-agent turn the engine ended at one of its bounds is followed by the
    /// turn-ended stop, and its end is still named `engine_limit:<code>`; the same stop after
    /// the owner's task limit or a cancel, with no bound code, stays `engine_stop:turn_ended`.
    #[test]
    fn a_turn_ended_stop_after_an_engine_bound_is_named_engine_limit() {
        let closed = |code: &str| format!("The agent process closed. ({code})");
        for (code, named) in super::ENGINE_LIMITS {
            assert!(engine_bound(code));
            assert_eq!(cause(Some(TURN_ENDED), true, Some(&closed(code))), named);
        }
        assert!(!engine_bound("launch_closed"));
        assert_eq!(cause(Some(TURN_ENDED), true, None), TURN_ENDED);
        assert_eq!(
            cause(Some(TURN_ENDED), true, Some(&closed("launch_closed"))),
            TURN_ENDED
        );
    }

    #[test]
    fn a_project_keeps_its_newest_end_rows_and_a_bounded_problem() {
        let connection = Connection::open_in_memory().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("ends").unwrap();
        let other = ProjectId::new("other-ends").unwrap();
        let long = "p".repeat(1000);
        let end = |project, agent| End {
            project,
            agent,
            role: AgentRole::Master,
            harness: "codex",
            at_ms: agent,
            cause: "ended_by_itself",
            problem: Some(long.as_str()),
            turn_open: false,
            leftovers: false,
        };
        record(&connection, &end(&other, 1)).unwrap();
        for agent in 1..=ENDS_PER_PROJECT_MAX + 2 {
            record(&connection, &end(&project, agent)).unwrap();
        }
        let (count, oldest, longest): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), MIN(agent), MAX(length(problem)) FROM team_agent_ends WHERE project='ends'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, ENDS_PER_PROJECT_MAX);
        assert_eq!(oldest, 3);
        assert!(longest <= 300);
        let others: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM team_agent_ends WHERE project='other-ends'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(others, 1, "another project's rows stay");
    }
}
