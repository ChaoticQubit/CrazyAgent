"""Builds the engine's bundled model price snapshot from a local CodeBurn price file.

Input: --source, a local copy of CodeBurn's src/data/litellm-snapshot.json; --as-of, its date as
YYYY-MM-DD; --out, the file to write. Output: sorted JSON with integer picodollar rates per
token. It reads only the named file, opens no network connection and writes nothing on a refusal.
A name in DROPPED is left out: CodeBurn prices it through an alias table the engine does not port,
so it must read as no price, never as the source row's wrong one.
"""
import argparse
import datetime
import hashlib
import json
import pathlib
import re
import sys

NAME = re.compile(r"^(?:claude-|gpt-|codex-|o[134](?:-|$))[a-z0-9._-]{0,60}$")
DATED = re.compile(r"-[0-9]{8}$")
DROPPED = frozenset({"claude-4-opus"})
MODELS_MAX = 512
RATE_MAX = 1e-3
PICO = 1e12
FAST_MAX = 20


def pico(rate, optional=False):
    """Converts a dollars-per-token rate. Inputs: the rate, whether null is allowed. Output: picodollars or None."""
    if rate is None and optional:
        return None
    if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not 0 < rate <= RATE_MAX:
        raise ValueError(f"bad rate {rate!r}")
    return round(rate * PICO)


def tier_of(tier):
    """Converts a long-context tier. Input: the source tier or None. Output: the snapshot tier or None."""
    if tier is None:
        return None
    if not isinstance(tier, dict) or not isinstance(tier.get("threshold"), int):
        raise ValueError("bad tier")
    converted = {"threshold": tier["threshold"], "input": pico(tier.get("input")), "output": pico(tier.get("output")), "cache_write": pico(tier.get("cacheWrite"), True), "cache_read": pico(tier.get("cacheRead"), True)}
    if tier.get("fast") is not None:
        converted["fast_milli"] = fast_milli(tier["fast"])
    return converted


def fast_milli(fast):
    """Converts a fast multiplier. Input: the multiplier. Output: thousandths."""
    if isinstance(fast, bool) or not isinstance(fast, (int, float)) or not 1 <= fast <= FAST_MAX:
        raise ValueError("bad fast multiplier")
    return round(fast * 1000)


def rates_of(entry):
    """Converts one compact entry. Input: [input, output, write, read, fast, tier, ...], where fast and tier may be absent. Output: the snapshot rates."""
    if not isinstance(entry, list) or len(entry) < 4:
        raise ValueError("bad entry")
    rates = {"input": pico(entry[0]), "output": pico(entry[1]), "cache_write": pico(entry[2], True), "cache_read": pico(entry[3], True)}
    fast = entry[4] if len(entry) > 4 else None
    if fast is not None:
        rates["fast_milli"] = fast_milli(fast)
    tier = tier_of(entry[5] if len(entry) > 5 else None)
    if tier is not None:
        rates["tier"] = tier
    return rates


def build(raw, as_of):
    """Builds the snapshot. Inputs: the source bytes and the date. Output: the snapshot object."""
    datetime.date.fromisoformat(as_of)
    if len(as_of) != 10:
        raise ValueError("bad date")
    source = json.loads(raw)
    if not isinstance(source, dict):
        raise ValueError("source is not an object")
    models = {name: rates_of(entry) for name, entry in source.items() if NAME.match(name) and not DATED.search(name) and name not in DROPPED}
    if not 0 < len(models) <= MODELS_MAX:
        raise ValueError(f"{len(models)} models")
    return {"as_of": as_of, "source_sha256": hashlib.sha256(raw).hexdigest(), "models": dict(sorted(models.items()))}


def main():
    """Parses the arguments and writes the snapshot. Output: the process exit code."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--as-of", required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        snapshot = build(args.source.read_bytes(), args.as_of)
    except (OSError, ValueError) as problem:
        print(f"refused: {problem}", file=sys.stderr)
        return 1
    args.out.write_text(json.dumps(snapshot, indent=1, sort_keys=False) + "\n", encoding="utf-8")
    print(f"{len(snapshot['models'])} models as of {snapshot['as_of']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
