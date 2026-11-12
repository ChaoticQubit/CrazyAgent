"""Trial 4 check N-E6 (DEC-114 OP-11, B-28; DEC-111 Q17): with the project's agent network on
Port 443 only, a tools-on agent reaches a host on port 443 and is denied the same host on
port 80.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The trial sets the network before the owner's prompt, and the prompt has one
sub-agent run two curl probes that print port443=<code> and port80=<code>. curl prints 000
for a connection it could not make, so 443 answered and 80 at 000 on one host is the
sandbox's port rule, not a dead network.
"""

import json
import re

import audit

ID = "N-E6"
OP = ["OP-11"]
TITLE = "Under Port 443 only, a tools-on agent reaches port 443 and is denied port 80"

MARKER = re.compile(r"port(?:443|80)=")
PROBE = re.compile(r"port(443|80)=(\d{3})")
NOT_REACHED = "000"
LINES_PER_FILE_MAX = 200_000
REASON_MAX = 120


def probes(folders):
    """Reads the last port probe outputs from Claude Code transcripts. Input: the folders.
    Output: {port: (code, output)} for each port a probe call printed."""
    calls, found = set(), {}
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
                for part in content if isinstance(content, list) else []:
                    if not isinstance(part, dict):
                        continue
                    if part.get("type") == "tool_use" and isinstance(part.get("id"), str):
                        if MARKER.search(json.dumps(part.get("input"), ensure_ascii=False)):
                            calls.add(part["id"])
                    elif part.get("type") == "tool_result" and part.get("tool_use_id") in calls:
                        output = audit.result_text(part.get("content"))
                        for port, code in PROBE.findall(output):
                            found[port] = (code, output)
    return found


def reason(output):
    """The first line of a probe's output that is not the probe's own line. Input: the
    output. Output: the masked line, cut to REASON_MAX characters, or an empty string."""
    lines = [line.strip() for line in output.splitlines() if line.strip() and not PROBE.fullmatch(line.strip())]
    return audit.mask(lines[0])[:REASON_MAX] if lines else ""


def check(t):
    """Runs N-E6. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    rows = t.q("SELECT body FROM project_agent_policy WHERE project=?", t.pid) if t.columns("project_agent_policy") else []
    try:
        body = json.loads(rows[0][0]) if rows else {}
    except ValueError:
        return ("UNKNOWN", "the project's agent policy is not JSON")
    network = body.get("network", "open") if isinstance(body, dict) else "open"
    if network != "https":
        return ("UNKNOWN", f"network={network}; the trial did not set Port 443 only")
    parts, failed = [], []
    for agent_id, agent in t.agents.items():
        found = probes(t.dirs_of.get(agent_id, []))
        if "443" not in found or "80" not in found:
            continue
        label = t.label(agent)
        (code_443, out_443), (code_80, out_80) = found["443"], found["80"]
        if code_443 == NOT_REACHED:
            failed.append(f"{label}: port 443 was not reached ({reason(out_443) or 'no curl error'})")
        if code_80 != NOT_REACHED:
            failed.append(f"{label}: port 80 answered {code_80} under Port 443 only")
        if code_443 != NOT_REACHED and code_80 == NOT_REACHED:
            parts.append(f"{label}: port 443 answered {code_443}; port 80 denied ({reason(out_80) or 'no curl error'})")
    if failed:
        return ("FAIL", "network=https; " + "; ".join(parts + failed))
    if not parts:
        return ("UNKNOWN", "network=https; no agent ran both port probes")
    return ("PASS", "network=https; " + "; ".join(parts))
