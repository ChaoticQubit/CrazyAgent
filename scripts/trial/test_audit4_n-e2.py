"""Tests trial 4 check N-E2 (OP-19) on a real temporary engine store with the engine's own
projects_home table and real git folders beside a projects home, through the audit's
Trial context.

Run: python3 scripts/trial/test_audit4_n-e2.py. Exit 0 when every case holds.
"""

import json
import shutil
import tempfile
import unittest

from audit4_fixture import add_tables, load_check, run
from test_audit import MARK, PROJECT, World

CHECK = load_check("N-E2")


class AddInPlaceTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        add_tables(self.world.store, "projects_home")
        self.home = self.world.base / "Projects"
        self.home.mkdir()
        self.world.row("projects_home", singleton=1, path=str(self.home))

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def outside(self, minute="202611031400", project=True):
        """Makes N-D6's scratch repo beside the projects home, with its linked worktree and the
        .crazyagent folder setup writes, and its project row. Inputs: the minute stamp and
        whether a project names it. Output: the repo folder."""
        w = self.world
        repo = w.base / f"ca-trial-4-outside-{minute}"
        repo.mkdir()
        w.git(repo, "init", "-q", "-b", "main")
        (repo / "README.md").write_text("# Trial 4 outside folder\n")
        w.git(repo, "add", "README.md")
        w.git(repo, "commit", "-q", "-m", "Seed the trial 4 outside folder")
        w.git(repo, "worktree", "add", "-q", "-b", "trial4-side", f"{repo}-wt")
        (repo / ".crazyagent").mkdir()
        if project:
            self.project(f"outside-{minute}", repo)
        return repo

    def project(self, project_id, folder):
        """Saves a project row whose repository is the folder. Inputs: the id and folder."""
        self.world.row("projects", id=project_id, body=json.dumps({"spec": {"repositories": [str(folder)]}}))

    def test_the_module_follows_the_trial_4_convention(self):
        self.assertEqual(CHECK.ID, "N-E2")
        self.assertEqual(CHECK.OP, ["OP-19"])
        self.assertTrue(CHECK.TITLE)

    def test_a_repo_set_up_where_it_is_passes(self):
        self.outside()
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("ca-trial-4-outside-202611031400: project outside-202611031400 uses the folder where it was", detail)
        self.assertIn("linked worktree kept; nothing of it in the projects folder", detail)

    def test_a_repo_moved_into_the_projects_home_fails(self):
        repo = self.outside(project=False)
        moved = self.home / repo.name
        shutil.move(str(repo), str(moved))
        self.project("outside-202611031400", moved)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("project outside-202611031400 uses a folder inside the projects folder", detail)
        self.assertIn("the original folder is gone", detail)

    def test_a_copy_left_in_the_projects_home_fails(self):
        repo = self.outside()
        shutil.copytree(repo, self.home / repo.name, symlinks=True)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn(f"the projects folder holds {repo.name}", detail)

    def test_a_lost_linked_worktree_fails(self):
        repo = self.outside()
        shutil.rmtree(f"{repo}-wt")
        self.world.git(repo, "worktree", "prune")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("its linked worktree is gone", detail)

    def test_a_worktree_folder_the_repo_no_longer_links_fails(self):
        repo = self.outside()
        self.world.git(repo, "worktree", "remove", "--force", f"{repo}-wt")
        (repo.parent / f"{repo.name}-wt").mkdir()
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "FAIL", detail)
        self.assertIn("its linked worktree is gone", detail)

    def test_only_the_newest_outside_add_is_judged(self):
        old = self.outside("202611021200")
        shutil.rmtree(old)
        self.outside("202611031400")
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "PASS", detail)
        self.assertIn("ca-trial-4-outside-202611031400", detail)

    def test_a_run_without_the_outside_add_is_unknown(self):
        self.outside(project=False)
        outcome, detail = run(CHECK, self.world, PROJECT, MARK)
        self.assertEqual(outcome, "UNKNOWN", detail)
        self.assertIn("no project names a ca-trial-4-outside folder", detail)


if __name__ == "__main__":
    unittest.main()
