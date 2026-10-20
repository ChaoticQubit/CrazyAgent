if not __debug__:
    raise SystemExit("Master TUI qualification requires Python assertions enabled")

from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import struct
import sys
import termios
import time

import tui_flow
import tui_project_conversation as conversation
import tui_project_creation as creation


NOTE = "Owner note before explicit Open 686"
BRIEF = "Explicit owner brief before master 686"
REPLACEMENT = "Explicit replacement while master configured 686"
TURN = "Literal master turn 686 🦀"
NEWER = TURN + "-newer-nN]"
DRAFT = "master setup retained 686"
FIELDS = (
    "slots", "cpu_millis", "memory_mib", "processes", "output_bytes",
    "wall_time_ms", "spend_microunits",
)
DEFAULTS = (1, 1000, 512, 4, 1048576, 60000, 0)
CAPACITY = (2, 2000, 1024, 8, 2097152, 120000, 10)
ALLOWANCE = (2, 900, 384, 3, 524288, 45000, 1)
F2 = b"\x1bOQ"
F3 = b"\x1bOR"
F4 = b"\x1bOS"

CATALOG = """
version = 1
timeout_ms = 2000
allowed_destinations = ["sandbox-profile"]
[routing]
approved_accounts = ["api-account", "codex-account", "claude-account"]
global_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
project_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
task_capacity = { slots=2, cpu_millis=2000, memory_mib=1024, processes=8, output_bytes=2097152, wall_time_ms=120000, spend_microunits=10 }
[model_access]
accounts = ["api-account", "codex-account", "claude-account", "unapproved-account"]
[[model_access.connections]]
id = "api-connection"
provider = "configured-api"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "api-account"
endpoint = { kind="http", url="https://model.example.invalid/v1" }
model = { id="configured-model", author="configured-author" }
auth = { method="none" }
[[model_access.connections]]
id = "codex-connection"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "codex-account"
endpoint = { kind="native" }
model = { id="configured-codex-model", author="openai" }
auth = { method="native_managed", profile_ref="codex-credential-profile" }
[[model_access.connections]]
id = "claude-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "claude-account"
endpoint = { kind="native" }
model = { id="configured-claude-model", author="anthropic" }
auth = { method="native_managed", profile_ref="claude-credential-profile" }
[[model_access.connections]]
id = "unapproved-connection"
provider = "configured-api"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "unapproved-account"
endpoint = { kind="http", url="https://unapproved.example.invalid/v1" }
model = { id="unapproved-model", author="configured-author" }
auth = { method="none" }
[[model_access.pools]]
id = "master-pool"
members = ["api-connection", "codex-connection", "claude-connection", "unapproved-connection"]
projects = ["p", "q"]
roles = ["project_leader"]
fallback_allowed = false
[[model_access.pools]]
id = "worker-only-pool"
members = ["api-connection"]
projects = ["p", "q"]
roles = ["worker"]
fallback_allowed = false
"""


def interrupted(signum, frame):
    raise SystemExit("master686 interrupted by signal %s" % signum)


@contextmanager
def terminal(binary, path):
    with creation.terminal(binary, path) as (child, master):
        group = os.getpgid(child.pid)
        assert group == child.supervisor.pid and group > 1
        assert os.getsid(child.pid) == group
        print("owned-group=%d" % group, flush=True)
        try:
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 42, 160, 0, 0))
            os.kill(child.pid, signal.SIGWINCH)
            tui_flow.expect(master, "Owner: connected", child)
            yield child, master
        finally:
            print("closing-group=%d" % group, flush=True)
    assert child.poll() == 0
    assert child.supervisor.poll() == 0
    print("reaped-group=%d" % group, flush=True)


def sessions(database):
    result = creation.read_rows(database, "SELECT body FROM master_sessions ORDER BY created LIMIT 4")
    assert len(result) <= 3, result
    return [json.loads(row[0]) for row in result]


def turns(database):
    result = creation.read_rows(database, "SELECT body FROM master_turns ORDER BY ordinal LIMIT 2")
    assert len(result) <= 1, result
    return [json.loads(row[0]) for row in result]


def counts(database):
    rows = creation.read_rows(database, "SELECT "
        "(SELECT COUNT(*) FROM master_sessions),(SELECT COUNT(*) FROM master_turns),"
        "(SELECT COUNT(*) FROM owner_operations),(SELECT COUNT(*) FROM events)")
    assert len(rows) == 1
    assert len(rows[0]) == 4
    return rows[0]


def wait_session(database, child, master, count, backend, phase):
    assert 1 <= count <= 3
    assert phase in ("configured", "closed")
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        found = sessions(database)
        if len(found) == count and found[-1]["phase"] == phase:
            session = found[-1]
            assert session["project_id"] == "p", session
            assert session["selection"]["backend"] == backend, session
            tui_flow.expect(master, "Master: " + phase.capitalize(), child)
            tui_flow.expect(master, session["session_id"], child)
            return session
        assert child.poll() is None, "TUI exited before actual session transition"
        conversation.drain(master)
    raise AssertionError(("actual master session mismatch", count, backend, phase, sessions(database)))


def wait_turn(database, child, master, phase):
    assert phase in ("queued", "cancelled")
    assert child.poll() is None
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        found = turns(database)
        if len(found) == 1 and found[0]["phase"] == phase:
            turn = found[0]
            assert turn["attempt"] is None and turn["runtime"] is None and turn["reply"] is None
            tui_flow.expect(master, phase.capitalize(), child)
            tui_flow.expect(master, turn["id"], child)
            tui_flow.expect(master, str(turn["input_cursor"]), child)
            return turn
        assert child.poll() is None, "TUI exited before actual turn transition"
        conversation.drain(master)
    raise AssertionError(("actual master turn mismatch", phase, turns(database)))


def setup(master, child, backend, field=0):
    assert 0 <= field < 11
    assert backend in ("RCLI", "Native Codex", "Native Claude")
    os.write(master, F2)
    tui_flow.expect(master, "Backend: " + backend, child)
    if field:
        os.write(master, b"\t" * field)


def replace_number(master, child, field, value):
    assert field in FIELDS
    assert isinstance(value, int) and 0 <= value <= 2097153
    os.write(master, b"\x7f" * 24 + str(value).encode())
    tui_flow.expect(master, "%s: %d" % (field, value), child)


def close_setup(master, child, draft):
    assert draft
    assert child.poll() is None
    os.write(master, b"\x1b")
    conversation.draft_contains(master, child, draft)


def truthful(master):
    text = conversation.screen(master)
    assert not re.search(r"Master: Ready|Turn: (Succeeded|Running|Delivered)|Assistant:", text), text
    assert "Execution sandbox: codex-credential-profile" not in text
    assert "Execution sandbox: claude-credential-profile" not in text


def seed(binary, root):
    database = root / "engine.db"
    with creation.engine(binary, root, writable=True):
        for index, name in enumerate(("p", "q")):
            reply = creation.request(root / "engine.sock", "create-master-686-" + name, {
                "kind": "CreateProject", "project_id": name,
                "spec": {"name": "Project %s 686" % name.upper(), "repositories": []},
            }, str(index))
            assert reply["outcome"] == {"kind": "Applied"}, reply
        with terminal(binary, root / "engine.sock") as (child, master):
            conversation.body_contains(master, child, "Project owner conversation")
            os.write(master, NOTE.encode() + b"\r")
            conversation.committed(master, child, database, "p", "message", 0, NOTE)
            os.write(master, b"\x02")
            tui_flow.expect(master, "Brief edit | expected version 0", child)
            os.write(master, BRIEF.encode() + b"\r")
            conversation.committed(master, child, database, "p", "brief", 1, BRIEF)
            os.write(master, b"\x1b")
            os.write(master, DRAFT.encode())
            conversation.draft_contains(master, child, DRAFT)
            before = counts(database)
            assert before[:2] == (0, 0), before
            setup(master, child, "RCLI")
            tui_flow.expect(master, "Settings", child)
            os.write(master, b"\r")
            close_setup(master, child, DRAFT)
            assert counts(database) == before, ("missing metadata caused an effect", counts(database), before)
            assert conversation.actual_rows(database, "p") == [("message", 0, NOTE), ("brief", 1, BRIEF)]
            truthful(master)


def edit_allowance(database, child, master):
    setup(master, child, "RCLI")
    for field, value in zip(FIELDS, DEFAULTS):
        tui_flow.expect(master, "%s: %d" % (field, value), child)
    for label in ("Pool: master-pool", "Account: api-account", "Execution sandbox: sandbox-profile"):
        tui_flow.expect(master, label, child)
    os.write(master, b"\t\x1b[B")
    tui_flow.expect(master, "Pool: master-pool", child)
    os.write(master, b"\t\x1b[B")
    tui_flow.expect(master, "Account: api-account", child)
    os.write(master, b"\t\x1b[B")
    tui_flow.expect(master, "Execution sandbox: sandbox-profile", child)
    close_setup(master, child, DRAFT)
    before = counts(database)
    for index, (field, cap, value) in enumerate(zip(FIELDS, CAPACITY, ALLOWANCE)):
        setup(master, child, "RCLI", 4 + index)
        replace_number(master, child, field, cap + 1)
        os.write(master, b"\r")
        close_setup(master, child, DRAFT)
        assert counts(database) == before, ("invalid allowance admitted", field, counts(database), before)
        setup(master, child, "RCLI", 4 + index)
        tui_flow.expect(master, "%s: %d" % (field, cap + 1), child)
        replace_number(master, child, field, value)
        close_setup(master, child, DRAFT)
    setup(master, child, "RCLI")
    for field, value in zip(FIELDS, ALLOWANCE):
        tui_flow.expect(master, "%s: %d" % (field, value), child)
    os.write(master, b"\r")
    session = wait_session(database, child, master, 1, "rcli", "configured")
    assert session["selection"]["route"]["resources"] == dict(zip(FIELDS, ALLOWANCE)), session
    assert session["selection"]["pool_ref"] == "master-pool"
    assert session["selection"]["route"]["account_ref"] == "api-account"
    assert session["selection"]["target_profile_ref"] == "sandbox-profile"
    assert session["selection"]["route"]["required_target"] == "sandbox-profile"
    assert session["current_turn"] is None and turns(database) == []
    assert counts(database) == (1, 0, before[2] + 1, before[3] + 1)
    conversation.draft_contains(master, child, DRAFT)
    truthful(master)
    return session


def ambiguous_submit(root, session, child, master):
    database = root / "engine.db"
    os.write(master, b"\x7f" * len(DRAFT))
    with sqlite3.connect(str(database), timeout=0.25) as blocker:
        blocker.execute("BEGIN IMMEDIATE")
        os.write(master, TURN.encode() + b"\r")
        tui_flow.expect(master, "Owner outcome unknown", child)
        match = re.search(r"Owner operation\s+([0-9a-f]{64})\s+unresolved", conversation.screen(master))
        assert match is not None, ("real retained master operation absent", conversation.screen(master))
        operation_id = match.group(1)
        assert turns(database) == []
        assert all(row[2] != TURN for row in conversation.actual_rows(database, "p"))
        os.write(master, b"-newer-nN]")
        conversation.draft_contains(master, child, NEWER)
        conversation.select_other(master, child, "Project Q 686", True)
        os.write(master, b"Other project draft 686")
        conversation.draft_contains(master, child, "Other project draft 686", NEWER)
        conversation.select_other(master, child, "Project P 686", False)
        conversation.draft_contains(master, child, NEWER, "Other project draft 686")
        blocker.rollback()
    os.write(master, b"\x12")
    tui_flow.expect(master, "Original owner operation not recorded at actual barrier", child)
    conversation.draft_contains(master, child, NEWER)
    os.write(master, b"\x12")
    turn = wait_turn(database, child, master, "queued")
    assert turn["id"] == operation_id and turn["session_id"] == session["session_id"], turn
    assert turn["reason"] is None and turn["ordinal"] == 1, turn
    conversation.draft_contains(master, child, NEWER)
    rows = creation.read_rows(database,
        "SELECT o.operation_id,lower(hex(o.request_hash)),o.event_cursor,o.result,"
        "o.revision_before,o.revision_after,c.text,t.input FROM owner_operations o "
        "JOIN conversation_entries c ON c.event_cursor=o.event_cursor "
        "JOIN master_turns t ON t.id=o.operation_id AND t.input=c.event_cursor "
        "WHERE o.origin='native_owner' AND c.project=? AND c.text=? LIMIT 2", ("p", TURN))
    assert len(rows) == 1, rows
    identifier, request_hash, cursor, result, before, after, text, input_cursor = rows[0]
    assert identifier == operation_id and re.fullmatch(r"[0-9a-f]{64}", request_hash)
    assert json.loads(result) == {"kind": "Applied"}, rows
    assert before + 1 == after == cursor == input_cursor == int(turn["input_cursor"]), rows
    assert text == TURN and text != NEWER
    receipt = {"operation_id": identifier, "request_hash": request_hash,
               "input_cursor": input_cursor, "observed_stage": "not_recorded_then_exact_retry"}
    path = root / "actual-contention-operation.json"
    with path.open("x") as output:
        path.chmod(0o600)
        json.dump(receipt, output)
    assert conversation.actual_rows(database, "q") == []
    truthful(master)
    return turn


def brief_cancel_close(database, session, turn, child, master):
    conversation.explicit_edit(master, child, BRIEF, REPLACEMENT)
    os.write(master, b"\r")
    conversation.committed(master, child, database, "p", "brief", 2, REPLACEMENT)
    os.write(master, b"\x1b")
    conversation.draft_contains(master, child, NEWER)
    assert turns(database) == [turn], turns(database)
    os.write(master, b"\x15")
    os.write(master, b"Unassigned draft 686")
    conversation.draft_contains(master, child, "Unassigned draft 686", NEWER)
    os.write(master, b"\x15")
    conversation.draft_contains(master, child, NEWER, "Unassigned draft 686")
    os.write(master, b"\x0e")
    tui_flow.expect(master, "No further", child)
    os.write(master, b"\x10")
    conversation.body_contains(master, child, NOTE)
    before = counts(database)
    os.write(master, F3)
    cancelled = wait_turn(database, child, master, "cancelled")
    assert cancelled["id"] == turn["id"] and cancelled["reason"] == "cancelled", cancelled
    assert counts(database) == (1, 1, before[2] + 1, before[3] + 1)
    conversation.draft_contains(master, child, NEWER)
    os.write(master, F4)
    closed = wait_session(database, child, master, 1, "rcli", "closed")
    assert closed["session_id"] == session["session_id"]
    assert closed["current_turn"] is None
    assert counts(database) == (1, 1, before[2] + 2, before[3] + 2)
    conversation.draft_contains(master, child, NEWER)
    truthful(master)


def native_metadata(database, child, master):
    previous_label = "RCLI"
    for count, label, backend, account in (
        (2, "Native Codex", "codex", "codex-account"),
        (3, "Native Claude", "claude", "claude-account"),
    ):
        setup(master, child, previous_label)
        os.write(master, b"\x1b[B")
        tui_flow.expect(master, "Backend: " + label, child)
        tui_flow.expect(master, "Account: " + account, child)
        tui_flow.expect(master, "Execution sandbox: sandbox-profile", child)
        close_setup(master, child, NEWER)
        setup(master, child, label)
        for field, value in zip(FIELDS, ALLOWANCE):
            tui_flow.expect(master, "%s: %d" % (field, value), child)
        os.write(master, b"\r")
        session = wait_session(database, child, master, count, backend, "configured")
        assert session["selection"]["route"]["account_ref"] == account
        assert session["selection"]["target_profile_ref"] == "sandbox-profile"
        assert session["current_turn"] is None and len(turns(database)) == 1
        truthful(master)
        os.write(master, F4)
        closed = wait_session(database, child, master, count, backend, "closed")
        assert closed["session_id"] == session["session_id"]
        conversation.draft_contains(master, child, NEWER)
        previous_label = label


def run(binary, root):
    assert root.is_absolute() and root.is_dir()
    assert root.stat().st_mode & 0o777 == 0o700
    old_mask = os.umask(0o077)
    try:
        seed(binary, root)
        catalog = root / "global.toml"
        catalog.write_text(CATALOG)
        catalog.chmod(0o600)
        assert catalog.stat().st_mode & 0o777 == 0o600
        database = root / "engine.db"
        with creation.engine(binary, root, writable=True) as service:
            before_effects = creation.effect_counts(database)
            assert before_effects[0] == 0 and before_effects[2:] == (0, 0, 0, 0)
            with terminal(binary, root / "engine.sock") as (child, master):
                conversation.body_contains(master, child, NOTE)
                assert sessions(database) == [] and turns(database) == []
                os.write(master, DRAFT.encode())
                conversation.draft_contains(master, child, DRAFT)
                print("stage=seven-dimension setup and real Open", flush=True)
                session = edit_allowance(database, child, master)
                print("stage=real ambiguous Submit and exact reconcile", flush=True)
                turn = ambiguous_submit(root, session, child, master)
                brief_cancel_close(database, session, turn, child, master)
                print("stage=native metadata only", flush=True)
                native_metadata(database, child, master)
                before_sessions, before_turns, before_counts = sessions(database), turns(database), counts(database)
                assert len(before_sessions) == 3 and len(before_turns) == 1
                print("stage=actual engine restart, retained client draft", flush=True)
                service.send_signal(signal.SIGINT)
                assert service.wait(timeout=5) == 0
                tui_flow.expect(master, "Disconnected", child)
                with creation.engine(binary, root, writable=True):
                    os.write(master, b"\x12")
                    tui_flow.expect(master, "Owner: connected", child)
                    tui_flow.expect(master, "Master: Closed", child)
                    tui_flow.expect(master, before_sessions[-1]["session_id"], child)
                    conversation.draft_contains(master, child, NEWER)
                    assert sessions(database) == before_sessions and turns(database) == before_turns
                    assert counts(database) == before_counts
                    assert conversation.actual_rows(database, "p") == [
                        ("message", 0, NOTE), ("brief", 1, BRIEF),
                        ("message", 0, TURN), ("brief", 2, REPLACEMENT),
                    ]
                    assert conversation.actual_rows(database, "q") == []
                    assert creation.effect_counts(database) == before_effects
                    truthful(master)
        print("PTY PASS master686: actual bootstrap only, three metadata configurations, one owner input/turn", flush=True)
    finally:
        os.umask(old_mask)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: tui_project_master.py BINARY PRIVATE_ROOT")
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
        signal.signal(handled, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 85)
    try:
        run(str(Path(sys.argv[1]).resolve()), Path(sys.argv[2]).resolve())
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
