"""Audits one trial run of a CrazyAgent project from the engine store, read only (DEC-112).

Inputs: the project folder or id, the engine root (agent homes, app instructions) and the
trial run folder. Output: checks/<id>.json per check with db/<id>.json rows and a masked
logs/<id>.log excerpt, one verdict line per check on stdout, exit 1 when any check fails.
"""

if not __debug__:
    raise SystemExit("the trial audit requires Python assertions enabled")

import argparse
from collections import Counter
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
from urllib.parse import quote

LIVE = ("starting", "idle", "working")
WORK_TOOLS = ("start_work", "inspect_work", "update_work", "transfer_work", "finish_work")
SHELL_TOOLS = ("Bash", "shell", "shell_command", "exec_command", "local_shell_call", "container.exec")
SEVERITIES = ("high", "normal", "low")
RESULTS = ("PASS", "FAIL", "UNKNOWN")
SLUG = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
CHECK_ID = re.compile(r"^T3-[0-9]{2}$")
OFFLOAD = "so CrazyAgent wrote it to `"
COMMAND_BYTES_MAX = 16 * 1024
LISTED_MAX = 8
REFS_MAX = 64
APP_MARK = "# Mandatory workflow"
INSTRUCTION_NAMES = ("CLAUDE.md", "AGENTS.md", "CLAUDE.local.md")
IMPORTED_HEADING = "## Imported from "
STATE = ".crazyagent"
TEXT_BYTES_MAX = 64 * 1024
SKILL_PREFIX = "skill:"
CALLER_UNKNOWN = "team_caller_unknown"
REPLY_INVALID = "coord_reply_invalid"
XCRUN = re.compile(r"xcrun_db|xcrun: error")
GIT_WORD = re.compile(r"(^|[\s;&|(])git(\s|$)")
FINISH_EVENTS = ("finished", "change_set_ready")
BACKGROUND_STEM = "while a command it started in the background still ran; that command was stopped"
BACKGROUND_ENDS = (
    ("grace", "The background grace time in Team settings ended this task"),
    ("stop", "This task ended with its turn, as Team settings say,"),
    ("time limit", "The time limit ended this task"),
    ("interrupt", "The owner's interrupt ended this task"),
)
BACKGROUND_TOLD = "Commands you start in the background stop when your turn ends."
FIRST_EDIT_REMINDER = "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped."
PHRASES = (BACKGROUND_TOLD, FIRST_EDIT_REMINDER)
NUDGE_KEYS = (
    ("first_edit", "adoption-first_edit-"),
    ("ten_minutes", "adoption-edits_without_start_work-"),
    ("ten_minutes", "adoption-no_graphify_or_context_mode-"),
)
NUDGE_MODES = ("off", "ten_minutes", "first_edit")
SKILL_ROLES = ("master", "lead", "worker")
DEPTH_DEFAULT = 2
EMPTY_REPLY_NOTE = "The master replied with no text, so no work was handed out."
HIDDEN_RULE = "(deny file-read* file-write* process-exec file-link file-clone"
MAILBOX = "crazyagent-team-calls"
GIT_ENV = dict(os.environ, GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
GIT_SAFE = ("-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false")
ROWS_PER_READ_MAX = 200
READS_PER_CHECK_MAX = 400
CITES_PER_CHECK_MAX = 16
EXCERPT_BYTES_MAX = 2000
VALUE_BYTES_MAX = 4096
MASK = "[masked]"
SECRETS = (
    re.compile(r"sk-[A-Za-z0-9_\-]{16,}"),
    re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/\-]+=*"),
    re.compile(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}"),
    re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|xox[abprs]-[A-Za-z0-9\-]{10,}|ya29\.[A-Za-z0-9_\-]+|AKIA[0-9A-Z]{16})\b"),
    re.compile(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}"),
    re.compile(r"\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b"),
)
KEYED = re.compile(
    r"(?i)(\"?\b(?:access_?token|refresh_?token|id_?token|api_?key|auth_?token|session_?token|token|secret|password"
    r"|cookie|set-cookie|authorization|account_?id|account_?uuid|organization_?uuid|org_?id|user_?id|email"
    r"|device_?code|user_?code)\b\"?\s*[:=]\s*)(\"(?:[^\"\\]|\\.)*\"|[^\s,;}\]]+)"
)


class Missing(Exception):
    pass


def mask(value):
    """Hides secrets in a value about to be written: keys, tokens, Bearer values, cookies,
    device codes, account ids and emails. Takes a string, number, list, dict or None.
    Returns the same shape with each secret replaced by [masked].
    """
    if isinstance(value, str):
        text = KEYED.sub(lambda m: m.group(1) + ('"' + MASK + '"' if m.group(2).startswith('"') else MASK), value)
        for pattern in SECRETS:
            text = pattern.sub(MASK, text)
        return text
    if isinstance(value, dict):
        return {mask(key): mask(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [mask(item) for item in value]
    return value


def listed(items):
    items = list(items)
    shown = ", ".join(str(item) for item in items[:LISTED_MAX])
    return shown + (f", +{len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def verdict(result, detail, lines=(), cites=(), extra=()):
    assert result in RESULTS, result
    return {"result": result, "detail": detail, "lines": list(lines), "cites": list(cites), "extra": list(extra)}


def bounded(value):
    if isinstance(value, bytes):
        return f"<{len(value)} bytes>"
    if isinstance(value, str) and len(value) > VALUE_BYTES_MAX:
        return value[:VALUE_BYTES_MAX] + f"... (+{len(value) - VALUE_BYTES_MAX} chars)"
    return value


def git_run(cwd, *args):
    if not cwd or not Path(cwd).is_dir():
        return None
    try:
        return subprocess.run(
            ["git", *GIT_SAFE, "-C", str(cwd), *args],
            capture_output=True, text=True, timeout=30, env=GIT_ENV, stdin=subprocess.DEVNULL,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None


def git(cwd, *args):
    done = git_run(cwd, *args)
    return done.stdout.strip() if done is not None and done.returncode == 0 else None


def git_ok(cwd, *args):
    done = git_run(cwd, *args)
    return done is not None and done.returncode == 0


def common_dir(wt):
    found = git(wt, "rev-parse", "--path-format=absolute", "--git-common-dir")
    return os.path.realpath(found) if found else None


def under(path, folder):
    path, folder = os.path.realpath(path), os.path.realpath(folder)
    return path == folder or path.startswith(folder.rstrip("/") + "/")


def shell_command(given):
    if not isinstance(given, dict):
        return None
    command = given.get("command")
    if command is None:
        command = given.get("cmd")
    if isinstance(command, list):
        command = " ".join(word for word in command if isinstance(word, str))
    return command[:COMMAND_BYTES_MAX] if isinstance(command, str) else None


def tool_key(name, given):
    if not isinstance(name, str):
        return None
    rest = name[len("mcp__"):] if name.startswith("mcp__") else name
    if "__" in rest:
        server, tool = rest.rsplit("__", 1)
        return "graphify" if "graphify" in server else tool
    if name in SHELL_TOOLS:
        command = shell_command(given)
        if command is not None:
            words = re.split(r"[\s;&|()'\"`]", command)
            if any(word.rsplit("/", 1)[-1] == "graphify" for word in words):
                return "graphify"
            if "SKILL.md" in command:
                return "Skill"
    return name


def skill_of(name, given):
    if not isinstance(given, dict):
        return None
    if name == "Skill":
        skill = given.get("skill")
        return skill if isinstance(skill, str) and skill else None
    command = shell_command(given) if name in SHELL_TOOLS else None
    for word in re.split(r"[\s;&|()'\"`]", command or ""):
        if word.endswith("/SKILL.md"):
            folder = word[: -len("/SKILL.md")].rsplit("/", 1)[-1]
            return folder or None
    return None


def result_text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict) and isinstance(part.get("text"), str))
    return ""


def stamp_ms(value):
    if not isinstance(value, str):
        return None
    try:
        return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)
    except ValueError:
        return None


def slug_of(path):
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def blank_scan():
    return {
        "files": 0, "calls": 0, "ctx": 0, "graphify": 0, "Skill": 0, "overlap": 0, "first": {}, "at": {},
        "skills": Counter(), "tools": Counter(), "codes": Counter(), "xcrun": 0, "git": 0, "git_ok": 0,
        "work_ok": Counter(), "work_failed": Counter(), "prompts": [], "launches": [], "repo_reads": [],
        "phrases": Counter(),
    }


def launch_record(text, spot, early, project_mark):
    return {
        "spot": spot,
        "app": text.find(APP_MARK),
        "project": text.find(project_mark),
        "marks": text.count(project_mark) + sum(item.count(project_mark) for item in early),
        "bytes": len(text.encode()),
    }


def scan_file(path, found, root, project_mark):
    found["files"] += 1
    top = path.parent.parent.name == "projects"
    where = str(path.relative_to(next(p for p in path.parents if p.name == "projects").parent))
    uses, early = {}, []
    launch = snapshot = None
    try:
        handle = path.open("r", encoding="utf-8", errors="replace")
    except OSError:
        return
    with handle:
        for number, line in enumerate(handle, 1):
            spot = f"{where}:{number}"
            for phrase in PHRASES:
                if phrase in line:
                    found["phrases"][phrase] += 1
                    if phrase not in found["first"]:
                        found["first"][phrase] = spot
                        found["at"][spot] = (str(path), number)
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if not isinstance(entry, dict):
                continue
            kind = entry.get("type")

            def first(name):
                if name not in found["first"]:
                    found["first"][name] = spot
                    found["at"][spot] = (str(path), number)

            if kind == "attachment":
                attachment = entry.get("attachment")
                if not isinstance(attachment, dict):
                    continue
                if attachment.get("type") == "instructions":
                    for item in attachment.get("files") or []:
                        file = item.get("path") if isinstance(item, dict) else None
                        if isinstance(file, str) and file.rsplit("/", 1)[-1] in INSTRUCTION_NAMES and not under(file, root):
                            found["repo_reads"].append(f"{file} at {spot}")
                            found["at"][spot] = (str(path), number)
                        if launch is None and isinstance(item, dict) and isinstance(item.get("content"), str):
                            early.append(item["content"])
                elif attachment.get("type") == "prompt_snapshot" and top and launch is None and snapshot is None:
                    parts = attachment.get("systemPrompt")
                    if isinstance(parts, list):
                        snapshot = ("\n".join(part for part in parts if isinstance(part, str)), spot)
                        found["at"][spot] = (str(path), number)
                elif attachment.get("type") == "queued_command":
                    found["prompts"].append(stamp_ms(entry.get("timestamp")))
                continue
            if kind == "api-request-shape":
                shape = entry.get("shape")
                system = shape.get("system") if isinstance(shape, dict) else None
                if top and launch is None and isinstance(system, list):
                    launch = ("\n".join(p["text"] for p in system if isinstance(p, dict) and isinstance(p.get("text"), str)), spot)
                    found["at"][spot] = (str(path), number)
                continue
            message = entry.get("message")
            content = message.get("content") if isinstance(message, dict) else None
            if kind == "user" and (
                isinstance(content, str)
                or (isinstance(content, list) and any(isinstance(p, dict) and p.get("type") == "text" for p in content))
            ):
                found["prompts"].append(stamp_ms(entry.get("timestamp")))
            if not isinstance(content, list):
                continue
            for part in content:
                if not isinstance(part, dict):
                    continue
                if part.get("type") == "tool_use":
                    name, given = part.get("name"), part.get("input")
                    key = tool_key(name, given)
                    if key is None:
                        continue
                    found["calls"] += 1
                    found["tools"][key] += 1
                    counted = "ctx" if key.startswith("ctx_") else key if key in ("graphify", "Skill") else None
                    if counted:
                        found[counted] += 1
                        first(counted)
                    skill = skill_of(name, given)
                    if skill:
                        found["skills"][skill] += 1
                    command = shell_command(given) if name in SHELL_TOOLS else None
                    uses[part.get("id")] = (key, bool(command and GIT_WORD.search(command)))
                elif part.get("type") == "tool_result":
                    text = result_text(part.get("content"))
                    failed = part.get("is_error") is True
                    for code in (CALLER_UNKNOWN, REPLY_INVALID):
                        if code in text:
                            found["codes"][code] += 1
                            first(code)
                    xcrun = bool(XCRUN.search(text))
                    if xcrun:
                        found["xcrun"] += 1
                        first("xcrun")
                    key, git_call = uses.get(part.get("tool_use_id"), (None, False))
                    if key in WORK_TOOLS:
                        (found["work_failed"] if failed else found["work_ok"])[key] += 1
                        if "overlap" in text.lower():
                            found["overlap"] += 1
                            first("overlap")
                    if git_call:
                        found["git"] += 1
                        if not failed and not xcrun:
                            found["git_ok"] += 1
    if top:
        chosen = launch or snapshot
        found["launches"].append(launch_record(chosen[0], chosen[1], early, project_mark) if chosen else {"spot": where, "app": None})


def processes():
    try:
        done = subprocess.run(["ps", "-axww", "-o", "pid=,pgid=,command="], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if done.returncode != 0:
        return None
    mine = {os.getpid(), os.getppid()}
    found = []
    for line in done.stdout.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) < 2 or not fields[0].isdigit() or not fields[1].isdigit():
            continue
        if int(fields[0]) in mine:
            continue
        found.append((int(fields[0]), int(fields[1]), fields[2] if len(fields) > 2 else ""))
    return found


def read_text(path):
    try:
        with open(path, "rb") as handle:
            return handle.read(TEXT_BYTES_MAX + 1).decode("utf-8", errors="replace")
    except OSError:
        return None


class Trial:
    """The project's engine rows, agent transcripts and processes one audit reads.
    Takes the open read-only store, the engine root, the project id or folder and the
    trial's project mark and owner answer. Holds the rows each check reads.
    """

    def __init__(self, db, root, target, project_mark, answer):
        self.db, self.root, self.target = db, root, target
        self.project_mark, self.answer = project_mark, answer
        self.reads = None
        self.cites = None
        self.pid = self.find_project()
        if self.pid is None:
            return
        self.repo = self.project_folder()
        self.load()

    def q(self, sql, *args):
        try:
            rows = self.db.execute(sql, args).fetchall()
        except sqlite3.OperationalError as failure:
            raise Missing(str(failure)) from None
        if self.reads is not None and len(self.reads) < READS_PER_CHECK_MAX:
            self.reads.append({
                "sql": " ".join(sql.split()),
                "args": [bounded(arg) for arg in args],
                "count": len(rows),
                "rows": [[bounded(value) for value in row] for row in rows[:ROWS_PER_READ_MAX]],
            })
        return rows

    def columns(self, table):
        try:
            return {row[1] for row in self.db.execute(f"PRAGMA table_info({table})").fetchall()}
        except sqlite3.OperationalError:
            return set()

    def cite(self, scan, spot, caption):
        if self.cites is None or spot not in scan["at"] or len(self.cites) >= CITES_PER_CHECK_MAX:
            return
        path, number = scan["at"][spot]
        self.cites.append((path, number, caption))

    def find_project(self):
        projects = self.q("SELECT id, body FROM projects")
        for pid, _ in projects:
            if pid == self.target:
                return pid
        want = os.path.realpath(os.path.expanduser(self.target))
        for pid, body in projects:
            try:
                repositories = json.loads(body)["spec"]["repositories"]
            except (ValueError, KeyError, TypeError):
                continue
            for repository in repositories:
                if isinstance(repository, str) and os.path.realpath(os.path.expanduser(repository)) == want:
                    return pid
        try:
            for (pid,) in self.q("SELECT DISTINCT project FROM team_worktrees WHERE repository=?", want):
                return pid
        except Missing:
            pass
        return None

    def project_folder(self):
        given = os.path.expanduser(self.target)
        if os.path.isdir(given):
            return os.path.realpath(given)
        try:
            repositories = json.loads(self.q("SELECT body FROM projects WHERE id=?", self.pid)[0][0])["spec"]["repositories"]
            return os.path.realpath(os.path.expanduser(repositories[0]))
        except (IndexError, ValueError, KeyError, TypeError):
            return None

    def load(self):
        self.reads = []
        optional = ", ".join(f"a.{name}" if name in self.columns("team_agents") else "NULL" for name in ("session", "pane", "mode"))
        self.agents = {}
        for row in self.q(
            "SELECT a.id,a.role,a.harness,a.worktree,a.status,a.process_group,a.parent,a.stopped_ms,a.created_ms,"
            "a.turns,a.heartbeat_ms," + optional + ","
            "(SELECT handle FROM team_names n WHERE n.agent=a.id),"
            "(SELECT persona FROM team_names n WHERE n.agent=a.id),"
            "(SELECT worktree FROM team_worktrees w WHERE w.agent=a.id) "
            "FROM team_agents a WHERE a.project=? ORDER BY a.id",
            self.pid,
        ):
            agent = dict(zip(
                ("id", "role", "harness", "worktree", "status", "pgid", "parent", "stopped", "created", "turns", "heartbeat",
                 "session", "pane", "mode", "handle", "persona", "recorded"),
                row,
            ))
            wt = agent["recorded"] or agent["worktree"]
            if wt and not os.path.isabs(wt) and self.repo:
                wt = os.path.join(self.repo, wt)
            agent["wt"] = wt
            self.agents[agent["id"]] = agent
        self.task_modes = {}
        if "mode" in self.columns("team_tasks"):
            for agent_id, mode in self.q(
                "SELECT agent, mode FROM team_tasks WHERE project=? AND agent IS NOT NULL AND mode IS NOT NULL ORDER BY id", self.pid,
            ):
                self.task_modes.setdefault(agent_id, mode)
        try:
            self.pane_records = {row[0] for row in self.q("SELECT agent FROM team_panes")}
        except Missing:
            self.pane_records = set()
        self.masters = [a for a in self.agents.values() if a["role"] == "master"]
        self.master_ids = {a["id"] for a in self.masters}
        self.leads = [a for a in self.agents.values() if a["role"] != "master" and (a["parent"] is None or a["parent"] in self.master_ids)]
        self.lead_ids = {a["id"] for a in self.leads}
        self.children = {
            lead["id"]: [a for a in self.agents.values() if a["parent"] == lead["id"] and a["id"] != lead["id"]] for lead in self.leads
        }
        try:
            self.panes = {
                lead["id"]: [r[0] for r in self.q("SELECT pane FROM team_teammates WHERE project=? AND agent=?", self.pid, lead["id"])]
                for lead in self.leads
            }
        except Missing:
            self.panes = {lead["id"]: [] for lead in self.leads}
        self.load_reads, self.reads = self.reads, None
        self.dirs_of = {agent_id: self.transcript_dirs(agent) for agent_id, agent in self.agents.items()}
        self.transcripts = {agent_id: self.scan(self.dirs_of[agent_id]) for agent_id in self.agents}
        claimed = {folder for folders in self.dirs_of.values() for folder in folders}
        self.pane_scans = []
        for lead in self.leads:
            base = self.home(lead["id"]) / "projects"
            if self.mode_of(lead) != "native" or not base.is_dir() or self.dirs_of[lead["id"]] in ([], [base]):
                continue
            for folder in sorted(base.iterdir()):
                if folder.is_dir() and folder not in claimed and any(folder.glob("*.jsonl")):
                    self.pane_scans.append((f"lead {lead['id']} pane {folder.name[-32:]}", lead, self.scan([folder])))
        self.procs = processes()
        self.tree = self.masters + self.leads + [a for lead in self.leads for a in self.children[lead["id"]]]

    def sub_count(self, lead):
        rows = self.children[lead["id"]]
        spawned = sum(1 for a in rows if a["role"] == "sub")
        native = sum(1 for a in rows if a["role"] != "sub")
        return spawned + max(native, len(self.panes[lead["id"]]))

    def is_native(self, lead):
        return any(a["role"] == "teammate" for a in self.children[lead["id"]]) or bool(self.panes[lead["id"]])

    def mode_of(self, lead):
        return lead["mode"] or self.task_modes.get(lead["id"]) or ("native" if self.is_native(lead) else "app_spawned")

    def label(self, agent):
        if agent["role"] == "master":
            return f"{agent['id']}(master)"
        if agent["id"] in self.lead_ids:
            return f"{agent['id']}(lead {self.mode_of(agent)})"
        return f"{agent['id']}({'teammate' if agent['role'] == 'teammate' else 'sub-agent'})"

    def home(self, agent_id):
        return self.root / "team" / "agents" / f"agent-{agent_id}" / "home"

    def tmp_path(self, agent_id):
        digest = hashlib.sha256(str(self.home(agent_id)).encode()).digest()[:8]
        return f"/private/tmp/ca-{digest.hex()}"

    def branch_of(self, wt):
        if not wt:
            return None
        try:
            rows = self.q("SELECT branch FROM landing_agent_branches WHERE worktree=?", wt)
        except Missing:
            rows = []
        if rows and rows[0][0]:
            branch = rows[0][0]
        else:
            branch = git(wt, "symbolic-ref", "--short", "-q", "HEAD")
        return branch[len("refs/heads/"):] if branch and branch.startswith("refs/heads/") else branch

    def transcript_dirs(self, agent):
        own = self.home(agent["id"]) / "projects"
        base = own
        if not own.is_dir() and agent["role"] == "teammate" and agent["parent"] in self.agents:
            base = self.home(agent["parent"]) / "projects"
        if not base.is_dir():
            return []
        named = base / slug_of(agent["wt"]) if agent["wt"] else None
        if named is not None and named.is_dir():
            return [named]
        return [base] if base == own else []

    def scan(self, dirs):
        found = blank_scan()
        for folder in dirs:
            for path in sorted(folder.glob("**/*.jsonl")):
                scan_file(path, found, self.root, self.project_mark)
        return found

    def owned_by(self, agent, process):
        _, pgid, command = process
        if agent["pgid"] is not None and pgid == agent["pgid"]:
            return True
        folder = Path("team") / "agents" / f"agent-{agent['id']}"
        marks = {f"{self.root / folder}/", f"{Path(os.path.realpath(self.root)) / folder}/", self.tmp_path(agent["id"]) + "/"}
        return any(mark in command for mark in marks)


def check_tree(t):
    roles = sorted({a["role"] for a in t.agents.values()})
    shape = []
    for lead in t.leads:
        by_role = {}
        for a in t.children[lead["id"]]:
            by_role.setdefault(a["role"], []).append(a["id"])
        parts = [f"{role}:[{listed(ids)}]" for role, ids in sorted(by_role.items())]
        if t.panes[lead["id"]]:
            parts.append(f"panes:{len(t.panes[lead['id']])}")
        parent = "none" if lead["parent"] is None else lead["parent"]
        shape.append(f"lead {lead['id']} mode {t.mode_of(lead)} (parent {parent}, subs {t.sub_count(lead)}) {' '.join(parts) or 'no subs'}")
    modes = sorted(t.mode_of(lead) for lead in t.leads)
    good = (
        len(t.masters) >= 1
        and len(t.leads) == 2
        and all(t.sub_count(lead) == 2 for lead in t.leads)
        and "teammate" in roles
        and modes == ["app_spawned", "native"]
    )
    evidence = (
        f"team_agents rows={len(t.agents)} masters=[{listed(sorted(t.master_ids))}] leads={len(t.leads)} "
        f"(expect 2: one native, one app_spawned, 2 subs each); {'; '.join(shape) or 'no leads'}; roles={','.join(roles) or 'none'}"
    )
    if "teammate" not in roles:
        evidence += " (no role teammate)"
    return verdict("PASS" if good else "FAIL", evidence)


def check_clones(t):
    if not t.leads:
        return verdict("FAIL", "no leads")
    seen = {}
    for a in t.agents.values():
        if a["wt"]:
            seen.setdefault(os.path.realpath(a["wt"]), []).append(a["id"])
    shared = [f"{path} by {listed(ids)}" for path, ids in seen.items() if len(ids) > 1]
    place = os.path.join(t.repo, STATE, "worktrees") if t.repo else None
    lines, bad = [], []
    for lead in t.leads:
        lead_wt = lead["wt"]
        lead_branch = t.branch_of(lead_wt)
        lead_common = common_dir(lead_wt)
        if not lead_wt or not lead_branch:
            bad.append(f"lead {lead['id']} worktree={lead_wt} branch={lead_branch}")
            continue
        for child in t.children[lead["id"]]:
            wt = child["wt"]
            branch = t.branch_of(wt)
            native = child["role"] == "teammate"
            if not wt:
                bad.append(f"agent {child['id']} has no worktree")
                continue
            suffix = branch[len(lead_branch) + 2:] if branch and branch.startswith(lead_branch + "--") else None
            named = suffix is not None and bool(SLUG.match(suffix))
            dot_git = Path(wt) / ".git"
            clone = dot_git.is_dir() and not dot_git.is_symlink()
            if native:
                placed = (
                    os.path.realpath(os.path.dirname(wt)) == os.path.realpath(os.path.join(lead_wt, ".worktrees"))
                    and common_dir(wt) == lead_common and lead_common is not None
                )
                kind = "native"
            else:
                own = common_dir(wt)
                placed = not under(wt, lead_wt) and own is not None and own != lead_common and clone and place is not None and under(wt, place)
                kind = "app-spawned " + ("own clone" if clone else ".git is a gitdir file" if dot_git.is_file() else ".git missing")
            line = f"agent {child['id']} {kind} wt={wt} branch={branch}"
            lines.append(line)
            if not (named and placed):
                bad.append(line + (" (branch not <task>--<name>)" if not named else "") + (" (wrong place)" if not placed else ""))
        native_rows = sum(1 for a in t.children[lead["id"]] if a["role"] == "teammate")
        if len(t.panes[lead["id"]]) > native_rows:
            listing = git(lead_wt, "worktree", "list", "--porcelain") or ""
            entries = []
            path = None
            for row in listing.splitlines():
                if row.startswith("worktree "):
                    path = row[len("worktree "):]
                elif row.startswith("branch ") and path and under(path, os.path.join(lead_wt, ".worktrees")):
                    entries.append((path, row[len("branch refs/heads/"):]))
            matching = [e for e in entries if e[1].startswith(lead_branch + "--") and SLUG.match(e[1][len(lead_branch) + 2:])]
            lines.append(f"lead {lead['id']} native worktrees without agent rows: {listed(f'{p} {b}' for p, b in matching) or 'none'}")
            if len(matching) < len(t.panes[lead["id"]]) - native_rows:
                bad.append(f"lead {lead['id']} has {len(t.panes[lead['id']])} teammate panes but {len(matching)} matching .worktrees entries")
    if shared:
        bad.append("shared worktrees: " + listed(shared))
    return verdict("PASS" if lines and not bad else "FAIL", listed(bad if bad else lines) or "no sub-agents")


def check_native(t):
    natives = [lead for lead in t.leads if t.mode_of(lead) == "native"]
    if not natives:
        return verdict("FAIL", "no lead with mode native")
    notes, bad = [], []
    for lead in natives:
        s = t.transcripts[lead["id"]]
        mates = [a for a in t.children[lead["id"]] if a["role"] == "teammate"]
        scans = [s] + [t.transcripts[a["id"]] for a in mates] + [p for _, owner, p in t.pane_scans if owner["id"] == lead["id"]]
        unknown = sum(x["codes"][CALLER_UNKNOWN] for x in scans)
        work_ok = s["work_ok"]["start_work"] + s["work_ok"]["inspect_work"]
        work_failed = s["work_failed"]["start_work"] + s["work_failed"]["inspect_work"]
        recorded = lead["id"] in t.pane_records or bool(lead["pane"])
        kept = recorded or bool(t.panes[lead["id"]])
        teammates = max(len(mates), len(t.panes[lead["id"]]))
        where = s["first"].get(CALLER_UNKNOWN)
        if where:
            t.cite(s, where, f"lead {lead['id']} first {CALLER_UNKNOWN}")
        note = (
            f"lead {lead['id']} kept session={kept} (pane record={recorded}, "
            f"transcript sessions={len(s['launches'])}, turns={lead['turns']}); start/inspect_work ok={work_ok} failed={work_failed}; "
            f"{CALLER_UNKNOWN}={unknown}{' first ' + where if where else ''}; teammates rows={len(mates)} "
            f"panes={listed(t.panes[lead['id']]) or 'none'}"
        )
        (notes if kept and unknown == 0 and work_ok >= 1 and teammates >= 2 else bad).append(note)
    return verdict("FAIL" if bad else "PASS", listed(bad + notes))


def greet_hello(t):
    places = {}
    for path in [t.repo] + [a["wt"] for a in t.agents.values() if a["wt"]]:
        common = common_dir(path) if path else None
        if common and common not in places:
            places[common] = path
    for path in places.values():
        refs = [r for r in (git(path, "for-each-ref", "--format=%(refname)", "refs/heads") or "").splitlines() if r][:REFS_MAX]
        for ref in refs:
            text = git(path, "cat-file", "blob", f"{ref}:greet.py")
            if text and t.answer in text:
                return f"{ref} in {path}"
    return None


def check_question(t):
    rows = t.q(
        "SELECT id,q_severity,receipt,from_kind,from_agent FROM coord_messages "
        "WHERE project=? AND kind='question' AND q_for='me' AND to_kind='owner' ORDER BY id",
        t.pid,
    )
    good, seen = [], []
    for mid, severity, receipt, from_kind, from_agent in rows:
        agent_replies = t.q("SELECT COUNT(*) FROM coord_messages WHERE reply_to=? AND from_kind='agent'", mid)[0][0]
        owner = [body for (body,) in t.q("SELECT body FROM coord_messages WHERE reply_to=? AND from_kind='owner'", mid)]
        said = any(t.answer in body for body in owner)
        note = (
            f"id {mid} from {from_kind} {from_agent} severity={severity} receipt={receipt} agent_replies={agent_replies} "
            f"owner_replies={len(owner)} owner_said_{t.answer}={said}"
        )
        seen.append(note)
        if from_kind == "agent" and agent_replies == 0 and severity in SEVERITIES and said:
            good.append(note)
    written = greet_hello(t)
    evidence = f"For: Me questions to owner={len(rows)}; " + (listed(good if good else seen) or "none") + f"; greet.py with {t.answer}: {written or 'none'}"
    return verdict("PASS" if good and written else "FAIL", evidence)


def check_wake(t):
    if not t.leads:
        return verdict("FAIL", "no leads")
    notes, missing = [], []
    for lead in t.leads:
        total = t.q("SELECT COUNT(*) FROM coord_messages WHERE project=? AND to_kind='agent' AND to_agent=?", t.pid, lead["id"])[0][0]
        read = t.q(
            "SELECT id,receipt,receipt_at FROM coord_messages WHERE project=? AND to_kind='agent' AND to_agent=? "
            "AND receipt IN ('read','replied') ORDER BY id LIMIT 1",
            t.pid, lead["id"],
        )
        if read:
            notes.append(f"lead {lead['id']} message {read[0][0]} {read[0][1]} at {read[0][2]} (of {total})")
        else:
            missing.append(f"lead {lead['id']} none read (of {total})")
        kids = [a["id"] for a in t.children[lead["id"]]]
        if t.mode_of(lead) == "native" or not kids:
            continue
        marks = ",".join("?" * len(kids))
        kinds = ",".join("?" * len(FINISH_EVENTS))
        finished = t.q(
            f"SELECT MAX(created_ms) FROM team_events WHERE project=? AND agent IN ({marks}) AND kind IN ({kinds})",
            t.pid, *kids, *FINISH_EVENTS,
        )[0][0]
        if finished is None:
            missing.append(f"lead {lead['id']} workers never finished")
            continue
        after = [p for p in t.transcripts[lead["id"]]["prompts"] if p is not None and p > finished]
        beat = "after" if (lead["heartbeat"] or 0) > finished else "before"
        note = f"lead {lead['id']} last worker finish {finished}: prompts after={len(after)} turns={lead['turns']} heartbeat {beat}"
        (notes if after else missing).append(note + ("" if after else " (not woken)"))
    return verdict("PASS" if not missing else "FAIL", listed(missing + notes))


def glob_match(glob, path):
    tokens, at = [], 0
    while at < len(glob):
        if glob.startswith("**/", at):
            tokens.append(("folders", None))
            at += 3
        elif glob.startswith("**", at):
            tokens.append(("any", None))
            at += 2
        elif glob[at] == "*":
            tokens.append(("star", None))
            at += 1
        elif glob[at] == "?":
            tokens.append(("one", None))
            at += 1
        else:
            tokens.append(("byte", glob[at]))
            at += 1
    row = [False] * (len(path) + 1)
    row[0] = True
    for kind, byte in tokens:
        after = [False] * (len(path) + 1)
        reached = False
        for end in range(len(path) + 1):
            if kind == "byte":
                after[end] = end > 0 and row[end - 1] and path[end - 1] == byte
            elif kind == "one":
                after[end] = end > 0 and row[end - 1] and path[end - 1] != "/"
            elif kind == "star":
                after[end] = row[end] or (end > 0 and after[end - 1] and path[end - 1] != "/")
            elif kind == "any":
                after[end] = row[end] or (end > 0 and after[end - 1])
            else:
                after[end] = row[end] or (end > 0 and reached and path[end - 1] == "/")
            reached = reached or row[end]
        row = after
    return row[len(path)]


def label_tokens(text):
    return sorted({word for word in re.split(r"[^a-z0-9]", text.lower()) if word})


def scope(pattern):
    if pattern.startswith("module:"):
        return ("label", (True, label_tokens(pattern[len("module:"):])))
    if pattern.startswith("domain:"):
        return ("label", (False, label_tokens(pattern[len("domain:"):])))
    if pattern == "**":
        return ("folder", "")
    if "*" in pattern or "?" in pattern:
        return ("glob", pattern)
    if pattern.endswith("/"):
        return ("folder", pattern)
    return ("file", pattern)


def covers(kind, value, file):
    if kind == "folder":
        return file.startswith(value)
    if kind == "glob":
        return glob_match(value, file)
    return False


def parent_folder(file):
    return file[:file.rfind("/")] if "/" in file else ""


def fixed(glob):
    cut = min([i for i in (glob.find("*"), glob.find("?")) if i >= 0], default=len(glob))
    literal = glob[:cut]
    return literal[:literal.rfind("/") + 1] if "/" in literal else ""


def reason(mine, theirs):
    a, b = scope(mine), scope(theirs)
    if a[0] == "file" and b[0] == "file":
        if a[1] == b[1]:
            return f"same file {a[1]}"
        folder = parent_folder(a[1])
        return f"same folder {folder}" if folder and folder == parent_folder(b[1]) else None
    if a[0] == "file" and b[0] in ("folder", "glob"):
        return f"{a[1]} inside {b[1] or 'the whole repository'}" if covers(b[0], b[1], a[1]) else None
    if b[0] == "file" and a[0] in ("folder", "glob"):
        return f"{b[1]} inside {a[1] or 'the whole repository'}" if covers(a[0], a[1], b[1]) else None
    if a[0] == "folder" and b[0] == "folder":
        return f"{a[1] or '**'} and {b[1] or '**'} nest" if a[1].startswith(b[1]) or b[1].startswith(a[1]) else None
    if a[0] in ("folder", "glob") and b[0] in ("folder", "glob"):
        left = a[1] if a[0] == "folder" else fixed(a[1])
        right = b[1] if b[0] == "folder" else fixed(b[1])
        return f"{a[1] or '**'} and {b[1] or '**'} may overlap" if left.startswith(right) or right.startswith(left) else None
    if a[0] == "label" and b[0] == "label" and a[1][0] == b[1][0]:
        shared = [word for word in a[1][1] if word in b[1][1]]
        return f"shared {'module' if a[1][0] else 'domain'} words {','.join(shared)}" if shared else None
    return None


def check_overlap(t):
    claims = t.q("SELECT c.id,c.agent,c.pattern,c.created,c.released FROM coord_claims c WHERE c.project=? ORDER BY c.id", t.pid)

    def related(x, y):
        return t.agents.get(x, {}).get("parent") == y or t.agents.get(y, {}).get("parent") == x

    found = []
    for index, mine in enumerate(claims):
        for theirs in claims[index + 1:]:
            if mine[1] == theirs[1] or related(mine[1], theirs[1]):
                continue
            together = (theirs[4] is None or mine[3] <= theirs[4]) and (mine[4] is None or theirs[3] <= mine[4])
            if not together:
                continue
            why = reason(mine[2], theirs[2])
            if why:
                found.append(f"claim {mine[0]} (agent {mine[1]}) x claim {theirs[0]} (agent {theirs[1]}): {why}")
    warned = {a: s["overlap"] for a, s in t.transcripts.items() if s["overlap"]}
    where = [f"agent {a} {t.transcripts[a]['first']['overlap']}" for a in warned]
    for a in warned:
        t.cite(t.transcripts[a], t.transcripts[a]["first"]["overlap"], f"agent {a} first work-tool result naming an overlap")
    told = t.q(
        "SELECT to_agent, COUNT(*) FROM coord_messages WHERE project=? AND to_kind='agent' "
        "AND lower(body) LIKE '%overlap%' GROUP BY to_agent ORDER BY to_agent",
        t.pid,
    )
    evidence = (
        f"claims={len(claims)} overlapping pairs={len(found)}; {listed(found) or 'none'}; "
        f"work-tool results naming an overlap: {sum(warned.values())} ({listed(where) or 'none'}); "
        f"messages naming an overlap to agents: {listed(f'{t.label(t.agents[a]) if a in t.agents else a} x{n}' for a, n in told) or 'none'}"
    )
    return verdict("PASS" if found and (warned or told) else "FAIL", evidence)


def check_merges(t):
    lead_notes, lead_bad = [], []
    for lead in t.leads:
        lead_wt, lead_branch = lead["wt"], t.branch_of(lead["wt"])
        for child in t.children[lead["id"]]:
            wt, branch = child["wt"], t.branch_of(child["wt"])
            head = git(wt, "rev-parse", "--verify", "-q", f"refs/heads/{branch}^{{commit}}") if branch else None
            head = head or git(wt, "rev-parse", "--verify", "-q", "HEAD^{commit}")
            if not (lead_wt and lead_branch and head):
                lead_bad.append(f"agent {child['id']} head={head} lead branch={lead_branch}")
                continue
            created = (child["created"] or 0) // 1000
            made = git(wt, "rev-list", "--no-merges", f"--since=@{created}", head) if created else None
            made = [c for c in (made or "").splitlines() if c]
            if not made:
                lead_bad.append(f"agent {child['id']} {branch} has no commit of its own")
                continue
            known = git_ok(lead_wt, "cat-file", "-e", f"{head}^{{commit}}")
            if known and git_ok(lead_wt, "merge-base", "--is-ancestor", head, f"refs/heads/{lead_branch}"):
                lead_notes.append(f"agent {child['id']} {branch} {head[:12]} in {lead_branch}")
                continue
            files = [f for f in (git(wt, "diff", "--no-ext-diff", "--no-textconv", "--name-only", f"{made[-1]}^", head) or "").splitlines() if f]
            same = files and all(
                git(wt, "rev-parse", "-q", "--verify", f"{head}:{f}") == git(lead_wt, "rev-parse", "-q", "--verify", f"refs/heads/{lead_branch}:{f}")
                for f in files
            )
            if same:
                lead_notes.append(f"agent {child['id']} {branch} content of {len(files)} files in {lead_branch} (squash)")
            else:
                lead_bad.append(f"agent {child['id']} {branch} {head[:12]} not in {lead_branch}")
    chosen = land_on(t)
    if not chosen:
        try:
            chosen = t.q("SELECT branch FROM team_master_branch WHERE project=?", t.pid)
        except Missing:
            chosen = []
    owner_branch = chosen[0][0] if chosen and chosen[0][0] else git(t.repo, "symbolic-ref", "--short", "-q", "HEAD")
    sets = t.q(
        "SELECT id,agent_kind,agent_id,landed,repository FROM landing_change_sets "
        "WHERE project=? AND state='accepted' AND landed IS NOT NULL ORDER BY created_ms",
        t.pid,
    )
    master_notes, master_bad, landed_leads = [], [], set()
    for set_id, kind, agent_id, landed, repository in sets:
        if kind != "sub" or not str(agent_id).isdigit() or int(agent_id) not in t.lead_ids:
            continue
        branch = chosen[0][0] if chosen and chosen[0][0] else git(repository, "symbolic-ref", "--short", "-q", "HEAD")
        if not branch:
            master_bad.append(f"set {set_id} lead {agent_id}: no Master's branch in {repository}")
            continue
        merged = git_ok(repository, "merge-base", "--is-ancestor", landed, f"refs/heads/{branch}")
        remote = [r for r in (git(repository, "for-each-ref", "--format=%(refname)", "--contains", landed, "refs/remotes") or "").splitlines() if r]
        remotes = len([r for r in (git(repository, "remote") or "").splitlines() if r])
        note = f"set {set_id} lead {agent_id} landed {landed[:12]} on {branch} merged={merged} remotes={remotes} remote refs containing it={len(remote)}"
        if merged and not remote:
            master_notes.append(note)
            landed_leads.add(int(agent_id))
        else:
            master_bad.append(note + (f" ({listed(remote)})" if remote else ""))
    targets = [b for b in [owner_branch] + [t.branch_of(m["wt"]) for m in t.masters] if b]
    for lead in t.leads:
        if lead["id"] in landed_leads:
            continue
        lead_branch = t.branch_of(lead["wt"])
        tip = git(lead["wt"], "rev-parse", "--verify", "-q", f"refs/heads/{lead_branch}^{{commit}}") if lead_branch else None
        created = (lead["created"] or 0) // 1000
        own = [c for c in (git(lead["wt"], "rev-list", f"--since=@{created}", tip) or "").splitlines() if c] if tip and created else []
        if not own:
            master_bad.append(f"lead {lead['id']} {lead_branch} has no commit or merge of its own")
            continue
        hit = next((b for b in targets if git_ok(t.repo, "merge-base", "--is-ancestor", tip, f"refs/heads/{b}")), None)
        remote = [r for r in (git(t.repo, "for-each-ref", "--format=%(refname)", "--contains", tip, "refs/remotes") or "").splitlines() if r]
        if hit and not remote:
            merges = git(t.repo, "rev-list", "--merges", "--count", f"--since=@{created}", f"refs/heads/{hit}")
            master_notes.append(f"lead {lead['id']} {lead_branch} {tip[:12]} in {hit} (git log: {merges} merge commits since the lead started)")
        else:
            master_bad.append(
                f"lead {lead['id']} {lead_branch} {tip[:12]} not in {listed(targets) or 'any master branch'}"
                + (f" (pushed: {listed(remote)})" if remote else "")
            )
    good = bool(lead_notes) and bool(master_notes) and not lead_bad and not master_bad
    evidence = (
        f"lead merges ok={len(lead_notes)} bad={len(lead_bad)}: {listed(lead_bad + lead_notes) or 'none'}; "
        f"master merges ok={len(master_notes)} bad={len(master_bad)}: {listed(master_bad + master_notes) or 'none'}"
    )
    return verdict("PASS" if good else "FAIL", evidence)


def check_reply(t):
    replies = t.q(
        "SELECT COUNT(*) FROM coord_messages WHERE project=? AND kind='reply' AND reply_to IS NOT NULL AND from_kind='agent'",
        t.pid,
    )[0][0]
    refused = {a["id"]: t.transcripts[a["id"]]["codes"][REPLY_INVALID] for a in t.tree if t.transcripts[a["id"]]["codes"][REPLY_INVALID]}
    where = [f"agent {a} x{n} first {t.transcripts[a]['first'][REPLY_INVALID]}" for a, n in refused.items()]
    for a in refused:
        t.cite(t.transcripts[a], t.transcripts[a]["first"][REPLY_INVALID], f"agent {a} first {REPLY_INVALID}")
    evidence = f"agent replies with reply_to stored={replies}; {REPLY_INVALID} results={sum(refused.values())} ({listed(where) or 'none'})"
    result = "FAIL" if refused else "PASS" if replies else "UNKNOWN"
    return verdict(result, evidence + ("" if replies or refused else "; no agent sent a reply"))


def check_git(t):
    notes, bad = [], []
    for a in t.tree:
        s = t.transcripts[a["id"]]
        if not s["git"] and not s["xcrun"]:
            continue
        note = f"{t.label(a)} git calls={s['git']} ok={s['git_ok']} xcrun errors={s['xcrun']}" + (f" first {s['first']['xcrun']}" if s["xcrun"] else "")
        if s["xcrun"]:
            t.cite(s, s["first"]["xcrun"], f"agent {a['id']} first xcrun error")
        (bad if s["xcrun"] else notes).append(note)
    ok = sum(t.transcripts[a["id"]]["git_ok"] for a in t.tree)
    result = "FAIL" if bad else "PASS" if ok else "UNKNOWN"
    return verdict(result, listed(bad + notes) or "no agent git call in any transcript")


def check_stopped(t):
    try:
        paused = t.q("SELECT since_ms FROM team_paused_projects WHERE project=?", t.pid)
    except Missing:
        paused = []
    try:
        stops = t.q("SELECT agent,stopped_ms FROM team_owner_stops WHERE project=? ORDER BY agent", t.pid)
    except Missing:
        stops = []
    try:
        open_panes = t.q("SELECT agent,pane FROM team_teammates WHERE project=? AND ended_ms IS NULL AND stopped_ms IS NULL", t.pid)
    except Missing:
        open_panes = []
    live_rows = [f"{a['id']}:{a['status']}" for a in t.agents.values() if a["status"] in LIVE]
    if t.procs is None:
        return verdict("UNKNOWN", "ps unavailable")
    alive = sorted({(p[0], p[1], a["id"]) for a in t.agents.values() for p in t.procs if t.owned_by(a, p)})
    good = bool(paused) and not live_rows and not open_panes and not alive
    evidence = (
        f"team_paused_projects={'since ' + str(paused[0][0]) if paused else 'none'}; "
        f"live agent rows={listed(live_rows) or 'none'}; open teammate panes={len(open_panes)}; "
        f"team_owner_stops={len(stops)} ({listed(f'{s[0]}@{s[1]}' for s in stops) or 'none'}); "
        f"statuses={listed(sorted({a['status'] for a in t.agents.values()})) or 'none'}; "
        f"live processes (pid/pgid/agent)={listed(f'{p}/{g}/{i}' for p, g, i in alive) or 'none'}"
    )
    return verdict("PASS" if good else "FAIL", evidence)


def instruction_mode(t):
    present = t.columns("agent_instruction_settings")
    column = "mode" if "mode" in present else "files" if "files" in present else None
    if column is None:
        return "no instruction settings table"
    rows = t.q(f"SELECT {column} FROM agent_instruction_settings WHERE project=?", t.pid)
    fallback = "default (both)" if column == "mode" else "default"
    return f"setting {column}={rows[0][0] if rows else fallback}"


def delivery_entries(t):
    return [(t.label(a), a, t.transcripts[a["id"]]) for a in t.tree] + [(name, None, found) for name, _, found in t.pane_scans]


def check_delivery(t):
    entries = delivery_entries(t)
    if not entries:
        return verdict("FAIL", "no agents")
    lines, bad = [], []
    for name, agent, s in entries:
        harness = agent["harness"] if agent else "claude-code"
        if not harness.startswith("claude"):
            path = t.home(agent["id"]) / "AGENTS.md"
            text = path.read_text(encoding="utf-8", errors="replace") if path.is_file() else None
            if text is None:
                bad.append(f"{name} harness {harness}: no transcript parser and no home AGENTS.md")
                continue
            app_at, project_at = text.find(APP_MARK), text.find(t.project_mark)
            line = f"{name} harness {harness} home AGENTS.md app@{app_at} project@{project_at}"
            lines.append(line)
            if not (0 <= app_at < project_at):
                bad.append(line)
            continue
        launches = s["launches"]
        if not launches:
            line = f"{name} no transcript"
            lines.append(line)
            bad.append(line)
            continue
        good = [l for l in launches if l["app"] is not None and 0 <= l["app"] < l["project"]]
        first = launches[0]
        t.cite(s, first["spot"], f"{name} first launch context")
        line = (
            f"{name} launches ok={len(good)}/{len(launches)} first {first['spot']} "
            f"app@{first['app']} project@{first.get('project')} project marks={first.get('marks')} bytes={first.get('bytes')}"
        )
        if s["repo_reads"]:
            line += f"; repo instruction files loaded: {listed(s['repo_reads'])}"
            for read in s["repo_reads"][:LISTED_MAX]:
                t.cite(s, read.rsplit(" at ", 1)[-1], f"{name} repo instruction file loaded")
        lines.append(line)
        if len(good) < len(launches) or s["repo_reads"]:
            bad.append(line)
    return verdict(
        "FAIL" if bad else "PASS",
        f"{instruction_mode(t)}; agents={len(entries)} bad={len(bad)}; '{APP_MARK}' must precede '{t.project_mark}' "
        "in every launch context and no repo CLAUDE.md, AGENTS.md or CLAUDE.local.md may load",
        lines,
    )


def enabled_skills(t):
    off = {row[0] for row in t.q("SELECT item FROM plugin_switches WHERE project=?", t.pid)}
    records = []
    for (body,) in t.q("SELECT body FROM plugin_items WHERE kind='skill' ORDER BY scope, name"):
        try:
            record = json.loads(body)
        except ValueError:
            continue
        if not isinstance(record, dict) or record.get("enabled") is not True or not isinstance(record.get("name"), str):
            continue
        place = record.get("scope") if isinstance(record.get("scope"), dict) else {}
        local = place.get("kind") == "project"
        if (local and place.get("project_id") != t.pid) or (not local and record.get("id") in off):
            continue
        records.append((record["name"], local, record.get("folder")))
    names = []
    for name, local, folder in records:
        shadowed = not local and any(other_local and other == name for other, other_local, _ in records)
        described = isinstance(folder, str) and (t.root / folder / "SKILL.md").is_file()
        if not shadowed and described and name not in names:
            names.append(name)
    return sorted(names)


def short(tool):
    if tool.startswith("mcp__"):
        rest = tool[len("mcp__"):]
        server, _, last = rest.rpartition("__")
        if "graphify" in server:
            return "graphify"
        return last or rest
    return tool


def skill_matches(call, skill):
    return call == skill or call.rsplit(":", 1)[-1] == skill


def check_adoption(t):
    skills = enabled_skills(t)
    if not t.tree:
        return verdict("FAIL", "no agents")
    lines, bad = [], []
    called = 0
    stored_any = False
    for a in t.tree:
        rows = t.q("SELECT tool,count FROM coord_tool_use WHERE agent=?", a["id"])
        used = [(tool[len(SKILL_PREFIX):], max(0, count)) for tool, count in rows if tool.startswith(SKILL_PREFIX)]
        stored_any = stored_any or bool(used)
        stored = Counter()
        for call, count in used:
            stored[call] += count
        per_skill = [sum(count for call, count in used if skill_matches(call, skill)) for skill in skills]
        others = [f"{call}={count}" for call, count in sorted(stored.items()) if count and not any(skill_matches(call, s) for s in skills)]
        names = {short(tool): count for tool, count in rows}
        ctx = {name: count for name, count in names.items() if name.startswith("ctx_")}
        s = t.transcripts[a["id"]]
        called += sum(s["skills"].values())
        unrecorded = [f"{skill}={n}" for skill, n in sorted(s["skills"].items()) if n and not stored[skill]]
        if unrecorded:
            bad.append(f"{t.label(a)} Skill calls with no stored skill count: {listed(unrecorded)}")
        if s["calls"] and not rows:
            bad.append(f"{t.label(a)} {s['calls']} transcript calls but no coord_tool_use rows")
        lines.append(
            f"{t.label(a)} skills: {' '.join(f'{k}={n}' for k, n in zip(skills, per_skill)) or 'none enabled'}"
            f"; not enabled: {listed(others) or 'none'}"
            f"; ctx_*={sum(ctx.values())} ({listed(f'{k}={v}' for k, v in sorted(ctx.items())) or 'none'})"
            f"; graphify={names.get('graphify', 0)}"
            f"; work: {' '.join(f'{w}={names.get(w, 0)}' for w in WORK_TOOLS)}"
            f"; transcript Skill={s['Skill']} by name {listed(f'{k}={v}' for k, v in sorted(s['skills'].items())) or 'none'} ctx={s['ctx']} graphify={s['graphify']}"
        )
    if not skills:
        result, why = "FAIL", "no enabled skill found for the project"
    elif bad:
        result, why = "FAIL", listed(bad)
    elif not called and not stored_any:
        result, why = "UNKNOWN", "no skill call in any transcript or store, so per-skill counting is not exercised"
    else:
        result, why = "PASS", "every transcript skill call has a stored per-skill count"
    unrowed = [name for name, _, _ in t.pane_scans]
    return verdict(
        result,
        f"enabled skills={len(skills)}; agents={len(t.tree)}; {why}"
        + (f"; pane transcripts without rows (not counted separately): {listed(unrowed)}" if unrowed else ""),
        lines,
    )


def check_tui(t):
    return verdict("UNKNOWN", "not in the engine store: read the recorded frames for a clean Claude input border and Ctrl-T opening Team")


def check_import(t):
    if not t.repo:
        return verdict("FAIL", "no project folder")
    folder = Path(t.repo)
    instructions = folder / STATE / "instructions.md"
    moved = folder / STATE / "imported" / "CLAUDE.md"
    gone = not os.path.lexists(folder / "CLAUDE.md")
    kept = moved.is_file() and not moved.is_symlink()
    original = read_text(moved) if kept else None
    text = read_text(instructions) if instructions.is_file() and not instructions.is_symlink() else None
    headings = [line for line in (text or "").splitlines() if line.startswith(IMPORTED_HEADING)]
    named = any("CLAUDE.md" in line[len(IMPORTED_HEADING):].split(", ") for line in headings)
    holds = bool(original and original.strip() and text and original.strip() in text)
    tracked = git_ok(t.repo, "ls-files", "--error-unmatch", "--", "CLAUDE.md")
    try:
        rows = t.q("SELECT moved FROM instruction_imports WHERE project=?", t.pid)
        listing = json.loads(rows[0][0]) if rows else []
        recorded = listed(f"{item.get('path')} tracked={item.get('tracked')}" for item in listing if isinstance(item, dict)) or "none"
    except (Missing, ValueError):
        recorded = "no instruction_imports table"
    evidence = (
        f"repo CLAUDE.md {'gone' if gone else 'still present'}; .crazyagent/imported/CLAUDE.md {'present' if kept else 'missing'}; "
        f"instructions.md {'missing' if text is None else str(len(text.encode())) + ' B'} headings={listed(headings) or 'none'} "
        f"holds the CLAUDE.md text={holds} '{t.project_mark}' x{(text or '').count(t.project_mark)}; "
        f"git index still has CLAUDE.md={tracked} (shows as deleted); onboarding moved list={recorded}"
    )
    return verdict("PASS" if gone and kept and named and holds else "FAIL", evidence)


def audit_transcripts(t):
    if not t.tree:
        return verdict("UNKNOWN", "no agents")
    notes, missing, empty = [], [], []
    for a in t.tree:
        s = t.transcripts[a["id"]]
        if not s["files"]:
            empty.append(str(a["id"]))
            continue
        first = " ".join(f"{k}@{v}" for k, v in sorted(s["first"].items()) if k in ("ctx", "graphify", "Skill"))
        for k in ("ctx", "graphify", "Skill"):
            if k in s["first"]:
                t.cite(s, s["first"][k], f"agent {a['id']} first {k} call")
        note = f"{a['id']}({a['role']}) files={s['files']} calls={s['calls']} ctx={s['ctx']} graphify={s['graphify']} Skill={s['Skill']} {first}".rstrip()
        (missing if 0 in (s["ctx"], s["graphify"], s["Skill"]) else notes).append(note)
    native_children = [c["id"] for l in t.leads if t.is_native(l) for c in t.children[l["id"]] if c["role"] == "teammate"]
    extra = f"; native teammates {listed(native_children)} share their lead's home" if native_children else ""
    result = "FAIL" if missing else "UNKNOWN" if empty and not notes else "PASS" if not empty else "FAIL"
    no_files = f"no transcripts for {listed(empty)}; " if empty else ""
    return verdict(result, no_files + (listed(missing + notes) or "none") + extra)


def denied(command):
    found = set()
    for match in re.finditer(r"--disallowed-?[Tt]ools(?:=|\s+)((?:[^\s-][^\s]*\s*)+)", command):
        found.update(word for word in re.split(r"[\s,]+", match.group(1)) if word)
    return found


def audit_argv(t):
    if t.procs is None:
        return verdict("UNKNOWN", "ps unavailable")
    notes, bad = [], []
    for lead in t.leads:
        mine = [p for p in t.procs if t.owned_by(lead, p) and re.search(r"(^|/|\s)claude(\s|$)", p[2])]
        native = t.is_native(lead)
        for p in mine:
            deny = denied(p[2])
            note = (
                f"lead {lead['id']} {'native' if native else 'app-spawned'} pid {p[0]}: "
                f"denies Agent={'Agent' in deny} TeamCreate={'TeamCreate' in deny} "
                f"teammate-mode={'--teammate-mode' in p[2]}"
            )
            (bad if native and ({"Agent", "TeamCreate"} & deny) else notes).append(note)
    if not notes and not bad:
        return verdict("UNKNOWN", "no live lead harness process; audit before Stop all to read argv")
    return verdict("FAIL" if bad else "PASS", listed(bad + notes))


def audit_instructions(t):
    notes, bad = [], []
    app = t.root / "instructions.md"
    app_size = app.stat().st_size if app.is_file() else None
    for a in t.tree:
        for name in ("CLAUDE.md", "AGENTS.md"):
            path = t.home(a["id"]) / name
            if not path.is_file():
                continue
            try:
                lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                continue
            offloaded = [n for n, line in enumerate(lines, 1) if OFFLOAD in line]
            note = f"{a['id']} {name} {path.stat().st_size} B " + (f"offloaded {len(offloaded)} ({listed(f'{name}:{n}' for n in offloaded)})" if offloaded else "inline")
            (bad if offloaded else notes).append(note)
    if not notes and not bad:
        return verdict("UNKNOWN", f"no CLAUDE.md or AGENTS.md in any agent home; app instructions.md={app_size} B")
    return verdict("FAIL" if bad else "PASS", f"app instructions.md={app_size} B; " + listed(bad + notes))


def audit_tool_use(t):
    if not t.tree:
        return verdict("FAIL", "no agents")
    notes, missing = [], []
    for a in t.tree:
        counts = {"work": 0, "ctx": 0, "graphify": 0}
        for tool, count in t.q("SELECT tool,count FROM coord_tool_use WHERE agent=?", a["id"]):
            name = short(tool)
            if name in WORK_TOOLS:
                counts["work"] += count
            elif name.startswith("ctx_"):
                counts["ctx"] += count
            elif name == "graphify":
                counts["graphify"] += count
        note = f"{a['id']}({a['role']}) work={counts['work']} ctx={counts['ctx']} graphify={counts['graphify']}"
        (missing if 0 in counts.values() else notes).append(note)
    unrowed = sum(max(0, len(t.panes[l["id"]]) - sum(1 for c in t.children[l["id"]] if c["role"] == "teammate")) for l in t.leads)
    extra = f"; {unrowed} native teammates have no agent row, so their counts are not separate" if unrowed else ""
    return verdict("PASS" if not missing else "FAIL", listed(missing + notes) + extra)


def land_on(t):
    if not t.columns("team_land_on"):
        return []
    return t.q("SELECT branch FROM team_land_on WHERE project=?", t.pid)


def check_land_on(t):
    if not t.columns("team_land_on"):
        return verdict("UNKNOWN", "not in this build: no team_land_on table")
    rows = land_on(t)
    if not rows:
        return verdict("UNKNOWN", "not exercised: the project has no Land on choice, so merges land on the Master's branch")
    branch = rows[0][0]
    head = git(t.repo, "symbolic-ref", "--short", "-q", "HEAD")
    sets = t.q(
        "SELECT id,agent_id,landed,repository FROM landing_change_sets "
        "WHERE project=? AND agent_kind='sub' AND state='accepted' AND landed IS NOT NULL ORDER BY created_ms",
        t.pid,
    )
    notes, bad = [], []
    for set_id, agent_id, landed, repository in sets:
        if not str(agent_id).isdigit() or int(agent_id) not in t.lead_ids:
            continue
        on = git_ok(repository, "merge-base", "--is-ancestor", landed, f"refs/heads/{branch}")
        line = f"set {set_id} lead {agent_id} landed {landed[:12]} in {branch}={on}"
        (notes if on else bad).append(line)
    told = t.q("SELECT COUNT(*) FROM team_messages WHERE project=? AND author='engine' AND text LIKE ?", t.pid, f"%into {branch} as %")[0][0]
    detail = (
        f"Land on={branch}; owner checkout on {head or 'no branch'}; lead change sets landed in {branch}: {len(notes)}, elsewhere: {len(bad)}; "
        f"{listed(bad + notes) or 'none'}; master notes naming {branch}: {told}"
    )
    if bad:
        return verdict("FAIL", detail)
    return verdict("PASS" if notes else "UNKNOWN", detail if notes else detail + "; no lead change set was accepted")


def check_background(t):
    if not t.columns("team_background"):
        return verdict("UNKNOWN", "not in this build: no team_background table")
    rows = t.q("SELECT mode, grace_secs FROM team_background WHERE project=?", t.pid)
    mode, grace = rows[0] if rows else ("wait", None)
    ended = t.q(
        "SELECT id,agent,result,started_ms,finished_ms FROM team_tasks WHERE project=? AND result LIKE ? ORDER BY id",
        t.pid, f"%{BACKGROUND_STEM}%",
    )
    kinds, bad = Counter(), []
    for task, agent, result, started, finished in ended:
        kind = next((name for name, opening in BACKGROUND_ENDS if opening in result), "other")
        kinds[kind] += 1
        if kind in ("grace", "stop") and kind != mode:
            bad.append(f"task {task} agent {agent} ended by {kind} under mode {mode}")
        if kind == "grace" and grace is not None and (started is None or finished is None or finished - started < grace * 1000):
            bad.append(f"task {task} agent {agent} ended by grace after {None if started is None or finished is None else finished - started} ms of {grace * 1000}")
    entries = delivery_entries(t)
    told = sum(s["phrases"][BACKGROUND_TOLD] for _, _, s in entries)
    for name, _, s in entries:
        if BACKGROUND_TOLD in s["first"]:
            t.cite(s, s["first"][BACKGROUND_TOLD], f"{name} first told background commands stop")
    if mode != "stop" and told:
        bad.append(f"{told} prompts say background commands stop at turn end under mode {mode}")
    if mode == "stop" and not told and t.tree:
        bad.append("no agent prompt says background commands stop at turn end")
    detail = (
        f"mode={mode}" + (f" grace={grace}s" if grace is not None else "") + f"; told in prompts={told}; "
        f"tasks ended with a background command={len(ended)} ({listed(f'{k}={n}' for k, n in sorted(kinds.items())) or 'none'})"
        + (f"; {listed(bad)}" if bad else "")
    )
    if bad:
        return verdict("FAIL", detail)
    if mode in ("grace", "stop") and kinds[mode]:
        return verdict("PASS", detail)
    return verdict("UNKNOWN", detail + ("; mode wait: tasks wait for their background commands" if mode == "wait" else "; no task ended with a background command running"))


def check_hidden(t):
    if not t.columns("project_hidden_paths"):
        return verdict("UNKNOWN", "not in this build: no project_hidden_paths table (feat/agent-hidden-folders is not merged)")
    hidden = [row[0] for row in t.q("SELECT path FROM project_hidden_paths WHERE project=? ORDER BY position", t.pid)]
    if not hidden:
        return verdict("UNKNOWN", "not exercised: the project hides no folder")
    profiles = [(t.label(a), t.root / "team" / "profiles" / f"agent-{a['id']}.sb") for a in t.tree if a["role"] != "teammate"]
    profiles += [(f"{t.label(l)} teammates", t.root / "team" / "profiles" / f"agent-{l['id']}-teammates.sb") for l in t.leads if t.is_native(l)]
    notes, bad = [], []
    for name, path in profiles:
        text = read_text(path)
        if text is None:
            bad.append(f"{name} no profile {shown_path(t, path)}")
            continue
        at = text.find(HIDDEN_RULE)
        block = text[at:text.find(")\n", text.find("\n", at))] if at >= 0 else ""
        denied_folders = set(re.findall(r'\(subpath "([^"]+)"\)', block))
        missing = [folder for folder in hidden if folder not in denied_folders]
        line = f"{name} {shown_path(t, path)} denies {len(hidden) - len(missing)}/{len(hidden)}" + (f" missing {listed(missing)}" if missing else "")
        (bad if missing else notes).append(line)
    return verdict("FAIL" if bad else "PASS", f"hidden={listed(hidden)}; {listed(bad + notes) or 'no agent profile'}")


def given_record(t, kind, agent_id):
    home = t.home(agent_id)
    if not home.is_dir():
        return None
    name = hashlib.sha256(os.path.realpath(home).encode()).hexdigest()
    return read_text(t.root / kind / name)


def first_line(text):
    return next((line.strip() for line in (text or "").splitlines() if line.strip()), None)


def check_part_one(t):
    if not t.tree:
        return verdict("FAIL", "no agents")
    lines, bad = [], []
    for a in t.tree:
        if a["role"] == "teammate":
            continue
        if not a["harness"].startswith("claude"):
            text = read_text(t.home(a["id"]) / "AGENTS.md")
            line = f"{t.label(a)} home AGENTS.md first line={first_line(text)!r}"
            lines.append(line)
            if first_line(text) != APP_MARK:
                bad.append(line)
            continue
        recorded = given_record(t, "agent-instructions-given", a["id"])
        given = recorded if recorded is not None else read_text(t.home(a["id"]) / "crazyagent" / "instructions.md")
        launches = t.transcripts[a["id"]]["launches"]
        reached = [l for l in launches if l["app"] is not None and l["app"] >= 0]
        source = "given record" if recorded is not None else "home crazyagent/instructions.md" if given is not None else "nothing given"
        line = f"{t.label(a)} {source} first line={first_line(given)!r}; launches with the set={len(reached)}/{len(launches)}"
        lines.append(line)
        if first_line(given) != APP_MARK or not launches or len(reached) < len(launches):
            bad.append(line)
        if launches:
            t.cite(t.transcripts[a["id"]], launches[0]["spot"], f"{t.label(a)} first launch context")
    for lead in t.leads:
        if not t.is_native(lead):
            continue
        mates = [a for a in t.children[lead["id"]] if a["role"] == "teammate"]
        scans = [(t.label(a), t.transcripts[a["id"]]) for a in mates] + [(name, s) for name, owner, s in t.pane_scans if owner["id"] == lead["id"]]
        seen = [(name, s) for name, s in scans if s["launches"]]
        reached = [(name, s) for name, s in seen if all(l["app"] is not None and l["app"] >= 0 for l in s["launches"])]
        wanted = max(len(mates), len(t.panes[lead["id"]]))
        line = f"lead {lead['id']} teammates={wanted} transcripts={len(seen)} with the set={len(reached)}"
        lines.append(line)
        for name, s in seen[:LISTED_MAX]:
            t.cite(s, s["launches"][0]["spot"], f"{name} first launch context")
        if wanted == 0 or len(reached) < wanted or len(reached) < len(seen):
            bad.append(line)
    return verdict(
        "FAIL" if bad else "PASS",
        f"the given instruction set opens with '{APP_MARK}' for every agent and reaches every launch, teammates included; agents bad={len(bad)}",
        lines,
    )


def depth_of(t, agent):
    depth, at, seen = 1, agent, {agent["id"]}
    while at["parent"] is not None and at["parent"] in t.agents and at["parent"] not in seen:
        seen.add(at["parent"])
        at = t.agents[at["parent"]]
        depth += 1
    return depth


def coord_json(t):
    rows = t.q("SELECT json FROM coord_settings WHERE project=?", t.pid)
    try:
        value = json.loads(rows[0][0]) if rows else {}
    except ValueError:
        return {}
    return value if isinstance(value, dict) else {}


def check_role_skills(t):
    chosen = coord_json(t).get("role_skills")
    choices = {role: chosen.get(role) for role in SKILL_ROLES} if isinstance(chosen, dict) else {}
    if not any(isinstance(choices.get(role), list) for role in SKILL_ROLES):
        return verdict("UNKNOWN", "not exercised: no role has a skill choice, so every role gets all approved skills")
    nesting = t.q("SELECT depth FROM team_nesting WHERE project=?", t.pid) if t.columns("team_nesting") else []
    limit = nesting[0][0] if nesting else DEPTH_DEFAULT
    approved = enabled_skills(t)
    lines, bad, checked = [], [], 0
    for a in t.tree:
        if a["role"] == "teammate":
            continue
        role = "master" if a["role"] == "master" else "lead" if a["role"] == "sub" and depth_of(t, a) < limit else "worker"
        record = given_record(t, "agent-skills", a["id"])
        got = sorted(line.strip() for line in (record or "").splitlines() if line.strip())
        folder = t.home(a["id"]) / ("crazyagent/plugin/skills" if a["harness"].startswith("claude") else "skills/crazyagent")
        installed = sorted(p.name for p in folder.iterdir() if p.is_dir()) if folder.is_dir() else []
        choice = choices.get(role)
        if isinstance(choice, list):
            checked += 1
            expected = sorted(name for name in choice if isinstance(name, str))
            line = f"{t.label(a)} role {role} chose {listed(expected) or 'none'}; given {listed(got) or 'none'}; installed {listed(installed) or 'none'}"
            if record is None or got != expected or installed != expected:
                bad.append(line + (" (no skills record)" if record is None else ""))
        else:
            line = f"{t.label(a)} role {role} all approved ({len(approved)}); given {len(got)}; installed {len(installed)}"
        lines.append(line)
    detail = f"role choices: {'; '.join(f'{r}=' + ('all' if not isinstance(choices.get(r), list) else listed(choices[r]) or 'none') for r in SKILL_ROLES)}; Sub-agent depth={limit}; agents checked={checked} bad={len(bad)}; teammates use their lead's set"
    if bad:
        return verdict("FAIL", detail, lines)
    return verdict("PASS" if checked else "UNKNOWN", detail if checked else detail + "; no agent of a role with a choice", lines)


def nudge_mode(value):
    if value is True:
        return "ten_minutes"
    if value is False or value is None:
        return "off"
    return value if value in NUDGE_MODES else f"unknown {value!r}"


def check_nudges(t):
    stored = coord_json(t)
    mode = nudge_mode(stored.get("nudges"))
    sent = t.q(
        "SELECT to_agent,idem_key FROM coord_messages WHERE project=? AND from_kind='engine' AND to_kind='agent' "
        "AND idem_key LIKE 'adoption-%' ORDER BY id",
        t.pid,
    )
    kinds = Counter()
    for _, key in sent:
        kinds[next((kind for kind, prefix in NUDGE_KEYS if key.startswith(prefix)), "other")] += 1
    marked = {}
    for agent_id in t.agents:
        mailbox = t.root / "team" / "agents" / f"agent-{agent_id}" / "tmp" / MAILBOX
        found = sorted(mailbox.glob("*.nudged")) if mailbox.is_dir() else []
        if found:
            marked[agent_id] = len(found)
    entries = delivery_entries(t)
    reminded = sum(s["phrases"][FIRST_EDIT_REMINDER] for _, _, s in entries)
    for name, _, s in entries:
        if FIRST_EDIT_REMINDER in s["first"]:
            t.cite(s, s["first"][FIRST_EDIT_REMINDER], f"{name} first first-edit reminder")
    first_edit = kinds["first_edit"] + sum(marked.values()) + reminded
    detail = (
        f"mode={mode}{'' if 'nudges' in stored else ' (no setting: off since 18.13, on before)'}; engine notes: first_edit={kinds['first_edit']} ten_minutes={kinds['ten_minutes']} other={kinds['other']}; "
        f"hook markers={listed(f'agent {a} x{n}' for a, n in sorted(marked.items())) or 'none'}; first-edit reminders in transcripts={reminded}"
    )
    if mode not in NUDGE_MODES:
        return verdict("FAIL", detail + "; the setting holds no known mode")
    if mode == "off":
        sent_any = first_edit + kinds["ten_minutes"]
        return verdict("FAIL" if sent_any else "UNKNOWN", detail + ("; nudges sent while off" if sent_any else "; nudges are off for the project"))
    if mode == "first_edit":
        if kinds["ten_minutes"]:
            return verdict("FAIL", detail + "; ten-minute nudges sent under first_edit")
        return verdict("PASS" if first_edit else "UNKNOWN", detail if first_edit else detail + "; no agent edited before a code-graph query")
    if first_edit:
        return verdict("FAIL", detail + "; first-edit nudges sent under ten_minutes")
    return verdict("PASS" if kinds["ten_minutes"] else "UNKNOWN", detail if kinds["ten_minutes"] else detail + "; no agent earned a ten-minute nudge")


def check_reask(t):
    tables = [row[0] for row in t.db.execute("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE '%reask%' OR name LIKE '%re_ask%')")]
    keys = [key for key in coord_json(t) if "reask" in key.replace("_", "").lower()]
    empty = t.q("SELECT COUNT(*) FROM team_messages WHERE project=? AND author='engine' AND text LIKE ?", t.pid, f"{EMPTY_REPLY_NOTE}%")[0][0]
    if not tables and not keys:
        return verdict("UNKNOWN", f"not in this build: no re-ask setting (feat/reask-empty-reply has no commits); empty master replies noted={empty}")
    return verdict("UNKNOWN", f"a re-ask setting exists ({listed(tables + keys)}) but this audit has no evidence rule for it yet; empty master replies noted={empty}")


CHECKS = (
    {
        "id": "T3-01", "name": "tree", "run": check_tree,
        "title": "The team tree has a master, a native lead and an app-spawned lead with two sub-agents each",
        "proves": "The master built the team the trial prompt asked for, with both lead modes the owner can choose.",
        "why": "DEC-104 coordination layer: native or app-spawned sub-agents by user choice. Trial 2: PASS (master 45, native lead 46 with teammates 50, 51, app-spawned lead 47 with sub-agents 48, 49).",
        "how": "team_agents rows of the project grouped by parent; a lead's mode from team_agents.mode, team_tasks.mode or its teammates; teammate panes from team_teammates.",
    },
    {
        "id": "T3-02", "name": "own clones", "run": check_clones,
        "title": "Every sub-agent works in its own place on a <lead branch>--<name> branch",
        "proves": "App-spawned sub-agents get their own clone under .crazyagent/worktrees and native teammates a worktree under the lead's .worktrees, never a shared folder.",
        "why": "Agent branches and own clones (DEC-095 landing). Trial 2: FAIL (teammates 50 and 51 had no worktree).",
        "how": "team_worktrees and landing_agent_branches rows, then git rev-parse --git-common-dir and the .git entry in each worktree; git worktree list for native panes without agent rows.",
    },
    {
        "id": "T3-03", "name": "native lead", "run": check_native,
        "title": "The native Claude lead keeps its session and runs its own teammates",
        "proves": "The native lead's team calls reach the engine as that lead and its teammates exist as panes.",
        "why": "DEC-104 native agent teams. Trial 2: PASS (lead 46 kept its session, start/inspect_work ok=11, team_caller_unknown=0, panes %2, %3).",
        "how": "team_panes, team_agents.pane and team_teammates rows; the lead's and teammates' transcripts for work-tool results and team_caller_unknown refusals.",
    },
    {
        "id": "T3-04", "name": "owner question", "run": check_question,
        "title": "A For: Me question reaches the owner, only the owner answers, and the answer is used",
        "proves": "An agent asked the owner directly, no agent answered for the owner, and the owner's answer reached the code.",
        "why": "K7 For: Me questions. Trial 2: PASS (question 11 from agent 50 replied by the owner with Hello; greet.py on crazyagent/lead-a holds it).",
        "how": "coord_messages questions with q_for='me' to the owner and their replies; git cat-file of greet.py on every local branch for the owner's answer.",
    },
    {
        "id": "T3-05", "name": "wake and receipts", "run": check_wake,
        "title": "Each lead reads its messages and an app-spawned lead wakes when its workers finish",
        "proves": "Read receipts are recorded and a lead whose sub-agents finished got a new prompt afterwards.",
        "why": "K6 receipts and the finished-agent wake (DEC-102). Trial 2: FAIL (lead 46 read none of its 2 messages; lead 47 was not woken after its workers finished).",
        "how": "coord_messages receipts per lead; the latest finished or change_set_ready team_events of the lead's sub-agents against prompt times in the lead's transcript.",
    },
    {
        "id": "T3-06", "name": "claim overlap", "run": check_overlap,
        "title": "Overlapping claims between agents are found and the agents are told",
        "proves": "Two unrelated agents claiming the same file or folder at the same time produce an overlap warning in a work-tool result or a message.",
        "why": "DEC-104 claims and overlap warnings. Trial 2: PASS (claims 16 and 17 on calc.py; agent 48 saw it in a work-tool result; agent 49 got 2 messages).",
        "how": "coord_claims pairs alive at the same time compared by pattern; work-tool results in transcripts and coord_messages naming an overlap.",
    },
    {
        "id": "T3-07", "name": "merges", "run": check_merges,
        "title": "Sub-agent work is merged into the lead branches and the leads into the Master's branch, locally",
        "proves": "Every sub-agent's own commits reach its lead's branch and every lead's work reaches the Master's branch with nothing pushed.",
        "why": "DEC-095 landing as owner-accepted change sets. Trial 2: FAIL (teammates 50, 51 had no head; lead 46's crazyagent/lead-a was not in main).",
        "how": "git rev-list and merge-base --is-ancestor per worktree; accepted landing_change_sets rows with their landed commit; team_master_branch; remote refs containing each commit.",
    },
    {
        "id": "T3-08", "name": "reply", "run": check_reply,
        "title": "Agent replies are stored with the message they answer",
        "proves": "A reply an agent sends is linked to its question and is never refused as invalid.",
        "why": "Coordination replies (DEC-104). Trial 2: UNKNOWN (no agent sent a reply).",
        "how": "coord_messages of kind reply with reply_to from agents; coord_reply_invalid results in transcripts.",
    },
    {
        "id": "T3-09", "name": "sandbox git", "run": check_git,
        "title": "Git works inside every agent's sandbox",
        "proves": "Agent git calls succeed with no xcrun shim errors under the sandbox.",
        "why": "Sandbox git through the real binary (Q73, IMP-AWF). Trial 2: PASS (every agent's git calls ok, 0 xcrun errors).",
        "how": "Shell tool calls naming git in each transcript and their results; xcrun_db or xcrun: error in any result.",
    },
    {
        "id": "T3-10", "name": "stop all", "run": check_stopped,
        "title": "Stop all leaves no live agent row, pane or process",
        "proves": "The owner's Stop all pauses the project and every agent process of the project is gone.",
        "why": "Owner Stop all. Trial 2: PASS (project paused, all agents stopped, no open panes or processes).",
        "how": "team_paused_projects, team_owner_stops, open team_teammates panes, team_agents statuses and ps for each agent's process group or engine paths.",
    },
    {
        "id": "T3-11", "name": "instruction delivery", "run": check_delivery,
        "title": "The app instruction set precedes the project's rules in every launch and no repo instruction file loads",
        "proves": "Each agent's first context holds the app instructions before the project's imported rules, and the harness loads no CLAUDE.md, AGENTS.md or CLAUDE.local.md from the repository.",
        "why": "DEC-106 instruction set in the app. Trial 2: FAIL (teammates 50 and 51 had no transcript).",
        "how": "The first api-request-shape or prompt snapshot of each transcript, teammate pane transcripts included, for both marks and their order; instructions attachments for repo files; home AGENTS.md for Codex agents.",
    },
    {
        "id": "T3-12", "name": "adoption", "run": check_adoption,
        "title": "Every skill an agent calls is counted per skill",
        "proves": "Skill calls in transcripts have matching per-skill counts in the engine's tool-use store.",
        "why": "Owner: per-skill counts, instructions not guards. Trial 2: UNKNOWN (no skill call in any transcript or store).",
        "how": "Enabled skills from plugin_items and plugin_switches; coord_tool_use rows per agent against Skill calls in transcripts.",
    },
    {
        "id": "T3-13", "name": "tui", "run": check_tui,
        "title": "The TUI shows a clean Claude input border and Ctrl-T opens Team",
        "proves": "The recorded TUI frames show the owner-facing screens; a person reads them.",
        "why": "Trial TUI surface (DEC-112 decision 1). Trial 2: UNKNOWN (read from frames only).",
        "how": "Not in the engine store: the recorded frames and trial.gif in the run folder.",
    },
    {
        "id": "T3-14", "name": "import", "run": check_import,
        "title": "Onboarding imports the repository CLAUDE.md into the project instructions",
        "proves": "The repo CLAUDE.md is moved to .crazyagent/imported and its text is held under a source heading in .crazyagent/instructions.md.",
        "why": "DEC-106 import. Trial 2: PASS (CLAUDE.md moved; instructions.md holds its text under ## Imported from CLAUDE.md).",
        "how": "The project folder's CLAUDE.md, .crazyagent/imported/CLAUDE.md and .crazyagent/instructions.md; git ls-files; instruction_imports rows.",
    },
    {
        "id": "T3-15", "name": "transcripts", "run": audit_transcripts,
        "title": "Every agent has a transcript and uses ctx, graphify and skills",
        "proves": "Each agent's transcript exists and shows context-mode, graphify and Skill calls.",
        "why": "Owner audit of agent transcripts after a team build. Trial 2: FAIL (no transcripts for 50, 51; no ctx, graphify or Skill call by any agent).",
        "how": "Tool calls in each agent's transcripts under its home, teammates through their lead's home.",
    },
    {
        "id": "T3-16", "name": "launch argv", "run": audit_argv,
        "title": "A native lead's harness is not denied the tools it needs to run its team",
        "proves": "The native lead's claude process does not disallow Agent or TeamCreate.",
        "why": "Native team launch arguments (DEC-104). Trial 2: UNKNOWN (no live lead process when the audit ran).",
        "how": "ps command lines of each lead's claude process for --disallowedTools and --teammate-mode; needs a run before Stop all.",
    },
    {
        "id": "T3-17", "name": "instructions", "run": audit_instructions,
        "title": "Agent instruction files hold the instructions inline",
        "proves": "No agent's home CLAUDE.md or AGENTS.md was offloaded to a side file.",
        "why": "DEC-106 delivery size. Trial 2: PASS (every home CLAUDE.md 20183 B inline).",
        "how": "CLAUDE.md and AGENTS.md in each agent home for the offload sentence; the app instructions.md size.",
    },
    {
        "id": "T3-18", "name": "tool use", "run": audit_tool_use,
        "title": "Every agent's work, ctx and graphify calls are counted",
        "proves": "The engine's tool-use store has work-tool, ctx and graphify counts for each agent.",
        "why": "Owner: counted tool adoption per agent. Trial 2: FAIL (no ctx or graphify count for any agent).",
        "how": "coord_tool_use rows per agent grouped into work tools, ctx_* and graphify.",
    },
    {
        "id": "T3-19", "name": "land on", "run": check_land_on,
        "title": "Lead merges land on the branch chosen in Land on",
        "proves": "Every accepted lead change set landed on the project's Land on branch and nowhere else.",
        "why": "DEC-111 Q1, 18.13 fix (feat/team-land-on). New in trial 3.",
        "how": "team_land_on for the project; accepted landing_change_sets of the leads, git merge-base --is-ancestor of each landed commit in the chosen branch; engine team_messages naming it.",
    },
    {
        "id": "T3-20", "name": "background commands", "run": check_background,
        "title": "A task's background commands end as the Team setting says",
        "proves": "Under stop or grace the engine stops a background command at task end and records why; under stop every prompt tells the agent so.",
        "why": "DEC-111 Q13, 18.13 fix (feat/team-background-commands). New in trial 3.",
        "how": "team_background for the project; team_tasks results naming a stopped background command and their duration; the stop notice in agent transcripts.",
    },
    {
        "id": "T3-21", "name": "hidden folders", "run": check_hidden,
        "title": "Hidden folders are denied in every agent's sandbox",
        "proves": "Each folder the owner hid is a deny rule in every agent and teammate sandbox profile.",
        "why": "DEC-111 Q7, 18.13 fix (feat/agent-hidden-folders). New in trial 3; UNKNOWN until the branch is merged.",
        "how": "project_hidden_paths for the project; the hidden-folder deny block in team/profiles/agent-<id>.sb and agent-<id>-teammates.sb.",
    },
    {
        "id": "T3-22", "name": "part 1 instructions", "run": check_part_one,
        "title": "The Part 1 instruction set reaches every agent, teammates included",
        "proves": "The text each agent was given opens with '# Mandatory workflow' and every launch context, native teammates' too, holds it.",
        "why": "DEC-111 QR1 and DEC-106, 18.13 fix (feat/instruction-edit-delivery). Re-proves trial 2 T3-11, which failed for teammates 50 and 51 with no transcript.",
        "how": "The engine's agent-instructions-given record per agent home (SHA-256 of its real path); the first launch context of every transcript, teammate panes through the lead's home; home AGENTS.md for Codex agents.",
    },
    {
        "id": "T3-23", "name": "skills per role", "run": check_role_skills,
        "title": "Each role gets the skills chosen for it",
        "proves": "Masters, leads and workers launch with exactly the skills the project chose for their role.",
        "why": "DEC-111 R3, 18.13 fix (feat/role-skills). New in trial 3.",
        "how": "coord_settings role_skills and team_nesting depth; the engine's agent-skills record per agent home and the skill folders installed in it.",
    },
    {
        "id": "T3-24", "name": "nudges", "run": check_nudges,
        "title": "Nudges follow the project's nudge setting",
        "proves": "Under first_edit an agent that edits before a code-graph query is reminded once, and no nudge of another mode is sent.",
        "why": "DEC-111 R4, 18.13 fix (feat/nudge-first-edit). New in trial 3.",
        "how": "coord_settings nudges; engine notes in coord_messages by idempotency key; hook .nudged markers in each agent mailbox; the reminder text in transcripts.",
    },
    {
        "id": "T3-25", "name": "re-ask", "run": check_reask,
        "title": "The master is asked again after an empty reply",
        "proves": "An empty master reply leads to a second ask as the setting says.",
        "why": "18.13 re-ask on an empty reply (feat/reask-empty-reply). UNKNOWN while the setting does not exist.",
        "how": "A re-ask table or coord_settings key; engine team_messages noting an empty master reply.",
    },
)


def excerpt(path, number):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            for at, line in enumerate(handle, 1):
                if at == number:
                    line = line.rstrip("\n")
                    return line[:EXCERPT_BYTES_MAX] + (f" ... (+{len(line) - EXCERPT_BYTES_MAX} chars)" if len(line) > EXCERPT_BYTES_MAX else "")
    except OSError:
        return None
    return None


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(text, encoding="utf-8")
    os.replace(temporary, path)


def write_json(path, value):
    write(path, json.dumps(mask(value), indent=2, ensure_ascii=False) + "\n")


def shown_path(t, path):
    return os.path.relpath(path, t.root) if under(path, t.root) else path


def record(t, out, check, outcome, reads):
    cid = check["id"]
    assert CHECK_ID.match(cid), cid
    header = f"{outcome['result']} {cid} {check['name']}: {outcome['detail']}"
    text = [header] + [f"  {cid} {line}" for line in outcome["lines"]]
    for path, number, caption in outcome["cites"]:
        line = excerpt(path, number)
        text.append(f"--- {caption}: {shown_path(t, path)}:{number}")
        text.append(line if line is not None else "(line not readable)")
    write(out / "logs" / f"{cid}.log", mask("\n".join(text) + "\n"))
    write_json(out / "db" / f"{cid}.json", {"check": cid, "project": t.pid, "reads": reads})
    rows = sum(read["count"] for read in reads)
    cited = len(outcome["cites"])
    evidence = [
        {"kind": "log", "path": f"logs/{cid}.log", "caption": f"audit output{f' and {cited} transcript line' + ('s' if cited > 1 else '') if cited else ''}, masked"},
        {"kind": "db", "path": f"db/{cid}.json", "caption": f"{len(reads)} engine store reads, {rows} rows, read only, masked"},
        {"kind": "db", "path": "db/agents.json", "caption": "the project's team agent rows"},
    ] + outcome["extra"]
    write_json(out / "checks" / f"{cid}.json", {
        "id": cid, "title": check["title"], "proves": check["proves"], "why": check["why"], "how": check["how"],
        "result": outcome["result"], "detail": outcome["detail"], "evidence": evidence,
    })
    print(mask(header), flush=True)
    for line in outcome["lines"]:
        print(mask(f"  {cid} {line}"), flush=True)


def tui_frames(out):
    found = []
    if (out / "trial.gif").is_file():
        found.append({"kind": "gif", "path": "trial.gif", "caption": "the recorded TUI session"})
    frames = sorted((out / "frames").glob("*")) if (out / "frames").is_dir() else []
    for frame in frames[:1] + frames[-1:] if len(frames) > 1 else frames:
        found.append({"kind": "frame", "path": f"frames/{frame.name}", "caption": "first recorded TUI frame" if frame == frames[0] else "last recorded TUI frame"})
    return found


def run_checks(t, out, chosen):
    results = []
    write_json(out / "db" / "agents.json", {"project": t.pid, "folder": t.repo, "reads": t.load_reads})
    for check in chosen:
        t.reads, t.cites = [], []
        try:
            outcome = check["run"](t)
        except Missing as failure:
            outcome = verdict("UNKNOWN", f"schema: {failure}")
        except Exception as failure:
            outcome = verdict("UNKNOWN", f"audit error: {type(failure).__name__}: {failure}")
        outcome["cites"] = t.cites
        if check["id"] == "T3-13":
            outcome["extra"] = outcome["extra"] + tui_frames(out)
        reads = t.reads
        t.reads = t.cites = None
        record(t, out, check, outcome, reads)
        results.append(outcome["result"])
    return results


def open_store(path, immutable):
    suffix = "?mode=ro&immutable=1" if immutable else "?mode=ro"
    db = sqlite3.connect("file:" + quote(str(path)) + suffix, uri=True)
    db.execute("PRAGMA query_only=ON")
    return db


def main(argv):
    parser = argparse.ArgumentParser(description="Audit one CrazyAgent trial run from the engine store, read only.")
    parser.add_argument("project", help="project folder or id")
    parser.add_argument("--out", required=True, help="trial run folder; checks/, db/ and logs/ are written in it")
    parser.add_argument("--root", default=os.environ.get("CRAZYAGENT_ENGINE_ROOT", str(Path.home() / "Library/Application Support/CrazyAgent/engine")))
    parser.add_argument("--db", help="engine.db to read instead of <root>/engine.db, such as a saved copy")
    parser.add_argument("--immutable", action="store_true", help="read a saved store that no engine writes")
    parser.add_argument("--project-mark", default="TRIAL3-PROJECT-RULE", help="a line of the seeded repo CLAUDE.md")
    parser.add_argument("--answer", default="Hello", help="the owner's answer to the For: Me question")
    parser.add_argument("--only", action="append", help="run only this check id; repeatable")
    args = parser.parse_args(argv)
    root = Path(os.path.abspath(args.root))
    store = Path(os.path.abspath(args.db)) if args.db else root / "engine.db"
    if not store.is_file():
        print(f"UNKNOWN T3-00 engine: no engine.db at {store}")
        return 2
    chosen = [check for check in CHECKS if not args.only or check["id"] in args.only]
    if args.only and len(chosen) != len(set(args.only)):
        print(f"unknown check id in {args.only}", file=sys.stderr)
        return 2
    out = Path(os.path.abspath(args.out))
    t = Trial(open_store(store, args.immutable), root, args.project, args.project_mark, args.answer)
    if t.pid is None:
        detail = f"no project with id or repository {args.project}"
        write(out / "logs" / "T3-00.log", mask(f"FAIL T3-00 project: {detail}\nstore {store}\n"))
        write_json(out / "checks" / "T3-00.json", {
            "id": "T3-00", "title": "The trial project is in the engine store", "proves": "The audit reads the trial's project.",
            "why": "Every check reads this project's rows.", "how": "projects rows by id or repository, then team_worktrees.",
            "result": "FAIL", "detail": detail, "evidence": [{"kind": "log", "path": "logs/T3-00.log", "caption": "audit output, masked"}],
        })
        print(f"FAIL T3-00 project: {detail}")
        return 1
    results = run_checks(t, out, chosen)
    print(f"SUMMARY project={t.pid} folder={t.repo} pass={results.count('PASS')} fail={results.count('FAIL')} unknown={results.count('UNKNOWN')}")
    return 1 if "FAIL" in results else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
