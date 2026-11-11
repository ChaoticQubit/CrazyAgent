"""Tests trial 4 check N-E10 (OP-40) on a real temporary engine store and engine settings
file with a pool, through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e10.py. Exit 0 when every case holds.
"""

import tempfile
import unittest

from audit4_fixture import load_check, run
from test_audit import MARK, PROJECT, World

CHECK = load_check("N-E10")


def settings(pools, connections):
    """Engine settings text with the given pools and connections. Inputs: (id, members,
    projects) pools and (id, account) connections. Output: global.toml text."""
    lines = ["version = 1", "timeout_ms = 2000", "allowed_destinations = []", "", "[model_access]", "accounts = []", "connections = ["]
    for connection, account in connections:
        lines.append(f'  {{ id = "{connection}", provider = "anthropic", harness = "claude-code", protocol = "anthropic", account_ref = "{account}" }},')
    lines += ["]", "pools = ["]
    for pool, members, projects in pools:
        lines.append(f"  {{ id = \"{pool}\", members = {members!r}, projects = {projects!r}, roles = [], fallback_allowed = false }},".replace("'", '"'))
    lines.append("]")
    return "\n".join(lines) + "\n"


class PoolTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def pool(self, members=("solo",), projects=(PROJECT,)):
        """Writes the engine settings: pool "team" with the given members, all on one account
        unless named "other-*", plus a connection "elsewhere" outside the pool. Inputs: the
        members and the pool's projects."""
        connections = [(member, "acct-b" if member.startswith("other") else "acct-a") for member in members] + [("elsewhere", "acct-z")]
        (self.world.root / "global.toml").write_text(settings([("team", list(members), list(projects))], connections))

    def work(self, connection, start_s, end_s, role="sub", error=None):
        """One agent on a connection with one task over a span. Inputs: the connection, the
        span in seconds before now, the role and the task error. Output: the agent id."""
        w = self.world
        agent = w.agent(role, connection=connection)
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, status="failed" if error else "done",
              agent=agent, prompt="Add a function to calc.py.", result="Done.", error=error,
              created_ms=w.now - start_s * 1000, started_ms=w.now - start_s * 1000, finished_ms=w.now - end_s * 1000)
        return agent

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E10")
        self.assertEqual(CHECK.OP, ["OP-40"])
        self.assertTrue(CHECK.TITLE)

    def test_three_agents_at_once_on_a_one_account_pool_pass(self):
        self.pool()
        for _ in range(3):
            self.work("solo", 300, 100)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("pool team: accounts=1; agents on it=3; tasks=3; most at once=3", detail)

    def test_an_agent_that_waited_for_a_free_account_fails(self):
        self.pool()
        self.work("solo", 300, 100)
        self.work("solo", 280, 90)
        waited = self.work("solo", 260, 80, error="The account is busy: native_profile_in_use")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("an agent waited for a free account", detail)
        self.assertIn("team_tasks", detail)
        self.assertTrue(waited)

    def test_agents_one_after_another_are_not_exercised(self):
        self.pool()
        self.work("solo", 300, 200)
        self.work("solo", 190, 100)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("most at once=1", detail)

    def test_agents_outside_the_pool_and_teammates_are_not_counted(self):
        self.pool(members=("solo", "other-1"))
        self.work("solo", 300, 100)
        self.work("other-1", 300, 100)
        self.work("elsewhere", 300, 100)
        self.work("solo", 300, 100, role="teammate")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("accounts=2; agents on it=2; tasks=2; most at once=2", detail)

    def test_a_project_in_no_pool_is_unknown(self):
        self.pool(projects=())
        self.work("solo", 300, 100)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no pool serves the project", detail)


if __name__ == "__main__":
    unittest.main()
