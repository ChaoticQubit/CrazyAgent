"""U1 speed row (DEC-169): times the real TUI the owner's way on a running engine that holds
the seeded speed project. Takes the crazyagent binary, the engine's socket and the project's
name. Prints one JSON line of `tui_<step>_ms` times: launch to a usable Home, then each key
press to its usable screen."""

if not __debug__:
    raise SystemExit("TUI speed rows require Python assertions enabled")

import errno
import fcntl
import json
import math
import os
from pathlib import Path
import re
import select
import signal
import struct
import sys
import termios
import time

sys.dont_write_bytecode = True

import tui_flow
import tui_project_conversation as conversation

ENTER = b"\r"
ESC = b"\x1b"
QUIT = b"\x11"
SHIFT_RIGHT = b"\x1b[1;2C"
BRAND = "CrazyAgent"
CRUMB = "  /  "
SPINNER = re.compile(r"[\u2800-\u28ff]\s+Loading")
PROJECT = "{project}"
STEP_SECONDS = 30
SETTLE_SECONDS = 0.3
LOOK_SECONDS = 0.005
QUIT_SECONDS = 3
OUTPUT_MAX = 1 << 20
STEPS = (
    ("launch", None, ("Home",)),
    ("settings", b"s", ("Settings", "Accounts")),
    ("tab_pools", SHIFT_RIGHT, ("Settings", "Pools")),
    ("home", ESC, ("Home",)),
    ("open_project", ENTER, ("Home", PROJECT)),
    ("tab_conversation", SHIFT_RIGHT, (PROJECT, "Conversation")),
    ("tab_agents", SHIFT_RIGHT, (PROJECT, "Agents")),
)


def crumb(screen):
    """The header path of a TUI screen: the crumbs after the brand on its first line, without
    the status on the right. Takes the screen text. Returns a tuple of crumbs, or None when no
    header with a path is drawn yet."""
    lines = screen.splitlines()[:1]
    if not lines or BRAND not in lines[0]:
        return None
    head = re.split(r"\s{3,}", lines[0][lines[0].index(BRAND):].rstrip())[0]
    parts = tuple(part.strip() for part in head.split(CRUMB))
    assert parts[0] == BRAND, parts
    return parts[1:] or None


def at(screen, path):
    """Whether a screen's header path ends with the given crumbs, crumb by crumb. Takes the
    screen text and the crumbs. Returns the answer."""
    assert path and all(path), path
    found = crumb(screen)
    return found is not None and len(found) >= len(path) and found[-len(path):] == tuple(path)


def usable(screen, path, project):
    """Whether a screen is usable: at the path, nothing on it still loading, and Home listing
    the project. Takes the screen text, the crumbs and the project. Returns the answer."""
    assert project.strip(), "a project name"
    listed = tuple(path) != ("Home",) or project in screen
    return at(screen, path) and listed and not SPINNER.search(screen)


def metrics(times):
    """Names each step's time as its speed metric. Takes `{step: ms}` holding every step.
    Returns `{"tui_<step>_ms": ms}`."""
    assert set(times) == {name for name, _, _ in STEPS}, sorted(times)
    named = {}
    for name, ms in times.items():
        assert isinstance(ms, float) and math.isfinite(ms) and ms >= 0, (name, ms)
        named["tui_%s_ms" % name] = round(ms, 2)
    assert len(named) == len(STEPS)
    return named


class Terminal:
    """One TUI on a pseudo-terminal, started by tui_flow's supervisor at its 24 by 100 size,
    with the time it was started at."""

    def __init__(self, binary, socket):
        self.began = time.monotonic()
        self.child, self.master, self.slave, self.before = tui_flow.launch(binary, socket)
        self.closed = False

    def screen(self):
        height, width, _, _ = struct.unpack(
            "HHHH", fcntl.ioctl(self.master, termios.TIOCGWINSZ, bytes(8)))
        return tui_flow.screen_text(tui_flow.outputs[self.master], width, height)

    def pump(self, wait):
        if not select.select([self.master], [], [], wait)[0]:
            return
        try:
            part = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            return
        output = tui_flow.outputs[self.master]
        output.extend(part)
        assert len(output) <= OUTPUT_MAX, "the TUI wrote over %d bytes" % OUTPUT_MAX

    def until(self, name, path, project, began):
        deadline = began + STEP_SECONDS
        while True:
            self.pump(LOOK_SECONDS)
            screen = self.screen()
            if usable(screen, path, project):
                return (time.monotonic() - began) * 1000.0
            assert self.child.poll() is None, \
                "%s: the TUI exited with %r:\n%s" % (name, self.child.poll(), screen)
            assert time.monotonic() < deadline, \
                "%s: %r not usable within %d s:\n%s" % (name, path, STEP_SECONDS, screen)

    def settle(self):
        deadline = time.monotonic() + SETTLE_SECONDS
        while time.monotonic() < deadline:
            self.pump(LOOK_SECONDS)

    def close(self):
        os.write(self.master, QUIT)
        deadline = time.monotonic() + QUIT_SECONDS
        while self.child.poll() is None and time.monotonic() < deadline:
            self.pump(0.05)
        if self.child.poll() is None:
            os.write(self.master, QUIT)
        tui_flow.close_tui(self.child, self.master, self.slave, self.before, key=None)
        self.closed = True


def walk(term, project):
    """Times every step on one launched TUI: launch from its start, each other step from its
    key press, to its usable screen, letting the screen settle after each. Takes the terminal
    and the project. Returns `{step: ms}`."""
    times = {}
    for name, key, crumbs in STEPS:
        path = tuple(project if part == PROJECT else part for part in crumbs)
        began = term.began
        if key is not None:
            assert not at(term.screen(), path), "%s: already at %r before the key" % (name, path)
            began = time.monotonic()
            os.write(term.master, key)
        times[name] = term.until(name, path, project, began)
        term.settle()
    assert len(times) == len(STEPS)
    return times


def run(binary, socket, project):
    """Launches the v1 TUI once, times its steps and quits it, reaping it on any failure.
    Takes the binary, the engine socket and the project. Returns the metrics."""
    os.environ["CRAZYAGENT_TUI"] = "v1"
    os.environ["LANG"] = "en_US.UTF-8"
    for name in ("LC_ALL", "LC_CTYPE"):
        os.environ.pop(name, None)
    term = Terminal(binary, socket)
    try:
        times = walk(term, project)
        term.close()
    finally:
        try:
            if not term.closed:
                conversation.cleanup(term.child, term.master, term.slave)
        finally:
            tui_flow.outputs.pop(term.master, None)
    return metrics(times)


def interrupted(signum, frame):
    raise SystemExit("TUI speed row interrupted by signal %s" % signum)


if __name__ == "__main__":
    assert len(sys.argv) == 4, "usage: tui_speed.py CRAZYAGENT_BINARY ENGINE_SOCKET PROJECT"
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(handled, interrupted)
    engine_socket = Path(sys.argv[2])
    assert engine_socket.is_socket(), "%s is a running engine's socket" % engine_socket
    print(json.dumps(run(str(Path(sys.argv[1]).resolve()), engine_socket, sys.argv[3]),
                     sort_keys=True), flush=True)
