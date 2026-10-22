if not __debug__:
    raise SystemExit("Preview TUI qualification requires Python assertions enabled")

import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time

sys.dont_write_bytecode = True

import tui_project_master as fixture

OUTPUT_LIMIT = 1048576


def interrupted(signum, frame):
    raise SystemExit("preview TUI qualification interrupted by signal %d" % signum)


def write_new(path, payload):
    assert len(payload) <= OUTPUT_LIMIT
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(payload)
    assert path.stat().st_mode & 0o777 == 0o600


def binary_hash(binary):
    digest = hashlib.sha256()
    with open(binary, "rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            digest.update(block)
    return digest.hexdigest()


def effects(database):
    rows = fixture.creation.read_rows(database, "SELECT "
        "(SELECT COUNT(*) FROM native_auth_operations),"
        "(SELECT COUNT(*) FROM master_sessions),"
        "(SELECT COUNT(*) FROM master_turns)")
    assert len(rows) == 1 and len(rows[0]) == 3
    return tuple(rows[0]) + fixture.creation.effect_counts(database)


def group_absent(group):
    if group is None:
        return None
    try:
        os.killpg(group, 0)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    return False


def accounts(child, master):
    os.write(master, b"\t\t")
    fixture.tui_flow.expect(master, "Accounts:", child, limit=6)
    fixture.tui_flow.expect(master, "codex-account", child, limit=6)
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        screen = fixture.conversation.screen(master)
        if "Account setup" in screen:
            return True
        assert child.poll() is None, "TUI exited before Accounts guidance check"
        fixture.conversation.drain(master)
    return "Account setup" in fixture.conversation.screen(master)


def run(binary, root):
    assert root.is_absolute() and root.is_dir() and not root.is_symlink()
    assert root.stat().st_mode & 0o777 == 0o700
    assert not any(root.iterdir()), "PRIVATE_ROOT must be a new empty directory"
    assert Path(binary).is_file() and os.access(binary, os.X_OK)
    started = time.monotonic()
    old_mask = os.umask(0o077)
    service = child = None
    group = None
    raw = bytearray()
    result = {"passed": False, "sign_in_qualified": False}
    try:
        result["binary_sha256"] = binary_hash(binary)
        write_new(root / "global.toml", fixture.CATALOG.encode())
        with fixture.creation.engine(binary, root, writable=True) as service:
            result["engine_pid"] = service.pid
            database = root / "engine.db"
            result["effects_before"] = effects(database)
            assert result["effects_before"][:3] == (0, 0, 0)
            with fixture.terminal(binary, root / "engine.sock") as (child, master):
                group = os.getpgid(child.pid)
                result.update(tui_pid=child.pid, supervisor_pid=child.supervisor.pid,
                              tui_group=group, owner_connected=True)
                raw = fixture.tui_flow.outputs[master]
                result["account_setup_visible"] = accounts(child, master)
                result["accounts_page_and_configured_account_observed"] = True
                result["effects_after"] = effects(database)
                assert result["effects_after"] == result["effects_before"]
        assert service.poll() == 0, "Owned engine did not exit cleanly"
        assert child.poll() == 0 and child.supervisor.poll() == 0
        assert group_absent(group), "Owned TUI process group remains"
        assert result["account_setup_visible"], "Accounts is missing Account setup guidance"
        result["passed"] = True
        print("PTY PASS preview Accounts setup guidance; no auth Start or master Open", flush=True)
    except BaseException as error:
        result["failure_type"] = type(error).__name__
        raise
    finally:
        result["engine_exit"] = None if service is None else service.poll()
        result["tui_exit"] = None if child is None else child.poll()
        result["supervisor_exit"] = None if child is None else child.supervisor.poll()
        result["tui_group_absent"] = group_absent(group)
        result["raw_bytes_observed"] = len(raw)
        result["raw_truncated"] = len(raw) > OUTPUT_LIMIT
        result["elapsed_ms"] = round((time.monotonic() - started) * 1000)
        try:
            write_new(root / "terminal.raw", bytes(raw[:OUTPUT_LIMIT]))
            payload = (json.dumps(result, sort_keys=True) + "\n").encode()
            assert len(payload) <= 8192
            write_new(root / "result.json", payload)
        finally:
            os.umask(old_mask)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: preview_tui_flow.py BINARY PRIVATE_ROOT")
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM):
        signal.signal(handled, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 60)
    try:
        run(str(Path(sys.argv[1]).resolve()), Path(sys.argv[2]).absolute())
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
