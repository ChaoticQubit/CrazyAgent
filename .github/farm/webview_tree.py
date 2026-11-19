"""Starts the desktop app on a Windows runner and prints its window tree: the WebView2 probe.

Usage: python webview_tree.py <app.exe> <log path>. Prints one "probe:" line per window
under the app's top-level window, in z-order, with class, owning process and placement.
Exit 0 when a WebView2 (Chrome_*) window sits under it, 3 when none, 4 when no window.
"""

import ctypes
import ctypes.wintypes as w
import os
import subprocess
import sys
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
WNDENUMPROC = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
user32.EnumWindows.argtypes = [WNDENUMPROC, w.LPARAM]
user32.GetWindow.argtypes = [w.HWND, w.UINT]
user32.GetWindow.restype = w.HWND
user32.GetClassNameW.argtypes = [w.HWND, w.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, ctypes.c_int]
user32.GetWindowRect.argtypes = [w.HWND, ctypes.POINTER(w.RECT)]
user32.GetClientRect.argtypes = [w.HWND, ctypes.POINTER(w.RECT)]
user32.ScreenToClient.argtypes = [w.HWND, ctypes.POINTER(w.POINT)]
user32.GetWindowThreadProcessId.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]
user32.IsWindowVisible.argtypes = [w.HWND]
user32.GetParent.argtypes = [w.HWND]
user32.GetParent.restype = w.HWND
user32.GetWindowLongPtrW.argtypes = [w.HWND, ctypes.c_int]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.GetDpiForWindow.argtypes = [w.HWND]
user32.GetDpiForWindow.restype = w.UINT
kernel32.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
kernel32.OpenProcess.restype = w.HANDLE
kernel32.QueryFullProcessImageNameW.argtypes = [
    w.HANDLE,
    w.DWORD,
    w.LPWSTR,
    ctypes.POINTER(w.DWORD),
]
kernel32.CloseHandle.argtypes = [w.HANDLE]

GW_HWNDNEXT = 2
GW_CHILD = 5
GWL_STYLE = -16
GWL_EXSTYLE = -20
QUERY_LIMITED = 0x1000
WINDOWS_MAX = 400
DEPTH_MAX = 10
START_WAIT_S = 120
WEBVIEW_WAIT_S = 90
SETTLE_S = 8


def text_of(call, hwnd):
    """Reads a window string (class or title). Inputs: the user32 call, the window.
    Output: the string, at most 255 characters."""
    buffer = ctypes.create_unicode_buffer(256)
    call(hwnd, buffer, 256)
    return buffer.value


def owner_of(hwnd):
    """Names the process that owns a window. Input: the window. Output: (pid, exe name)."""
    pid = w.DWORD(0)
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    handle = kernel32.OpenProcess(QUERY_LIMITED, False, pid.value)
    if not handle:
        return pid.value, "?"
    size = w.DWORD(1024)
    path = ctypes.create_unicode_buffer(1024)
    named = kernel32.QueryFullProcessImageNameW(handle, 0, path, ctypes.byref(size))
    kernel32.CloseHandle(handle)
    return pid.value, os.path.basename(path.value) if named else "?"


def tops_of(pid):
    """Lists the top-level windows a process owns, shown or hidden, those with a child
    first so a cut listing keeps the main tree. Input: the pid. Output: hwnds."""
    found = []

    def each(hwnd, _):
        if owner_of(hwnd)[0] == pid:
            found.append(hwnd)
        return len(found) < WINDOWS_MAX

    user32.EnumWindows(WNDENUMPROC(each), 0)
    return sorted(found, key=lambda top: not user32.GetWindow(top, GW_CHILD))


def tree(top):
    """Walks a window's descendants in z-order, depth first, bounded. Input: the top-level
    window. Output: (depth, hwnd) pairs, the top first."""
    walked = [(0, top)]
    stack = [(1, user32.GetWindow(top, GW_CHILD))]
    while stack and len(walked) < WINDOWS_MAX:
        depth, hwnd = stack.pop()
        if not hwnd:
            continue
        walked.append((depth, hwnd))
        stack.append((depth, user32.GetWindow(hwnd, GW_HWNDNEXT)))
        if depth < DEPTH_MAX:
            stack.append((depth + 1, user32.GetWindow(hwnd, GW_CHILD)))
    return walked


def line_of(top, depth, hwnd):
    """Formats one window: depth, handle, class, owner, visibility, styles, its rect in
    the top-level's client area, its size and DPI. Inputs: the top, depth, window."""
    rect = w.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    corner = w.POINT(rect.left, rect.top)
    user32.ScreenToClient(top, ctypes.byref(corner))
    pid, exe = owner_of(hwnd)
    parent = user32.GetParent(hwnd) or 0
    style = user32.GetWindowLongPtrW(hwnd, GWL_STYLE) & 0xFFFFFFFF
    extended = user32.GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & 0xFFFFFFFF
    name = text_of(user32.GetClassNameW, hwnd)
    title = text_of(user32.GetWindowTextW, hwnd)[:40]
    shown = bool(user32.IsWindowVisible(hwnd))
    return (
        f"probe: depth {depth} hwnd {hwnd:#x} parent {parent:#x} class {name!r} "
        f"title {title!r} owner {exe} {pid} visible {shown} style {style:#010x} "
        f"exstyle {extended:#010x} at ({corner.x}, {corner.y}) size "
        f"{rect.right - rect.left}x{rect.bottom - rect.top} dpi {user32.GetDpiForWindow(hwnd)}"
    )


def await_tops(app):
    """Waits for the app's first top-level window. Input: the process.
    Output: its windows, empty if none came or the app ended."""
    deadline = time.monotonic() + START_WAIT_S
    while time.monotonic() < deadline and app.poll() is None:
        tops = tops_of(app.pid)
        if tops:
            return tops
        time.sleep(1)
    return []


def has_webview(tops):
    """Whether a Chrome_* window sits under one of the windows. Input: top-level windows."""
    for top in tops:
        for _, hwnd in tree(top)[1:]:
            if text_of(user32.GetClassNameW, hwnd).startswith("Chrome_"):
                return True
    return False


def main():
    """Runs the probe and prints its lines. Output: the exit code."""
    exe, log_path = sys.argv[1], sys.argv[2]
    with open(log_path, "w") as log:
        app = subprocess.Popen([exe], stdout=log, stderr=subprocess.STDOUT)
        tops = await_tops(app)
        print(f"probe: app pid {app.pid} tops {len(tops)} ended {app.poll()}")
        deadline = time.monotonic() + WEBVIEW_WAIT_S
        while tops and not has_webview(tops) and time.monotonic() < deadline:
            time.sleep(1)
            tops = tops_of(app.pid) or tops
        time.sleep(SETTLE_S)
        tops = tops_of(app.pid) or tops
        found = has_webview(tops)
        for top in tops:
            client = w.RECT()
            user32.GetClientRect(top, ctypes.byref(client))
            print(f"probe: top {top:#x} client {client.right}x{client.bottom}")
            for depth, hwnd in tree(top):
                print(line_of(top, depth, hwnd))
        print(f"probe: webview under the app window {found}")
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(app.pid)], capture_output=True)
        try:
            app.wait(timeout=30)
        except subprocess.TimeoutExpired:
            app.kill()
    if not tops:
        return 4
    return 0 if found else 3


if __name__ == "__main__":
    sys.exit(main())
