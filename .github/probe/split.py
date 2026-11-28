"""Splits a folded flame file of one perf row into the row closure's children.
Inputs: the folded path. Output: printed shares of each child (reset, timer, the call),
the self and inclusive top frames inside the call, demangled through c++filt."""
import sys, re, subprocess, collections

RESET = re.compile(r'(reset|copy_tree|clear_backups|remove_dir_all|fs::copy|named_copy|write_restore_record\b(?!.*recovery))')
TIMER = re.compile(r'(Instant::now|nanos|mach_absolute_time|elapsed)')
CLOSURE = re.compile(r'(storage_\w+_calls\{c\}|time_tx::\{c\}|time_appender::\{c\}|settings_save_quit\{c\})$')
FALLBACK = re.compile(r'(perf_kit::time|perf_tests::sampled)')

def demangle(names):
    out = subprocess.run(['c++filt'], input='\n'.join(names), capture_output=True, text=True).stdout.splitlines()
    clean = []
    for s in out:
        s = re.sub(r'\[[0-9a-f]{16,}\]', '', s)
        while True:
            t = re.sub(r'<[^<>]*>', '', s)
            if t == s: break
            s = t
        s = re.sub(r'::\{closure#\d+\}', '{c}', s).replace('::::', '::')
        clean.append(s.strip())
    return dict(zip(names, clean))

stacks = []
names = set()
for line in open(sys.argv[1]):
    st, n = line.rstrip('\n').rsplit(' ', 1)
    fr = [f.split(' [')[0] for f in st.split(';')]
    names.update(fr); stacks.append((fr, int(n)))
d = demangle(sorted(names))
tot = sum(n for _, n in stacks)
kids = collections.Counter(); leaves = collections.defaultdict(collections.Counter)
callself = collections.Counter(); callincl = collections.Counter(); callsum = 0
for fr, n in stacks:
    fr = [d[f] for f in fr]
    idx = [i for i, f in enumerate(fr) if CLOSURE.search(f)]
    if not idx:
        idx = [i for i, f in enumerate(fr) if FALLBACK.search(f)]
    if not idx or idx[-1] + 1 >= len(fr):
        kids['(row frame self / outside)'] += n; continue
    i = idx[-1]; k = fr[i + 1]
    kind = 'reset' if RESET.search(k) else 'timer' if TIMER.search(k) else 'call'
    kids[f'{kind}: {k}'] += n; leaves[f'{kind}: {k}'][fr[-1]] += n
    if kind == 'call':
        callsum += n; callself[fr[-1]] += n
        for f in set(fr[i + 1:]): callincl[f] += n
print(f'samples {tot}')
print('CHILDREN of the row closure (share of all samples)')
for k, c in kids.most_common(10):
    top = ', '.join(f'{l[:40]} {100*v/tot:.1f}%' for l, v in leaves[k].most_common(3))
    print(f'  {100*c/tot:5.1f}%  {k[:80]}  | {top}')
share = lambda kind: sum(c for k, c in kids.items() if k.startswith(kind)) * 100 / tot
print(f'SPLIT call {share("call"):.1f}%  reset {share("reset"):.1f}%  timer {share("timer"):.1f}%  outside {share("(row"):.1f}%')
if callsum:
    print('CALL SELF top 10 (share of the call)')
    for f, c in callself.most_common(10): print(f'  {100*c/callsum:5.1f}%  {f[:100]}')
    print('CALL TOTAL top 10 (share of the call)')
    for f, c in callincl.most_common(11)[1:]: print(f'  {100*c/callsum:5.1f}%  {f[:100]}')
