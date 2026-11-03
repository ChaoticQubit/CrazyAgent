from datetime import datetime, timezone
import json
from pathlib import Path
import re

ID = "N-E11"
OP = ["OP-24"]
TITLE = "No macOS app crash dialog from an app the engine's agents started during the trial"

REPORT_BYTES_MAX = 256 * 1024
REPORTS_MAX = 5000
FIELDS = ("procName", "procPath", "parentProc", "responsibleProc", "coalitionName", "captureTime")
APP_MAIN = re.compile(r"\.app/contents/macos/", re.IGNORECASE)


def folders():
    """The folders macOS keeps crash reports in for this user, newest first.
    Input: none, read from the home folder. Output: the existing folders.
    """
    base = Path.home() / "Library" / "Logs" / "DiagnosticReports"
    return [folder for folder in (base, base / "Retired") if folder.is_dir()]


def fields(path):
    """Reads the parts of one .ips crash report that say what crashed and who started it.
    Input: the report path. Output: a dict of the app name and FIELDS, or None when unreadable.
    """
    try:
        with open(path, "rb") as handle:
            text = handle.read(REPORT_BYTES_MAX).decode("utf-8", "replace")
    except OSError:
        return None
    head, _, body = text.partition("\n")
    try:
        header = json.loads(head)
    except ValueError:
        header = {}
    found = {}
    for key in FIELDS:
        match = re.search(rf'"{key}"\s*:\s*"((?:[^"\\]|\\.)*)"', body)
        found[key] = match.group(1) if match else ""
    found["app"] = str(header.get("app_name") or header.get("name") or found["procName"] or path.name)
    return found


def check(t):
    """Fails when macOS wrote a crash report, the kind that pops a "quit unexpectedly" dialog,
    for an app bundle the engine started after the trial's first agent began (OP-24).
    Input: the trial context. Output: (result, detail).
    """
    started = [agent["created"] for agent in t.agents.values() if isinstance(agent.get("created"), int)]
    if not started:
        return ("UNKNOWN", "not exercised: the project has no team agents, so there is no trial window")
    since = min(started) / 1000
    where = folders()
    if not where:
        return ("UNKNOWN", "no crash report folder under ~/Library/Logs/DiagnosticReports")
    read = 0
    blamed = []
    for folder in where:
        for path in sorted(folder.glob("*.ips"))[:REPORTS_MAX]:
            try:
                if path.stat().st_mtime < since:
                    continue
            except OSError:
                continue
            report = fields(path)
            if report is None:
                continue
            read += 1
            engine = report["responsibleProc"] == "crazyagent" or report["coalitionName"].startswith("dev.crazyagent")
            if engine and APP_MAIN.search(report["procPath"]):
                blamed.append(f"{report['app']} at {report['captureTime'] or 'unknown time'} from {report['parentProc'] or 'unknown parent'} ({path.name})")
    window = datetime.fromtimestamp(since, tz=timezone.utc).isoformat(timespec="seconds")
    detail = f"crash reports since the first agent started at {window}: {read}; app crashes the engine started: {len(blamed)}"
    if blamed:
        return ("FAIL", detail + ": " + "; ".join(blamed[:10]) + (f"; +{len(blamed) - 10} more" if len(blamed) > 10 else ""))
    return ("PASS", detail)
