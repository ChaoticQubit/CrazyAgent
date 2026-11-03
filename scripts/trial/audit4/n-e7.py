"""Trial 4 check N-E7 (DEC-114 OP-14, B-43): the live master starts quiet. Its first typed
turn is the owner's first message under the [owner] tag and nothing else, its role and the
app instructions reach it as launch system context, and its first turn reads fewer input
tokens than a set bound.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw the master typed a very long prompt with every agent and
instruction, start work before being asked, and burn 35,000 tokens. Master 39 (10-28)
measured an 11,164-character typed framing and 41,471 first-turn input tokens; the bound
sits below that.
"""

import json
from pathlib import Path

import audit

ID = "N-E7"
OP = ["OP-14"]
TITLE = "The live master starts quiet: its first turn is the owner's message, its instructions are system context"

OWNER_TAG = "[owner] "
ROLE = "You are the master agent of the project"
TYPED_MARKS = (ROLE, "Your sub-agents:", audit.APP_MARK)
FIRST_TURN_TOKENS_MAX = 40_000
USAGE_KEYS = ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")
LINES_PER_FILE_MAX = 200_000


def flat(text):
    """Joins every whitespace run into one space. Input: text. Output: the flat text."""
    return " ".join(text.split())


def typed_text(entry):
    """The text a user entry typed, or None for meta, tool results and wrapped commands.
    Input: the transcript entry. Output: the text or None."""
    message = entry.get("message")
    if entry.get("type") != "user" or entry.get("isMeta") is True or not isinstance(message, dict):
        return None
    content = message.get("content")
    if isinstance(content, list):
        content = "\n".join(p["text"] for p in content if isinstance(p, dict) and p.get("type") == "text" and isinstance(p.get("text"), str))
    if not isinstance(content, str) or not content.strip() or content.lstrip().startswith("<"):
        return None
    return content


def system_text(entry):
    """The launch system context an entry records: Claude Code's request shape or prompt
    snapshot. Input: the entry. Output: the text or None."""
    if entry.get("type") == "api-request-shape" and isinstance(entry.get("shape"), dict):
        parts = entry["shape"].get("system")
        if isinstance(parts, list):
            return "\n".join(p["text"] for p in parts if isinstance(p, dict) and isinstance(p.get("text"), str))
    attachment = entry.get("attachment")
    if entry.get("type") == "attachment" and isinstance(attachment, dict) and attachment.get("type") == "prompt_snapshot":
        parts = attachment.get("systemPrompt")
        if isinstance(parts, list):
            return "\n".join(part for part in parts if isinstance(part, str))
    return None


def session(path):
    """Reads one master session's start. Input: the transcript path. Output: a dict with the
    first system context, first typed turn, its time and the first reply's input tokens."""
    found = {"system": None, "typed": None, "at": None, "tokens": None}
    try:
        handle = path.open("r", encoding="utf-8", errors="replace")
    except OSError:
        return found
    with handle:
        for number, line in enumerate(handle, 1):
            if number > LINES_PER_FILE_MAX or (found["tokens"] is not None and found["system"] is not None):
                break
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if not isinstance(entry, dict):
                continue
            found["system"] = found["system"] or system_text(entry)
            if found["typed"] is None:
                found["typed"] = typed_text(entry)
                found["at"] = audit.stamp_ms(entry.get("timestamp")) if found["typed"] is not None else None
                continue
            usage = entry.get("message", {}).get("usage") if entry.get("type") == "assistant" and isinstance(entry.get("message"), dict) else None
            if isinstance(usage, dict):
                found["tokens"] = sum(value for value in (usage.get(key) for key in USAGE_KEYS) if isinstance(value, int))
    return found


def first_session(t):
    """Finds the master's first session: the top-level transcript whose first typed turn is
    earliest. Input: the trial. Output: the session dict or None."""
    sessions = []
    for master_id in t.master_ids:
        for folder in t.dirs_of.get(master_id, []):
            for path in sorted(Path(folder).glob("*/*.jsonl")) + sorted(Path(folder).glob("*.jsonl")):
                found = session(path)
                if found["typed"] is not None:
                    sessions.append(found)
    sessions.sort(key=lambda found: found["at"] if found["at"] is not None else float("inf"))
    return sessions[0] if sessions else None


def check(t):
    """Runs N-E7. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    rows = t.q("SELECT text FROM team_messages WHERE project=? AND author='owner' ORDER BY created_ms, id LIMIT 1", t.pid) if t.columns("team_messages") else []
    if not rows:
        return ("UNKNOWN", "no owner message reached the master")
    owner = rows[0][0]
    found = first_session(t)
    if found is None:
        return ("UNKNOWN", "no master transcript has a typed turn")
    typed, parts, failed, unread = found["typed"], [], [], []
    if flat(typed) == flat(OWNER_TAG + owner):
        parts.append(f"first typed turn is the owner's message ({len(owner)} chars)")
    elif flat(OWNER_TAG + owner) in flat(typed):
        failed.append(f"the first typed turn holds {len(typed)} chars, not only the owner's message ({len(owner)} chars)")
    else:
        failed.append("the first typed turn is not the owner's message")
    if any(mark in typed for mark in TYPED_MARKS):
        failed.append("the master's role text was typed")
    system = found["system"]
    if system is None:
        unread.append("the transcript records no launch system context")
    else:
        lacks = [name for name, mark in (("the master role", ROLE), ("the app instructions", audit.APP_MARK)) if mark not in system]
        failed += [f"the system context lacks {name}" for name in lacks]
        if not lacks:
            parts.append("system context holds the master role and the app instructions")
    tokens = found["tokens"]
    if tokens is None:
        unread.append("no reply with token usage followed the first typed turn")
    elif tokens > FIRST_TURN_TOKENS_MAX:
        failed.append(f"first turn input tokens={tokens} over the bound {FIRST_TURN_TOKENS_MAX}")
    else:
        parts.append(f"first turn input tokens={tokens} (bound {FIRST_TURN_TOKENS_MAX})")
    detail = "; ".join(parts + unread) or "nothing read"
    if failed:
        return ("FAIL", f"{detail}; " + "; ".join(failed))
    if unread:
        return ("UNKNOWN", detail)
    return ("PASS", detail)
