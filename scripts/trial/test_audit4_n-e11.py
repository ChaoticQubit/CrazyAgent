import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import time
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import audit

SPEC = importlib.util.spec_from_file_location("n_e11", HERE / "audit4" / "n-e11.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)
SCHEMA = (HERE / "fixture-schema.sql").read_text(encoding="utf-8")
PROJECT = "trial"


def report(folder, name, app, path, responsible, parent, age_s=0):
    """Writes one .ips crash report shaped as macOS writes them: a JSON header line, then the
    JSON body. Inputs: the folder, file name, app name, executable path, responsible process,
    parent process and how many seconds old its file is. Output: the path.
    """
    folder.mkdir(parents=True, exist_ok=True)
    header = {"app_name": app, "bug_type": "309", "name": app, "timestamp": "2026-11-03 13:00:00.00 -0500"}
    body = {"procName": app, "procPath": path, "parentProc": parent, "responsibleProc": responsible,
            "coalitionName": "dev.crazyagent.engine" if responsible == "crazyagent" else "com.apple.Terminal", "captureTime": "2026-11-03 13:00:00.0000 -0500"}
    target = folder / name
    target.write_text(json.dumps(header) + "\n" + json.dumps(body, indent=2) + "\n", encoding="utf-8")
    stamp = time.time() - age_s
    os.utime(target, (stamp, stamp))
    return target


class CrashDialogCheck(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-e11-")
        self.base = Path(os.path.realpath(self.folder.name))
        self.home = os.environ.get("HOME")
        os.environ["HOME"] = str(self.base)
        self.reports = self.base / "Library" / "Logs" / "DiagnosticReports"
        root = self.base / "engine"
        root.mkdir()
        store = root / "engine.db"
        db = sqlite3.connect(store)
        db.executescript(SCHEMA)
        db.execute("INSERT INTO projects (id, body) VALUES (?, ?)", (PROJECT, json.dumps({"spec": {"repositories": [str(self.base / "repo")]}})))
        db.commit()
        self.db, self.root = db, root

    def tearDown(self):
        self.db.close()
        if self.home is None:
            os.environ.pop("HOME", None)
        else:
            os.environ["HOME"] = self.home
        self.folder.cleanup()

    def agent(self, created_s_ago):
        columns = {row[1]: row for row in self.db.execute("PRAGMA table_info(team_agents)")}
        values = {"project": PROJECT, "role": "master", "harness": "claude-code", "status": "stopped", "created_ms": int((time.time() - created_s_ago) * 1000)}
        for name, (_, _, kind, required, default, key) in columns.items():
            if required and default is None and not key and name not in values:
                values[name] = 0 if "INT" in kind.upper() else ""
        self.db.execute(f"INSERT INTO team_agents ({', '.join(values)}) VALUES ({', '.join('?' * len(values))})", tuple(values.values()))
        self.db.commit()

    def run_check(self):
        store = audit.open_store(self.root / "engine.db", False)
        try:
            return CHECK.check(audit.Trial(store, self.root, PROJECT, "MARK", "Hello"))
        finally:
            store.close()

    def test_an_app_the_engine_started_that_crashed_in_the_trial_fails_and_others_pass(self):
        self.agent(created_s_ago=600)
        report(self.reports, "Google Chrome-old.ips", "Google Chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "crazyagent", "node", age_s=3600)
        report(self.reports, "Google Chrome-terminal.ips", "Google Chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "Terminal", "node")
        report(self.reports, "rust-objcopy.ips", "rust-objcopy", "/Users/x/.rustup/toolchains/stable/bin/rust-objcopy", "crazyagent", "cargo")
        result, detail = self.run_check()
        self.assertEqual(result, "PASS", detail)
        self.assertIn("crash reports since the first agent started", detail)
        self.assertIn(": 2;", detail)
        self.assertIn("app crashes the engine started: 0", detail)

        report(self.reports / "Retired", "Google Chrome for Testing-1.ips", "Google Chrome for Testing",
               "/Users/x/agent/home/.cache/puppeteer/chrome/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing", "crazyagent", "node")
        result, detail = self.run_check()
        self.assertEqual(result, "FAIL", detail)
        self.assertIn("app crashes the engine started: 1", detail)
        self.assertIn("Google Chrome for Testing at 2026-11-03 13:00:00.0000 -0500 from node", detail)
        self.assertNotIn("/Users/x", detail)

    def test_no_agent_or_no_report_folder_is_unknown(self):
        result, detail = self.run_check()
        self.assertEqual(result, "UNKNOWN", detail)
        self.assertIn("no team agents", detail)
        self.agent(created_s_ago=60)
        result, detail = self.run_check()
        self.assertEqual(result, "UNKNOWN", detail)
        self.assertIn("no crash report folder", detail)


if __name__ == "__main__":
    unittest.main()
