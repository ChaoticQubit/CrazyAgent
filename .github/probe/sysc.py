"""Counts one process's syscalls in an exported xctrace syscall table, split by stack.
Inputs: the exported XML path and a substring of the process name. Output: JSON of counts
per syscall name for the reset, the row's call and the rest, decided by each stack's frames."""
import sys, re, json, collections
import xml.etree.ElementTree as ET

RESET = re.compile(r'(sized(::|\d+)reset|storage(::|\d+)reset|copy_tree|clear_backups|remove_dir_all)')
CALL = re.compile(r'(recovery\d*put_back|put_back_with|restore_pinned|upgrade\d*upgrade|upgrade::upgrade|migrations\d*backup|migrations::backup|events\d*persist|events::persist|write_restore_record|private_file|recorded|time_tx|append)')
FCNTL = {'0x33': 'F_FULLFSYNC', '0x55': 'F_BARRIERFSYNC', '0x7': 'F_GETLK', '0x8': 'F_SETLK', '0x9': 'F_SETLKW', '0x3': 'F_GETFL', '0x4': 'F_SETFL', '0x1': 'F_GETFD', '0x2': 'F_SETFD', '0x32': 'F_GETPATH', '0x29': 'F_NOCACHE', '0x2c': 'F_RDAHEAD'}
path, proc = sys.argv[1], sys.argv[2]
ids = {}
counts = {'reset': collections.Counter(), 'call': collections.Counter(), 'other': collections.Counter()}

def resolve(el):
    r = el.get('ref')
    return ids.get(r, el) if r else el

for ev, el in ET.iterparse(path, events=('end',)):
    i = el.get('id')
    if i: ids[i] = el
    if el.tag != 'row': continue
    kids = list(el)
    procel = next((resolve(k) for k in kids if k.tag == 'process'), None)
    if procel is None or proc not in (procel.get('fmt') or ''):
        continue
    sc = next((resolve(k) for k in kids if k.tag == 'syscall'), None)
    bt = next((resolve(k) for k in kids if k.tag == 'tagged-backtrace'), None)
    name = (sc.get('fmt') if sc is not None else '?')
    if 'fcntl' in name:
        assert len(kids) >= 13, len(kids)
        arg2 = resolve(kids[8])
        cmd = arg2.get('fmt') if arg2.tag == 'syscall-arg' else None
        name = f"{name}({FCNTL.get(cmd, cmd)})"
    frames = ' '.join((resolve(f).get('name') or '') for f in (bt if bt is not None else []))
    kind = 'reset' if RESET.search(frames) else 'call' if CALL.search(frames) else 'other'
    counts[kind][name] += 1
json.dump({k: dict(v) for k, v in counts.items()}, sys.stdout)
