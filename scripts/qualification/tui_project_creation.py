if not __debug__:
    raise SystemExit("TUI project creation qualification requires Python assertions enabled")

from contextlib import closing, contextmanager
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time

import tui_flow
import tui_project_conversation as conversation


NAME = " Declared 日本369 "
BRIEF = "First declared project brief 369 🦀"


def interrupted(signum, frame):
    raise SystemExit("project creation driver interrupted by signal %s" % signum)


def receive(peer, size):
    assert 0 < size <= 65536, size
    data = bytearray()
    while len(data) < size:
        part = peer.recv(size - len(data))
        assert part, ("actual engine closed a partial frame", len(data), size)
        data.extend(part)
    assert len(data) == size
    return bytes(data)


def request(path, identifier, command, revision="0"):
    payload = json.dumps({
        "protocol_version": 1,
        "client_id": "tui-creation-369-observer",
        "request_id": identifier,
        "expected_revision": revision,
        "command": command,
    }).encode()
    assert 0 < len(payload) <= 65536
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        peer.settimeout(2)
        peer.connect(str(path))
        peer.sendall(struct.pack("!I", len(payload)) + payload)
        size = struct.unpack("!I", receive(peer, 4))[0]
        reply = json.loads(receive(peer, size))
    assert reply["request_id"] == identifier, reply
    return reply


def seed_projects(path):
    initial = request(path, "initial-empty", {"kind": "Snapshot"})["outcome"]
    assert initial["kind"] == "Snapshot", initial
    assert initial["projects"] == [], initial
    assert initial["tasks"] == [], initial
    for index in range(17):
        reply = request(path, "setup-%02d" % index, {
            "kind": "CreateProject",
            "project_id": "existing-%02d" % index,
            "spec": {"name": "Existing %02d" % index, "repositories": []},
        }, str(index))
        assert reply["outcome"] == {"kind": "Applied"}, reply
        assert reply["revision"] == str(index + 1), reply


def read_rows(database, statement, parameters=()):
    assert database.is_absolute(), database
    assert len(statement) <= 2048
    with closing(sqlite3.connect(
        database.as_uri() + "?mode=ro", uri=True, timeout=0.25
    )) as reader:
        reader.execute("PRAGMA query_only=ON")
        return reader.execute(statement, parameters).fetchall()


def project_rows(database):
    rows = read_rows(database, "SELECT body FROM projects ORDER BY id LIMIT 129")
    assert len(rows) <= 128, len(rows)
    projects = [json.loads(row[0]) for row in rows]
    assert all("project_id" in project and "spec" in project for project in projects)
    return projects


def effect_counts(database):
    rows = read_rows(database, "SELECT "
        "(SELECT COUNT(*) FROM tasks), (SELECT COUNT(*) FROM configs), "
        "(SELECT COUNT(*) FROM repository_admissions), "
        "(SELECT COUNT(*) FROM worktree_leases), "
        "(SELECT COUNT(*) FROM settings_publication), "
        "(SELECT COUNT(*) FROM verification_baselines)")
    assert len(rows) == 1, rows
    assert len(rows[0]) == 6, rows
    return rows[0]


def owner_effects(database):
    rows = read_rows(database, "SELECT (SELECT COUNT(*) FROM events), "
        "(SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner')")
    assert len(rows) == 1, rows
    assert len(rows[0]) == 2, rows
    return rows[0]


@contextmanager
def engine(binary, root):
    child = tui_flow.engine(binary, root)
    assert child.pid > 0
    assert child.poll() is None
    print("owned engine started pid=%d" % child.pid, flush=True)
    try:
        yield child
    finally:
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
        try:
            status = child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
            raise AssertionError("owned engine required forced cleanup pid=%d" % child.pid)
        assert status == 0, ("owned engine shutdown", child.pid, status)
        print("owned engine reaped pid=%d" % child.pid, flush=True)


@contextmanager
def terminal(binary, path):
    child, master, slave, before = tui_flow.launch(binary, path)
    assert child.pid > 0
    assert master != slave
    print("owned TUI started pid=%d supervisor=%d" % (
        child.pid, child.supervisor.pid
    ), flush=True)
    closed = False
    try:
        yield child, master
        tui_flow.close_tui(child, master, slave, before)
        closed = True
    finally:
        try:
            if not closed:
                conversation.cleanup(child, master, slave)
        finally:
            tui_flow.outputs.pop(master, None)
        print("owned TUI reaped pid=%d supervisor=%d" % (
            child.pid, child.supervisor.pid
        ), flush=True)


def created_project(database, paths, child, master):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        matching = [project for project in project_rows(database)
                    if project["spec"]["name"] == NAME]
        if matching:
            assert len(matching) == 1, matching
            project = matching[0]
            assert project["spec"] == {"name": NAME, "repositories": paths}, project
            assert not project["project_id"].startswith("existing-"), project
            return project["project_id"]
        assert child.poll() is None, "TUI exited before declaration persisted"
        conversation.drain(master)
    raise AssertionError("N input did not persist the exact declared project: %r" % (
        project_rows(database),
    ))


def wait_brief(database, project, child, master):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        rows = conversation.actual_rows(database, project)
        if rows:
            assert rows == [("brief", 1, BRIEF)], rows
            return
        assert child.poll() is None, "TUI exited before first brief persisted"
        conversation.drain(master)
    raise AssertionError("explicit first brief did not persist for %s" % project)


def declare_and_write_brief(binary, root, paths):
    database = root / "engine.db"
    with terminal(binary, root / "engine.sock") as (child, master):
        tui_flow.expect(master, "Owner: connected", child)
        conversation.body_contains(master, child, "Existing 00")
        assert len(project_rows(database)) == 17
        assert not any(project["spec"]["name"] == NAME for project in project_rows(database))
        print("stage=N literal repository input", flush=True)
        os.write(master, b"\tN" + NAME.encode() + b"\t" + paths[0].encode())
        tui_flow.expect(master, paths[0], child)
        os.write(master, b"\x0e" + paths[1].encode())
        tui_flow.expect(master, paths[1], child)
        os.write(master, b"\r")
        project = created_project(database, paths, child, master)
        assert len(project_rows(database)) == 18
        assert conversation.actual_rows(database, project) == []
        conversation.body_contains(master, child, "Project owner conversation")
        conversation.body_contains(master, child, "No project brief recorded")
        title = conversation.screen(master).splitlines()[3]
        assert NAME in title, ("created ID was not selected", project, title)
        print("stage=explicit brief version zero project=%s" % project, flush=True)
        os.write(master, b"\x02")
        tui_flow.expect(master, "Brief edit | expected version 0", child)
        os.write(master, BRIEF.encode() + b"\r")
        wait_brief(database, project, child, master)
        conversation.body_contains(master, child, BRIEF)
        return project


def reopen(binary, root, project, paths):
    snapshot = request(root / "engine.sock", "reopened-snapshot", {"kind": "Snapshot"})
    assert snapshot["outcome"]["tasks"] == [], snapshot
    projects = snapshot["outcome"]["projects"]
    assert len(projects) == 18, projects
    target = [row for row in projects if row["project_id"] == project]
    assert target == [{"project_id": project,
                       "spec": {"name": NAME, "repositories": paths}}], target
    index = [row["project_id"] for row in projects].index(project)
    assert 0 <= index < 18, index
    assert index >= 16, index
    with terminal(binary, root / "engine.sock") as (child, master):
        tui_flow.expect(master, "Owner: connected", child)
        conversation.body_contains(master, child, "History page:")
        os.write(master, b"\t")
        os.write(master, b"\x1b[B" * index)
        os.write(master, b"\x1b[6~" * 3)
        conversation.body_contains(master, child, "> %s [%s]" % (NAME, project))
        conversation.body_contains(master, child, "Declared repositories: 2")
        conversation.body_contains(master, child, "Repositories recorded in project metadata")
        conversation.body_contains(master, child, paths[0])
        conversation.body_contains(master, child, paths[1])
        os.write(master, b"\x1b[Z")
        conversation.body_contains(master, child, "History page:")
        conversation.body_contains(master, child, BRIEF)
        title = conversation.screen(master).splitlines()[3]
        assert NAME in title, ("reopened wrong project", project, title)
        assert conversation.actual_rows(root / "engine.db", project) == [("brief", 1, BRIEF)]


def run(binary):
    with tempfile.TemporaryDirectory(prefix="ca-tui369-", dir="/private/tmp") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        paths = [str(root / "declared frontend"), str(root / "declared backend, literal")]
        assert not Path(paths[0]).exists()
        assert not Path(paths[1]).exists()
        with engine(binary, root):
            seed_projects(root / "engine.sock")
            before = effect_counts(root / "engine.db")
            assert before[0] == 0, before
            assert owner_effects(root / "engine.db") == (17, 0)
            project = declare_and_write_brief(binary, root, paths)
            assert effect_counts(root / "engine.db") == before
            assert owner_effects(root / "engine.db") == (19, 2)
        print("stage=restart and independent readback", flush=True)
        with engine(binary, root):
            reopen(binary, root, project, paths)
            assert effect_counts(root / "engine.db") == before
            assert owner_effects(root / "engine.db") == (19, 2)
        assert not Path(paths[0]).exists()
        assert not Path(paths[1]).exists()
        print("PTY PASS: exact declared project, first brief, restart, no admission/task effects")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: tui_project_creation.py BINARY")
    for received in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
        signal.signal(received, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 25)
    try:
        run(str(Path(sys.argv[1]).resolve()))
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
