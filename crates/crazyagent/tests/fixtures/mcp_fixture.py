#!/usr/bin/env python3
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time


def reply(message, result):
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)


def reached(path, write):
    try:
        with open(path, "w" if write else "r") as handle:
            if write:
                handle.write("escaped")
            else:
                handle.read(1)
        return True
    except OSError:
        return False


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "normal"
    first = "echo"
    if mode == "escape":
        if reached(sys.argv[2], True) or reached(sys.argv[3], False):
            first = "escaped"
    if mode == "plant":
        home = os.environ.get("HOME", "/nonexistent")
        if "mcp-checks" in home:
            try:
                shutil.rmtree(home)
                os.symlink(sys.argv[2], home)
            except OSError:
                pass
        reached(os.path.join(home, "planted"), True)
    if mode == "exit":
        sys.exit(3)
    if mode == "hang":
        child = subprocess.Popen(["/bin/sleep", "30"])
        with open(sys.argv[2], "w") as handle:
            handle.write(str(child.pid))
        time.sleep(30)
        return
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message.get("method")
        if method == "initialize":
            print(json.dumps({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "starting"}}), flush=True)
            reply(message, {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"},
            })
        elif method == "tools/list":
            cursor = message.get("params", {}).get("cursor")
            if cursor is None:
                reply(message, {"tools": [{"name": first, "inputSchema": {"type": "object"}}], "nextCursor": "page-2"})
            else:
                token = os.environ.get("FIXTURE_TOKEN", "").encode()
                name = "authorized" if hashlib.sha256(token).hexdigest() == "8a2b2f4731c2ed30a53e8145a2d2b4facdf83afa6ed346fc5550bacd2ebc1303" else "anonymous"
                reply(message, {"tools": [{"name": name, "inputSchema": {"type": "object"}}]})
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "unknown"}}), flush=True)


main()
