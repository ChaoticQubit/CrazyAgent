"""Trial 4 check N-E10 (DEC-114 OP-40, B-55; DEC-086): more of the project's agents work at
the same time than its pool has accounts, and no agent waits for a free account.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw the pool cap agents at one per account, so extra agents waited.
T3-27 proves two agents share one account; this check counts the whole pool.
"""

import tomllib

import audit

ID = "N-E10"
OP = ["OP-40"]
TITLE = "More agents work at once than the pool has accounts, and none waits for a free account"

WAIT_WORDS = ("native_profile_in_use", "free account")
SETTINGS_BYTES_MAX = 1024 * 1024
TASKS_MAX = 5000
LISTED_MAX = 8


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def project_pool(t):
    """Reads the engine settings for the pool that serves the project. Input: the trial.
    Output: the pool id, its member connections and its accounts, or None when no pool does."""
    path = t.root / "global.toml"
    if not path.is_file() or path.stat().st_size > SETTINGS_BYTES_MAX:
        return None
    try:
        access = tomllib.loads(path.read_text(encoding="utf-8")).get("model_access", {})
    except (tomllib.TOMLDecodeError, UnicodeDecodeError):
        return None
    pool = next((pool for pool in access.get("pools", []) if t.pid in pool.get("projects", [])), None)
    if pool is None:
        return None
    members = set(pool.get("members", []))
    account_of = {c.get("id"): c.get("account_ref") or c.get("id") for c in access.get("connections", []) if isinstance(c, dict)}
    return pool.get("id", "?"), members, {account_of.get(member, member) for member in members}


def most_at_once(spans):
    """Finds the most agents with a task running at one moment. Input: (start, end, agent)
    spans in ms. Output: the count and the agents of the first moment it is reached."""
    best, agents = 0, []
    for start, _, _ in spans:
        running = sorted({agent for other_start, other_end, agent in spans if other_start <= start < other_end})
        if len(running) > best:
            best, agents = len(running), running
    return best, agents


def waits(t):
    """Finds rows that say an agent waited for an account. Input: the trial. Output: the
    table and id of each."""
    likes = [f"%{word}%" for word in WAIT_WORDS]
    found = []
    for table, text in (
        ("team_agents", "coalesce(error,'')"),
        ("team_tasks", "coalesce(error,'') || ' ' || coalesce(result,'')"),
        ("team_events", "text"),
    ):
        if t.columns(table):
            for (row_id,) in t.q(f"SELECT id FROM {table} WHERE project=? AND ({text} LIKE ? OR {text} LIKE ?) ORDER BY id", t.pid, *likes):
                found.append(f"{table} {row_id}")
    return found


def check(t):
    """Runs N-E10. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    pool = project_pool(t)
    if pool is None:
        return ("UNKNOWN", "no pool serves the project in the engine settings")
    pool_id, members, accounts = pool
    agents = {
        agent_id: connection
        for agent_id, connection in t.q(
            "SELECT id, connection FROM team_agents WHERE project=? AND role<>'teammate' AND connection IS NOT NULL ORDER BY id", t.pid,
        )
        if connection in members
    }
    spans = []
    for agent_id, started, finished in t.q(
        "SELECT agent, started_ms, finished_ms FROM team_tasks WHERE project=? AND agent IS NOT NULL AND started_ms IS NOT NULL ORDER BY id LIMIT ?",
        t.pid, TASKS_MAX,
    ):
        if agent_id in agents:
            end = finished if finished is not None else max(started, t.agents[agent_id]["heartbeat"] or started)
            spans.append((started, end, agent_id))
    peak, together = most_at_once(spans)
    assert peak <= len(agents)
    detail = (
        f"pool {audit.mask(str(pool_id))}: accounts={len(accounts)}; agents on it={len(agents)}; tasks={len(spans)}; most at once={peak}"
        + (f" (agents {listed(str(agent) for agent in together)})" if together else "")
    )
    waited = waits(t)
    if waited:
        return ("FAIL", f"{detail}; an agent waited for a free account: {listed(waited)}")
    if peak > len(accounts):
        return ("PASS", detail)
    return ("UNKNOWN", f"{detail}; not exercised: no more agents worked at once than the pool has accounts")
