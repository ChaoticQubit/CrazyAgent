"""Checks the price snapshot builder on real temporary files. Run it directly; it prints ok."""
import json
import pathlib
import subprocess
import sys
import tempfile

SCRIPT = pathlib.Path(__file__).with_name("refresh_prices.py")
GOOD = {
    "claude-b": [3e-06, 1.5e-05, 3.75e-06, 3e-07, None, {"threshold": 200000, "input": 6e-06, "output": 2.25e-05, "cacheWrite": 7.5e-06, "cacheRead": 6e-07}],
    "claude-a": [5e-06, 2.5e-05, 6.25e-06, 5e-07, 6, None],
    "gpt-5": [1.25e-06, 1e-05, None, 1.25e-07, 2, None, [6.25e-07, 5e-06, None, 6.25e-08, None, None]],
    "azure/gpt-5": [1e-06, 1e-05, None, 1e-07, None, None],
    "claude-a-20251101": [5e-06, 2.5e-05, 6.25e-06, 5e-07, None, None],
    "claude-a@default": [5e-06, 2.5e-05, 6.25e-06, 5e-07, None, None],
    "gemini-x": [1e-06, 1e-06, None, 1e-07, None, None],
}


def run(root, source, as_of="2026-11-08"):
    """Runs the builder. Inputs: the folder, the source data and the date. Output: rc and out path."""
    path = root / "source.json"
    path.write_text(json.dumps(source), encoding="utf-8")
    out = root / "prices.json"
    done = subprocess.run([sys.executable, "-B", "-I", str(SCRIPT), "--source", str(path), "--as-of", as_of, "--out", str(out)], capture_output=True, text=True, check=False)
    return done.returncode, out


with tempfile.TemporaryDirectory() as folder:
    root = pathlib.Path(folder)
    rc, out = run(root, GOOD)
    assert rc == 0
    built = json.loads(out.read_text(encoding="utf-8"))
    assert built["as_of"] == "2026-11-08"
    assert len(built["source_sha256"]) == 64
    assert list(built["models"]) == ["claude-a", "claude-b", "gpt-5"]
    assert built["models"]["claude-a"] == {"input": 5000000, "output": 25000000, "cache_write": 6250000, "cache_read": 500000, "fast_milli": 6000}
    assert built["models"]["gpt-5"]["cache_write"] is None
    assert built["models"]["claude-b"]["tier"] == {"threshold": 200000, "input": 6000000, "output": 22500000, "cache_write": 7500000, "cache_read": 600000}
    first = out.read_bytes()
    assert run(root, GOOD)[0] == 0
    assert out.read_bytes() == first
    for bad_source in ({"claude-a": [-1, 1e-06, None, None, None, None]}, {"claude-a": ["x", 1e-06, None, None, None, None]}, {"claude-a": [1, 1e-06, None, None, None, None]}, {"claude-a": [1e-06]}, []):
        out.unlink(missing_ok=True)
        assert run(root, bad_source)[0] != 0
        assert not out.exists()
    rc, out = run(root, {"claude-c": [5e-06, 2.5e-05, None, None]})
    assert rc == 0
    assert json.loads(out.read_text(encoding="utf-8"))["models"]["claude-c"] == {"input": 5000000, "output": 25000000, "cache_write": None, "cache_read": None}
    rc, out = run(root, {"gpt-t": [1e-06, 1e-06, None, None, 2, {"threshold": 272000, "input": 2e-06, "output": 2e-06, "fast": 1}]})
    assert rc == 0
    assert json.loads(out.read_text(encoding="utf-8"))["models"]["gpt-t"]["tier"]["fast_milli"] == 1000
    assert run(root, GOOD, "last week")[0] != 0
print("ok")
