#!/usr/bin/python3 -I
import json
import os
import subprocess
import sys

SESSION = "fixture-session-%d" % os.getpid()
COUNTER = [0]


def emit(value):
    COUNTER[0] += 1
    value["session_id"] = SESSION
    value["uuid"] = "fixture-%d-%d" % (os.getpid(), COUNTER[0])
    sys.stdout.write(json.dumps(value) + "\n")
    sys.stdout.flush()


def reply(text):
    emit({
        "type": "assistant",
        "parent_tool_use_id": None,
        "message": {
            "id": "message-%d" % COUNTER[0],
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        },
    })
    emit({"type": "result", "subtype": "success", "is_error": False, "result": text})


def main():
    grandchild = subprocess.Popen(
        ["/bin/sleep", "300"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    with open("pids.json.tmp", "w") as handle:
        json.dump({"pid": os.getpid(), "grandchild": grandchild.pid}, handle)
    os.replace("pids.json.tmp", "pids.json")
    emit({"type": "system", "subtype": "init", "model": "fixture", "capabilities": []})
    waiting = False
    while True:
        line = sys.stdin.readline()
        if not line:
            return 0
        message = json.loads(line)
        if message.get("type") == "control_request":
            emit_control(message)
            if waiting:
                waiting = False
                emit({"type": "result", "subtype": "error_during_execution", "is_error": True})
            continue
        if message.get("type") != "user":
            continue
        text = message["message"]["content"]
        if text == "wait":
            waiting = True
        elif text.startswith("echo "):
            reply(text[len("echo "):])
        elif text.startswith("exit "):
            reply("leaving")
            return int(text[len("exit "):])
        else:
            reply("unknown")


def emit_control(message):
    response = {"subtype": "success", "request_id": message["request_id"]}
    sys.stdout.write(json.dumps({"type": "control_response", "response": response}) + "\n")
    sys.stdout.flush()


sys.exit(main())
