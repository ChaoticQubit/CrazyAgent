"""Trial 4 check N-E5 (DEC-114 OP-11, B-22): a sandboxed agent runs the project's own test
command, cargo test on the tiny crate the runner seeds, with exit 0, no sandbox denial and
no toolchain download by any agent.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw an agent sandbox with no Rust toolchain: rustup fetched 1.3 GB
over the open network and cargo was blocked. The seeded Cargo.toml makes the engine grant
the owner's Rust toolchain, so a passing run reads it in place. The last cargo test run of
an agent decides its exit; a denial or download in any run fails. Only Bash calls are
runs: a team tool's input that names cargo test is not one. The linker's note that xcrun
could not write its cache in the per-user temp folder, which the sandbox denies on purpose
(DEC-104), is a toolchain note, not a denial, only in a toolchain call that runs no git, by
the same rule as T3-09 (audit.toolchain_call).
"""

import json
import re

import audit

ID = "N-E5"
OP = ["OP-11"]
TITLE = "A sandboxed agent runs the project's cargo test with exit 0, no sandbox denial and no toolchain download"

CARGO_TEST = re.compile(r"\bcargo\s+(?:\+\S+\s+)?test\b")
DENIAL = re.compile(r"Operation not permitted|Permission denied")
XCRUN_CACHE = re.compile(r"xcrun: error: couldn't create cache file '[^']*xcrun_db[^']*' \(errno=Operation not permitted\)")
SHELL = "Bash"
DOWNLOAD = re.compile(r"info: (?:syncing channel updates|downloading component|installing component)|rustup-init|sh\.rustup\.rs|rustup\s+(?:toolchain\s+)?install")
RESULT = "test result:"
PASSED = "test result: ok."
UNSANDBOXED = "full"
LINES_PER_FILE_MAX = 200_000
LINE_MAX = 120


def tool_runs(folders):
    """Reads every Bash call with its result from Claude Code transcripts. Input: the
    transcript folders or files. Output: (command, output text, failed) tuples in order."""
    given, runs = {}, []
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
                    if part.get("type") == "tool_use" and part.get("name") == SHELL and isinstance(part.get("id"), str):
                        value = part.get("input")
                        command = value.get("command") if isinstance(value, dict) else None
                        if isinstance(command, str):
                            given[part["id"]] = command
                    elif part.get("type") == "tool_result" and part.get("tool_use_id") in given:
                        runs.append((given[part["tool_use_id"]], audit.result_text(part.get("content")), part.get("is_error") is True))
    return runs


def first_line(text, pattern):
    """The first line of a text a pattern finds. Inputs: the text and a compiled pattern or
    a plain string. Output: the stripped, masked line cut to LINE_MAX, or None."""
    for line in text.splitlines():
        if (pattern.search(line) if isinstance(pattern, re.Pattern) else pattern in line):
            return audit.mask(line.strip())[:LINE_MAX]
    return None


def denial(text, toolchain):
    """The first sandbox denial line of a run's output, the xcrun cache note left out of a
    toolchain call. Inputs: the output and whether its command is a toolchain call that runs
    no git. Output: the masked line cut to LINE_MAX, or None."""
    for line in text.splitlines():
        if DENIAL.search(line) and not (toolchain and XCRUN_CACHE.search(line)):
            return audit.mask(line.strip())[:LINE_MAX]
    return None


def check(t):
    """Runs N-E5. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    rows = t.q("SELECT body FROM project_agent_policy WHERE project=?", t.pid) if t.columns("project_agent_policy") else []
    try:
        body = json.loads(rows[0][0]) if rows else {}
    except ValueError:
        return ("UNKNOWN", "the project's agent policy is not JSON")
    access = body.get("access", "sandboxed") if isinstance(body, dict) else "sandboxed"
    if access == UNSANDBOXED:
        return ("UNKNOWN", f"access={access}; agents run without the sandbox")
    parts, failed = [], []
    for agent_id, agent in t.agents.items():
        label = t.label(agent)
        runs = tool_runs(t.dirs_of.get(agent_id, []))
        for command, output, _ in runs:
            fetched = first_line(command, DOWNLOAD) or first_line(output, DOWNLOAD)
            if fetched:
                failed.append(f"{label} downloaded a toolchain: {fetched}")
                break
        tests = [(audit.toolchain_call(command), output, error) for command, output, error in runs if CARGO_TEST.search(command)]
        if not tests:
            continue
        denied = next((line for line in (denial(output, toolchain) for toolchain, output, _ in tests) if line), None)
        toolchain, output, error = tests[-1]
        summary = first_line(output, RESULT) or first_line(output, re.compile(r"\S")) or "no output"
        cache = first_line(output, XCRUN_CACHE) if toolchain else None
        note = f"; toolchain note: {cache}" if cache else ""
        if denied:
            failed.append(f"{label}: cargo test was denied by the sandbox ({denied})")
        elif error or PASSED not in output:
            failed.append(f"{label}: cargo test did not exit 0 ({summary}){note}")
        else:
            parts.append(f"{label}: cargo test exit 0 ({summary}){note}")
    if failed:
        return ("FAIL", f"access={access}; " + "; ".join(parts + failed))
    if not parts:
        return ("UNKNOWN", f"access={access}; no agent ran cargo test")
    return ("PASS", f"access={access}; " + "; ".join(parts) + "; no sandbox denial; no toolchain download")
