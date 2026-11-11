"""Trial 4 check N-E3 (DEC-114 OP-52, B-11): agent time limits are Off unless the owner
sets them, and no task of the trial ended because one ran out.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw 5 of 11 sub-agent tasks fail with "The sub-agent ran out of
time" at about 15 minutes and asked why a time limit exists at all.
"""

ID = "N-E3"
OP = ["OP-52"]
TITLE = "Agent time limits are Off by default and no task ran out of time"

TIMED_OUT = (
    "ran out of time",
    "stopped by a time limit",
    "limit; change it in Team settings",
    "the longest one agent process may run",
    "team_turn_limit",
)
IDLE_DEFAULT_SECS = 600
LISTED_MAX = 8
WORDS_MAX = 160


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def limits(t):
    """Reads the project's stored time limits. Input: the trial. Output: the task, session
    and idle seconds, None for Off, with the engine's idle default when no row exists."""
    rows = t.q("SELECT turn_secs, session_secs, idle_secs FROM team_time_limits WHERE project=?", t.pid)
    assert len(rows) <= 1, "team_time_limits keys rows by project"
    return rows[0] if rows else (None, None, IDLE_DEFAULT_SECS)


def spoken(secs):
    """Names a stored limit. Input: seconds, or None or 0 for Off. Output: the words."""
    return f"{secs} s" if secs else "Off"


def check(t):
    """Runs N-E3. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    if not t.columns("team_time_limits"):
        return ("UNKNOWN", "the engine store has no team_time_limits table")
    turn, session, idle = limits(t)
    detail = f"task limit {spoken(turn)}; session limit {spoken(session)}; idle stop {spoken(idle)}"
    failed = [f"{kind} limit {secs} s is set" for kind, secs in (("task", turn), ("session", session)) if secs is not None]
    tasks = t.q(
        "SELECT id, agent, started_ms, finished_ms, error, result FROM team_tasks WHERE project=? AND finished_ms IS NOT NULL ORDER BY id",
        t.pid,
    )
    for task_id, agent_id, _, _, error, result in tasks:
        text = " ".join(part for part in (error, result) if isinstance(part, str))
        if any(phrase in text for phrase in TIMED_OUT):
            who = t.label(t.agents[agent_id]) if agent_id in t.agents else f"agent {agent_id}"
            failed.append(f"task {task_id} of {who}: {text[:WORDS_MAX]}")
    ran = [finished - started for _, _, started, finished, _, _ in tasks if started is not None and finished >= started]
    detail += f"; tasks finished={len(tasks)}" + (f"; longest {max(ran) // 1000} s" if ran else "")
    if failed:
        return ("FAIL", f"{detail}; {listed(failed)}")
    if not tasks:
        return ("UNKNOWN", f"{detail}; no task finished")
    return ("PASS", detail)
