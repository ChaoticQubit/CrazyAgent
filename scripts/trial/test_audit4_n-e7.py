"""Tests trial 4 check N-E7 (OP-14) on a real temporary engine store with the owner's message
and a live master transcript holding its launch system context, first typed turn and first
reply usage, through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e7.py. Exit 0 when every case holds.
"""

import tempfile
import unittest

from audit4_fixture import load_check, run
from test_audit import MARK, PROJECT, World, launch, prompt

CHECK = load_check("N-E7")
OWNER = "Spawn two Claude leads with harness claude-code.\n\nLead A: use spawn with mode native."
ROLE = "## Your role\n\nYou are the master agent of the project \"trial\", running in the owner's terminal. The owner types to you here."
SYSTEM = f"You are Claude Code.\n# Mandatory workflow\n\nGates in order.\n{ROLE}"
FRAMING = "You are the master agent of the project \"trial\". Sub-agents do the hands-on work.\nYour sub-agents:\n- Ada (idle)\n"


class FirstTurnTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        self.master = self.world.agent("master")

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def start(self, typed=None, system=SYSTEM, tokens=(2, 20_000, 8_000), owner=OWNER):
        """Stores the owner's message and writes the master's first session in Claude Code's
        order: first typed turn, launch context, first reply. Inputs: the typed text (the tagged owner message by
        default), the system context, the reply's input, cache-write and cache-read tokens,
        and the owner's message or None. Output: none."""
        w = self.world
        if owner is not None:
            w.row("team_messages", project=PROJECT, author="owner", text=owner, state="delivered", created_ms=w.now - 50_000)
        usage = dict(zip(("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"), tokens))
        reply = {"type": "assistant", "timestamp": prompt(w.now - 40_000)["timestamp"],
                 "message": {"content": [{"type": "text", "text": "Spawning the leads."}], "usage": usage}}
        w.transcript(self.master, [prompt(w.now - 45_000, typed if typed is not None else f"[owner] {OWNER}"), launch(system), reply])

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E7")
        self.assertEqual(CHECK.OP, ["OP-14"])
        self.assertTrue(CHECK.TITLE)

    def test_a_quiet_start_with_the_owner_message_first_passes(self):
        self.start()
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn(f"first typed turn is the owner's message ({len(OWNER)} chars)", detail)
        self.assertIn("system context holds the master role and the app instructions", detail)
        self.assertIn("first turn input tokens=28002 (bound 40000)", detail)

    def test_the_master_framing_typed_with_the_owner_message_fails(self):
        self.start(typed=FRAMING + f"[owner] {OWNER}")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"the first typed turn holds {len(FRAMING) + 8 + len(OWNER)} chars, not only the owner's message", detail)
        self.assertIn("the master's role text was typed", detail)

    def test_an_engine_line_typed_before_the_owner_spoke_fails(self):
        self.start(typed="[team] Ada finished task 3.")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("the first typed turn is not the owner's message", detail)

    def test_a_first_turn_over_the_token_bound_fails(self):
        self.start(tokens=(2, 36_000, 9_000))
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("first turn input tokens=45002 over the bound 40000", detail)

    def test_a_system_context_without_the_master_role_fails(self):
        self.start(system="You are Claude Code.\n# Mandatory workflow\n")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("the system context lacks the master role", detail)

    def test_a_run_where_the_owner_never_wrote_is_unknown(self):
        self.start(owner=None)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no owner message", detail)


if __name__ == "__main__":
    unittest.main()
