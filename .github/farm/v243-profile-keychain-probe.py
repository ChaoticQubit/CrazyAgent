"""Locked keychain probes for V243-PROFILE-KEYCHAIN-PROMPT on a farm runner only.

Input: a scratch folder that does not exist yet. Output: one line per step with the exit
code, the time and the SecurityAgent count, for keychains in a scratch HOME. The process
exits 0 unless the scratch folder already exists or a keychain is left behind.
"""

import os
import pathlib
import subprocess
import sys
import time

STEP_SECONDS = 15
ALLOWED = {
    "create-keychain", "lock-keychain", "unlock-keychain", "delete-keychain",
    "find-generic-password", "show-keychain-info", "set-keychain-settings", "-i",
    "default-keychain", "list-keychains",
}


def agents():
    """Counts running SecurityAgent processes. Output: the count."""
    found = subprocess.run(["pgrep", "-x", "SecurityAgent"], capture_output=True, text=True)
    return len(found.stdout.split())


def run(home, keychain, args, stdin=None):
    """Runs security once with only HOME and PATH set, bounded in time. Inputs: the home,
    the keychain the call must name, the arguments and stdin. Output: a printable result."""
    if args[0] not in ALLOWED:
        raise SystemExit(f"refused {args[0]}")
    if keychain not in args and (stdin is None or keychain not in stdin):
        raise SystemExit("a call names no scratch keychain")
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin"}
    start = time.monotonic()
    try:
        done = subprocess.run(["/usr/bin/security", *args], env=env, cwd=str(home), input=stdin,
                              stdin=None if stdin is not None else subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=STEP_SECONDS)
        code = str(done.returncode)
        out = done.stdout.strip()
    except subprocess.TimeoutExpired:
        code, out = "timeout", ""
    took = time.monotonic() - start
    return code, out, took


def step(label, home, keychain, args, stdin=None):
    """Prints one probe step. Inputs: the label and the run inputs. Output: the exit code."""
    code, out, took = run(home, keychain, args, stdin)
    shown = "probe-value" if out == "probe-value" else ("other output" if out else "")
    print(f"{label:58} rc={code:7} {took:5.1f}s SecurityAgent={agents()} {shown}", flush=True)
    return code


def add(home, keychain):
    """Files one dummy item through stdin. Inputs: the home and the keychain."""
    line = f'add-generic-password -a probe -s crazyagent-farm-probe -w probe-value "{keychain}"\n'
    return step("add item", home, keychain, ["-i"], line)


def main():
    root = pathlib.Path(sys.argv[1])
    if root.exists():
        raise SystemExit("the scratch folder already exists")
    home = root / "home"
    folder = home / "Library" / "Keychains"
    folder.mkdir(parents=True, mode=0o700)
    (home / "Library" / "Preferences").mkdir(mode=0o700)
    login = str(folder / "login.keychain-db")
    profile = str(folder / "profile.keychain-db")
    print(f"macOS {subprocess.run(['sw_vers', '-productVersion'], capture_output=True, text=True).stdout.strip()}, SecurityAgent={agents()}")
    print("== login.keychain-db, empty password")
    step("create", home, login, ["create-keychain", "-p", "", login])
    step("unlock '' while unlocked", home, login, ["unlock-keychain", "-p", "", login])
    add(home, login)
    step("lock", home, login, ["lock-keychain", login])
    step("unlock '' while locked", home, login, ["unlock-keychain", "-p", "", login])
    step("show-keychain-info while locked", home, login, ["show-keychain-info", login])
    step("set-keychain-settings while locked", home, login, ["set-keychain-settings", login])
    step("default-keychain -s while locked", home, login,
         ["default-keychain", "-d", "user", "-s", login])
    step("list-keychains -s while locked", home, login,
         ["list-keychains", "-d", "user", "-s", login])
    step("find -w while locked (the engine read at base)", home, login,
         ["find-generic-password", "-s", "crazyagent-farm-probe", "-w", login])
    print("== the same locked file renamed profile.keychain-db")
    os.rename(login, profile)
    step("unlock '' after the rename", home, profile, ["unlock-keychain", "-p", "", profile])
    step("find -w after the unlock", home, profile,
         ["find-generic-password", "-s", "crazyagent-farm-probe", "-w", profile])
    step("lock", home, profile, ["lock-keychain", profile])
    step("unlock '' again", home, profile, ["unlock-keychain", "-p", "", profile])
    step("delete", home, profile, ["delete-keychain", profile])
    left = sorted(p.name for p in folder.iterdir() if p.name.endswith(".keychain-db"))
    print(f"keychains left: {left}")
    if left:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
