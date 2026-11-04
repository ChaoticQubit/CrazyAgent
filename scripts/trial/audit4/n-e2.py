"""Trial 4 check N-E2 (DEC-114 OP-19, B-06): the repo N-D6 adds from beside the projects
folder is set up where it is: its project names the original folder, that folder and its
linked worktree are still there, and nothing of it was moved or copied into the projects
folder.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner's Add folder moved a live repo into the projects folder, and a
failed setup then left the repo missing.
"""

import json
import os
import re

import audit

ID = "N-E2"
OP = ["OP-19"]
TITLE = "An existing repo added from outside the projects folder is used where it is"

OUTSIDE = re.compile(r"^ca-trial-4-outside-([0-9]{12})$")
WORKTREE_SUFFIX = "-wt"
PROJECTS_MAX = 1000
HOME_ENTRIES_MAX = 10_000
LISTED_MAX = 8


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def outside_projects(t):
    """Finds the projects whose repository folder N-D6 made. Input: the trial. Output:
    (minute stamp, project id, folder) tuples."""
    found = []
    for project_id, body in t.q("SELECT id, body FROM projects LIMIT ?", PROJECTS_MAX):
        try:
            repositories = json.loads(body)["spec"]["repositories"]
        except (ValueError, KeyError, TypeError):
            continue
        for folder in repositories if isinstance(repositories, list) else []:
            match = OUTSIDE.match(os.path.basename(str(folder).rstrip("/")))
            if match:
                found.append((match.group(1), project_id, os.path.realpath(os.path.expanduser(str(folder)))))
    return found


def linked(folder):
    """Lists a repository's linked worktrees. Input: the folder. Output: their real paths."""
    listing = audit.git(folder, "worktree", "list", "--porcelain") or ""
    paths = [line[len("worktree "):] for line in listing.splitlines() if line.startswith("worktree ")]
    return [os.path.realpath(path) for path in paths[1:]]


def check(t):
    """Runs N-E2. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    if not t.columns("projects_home"):
        return ("UNKNOWN", "the engine store has no projects_home table")
    rows = t.q("SELECT path FROM projects_home WHERE singleton=1")
    if not rows:
        return ("UNKNOWN", "no projects folder is set")
    home = os.path.realpath(os.path.expanduser(rows[0][0]))
    found = outside_projects(t)
    if not found:
        return ("UNKNOWN", "no project names a ca-trial-4-outside folder: the N-D6 add did not run")
    _, project_id, folder = max(found)
    name = os.path.basename(folder)
    original = os.path.join(os.path.dirname(home), name)
    failed = []
    if audit.under(folder, home):
        failed.append(f"project {project_id} uses a folder inside the projects folder")
    elif folder != original:
        failed.append(f"project {project_id} uses {name} somewhere other than beside the projects folder")
    if not os.path.isdir(original):
        failed.append("the original folder is gone from beside the projects folder")
    elif os.path.realpath(original + WORKTREE_SUFFIX) not in linked(original) or not os.path.isdir(original + WORKTREE_SUFFIX):
        failed.append("its linked worktree is gone")
    entries = sorted(os.listdir(home))[:HOME_ENTRIES_MAX] if os.path.isdir(home) else []
    failed += [f"the projects folder holds {entry}" for entry in entries if entry.startswith(name)]
    detail = f"{name}: project {project_id}"
    if failed:
        return ("FAIL", f"{detail}; {listed(failed)}")
    return ("PASS", f"{detail} uses the folder where it was; linked worktree kept; nothing of it in the projects folder")
