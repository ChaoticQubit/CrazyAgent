"""Prints the verdict table of one farm perf sitting reading from run.sh raw logs.

Usage: sitting_summary.py <reading> <condition> <new label> <base label> <log>. The log is the
raw `run.sh --inner` file. Output: a header with commits, run counts and load1 per block, then
one line per paired row with the medians per block, the gap, the A/A gap and both verdicts.
"""

import statistics
import sys

AA = "2.5-aa"


WHOLE = ({"p50_ns", "p95_ns", "n"}, {"p50", "p95", "n"})


def runs_of(path):
    """Reads the measures of a raw log in order, skipping a PERF line cut before its p95 or
    n, as a killed run leaves its last line. Input: the path. Output: a list of
    (header dict, {row key: (p50 ns, p95 ns)}) and the count of cut lines."""
    runs = []
    cut = 0
    for raw in open(path, encoding="utf-8", errors="replace"):
        line = raw.rstrip("\n")
        if line.startswith("PERF-RUN "):
            header = dict(pair.split("=", 1) for pair in line.split()[1:] if "=" in pair)
            runs.append((header, {}))
        elif line.startswith("PERF ") and runs:
            words = line.split()
            values = dict(pair.split("=", 1) for pair in words[4:] if "=" in pair)
            if len(words) < 5 or not any(need <= set(values) for need in WHOLE):
                cut += 1
                continue
            if "p50_ns" in values:
                p50, p95 = int(values["p50_ns"]), int(values["p95_ns"])
            else:
                p50, p95 = int(values["p50"]) * 1000, int(values["p95"]) * 1000
            runs[-1][1][" ".join(words[1:4])] = (p50, p95)
    assert cut >= 0
    return runs, cut


def blocks_of(runs):
    """Groups the measures into ABBA blocks by the plan order in the headers. Input: the
    measures. Output: (block size, list of blocks, each a list of measures)."""
    orders = {header["order"] for header, _ in runs}
    assert len(orders) == 1, f"one plan order, got {orders}"
    size = len(orders.pop().split(","))
    assert size > 0 and len(runs) % size == 0, f"{len(runs)} runs, block size {size}"
    return size, [runs[index:index + size] for index in range(0, len(runs), size)]


def median_of(runs, label, key, field):
    """The median of one row's field over the measures of one label. Inputs: the measures,
    the label, the row key and 0 for p50 or 1 for p95. Output: the median, or None."""
    values = [rows[key][field] for header, rows in runs if header["build"] == label and key in rows]
    return statistics.median(values) if values else None


def main(reading, condition, new, base, path):
    """Prints the table. Inputs: the reading name, the condition, the new and base labels
    and the raw log path. Output: none; it exits 1 when the log is not a whole plan."""
    runs, cut = runs_of(path)
    assert runs, f"{path}: no measures"
    print(f"SITTING cut_lines={cut}")
    size, blocks = blocks_of(runs)
    labels = {header["build"] for header, _ in runs}
    assert labels == {new, base, AA}, f"labels {sorted(labels)}"
    counts = {label: sum(1 for header, _ in runs if header["build"] == label) for label in sorted(labels)}
    assert len(set(counts.values())) == 1, f"run counts {counts}"
    commits = {label: sorted({header["commit"] for header, _ in runs if header["build"] == label}) for label in sorted(labels)}
    assert all(len(found) == 1 for found in commits.values()), f"commits {commits}"
    print(f"SITTING reading={reading} condition={condition} log={path} blocks={len(blocks)} block_size={size} runs={counts}")
    print("SITTING commits " + " ".join(f"{label}={found[0]}" for label, found in commits.items()))
    for index, block in enumerate(blocks, 1):
        loads = [float(header["load1"]) for header, _ in block]
        print(f"SITTING block={index} load1_first={loads[0]:.2f} load1_last={loads[-1]:.2f} load1_min={min(loads):.2f} load1_max={max(loads):.2f} first={block[0][0]['date']} last={block[-1][0]['date']}")
    keys = [key for key in runs[0][1] if all(key in rows for _, rows in runs)]
    slower = strict_fail = 0
    for key in keys:
        new_p95, base_p95, aa_p95 = (median_of(runs, label, key, 1) for label in (new, base, AA))
        new_p50, base_p50 = median_of(runs, new, key, 0), median_of(runs, base, key, 0)
        per_new = [median_of(block, new, key, 1) for block in blocks]
        per_base = [median_of(block, base, key, 1) for block in blocks]
        slower_blocks = sum(1 for a, b in zip(per_new, per_base) if a > b)
        gap = (new_p95 - base_p95) / base_p95 if base_p95 else 0.0
        aa_gap = abs(new_p95 - aa_p95) / new_p95 if new_p95 else 0.0
        rule = "SLOWER" if slower_blocks == len(blocks) and gap > aa_gap else "OK"
        strict = "PASS" if new_p95 <= base_p95 else "FAIL"
        slower += rule == "SLOWER"
        strict_fail += strict == "FAIL"
        print(
            f"ROW {key} base_p50={base_p50:.0f} new_p50={new_p50:.0f} base_p95={base_p95:.0f} new_p95={new_p95:.0f} aa_p95={aa_p95:.0f} "
            f"gap={gap * 100:+.1f}% aa_gap={aa_gap * 100:.1f}% slower_blocks={slower_blocks}/{len(blocks)} "
            f"block_new={[round(v) for v in per_new]} block_base={[round(v) for v in per_base]} rule={rule} strict={strict}"
        )
    for key in [key for key in runs[0][1] if key not in keys]:
        if all(key in rows for header, rows in runs if header["build"] in (new, AA)):
            print(f"NEW-ONLY {key} new_p50={median_of(runs, new, key, 0):.0f} new_p95={median_of(runs, new, key, 1):.0f} aa_p95={median_of(runs, AA, key, 1):.0f}")
    print(f"VERDICT reading={reading} condition={condition} rows={len(keys)} slower_rule={'FAIL' if slower else 'PASS'} slower_rows={slower} strict_fail_rows={strict_fail}")


if __name__ == "__main__":
    main(*sys.argv[1:6])
