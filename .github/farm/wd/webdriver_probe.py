"""Drives the real desktop app on a Windows runner through W3C WebDriver calls: the probe.

Usage: python webdriver_probe.py embedded <app.exe> <port> <app log>, which starts the
app with its embedded server on the port, or python webdriver_probe.py official <app.exe>
<tauri-driver.exe> <msedgedriver.exe> <port> <driver log>, which starts tauri-driver.
Prints one "wd:" line per step. Exit 0 all steps ok, 5 no server, 6 no session, 7 a step.
"""

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

SERVER_WAIT_S = 120
CALL_WAIT_S = 20
ELEMENT = "element-6066-11e4-a07c-4abb-4a3d9c5e0f"
TYPED = "abc"


def call(base, method, path, body=None):
    """Makes one WebDriver call. Inputs: the server, the method, the path and a JSON body.
    Output: (HTTP status, the decoded answer or the raw text)."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(base + path, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=CALL_WAIT_S) as answer:
            text = answer.read().decode(errors="replace")
            return answer.status, json.loads(text) if text else {}
    except urllib.error.HTTPError as refused:
        return refused.code, refused.read().decode(errors="replace")[:400]
    except (urllib.error.URLError, OSError, ValueError) as failed:
        return 0, str(failed)[:400]


def await_server(base, process):
    """Waits for the server's /status. Inputs: the server and its process.
    Output: true once it answers 200, false past the bound or when the process ended."""
    deadline = time.monotonic() + SERVER_WAIT_S
    while time.monotonic() < deadline and process.poll() is None:
        status, _ = call(base, "GET", "/status")
        if status == 200:
            return True
        time.sleep(1)
    return False


def step(base, name, method, path, body=None):
    """Makes one probe step and prints its line. Inputs: the server, the step's name, the
    call. Output: the answer's value, or None when the step failed."""
    status, answer = call(base, method, path, body)
    if status != 200 or not isinstance(answer, dict):
        print(f"wd: {name} failed {status} {str(answer)[:300]}")
        return None
    value = answer.get("value")
    shown = len(value) if isinstance(value, str) and len(value) >= 200 else str(value)[:120]
    print(f"wd: {name} ok {shown}")
    return value if value is not None else True


def steps(base, session):
    """Runs the page steps in one session. Inputs: the server and the session id.
    Output: whether every step passed and the typed text came back."""
    at = f"/session/{session}"
    title = step(base, "title", "GET", f"{at}/title")
    wanted = {"using": "css selector", "value": "#probe-input"}
    found = step(base, "find", "POST", f"{at}/element", wanted)
    if not isinstance(found, dict) or ELEMENT not in found:
        return False
    element = f"{at}/element/{found[ELEMENT]}"
    clicked = step(base, "click", "POST", f"{element}/click", {})
    typed = step(base, "type", "POST", f"{element}/value", {"text": TYPED})
    value = step(base, "value", "GET", f"{element}/property/value")
    script = {"script": "return document.getElementById('echo').textContent", "args": []}
    echo = step(base, "echo", "POST", f"{at}/execute/sync", script)
    shot = step(base, "screenshot", "GET", f"{at}/screenshot")
    step(base, "end", "DELETE", at)
    shot_bytes = len(shot) if isinstance(shot, str) else 0
    print(f"wd: summary title {title!r} value {value!r} echo {echo!r} shot {shot_bytes}")
    acted = clicked is not None and typed is not None
    return acted and title == "farm" and value == TYPED and echo == TYPED


def started(mode, args, log):
    """Starts what serves WebDriver. Inputs: the mode, its arguments and the log file.
    Output: (the process, the server's base URL, the session's capabilities)."""
    env = dict(os.environ)
    if mode == "embedded":
        exe, port = args[0], args[1]
        env["TAURI_WEBDRIVER_PORT"] = port
        process = subprocess.Popen([exe], stdout=log, stderr=subprocess.STDOUT, env=env)
        return process, f"http://127.0.0.1:{port}", {}
    exe, driver, edge, port = args
    env["TAURI_WEBDRIVER_PORT"] = str(int(port) + 2)
    native = str(int(port) + 1)
    command = [driver, "--port", port, "--native-port", native, "--native-driver", edge]
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=env)
    return process, f"http://127.0.0.1:{port}", {"tauri:options": {"application": exe}}


def main():
    """Runs the probe in the mode argv names. Output: the exit code."""
    mode = sys.argv[1]
    args = sys.argv[2:-1]
    with open(sys.argv[-1], "w") as log:
        process, base, wanted = started(mode, args, log)
        code = 0
        try:
            if not await_server(base, process):
                print(f"wd: server failed, process ended {process.poll()}")
                return 5
            print(f"wd: server ok {mode}")
            asked = {"capabilities": {"alwaysMatch": wanted}}
            status, answer = call(base, "POST", "/session", asked)
            session = answer.get("value", {}).get("sessionId") if isinstance(answer, dict) else None
            if status != 200 or not session:
                print(f"wd: session failed {status} {str(answer)[:400]}")
                return 6
            print(f"wd: session ok {session}")
            code = 0 if steps(base, session) else 7
        finally:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
        return code


if __name__ == "__main__":
    sys.exit(main())
