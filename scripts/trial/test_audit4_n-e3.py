"""Tests trial 4 check N-E3 (OP-52) on a real temporary engine store with the engine's own
team_time_limits table, through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e3.py. Exit 0 when every case holds.
"""

import tempfile
import unittest

from audit4_fixture import add_tables, load_check, run
from test_audit import MARK, PROJECT, World

CHECK = load_check("N-E3")


class TimeLimitTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        add_tables(self.world.store, "team_time_limits")
        self.lead = self.world.agent("sub", mode="app_spawned")

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def task(self, ran_ms, status="done", error=None, result="Added add() to calc.py."):
        """One finished task of a lead's sub-agent. Inputs: how long it ran in ms, its status, error and
        result. Output: the agent and task ids."""
        w = self.world
        agent = w.agent("sub", parent=self.lead)
        w.row("team_names", agent=agent, project=PROJECT, persona="Ada", handle="calc-0")
        return agent, w.row("team_tasks", project=PROJECT, source="agent", harness="claude-code", tools=1, status=status, agent=agent,
                            prompt="Add add(a, b) to calc.py.", result=result, error=error, parent=self.lead,
                            created_ms=w.now - ran_ms, started_ms=w.now - ran_ms, finished_ms=w.now)

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E3")
        self.assertEqual(CHECK.OP, ["OP-52"])
        self.assertTrue(CHECK.TITLE)

    def test_no_limits_and_tasks_that_finished_pass(self):
        self.task(1_200_000)
        self.task(60_000)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("task limit Off; session limit Off", detail)
        self.assertIn("tasks finished=2; longest 1200 s", detail)

    def test_an_idle_stop_alone_is_not_a_task_time_limit(self):
        self.world.row("team_time_limits", project=PROJECT, turn_secs=None, session_secs=None, idle_secs=300)
        self.task(60_000)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("idle stop 300 s", detail)

    def test_a_task_or_session_limit_on_the_trial_project_fails(self):
        self.world.row("team_time_limits", project=PROJECT, turn_secs=900, session_secs=None, idle_secs=600)
        self.task(60_000)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("task limit 900 s is set", detail)

    def test_a_task_stopped_by_a_time_limit_fails_with_its_words(self):
        agent, stopped = self.task(900_000, status="failed", result=None,
                                   error="The sub-agent was stopped by the 15 min task limit; change it in Team settings")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"task {stopped} of {agent}(sub-agent)", detail)
        self.assertIn("stopped by the 15 min task limit", detail)

    def test_the_words_the_owner_saw_fail(self):
        agent, stopped = self.task(900_000, status="failed", result=None, error="The sub-agent ran out of time")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"task {stopped} of {agent}(sub-agent)", detail)
        self.assertIn("ran out of time", detail)

    def test_a_run_with_no_finished_task_is_unknown(self):
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no task finished", detail)


if __name__ == "__main__":
    unittest.main()
