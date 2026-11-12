import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import audit
from test_audit import HERE, MARK, PROJECT, World

VALID = '''import audit
ID = "N-E1"
OP = ["OP-7", "OP-44"]
TITLE = "Every sign-in attempt leaves at most one account row"
def check(t):
    rows = t.q("SELECT id FROM projects")
    return ("PASS", "projects: " + audit.listed(row[0] for row in rows))
'''


def module(**fields):
    """Source of one audit4 module. Takes the fields to change (ID, OP, TITLE as Python
    literals, BODY as check's body). Returns the source."""
    values = {"ID": '"N-E2"', "OP": '["OP-1"]', "TITLE": '"A title"', "BODY": 'return ("FAIL", "seen nothing")'}
    values.update(fields)
    return f"ID = {values['ID']}\nOP = {values['OP']}\nTITLE = {values['TITLE']}\ndef check(t):\n    {values['BODY']}\n"


class Audit4LoaderTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit4-")
        self.world = World(self.folder.name)
        self.checks = Path(self.folder.name) / "audit4"
        self.checks.mkdir()

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def write(self, name, source):
        (self.checks / name).write_text(source, encoding="utf-8")

    def run_audit(self, *only):
        w = self.world
        argv = [sys.executable, "-I", str(HERE / "audit.py"), PROJECT, "--engine", str(w.root), "--evidence", str(w.out),
                "--project-mark", MARK, "--checks", str(self.checks)]
        for cid in only:
            argv += ["--only", cid]
        return subprocess.run(argv, capture_output=True, text=True, env=w.env, timeout=120)

    def test_an_audit4_check_is_run_and_recorded_like_a_trial_3_check(self):
        self.write("n-e1.py", VALID)
        done = self.run_audit("N-E1")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn(f"PASS N-E1 n-e1: projects: {PROJECT}", done.stdout)
        record = json.loads((self.world.out / "checks" / "N-E1.json").read_text(encoding="utf-8"))
        self.assertEqual((record["id"], record["result"], record["title"]), ("N-E1", "PASS", "Every sign-in attempt leaves at most one account row"))
        self.assertIn("OP-7, OP-44", record["why"])
        for item in record["evidence"]:
            self.assertTrue((self.world.out / item["path"]).exists(), item)

    def test_a_failing_audit4_check_fails_the_audit_and_a_broken_one_is_unknown(self):
        self.write("n-e2.py", module())
        self.write("n-e3.py", module(ID='"N-E3"', BODY="raise RuntimeError('no table')"))
        self.write("n-e4.py", module(ID='"N-E4"', BODY='return "PASS"'))
        self.write("_helpers.py", "raise SystemExit('a helper module is never loaded as a check')\n")
        done = self.run_audit("N-E2", "N-E3", "N-E4")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("FAIL N-E2 n-e2: seen nothing", done.stdout)
        self.assertIn("UNKNOWN N-E3 n-e3: audit error: RuntimeError: no table", done.stdout)
        self.assertIn("UNKNOWN N-E4 n-e4: audit error: ValueError: N-E4 check must return (result, detail)", done.stdout)

    def test_the_trial_3_checks_still_run_beside_the_audit4_ones(self):
        self.write("n-e1.py", VALID)
        done = self.run_audit("T3-08", "N-E1")
        self.assertIn(" T3-08 reply: ", done.stdout)
        self.assertIn("PASS N-E1 ", done.stdout)
        self.assertIn("SUMMARY ", done.stdout)

    def test_a_module_that_breaks_the_convention_stops_the_audit_with_its_file_and_rule(self):
        cases = [
            ({"a.py": module(ID='"ne2"')}, "a.py: ID must look like N-E1"),
            ({"a.py": module(ID='"T3-30"')}, "a.py: ID must look like N-E1"),
            ({"a.py": module(), "b.py": module()}, "b.py: N-E2 is already a check"),
            ({"a.py": module(OP="[]")}, "a.py: N-E2 OP must list OP-<n> rows"),
            ({"a.py": module(OP='["7"]')}, "a.py: N-E2 OP must list OP-<n> rows"),
            ({"a.py": module(TITLE='""')}, "a.py: N-E2 TITLE must be a non-empty string"),
            ({"a.py": 'ID = "N-E2"\nOP = ["OP-1"]\nTITLE = "t"\ncheck = 3\n'}, "a.py: N-E2 check must be a function"),
            ({"a.py": "def check(:\n"}, "a.py: does not load: SyntaxError"),
        ]
        for files, refusal in cases:
            for path in self.checks.iterdir():
                path.unlink()
            for name, source in files.items():
                self.write(name, source)
            with self.subTest(refusal=refusal):
                done = self.run_audit()
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn(refusal, done.stderr)
                self.assertFalse((self.world.out / "checks").exists(), "no check ran")

    def test_the_committed_audit4_modules_load(self):
        loaded = audit.load_audit4(audit.AUDIT4, {check["id"] for check in audit.CHECKS})
        for check in loaded:
            self.assertTrue(callable(check["run"]), check["id"])


if __name__ == "__main__":
    unittest.main()
