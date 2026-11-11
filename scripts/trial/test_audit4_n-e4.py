"""Tests trial 4 check N-E4 (B-20) on a real temporary engine store and a real git project
with a lead worktree and a Land on branch, through the audit's Trial context.

Run: python3 scripts/trial/test_audit4_n-e4.py. Exit 0 when every case holds.
"""

import tempfile
import unittest

from audit4_fixture import load_check, run
from test_audit import MARK, PROJECT, World

CHECK = load_check("N-E4")
OWNER = ("Repo Owner", "owner@example.invalid")
LAND = "trial3-land"
TASK = "Add a subtract function to calc.py and document it in the readme"


class CommitTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        w = self.world
        w.init_repo()
        w.git(w.repo, "config", "user.name", OWNER[0])
        w.git(w.repo, "config", "user.email", OWNER[1])
        w.git(w.repo, "branch", LAND)
        w.row("team_land_on", project=PROJECT, branch=LAND)
        self.lead_wt = w.worktree("lead-a", "crazyagent/lead-a")
        self.lead = w.agent("sub", mode="app_spawned", worktree=str(self.lead_wt))
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, status="done", agent=self.lead,
              prompt=TASK, result="Done.", created_ms=w.now - 60_000, started_ms=w.now - 60_000, finished_ms=w.now)

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def lead_commit(self, message, author=OWNER, name="calc.py"):
        """Commits one file on the lead's branch and lands the branch on the Land on branch.
        Inputs: the message, the author name and email, and the file. Output: the commit."""
        w = self.world
        (self.lead_wt / name).write_text(message + "\n")
        w.git(self.lead_wt, "add", name)
        w.git(self.lead_wt, "-c", f"user.name={author[0]}", "-c", f"user.email={author[1]}", "commit", "-q", "-m", message)
        sha = w.git(self.lead_wt, "rev-parse", "HEAD")
        w.git(w.repo, "update-ref", f"refs/heads/{LAND}", sha)
        return sha

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E4")
        self.assertEqual(CHECK.OP, ["OP-54"])
        self.assertTrue(CHECK.TITLE)

    def test_commits_by_the_repo_identity_with_whole_subjects_pass(self):
        self.lead_commit("Add subtract() to calc.py")
        self.lead_commit("Document subtract() in the readme\n\nThe readme lists every function.")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn(f"commits=2 on {LAND} and 1 lead branches", detail)
        self.assertIn("author repo identity=2", detail)

    def test_a_commit_by_the_machine_identity_fails_with_its_email_masked(self):
        sha = self.lead_commit("Add subtract() to calc.py", author=("Arman Singhal", "chaoticqubit@Armans-MacBook-Pro.local"))
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{sha[:12]} by Arman Singhal <[masked]> is not the repo identity", detail)
        self.assertNotIn("chaoticqubit@", detail)

    def test_a_co_authored_by_trailer_fails(self):
        sha = self.lead_commit("Add subtract() to calc.py\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{sha[:12]} has a Co-Authored-By trailer", detail)

    def test_a_subject_cut_mid_word_from_the_task_fails(self):
        sha = self.lead_commit(TASK[:43])
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{sha[:12]} subject ends mid-word: Add a subtract function to calc.py and docu", detail)

    def test_an_absolute_local_path_in_a_message_fails(self):
        sha = self.lead_commit(f"Fix {self.world.repo}/calc.py\n\nSee /Users/someone/notes.txt too.")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"{sha[:12]} names an absolute local path", detail)

    def test_a_run_with_no_commit_past_the_seed_is_unknown(self):
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no commit past the seed", detail)


if __name__ == "__main__":
    unittest.main()
