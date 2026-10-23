if not __debug__:
    raise SystemExit("TUI dispatch qualification requires Python assertions enabled")

import fcntl
import json
import os
from pathlib import Path
import signal
import struct
import sys
import termios
import time

import tui_flow
import tui_project_conversation as conversation


def interrupted(signum, frame):
    raise SystemExit("dispatch PTY deadline or interruption: %s" % signum)


def body(master):
    return "\n".join(conversation.screen(master).splitlines()[3:-7])


def wait_frame(child, master, required, forbidden=(), fresh_after=None):
    deadline = time.monotonic() + 6
    rendered = ""
    while time.monotonic() < deadline:
        rendered = body(master)
        fresh = fresh_after is None or len(tui_flow.outputs[master]) > fresh_after
        if fresh and all(text in rendered for text in required):
            assert all(text not in rendered for text in forbidden), rendered
            return rendered
        assert child.poll() is None, ("actual TUI exited", child.poll(), rendered)
        conversation.drain(master)
    raise AssertionError(("actual dispatch screen missing", required, rendered))


def choose_project(child, master, project, name):
    marker = "> %s [%s]" % (name, project)
    for unused in range(2):
        before = len(tui_flow.outputs[master])
        os.write(master, b"\x1b[B")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            conversation.drain(master)
            assert child.poll() is None, "TUI exited during project selection"
            if len(tui_flow.outputs[master]) > before and marker in body(master):
                return
    raise AssertionError(("explicit project selection unavailable", marker, body(master)))


def choose_task(child, master, task, other):
    for unused in range(2):
        before = len(tui_flow.outputs[master])
        os.write(master, b"]")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            conversation.drain(master)
            assert child.poll() is None, "TUI exited during task selection"
            rendered = body(master)
            if len(tui_flow.outputs[master]) > before and "Task: %s" % task in rendered:
                assert "Task: %s" % other not in rendered, rendered
                return
    raise AssertionError(("explicit task selection unavailable", task, body(master)))


def denied(child, master, expected):
    dispatch = expected["dispatch"]
    receipt = dispatch["receipt"]
    assert dispatch["state"] == "delivered_denied", dispatch
    assert receipt["reason"] == "native_runtime_admission_unavailable", receipt
    assert receipt["native_identity"] is None, receipt
    assert dispatch["attempt"]["item_id"] == "worker-z", dispatch
    assert receipt["dispatch_id"] == dispatch["dispatch_id"], receipt
    assert receipt["attempt"] == dispatch["attempt"], receipt
    assert receipt["journal_sequence"] > 0, receipt
    required = (
        "Worker dispatch readback",
        "Task: %s | Plan: %s" % (expected["task_id"], expected["plan_id"]),
        "Item: worker-z | reservation %s | Current" % dispatch["attempt"]["reservation_cursor"],
        "Delivered, denied: native runtime admission unavailable",
        "Dispatch: %s" % dispatch["dispatch_id"],
        "Last protected read: owner revision %s" % expected["read"]["revision"],
        "Delivery generation: %s" % dispatch["delivery_generation"],
        "Retained denial: native_runtime_admission_unavailable | journal %s" % receipt["journal_sequence"],
        "No provider or native execution is confirmed by this readback.",
    )
    return wait_frame(child, master, required, (
        "Item: worker-a | reservation", "Task: %s | Plan:" % expected["other_task"],
        "Not found: no recorded dispatch", "Cancellation requested; completion is not confirmed",
    ))


def inspection(child, master, expected):
    tui_flow.expect(master, "Owner: connected", child)
    os.write(master, b"\t")
    wait_frame(child, master, ("Projects |", "Dispatch target [project-z]"))
    choose_project(child, master, expected["project_id"], expected["project_name"])
    choose_task(child, master, expected["task_id"], expected["other_task"])
    os.write(master, b"\t\t")
    denied(child, master, expected)
    os.write(master, b"\t")
    denied(child, master, expected)
    wait_frame(child, master, (
        "Supervision", "Child status, pause/resume/cancel and native terminal attachment are unavailable:",
    ))
    os.write(master, b"\x1b[Z\x1b[Z\x1b[Z")
    wait_frame(child, master, ("Projects |",))
    choose_project(child, master, expected["other_project"], "Other project")
    os.write(master, b"\t\t")
    wait_frame(child, master, ("No current selected task plan; no dispatch status is inferred.",), (
        "Dispatch: %s" % expected["dispatch"]["dispatch_id"], "Delivered, denied:",
    ))
    before = len(tui_flow.outputs[master])
    os.write(master, b"\x12")
    wait_frame(child, master, ("No current selected task plan; no dispatch status is inferred.",),
               fresh_after=before)
    tui_flow.expect(master, "Owner: connected", child)
    os.write(master, b"\x1b[Z\x1b[Z")
    wait_frame(child, master, ("Projects |",))
    choose_project(child, master, expected["project_id"], expected["project_name"])
    choose_task(child, master, expected["task_id"], expected["other_task"])
    os.write(master, b"\t\t")
    denied(child, master, expected)
    print("actual task-selected reserved denial and owner reconnect inspected", flush=True)


def run(binary, socket, expected_path):
    assert expected_path.stat().st_size <= 65536, "expected receipt exceeds bound"
    expected = json.loads(expected_path.read_text())
    assert expected["read"]["data"]["dispatch"] == expected["dispatch"], expected
    child, master, slave, before = tui_flow.launch(binary, socket)
    closed = False
    print("owned TUI pid=%d supervisor=%d" % (child.pid, child.supervisor.pid), flush=True)
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 60, 200, 0, 0))
        os.kill(child.pid, signal.SIGWINCH)
        inspection(child, master, expected)
        restored = termios.tcgetattr(slave)
        tui_flow.close_tui(child, master, slave, before)
        closed = True
        expected_path.with_name("cleanup.json").write_text(json.dumps({
            "pid": child.pid, "supervisor_pid": child.supervisor.pid,
            "tui_status": child.returncode, "supervisor_status": child.supervisor.returncode,
            "terminal_restored": True,
        }))
        assert restored != before, "actual TUI never entered raw terminal mode"
    finally:
        try:
            if not closed:
                conversation.cleanup(child, master, slave)
        finally:
            retained = tui_flow.outputs.pop(master)
            expected_path.with_name("tui.pty").write_bytes(bytes(retained[:1048576]))
            print("owned TUI cleanup pid=%d status=%r supervisor=%d status=%r" % (
                child.pid, child.poll(), child.supervisor.pid, child.supervisor.poll()
            ), flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: tui_dispatch.py BINARY SOCKET EXPECTED_JSON")
    os.umask(0o077)
    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        signal.signal(signum, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 32)
    run(sys.argv[1], sys.argv[2], Path(sys.argv[3]))
