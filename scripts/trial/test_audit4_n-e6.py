"""Tests trial 4 check N-E6 (OP-11) on a real temporary engine store with the engine's own
project_agent_policy table and a sub-agent transcript holding its two curl probes, through
the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e6.py. Exit 0 when every case holds.
"""

import json
import tempfile
import unittest

from audit4_fixture import add_tables, load_check, run
from test_audit import MARK, PROJECT, World, call

CHECK = load_check("N-E6")
PROBE_443 = "curl -sS -m 10 -o /dev/null -w 'port443=%{http_code}\\n' https://example.com"
PROBE_80 = "curl -sS -m 10 -o /dev/null -w 'port80=%{http_code}\\n' http://example.com"
DENIED = "curl: (7) Failed to connect to example.com port 80 after 3 ms: Couldn't connect to server\nport80=000"


class NetworkTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        add_tables(self.world.store, "project_agent_policy")

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def network(self, choice):
        """Saves the project's agent policy with a network choice. Input: the choice id."""
        self.world.row("project_agent_policy", project=PROJECT, body=json.dumps({"access": "sandboxed", "toolchains": [], "network": choice}))

    def probes(self, out_443="port443=200", out_80=DENIED):
        """One lead's sub-agent that ran both probes. Inputs: what each printed, None for a
        probe not run. Output: the agent id."""
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        agent = w.agent("sub", parent=lead)
        entries = []
        if out_443 is not None:
            entries += call(1, "Bash", {"command": PROBE_443}, out_443, error=out_443.endswith("=000"))
        if out_80 is not None:
            entries += call(2, "Bash", {"command": PROBE_80}, out_80, error=out_80.endswith("=000"))
        w.transcript(agent, entries)
        return agent

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E6")
        self.assertEqual(CHECK.OP, ["OP-11"])
        self.assertTrue(CHECK.TITLE)

    def test_port_443_reached_and_port_80_denied_pass(self):
        self.network("https")
        agent = self.probes()
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("network=https", detail)
        self.assertIn(f"{agent}(sub-agent): port 443 answered 200; port 80 denied", detail)

    def test_port_80_reached_under_port_443_only_fails(self):
        self.network("https")
        self.probes(out_80="port80=200")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("port 80 answered 200 under Port 443 only", detail)

    def test_port_443_blocked_fails(self):
        self.network("https")
        self.probes(out_443="curl: (7) Failed to connect to example.com port 443 after 2 ms: Couldn't connect to server\nport443=000")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("port 443 was not reached", detail)

    def test_an_open_network_is_not_exercised(self):
        self.probes()
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("network=open", detail)

    def test_a_run_with_no_probe_is_unknown(self):
        self.network("https")
        self.probes(out_443=None, out_80=None)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no agent ran both port probes", detail)

    def test_an_agent_that_ran_only_the_port_443_probe_is_unknown(self):
        self.network("https")
        self.probes(out_80=None)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no agent ran both port probes", detail)


if __name__ == "__main__":
    unittest.main()
