if not __debug__:
    raise SystemExit("TUI qualification requires Python assertions enabled")

import codecs
from contextlib import contextmanager
import errno
import fcntl
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

sys.dont_write_bytecode = True

import tui_flow
import tui_project_conversation as conversation


ROWS = 40
COLUMNS = 120
SETTINGS = "version = 1\ntimeout_ms = 5000\n"
NAME = "Mira Okafor"
ROLE = "Founder and full-stack developer"
BIO = "Builds tools for marine researchers."
STACK = "TypeScript, Rust, Postgres"
POOL = "E2E pool"
PROJECT = "Tidepool"
ABOUT = "A live dashboard for marine researchers to watch sensor buoys and get alerts."
RESEARCH = "Competitor research"
REPORT = "competitor_research.md"
ENTER = b"\r"
ESC = b"\x1b"
TAB = b"\t"
DOWN = b"\x1b[B"
SHIFT_LEFT = b"\x1b[1;2D"
SHIFT_RIGHT = b"\x1b[1;2C"
QUIT = b"\x11"
TOKEN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b[^\[]|[^\x1b]")
COMPLETE = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|[^\[])")
CARRY_MAX = 64
WINDOW_MAX = 16384
LIMIT_SECONDS = 2700
STARTED = time.monotonic()


def interrupted(signum, frame):
    raise SystemExit("V1 TUI flow interrupted by signal %d" % signum)


def step(text):
    print("[%7.1f s] %s" % (time.monotonic() - STARTED, text), flush=True)


class Screen:
    def __init__(self, secrets):
        self.cells = {}
        self.row = 1
        self.column = 1
        self.carry = ""
        self.window = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.secrets = secrets

    def feed(self, data):
        text = self.carry + self.decoder.decode(data)
        self.carry = ""
        cut = text.rfind("\x1b")
        if cut != -1 and len(text) - cut < CARRY_MAX and not COMPLETE.match(text, cut):
            self.carry = text[cut:]
            text = text[:cut]
        self.window = (self.window + text)[-WINDOW_MAX:]
        assert not any(secret in self.window for secret in self.secrets), \
            "a sign-in credential reached the terminal"
        for token in TOKEN.findall(text):
            self.apply(token)

    def apply(self, token):
        if token.startswith("\x1b["):
            code = token[-1]
            values = token[2:-1]
            numbers = []
            if values[:1] not in ("?", ">", "<", "="):
                numbers = [int(value) if value.isdigit() else 1 for value in values.split(";")]
            if code in "Hf":
                self.row = numbers[0] if numbers else 1
                self.column = numbers[1] if len(numbers) > 1 else 1
            elif code == "J" and values in ("2", "3", ""):
                self.cells.clear()
            elif code == "K":
                tui_flow.clear_screen_cell(self.cells, self.row, self.column)
                self.cells = {position: value for position, value in self.cells.items()
                              if position[0] != self.row or position[1] < self.column}
            elif code in "ABCD" and numbers:
                distance = numbers[0]
                self.row += distance * ((code == "B") - (code == "A"))
                self.column += distance * ((code == "C") - (code == "D"))
            elif code == "h" and values == "?1049":
                self.cells.clear()
            return
        if token.startswith("\x1b"):
            return
        if token == "\r":
            self.column = 1
        elif token == "\n":
            self.row += 1
        elif not unicodedata.category(token).startswith("C"):
            if unicodedata.combining(token):
                previous = (self.row, self.column - 1)
                if self.cells.get(previous) == "":
                    previous = (self.row, self.column - 2)
                if self.cells.get(previous):
                    self.cells[previous] += token
                return
            advance = 2 if unicodedata.east_asian_width(token) in "WF" else 1
            tui_flow.clear_screen_cell(self.cells, self.row, self.column)
            if advance == 2:
                tui_flow.clear_screen_cell(self.cells, self.row, self.column + 1)
            self.cells[(self.row, self.column)] = token
            if advance == 2:
                self.cells[(self.row, self.column + 1)] = ""
            self.column += advance

    def text(self, width, height):
        return "\n".join("".join(self.cells.get((row, column), " ")
                                 for column in range(1, min(width, 300) + 1))
                         for row in range(1, min(height, 120) + 1))


class Terminal:
    def __init__(self, binary, socket, secrets):
        self.child, self.master, self.slave, self.before = tui_flow.launch(binary, socket)
        self.screen = Screen(secrets)
        self.closed = False
        step("TUI started pid=%d supervisor=%d" % (self.child.pid, self.child.supervisor.pid))

    def size(self):
        height, width, _, _ = struct.unpack(
            "HHHH", fcntl.ioctl(self.master, termios.TIOCGWINSZ, bytes(8)))
        return width, height

    def pump(self, wait):
        if not select.select([self.master], [], [], wait)[0]:
            return False
        try:
            part = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            return False
        self.screen.feed(part)
        return bool(part)

    def settle(self):
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline and self.pump(0.15):
            pass

    def text(self):
        width, height = self.size()
        return self.screen.text(width, height)

    def lines(self, text):
        return [line for line in self.text().splitlines() if text in line]

    def row(self, text):
        found = self.lines(text)
        assert found, "no row shows %r:\n%s" % (text, self.text())
        return found[0]

    def resize(self):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
        os.kill(self.child.pid, signal.SIGWINCH)
        self.settle()
        assert self.size() == (COLUMNS, ROWS)

    def until(self, label, limit, check, problems=()):
        deadline = time.monotonic() + limit
        while True:
            self.pump(0.1)
            current = self.text()
            for problem in problems:
                assert problem not in current, \
                    "%s: the screen shows %r:\n%s" % (label, problem, current)
            if check(current):
                return current
            assert self.child.poll() is None, \
                "%s: the TUI exited with %r:\n%s" % (label, self.child.poll(), current)
            assert time.monotonic() < deadline, \
                "%s: not seen within %d s:\n%s" % (label, limit, current)

    def wait(self, text, limit, problems=()):
        return self.until("waiting for %r" % text, limit, lambda current: text in current, problems)

    def key(self, data):
        assert self.child.poll() is None, "the TUI exited before a key press"
        os.write(self.master, data)
        self.settle()

    def type(self, text):
        assert "\x1b" not in text and "\r" not in text
        self.key(text.encode())

    def close(self):
        os.write(self.master, QUIT)
        deadline = time.monotonic() + 3
        while self.child.poll() is None and time.monotonic() < deadline:
            self.pump(0.05)
        if self.child.poll() is None:
            os.write(self.master, QUIT)
        tui_flow.close_tui(self.child, self.master, self.slave, self.before, key=None)
        self.closed = True


@contextmanager
def terminal(binary, socket, secrets):
    term = Terminal(binary, socket, secrets)
    try:
        term.wait("CrazyAgent", 20)
        term.resize()
        yield term
        term.close()
    finally:
        try:
            if not term.closed:
                conversation.cleanup(term.child, term.master, term.slave)
        finally:
            tui_flow.outputs.pop(term.master, None)
        step("TUI reaped pid=%d" % term.child.pid)


def secrets_of(source):
    data = json.loads((source / "auth.json").read_text())
    assert isinstance(data, dict), "BLOCKED: the managed profile auth.json is not a sign-in record"
    tokens = data.get("tokens") or {}
    values = [value for key, value in tokens.items()
              if key.endswith("_token") and isinstance(value, str) and len(value) >= 24]
    key = data.get("OPENAI_API_KEY")
    if isinstance(key, str) and len(key) >= 24:
        values.append(key)
    assert values, "BLOCKED: the managed profile holds no ChatGPT sign-in tokens"
    return values


def private_file(path, data):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        output.write(data)


def start_engine(binary, root, logs, codex):
    output = os.open(logs / "engine.out", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    errors = os.open(logs / "engine.err", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        child = subprocess.Popen(
            [binary, "engine", str(root), str(root / "global.toml"), str(root / "project.toml"),
             "engine.sock", "--writable-settings", "--codex-executable", codex],
            stdin=subprocess.DEVNULL, stdout=output, stderr=errors, start_new_session=True,
        )
    finally:
        os.close(output)
        os.close(errors)
    deadline = time.monotonic() + 60
    while b"ready\n" not in (logs / "engine.out").read_bytes():
        assert child.poll() is None, ("engine exited before ready", child.returncode,
                                      (logs / "engine.err").read_bytes()[-2000:])
        assert time.monotonic() < deadline, "engine was not ready within 60 s"
        time.sleep(0.1)
    step("engine ready pid=%d" % child.pid)
    return child


def stop_engine(child):
    if child.poll() is None:
        child.send_signal(signal.SIGINT)
    try:
        status = child.wait(timeout=60)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
        raise AssertionError("engine needed a forced stop pid=%d" % child.pid)
    assert status == 0, ("engine exit status", status)
    step("engine stopped pid=%d" % child.pid)


def remove(base):
    for directory, folders, _ in os.walk(base):
        for folder in folders:
            path = os.path.join(directory, folder)
            if not os.path.islink(path):
                os.chmod(path, 0o700)
    shutil.rmtree(base)


def first_run(term):
    term.wait("Welcome to CrazyAgent", 30)
    term.type(NAME)
    for value in (ROLE, BIO, STACK):
        term.key(TAB)
        term.type(value)
    for _ in range(8):
        if "Enter Save and continue" in term.text():
            break
        term.key(TAB)
    term.wait("Enter Save and continue", 5)
    term.key(ENTER)
    term.wait("No projects yet.", 30, ("Could not save your profile.",))
    step("first-run profile saved")


def home_empty(term):
    screen = term.wait("Get set up", 10)
    assert "Add an account" in screen and "Create a pool" in screen, screen
    assert PROJECT not in screen, screen
    step("Home shows the empty state")


def seed_signed_in_home(root, source):
    profiles = root / "native-profiles"
    slots = [entry for entry in profiles.iterdir() if entry.name != "identity.json"]
    assert len(slots) == 1, "expected one engine-made managed profile, found %d" % len(slots)
    home = slots[0] / "home"
    assert home.is_dir() and not home.is_symlink() and not slots[0].is_symlink()
    assert (home.stat().st_mode & 0o7777) == 0o700
    assert not (home / "auth.json").exists(), "the new engine profile already holds a sign-in"
    private_file(home / "auth.json", (source / "auth.json").read_bytes())
    return home


def accounts(term, root, source):
    term.key(b"s")
    term.wait("No accounts yet.", 20)
    term.key(ENTER)
    term.wait("Where does the model come from?", 20)
    term.wait("ChatGPT / Codex", 10)
    term.key(ENTER)
    term.wait("Use the Codex sign-in on this Mac", 10)
    term.key(DOWN)
    term.key(DOWN)
    assert "\u258c" in term.row("Use the Codex sign-in on this Mac"), term.text()
    term.key(ENTER)
    failed = ("This sign-in is not available on this Mac.", "The sign-in did not finish.")
    term.wait("Codex is not signed in on this Mac.", 120, failed + ("Add to pools",))
    step("a new engine-owned profile is not signed in")
    home = seed_signed_in_home(root, source)
    term.key(b"n")
    term.wait("Add to pools", 120, failed + ("Codex is not signed in on this Mac.",))
    term.wait("Account name", 5)
    step("the engine observed the signed-in managed Codex account")
    term.key(ENTER)
    term.until("the account list", 30, lambda screen: "Add to pools" not in screen
               and "No accounts yet." not in screen and "Signed in" in screen)
    step("Settings, Accounts lists the account as signed in")
    return home


def pools(term):
    term.key(SHIFT_RIGHT)
    term.wait("No pools yet.", 20)
    term.key(b"a")
    term.wait("0 of 15 selected", 20)
    term.type(POOL)
    term.key(ENTER)
    term.wait("Showing 1-1 of 1", 20)
    term.key(b" ")
    term.wait("1 of 15 selected", 5)
    term.key(ENTER)
    term.wait("Enter Save pool", 5)
    term.key(ENTER)
    term.until("the saved pool", 20, lambda screen: "Save pool" not in screen and POOL in screen,
               ("Choose at least one account.",))
    step("Settings, Pools saved %r with the account" % POOL)
    term.key(ESC)
    term.wait("No projects yet.", 20)


def new_project(term):
    term.key(b"n")
    term.wait("Tell agents what you are building.", 20)
    term.type(PROJECT)
    term.key(ENTER)
    term.type(ABOUT)
    term.key(ENTER)
    term.wait("New project  /  Stack and platforms", 10, ("Give the project a name.",))
    term.key(SHIFT_RIGHT)
    term.wait("New project  /  Documents", 10)
    term.key(SHIFT_RIGHT)
    term.wait("New project  /  Work to start", 10)
    term.wait(RESEARCH, 10)
    term.key(DOWN)
    term.key(b" ")
    assert "[x]" in term.row(RESEARCH), term.text()
    assert "[ ]" in term.row("Market research"), term.text()
    term.key(SHIFT_RIGHT)
    term.wait("New project  /  Pool", 10)
    term.wait(POOL, 10)
    assert "(*)" in term.row(POOL), term.text()
    term.key(SHIFT_RIGHT)
    term.wait("New project  /  Team", 10)
    term.until("the default team", 30, lambda screen: any(
        "Codex" in line for line in term.lines("Master")))
    term.key(SHIFT_RIGHT)
    review = term.wait("Create project", 10)
    assert any(RESEARCH in line for line in term.lines("Work to start")), review
    assert any(POOL in line for line in term.lines("Pool")), review
    assert ABOUT[:40] in review, review
    step("New project review lists the research toggle and the pool")
    term.key(ENTER)
    term.wait("Home  /  %s" % PROJECT, 60, (
        "Could not create the project.", "Could not create the folder.",
        "CrazyAgent could not confirm the new project.",
        "The project is made, but its pool is not assigned."))
    term.until("the assigned pool", 30, lambda screen: any(
        "1 account" in line for line in term.lines(POOL)))
    step("the project opened with its pool assigned")


def research_state(term):
    rows = term.lines(RESEARCH)
    for state in ("Failed", "Needs a pool", "Stopped", "Done", "Running", "Queued"):
        if any(state in row for row in rows):
            return state
    return None


def research(term):
    problems = ("Failed", "Needs a pool", "Stopped")
    started = time.monotonic()
    term.until("research progress", 300, lambda screen: research_state(term) in
               ("Running", "Done") + problems)
    state = research_state(term)
    assert state in ("Running", "Done"), "%s shows %s:\n%s" % (RESEARCH, state, term.text())
    step("the project view shows %s as %s" % (RESEARCH, state))
    term.until("research done", 1500, lambda screen: research_state(term) == "Done"
               or research_state(term) in problems)
    state = research_state(term)
    assert state == "Done", "%s ended as %s:\n%s" % (RESEARCH, state, term.text())
    step("%s finished in %.1f s" % (RESEARCH, time.monotonic() - started))


def context(term):
    term.key(SHIFT_LEFT)
    term.key(SHIFT_LEFT)
    term.wait("%s  /  Context" % PROJECT, 10)
    term.wait("brief.md", 30)
    term.wait(REPORT, 30)
    step("the Context tab lists brief.md and research/%s" % REPORT)


def master(term, marker):
    term.key(SHIFT_LEFT)
    term.wait("%s  /  Agents" % PROJECT, 10)
    term.key(SHIFT_LEFT)
    term.wait("%s  /  Conversation" % PROJECT, 10)
    blocked = ("Not signed in", "Needs a pool", "Needs you")
    term.wait("No messages yet.", 120, blocked)
    term.until("a ready master", 60, lambda screen: any(
        "Ready" in line for line in term.lines("Master")), blocked)
    term.type("Reply with only this word and nothing else: %s" % marker)
    term.key(ENTER)
    started = time.monotonic()
    term.until("the master reply", 600, lambda screen: screen.count(marker) >= 2 and any(
        "Replied" in line for line in term.lines("Master")), blocked + ("Stopped",))
    step("the master replied in %.1f s" % (time.monotonic() - started))


def reopened(term):
    term.wait(PROJECT, 30)
    screen = term.wait("Projects", 10)
    assert "Welcome to CrazyAgent" not in screen, screen
    step("a second TUI opens Home with the project and no first-run page")


def files(root, logs, secrets):
    reports = sorted(root.glob("projects/*/context/research/%s" % REPORT))
    assert len(reports) == 1, reports
    report = reports[0].read_text()
    assert report.startswith("# %s\n" % RESEARCH), report[:200]
    assert "https://" in report, report[:2000]
    brief = (reports[0].parent.parent / "brief.md").read_text()
    assert brief.startswith("# %s\n" % PROJECT) and ABOUT in brief, brief[:400]
    checked = [root / "global.toml", logs / "engine.out", logs / "engine.err"]
    checked += [path for path in (root / "project.toml", root / "engine.db", root / "engine.db-wal")
                if path.is_file()]
    checked += [path for path in (root / "projects").rglob("*")
                if path.is_file() and not path.is_symlink()]
    for path in checked:
        data = path.read_bytes()
        assert len(data) <= 268435456, path
        text = data.decode("utf-8", "replace")
        assert not any(secret in text for secret in secrets), \
            "a sign-in credential reached %s" % path.name
    step("research report, brief and %d engine files checked; no credential found" % len(checked))


def run(binary, codex, source):
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
        signal.signal(handled, interrupted)
    signal.setitimer(signal.ITIMER_REAL, LIMIT_SECONDS)
    os.environ["CRAZYAGENT_TUI"] = "v1"
    os.environ["LANG"] = "en_US.UTF-8"
    for name in ("LC_ALL", "LC_CTYPE"):
        os.environ.pop(name, None)
    secrets = secrets_of(source)
    marker = "TIDE%d" % os.getpid()
    base = Path(tempfile.mkdtemp(prefix="ca-v1e2e-", dir="/private/tmp")).resolve()
    engine = None
    try:
        root = base / "r"
        logs = base / "logs"
        root.mkdir(mode=0o700)
        logs.mkdir(mode=0o700)
        private_file(root / "global.toml", SETTINGS.encode())
        engine = start_engine(binary, root, logs, codex)
        socket = root / "engine.sock"
        with terminal(binary, socket, secrets) as term:
            first_run(term)
            home_empty(term)
            home = accounts(term, root, source)
            pools(term)
            new_project(term)
            research(term)
            context(term)
            master(term, marker)
        with terminal(binary, socket, secrets) as term:
            reopened(term)
        stop_engine(engine)
        engine = None
        files(root, logs, secrets + secrets_of(home))
        step("V1 TUI flow passed")
    finally:
        if engine is not None and engine.poll() is None:
            try:
                os.killpg(engine.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            engine.wait(timeout=5)
        remove(base)


if __name__ == "__main__":
    assert len(sys.argv) == 4, "usage: tui_v1_flow.py CRAZYAGENT_BINARY CODEX_EXECUTABLE MANAGED_CODEX_HOME"
    run(str(Path(sys.argv[1]).resolve()), str(Path(sys.argv[2]).resolve()), Path(sys.argv[3]).resolve())
