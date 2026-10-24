import errno
import fcntl
import json
import os
import re
import select
import signal
import sqlite3
import struct
import sys
import time
from pathlib import Path

import tui_flow


def interrupted(signum, frame):
    raise SystemExit("project conversation driver interrupted by signal %s" % signum)


def drain(master):
    if select.select([master], [], [], 0.05)[0]:
        try:
            part = os.read(master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            part = b""
        tui_flow.outputs[master].extend(part)
        assert len(tui_flow.outputs[master]) <= 1048576, "PTY output exceeded one MiB"


def screen(master):
    height, width, _, _ = struct.unpack(
        "HHHH", fcntl.ioctl(master, tui_flow.termios.TIOCGWINSZ, bytes(8))
    )
    return tui_flow.screen_text(tui_flow.outputs[master], width, height)


def actual_rows(database, project):
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=0.25) as reader:
        reader.execute("PRAGMA query_only=ON")
        return reader.execute(
            "SELECT kind,brief_version,text FROM conversation_entries "
            "WHERE project=? ORDER BY event_cursor LIMIT 33",
            (project,),
        ).fetchall()


def committed(master, child, database, project, kind, version, text):
    deadline = time.monotonic() + 8
    rows = []
    notices = []
    while time.monotonic() < deadline:
        rows = actual_rows(database, project)
        if (kind, version, text) in rows:
            return
        if child.poll() is not None:
            break
        drain(master)
        notice = screen(master).splitlines()[-2].strip()
        if notice not in notices and len(notices) < 16:
            notices.append(notice)
    raise AssertionError(
        "actual TUI owner %s not committed: expected_version=%r text=%r "
        "rows=%r status=%r notices=%r screen=%r"
        % (kind, version, text, rows, child.poll(), notices, screen(master))
    )


def body_contains(master, child, marker):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        rendered = screen(master)
        body = "\n".join(rendered.splitlines()[3:-7])
        if marker in body:
            return
        if child.poll() is not None:
            break
        drain(master)
    raise AssertionError(
        "actual durable conversation content absent from rendered body: "
        "marker=%r status=%r screen=%r"
        % (marker, child.poll(), screen(master))
    )


def explicit_edit(master, child, initial, marker):
    os.write(master, b"\x02")
    tui_flow.expect(master, "Brief edit | expected version 1", child)
    os.write(master, b"\x7f" * len(initial))
    os.write(master, marker.encode())
    tui_flow.expect(master, marker, child)


def project_input(master, child):
    deadline = time.monotonic() + 6
    label = ""
    while time.monotonic() < deadline:
        label = screen(master).splitlines()[-7]
        if "Owner message | " in label and "unassigned retained: 0 bytes" in label:
            return
        assert child.poll() is None, "TUI exited while waiting for the project input"
        drain(master)
    raise AssertionError("project input not ready: label=%r screen=%r" % (
        label, screen(master)))


def draft_contains(master, child, marker, absent=None):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        draft = screen(master).splitlines()[-6]
        if marker in draft and (absent is None or absent not in draft):
            return
        assert child.poll() is None, "TUI exited while checking retained draft"
        drain(master)
    raise AssertionError("project draft mismatch: expected=%r absent=%r screen=%r" % (
        marker, absent, screen(master)))


def select_other(master, child, expected, forward):
    os.write(master, b"\t" + (b"\x1b[B" if forward else b"\x1b[A") + b"\x1b[Z")
    tui_flow.expect(master, "Conversation", child)
    tui_flow.expect(master, expected, child)


def isolated_drafts(master, child, database, project, initial, marker):
    other = "b-isolated-project"
    first = marker + "-A"
    second = marker + "-B"
    os.write(master, first.encode())
    draft_contains(master, child, first)
    explicit_edit(master, child, initial, marker + "-brief-A")
    os.write(master, b"\x1b")
    draft_contains(master, child, first)
    select_other(master, child, other, True)
    os.write(master, second.encode())
    draft_contains(master, child, second, first)
    os.write(master, b"\x02")
    tui_flow.expect(master, "Brief edit | expected version 0", child)
    os.write(master, (marker + "-brief-B").encode())
    os.write(master, b"\x1b")
    draft_contains(master, child, second)
    select_other(master, child, project, False)
    draft_contains(master, child, first, second)
    os.write(master, b"\x02")
    tui_flow.expect(master, "Brief edit | expected version 1", child)
    draft_contains(master, child, marker + "-brief-A", marker + "-brief-B")
    os.write(master, b"\x1b")
    draft_contains(master, child, first)
    body_contains(master, child, "No master configured")
    os.write(master, b"\r")
    committed(master, child, database, project, "message", 0, first)
    select_other(master, child, other, True)
    draft_contains(master, child, second, first)
    body_contains(master, child, "No master configured")
    os.write(master, b"\r")
    committed(master, child, database, other, "message", 0, second)
    os.write(master, b"\x02")
    tui_flow.expect(master, "Brief edit | expected version 0", child)
    draft_contains(master, child, marker + "-brief-B", marker + "-brief-A")
    os.write(master, b"\x1b")


def actual_contention(master, child, database, project, marker):
    body_contains(master, child, "Retained owner brief version one")
    with sqlite3.connect(str(database), timeout=0.25) as blocker:
        blocker.execute("BEGIN IMMEDIATE")
        os.write(master, marker.encode() + b"\r")
        tui_flow.expect(master, "Owner outcome unknown", child)
        rendered = screen(master)
        matched = re.search(r"Owner operation\s+([0-9a-f]{64})\s+unresolved", rendered)
        assert matched is not None, ("actual retained operation ID absent", rendered)
        operation_id = matched.group(1)
        assert all(row[2] != marker for row in actual_rows(database, project))
        os.write(master, b"-newer")
        draft_contains(master, child, marker + "-newer")
        blocker.rollback()
    os.write(master, b"\x12")
    tui_flow.expect(master, "Original owner operation not recorded at actual barrier", child)
    draft_contains(master, child, marker + "-newer")
    os.write(master, b"\x12")
    committed(master, child, database, project, "message", 0, marker)
    tui_flow.expect(master, "Owner input durably recorded", child)
    draft_contains(master, child, marker + "-newer")
    body_contains(master, child, marker)
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as observer:
        rows = observer.execute(
            "SELECT o.operation_id,hex(o.request_hash),o.event_cursor "
            "FROM owner_operations o JOIN conversation_entries c "
            "ON c.event_cursor=o.event_cursor WHERE c.project=? AND c.text=?",
            (project, marker),
        ).fetchall()
        assert len(rows) == 1 and rows[0][0] == operation_id, rows
        assert len(rows[0][1]) == 64 and rows[0][2] > 0, rows
    (database.parent / "actual-contention-operation.json").write_text(json.dumps({
        "operation_id": operation_id, "request_hash": rows[0][1].lower(),
        "event_cursor": rows[0][2], "observed_stage": "not_recorded_then_exact_retry",
    }))


def paged_history(master, child, marker):
    tui_flow.expect(master, "History page: 32 actual entries", child)
    os.write(master, b"\x0e")
    tui_flow.expect(master, "History page: 2 actual entries", child)
    body_contains(master, child, "page-32-" + marker)
    body_contains(master, child, "[2J")
    os.write(master, b"\x10")
    tui_flow.expect(master, "History page: 32 actual entries", child)
    body_contains(master, child, "page-00-" + marker)


def conflict(master, child, database, project, marker):
    ready = database.parent / "brief-edit-ready"
    receipt = database.parent / "external-brief-receipt.json"
    ready.write_text(str(child.pid))
    deadline = time.monotonic() + 8
    while not receipt.exists():
        assert child.poll() is None, "TUI exited during external real owner edit"
        assert time.monotonic() < deadline, "independent actual owner edit deadline"
        drain(master)
    operation = json.loads(receipt.read_text())
    assert operation["result"]["kind"] == "Applied", operation
    assert operation["provenance"]["event_cursor"] is not None, operation
    os.write(master, b"\r")
    tui_flow.expect(master, "stale_brief_version", child)
    assert marker in screen(master), "stale expected-version draft was discarded"
    rows = actual_rows(database, project)
    assert len(rows) == 2, rows
    assert all(row[2] != marker for row in rows), rows


def cleanup(child, master, slave):
    try:
        if child.poll() is None:
            child.kill()
        deadline = time.monotonic() + 5
        while child.poll() is None and time.monotonic() < deadline:
            drain(master)
        child.wait(timeout=5)
    finally:
        try:
            child.release()
        finally:
            for descriptor in (master, slave):
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF:
                        raise


def run(case, binary, socket, database, project, initial, marker):
    child, master, slave, before = tui_flow.launch(binary, socket)
    closed = False
    try:
        tui_flow.expect(master, "Connected", child)
        tui_flow.expect(master, "Owner: connected", child)
        tui_flow.inspect_pty(child, master, slave, "project conversation " + case)
        tui_flow.expect(master, project, child)
        rows = actual_rows(database, project)
        if case == "pages":
            assert len(rows) == 33 and rows[0] == ("brief", 1, initial), rows
            paged_history(master, child, marker)
        elif case == "isolation":
            assert rows == [("brief", 1, initial)], rows
            isolated_drafts(master, child, database, project, initial, marker)
        elif case == "contention":
            assert rows == [("brief", 1, initial)], rows
            actual_contention(master, child, database, project, marker)
        elif case == "message":
            assert rows == [("brief", 1, initial)], rows
            os.write(master, marker.encode() + b"\r")
            committed(master, child, database, project, "message", 0, marker)
            assert actual_rows(database, project) == [
                ("brief", 1, initial), ("message", 0, marker)
            ]
            body_contains(master, child, marker)
        else:
            assert rows == [("brief", 1, initial)], rows
            explicit_edit(master, child, initial, marker)
            if case == "conflict":
                conflict(master, child, database, project, marker)
            else:
                os.write(master, b"\r")
                committed(master, child, database, project, "brief", 2, marker)
                assert actual_rows(database, project) == [
                    ("brief", 1, initial), ("brief", 2, marker)
                ]
                body_contains(master, child, marker)
        tui_flow.close_tui(child, master, slave, before)
        closed = True
    finally:
        try:
            if not closed:
                cleanup(child, master, slave)
        finally:
            tui_flow.outputs.pop(master, None)


if __name__ == "__main__":
    if len(sys.argv) != 8 or sys.argv[1] not in (
        "message", "brief", "conflict", "isolation", "contention", "pages"
    ):
        raise SystemExit(
            "usage: tui_project_conversation.py CASE BINARY SOCKET DATABASE PROJECT INITIAL MARKER"
        )
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(handled, interrupted)
    run(
        sys.argv[1],
        str(Path(sys.argv[2]).resolve()),
        Path(sys.argv[3]),
        Path(sys.argv[4]).resolve(),
        *sys.argv[5:]
    )

