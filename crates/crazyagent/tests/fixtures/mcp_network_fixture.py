#!/usr/bin/env python3
import json
import socket
import sys


def connected(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2):
            return True
    except OSError:
        return False


def main():
    first = "connected" if connected(int(sys.argv[1])) else "refused"
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message.get("method")
        if method == "initialize":
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "protocolVersion": message["params"]["protocolVersion"],
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "netfixture", "version": "1"},
                        },
                    }
                ),
                flush=True,
            )
        elif method == "tools/list":
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "tools": [{"name": first, "inputSchema": {"type": "object"}}]
                        },
                    }
                ),
                flush=True,
            )
        else:
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "error": {"code": -32601, "message": "unknown"},
                    }
                ),
                flush=True,
            )


main()
