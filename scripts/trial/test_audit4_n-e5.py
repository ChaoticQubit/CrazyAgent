"""Tests trial 4 check N-E5 (OP-11) on a real temporary engine store with the engine's own
project_agent_policy table and agent transcripts holding cargo test runs and their output,
through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e5.py. Exit 0 when every case holds.
"""

import json
import tempfile
import unittest

from audit4_fixture import add_tables, load_check, run
from test_audit import MARK, PROJECT, World, call

CHECK = load_check("N-E5")
PASSED = (
    "   Compiling ca-trial-4 v0.1.0 (/w/trial-b-2)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.61s\n"
    "     Running unittests src/lib.rs (target/debug/deps/ca_trial_4-1)\n\nrunning 1 test\ntest tests::doubles ... ok\n\n"
    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
)
FAILED = "Exit code 101\nrunning 1 test\ntest tests::doubles ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
DENIED = "Exit code 101\nerror: failed to open: /Users/owner/.cargo/.package-cache\n\nCaused by:\n  Operation not permitted (os error 1)\n"
DOWNLOAD = "info: syncing channel updates for 'stable-aarch64-apple-darwin'\ninfo: downloading component 'cargo'\n"
XCRUN = "  = note: xcrun: error: couldn't create cache file '/var/folders/_c/xk/T/xcrun_db-jh7lWigU' (errno=Operation not permitted)\n"


class CargoTestTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        add_tables(self.world.store, "project_agent_policy")
        self.lead = self.world.agent("sub", mode="app_spawned")

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def access(self, level):
        """Saves the project's agent policy with an access level. Input: the level id."""
        self.world.row("project_agent_policy", project=PROJECT, body=json.dumps({"access": level, "toolchains": [], "network": "https"}))

    def runs(self, *outputs, command="cargo test", tool="Bash"):
        """One sub-agent of the lead that ran a command once per output. Inputs: the outputs,
        an output starting with Exit code marking a failed run, the command and the tool it
        called. Output: the agent id."""
        agent = self.world.agent("sub", parent=self.lead)
        entries = []
        for number, output in enumerate(outputs, 1):
            entries += call(number, tool, {"command": command}, output, error=output.startswith("Exit code"))
        self.world.transcript(agent, entries)
        return agent

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E5")
        self.assertEqual(CHECK.OP, ["OP-11"])
        self.assertTrue(CHECK.TITLE)

    def test_a_sandboxed_cargo_test_that_passed_with_no_download_passes(self):
        agent = self.runs(PASSED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("access=sandboxed", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test exit 0 (test result: ok. 1 passed;", detail)
        self.assertIn("no sandbox denial; no toolchain download", detail)

    def test_a_cargo_test_the_sandbox_denied_fails(self):
        agent = self.runs(DENIED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test was denied by the sandbox (Operation not permitted (os error 1))", detail)

    def test_a_cargo_test_that_failed_fails(self):
        agent = self.runs(FAILED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test did not exit 0 (test result: FAILED. 0 passed; 1 failed;", detail)

    def test_the_last_run_decides_when_an_earlier_one_failed(self):
        self.runs(FAILED, PASSED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)

    def test_a_toolchain_download_fails_even_when_the_tests_passed(self):
        agent = self.runs(DOWNLOAD + PASSED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent) downloaded a toolchain: info: syncing channel updates for", detail)

    def test_a_download_by_another_agent_fails(self):
        self.runs(PASSED)
        other = self.runs(DOWNLOAD, command="cargo build")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{other}(sub-agent) downloaded a toolchain", detail)

    def test_full_access_is_not_exercised(self):
        self.access("full")
        self.runs(PASSED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("access=full", detail)

    def test_a_run_with_no_cargo_test_is_unknown(self):
        self.runs("ok\n", command="python3 -c 'print(\"ok\")'")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no agent ran cargo test", detail)

    def test_a_team_tool_call_that_names_cargo_test_is_not_a_test_run(self):
        self.runs("Task 52 is queued for a claude-code sub-agent. The engine tells you when it ends.", command="Run cargo test in calc", tool="mcp__crazyagent-team__spawn")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no agent ran cargo test", detail)

    def test_the_xcrun_cache_note_of_a_passing_run_is_a_toolchain_note_not_a_denial(self):
        agent = self.runs(XCRUN + PASSED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test exit 0 (test result: ok. 1 passed;", detail)
        self.assertIn("toolchain note: = note: xcrun: error: couldn't create cache file", detail)
        self.assertIn("no sandbox denial", detail)

    def test_the_xcrun_cache_line_in_a_cargo_test_call_that_also_runs_git_fails_as_t3_09_does(self):
        agent = self.runs(XCRUN + PASSED, command="git pull && cargo test")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test was denied by the sandbox (= note: xcrun: error: couldn't create cache file", detail)
        self.assertNotIn("toolchain note:", detail)
        record = self.world.audit("T3-09")
        self.assertEqual(record["result"], "FAIL", record["detail"])

    def test_the_xcrun_cache_line_in_a_piped_cargo_test_call_is_a_note_in_n_e5_and_t3_09(self):
        agent = self.runs(XCRUN + PASSED, command='cargo test 2>&1 | grep -E "^test result|error" ; cat calc.py; ls')
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test exit 0 (test result: ok. 1 passed;", detail)
        self.assertIn("toolchain note: = note: xcrun: error: couldn't create cache file", detail)
        record = self.world.audit("T3-09")
        self.assertIn("toolchain xcrun notes=1", record["detail"])
        self.assertNotEqual(record["result"], "FAIL", record["detail"])

    def test_a_real_denial_beside_the_xcrun_cache_note_still_fails(self):
        agent = self.runs(XCRUN + DENIED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test was denied by the sandbox (Operation not permitted (os error 1))", detail)

    def test_a_failed_run_with_the_xcrun_cache_note_did_not_exit_0_and_was_not_denied(self):
        agent = self.runs("Exit code 101\n" + XCRUN + FAILED)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{agent}(sub-agent): cargo test did not exit 0 (test result: FAILED.", detail)
        self.assertNotIn("denied by the sandbox", detail)

    def test_a_cargo_build_is_not_a_test_run(self):
        self.runs("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.40s\n", command="cargo build")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no agent ran cargo test", detail)


if __name__ == "__main__":
    unittest.main()
