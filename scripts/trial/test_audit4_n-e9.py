"""Tests trial 4 check N-E9 (OP-26) on a real temporary engine store, agent homes and
transcripts built with the engine's schema, through the audit's own Trial context.

Run: python3 scripts/trial/test_audit4_n-e9.py. Exit 0 when every case holds.
"""

import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

import audit
from test_audit import MARK, PROJECT, World, launch, prompt, iso

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("n_e9", HERE / "audit4" / "n-e9.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)
COMMAND = "sleep 40; date +%s > lead-bg-done.txt"
WAKE = "A message from your team woke you. It is at the end of this task, under Messages for you. Act on it."


def background(ms, task_id):
    """A Claude Code Bash call run in the background and its result line. Inputs: the time
    in ms and the background task id. Output: the two transcript entries."""
    use = {"type": "assistant", "timestamp": iso(ms), "message": {"content": [
        {"type": "tool_use", "id": f"toolu_{task_id}", "name": "Bash", "input": {"command": COMMAND, "run_in_background": True}}]}}
    done = {"type": "user", "timestamp": iso(ms), "toolUseResult": {"backgroundTaskId": task_id, "stdout": "", "stderr": ""},
            "message": {"content": [{"type": "tool_result", "tool_use_id": f"toolu_{task_id}",
                                     "content": f"Command running in background with ID: {task_id}"}]}}
    return [use, done]


def notification(ms, task_id, status="completed"):
    """The notification Claude Code writes when a background command ends. Inputs: the time,
    the task id and its status. Output: the transcript entry."""
    text = f"<task-notification>\n<task-id>{task_id}</task-id>\n<status>{status}</status>\n</task-notification>"
    return {"type": "user", "timestamp": iso(ms), "message": {"content": text}}


class LeadDoneTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def team(self, lead_end=-10_000, result="Merged add() and sub() into calc.py.", bg="held", worker_end=-90_000,
             late=None, late_start=5_000):
        """One app-spawned lead with two sub-agents that each add a function to calc.py, the
        lead's first and final tasks, and its background command in the final task. Inputs:
        the lead's final task end and result, how its background command ends ("held", "lost",
        "late", "detached" or None), the later worker's task end as an offset from now in ms,
        and the prompt and start of its later task, as an offset from the lead's end. Output: none."""
        w = self.world
        now = w.now
        lead = w.agent("sub", mode="app_spawned")
        w.row("team_names", agent=lead, project=PROJECT, persona="Grace", handle="calc-lead")
        for number, (persona, end) in enumerate((("Ada", -100_000), ("Alan", worker_end))):
            worker = w.agent("sub", parent=lead)
            w.row("team_names", agent=worker, project=PROJECT, persona=persona, handle=f"calc-{number}")
            w.row("team_tasks", project=PROJECT, source="agent", harness="claude-code", tools=1, status="done", agent=worker,
                  prompt=f"Add {'add' if number == 0 else 'sub'}(a, b) to calc.py and commit it on your branch.",
                  result="Done.", created_ms=now - 280_000, started_ms=now - 280_000, finished_ms=now + end, parent=lead)
        if late is not None:
            begun = now + lead_end + late_start
            w.row("team_tasks", project=PROJECT, source="engine", harness="claude-code", tools=1, status="done", agent=worker,
                  prompt=late, result="No response requested.", created_ms=begun, started_ms=begun,
                  finished_ms=now + lead_end + 9_000, parent=lead)
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, status="done", agent=lead,
              prompt="Lead B: two sub-agents add add() and sub() to calc.py.", result="Started Ada and Alan.",
              created_ms=now - 300_000, started_ms=now - 300_000, finished_ms=now - 285_000)
        started = now - 80_000
        finished = now + lead_end
        w.row("team_tasks", project=PROJECT, source="engine", harness="claude-code", tools=1, status="done", agent=lead,
              prompt="A message from your team woke you.", result=result,
              created_ms=started, started_ms=started, finished_ms=finished)
        entries = [launch("x"), prompt(started, "A message from your team woke you.")]
        if bg is not None:
            entries += background(started + 5_000, "bub9bmz4a")
        if bg == "held":
            entries.append(notification(finished - 5_000, "bub9bmz4a"))
        if bg == "detached":
            entries.append(notification(started + 5_500, "bub9bmz4a"))
        if bg == "late":
            entries.append(notification(finished + 30_000, "bub9bmz4a"))
        if bg == "lost":
            entries.append(prompt(finished + 60_000, "Background shell command didn't finish before the previous session ended."))
        w.transcript(lead, entries)

    def run_check(self):
        db = sqlite3.connect(f"file:{self.world.store}?mode=ro", uri=True)
        try:
            t = audit.Trial(db, self.world.root, PROJECT, MARK, "Hello")
            outcome, detail = CHECK.check(t)
        finally:
            db.close()
        self.assertIn(outcome, audit.RESULTS)
        self.assertIsInstance(detail, str)
        return outcome, detail

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E9")
        self.assertEqual(CHECK.OP, ["OP-26"])
        self.assertTrue(CHECK.TITLE)

    def test_a_lead_done_after_its_workers_and_its_background_command_passes(self):
        outcome, detail = self.run_check_for()
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("leads compared=1", detail)
        self.assertIn("lead background commands held=1", detail)

    def run_check_for(self, **case):
        self.team(**case)
        return self.run_check()

    def test_a_lead_done_before_a_worker_ends_fails(self):
        outcome, detail = self.run_check_for(lead_end=-120_000, bg=None)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("done 30000 ms before", detail)
        self.assertIn("(lead app_spawned)", detail)

    def test_a_worker_s_wake_that_began_after_the_lead_ended_is_not_work_the_lead_left(self):
        outcome, detail = self.run_check_for(late=WAKE)
        self.assertEqual(outcome, "PASS", detail)

    def test_a_worker_s_wake_that_began_before_the_lead_ended_still_counts(self):
        outcome, detail = self.run_check_for(late=WAKE, late_start=-1_000)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("done 9000 ms before", detail)

    def test_an_assigned_task_that_began_after_the_lead_ended_still_fails(self):
        outcome, detail = self.run_check_for(late="Add mul(a, b) to calc.py and commit it on your branch.")
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("done 9000 ms before", detail)

    def test_a_background_command_that_ended_at_once_proves_no_hold(self):
        outcome, detail = self.run_check_for(bg="detached")
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("lead background commands held=0", detail)
        self.assertIn("detached=1", detail)

    def test_a_lead_result_that_names_none_of_its_workers_work_fails(self):
        outcome, detail = self.run_check_for(result="All done.")
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("names none of its workers' work", detail)

    def test_a_lead_task_that_ends_before_its_background_command_fails(self):
        outcome, detail = self.run_check_for(bg="lost")
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("ended before its background command bub9bmz4a ended (no end notice)", detail)
        self.assertIn("lost in a later session=1", detail)
        self.tearDown()
        self.setUp()
        outcome, detail = self.run_check_for(bg="late")
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("ended before its background command bub9bmz4a ended (30000 ms later)", detail)

    def test_a_native_lead_done_before_its_teammate_s_last_reply_in_the_teammate_home_fails(self):
        w = self.world
        now = w.now
        lead = w.agent("sub", mode="native", worktree="/w/agent-lead")
        w.row("team_names", agent=lead, project=PROJECT, persona="Grace", handle="greet-lead")
        mate = w.agent("teammate", parent=lead, pane="%1", worktree="/w/agent-lead/.worktrees/greeter")
        w.row("team_names", agent=mate, project=PROJECT, persona="Greeter", handle="greeter")
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, status="done", agent=lead,
              prompt="Lead A: a native team writes greet.py.", result="Merged greet.py from greeter.",
              created_ms=now - 300_000, started_ms=now - 300_000, finished_ms=now - 10_000)
        w.transcript(lead, [launch("x")], worktree="/w/agent-lead")
        folder = w.root / "team" / "agents" / f"agent-{lead}" / "mates-home" / "projects" / audit.slug_of("/w/agent-lead")
        folder.mkdir(parents=True, exist_ok=True)
        reply = {"type": "assistant", "timestamp": iso(now - 2_000), "message": {"content": [{"type": "text", "text": "Wrote greet.py."}]}}
        with (folder / "greeter.jsonl").open("w", encoding="utf-8") as handle:
            for entry in (launch("Mate."), reply):
                handle.write(json.dumps(dict(entry, agentName="greeter", teamName="session-1")) + "\n")
        outcome, detail = self.run_check()
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("leads compared=1", detail)
        self.assertIn(f"done 8000 ms before {mate}(teammate)'s work ended", detail)

    def test_a_run_whose_leads_started_no_background_command_is_unknown(self):
        outcome, detail = self.run_check_for(bg=None)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("the lead background step was not exercised", detail)

    def test_a_run_with_no_lead_that_finished_with_workers_is_unknown(self):
        self.world.agent("sub", mode="app_spawned")
        outcome, detail = self.run_check()
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("leads compared=0", detail)

    def test_the_background_case_is_left_to_t3_20_under_stop(self):
        self.world.row("team_background", project=PROJECT, mode="stop", grace_secs=None)
        outcome, detail = self.run_check_for(bg="lost")
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("mode=stop: background commands are T3-20's", detail)


if __name__ == "__main__":
    unittest.main()
