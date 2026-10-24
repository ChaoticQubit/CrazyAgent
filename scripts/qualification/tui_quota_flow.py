if not __debug__:
    raise SystemExit("TUI quota qualification requires Python assertions enabled")

import fcntl
import json
import math
import os
from pathlib import Path
import pty
import selectors
import signal
import struct
import subprocess
import sys
import termios
import threading
import time

import tui_flow


STREAM_MAX = 65536
PTY_MAX = 1048576


def write_private(path, data):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)


def group_absent(group):
    if group is None:
        return False
    try:
        os.killpg(group, 0)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    return False


def kill_group(group):
    assert group is not None and group > 0 and group != os.getpgrp()
    try:
        os.killpg(group, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def interrupted(received, _frame):
    raise TimeoutError("quota scenario interrupted by signal %s" % received)


class EngineCapture:
    def __init__(self, process, deadline):
        self.process = process
        self.deadline = deadline
        self.buffers = {"stdout": bytearray(), "stderr": bytearray()}
        self.condition = threading.Condition()
        self.stopped = threading.Event()
        self.problem = None
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()

    def drain(self):
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(self.process.stdout, selectors.EVENT_READ, "stdout")
                selector.register(self.process.stderr, selectors.EVENT_READ, "stderr")
                while selector.get_map() and not self.stopped.is_set():
                    remaining = self.deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    for key, _mask in selector.select(min(0.1, remaining)):
                        part = os.read(key.fileobj.fileno(), 4096)
                        if not part:
                            selector.unregister(key.fileobj)
                            continue
                        with self.condition:
                            buffer = self.buffers[key.data]
                            room = STREAM_MAX - len(buffer)
                            buffer.extend(part[:room])
                            if len(part) > room:
                                self.problem = "engine %s exceeded 64 KiB" % key.data
                            self.condition.notify_all()
        except Exception as problem:
            with self.condition:
                self.problem = "engine capture failed: %s" % problem
                self.condition.notify_all()
        finally:
            with self.condition:
                self.condition.notify_all()

    def check(self):
        with self.condition:
            if self.problem is not None:
                raise AssertionError(self.problem)

    def ready(self, deadline):
        with self.condition:
            while b"\n" not in self.buffers["stdout"]:
                self.check()
                if self.process.poll() is not None:
                    raise AssertionError("actual engine exited before ready")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("actual writable engine did not become ready")
                self.condition.wait(remaining)
            self.check()
            assert bytes(self.buffers["stdout"]).split(b"\n", 1)[0] == b"ready"

    def finish(self, root, deadline):
        self.thread.join(timeout=max(0, min(0.5, deadline - time.monotonic())))
        self.stopped.set()
        self.thread.join(timeout=max(0, min(0.5, deadline - time.monotonic())))
        assert not self.thread.is_alive(), "engine stream reader did not stop"
        for label, buffer in self.buffers.items():
            write_private(root / ("engine." + label), bytes(buffer))
        self.process.stdout.close()
        self.process.stderr.close()
        self.check()


def remaining(state, maximum=6, cleanup=False):
    end = state["deadline"] if cleanup else state["work_deadline"]
    seconds = min(maximum, end - time.monotonic())
    if seconds <= 0:
        raise TimeoutError("quota %s budget exhausted" % ("cleanup" if cleanup else "work"))
    return seconds


def save_raw(state):
    total = 0
    for session in state["sessions"]:
        data = session["raw"]
        if data is None:
            data = tui_flow.outputs.get(session["master"], bytearray())
        total += len(data)
        write_private(state["root"] / ("pty-%s.raw" % session["number"]), bytes(data[:PTY_MAX]))
        assert len(data) <= PTY_MAX, "quota PTY session exceeded one MiB"
    assert total <= 2 * PTY_MAX, "quota PTY output exceeded two MiB total"


def session_identity(session):
    child = session["child"]
    supervisor = session["supervisor"]
    return {
        "number": session["number"],
        "tui_pid": child.pid if child is not None else None,
        "tui_exit": child.returncode if child is not None else None,
        "supervisor_pid": supervisor.pid if supervisor is not None else None,
        "supervisor_exit": supervisor.poll() if supervisor is not None else None,
        "pgid": session["group"],
        "session_id": session["session_id"],
        "group_absent": group_absent(session["group"]),
        "terminal_modes_restored": session["restored"],
    }


def save_identities(state):
    data = {
        "python": state["python_identity"],
        "engine": state["engine_identity"],
        "sessions": [session_identity(session) for session in state["sessions"]],
        "stage": state["stage"],
    }
    write_private(state["root"] / "identities.json", json.dumps(data, sort_keys=True).encode())


def checkpoint(state, stage):
    state["stage"] = stage
    remaining(state)
    state["capture"].check()
    save_raw(state)
    save_identities(state)
    print(json.dumps({"stage": stage, "elapsed_seconds": time.monotonic() - state["started"]}), flush=True)


def launch_session(state):
    checkpoint(state, "launch session %s" % (len(state["sessions"]) + 1))
    master, slave = pty.openpty()
    session = {
        "number": len(state["sessions"]) + 1,
        "master": master,
        "slave": slave,
        "before": None,
        "child": None,
        "supervisor": None,
        "group": None,
        "session_id": None,
        "closed": False,
        "restored": False,
        "release": None,
        "raw": None,
    }
    state["sessions"].append(session)
    session["before"] = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    status_read, status_write = os.pipe()
    release_read, release_write = os.pipe()
    session["release"] = release_write
    try:
        supervisor = subprocess.Popen(
            [sys.executable, str(Path(tui_flow.__file__).resolve()), "--session",
             state["binary"], str(state["root"] / "engine.sock"), str(status_write), str(release_read)],
            stdin=slave, stdout=slave, stderr=slave,
            start_new_session=True,
            pass_fds=(status_write, release_read),
            env=dict(os.environ, TERM="xterm-256color"),
        )
        session["supervisor"] = supervisor
        session["group"] = os.getpgid(supervisor.pid)
        session["session_id"] = os.getsid(supervisor.pid)
        assert session["group"] == session["session_id"] == supervisor.pid
        save_identities(state)
    except BaseException:
        os.close(status_read)
        raise
    finally:
        os.close(status_write)
        os.close(release_read)
    try:
        child = tui_flow.PtyProcess(supervisor, status_read, release_write)
    except BaseException:
        try:
            os.close(status_read)
        except OSError:
            pass
        raise
    session["child"] = child
    session["release"] = None
    tui_flow.outputs[master] = bytearray()
    assert os.getpgid(child.pid) == session["group"]
    save_identities(state)
    tui_flow.inspect_pty(child, master, slave, "quota session %s" % session["number"])
    return session


def current_screen(session):
    height, width, _x, _y = struct.unpack(
        "HHHH", fcntl.ioctl(session["master"], termios.TIOCGWINSZ, bytes(8))
    )
    return tui_flow.screen_text(tui_flow.outputs[session["master"]], width, height)


def expect_screen(state, session, stage, *texts):
    checkpoint(state, stage)
    deadline = time.monotonic() + remaining(state)
    for text in texts:
        if text not in current_screen(session):
            limit = min(remaining(state), deadline - time.monotonic())
            if limit <= 0:
                raise TimeoutError("quota screen stage exhausted: " + stage)
            tui_flow.expect(session["master"], text, session["child"], limit=limit)
    screen = current_screen(session)
    assert all(text in screen for text in texts), (stage, texts, screen)
    checkpoint(state, stage + " observed")


def send(state, session, keys):
    remaining(state)
    state["capture"].check()
    assert os.write(session["master"], keys) == len(keys)


def enter_pools(state, session, count):
    expect_screen(state, session, "connected", "Connected")
    send(state, session, b"\t\t3")
    expect_screen(state, session, "pool page", "Pools: %s records" % count,
                  "Writable: host-granted metadata sources")


def inspect_pool(state, session, policy):
    send(state, session, b"\r")
    expect_screen(state, session, "actual owner pool details " + policy,
                  "Provider quota policy: " + policy,
                  "Member connections: connection-1",
                  "Projects:  | Roles:  | Fallback policy: false")


def close_session(state, session):
    checkpoint(state, "close session %s" % session["number"])
    tui_flow.close_tui(session["child"], session["master"], session["slave"], session["before"])
    session["raw"] = bytes(tui_flow.outputs[session["master"]])
    session["closed"] = True
    session["restored"] = True
    save_raw(state)
    assert group_absent(session["group"]), "PTY group survived actual close"
    save_identities(state)
    assert state["engine"].poll() is None, "closing the TUI terminated the engine"


def cleanup_session(state, session):
    if session["closed"]:
        return
    child = session["child"]
    supervisor = session["supervisor"]
    try:
        if child is not None:
            if child.poll() is None:
                os.write(session["master"], b"\x11")
                try:
                    child.wait(timeout=remaining(state, 1, cleanup=True))
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=remaining(state, 1, cleanup=True))
            if child.release_fd is not None:
                child.release()
            elif supervisor.poll() is None:
                kill_group(session["group"])
                supervisor.wait(timeout=remaining(state, 1, cleanup=True))
            child.status.close()
        elif supervisor is not None:
            if session["group"] is None:
                supervisor.kill()
            else:
                kill_group(session["group"])
            supervisor.wait(timeout=remaining(state, 1, cleanup=True))
    finally:
        session["raw"] = bytes(tui_flow.outputs.get(session["master"], bytearray()))
        if session["release"] is not None:
            os.close(session["release"])
            session["release"] = None
        session["restored"] = termios.tcgetattr(session["slave"]) == session["before"]
        os.close(session["master"])
        os.close(session["slave"])
        session["closed"] = True


def run(binary, root, unix_deadline):
    started = time.monotonic()
    allowance = min(60, unix_deadline - time.time())
    assert math.isfinite(allowance) and allowance > 10, "scenario launch consumed cleanup reserve"
    state = {
        "binary": str(Path(binary).resolve()),
        "root": Path(root).resolve(),
        "started": started,
        "python_identity": {
            "pid": os.getpid(), "pgid": os.getpgrp(), "session_id": os.getsid(0),
        },
        "deadline": started + allowance,
        "work_deadline": started + allowance - 10,
        "sessions": [],
        "engine": None,
        "engine_identity": None,
        "capture": None,
        "stage": "start writable engine",
    }
    for received in [signal.SIGALRM, signal.SIGTERM, signal.SIGINT]:
        signal.signal(received, interrupted)
    signal.setitimer(signal.ITIMER_REAL, remaining(state, 50))
    failure = None
    cleanup_errors = []
    try:
        service = subprocess.Popen(
            [state["binary"], "engine", str(state["root"]), str(state["root"] / "global.toml"),
             str(state["root"] / "project.toml"), "engine.sock", "--writable-settings"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )
        state["engine"] = service
        state["engine_identity"] = {
            "pid": service.pid, "pgid": os.getpgid(service.pid), "session_id": os.getsid(service.pid),
        }
        assert state["engine_identity"]["pgid"] == service.pid
        assert state["engine_identity"]["session_id"] == service.pid
        save_identities(state)
        state["capture"] = EngineCapture(service, state["deadline"])
        state["capture"].ready(time.monotonic() + remaining(state, 5))
        session = launch_session(state)
        enter_pools(state, session, 0)
        send(state, session, b"n")
        expect_screen(state, session, "pool label", "Account pool", "Pool label (optional)")
        send(state, session, b"pty quota pool\r")
        expect_screen(state, session, "default quota choice", "Choose connections", "> [ ] connection-1",
                      "Provider quota policy: bounded_attempt",
                      "Strict blocks unknown provider quota; numeric quota is currently unavailable.")
        send(state, session, b" ")
        expect_screen(state, session, "member chosen", "> [x] connection-1", "1 selected across all pages")
        send(state, session, b"\r")
        expect_screen(state, session, "created pool saved", "Pools: 1 records", "> pool-ptyquotapool-")
        inspect_pool(state, session, "bounded_attempt")
        global_path = state["root"] / "global.toml"
        with global_path.open("rb") as stream:
            snapshot = stream.read(STREAM_MAX + 1)
        assert len(snapshot) <= STREAM_MAX, "actual persisted config exceeded 64 KiB"
        write_private(state["root"] / "created-default.toml", snapshot)
        send(state, session, b"e")
        expect_screen(state, session, "replace retains default", "Choose connections", "> [x] connection-1",
                      "Provider quota policy: bounded_attempt")
        send(state, session, b"p")
        expect_screen(state, session, "strict selected", "Provider quota policy: strict",
                      "Strict blocks unknown provider quota; numeric quota is currently unavailable.",
                      "Bounded attempt still requires fresh readiness and execution admission.")
        send(state, session, b"\r")
        expect_screen(state, session, "strict pool saved", "Pools: 1 records", "> pool-ptyquotapool-")
        close_session(state, session)
        fresh = launch_session(state)
        enter_pools(state, fresh, 1)
        inspect_pool(state, fresh, "strict")
        close_session(state, fresh)
        checkpoint(state, "stop actual engine")
        service.send_signal(signal.SIGINT)
        assert service.wait(timeout=remaining(state, 5)) == 0, "actual engine shutdown failed"
    except BaseException as problem:
        failure = problem
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        try:
            signal.setitimer(signal.ITIMER_REAL, remaining(state, 10, cleanup=True))
            for session in reversed(state["sessions"]):
                try:
                    cleanup_session(state, session)
                except Exception as problem:
                    cleanup_errors.append("PTY cleanup: %s" % problem)
            service = state["engine"]
            if service is not None and service.poll() is None:
                service.send_signal(signal.SIGINT)
                try:
                    service.wait(timeout=remaining(state, 2, cleanup=True))
                except subprocess.TimeoutExpired:
                    kill_group(state["engine_identity"]["pgid"])
                    service.wait(timeout=remaining(state, 1, cleanup=True))
            if state["capture"] is not None:
                state["capture"].finish(state["root"], state["deadline"])
            save_raw(state)
        except BaseException as problem:
            cleanup_errors.append("cleanup budget/capture: %s" % problem)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            for session in state["sessions"]:
                if session["group"] is not None and not group_absent(session["group"]):
                    kill_group(session["group"])
            if state["engine_identity"] is not None and not group_absent(state["engine_identity"]["pgid"]):
                kill_group(state["engine_identity"]["pgid"])
            rows = [session_identity(session) for session in state["sessions"]]
            engine_group_absent = state["engine_identity"] is not None and group_absent(
                state["engine_identity"]["pgid"]
            )
            report = {
                "python": state["python_identity"],
                "engine": state["engine_identity"],
                "engine_exit": state["engine"].poll() if state["engine"] is not None else None,
                "engine_group_absent": engine_group_absent,
                "sessions": rows,
                "groups_absent": engine_group_absent and all(row["group_absent"] for row in rows),
                "terminal_modes_restored": all(row["terminal_modes_restored"] for row in rows),
                "stage": state["stage"],
                "elapsed_seconds": time.monotonic() - started,
                "cleanup_errors": cleanup_errors,
            }
            write_private(state["root"] / "cleanup.json", json.dumps(report, sort_keys=True).encode())
            save_identities(state)
    if failure is not None:
        raise AssertionError("quota stage %r failed; actual fixture %s; cleanup %s" % (
            state["stage"], state["root"], json.dumps(report, sort_keys=True)
        )) from failure
    assert not cleanup_errors, report
    assert report["groups_absent"] and report["terminal_modes_restored"], report
    assert len(report["sessions"]) == 2, report
    print(json.dumps({"quota_pty_ui_complete": True, "cleanup": report}, sort_keys=True), flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: tui_quota_flow.py BINARY ROOT UNIX_DEADLINE")
    run(sys.argv[1], sys.argv[2], float(sys.argv[3]))
