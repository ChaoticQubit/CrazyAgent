#!/usr/bin/env bash
set -euo pipefail

if [ -n "${PERF_RUNS+set}" ]; then
  echo "run.sh: PERF_RUNS is gone, use PERF_BLOCKS (ABBA blocks, default 2)" >&2
  exit 2
fi

packages_of() {
  local dir=$1 tests manifest
  for tests in "$dir"/crates/*/src/perf_tests.rs "$dir"/crates/*/src/perf_tests "$dir"/crates/*/*/src/perf_tests.rs "$dir"/crates/*/*/src/perf_tests; do
    [ -e "$tests" ] || continue
    manifest=${tests%/src/*}/Cargo.toml
    printf -- '-p\n%s\n' "$(awk -F'"' '/^name = / { print $2; exit }' "$manifest")"
  done
}

busy_slots() {
  local sprint=$1 busy=0 d p
  for d in "$sprint"/slots/[0-9]* "$sprint"/slots/L1; do
    [ -d "$d" ] || continue
    p=$(cat "$d/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -0 "$p" 2>/dev/null && busy=$((busy + 1))
  done
  echo "$busy"
}

build_of() {
  if [ -d "$1/crates/kernel" ]; then echo 2.5; else echo 2.4.4; fi
}

plan() {
  local dir index forward="" backward=""
  local -a first=() rest=() ordered=()
  for dir in "$@"; do
    if [ "$(build_of "$dir")" = 2.5 ]; then first+=("$dir"); else rest+=("$dir"); fi
  done
  ordered=(${first[@]+"${first[@]}"} ${rest[@]+"${rest[@]}"})
  for dir in "${ordered[@]}"; do forward="$forward,$(build_of "$dir")"; done
  for ((index = ${#ordered[@]} - 1; index >= 0; index--)); do
    backward="$backward,$(build_of "${ordered[$index]}")"
  done
  echo "order=${forward#,}$backward"
  printf '%s\n' "${ordered[@]}"
}

load_one() {
  local load
  load=$(python3 -I -c 'import os; print(f"{os.getloadavg()[0]:.2f}")') || load=""
  if ! [[ "$load" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    echo "run.sh: no load average (read '$load')" >&2
    return 1
  fi
  echo "$load"
}

measure() {
  local dir=$1 sprint=$2 order=$3 build load1 others condition=loaded
  local -a packages
  local word
  packages=()
  while IFS= read -r word; do packages+=("$word"); done < <(packages_of "$dir")
  build=$(build_of "$dir")
  load1=$(load_one)
  others=$(($(busy_slots "$sprint") - 1))
  [ "$others" -ge 0 ] || others=0
  awk -v l="$load1" 'BEGIN { exit !(l < 4) }' && [ "$others" -eq 0 ] && condition=quiet
  echo "PERF-RUN build=$build commit=$(git -C "$dir" rev-parse --short HEAD) date=$(date +%Y-%m-%dT%H:%M) condition=$condition load1=$load1 slots=$others runs=1 order=$order"
  (cd "$dir" && CARGO_TARGET_DIR="$dir/.local-state/target" cargo test --release "${packages[@]}" --lib perf_tests -- --ignored --nocapture --test-threads=1 2>&1) | sed -E 's/^test [^ ]+ \.\.\. (PERF )/\1/'
  return "${PIPESTATUS[0]}"
}

median() {
  python3 -I - "$1" "$2" <<'PY'
import math
import statistics
import sys

path, want = sys.argv[1], int(sys.argv[2])
builds = {}
block = None
for raw in open(path):
    line = raw.rstrip("\n")
    if line.startswith("PERF-RUN "):
        header = dict(pair.split("=", 1) for pair in line.split()[1:])
        block = (header, {})
        builds.setdefault(header["build"], []).append(block)
    elif line.startswith("PERF ") and block is not None:
        words = line.split()
        values = dict(pair.split("=", 1) for pair in words[4:])
        nanoseconds = "p50_ns" in values
        if nanoseconds:
            p50, p95 = int(values["p50_ns"]), int(values["p95_ns"])
        else:
            p50, p95 = int(values["p50"]) * 1000, int(values["p95"]) * 1000
        block[1][tuple(words[1:4])] = (p50, p95, values["n"], int(values.get("batch", "1")), nanoseconds)
for build, blocks in builds.items():
    assert len(blocks) == want, f"{build}: {len(blocks)} runs, wanted {want}"
    headers = [header for header, _ in blocks]
    assert len({header["commit"] for header in headers}) == 1
    condition = "quiet" if all(h["condition"] == "quiet" for h in headers) else "loaded"
    orders = {header.get("order", "") for header in headers}
    assert len(orders) == 1
    order = f" order={orders.pop()}" if "order" in headers[0] else ""
    load1 = statistics.median(float(h["load1"]) for h in headers)
    slots = max(int(h["slots"]) for h in headers)
    print(f"PERF-RUN build={build} commit={headers[0]['commit']} date={headers[0]['date']} condition={condition} load1={load1:.2f} slots={slots} runs={want}{order}")
    for key in blocks[0][1]:
        rows = [rows.get(key) for _, rows in blocks]
        if any(row is None for row in rows):
            continue
        p50 = math.floor(statistics.median(row[0] for row in rows))
        p95 = math.floor(statistics.median(row[1] for row in rows))
        n, batch = rows[0][2], rows[0][3]
        head = "PERF " + " ".join(key)
        if p50 < 10_000 or all(row[4] for row in rows):
            print(f"{head} p50_ns={p50} p95_ns={p95} n={n} batch={batch}")
        elif batch == 1:
            print(f"{head} p50={p50 // 1000} p95={p95 // 1000} n={n}")
        else:
            print(f"{head} p50={p50 // 1000} p95={p95 // 1000} n={n} batch={batch}")
PY
}

if [ "${1:-}" = "--median" ]; then
  median "${2:?}" "${3:?}"
  exit 0
fi

if [ "${1:-}" = "--plan" ]; then
  shift
  plan "$@"
  exit 0
fi

if [ "${1:-}" = "--inner" ]; then
  shift
  sprint=$1 blocks=$2 log=$3
  shift 3
  planned=()
  while IFS= read -r word; do planned+=("$word"); done < <(plan "$@")
  order=${planned[0]#order=}
  ordered=("${planned[@]:1}")
  for dir in "${ordered[@]}"; do
    measure "$dir" "$sprint" "$order" > /dev/null
  done
  for _ in $(seq 1 "$blocks"); do
    for dir in "${ordered[@]}"; do
      measure "$dir" "$sprint" "$order" >> "$log"
    done
    for ((index = ${#ordered[@]} - 1; index >= 0; index--)); do
      measure "${ordered[$index]}" "$sprint" "$order" >> "$log"
    done
  done
  median "$log" $((blocks * 2))
  exit 0
fi

layer=${1:?usage: run.sh <layer> [2.4.4 checkout]}
other=${2:-}
blocks=${PERF_BLOCKS:-2}
[ "$layer" = 1 ] || { echo "run.sh: layer $layer has no rows yet" >&2; exit 2; }
self=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
root=$(git rev-parse --show-toplevel)
main=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
sprint=${PERF_SPRINT:-$main/.local-state/flow-sprint}
[ -f "$sprint/slot.sh" ] && [ -f "$sprint/env.sh" ] || { echo "run.sh: no slot.sh or env.sh under $sprint" >&2; exit 2; }
dirs=("$root")
[ -n "$other" ] && dirs+=("$(cd "$other" && git rev-parse --show-toplevel)")
cd "$root"
source "$sprint/env.sh"
export SLOT_TASK=${SLOT_TASK:-perf} SLOT_LINE=${SLOT_LINE:-v250}
for dir in "${dirs[@]}"; do
  packages=()
  while IFS= read -r word; do packages+=("$word"); done < <(packages_of "$dir")
  [ ${#packages[@]} -gt 0 ] || { echo "run.sh: no perf_tests under $dir" >&2; exit 2; }
  (cd "$dir" && CARGO_TARGET_DIR="$dir/.local-state/target" bash "$sprint/slot.sh" cargo test --release "${packages[@]}" --lib --no-run)
done
if [ -n "${PERF_WAIT_QUIET:-}" ]; then
  while :; do
    load1=$(load_one)
    awk -v l="$load1" 'BEGIN { exit !(l < 4) }' && [ "$(busy_slots "$sprint")" -eq 0 ] && break
    sleep 60
  done
fi
mkdir -p "$root/.local-state/perf"
log="$root/.local-state/perf/L$layer-$(date +%Y%m%dT%H%M%S).log"
bash "$sprint/slot.sh" bash "$self" --inner "$sprint" "$blocks" "$log" "${dirs[@]}"
echo "raw runs: $log"
