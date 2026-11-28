"""Turn a macOS `sample` report into folded stacks, a top-frames table and a flame graph SVG.

Usage: python3 flame.py SAMPLE.txt OUT.svg [--thread SUBSTRING] [--top N]
Inputs: the text file `sample <pid> <secs> -file SAMPLE.txt` writes.
Outputs: OUT.svg (flame graph), OUT.folded (one `a;b;c count` line per stack),
and a table on stdout of the top N frames by self and by total samples.
"""

import html
import re
import sys
from collections import defaultdict

LINE = re.compile(r"^(?P<lead>[ +!:|]*)(?P<count>\d+) (?P<rest>.+)$")


def frame_name(rest):
    """Return the frame label of one call-graph line: function plus its library, offsets dropped."""
    name, _, tail = rest.partition("  (in ")
    lib = tail.split(")", 1)[0] if tail else ""
    name = name.strip()
    assert name, rest
    return f"{name} [{lib}]" if lib else name


def parse(text, thread=None):
    """Parse the `Call graph:` section into a list of (stack tuple, self samples)."""
    lines = text.splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if l.startswith("Call graph:")) + 1
    except StopIteration:
        raise SystemExit("no 'Call graph:' section in the sample file")
    stack = []
    nodes = []
    keep = thread is None
    for line in lines[start:]:
        if not line.strip():
            break
        m = LINE.match(line)
        if not m:
            continue
        depth = len(m.group("lead"))
        count = int(m.group("count"))
        name = frame_name(m.group("rest"))
        while stack and stack[-1][0] >= depth:
            stack.pop()
        if not stack:
            keep = thread is None or thread in m.group("rest")
        stack.append((depth, name))
        if keep:
            nodes.append((tuple(n for _, n in stack), count))
    totals = {}
    for path, count in nodes:
        totals[path] = totals.get(path, 0) + count
    child_sum = defaultdict(int)
    for path, count in totals.items():
        if len(path) > 1:
            child_sum[path[:-1]] += count
    out = []
    for path, count in totals.items():
        own = count - child_sum[path]
        assert own >= 0, (path, count, child_sum[path])
        if own:
            out.append((path, own))
    return out


def tables(stacks, top):
    """Return (self, total) lists of (frame, samples), largest first, top entries each."""
    self_c = defaultdict(int)
    total_c = defaultdict(int)
    for path, n in stacks:
        self_c[path[-1]] += n
        for f in set(path):
            total_c[f] += n
    key = lambda kv: -kv[1]
    return sorted(self_c.items(), key=key)[:top], sorted(total_c.items(), key=key)[:top]


def svg(stacks, width=1200, row=16):
    """Render folded stacks as a flame graph SVG string, root at the bottom."""
    tree = {}
    for path, n in stacks:
        node = tree
        for f in path:
            node = node.setdefault(f, {"_n": 0, "_k": {}})
            node["_n"] += n
            node = node["_k"]
    total = sum(n for _, n in stacks)
    assert total > 0
    depth_max = max(len(p) for p, _ in stacks)
    height = (depth_max + 2) * row
    rects = []

    def walk(children, x, depth):
        for name, node in sorted(children.items()):
            w = node["_n"] / total * width
            y = height - (depth + 1) * row
            hue = 20 + (hash(name) % 40)
            label = html.escape(name)
            pct = node["_n"] / total * 100
            text = label if w > 7 * len(name) else (label[: int(w / 7) - 1] + ".." if w > 30 else "")
            rects.append(
                f'<g><title>{label} ({node["_n"]} samples, {pct:.1f}%)</title>'
                f'<rect x="{x:.2f}" y="{y}" width="{max(w - 0.5, 0.1):.2f}" height="{row - 1}" fill="hsl({hue},85%,60%)"/>'
                f'<text x="{x + 3:.2f}" y="{y + row - 4}" font-size="11" font-family="monospace">{text}</text></g>'
            )
            walk(node["_k"], x, depth + 1)
            x += w

    walk(tree, 0.0, 0)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">'
        f'<rect width="100%" height="100%" fill="#fff"/>' + "".join(rects) + "</svg>"
    )


def main(argv):
    """Read the sample file named in argv, write the SVG and folded file, print the top-frames table."""
    if len(argv) < 3:
        raise SystemExit(__doc__)
    src, out = argv[1], argv[2]
    thread = argv[argv.index("--thread") + 1] if "--thread" in argv else None
    top = int(argv[argv.index("--top") + 1]) if "--top" in argv else 15
    stacks = parse(open(src).read(), thread)
    if not stacks:
        raise SystemExit("no samples parsed")
    open(out, "w").write(svg(stacks))
    with open(out.rsplit(".", 1)[0] + ".folded", "w") as f:
        for path, n in sorted(stacks):
            f.write(";".join(path) + f" {n}\n")
    total = sum(n for _, n in stacks)
    s, t = tables(stacks, top)
    print(f"samples {total}")
    print("SELF")
    for name, n in s:
        print(f"{n:8d} {n / total * 100:5.1f}%  {name}")
    print("TOTAL")
    for name, n in t:
        print(f"{n:8d} {n / total * 100:5.1f}%  {name}")


if __name__ == "__main__":
    main(sys.argv)
