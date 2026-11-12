"""Tests trial 4 check N-E1 (OP-7) on a real temporary engine store with the engine's own
accounts and native sign-in tables, through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e1.py. Exit 0 when every case holds.
"""

import hashlib
import json
import tempfile
import unittest

from audit4_fixture import add_tables, load_check, run
from test_audit import MARK, PROJECT, World

CHECK = load_check("N-E1")
TRIAL = "ca-trial-4-claude"


class SignInTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        add_tables(self.world.store, "accounts", "native_auth_operations", "native_auth_fences", "native_auth_doubts")

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def account(self, name, *phases):
        """Saves one native-managed Claude account and its sign-in operations, one per phase in
        generation order. Inputs: the account name and the phases. Output: its profile key and
        the operation ids."""
        w = self.world
        profile = f"profile-{name}"
        connection = {"id": f"conn-{name}", "account_ref": f"acct-{name}", "provider": "anthropic", "harness": "claude-code",
                      "auth": {"method": "native_managed", "profile_ref": profile}}
        w.row("accounts", id=f"acct-{name}", body=json.dumps({"connection": connection, "details": {"name": name, "provider": "anthropic"}}))
        key = hashlib.sha256(profile.encode()).hexdigest()
        ids = []
        for generation, phase in enumerate(phases, 1):
            ids.append(f"op-{name}-{generation}")
            w.row("native_auth_operations", id=ids[-1], profile_key=key, generation=generation, phase=phase,
                  request=json.dumps({"method": "sign_in", "scope": {"connection": connection["id"]}}).encode(),
                  fact=json.dumps({"phase": phase, "generation": generation, "binding": {"connection": connection}}).encode())
        return key, ids

    def fence(self, key, operation, generation):
        """Holds the sign-in fence of a profile. Inputs: the key, operation id and generation."""
        self.world.row("native_auth_fences", profile_key=key, operation_id=operation, generation=generation,
                       db_instance="d" * 64, engine_epoch="e" * 64, custody_id="c" * 64, owner_id="o" * 64, owner_epoch=1)

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E1")
        self.assertEqual(CHECK.OP, ["OP-7"])
        self.assertTrue(CHECK.TITLE)

    def test_a_trial_account_signed_in_again_after_an_interrupted_attempt_passes(self):
        self.account("owner@example.com", "authenticated_observed")
        self.account(TRIAL, "unauthenticated_observed", "authenticated_observed")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn(f"{TRIAL} signed in at attempt 2", detail)
        self.assertIn("fences=0; left uncertain or in flight=0", detail)
        self.assertNotIn("owner@example.com", detail)

    def test_a_sign_in_fence_still_held_fails(self):
        key, ids = self.account(TRIAL, "unauthenticated_observed", "authenticated_observed")
        self.fence(key, ids[0], 1)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"a sign-in fence is still held for {TRIAL} by attempt 1", detail)

    def test_a_trial_account_left_uncertain_fails(self):
        self.account(TRIAL, "unauthenticated_observed", "uncertain")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{TRIAL} attempt 2 left uncertain", detail)

    def test_any_account_left_in_flight_fails(self):
        self.account(TRIAL, "authenticated_observed")
        self.account("Work", "authenticated_observed", "awaiting_owner")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("Work attempt 2 left awaiting_owner", detail)

    def test_an_uncertain_attempt_a_later_sign_in_settled_passes(self):
        self.account(TRIAL, "uncertain", "authenticated_observed")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("settled by a later attempt=1", detail)

    def test_a_run_without_a_trial_account_is_unknown(self):
        self.account("Work", "authenticated_observed")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no ca-trial account", detail)


if __name__ == "__main__":
    unittest.main()
