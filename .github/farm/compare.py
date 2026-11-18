"""Compares the JUnit reports under a folder with the 47-row macOS baseline.

Inputs: argv[1] the folder of JUnit files (searched recursively), argv[2] the baseline list,
one crazyagent library row per line, argv[3] a label. Output: each baseline row's state
(passed, failed or not run) and every failed row outside the baseline; exit 0 always.
"""
import pathlib
import sys
import xml.etree.ElementTree as ET

folder, baseline_path, label = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
baseline = [line.strip() for line in baseline_path.read_text().splitlines() if line.strip()]
assert len(baseline) == len(set(baseline)) == 47, len(baseline)
runs = {}
fails = {}
skips = {}
files = sorted(folder.rglob("*.xml"))
for path in files:
    for case in ET.parse(path).getroot().iter("testcase"):
        key = (case.get("classname") or "", case.get("name") or "")
        runs[key] = runs.get(key, 0) + 1
        if case.find("failure") is not None or case.find("error") is not None:
            fails[key] = fails.get(key, 0) + 1
        elif case.find("skipped") is not None:
            skips[key] = skips.get(key, 0) + 1
state = {key: "failed" if fails.get(key) else ("skipped" if skips.get(key) == count else "passed") for key, count in runs.items()}
counts = {"passed": 0, "failed": 0, "skipped": 0}
for value in state.values():
    counts[value] += 1
print(f"{label}: {len(files)} JUnit files, {len(state)} rows: {counts['passed']} passed, {counts['failed']} failed, {counts['skipped']} skipped")
library = {name: value for (classname, name), value in state.items() if classname == "crazyagent"}
if not library:
    print(f"{label}: no rows with classname crazyagent; classnames seen: {sorted({classname for classname, _ in state})[:20]}")
tally = {"passed": 0, "failed": 0, "not run": 0}
for row in baseline:
    value = library.get(row, "not run")
    value = "not run" if value == "skipped" else value
    tally[value] += 1
    seen = runs.get(("crazyagent", row), 0)
    print(f"BASELINE {value} ({fails.get(('crazyagent', row), 0)} of {seen} runs failed): {row}")
print(f"{label} BASELINE 47: {tally['passed']} passed, {tally['failed']} failed, {tally['not run']} not run")
others = sorted(f"{classname} {name}" for (classname, name), value in state.items() if value == "failed" and not (classname == "crazyagent" and name in baseline))
print(f"{label} FAILED OUTSIDE THE BASELINE: {len(others)}")
for row in others:
    classname, name = row.split(" ", 1)
    print(f"OTHER failed ({fails[(classname, name)]} of {runs[(classname, name)]} runs): {row}")
