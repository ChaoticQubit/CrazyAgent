from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

import audit

HERE = Path(__file__).resolve().parent
SCHEMA = (HERE / "fixture-schema.sql").read_text(encoding="utf-8")
MARK = "TRIAL3-PROJECT-RULE"
APP_SET = "# Mandatory workflow\n\nUse tdd."
PROJECT = "trial"
GIT_ENV_KEYS = ("PATH", "TMPDIR", "LANG")


def iso(ms):
    return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).isoformat().replace("+00:00", "Z")


def call(number, name, given, result, error=False):
    use = {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": f"toolu_{number}", "name": name, "input": given}]}}
    done = {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": f"toolu_{number}", "content": result, "is_error": error}]}}
    return [use, done]


def launch(text):
    return {"type": "api-request-shape", "shape": {"system": [{"type": "text", "text": text}]}}


def rendered(app, project=""):
    """The text CrazyAgent gives an agent, in the shape crates/crazyagent/src/agent_home.rs
    render writes: its title, then the app and the project instructions under their
    headings, a blank part left out. Input: both texts. Output: the text."""
    text = "# Instructions from CrazyAgent\n"
    for heading, part in (("## App instructions", app), ("## Project instructions", project)):
        if part.strip():
            text += f"\n{heading}\n\n{part.strip()}\n"
    return text


def prompt(ms, text="go on"):
    return {"type": "user", "timestamp": iso(ms), "message": {"content": text}}


class World:
    """A real engine store, engine root, git project and agent homes in a temporary folder.
    Takes the folder. Each helper writes real rows, files, commits or processes, and audit()
    runs scripts/trial/audit.py on them and returns the check's written record.
    """

    def __init__(self, base):
        self.base = Path(os.path.realpath(base))
        self.root = self.base / "engine"
        self.root.mkdir()
        self.repo = self.base / "repo"
        self.out = self.base / "run"
        self.store = self.root / "engine.db"
        self.db = sqlite3.connect(self.store)
        self.db.executescript(SCHEMA)
        self.now = int(time.time() * 1000)
        self.children = []
        self.env = {key: os.environ[key] for key in GIT_ENV_KEYS if key in os.environ}
        self.env.update(HOME=str(self.base), GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0")
        self.row("projects", id=PROJECT, body=json.dumps({"spec": {"repositories": [str(self.repo)]}}))

    def close(self):
        for child in self.children:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=10)
        self.db.close()

    def row(self, table, **values):
        for _, name, kind, required, default, key in self.db.execute(f"PRAGMA table_info({table})"):
            if required and default is None and not key and name not in values:
                values[name] = 0 if "INT" in kind.upper() else ""
        names = ", ".join(values)
        marks = ", ".join("?" * len(values))
        cursor = self.db.execute(f"INSERT INTO {table} ({names}) VALUES ({marks})", tuple(values.values()))
        self.db.commit()
        return cursor.lastrowid

    def agent(self, role, status="stopped", **extra):
        values = dict(project=PROJECT, role=role, harness="claude-code", tools=1, status=status, created_ms=self.now - 600_000, heartbeat_ms=self.now - 60_000)
        values.update(extra)
        return self.row("team_agents", **values)

    def home(self, agent):
        return self.root / "team" / "agents" / f"agent-{agent}" / "home"

    def transcript(self, agent, entries, worktree=None):
        folder = self.home(agent) / "projects" / audit.slug_of(str(worktree) if worktree else f"/agent-{agent}")
        folder.mkdir(parents=True, exist_ok=True)
        path = folder / f"{agent:08d}-0000-4000-8000-000000000000.jsonl"
        with path.open("a", encoding="utf-8") as handle:
            for entry in entries:
                handle.write(json.dumps(entry) + "\n")
        return path

    def git(self, cwd, *args):
        done = subprocess.run(
            ["git", "-c", "user.name=Trial", "-c", "user.email=trial@example.invalid", "-c", "commit.gpgsign=false", "-C", str(cwd), *args],
            capture_output=True, text=True, env=self.env, timeout=60,
        )
        assert done.returncode == 0, f"git {args}: {done.stderr}"
        return done.stdout.strip()

    def init_repo(self):
        self.repo.mkdir()
        self.git(self.repo, "init", "-q", "-b", "main")
        (self.repo / "README.md").write_text("# Trial\n")
        self.git(self.repo, "add", ".")
        self.git(self.repo, "commit", "-q", "-m", "start")

    def commit(self, cwd, name, text, message):
        (Path(cwd) / name).write_text(text)
        self.git(cwd, "add", name)
        self.git(cwd, "commit", "-q", "-m", message)
        return self.git(cwd, "rev-parse", "HEAD")

    def worktree(self, name, branch):
        place = self.repo / ".crazyagent" / "worktrees" / name
        self.git(self.repo, "worktree", "add", "-q", "-b", branch, str(place))
        return place

    def spawn(self, *argv):
        child = subprocess.Popen(list(argv), start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.children.append(child)
        return child

    def audit(self, check, *extra):
        before = hashlib.sha256(self.store.read_bytes()).hexdigest()
        done = subprocess.run(
            [sys.executable, "-I", str(HERE / "audit.py"), PROJECT, "--engine", str(self.root), "--evidence", str(self.out),
             "--only", check, "--project-mark", MARK, *extra],
            capture_output=True, text=True, env=self.env, timeout=120,
        )
        assert done.returncode in (0, 1), done.stdout + done.stderr
        assert hashlib.sha256(self.store.read_bytes()).hexdigest() == before, "the audit changed the engine store"
        record = json.loads((self.out / "checks" / f"{check}.json").read_text(encoding="utf-8"))
        assert record["id"] == check and record["evidence"], record
        for item in record["evidence"]:
            assert (self.out / item["path"]).exists(), item
        record["stdout"] = done.stdout
        record["log"] = (self.out / "logs" / f"{check}.log").read_text(encoding="utf-8")
        return record



def recorded_ids():
    """The check ids a default audit run records: audit.py's own checks and every committed
    scripts/trial/audit4 module. Takes nothing. Returns the ids sorted, as the record file
    names list them."""
    own = {check["id"] for check in audit.CHECKS}
    return sorted(own | {check["id"] for check in audit.load_audit4(audit.AUDIT4, own)})


class AuditTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="ca-audit-")
        self.world = World(self.folder.name)

    def tearDown(self):
        self.world.close()
        self.folder.cleanup()

    def verdict(self, check, *extra):
        return self.world.audit(check, *extra)["result"]

    def tree(self, subs_b=2):
        w = self.world
        master = w.agent("master")
        native = w.agent("sub", mode="native")
        mates = [w.agent("teammate", parent=native) for _ in range(2)]
        spawned = w.agent("sub", mode="app_spawned")
        subs = [w.agent("sub", parent=spawned) for _ in range(subs_b)]
        return master, native, mates, spawned, subs

    def test_t3_01_tree_passes_with_both_lead_modes_and_two_sub_agents_each_and_fails_short_of_one(self):
        self.tree()
        self.assertEqual(self.verdict("T3-01"), "PASS")
        self.world.db.execute("DELETE FROM team_agents WHERE id=(SELECT max(id) FROM team_agents)")
        self.world.db.commit()
        record = self.world.audit("T3-01")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("subs 1", record["detail"])

    def clones(self, teammate_worktree=True, own_clone=True):
        w = self.world
        w.init_repo()
        native = w.agent("sub", mode="native")
        lead_a = w.worktree("agent-a", "crazyagent/lead-a")
        w.db.execute("UPDATE team_agents SET worktree=? WHERE id=?", (str(lead_a), native))
        mate_wt = None
        if teammate_worktree:
            w.git(lead_a, "worktree", "add", "-q", "-b", "crazyagent/lead-a--mate-one", str(lead_a / ".worktrees" / "mate-one"))
            mate_wt = str(lead_a / ".worktrees" / "mate-one")
        w.agent("teammate", parent=native, worktree=mate_wt)
        spawned = w.agent("sub", mode="app_spawned")
        lead_b = w.worktree("agent-b", "crazyagent/lead-b")
        w.db.execute("UPDATE team_agents SET worktree=? WHERE id=?", (str(lead_b), spawned))
        clone = w.repo / ".crazyagent" / "worktrees" / "agent-sub"
        if own_clone:
            w.git(w.base, "clone", "-q", str(lead_b), str(clone))
            w.git(clone, "checkout", "-q", "-b", "crazyagent/lead-b--add-fn")
        else:
            w.git(lead_b, "worktree", "add", "-q", "-b", "crazyagent/lead-b--add-fn", str(clone))
        sub = w.agent("sub", parent=spawned, worktree=str(clone), created_ms=w.now - 600_000)
        w.db.commit()
        return spawned, lead_b, sub, clone

    def test_t3_02_own_clones_pass_with_a_lead_worktree_teammate_and_an_own_clone_and_fail_without_a_teammate_worktree(self):
        self.clones()
        record = self.world.audit("T3-02")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("app-spawned own clone", record["detail"])
        self.tearDown()
        self.setUp()
        self.clones(teammate_worktree=False)
        record = self.world.audit("T3-02")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("has no worktree", record["detail"])
        self.tearDown()
        self.setUp()
        self.clones(own_clone=False)
        record = self.world.audit("T3-02")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("app-spawned .git is a gitdir file", record["detail"])
        self.assertIn("(wrong place)", record["detail"])

    def native_lead(self, refusal):
        w = self.world
        lead = w.agent("sub", mode="native", turns=3)
        w.row("team_panes", agent=lead, record="{}")
        for pane in ("%2", "%3"):
            w.row("team_teammates", agent=lead, project=PROJECT, pane=pane, seen_ms=w.now)
        result = "team_caller_unknown: the caller is not a team agent" if refusal else "Started work #1"
        w.transcript(lead, [launch("x")] + call(1, "mcp__crazyagent-team__start_work", {"title": "a"}, "Started work #1")
                     + call(2, "mcp__crazyagent-team__inspect_work", {}, result, error=refusal))

    def test_t3_03_native_lead_passes_with_its_session_and_panes_and_fails_on_a_caller_refusal(self):
        self.native_lead(refusal=False)
        self.assertEqual(self.verdict("T3-03"), "PASS")
        self.tearDown()
        self.setUp()
        self.native_lead(refusal=True)
        record = self.world.audit("T3-03")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("team_caller_unknown=1", record["detail"])
        self.assertIn("first team_caller_unknown", record["log"])

    def question(self, agent_answers):
        w = self.world
        w.init_repo()
        w.git(w.repo, "checkout", "-q", "-b", "crazyagent/lead-a")
        w.commit(w.repo, "greet.py", 'print("Hello")\n', "greet")
        w.git(w.repo, "checkout", "-q", "main")
        mate = w.agent("teammate", parent=w.agent("sub", mode="native"))
        asked = w.row("coord_messages", project=PROJECT, from_kind="agent", from_agent=mate, to_kind="owner", kind="question",
                      q_for="me", q_severity="normal", body="What should greet print?", receipt="replied", created=w.now, receipt_at=w.now)
        w.row("coord_messages", project=PROJECT, from_kind="owner", to_kind="agent", to_agent=mate, kind="reply", reply_to=asked,
              body="Hello", receipt="read", created=w.now, receipt_at=w.now)
        if agent_answers:
            w.row("coord_messages", project=PROJECT, from_kind="agent", from_agent=mate + 1, to_kind="agent", to_agent=mate, kind="reply",
                  reply_to=asked, body="Say hi", receipt="read", created=w.now, receipt_at=w.now, idem_key="x")

    def test_t3_04_owner_question_passes_when_only_the_owner_answers_and_fails_when_an_agent_answers_too(self):
        self.question(agent_answers=False)
        record = self.world.audit("T3-04")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("greet.py with Hello: refs/heads/crazyagent/lead-a", record["detail"])
        self.tearDown()
        self.setUp()
        self.question(agent_answers=True)
        record = self.world.audit("T3-04")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("agent_replies=1", record["detail"])

    def wake(self, woken):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        sub = w.agent("sub", parent=lead)
        w.row("coord_messages", project=PROJECT, from_kind="agent", from_agent=sub, to_kind="agent", to_agent=lead, kind="note",
              body="done", receipt="read", created=w.now, receipt_at=w.now)
        finished = w.now - 30_000
        w.row("team_events", project=PROJECT, agent=sub, kind="finished", text="done", state="delivered", created_ms=finished)
        w.transcript(lead, [launch("x"), prompt(finished + (5_000 if woken else -5_000))])

    def test_t3_05_wake_passes_when_the_lead_gets_a_prompt_after_its_workers_finish_and_fails_when_not_woken(self):
        self.wake(woken=True)
        self.assertEqual(self.verdict("T3-05"), "PASS")
        self.tearDown()
        self.setUp()
        self.wake(woken=False)
        record = self.world.audit("T3-05")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("(not woken)", record["detail"])

    def overlap(self, same_file):
        w = self.world
        first, second = w.agent("sub", mode="app_spawned"), w.agent("sub", mode="app_spawned")
        w.row("coord_claims", project=PROJECT, agent=first, work=1, pattern="calc.py", created=w.now - 10)
        w.row("coord_claims", project=PROJECT, agent=second, work=2, pattern="calc.py" if same_file else "docs/notes.md", created=w.now - 5)
        w.row("coord_messages", project=PROJECT, from_kind="engine", to_kind="agent", to_agent=second, kind="note",
              body="Your claim calc.py overlaps claim 1", receipt="read", created=w.now, receipt_at=w.now)

    def test_t3_06_claim_overlap_passes_on_a_shared_file_and_fails_when_claims_do_not_overlap(self):
        self.overlap(same_file=True)
        record = self.world.audit("T3-06")
        self.assertEqual(record["result"], "PASS")
        self.assertIn("same file calc.py", record["detail"])
        self.tearDown()
        self.setUp()
        self.overlap(same_file=False)
        self.assertEqual(self.verdict("T3-06"), "FAIL")

    def test_t3_07_merges_pass_when_sub_agent_and_lead_work_reach_main_and_fail_when_the_lead_is_not_merged(self):
        w = self.world
        spawned, lead_b, sub, clone = self.clones()
        w.commit(clone, "calc.py", "def add(a, b):\n    return a + b\n", "add")
        w.git(lead_b, "fetch", "-q", str(clone), "crazyagent/lead-b--add-fn")
        w.git(lead_b, "merge", "-q", "--no-ff", "-m", "merge add", "FETCH_HEAD")
        w.db.execute("DELETE FROM team_agents WHERE mode='native' OR role='teammate'")
        w.db.commit()
        record = w.audit("T3-07")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("not in main", record["detail"])
        w.git(w.repo, "merge", "-q", "--no-ff", "-m", "land lead b", "crazyagent/lead-b")
        record = w.audit("T3-07")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn(f"agent {sub} crazyagent/lead-b--add-fn", record["detail"])

    def test_t3_08_reply_passes_on_a_stored_agent_reply_and_fails_on_a_refused_one(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.row("coord_messages", project=PROJECT, from_kind="agent", from_agent=lead, to_kind="agent", to_agent=lead + 1, kind="reply",
              reply_to=1, body="ok", receipt="read", created=w.now, receipt_at=w.now)
        self.assertEqual(self.verdict("T3-08"), "PASS")
        w.transcript(lead, call(1, "mcp__crazyagent-team__send_message", {"reply_to": 99}, "coord_reply_invalid: no such message", error=True))
        record = w.audit("T3-08")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("coord_reply_invalid results=1", record["detail"])

    def test_t3_09_sandbox_git_passes_on_working_git_and_fails_on_an_xcrun_error_with_the_line_masked(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.transcript(lead, call(1, "Bash", {"command": "git status"}, "On branch main"))
        self.assertEqual(self.verdict("T3-09"), "PASS")
        secret = "Authorization: Bearer sk-ant-api03-" + "Q" * 40 + " for owner@example.com"
        w.transcript(lead, call(2, "Bash", {"command": "git log"}, f"xcrun: error: cannot be used within an App Sandbox. {secret}", error=True))
        record = w.audit("T3-09")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("first xcrun error", record["log"])
        for leaked in ("sk-ant-api03", "owner@example.com", "Q" * 40):
            self.assertNotIn(leaked, record["log"])
            self.assertNotIn(leaked, (w.out / "checks" / "T3-09.json").read_text())
        self.assertIn(audit.MASK, record["log"])

    def test_t3_09_notes_the_xcrun_lines_of_a_toolchain_call_that_runs_no_git(self):
        w = self.world
        sub = w.agent("sub", mode="app_spawned")
        cache = "  = note: xcrun: error: couldn't create cache file '/var/folders/x/T/xcrun_db-AbCd1234' (errno=Operation not permitted)"
        w.transcript(sub, call(1, "Bash", {"command": "cargo test 2>&1 | grep -E \"^test result|error\" ; cat calc.py; ls"}, f"{cache}\ntest result: ok. 1 passed"))
        w.transcript(sub, call(2, "Bash", {"command": "git add calc.py && git commit -qm \"feat: add sub\""}, "[main 376e354] feat: add sub"))
        record = w.audit("T3-09")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("git calls=1 ok=1 xcrun errors=0", record["detail"])
        self.assertIn("toolchain xcrun notes=1", record["detail"])

    def t3_09_alone(self, command, result, error=False):
        """Audits T3-09 on a fresh store whose one agent made one shell call, then a clean git
        call. Takes the call's command, its result and whether it failed. Returns the record."""
        with tempfile.TemporaryDirectory(prefix="ca-audit-") as folder:
            world = World(folder)
            try:
                agent = world.agent("sub", mode="app_spawned")
                world.transcript(agent, call(1, "Bash", {"command": command}, result, error=error))
                world.transcript(agent, call(2, "Bash", {"command": "git status"}, "On branch main"))
                return world.audit("T3-09")
            finally:
                world.close()

    def test_t3_09_finds_git_by_its_parsed_command_word_and_fails_the_xcrun_shim_in_every_shell_form(self):
        shim = "xcrun: error: couldn't create cache file '/var/folders/x/T/xcrun_db-AbCd1234' (errno=Operation not permitted)"
        for command in (
            "/usr/bin/git commit -qm x",
            "/Library/Developer/CommandLineTools/usr/bin/git status",
            "cd repo && git status",
            "true; git log",
            "echo x | git apply",
            "false || git fetch",
            "cd repo && git;",
            "(git status)",
            "{ git status; }",
            "if git diff --quiet; then echo same; fi",
            "echo `git rev-parse HEAD`",
            "echo \"$(/usr/bin/git rev-parse HEAD)\"",
            "$(which git) commit -qm x",
            "sh -c \"git log\"",
            "bash -lc 'cd repo && /usr/bin/git status'",
            "bash -c git",
            "GIT_DIR=.git git log",
            "env -i PATH=/usr/bin git log",
            "xcrun git status",
            "cd repo\ngit status",
            "git commit -F - <<'EOF'\nit's done\nEOF",
        ):
            with self.subTest(command=command):
                record = self.t3_09_alone(command, f"{shim}\nfatal: git failed under the sandbox", error=True)
                self.assertEqual(record["result"], "FAIL", record["detail"])
                self.assertIn("git calls=2 ok=1 xcrun errors=1", record["detail"])

    def test_t3_09_fails_an_xcrun_line_in_any_call_but_a_known_toolchain_one(self):
        shim = "xcrun: error: couldn't create cache file '/var/folders/x/T/xcrun_db-AbCd1234' (errno=Operation not permitted)"
        for command in (
            "./land.sh",
            "python3 tools/release.py",
            "make release && ./land.sh",
            "cargo test; echo $(date)",
            "bash -c \"cargo test\"",
            "xargs cargo build",
            "cargo test 'open",
        ):
            with self.subTest(command=command):
                record = self.t3_09_alone(command, shim)
                self.assertEqual(record["result"], "FAIL", record["detail"])
                self.assertIn("git calls=1 ok=1 xcrun errors=1", record["detail"])
        for command in (
            "cargo test 2>&1 | grep -E \"^test result|error\" ; cat calc.py; ls",
            "cargo build -q",
            "cd crate && make",
            "time cargo test",
            "~/.cargo/bin/cargo test -q",
            "RUSTFLAGS=-Dwarnings cargo clippy",
            "rustc main.rs && ls",
            "swift build 2>&1 | tail -3",
        ):
            with self.subTest(command=command):
                record = self.t3_09_alone(command, f"{shim}\ntest result: ok. 1 passed")
                self.assertEqual(record["result"], "PASS", record["detail"])
                self.assertIn("git calls=1 ok=1 xcrun errors=0, toolchain xcrun notes=1", record["detail"])

    def test_t3_09_fails_an_xcrun_line_in_a_toolchain_call_whose_words_name_git(self):
        shim = "xcrun: error: couldn't create cache file '/var/folders/x/T/xcrun_db-AbCd1234' (errno=Operation not permitted)"
        for command in (
            "make GIT=/usr/bin/git",
            "make release GIT=git",
            "cargo run -q -- git status",
            "cargo build --config 'net.git-fetch-with-cli=true' && /usr/bin/git status",
        ):
            with self.subTest(command=command):
                record = self.t3_09_alone(command, shim)
                self.assertEqual(record["result"], "FAIL", record["detail"])
                self.assertIn("xcrun errors=1", record["detail"])

    def test_t3_10_stop_all_passes_on_a_paused_project_and_fails_while_an_agent_process_lives(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.row("team_paused_projects", project=PROJECT, since_ms=w.now)
        self.assertEqual(self.verdict("T3-10"), "PASS")
        child = w.spawn("/bin/sleep", "60")
        w.db.execute("UPDATE team_agents SET process_group=? WHERE id=?", (child.pid, lead))
        w.db.commit()
        record = w.audit("T3-10")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn(f"{child.pid}/{child.pid}/{lead}", record["detail"])

    def test_t3_11_instruction_delivery_passes_when_the_app_set_leads_and_fails_when_the_project_rule_comes_first(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.transcript(lead, [launch(f"{audit.APP_MARK}\n\nUse tdd.\n\n{MARK}\n")])
        self.assertEqual(self.verdict("T3-11"), "PASS")
        mate = w.agent("sub", parent=lead)
        w.transcript(mate, [launch(f"{MARK}\n\n{audit.APP_MARK}\n")])
        record = w.audit("T3-11")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("bad=1", record["detail"])
        self.assertIn("first launch context", record["log"])

    def skills(self, stored):
        w = self.world
        w.row("plugin_items", id="skill-1", kind="skill", scope="global", name="tdd",
              body=json.dumps({"id": "skill-1", "name": "tdd", "enabled": True, "scope": {"kind": "global"}, "folder": "plugins/tdd"}))
        (w.root / "plugins" / "tdd").mkdir(parents=True)
        (w.root / "plugins" / "tdd" / "SKILL.md").write_text("# tdd\n")
        lead = w.agent("sub", mode="app_spawned")
        w.transcript(lead, call(1, "Skill", {"skill": "tdd"}, "Loaded tdd"))
        w.row("coord_tool_use", agent=lead, tool="mcp__crazyagent-team__start_work", count=1, last=w.now)
        if stored:
            w.row("coord_tool_use", agent=lead, tool="skill:tdd", count=1, last=w.now)

    def test_t3_12_adoption_passes_when_a_skill_call_is_counted_and_fails_when_it_is_not(self):
        self.skills(stored=True)
        self.assertEqual(self.verdict("T3-12"), "PASS")
        self.tearDown()
        self.setUp()
        self.skills(stored=False)
        record = self.world.audit("T3-12")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("Skill calls with no stored skill count: tdd=1", record["detail"])

    def test_t3_13_tui_fails_without_a_recording_or_on_a_failed_tui_step_and_is_a_manual_check_otherwise(self):
        w = self.world
        record = w.audit("T3-13")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("no TUI recording", record["detail"])
        (w.out / "frames").mkdir(parents=True)
        for name in ("0001.png", "0002.png", "0003.png"):
            (w.out / "frames" / name).write_bytes(b"\x89PNG")
        (w.out / "trial.gif").write_bytes(b"GIF89a")
        steps = [
            {"n": 1, "surface": "desktop", "title": "Open the project", "ok": False},
            {"n": 2, "surface": "tui", "title": "Start the TUI recorder", "ok": True},
        ]
        (w.out / "steps.jsonl").write_text("".join(json.dumps(step) + "\n" for step in steps))
        record = w.audit("T3-13")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("manual check", record["detail"])
        self.assertEqual([item["path"] for item in record["evidence"] if item["kind"] in ("gif", "frame")], ["trial.gif", "frames/0001.png", "frames/0003.png"])
        self.assertIn("steps.jsonl", [item["path"] for item in record["evidence"]])
        with (w.out / "steps.jsonl").open("a") as handle:
            handle.write(json.dumps({"n": 3, "surface": "tui", "title": "Open Team with Ctrl-T", "ok": False}) + "\n")
        record = w.audit("T3-13")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("step 3 Open Team with Ctrl-T", record["detail"])
        self.assertNotIn("Open the project", record["detail"])

    def imported(self, original_left):
        w = self.world
        w.init_repo()
        rules = f"# Rules\n{MARK}\n"
        (w.repo / ".crazyagent" / "imported").mkdir(parents=True)
        (w.repo / ".crazyagent" / "imported" / "CLAUDE.md").write_text(rules)
        (w.repo / ".crazyagent" / "instructions.md").write_text(f"## Imported from CLAUDE.md\n\n{rules}")
        if original_left:
            (w.repo / "CLAUDE.md").write_text(rules)

    def test_t3_14_import_passes_when_the_repo_rules_moved_into_the_project_set_and_fails_when_left_in_place(self):
        self.imported(original_left=False)
        self.assertEqual(self.verdict("T3-14"), "PASS")
        self.tearDown()
        self.setUp()
        self.imported(original_left=True)
        record = self.world.audit("T3-14")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("repo CLAUDE.md still present", record["detail"])

    def used(self, graphify):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        entries = call(1, "mcp__plugin_context-mode_context-mode__ctx_execute", {"code": "1"}, "1") + call(2, "Skill", {"skill": "tdd"}, "ok")
        if graphify:
            entries += call(3, "Bash", {"command": "graphify query 'who calls x'"}, "x")
        w.transcript(lead, entries)
        return lead

    def test_t3_15_transcripts_pass_with_ctx_graphify_and_skill_calls_and_fail_without_graphify(self):
        self.used(graphify=True)
        self.assertEqual(self.verdict("T3-15"), "PASS")
        self.tearDown()
        self.setUp()
        self.used(graphify=False)
        record = self.world.audit("T3-15")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("graphify=0", record["detail"])

    def harness(self, deny):
        w = self.world
        lead = w.agent("sub", mode="native")
        w.row("team_teammates", agent=lead, project=PROJECT, pane="%2", seen_ms=w.now)
        script = w.base / "bin" / "claude"
        script.parent.mkdir()
        script.write_text("#!/bin/sh\nsleep 60\n")
        flags = ["--disallowedTools", "Agent", "TeamCreate"] if deny else ["--teammate-mode", "tmux"]
        child = w.spawn("/bin/sh", str(script), *flags)
        w.db.execute("UPDATE team_agents SET process_group=?, status='working' WHERE id=?", (child.pid, lead))
        w.db.commit()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if any(p[0] == child.pid and "/claude" in p[2] for p in audit.processes() or []):
                return
            time.sleep(0.05)
        raise AssertionError("the stand-in claude process never showed in ps")

    def test_t3_16_launch_argv_passes_for_a_native_lead_with_its_team_tools_and_fails_when_they_are_denied(self):
        self.harness(deny=False)
        record = self.world.audit("T3-16")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("teammate-mode=True", record["detail"])
        self.tearDown()
        self.setUp()
        self.harness(deny=True)
        record = self.world.audit("T3-16")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("denies Agent=True TeamCreate=True", record["detail"])

    def test_t3_17_instructions_pass_inline_and_fail_when_offloaded(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.home(lead).mkdir(parents=True)
        (w.home(lead) / "CLAUDE.md").write_text(f"{audit.APP_MARK}\nUse tdd.\n")
        self.assertEqual(self.verdict("T3-17"), "PASS")
        (w.home(lead) / "CLAUDE.md").write_text(f"The set is too large, {audit.OFFLOAD}instructions.md`.\n")
        record = w.audit("T3-17")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("offloaded 1", record["detail"])

    def test_t3_18_tool_use_passes_with_work_ctx_and_graphify_counts_and_fails_without_ctx(self):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        for tool in ("mcp__crazyagent-team__start_work", "mcp__graphify__query_graph"):
            w.row("coord_tool_use", agent=lead, tool=tool, count=2, last=w.now)
        record = w.audit("T3-18")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("ctx=0", record["detail"])
        w.row("coord_tool_use", agent=lead, tool="mcp__plugin_context-mode_context-mode__ctx_execute", count=1, last=w.now)
        self.assertEqual(self.verdict("T3-18"), "PASS")

    def landed(self, on_chosen):
        w = self.world
        w.init_repo()
        w.git(w.repo, "branch", "release")
        lead = w.agent("sub", mode="app_spawned")
        lead_wt = w.worktree("agent-lead", "crazyagent/lead-b")
        w.db.execute("UPDATE team_agents SET worktree=? WHERE id=?", (str(lead_wt), lead))
        tip = w.commit(lead_wt, "calc.py", "x = 1\n", "lead work")
        w.git(w.repo, "checkout", "-q", "release" if on_chosen else "main")
        w.git(w.repo, "merge", "-q", "--no-ff", "-m", "land", "crazyagent/lead-b")
        landed = w.git(w.repo, "rev-parse", "HEAD")
        w.git(w.repo, "checkout", "-q", "main")
        w.row("team_land_on", project=PROJECT, branch="release")
        w.row("landing_change_sets", id="set-1", project=PROJECT, agent_kind="sub", agent_id=str(lead), repository=str(w.repo),
              head=tip, state="accepted", last_outcome="succeeded", landed=landed, created_ms=w.now)
        w.row("team_messages", project=PROJECT, author="engine", state="note", created_ms=w.now,
              text=f"The master merged Bo's work from task 3 into release as {landed[:12]}. Nothing was pushed.")

    def test_t3_19_land_on_passes_when_lead_work_lands_on_the_chosen_branch_and_fails_when_it_lands_elsewhere(self):
        self.assertEqual(self.verdict("T3-19"), "UNKNOWN")
        self.landed(on_chosen=True)
        record = self.world.audit("T3-19")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("master notes naming release: 1", record["detail"])
        self.tearDown()
        self.setUp()
        self.landed(on_chosen=False)
        record = self.world.audit("T3-19")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("in release=False", record["detail"])

    def background(self, mode, ending):
        w = self.world
        lead = w.agent("sub", mode="app_spawned")
        w.row("team_background", project=PROJECT, mode=mode, grace_secs=120 if mode == "grace" else None)
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt="sleep in the background", status="done",
              agent=lead, created_ms=w.now - 400_000, started_ms=w.now - 300_000, finished_ms=w.now,
              result=f"{ending}\n\nI started a server.")
        if mode == "stop":
            w.transcript(lead, [launch("x"), prompt(w.now - 300_000, f"Run the server.\n\n{audit.BACKGROUND_TOLD}")])

    def test_t3_20_background_passes_when_a_command_stops_as_the_setting_says_and_fails_against_the_setting(self):
        stopped = "This task ended with its turn, as Team settings say, while a command it started in the background still ran; that command was stopped."
        self.background("stop", stopped)
        record = self.world.audit("T3-20")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("told in prompts=1", record["detail"])
        self.tearDown()
        self.setUp()
        self.background("wait", stopped)
        record = self.world.audit("T3-20")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("ended by stop under mode wait", record["detail"])
        self.tearDown()
        self.setUp()
        grace = "The background grace time in Team settings ended this task while a command it started in the background still ran; that command was stopped."
        self.background("grace", grace)
        self.assertEqual(self.verdict("T3-20"), "PASS")

    def hidden(self, denied, folders=1):
        w = self.world
        w.db.executescript(
            "CREATE TABLE project_hidden_paths (project TEXT NOT NULL, position INTEGER NOT NULL, path TEXT NOT NULL, PRIMARY KEY (project, position));"
        )
        secrets = [str(w.base / "repo" / f"private-{n}") for n in range(folders)]
        for position, secret in enumerate(secrets):
            w.row("project_hidden_paths", project=PROJECT, position=position, path=secret)
        lead = w.agent("sub", mode="app_spawned")
        profiles = w.root / "team" / "profiles"
        profiles.mkdir(parents=True)
        subpaths = "".join(f'\n  (subpath "{secret}")' for secret in secrets)
        rule = f"(deny file-read* file-write* process-exec file-link file-clone{subpaths})\n" if denied else ""
        (profiles / f"agent-{lead}.sb").write_text(f'(version 1)\n(allow default)\n{rule}(deny file-write*\n  (subpath "{w.repo}/context"))\n')

    def test_t3_21_hidden_folders_pass_when_every_profile_denies_them_and_fail_when_one_does_not(self):
        record = self.world.audit("T3-21")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not in this build", record["detail"])
        self.hidden(denied=True)
        self.assertEqual(self.verdict("T3-21"), "PASS")
        self.tearDown()
        self.setUp()
        self.hidden(denied=False)
        record = self.world.audit("T3-21")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("denies 0/1", record["detail"])
        for folders in (2, 3):
            self.tearDown()
            self.setUp()
            self.hidden(denied=True, folders=folders)
            record = self.world.audit("T3-21")
            self.assertEqual(record["result"], "PASS", record["detail"])
            self.assertIn(f"denies {folders}/{folders}", record["detail"])

    def given(self, agent, text, kind="agent-instructions-given"):
        w = self.world
        home = w.home(agent)
        home.mkdir(parents=True, exist_ok=True)
        folder = w.root / kind
        folder.mkdir(exist_ok=True)
        (folder / hashlib.sha256(os.path.realpath(home).encode()).hexdigest()).write_text(text)

    def part_one(self, mate_reached):
        w = self.world
        lead = w.agent("sub", mode="native", worktree="/w/agent-lead")
        for pane in ("%2", "%3"):
            w.row("team_teammates", agent=lead, project=PROJECT, pane=pane, seen_ms=w.now)
        (w.root / "instructions.md").write_text(f"\n{APP_SET}\n")
        self.given(lead, rendered(APP_SET, MARK))
        codex = w.agent("sub", harness="codex")
        w.home(codex).mkdir(parents=True)
        (w.home(codex) / "AGENTS.md").write_text(rendered(APP_SET, MARK))
        w.transcript(lead, [launch(f"Claude.\n{audit.APP_MARK}\n{MARK}")], worktree="/w/agent-lead")
        w.transcript(lead, [launch(f"Mate.\n{audit.APP_MARK}\n")], worktree="/w/agent-lead/.worktrees/mate-one")
        w.transcript(lead, [launch(f"Mate.\n{audit.APP_MARK if mate_reached else 'no set'}\n")], worktree="/w/agent-lead/.worktrees/mate-two")

    def mate_transcript(self, lead, name, entries):
        """Writes a native teammate's transcript where its Claude Code keeps it: under its
        lead's teammates' config home, in the folder of the lead's worktree, each line naming
        the teammate as agent teams do. Inputs: the lead, the agent-team name, the entries."""
        w = self.world
        folder = w.root / "team" / "agents" / f"agent-{lead}" / "mates-home" / "projects" / audit.slug_of("/w/agent-lead")
        folder.mkdir(parents=True, exist_ok=True)
        with (folder / f"{name}.jsonl").open("a", encoding="utf-8") as handle:
            for entry in entries:
                handle.write(json.dumps(dict(entry, agentName=name, teamName="session-1")) + "\n")

    def native_team(self, both_reached=True, rows=True):
        """A native lead whose two teammates' transcripts are in its teammates' config home,
        as agent rows with their handles or as pane rows only. Inputs: whether the second
        teammate's launch had the set and whether teammates have agent rows."""
        w = self.world
        lead = w.agent("sub", mode="native", worktree="/w/agent-lead")
        (w.root / "instructions.md").write_text(f"\n{APP_SET}\n")
        self.given(lead, rendered(APP_SET, MARK))
        w.transcript(lead, [launch(f"Claude.\n{audit.APP_MARK}\n{MARK}")], worktree="/w/agent-lead")
        for number, handle in enumerate(("greeter", "docs"), 1):
            if rows:
                mate = w.agent("teammate", parent=lead, pane=f"%{number}", worktree=f"/w/agent-lead/.worktrees/{handle}")
                w.row("team_names", agent=mate, project=PROJECT, persona=handle.title(), handle=handle)
            else:
                w.row("team_teammates", agent=lead, project=PROJECT, pane=f"%{number}", seen_ms=w.now)
            reached = both_reached or number == 1
            self.mate_transcript(lead, handle, [launch(f"Mate.\n{audit.APP_MARK if reached else 'no set'}\n{MARK}")])
        return lead

    def test_t3_11_and_t3_15_read_native_teammates_transcripts_in_their_lead_s_teammate_home(self):
        self.native_team()
        record = self.world.audit("T3-11")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertEqual(record["stdout"].count("(teammate) launches ok=1/1"), 2, record["stdout"])
        self.assertNotIn("no transcript", record["stdout"])
        record = self.world.audit("T3-15")
        self.assertNotIn("no transcripts for", record["detail"])
        self.assertEqual(record["detail"].count("(teammate) files=1"), 2, record["detail"])
        self.tearDown()
        self.setUp()
        self.native_team(both_reached=False)
        record = self.world.audit("T3-11")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("(teammate) launches ok=0/1", record["stdout"])
        self.assertIn("(teammate) launches ok=1/1", record["stdout"])

    def test_planted_symlinks_in_a_teammate_home_or_an_agent_home_are_not_read(self):
        w = self.world
        lead = self.native_team()
        outside = w.base / "outside"
        (outside / "linked").mkdir(parents=True)
        for path, name in ((outside / "planted.jsonl", "greeter"), (outside / "linked" / "other-session.jsonl", "docs")):
            path.write_text(json.dumps(dict(launch(f"Mate.\n{audit.APP_MARK}\n{MARK}"), agentName=name, teamName="session-1")) + "\n")
        mates = w.root / "team" / "agents" / f"agent-{lead}" / "mates-home" / "projects"
        (mates / audit.slug_of("/w/agent-lead") / "planted.jsonl").symlink_to(outside / "planted.jsonl")
        (mates / "linked").symlink_to(outside / "linked")
        sub = w.agent("sub", mode="app_spawned")
        (outside / "projects" / "elsewhere").mkdir(parents=True)
        (outside / "projects" / "elsewhere" / "session.jsonl").write_text(json.dumps(launch(f"Sub.\n{audit.APP_MARK}\n{MARK}")) + "\n")
        w.home(sub).mkdir(parents=True)
        (w.home(sub) / "projects").symlink_to(outside / "projects")
        record = w.audit("T3-15")
        self.assertEqual(record["detail"].count("(teammate) files=1"), 2, record["detail"])
        self.assertNotIn("files=2", record["detail"])
        self.assertIn(f"no transcripts for {sub}", record["detail"])

    def test_t3_23_never_reads_a_codex_rollout_planted_as_a_symlink(self):
        lead = self.role_skills(lead_harness="codex")
        folder = self.world.home(lead) / "sessions" / "2026" / "11" / "04"
        rollout = folder / "rollout-2026-11-04T10-00-00-1.jsonl"
        outside = self.world.base / "outside"
        outside.mkdir()
        rollout.rename(outside / rollout.name)
        rollout.symlink_to(outside / rollout.name)
        record = self.world.audit("T3-23")
        self.assertIn("role lead chose grill-me, tdd; given grill-me, tdd; installed unknown (no skill listing in its transcripts)", record["stdout"])

    def test_t3_22_counts_pane_teammates_transcripts_in_their_lead_s_teammate_home(self):
        self.native_team(rows=False)
        record = self.world.audit("T3-22")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertIn("lead 1 teammates=2 transcripts=2 with the set=2", record["stdout"])
        self.tearDown()
        self.setUp()
        self.native_team(both_reached=False, rows=False)
        record = self.world.audit("T3-22")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("with the set=1", record["stdout"])

    def test_t3_22_part_one_passes_when_every_agent_and_teammate_got_the_set_and_fails_when_a_teammate_did_not(self):
        self.part_one(mate_reached=True)
        record = self.world.audit("T3-22")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertIn("lead 1 teammates=2 transcripts=2 with the set=2", record["stdout"])
        self.assertIn("agents bad=0", record["detail"])
        self.assertIn("2(lead app_spawned) home AGENTS.md opening=['# Instructions from CrazyAgent', '## App instructions', '# Mandatory workflow']", record["stdout"])
        same = hashlib.sha256(APP_SET.encode()).hexdigest()[:12]
        self.assertEqual(record["stdout"].count(f"app part sha256={same} app copy sha256={same}"), 2, record["stdout"])
        self.tearDown()
        self.setUp()
        self.part_one(mate_reached=False)
        record = self.world.audit("T3-22")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("with the set=1", record["stdout"])

    def test_t3_22_part_one_fails_when_the_given_set_does_not_follow_the_title(self):
        replaced = f"{audit.APP_MARK}\n\nUse anything."
        for claude, codex in (
            (f"{APP_SET}\n", rendered(APP_SET)),
            (rendered(f"Use tdd.\n\n{audit.APP_MARK}"), rendered(APP_SET)),
            (rendered(APP_SET), rendered("", APP_SET)),
            (rendered(replaced, MARK), rendered(APP_SET, MARK)),
            (rendered(APP_SET, MARK), rendered(replaced, MARK)),
        ):
            self.tearDown()
            self.setUp()
            self.part_one(mate_reached=True)
            w = self.world
            self.given(1, claude)
            (w.home(2) / "AGENTS.md").write_text(codex)
            record = w.audit("T3-22")
            self.assertEqual(record["result"], "FAIL", record["stdout"])
            self.assertIn("agents bad=1", record["detail"])

    def skill(self, name, hidden=False):
        """Approves one global skill with the engine's copy of it, its SKILL.md front matter
        setting disable-model-invocation when hidden. Inputs: the name and the flag."""
        w = self.world
        folder = f"plugins/skills/global/{name}"
        (w.root / folder).mkdir(parents=True)
        flag = "disable-model-invocation: true\n" if hidden else ""
        (w.root / folder / "SKILL.md").write_text(f"---\nname: {name}\ndescription: The {name} skill.\n{flag}---\n\nUse it.\n")
        record = {"id": f"skill-{name}", "name": name, "description": f"The {name} skill.", "scope": {"kind": "global"}, "enabled": True, "folder": folder}
        w.row("plugin_items", id=f"skill-{name}", kind="skill", scope="global", name=name, body=json.dumps(record))

    def listing(self, agent, names, content=False):
        """Writes the skill listing Claude Code records in an agent's transcript, by names or
        by its text only. Inputs: the agent, the listed names and whether only text is kept."""
        attachment = {"type": "skill_listing", "content": "".join(f"- {name}: The skill.\n" for name in names), "isInitial": True}
        if not content:
            attachment["names"] = list(names)
        self.world.transcript(agent, [{"type": "attachment", "attachment": attachment}])

    def rollout(self, agent, names):
        """Writes a Codex rollout whose developer text holds the engine's skills index for an
        agent's home. Inputs: the agent and the indexed names."""
        home = self.world.home(agent)
        folder = home / "sessions" / "2026" / "11" / "04"
        folder.mkdir(parents=True, exist_ok=True)
        index = "\n## Skills\n\n" + "".join(f"- {name}: The skill. ({home}/skills/crazyagent/{name}/SKILL.md)\n" for name in names)
        line = {"type": "response_item", "payload": {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": index}]}}
        (folder / "rollout-2026-11-04T10-00-00-1.jsonl").write_text(json.dumps(line) + "\n")

    def role_skills(self, worker_given=False, worker_listed=(), lead_harness="claude-code"):
        """A lead whose role chose tdd and grill-me, which Claude Code lists to no model, and a
        worker whose role chose none, each with the engine's skills record and the listing its
        harness made. Inputs: the worker's given skills, its listing and the lead's harness.
        Output: the lead id."""
        w = self.world
        self.skill("tdd")
        self.skill("grill-me", hidden=True)
        w.row("coord_settings", project=PROJECT, json=json.dumps({"role_skills": {"master": None, "lead": ["tdd", "grill-me"], "worker": []}}))
        lead = w.agent("sub", mode="app_spawned", harness=lead_harness)
        worker = w.agent("sub", parent=lead)
        self.given(lead, "grill-me\ntdd\n", kind="agent-skills")
        if lead_harness == "codex":
            self.rollout(lead, ["tdd", "grill-me"])
        else:
            self.listing(lead, ["crazyagent:tdd", "update-config"])
        self.given(worker, "tdd\n" if worker_given else "", kind="agent-skills")
        self.listing(worker, ["update-config", *worker_listed])
        return lead

    def test_t3_23_skills_per_role_pass_when_each_role_gets_its_choice_and_fail_when_a_worker_gets_more(self):
        self.assertEqual(self.verdict("T3-23"), "UNKNOWN")
        self.role_skills()
        record = self.world.audit("T3-23")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertIn("role lead chose grill-me, tdd; given grill-me, tdd; installed tdd; Claude Code lists none of grill-me to the model", record["stdout"])
        self.assertIn("role worker chose none; given none; installed none", record["stdout"])
        self.assertIn("skills Claude Code lists to no model: grill-me", record["detail"])
        for given in (dict(worker_given=True), dict(worker_listed=["crazyagent:tdd"])):
            self.tearDown()
            self.setUp()
            self.role_skills(**given)
            record = self.world.audit("T3-23")
            self.assertEqual(record["result"], "FAIL", (given, record["stdout"]))
            self.assertIn("role worker chose none; given " + ("tdd" if given.get("worker_given") else "none"), record["stdout"])

    def test_t3_23_reads_codex_skills_from_its_rollout_index_and_claude_skills_from_listing_text(self):
        self.role_skills(lead_harness="codex")
        record = self.world.audit("T3-23")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertIn("role lead chose grill-me, tdd; given grill-me, tdd; installed grill-me, tdd", record["stdout"])
        self.tearDown()
        self.setUp()
        w = self.world
        self.skill("tdd")
        w.row("coord_settings", project=PROJECT, json=json.dumps({"role_skills": {"master": None, "lead": ["tdd"], "worker": None}}))
        lead = w.agent("sub", mode="app_spawned")
        self.given(lead, "tdd\n", kind="agent-skills")
        self.listing(lead, ["crazyagent:tdd"], content=True)
        record = self.world.audit("T3-23")
        self.assertEqual(record["result"], "PASS", record["stdout"])
        self.assertIn("role lead chose tdd; given tdd; installed tdd", record["stdout"])

    def test_t3_23_is_unknown_when_no_harness_listing_was_read(self):
        w = self.world
        self.skill("tdd")
        w.row("coord_settings", project=PROJECT, json=json.dumps({"role_skills": {"master": None, "lead": ["tdd"], "worker": None}}))
        lead = w.agent("sub", mode="app_spawned")
        self.given(lead, "tdd\n", kind="agent-skills")
        record = self.world.audit("T3-23")
        self.assertEqual(record["result"], "UNKNOWN", record["stdout"])
        self.assertIn("installed unknown (no skill listing in its transcripts)", record["stdout"])
        self.assertIn(f"no skill listing read for {lead}(lead app_spawned)", record["detail"])

    def nudges(self, ten_minutes_too):
        w = self.world
        w.row("coord_settings", project=PROJECT, json=json.dumps({"nudges": "first_edit"}))
        lead = w.agent("sub", mode="app_spawned")
        mailbox = w.root / "team" / "agents" / f"agent-{lead}" / "tmp" / audit.MAILBOX
        mailbox.mkdir(parents=True)
        (mailbox / f"{0:032x}.nudged").write_text("")
        if ten_minutes_too:
            w.row("coord_messages", project=PROJECT, from_kind="engine", to_kind="agent", to_agent=lead, kind="note",
                  idem_key=f"adoption-edits_without_start_work-{lead}", body="CrazyAgent reminder", receipt="read", created=w.now, receipt_at=w.now)

    def test_t3_24_nudges_pass_on_a_first_edit_reminder_and_fail_when_a_ten_minute_nudge_is_sent_too(self):
        record = self.world.audit("T3-24")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("no setting: off since 18.13", record["detail"])
        self.nudges(ten_minutes_too=False)
        record = self.world.audit("T3-24")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("hook markers=agent 1 x1", record["detail"])
        self.tearDown()
        self.setUp()
        self.nudges(ten_minutes_too=True)
        record = self.world.audit("T3-24")
        self.assertEqual(record["result"], "FAIL")
        self.assertIn("ten-minute nudges sent under first_edit", record["detail"])

    def reask(self, setting, asked, reply="Added test_calc.py.", said=audit.REASK_EMPTY):
        w = self.world
        w.db.executescript("CREATE TABLE team_reask_empty (project TEXT PRIMARY KEY, reask INTEGER NOT NULL CHECK(reask IN (0,1)));")
        if setting is not None:
            w.row("team_reask_empty", project=PROJECT, reask=setting)
        lead = w.agent("sub", mode="app_spawned")
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt="add calc.py", status="done",
              agent=lead, result="", created_ms=w.now)
        w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt="add a test", status="done",
              agent=lead, result=reply, created_ms=w.now)
        w.transcript(lead, [prompt(w.now - 5000, "add calc.py")] + ([prompt(w.now, said)] if asked else []))

    def test_t3_25_re_ask_fails_when_an_empty_reply_is_not_re_asked_as_the_setting_says(self):
        w = self.world
        w.row("team_messages", project=PROJECT, author="engine", state="note", created_ms=w.now,
              text="The master replied with no text, so no work was handed out. Send the message again, or hand a task to a sub-agent yourself.")
        record = w.audit("T3-25")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not in this build", record["detail"])
        self.assertIn("empty master replies noted=1", record["detail"])
        self.assertNotIn("feat/", record["detail"])
        cases = (
            (None, False, "FAIL", "no re-ask"),
            (None, True, "PASS", "setting=on (no row: on by default)"),
            (1, False, "FAIL", "no re-ask"),
            (1, True, "PASS", "re-ask turns in transcripts=1"),
            (0, False, "PASS", "setting=off"),
            (0, True, "FAIL", "re-asked while the setting is off"),
        )
        for setting, asked, result, shown in cases:
            self.tearDown()
            self.setUp()
            self.reask(setting, asked)
            record = self.world.audit("T3-25")
            self.assertEqual(record["result"], result, (setting, asked, record["detail"]))
            self.assertIn(shown, record["detail"])
            self.assertIn("done tasks with no reply text=1", record["detail"])
        self.tearDown()
        self.setUp()
        self.world.db.executescript("CREATE TABLE team_reask_empty (project TEXT PRIMARY KEY, reask INTEGER NOT NULL CHECK(reask IN (0,1)));")
        record = self.world.audit("T3-25")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not exercised", record["detail"])

    def finished(self, shown, drawn="Master", ok=True, show=0):
        w = self.world
        w.db.executescript("CREATE TABLE team_finished_view (project TEXT PRIMARY KEY, show INTEGER NOT NULL);")
        w.row("team_finished_view", project=PROJECT, show=show)
        w.agent("master", status="idle")
        lead = w.agent("sub", mode="app_spawned", stopped_ms=w.now - 60_000)
        worker = w.agent("sub", parent=lead, stopped_ms=w.now - 900_000)
        for agent, handle in ((lead, "lead-ada"), (worker, "worker-bo")):
            w.row("team_names", agent=agent, project=PROJECT, persona=handle.title(), handle=handle)
        for prompt in ("Write a.md", "Write b.md"):
            w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt=prompt, status="done",
                  agent=worker, result="Done.", created_ms=w.now)
        (w.out / "screenshots").mkdir(parents=True, exist_ok=True)
        (w.out / "screenshots" / "20-finished-hide.png").write_bytes(b"\x89PNG")
        steps = [
            {"n": 18, "surface": "desktop", "title": "Stop all", "at": iso(w.now - 300_000), "ok": True},
            {"n": 20, "surface": "desktop", "title": audit.FINISHED_HIDE_STEP, "at": iso(w.now), "ok": ok,
             "detail": f"Finished ({shown}); drawn: {drawn}", "evidence": ["screenshots/20-finished-hide.png"]},
        ]
        (w.out / "steps.jsonl").write_text("".join(json.dumps(step) + "\n" for step in steps))

    def test_t3_26_finished_agents_pass_when_hide_lists_every_stopped_agent_in_finished_and_draws_none(self):
        record = self.world.audit("T3-26")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not in this build", record["detail"])
        self.finished(shown=2)
        record = self.world.audit("T3-26")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("finished in the store=2", record["detail"])
        self.assertIn("left on their own before Stop all=1", record["detail"])
        self.assertIn("agents given more than one task=1", record["detail"])
        self.assertIn("screenshots/20-finished-hide.png", [item["path"] for item in record["evidence"] if item["kind"] == "screenshot"])
        cases = (
            (dict(shown=1), "Finished (1) shown, 2 finished in the store"),
            (dict(shown=2, drawn="Master, worker-bo"), "drawn while finished: worker-bo"),
            (dict(shown=2, ok=False), "the step failed"),
        )
        for given, shown in cases:
            self.tearDown()
            self.setUp()
            self.finished(**given)
            record = self.world.audit("T3-26")
            self.assertEqual(record["result"], "FAIL", (given, record["detail"]))
            self.assertIn(shown, record["detail"])
        self.tearDown()
        self.setUp()
        self.finished(shown=2, show=1)
        record = self.world.audit("T3-26")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn("setting at the end=show, a note: the trial sets Show for its final tree", record["detail"])
        self.tearDown()
        self.setUp()
        self.world.db.executescript("CREATE TABLE team_finished_view (project TEXT PRIMARY KEY, show INTEGER NOT NULL);")
        record = self.world.audit("T3-26")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not exercised", record["detail"])

    def hide_check(self, ok, said):
        """Finished agents in the store and the trial 4 N-D25 step, which sets Hide once an
        agent left on its own and Show again at its end, with what it said. Inputs: whether
        the step held and its detail or failure."""
        self.finished(shown=2, show=1)
        w = self.world
        title = "N-D25 (OP-27) A sub-agent that finished leaves the graph under Hide and Wake brings the same agent back"
        steps = [
            {"n": 31, "surface": "desktop", "title": title, "at": iso(w.now - 400_000), "ok": ok, "detail": said, "evidence": ["screenshots/20-finished-hide.png"]},
            {"n": 32, "surface": "desktop", "title": "Stop all", "at": iso(w.now - 300_000), "ok": True},
        ]
        (w.out / "steps.jsonl").write_text("".join(json.dumps(step) + "\n" for step in steps))

    def test_t3_26_judges_hide_at_the_n_d25_step_when_no_hide_step_ran(self):
        left = "Bo (@worker-bo) left on its own; under Hide it left the graph for Finished (1); Wake ran the same agent 3 again (working); Finished agents set back to Show"
        cases = (
            (True, left, "PASS", "N-D25 step 31 ok=True"),
            (False, "Bo (@worker-bo) did not run again as the same agent after Wake", "PASS", "Hide held before the step failed"),
            (False, "under Hide, Bo (@worker-bo) did not leave the graph for the Finished list", "FAIL", "under Hide the finished agent stayed in the graph"),
            (True, "Bo (@worker-bo) left on its own; Finished agents set back to Show", "FAIL", "recorded no Finished (N) count under Hide"),
            (False, "no sub-agent left on its own after its idle stop within 1200 s; last seen: 2 ended agents", "UNKNOWN", "N-D25 did not reach Hide"),
            (False, "element not found", "UNKNOWN", "N-D25 failed before Hide could be told"),
        )
        for ok, said, result, shown in cases:
            self.tearDown()
            self.setUp()
            self.hide_check(ok, said)
            record = self.world.audit("T3-26")
            self.assertEqual(record["result"], result, (said, record["detail"]))
            self.assertIn(shown, record["detail"])
            self.assertIn("setting at the end=show, a note", record["detail"])
            self.assertIn("screenshots/20-finished-hide.png", [item["path"] for item in record["evidence"] if item["kind"] == "screenshot"])

    def two_on_accounts(self, first, second, second_role="sub"):
        w = self.world
        lead = w.agent("sub", mode="app_spawned", connection=first[0])
        other = w.agent(second_role, parent=lead, connection=second[0])
        for agent, (_, start, end) in ((lead, first), (other, second)):
            w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt="Write a.md", status="done",
                  agent=agent, created_ms=w.now - 900_000, started_ms=w.now - 600_000 + start * 1000,
                  finished_ms=None if end is None else w.now - 600_000 + end * 1000)
        return lead, other

    def test_t3_27_shared_account_passes_when_two_agents_run_on_one_account_at_once_and_fails_when_one_waited(self):
        record = self.world.audit("T3-27")
        self.assertEqual(record["result"], "UNKNOWN")
        self.assertIn("not exercised", record["detail"])
        work, home = "claude-max-work", "claude-max-home"
        cases = (
            ((work, 0, 300), (work, 100, 400), "sub", "PASS", "ran tasks 1 and 2 on account 1 together for 200 s"),
            ((work, 0, None), (work, 100, 400), "sub", "PASS", "together for 300 s"),
            ((work, 0, 300), (work, 300, 400), "sub", "UNKNOWN", "agents on one account at once=0"),
            ((work, 0, 300), (home, 100, 400), "sub", "UNKNOWN", "on accounts=2"),
            ((work, 0, 300), (work, 100, 400), "teammate", "UNKNOWN", "agents with an account=1"),
        )
        for first, second, role, result, shown in cases:
            self.tearDown()
            self.setUp()
            self.two_on_accounts(first, second, role)
            record = self.world.audit("T3-27")
            self.assertEqual(record["result"], result, (first, second, role, record["detail"]))
            self.assertIn(shown, record["detail"])
            for path in self.world.out.rglob("*"):
                if path.is_file():
                    self.assertNotIn("claude-max", path.read_text(encoding="utf-8", errors="replace"), path)
        for table, said in (("team_tasks", "error"), ("team_events", "text")):
            self.tearDown()
            self.setUp()
            w = self.world
            lead, _ = self.two_on_accounts((work, 0, 300), (work, 100, 400))
            if table == "team_tasks":
                waited = w.row("team_tasks", project=PROJECT, source="master", harness="claude-code", tools=1, prompt="Write b.md",
                               status="queued", agent=lead, created_ms=w.now, error="native_profile_in_use")
            else:
                waited = w.row("team_events", project=PROJECT, agent=lead, kind="status", text="Waiting for a free account",
                               state="new", created_ms=w.now)
            record = w.audit("T3-27")
            self.assertEqual(record["result"], "FAIL", (table, record["detail"]))
            self.assertIn(f"an agent waited for its account: {table} {waited}", record["detail"])

    def test_every_check_writes_its_contract_record_with_evidence(self):
        w = self.world
        w.agent("sub", mode="app_spawned")
        done = subprocess.run(
            [sys.executable, "-I", str(HERE / "audit.py"), PROJECT, "--engine", str(w.root), "--evidence", str(w.out)],
            capture_output=True, text=True, env=w.env, timeout=120,
        )
        self.assertIn(done.returncode, (0, 1), done.stderr)
        written = sorted(p.stem for p in (w.out / "checks").glob("*.json"))
        self.assertEqual(written, recorded_ids())
        self.assertEqual(sum(name.startswith("T3-") for name in written), 27)
        for name in written:
            record = json.loads((w.out / "checks" / f"{name}.json").read_text())
            self.assertEqual(set(record), {"id", "title", "proves", "why", "how", "result", "detail", "evidence"})
            self.assertIn(record["result"], audit.RESULTS)
            self.assertTrue(record["evidence"])
            for item in record["evidence"]:
                self.assertIn(item["kind"], ("screenshot", "frame", "log", "db", "gif"))
                self.assertTrue((w.out / item["path"]).is_file(), item)
        self.assertIn("SUMMARY project=trial", done.stdout)

    def test_the_runner_argv_from_the_evidence_contract_runs_the_audit(self):
        w = self.world
        w.agent("sub", mode="app_spawned")
        argv = ["python3", "-I", str(HERE / "audit.py"), PROJECT, "--engine", str(w.root), "--evidence", str(w.out), "--project-mark", MARK]
        done = subprocess.run(argv, capture_output=True, text=True, env=w.env, timeout=120)
        self.assertIn(done.returncode, (0, 1), done.stdout + done.stderr)
        self.assertEqual(sorted(p.stem for p in (w.out / "checks").glob("*.json")), recorded_ids())
        self.assertIn("SUMMARY project=trial", done.stdout)

    def test_secrets_across_a_cut_are_masked_before_the_cut(self):
        w = self.world
        jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        reply = "r" * (audit.VALUE_BYTES_MAX - 25) + " " + jwt + " " + "t" * 200
        key = "sk-ant-api03-" + "K" * 40
        start = json.dumps(prompt(w.now, f"{audit.REASK_EMPTY}  {key}")).index(key)
        said = f"{audit.REASK_EMPTY} {'p' * (audit.EXCERPT_BYTES_MAX - 11 - start)} {key} {'t' * 200}"
        self.reask(None, True, reply=reply, said=said)
        record = w.audit("T3-25")
        self.assertEqual(record["result"], "PASS", record["detail"])
        self.assertIn(" chars)", (w.out / "db" / "T3-25.json").read_text())
        self.assertIn(" chars)", record["log"])
        files = sorted(p for p in w.out.rglob("*") if p.is_file())
        leaks = [
            (str(path.relative_to(w.out)), leaked)
            for path in files
            for leaked in ("eyJhbGciOiJIUzI1NiJ9", "sk-ant")
            if leaked in path.read_text(encoding="utf-8", errors="replace")
        ]
        self.assertEqual(leaks, [])

    def test_masking_hides_keys_tokens_codes_ids_and_emails_and_keeps_plain_text(self):
        text = (
            'sk-ant-api03-abcdefghijklmnopqrstu Bearer abc.def-123 "access_token": "tok123" account_id=9f3c '
            "eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4 ghp_" + "a" * 30 + " code WDJB-MJHT a@b.io Cookie: s=1"
        )
        shown = audit.mask(text)
        for leaked in ("abcdefghijklmnopqrstu", "abc.def-123", "tok123", "9f3c", "eyJhbGci", "ghp_", "WDJB-MJHT", "a@b.io", "s=1"):
            self.assertNotIn(leaked, shown)
        self.assertEqual(audit.mask({"rows": [["calc.py", 3, None]]}), {"rows": [["calc.py", 3, None]]})
        self.assertEqual(json.loads(audit.mask('{"email": "x@y.dev", "n": 1}')), {"email": audit.MASK, "n": 1})

    def test_a_missing_project_is_a_failed_check_with_its_own_record(self):
        w = self.world
        done = subprocess.run(
            [sys.executable, "-I", str(HERE / "audit.py"), "no-such-project", "--engine", str(w.root), "--evidence", str(w.out)],
            capture_output=True, text=True, env=w.env, timeout=60,
        )
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        record = json.loads((w.out / "checks" / "T3-00.json").read_text())
        self.assertEqual(record["result"], "FAIL")
        self.assertTrue(record["evidence"])


if __name__ == "__main__":
    unittest.main()
