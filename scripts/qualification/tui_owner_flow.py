import errno
import fcntl
import os
import select
import signal
import struct
import sys
import time
from pathlib import Path

import tui_flow


def interrupted(signum, frame):
    raise SystemExit("owner Evidence driver interrupted by signal %s" % signum)


def run(binary, socket, project, marker, baseline_id):
    child, master, slave, before = tui_flow.launch(binary, socket)
    closed = False
    try:
        tui_flow.expect(master, "Connected", child)
        tui_flow.inspect_pty(child, master, slave, "owner Evidence")
        os.write(master, b"\t")
        tui_flow.expect(master, project, child)
        os.write(master, b"\t" * 5)
        tui_flow.expect(master, "Receipts, evidence and merge", child)
        deadline = time.monotonic() + 6
        while True:
            height, width, _, _ = struct.unpack(
                "HHHH", fcntl.ioctl(master, tui_flow.termios.TIOCGWINSZ, bytes(8))
            )
            rendered = tui_flow.screen_text(tui_flow.outputs[master], width, height)
            if marker in rendered and baseline_id in rendered:
                break
            if child.poll() is not None or time.monotonic() >= deadline:
                raise AssertionError(
                    "Evidence missing actual discovered baseline detail: "
                    "marker=%r id=%r status=%r screen=%r"
                    % (marker, baseline_id, child.poll(), rendered)
                )
            if select.select([master], [], [], 0.1)[0]:
                try:
                    part = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    part = b""
                tui_flow.outputs[master].extend(part)
                assert len(tui_flow.outputs[master]) <= 1048576, "PTY output exceeded one MiB"
        tui_flow.close_tui(child, master, slave, before)
        closed = True
    finally:
        if not closed:
            try:
                if child.poll() is None:
                    child.kill()
                deadline = time.monotonic() + 5
                while child.poll() is None and time.monotonic() < deadline:
                    if select.select([master], [], [], 0.05)[0]:
                        try:
                            os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
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
        tui_flow.outputs.pop(master, None)


if __name__ == "__main__":
    if len(sys.argv) != 6:
        raise SystemExit("usage: tui_owner_flow.py BINARY SOCKET PROJECT MARKER BASELINE_ID")
    for handled in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(handled, interrupted)
    run(str(Path(sys.argv[1]).resolve()), Path(sys.argv[2]), *sys.argv[3:])
