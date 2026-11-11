"""Trial 4 check N-E1 (DEC-114 OP-7, B-01): after the trial's interrupted Claude sign-in and
the sign-in that follows it, no sign-in fence is held and no account's newest attempt is
left uncertain or in flight.

Input: the audit's read-only Trial context. Output: check(t) returns the verdict and a
detail line. The owner closed a browser sign-in early, and the fence it left blocked every
later attempt, so the account stayed "Not signed in".
"""

import hashlib
import json

import audit

ID = "N-E1"
OP = ["OP-7"]
TITLE = "After an interrupted sign-in, signing in again leaves no fence and no attempt uncertain"
WHY = "Trial accounts are the scratch accounts whose name starts with ca-trial (N-D1)."

TRIAL_ACCOUNT = "ca-trial"
SIGNED_IN = "authenticated_observed"
LEFT = ("starting", "awaiting_owner", "checking_account", "cancelling", "uncertain")
ACCOUNTS_MAX = 1000
OPERATIONS_MAX = 1024
LISTED_MAX = 8


def listed(items):
    """Joins at most LISTED_MAX items and counts the rest. Input: strings. Output: the text."""
    items = list(items)
    shown = ", ".join(items[:LISTED_MAX])
    return shown + (f" and {len(items) - LISTED_MAX} more" if len(items) > LISTED_MAX else "")


def names(t):
    """Names each native-managed profile by its saved account. Input: the trial. Output: a
    dict from the profile key, the hex SHA-256 of the profile reference, to the name."""
    named = {}
    for account_id, body in t.q("SELECT id, body FROM accounts ORDER BY seq LIMIT ?", ACCOUNTS_MAX):
        try:
            saved = json.loads(body)
            profile = saved["connection"]["auth"].get("profile_ref")
            name = saved.get("details", {}).get("name") or account_id
        except (ValueError, KeyError, TypeError, AttributeError):
            continue
        if isinstance(profile, str) and profile:
            named[hashlib.sha256(profile.encode()).hexdigest()] = str(name)
    return named


def spoken(name):
    """Shows an account name with any email or secret in it masked. Input: the name. Output: text."""
    return audit.mask(name)


def check(t):
    """Runs N-E1. Input: the trial. Output: ("PASS" | "FAIL" | "UNKNOWN", detail)."""
    if not (t.columns("native_auth_operations") and t.columns("native_auth_fences") and t.columns("accounts")):
        return ("UNKNOWN", "the engine store has no native sign-in tables")
    named = names(t)
    label = lambda key: spoken(named.get(key, f"profile {key[:8]}"))
    attempts = {}
    for operation, key, generation, phase in t.q(
        "SELECT id, profile_key, generation, phase FROM native_auth_operations ORDER BY profile_key, generation LIMIT ?", OPERATIONS_MAX,
    ):
        attempts.setdefault(key, []).append((generation, phase, operation))
    generations = {operation: (key, generation) for key, rows in attempts.items() for generation, _, operation in rows}
    failed = []
    fences = t.q("SELECT profile_key, operation_id FROM native_auth_fences")
    assert len(fences) <= 1, "the engine holds one sign-in fence at a time"
    for key, operation in fences:
        failed.append(f"a sign-in fence is still held for {label(key)} by attempt {generations.get(operation, (key, '?'))[1]}")
    settled = 0
    for key, rows in attempts.items():
        generation, phase, _ = rows[-1]
        if phase in LEFT:
            failed.append(f"{label(key)} attempt {generation} left {phase}")
        settled += sum(1 for _, older, _ in rows[:-1] if older in LEFT)
    trial = {key: rows[-1] for key, rows in attempts.items() if named.get(key, "").startswith(TRIAL_ACCOUNT)}
    shown = []
    for key, (generation, phase, _) in sorted(trial.items(), key=lambda item: named[item[0]]):
        if phase == SIGNED_IN:
            shown.append(f"{label(key)} signed in at attempt {generation}")
        elif phase not in LEFT:
            failed.append(f"{label(key)} attempt {generation} ended {phase}, not signed in")
    left = sum(1 for rows in attempts.values() if rows[-1][1] in LEFT)
    detail = f"fences={len(fences)}; left uncertain or in flight={left}; settled by a later attempt={settled}"
    detail += f"; {listed(shown)}" if shown else ""
    if failed:
        return ("FAIL", f"{detail}; {listed(failed)}")
    if not trial:
        return ("UNKNOWN", f"{detail}; no ca-trial account made a sign-in attempt")
    return ("PASS", detail)
