"""Trial 4 check N-E9 (DEC-114 OP-26): a lead reports done only after its workers' work
and the commands it started in the background have ended.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw a lead finish in under a minute with no visible work: it had
ended its turn waiting on a background cargo check, and the engine ended the task with it.
"""

from collections import Counter
from datetime import datetime
import json
import re

import audit

ID = "N-E9"
OP = ["OP-26"]
TITLE = "A lead reports done only after its workers' work and its own background commands end"

NOTICE = "<task-notification>"
NOTICE_TASK = re.compile(r"<task-id>\s*([^<\s]+)\s*</task-id>")
NOTICE_STATUS = re.compile(r"<status>\s*([A-Za-z_]+)\s*</status>")
LOST = "didn't finish before the previous session ended"
WORK_FILE = re.compile(r"\b[\w-]{2,}(?:/[\w.-]+)*\.(?:py|rs|md|js|mjs|ts|tsx|json|toml|txt|sh|yaml|yml|html|css)\b")
LINES_PER_FILE_MAX = 200_000
BACKGROUND_PER_AGENT_MAX = 256
LISTED_MAX = 8
WAKE_PROMPTS = ("A message from your team woke you.", "The message that woke you was already delivered")
DETACHED_MS_MAX = 5_000


def stamp_ms(value):
    """Reads a transcript's ISO time. Input: the value. Output: ms since the epoch, or None."""
    if not isinstance(value, str):
        return None
    try:
        return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)
    except ValueError:
        return None


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def marks(entry, at, found):
    """Records the background commands one transcript entry starts or ends: Claude Code's
    toolUseResult.backgroundTaskId, a TaskStop's task_id and <task-notification> ids with
    their status. Inputs: the entry, its time and the agent's findings. Output: none."""
    texts = []
    kind = entry.get("type")
    if kind == "user":
        result = entry.get("toolUseResult")
        if isinstance(result, dict):
            started, stopped = result.get("backgroundTaskId"), result.get("task_id")
            if isinstance(started, str) and started and len(found["started"]) < BACKGROUND_PER_AGENT_MAX:
                found["started"].append((started, at))
            if isinstance(stopped, str) and stopped and len(found["ended"]) < BACKGROUND_PER_AGENT_MAX:
                found["ended"].setdefault(stopped, (at, "stopped"))
        message = entry.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if isinstance(content, str):
            texts = [content]
        elif isinstance(content, list):
            texts = [p["text"] for p in content if isinstance(p, dict) and p.get("type") == "text" and isinstance(p.get("text"), str)]
    elif kind == "attachment" and isinstance(entry.get("attachment"), dict):
        texts = [value for value in entry["attachment"].values() if isinstance(value, str)]
    for text in texts:
        if NOTICE not in text:
            continue
        status = NOTICE_STATUS.search(text)
        for task_id in NOTICE_TASK.findall(text):
            if len(found["ended"]) >= BACKGROUND_PER_AGENT_MAX:
                break
            found["ended"].setdefault(task_id, (at, status.group(1) if status else "unknown"))
    assert len(found["started"]) <= BACKGROUND_PER_AGENT_MAX and len(found["ended"]) <= BACKGROUND_PER_AGENT_MAX


def scan(t, agent_id):
    """Reads one agent's Claude Code transcripts. Inputs: the trial and the agent id.
    Output: its background starts and ends, lost-command notices and last reply time."""
    found = {"started": [], "ended": {}, "lost": 0, "last": None}
    for path in audit.transcript_files(t.dirs_of.get(agent_id, [])):
        try:
            handle = path.open("r", encoding="utf-8", errors="replace")
        except OSError:
            continue
        with handle:
            for number, line in enumerate(handle, 1):
                if number > LINES_PER_FILE_MAX:
                    break
                if LOST in line:
                    found["lost"] += 1
                try:
                    entry = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(entry, dict):
                    continue
                at = stamp_ms(entry.get("timestamp"))
                if at is not None and entry.get("type") == "assistant":
                    found["last"] = max(found["last"] or at, at)
                marks(entry, at, found)
    return found


def late_wake(row, lead_end):
    """Tells an engine wake that began after the lead ended or never began. Its prompt and
    start alone cannot tell an acknowledgement from work, so the check prints how many it
    leaves out. Inputs: the task row and the lead's end in ms. Output: the answer."""
    return (row[5] or "").startswith(WAKE_PROMPTS) and (row[2] is None or row[2] > lead_end)


def worker_end(worker, tasks, scans, lead_end):
    """When a worker's work ended: its last task's end, leaving out wakes begun after the
    lead ended, or its last reply for a teammate with no task. Inputs: the worker, tasks by
    agent, scans and the lead's end. Output: ms, "open" while a task of it has no end, or
    None when nothing tells."""
    every = tasks.get(worker["id"], [])
    if not every:
        return scans[worker["id"]]["last"]
    own = [row for row in every if not late_wake(row, lead_end)]
    if any(row[3] is None for row in own):
        return "open"
    return max((row[3] for row in own), default=None)


def lead_findings(t, tasks, scans):
    """Compares each lead's last done task with its workers' work. Inputs: the trial, tasks
    by agent and scans. Output: leads compared, early ends, results that name no work and
    the ids of the late wakes left out."""
    compared, early, unnamed, late = 0, [], [], []
    for lead in t.leads:
        done = [row for row in tasks.get(lead["id"], []) if row[4] == "done" and row[3] is not None]
        workers = t.children[lead["id"]]
        if not done or not workers:
            continue
        last = max(done, key=lambda row: row[3])
        ends = [(worker, worker_end(worker, tasks, scans, last[3])) for worker in workers]
        if all(end is None for _, end in ends):
            continue
        compared += 1
        late += [row[0] for worker in workers for row in tasks.get(worker["id"], []) if late_wake(row, last[3])]
        name = f"task {last[0]} of {t.label(lead)}"
        for worker, end in ends:
            if end == "open":
                early.append(f"{name} done while {t.label(worker)} still had an open task")
            elif end is not None and end > last[3]:
                early.append(f"{name} done {end - last[3]} ms before {t.label(worker)}'s work ended")
        work = set()
        for worker in workers:
            work.update(word for word in (worker["handle"], worker["persona"]) if word)
            for row in tasks.get(worker["id"], []):
                work.update(WORK_FILE.findall(row[5] or ""))
        text = last[6] or ""
        if work and not any(word in text for word in work):
            unnamed.append(f"{name} names none of its workers' work ({listed(sorted(work))})")
    return compared, early, unnamed, late


def background_findings(t, tasks, scans):
    """Finds tasks that ended while a command their agent started in the background still
    ran. A command that ended within DETACHED_MS_MAX of its start, as `nohup ... &` does,
    never needed its turn held. Inputs: the trial, tasks by agent and scans. Output: tasks
    held by role, detached commands, early ends and the lost-command notices."""
    held, detached, early = Counter(), 0, []
    for agent_id, found in scans.items():
        role = "master" if agent_id in t.master_ids else "lead" if agent_id in t.lead_ids else "sub-agent"
        for command, at in found["started"]:
            own = next((row for row in tasks.get(agent_id, []) if at is not None and row[2] is not None
                        and row[2] <= at and (row[3] is None or at <= row[3])), None)
            if own is None or own[3] is None:
                continue
            name = f"task {own[0]} of {t.label(t.agents[agent_id])}"
            ended = found["ended"].get(command)
            if ended is None or ended[0] is None:
                early.append(f"{name} ended before its background command {command} ended (no end notice)")
            elif ended[0] > own[3]:
                early.append(f"{name} ended before its background command {command} ended ({ended[0] - own[3]} ms later)")
            elif ended[1] == "killed":
                early.append(f"{name}: its background command {command} was killed")
            elif ended[0] - at < DETACHED_MS_MAX:
                detached += 1
            else:
                held[role] += 1
    return held, detached, early, sum(found["lost"] for found in scans.values())


def check(t):
    """Runs N-E9. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    stored = t.q("SELECT mode FROM team_background WHERE project=?", t.pid) if t.columns("team_background") else []
    mode = stored[0][0] if stored else "wait"
    tasks = {}
    for row in t.q(
        "SELECT id,agent,started_ms,finished_ms,status,prompt,result FROM team_tasks WHERE project=? AND agent IS NOT NULL ORDER BY id",
        t.pid,
    ):
        tasks.setdefault(row[1], []).append(row)
    scans = {agent_id: scan(t, agent_id) for agent_id in t.agents}
    compared, early, unnamed, late = lead_findings(t, tasks, scans)
    detail = f"leads compared={compared}"
    detail += f"; late wakes left out={len(late)} (tasks {listed(sorted(late))})" if late else ""
    failed = early + unnamed
    exercised = True
    if mode == "wait":
        held, detached, cut, lost = background_findings(t, tasks, scans)
        detail += f"; mode=wait; lead background commands held={held['lead']}; sub-agent held={held['sub-agent']}"
        detail += f"; detached={detached}" if detached else ""
        detail += f"; lost in a later session={lost}" if lost else ""
        failed += cut + ([f"{lost} background commands lost with their session"] if lost else [])
        exercised = held["lead"] > 0
    else:
        detail += f"; mode={mode}: background commands are T3-20's"
    if failed:
        return ("FAIL", f"{detail}; {listed(failed)}")
    if not compared:
        return ("UNKNOWN", f"{detail}; no lead with workers finished a task")
    if not exercised:
        return ("UNKNOWN", f"{detail}; the lead background step was not exercised")
    return ("PASS", detail)
