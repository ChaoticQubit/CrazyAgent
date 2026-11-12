"""Trial 4 check N-E4 (DEC-114, B-20; DEC-096 decisions 11 and 12): the commits a trial
adds to the Land on branch and to the lead branches carry the repo's git identity, no
Co-Authored-By trailer, subjects that end on a whole word and no absolute local path.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner saw agent commits authored by the machine's host identity with a
Co-Authored-By trailer, and squash subjects cut mid-word that carried local paths.
"""

import re

import audit

ID = "N-E4"
OP = ["OP-54"]
WHY = "B-20 is not in owner problems part A; its commits are the agent work OP-54 lands through Changes."
TITLE = "Trial commits carry the repo identity, no Co-Authored-By, whole-word subjects and no local paths"

FIELD = "\x1f"
RECORD = "\x1e"
COAUTHOR = re.compile(r"(?im)^\s*co-authored-by\s*:")
LOCAL_PATH = re.compile(r"(?:^|[\s'\"`(=:<\[])/(?:Users|private|var|tmp|home|Volumes)/")
COMMITS_MAX = 2000
TEXTS_MAX = 4000
LISTED_MAX = 8


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def commits(cwd, ref):
    """Lists a branch's commits past the repo's root commits. Inputs: the git folder and the
    ref. Output: (sha, author name, author email, message) tuples, newest first."""
    roots = (audit.git(cwd, "rev-list", "--max-parents=0", ref) or "").split()
    done = audit.git_run(cwd, "log", f"-n{COMMITS_MAX}", f"--format=%H{FIELD}%an{FIELD}%ae{FIELD}%B{RECORD}", ref, "--not", *roots)
    if done is None or done.returncode != 0:
        return []
    found = []
    for record in done.stdout.split(RECORD):
        fields = record.strip("\n").split(FIELD)
        if len(fields) == 4 and fields[0]:
            found.append((fields[0], fields[1], fields[2], fields[3].strip()))
    assert len(found) <= COMMITS_MAX
    return found


def branches(t):
    """Names the refs the trial lands on: the Land on branch and each lead's branch. Input:
    the trial. Output: the Land on branch or None, and (git folder, ref) pairs."""
    land = None
    if t.columns("team_land_on"):
        rows = t.q("SELECT branch FROM team_land_on WHERE project=?", t.pid)
        land = rows[0][0] if rows else None
    refs = [(t.repo, f"refs/heads/{land}")] if land and t.repo else []
    for lead in t.leads:
        branch = t.branch_of(lead["wt"])
        if lead["wt"] and branch:
            refs.append((lead["wt"], f"refs/heads/{branch}"))
    return land, refs


def sources(t):
    """Reads the first lines of the trial's task prompts and results and team messages, the
    texts a squash subject is cut from. Input: the trial. Output: the lines."""
    texts = [row[0] for row in t.q("SELECT prompt FROM team_tasks WHERE project=? LIMIT ?", t.pid, TEXTS_MAX)]
    texts += [row[0] for row in t.q("SELECT result FROM team_tasks WHERE project=? AND result IS NOT NULL LIMIT ?", t.pid, TEXTS_MAX)]
    if t.columns("team_messages"):
        texts += [row[0] for row in t.q("SELECT text FROM team_messages WHERE project=? LIMIT ?", t.pid, TEXTS_MAX)]
    return [text.strip().splitlines()[0] for text in texts if isinstance(text, str) and text.strip()]


def cut_mid_word(subject, lines):
    """Tells whether a subject is a known text's first line cut inside a word. Inputs: the
    subject and the lines. Output: True when cut."""
    if not subject or not subject[-1].isalnum():
        return False
    return any(len(line) > len(subject) and line.startswith(subject) and line[len(subject)].isalnum() for line in lines)


def check(t):
    """Runs N-E4. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    if not t.repo:
        return ("UNKNOWN", "the project folder is not known")
    owner = (audit.git(t.repo, "config", "user.name"), audit.git(t.repo, "config", "user.email"))
    land, refs = branches(t)
    seen = {}
    for cwd, ref in refs:
        for sha, name, email, message in commits(cwd, ref):
            seen.setdefault(sha, (name, email, message))
    lines = sources(t)
    paths = [path for path in (t.repo, str(t.root)) if path]
    failed, matched = [], 0
    for sha, (name, email, message) in seen.items():
        short, subject = sha[:12], message.splitlines()[0] if message else ""
        if None not in owner and (name, email) == owner:
            matched += 1
        elif None not in owner:
            failed.append(f"{short} by {audit.mask(f'{name} <{email}>')} is not the repo identity")
        if COAUTHOR.search(message):
            failed.append(f"{short} has a Co-Authored-By trailer")
        if cut_mid_word(subject, lines):
            failed.append(f"{short} subject ends mid-word: {audit.mask(subject)}")
        if LOCAL_PATH.search(message) or any(path in message for path in paths):
            failed.append(f"{short} names an absolute local path")
    lead_refs = len(refs) - (1 if land and t.repo else 0)
    detail = (
        f"commits={len(seen)} on {land or 'no Land on branch'} and {lead_refs} lead branches; "
        + (f"author repo identity={matched}" if None not in owner else "the repo has no git identity set")
    )
    if failed:
        return ("FAIL", f"{detail}; {listed(failed)}")
    if not seen:
        return ("UNKNOWN", f"{detail}; no commit past the seed")
    if None in owner:
        return ("UNKNOWN", detail)
    return ("PASS", detail)
