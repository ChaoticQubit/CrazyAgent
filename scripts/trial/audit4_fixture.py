"""Gives trial 4 audit tests the engine's own table definitions and a runner for one check.

The shared fixture schema holds the team tables trial 3 reads; trial 4 checks also read
tables the engine creates elsewhere. engine_table() lifts a table's CREATE statement from
the crate source, so a test store matches the engine it audits. Run nothing directly.
"""

import importlib.util
from pathlib import Path
import re
import sqlite3

import audit

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parent.parent / "crates" / "crazyagent" / "src"
TABLE_BYTES_MAX = 16 * 1024


def engine_table(name):
    """Finds the one CREATE TABLE statement the engine source gives a table, outside tests.
    Input: the table name. Output: the statement as SQL text."""
    start = re.compile(r"CREATE TABLE IF NOT EXISTS " + re.escape(name) + r"\s*\(")
    found = set()
    for path in sorted(SOURCE.rglob("*.rs")):
        if "tests" in path.parts or path.name in ("tests.rs", "test.rs"):
            continue
        text = path.read_text(encoding="utf-8")
        for match in start.finditer(text):
            depth, end = 0, None
            for index in range(match.end() - 1, min(len(text), match.start() + TABLE_BYTES_MAX)):
                depth += {"(": 1, ")": -1}.get(text[index], 0)
                if depth == 0:
                    end = index + 1
                    break
            assert end is not None, f"{name} in {path} has no closing parenthesis"
            sql = re.sub(r"\\\n\s*", " ", text[match.start():end]).replace('\\"', '"')
            found.add(" ".join(sql.split()))
    assert len(found) == 1, f"{name}: {len(found)} engine definitions"
    return found.pop()


def add_tables(store, *names):
    """Creates the engine's own tables in a test store. Inputs: the store path and table
    names. Output: none."""
    db = sqlite3.connect(store)
    try:
        for name in names:
            db.execute(engine_table(name))
        db.commit()
    finally:
        db.close()


def load_check(check_id):
    """Loads one trial 4 audit module. Input: the check id, such as "N-E3". Output: the module."""
    spec = importlib.util.spec_from_file_location(check_id.lower().replace("-", "_"), HERE / "audit4" / f"{check_id.lower()}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(check, world, project, mark, answer="Hello"):
    """Runs a check on a world's store opened read-only, through the audit's Trial context.
    Inputs: the module, the world, the project, the mark and the owner answer. Output: the
    verdict and detail, checked for shape."""
    db = sqlite3.connect(f"file:{world.store}?mode=ro", uri=True)
    try:
        outcome, detail = check.check(audit.Trial(db, world.root, project, mark, answer))
    finally:
        db.close()
    assert outcome in audit.RESULTS, outcome
    assert isinstance(detail, str) and detail, detail
    return outcome, detail
