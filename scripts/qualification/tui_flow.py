if not __debug__:
    raise SystemExit("TUI qualification requires Python assertions enabled")

import codecs
import errno
import json
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
import threading
import unicodedata


outputs = {}


class PtyProcess:
    def __init__(self, supervisor, status, release):
        self.supervisor = supervisor
        self.status = os.fdopen(status, "rb", buffering=0)
        self.release_fd = release
        self.returncode = None
        if not select.select([self.status], [], [], 5)[0]:
            raise AssertionError("PTY supervisor did not report the real TUI PID")
        self.pid = int(self.status.readline())

    def poll(self):
        if self.returncode is None and select.select([self.status], [], [], 0)[0]:
            self.returncode = int(self.status.readline())
        return self.returncode

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while self.poll() is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired("actual TUI child", timeout)
            select.select([self.status], [], [], remaining)
        return self.returncode

    def kill(self):
        if self.poll() is None:
            os.kill(self.pid, signal.SIGKILL)

    def release(self):
        if self.release_fd is not None:
            os.write(self.release_fd, b"x")
            os.close(self.release_fd)
            self.release_fd = None
            assert self.supervisor.wait(timeout=5) == 0
            self.status.close()


def supervise(binary, socket, status, release):
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    os.tcsetpgrp(0, os.getpgrp())
    shell = os.environ.get("CRAZYAGENT_TUI", "classic")
    child = subprocess.Popen([binary, "tui", socket], env=dict(os.environ, CRAZYAGENT_TUI=shell))
    os.write(status, (str(child.pid) + "\n").encode())
    os.write(status, (str(child.wait()) + "\n").encode())
    os.read(release, 1)


def inspect_pty(child, master, slave, label):
    dimensions = []
    foreground = []
    for descriptor in [master, slave]:
        dimensions.append(struct.unpack("HHHH", fcntl.ioctl(descriptor, termios.TIOCGWINSZ, bytes(8))))
        try:
            foreground.append(os.tcgetpgrp(descriptor))
        except OSError as error:
            foreground.append((error.errno, error.strerror))
    topology = subprocess.run(
        ["ps", "-p", str(child.pid), "-o", "pid=,ppid=,pgid=,sess=,tpgid=,tty="],
        capture_output=True, text=True, timeout=2,
    )
    session = os.getsid(child.pid)
    process_group = os.getpgid(child.pid)
    assert session == child.supervisor.pid
    assert foreground[0] == process_group and process_group > 0
    assert dimensions[0] == dimensions[1]
    print("PTY topology", label, "master/slave kernel dimensions", dimensions,
          "foreground groups", foreground, "session", session, "process", topology.stdout.strip(),
          "ps status", topology.returncode, topology.stderr.strip(), flush=True)


def clear_screen_cell(cells, row, column):
    value = cells.pop((row, column), None)
    if value == "":
        assert cells.get((row, column - 1)), "wide continuation has no leading cell"
        del cells[(row, column - 1)]
    elif value is not None and cells.get((row, column + 1)) == "":
        del cells[(row, column + 1)]


SCREEN_TOKEN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|[^\x1b]")
SCREEN_PARTIAL = re.compile(r"\x1b(\[[0-?]*[ -/]*)?")
SCREEN_TAIL = 64
screens = {}


def screen_state(raw):
    state = screens.get(id(raw))
    if (isinstance(raw, bytearray) and state is not None and state["raw"] is raw
            and state["offset"] <= len(raw)
            and raw[max(0, state["offset"] - SCREEN_TAIL):state["offset"]] == state["tail"]):
        return state
    state = {"raw": raw, "offset": 0, "tail": b"", "carry": "", "cells": {}, "row": 1, "column": 1,
             "decoder": codecs.getincrementaldecoder("utf-8")("replace")}
    if isinstance(raw, bytearray):
        screens[id(raw)] = state
    return state


def screen_advance(state, text):
    cells = state["cells"]
    row, column = state["row"], state["column"]
    for token in SCREEN_TOKEN.findall(text):
        if token.startswith("\x1b["):
            code = token[-1]
            values = token[2:-1]
            numbers = [int(x or "1") for x in values.split(";")] if not values.startswith("?") else []
            if code in "Hf":
                row = numbers[0] if numbers else 1
                column = numbers[1] if len(numbers) > 1 else 1
            elif code == "J" and values in ("2", "3", ""):
                cells.clear()
            elif code == "K":
                clear_screen_cell(cells, row, column)
                for position in [position for position in cells
                                 if position[0] == row and position[1] >= column]:
                    del cells[position]
            elif code in "ABCD" and numbers:
                distance = numbers[0]
                row += distance * ((code == "B") - (code == "A"))
                column += distance * ((code == "C") - (code == "D"))
            elif code == "h" and values == "?1049":
                cells.clear()
            continue
        if token == "\r":
            column = 1
        elif token == "\n":
            row += 1
        elif not unicodedata.category(token).startswith("C"):
            if unicodedata.combining(token):
                previous = (row, column - 1)
                if cells.get(previous) == "":
                    previous = (row, column - 2)
                if cells.get(previous):
                    cells[previous] += token
                continue
            advance = 2 if unicodedata.east_asian_width(token) in "WF" else 1
            clear_screen_cell(cells, row, column)
            if advance == 2:
                clear_screen_cell(cells, row, column + 1)
            cells[(row, column)] = token
            if advance == 2:
                cells[(row, column + 1)] = ""
            column += advance
    state["row"], state["column"] = row, column


def screen_text(raw, width, height):
    state = screen_state(raw)
    if state["offset"] < len(raw):
        text = state["carry"] + state["decoder"].decode(bytes(raw[state["offset"]:]))
        state["offset"] = len(raw)
        state["tail"] = bytes(raw[max(0, len(raw) - SCREEN_TAIL):])
        escape = text.rfind("\x1b")
        state["carry"] = ""
        if escape >= 0 and SCREEN_PARTIAL.fullmatch(text, escape):
            state["carry"] = text[escape:]
            text = text[:escape]
        screen_advance(state, text)
    cells = state["cells"]
    assert state["offset"] == len(raw)
    return "\n".join("".join(cells.get((r, c), " ") for c in range(1, min(width, 300) + 1))
                     for r in range(1, min(height, 120) + 1))


def engine(binary, root, *, writable=False):
    command = [binary, "engine", str(root), str(root / "global.toml"),
               str(root / "project.toml"), "engine.sock"]
    if writable:
        command.append("--writable-settings")
    child = subprocess.Popen(
        command,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if not select.select([child.stdout], [], [], 5)[0]:
        child.kill()
        child.wait()
        raise AssertionError("engine did not become ready within five seconds")
    ready = child.stdout.readline()
    if ready != b"ready\n":
        child.wait(timeout=5)
        raise AssertionError((ready, child.stderr.read(8192)))
    return child


def launch(binary, socket):
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

    status_read, status_write = os.pipe()
    release_read, release_write = os.pipe()
    supervisor = subprocess.Popen(
        [sys.executable, str(Path(__file__).resolve()), "--session", binary,
         str(socket), str(status_write), str(release_read)],
        stdin=slave, stdout=slave, stderr=slave,
        start_new_session=True,
        pass_fds=(status_write, release_read),
        env=dict(os.environ, TERM="xterm-256color"),
    )
    os.close(status_write)
    os.close(release_read)
    child = PtyProcess(supervisor, status_read, release_write)
    outputs[master] = bytearray()
    return child, master, slave, before


def expect(master, text, child, limit=6):
    output = outputs[master]
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                part = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                part = b""
            output.extend(part)
            assert len(output) <= 1048576, "PTY output exceeded one MiB"
            height, width, _, _ = struct.unpack("HHHH", fcntl.ioctl(master, termios.TIOCGWINSZ, bytes(8)))
            if text in screen_text(output, width, height):
                return bytes(output)
        if child.poll() is not None:
            break
    height, width, _, _ = struct.unpack("HHHH", fcntl.ioctl(master, termios.TIOCGWINSZ, bytes(8)))
    raise AssertionError("missing %r; status=%r; screen=%r; tail=%r" %
                         (text, child.poll(), screen_text(output, width, height), bytes(output[-1200:])))


def close_tui(child, master, slave, before, key=b"\x11"):
    if key is not None:
        os.write(master, key)
    deadline = time.monotonic() + 5
    while child.poll() is None and time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            try:
                outputs[master].extend(os.read(master, 65536))
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
            assert len(outputs[master]) <= 1048576
    status = child.wait(timeout=5)
    assert status == 0, "TUI exit status: %s" % status
    assert termios.tcgetattr(slave) == before, "terminal modes were not restored"
    child.release()
    os.close(master)
    os.close(slave)


class ReplyRelay:
    def __init__(self, root):
        self.path = root / "relay.sock"
        self.upstream = root / "engine.sock"
        self.held = threading.Event()
        self.release = threading.Event()
        self.stopped = threading.Event()
        self.failure = None
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(self.path))
        self.path.chmod(0o600)
        self.listener.listen(1)
        self.listener.settimeout(0.1)
        self.worker = threading.Thread(target=self.forward, daemon=True)
        self.worker.start()

    def frame(self, peer):
        def exact(size):
            data = bytearray()
            while len(data) < size:
                part = peer.recv(size - len(data))
                if not part:
                    raise AssertionError("relay peer closed before complete actual frame")
                data.extend(part)
            return bytes(data)
        header = exact(4)
        size = struct.unpack("!I", header)[0]
        assert 0 < size <= 1048576
        return header + exact(size)

    def forward(self):
        try:
            while not self.stopped.is_set():
                try:
                    downstream, _ = self.listener.accept()
                except socket.timeout:
                    continue
                with downstream, socket.socket(socket.AF_UNIX) as upstream:
                    downstream.settimeout(5)
                    upstream.settimeout(5)
                    upstream.connect(str(self.upstream))
                    request = self.frame(downstream)
                    upstream.sendall(request)
                    reply = self.frame(upstream)
                    command = json.loads(request[4:])["command"]
                    if command.get("kind") == "CreateProject" and not self.held.is_set():
                        assert json.loads(reply[4:])["outcome"]["kind"] == "Applied"
                        self.held.set()
                        assert self.release.wait(5), "actual A reply was not released"
                    downstream.sendall(reply)
        except BaseException as error:
            self.failure = error

    def close(self):
        self.stopped.set()
        self.release.set()
        self.worker.join(timeout=6)
        self.listener.close()
        assert not self.worker.is_alive(), "relay cleanup exceeded bound"
        if self.failure is not None:
            raise self.failure


def run(binary):
    children = []
    descriptors = []
    relay = None
    with tempfile.TemporaryDirectory(prefix="crazyagent-tui-") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        try:
            service = engine(binary, root)
            children.append(service)
            relay = ReplyRelay(root)
            child, master, slave, before = launch(binary, relay.path)
            children.append(child)
            descriptors.extend([master, slave])
            expect(master, "Connected", child)
            inspect_pty(child, master, slave, "initial")
            os.write(master, b"\t")
            expect(master, "New project", child)
            os.write(master, b"n")
            expect(master, "Project name", child)
            os.write(master, "界-pty-project".encode() + b"\r")
            assert relay.held.wait(5), "relay did not hold actual committed A reply"
            os.write(master, b"\x1b")
            expect(master, "Draft cleared", child)
            os.write(master, b"n")
            expect(master, "Project name", child)
            os.write(master, b"second-draft-unique")
            expect(master, "second-draft-unique", child)
            relay.release.set()
            expect(master, "Project created", child)
            os.write(master, b"\r")
            with sqlite3.connect(str(root / "engine.db")) as database:
                rows = database.execute("SELECT body FROM projects").fetchall()
                assert any("pty-project" in row[0] for row in rows), rows
                deadline = time.monotonic() + 5
                while not any("second-draft-unique" in row[0] for row in rows) and time.monotonic() < deadline:
                    time.sleep(0.02)
                    rows = database.execute("SELECT body FROM projects").fetchall()
                assert any("second-draft-unique" in row[0] for row in rows), ("actual second project missing", rows)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 8, 30, 0, 0))
            inspect_pty(child, master, slave, "kernel resize with automatic SIGWINCH only")
            expect(master, "Projects | Connected", child)
            os.write(master, b"\t" * 5)
            expect(master, "Evidence", child)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
            expect(master, "PgUp/PgDn", child)
            close_tui(child, master, slave, before)
            descriptors.clear()
            assert service.poll() is None, "closing TUI terminated the engine"

            child, master, slave, before = launch(binary, root / "engine.sock")
            children.append(child)
            descriptors.extend([master, slave])
            expect(master, "Connected", child)
            os.write(master, b"\t")
            expect(master, "pty-project", child)
            service.send_signal(signal.SIGINT)
            assert service.wait(timeout=5) == 0
            expect(master, "Disconnected", child)
            service = engine(binary, root)
            children.append(service)
            expect(master, "Connected", child)
            close_tui(child, master, slave, before, b"\x03")
            descriptors.clear()
            assert service.poll() is None
            for received in [signal.SIGTERM, signal.SIGINT, signal.SIGHUP]:
                child, master, slave, before = launch(binary, root / "engine.sock")
                children.append(child)
                descriptors.extend([master, slave])
                expect(master, "Connected", child)
                os.kill(child.pid, received)
                close_tui(child, master, slave, before, None)
                descriptors.clear()
                assert service.poll() is None
            print("PTY PASS: durable project, Unicode, resize, close/reopen, reconnect, Ctrl-C/Q and SIGTERM/INT/HUP restore")
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    child.kill()
                deadline = time.monotonic() + 5
                while child.poll() is None and time.monotonic() < deadline:
                    readable = select.select(descriptors[::2], [], [], 0.05)[0]
                    for descriptor in readable:
                        try:
                            os.read(descriptor, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                child.wait(timeout=5)
                if isinstance(child, PtyProcess):
                    child.release()
            for descriptor in descriptors:
                os.close(descriptor)
            if relay is not None:
                relay.close()


if __name__ == "__main__":
    if sys.argv[1] == "--session":
        supervise(sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5]))
    else:
        run(str(Path(sys.argv[1]).resolve()))
