"""Trial 4 check N-E8 (DEC-114 OP-25, B-45; DEC-105): the master's sessions search returns
the input and the output text of one of the master's own tool calls, not only messages.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner found that past sessions keep tool counts but not tool inputs and
outputs, so they cannot be queried in full. A hit proves tool text only when it carries a
token that a master tool call's input (or output) holds and that no message, task, other
tool input (or output) or answer header holds, so a value the master repeated in a reply
or typed as the query proves nothing.
"""

import json
import re

import audit

ID = "N-E8"
OP = ["OP-25"]
TITLE = "The session search returns a master tool call's input and output text"

SESSIONS = re.compile(r"(?:^|__)sessions$")
TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/-]{7,}")
HIT = "- "
LINES_PER_FILE_MAX = 200_000
TOKENS_PER_TEXT_MAX = 200
TEXTS_MAX = 20_000
LISTED_MAX = 8


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = sorted(set(items))
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def read(folders):
    """Reads Claude Code transcripts. Input: the folders. Output: message texts, tool calls as
    {id: (name, input text)} and tool results as {id: output text}."""
    said, calls, results = [], {}, {}
    for path in audit.transcript_files(folders):
        try:
            handle = path.open("r", encoding="utf-8", errors="replace")
        except OSError:
            continue
        with handle:
            for number, line in enumerate(handle, 1):
                if number > LINES_PER_FILE_MAX:
                    break
                try:
                    entry = json.loads(line)
                except ValueError:
                    continue
                message = entry.get("message") if isinstance(entry, dict) else None
                content = message.get("content") if isinstance(message, dict) else None
                if isinstance(content, str):
                    said.append(content)
                    continue
                for part in content if isinstance(content, list) else []:
                    if not isinstance(part, dict):
                        continue
                    if part.get("type") == "text" and isinstance(part.get("text"), str):
                        said.append(part["text"])
                    elif part.get("type") == "tool_use" and isinstance(part.get("id"), str):
                        calls[part["id"]] = (str(part.get("name")), json.dumps(part.get("input"), ensure_ascii=False))
                    elif part.get("type") == "tool_result" and isinstance(part.get("tool_use_id"), str):
                        results[part["tool_use_id"]] = audit.result_text(part.get("content"))
    return said, calls, results


def tokens(text, *elsewhere):
    """The tokens of a text that none of the other corpora hold. Inputs: the text and
    lowercased corpora. Output: the lowercased tokens."""
    found = {token.lower() for token in TOKEN.findall(text)[:TOKENS_PER_TEXT_MAX]}
    return {token for token in found if not any(token in corpus for corpus in elsewhere)}


def check(t):
    """Runs N-E8. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    if not t.masters:
        return ("UNKNOWN", "the trial has no master")
    said = []
    for agent_id in t.agents:
        if agent_id not in t.master_ids:
            said += read(t.dirs_of.get(agent_id, []))[0]
    calls, results = {}, {}
    for master_id in t.master_ids:
        own_said, own_calls, own_results = read(t.dirs_of.get(master_id, []))
        said += own_said
        calls.update(own_calls)
        results.update(own_results)
    said += [row[0] for row in t.q("SELECT text FROM team_messages WHERE project=? LIMIT ?", t.pid, TEXTS_MAX)] if t.columns("team_messages") else []
    for prompt, result in t.q("SELECT prompt, result FROM team_tasks WHERE project=? LIMIT ?", t.pid, TEXTS_MAX):
        said += [text for text in (prompt, result) if isinstance(text, str)]
    searches = [results[key] for key, (name, _) in calls.items() if SESSIONS.search(name) and key in results]
    if not searches:
        return ("UNKNOWN", f"the master made no sessions call; master tool calls={len(calls)}")
    messages = "\n".join(text for text in said if isinstance(text, str)).lower()
    inputs = "\n".join(given for _, given in calls.values()).lower()
    outputs = "\n".join(text for key, text in results.items() if not SESSIONS.search(calls.get(key, ("", ""))[0])).lower()
    lines = [line for answer in searches for line in answer.splitlines() if line.startswith(HIT)]
    hits = "\n".join(lines).lower()
    found_in, found_out = [], []
    for key, (name, given) in calls.items():
        if SESSIONS.search(name):
            continue
        if any(token in hits for token in tokens(given, messages, outputs)):
            found_in.append(name)
        if key in results and any(token in hits for token in tokens(results[key], messages, inputs)):
            found_out.append(name)
    detail = (
        f"sessions calls={len(searches)}; hits={len(lines)}; "
        f"tool input text found: {listed(found_in) or 'none'}; tool output text found: {listed(found_out) or 'none'}"
    )
    if found_in and found_out:
        return ("PASS", detail)
    return ("FAIL", f"{detail}; the search returned no tool input or output text of the master's {len(calls) - len(searches)} other calls")
