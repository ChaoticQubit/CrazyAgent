#!/bin/bash
set -euo pipefail

if [ "${1:-}" = "--measure" ]; then
  shift
  build=$1 sprint=$2
  shift 2
  load1=$(sysctl -n vm.loadavg | awk '{print $2}')
  busy=0
  for d in "$sprint"/slots/[0-9]* "$sprint"/slots/L1; do
    [ -d "$d" ] || continue
    p=$(cat "$d/pid" 2>/dev/null || true)
    [ -n "$p" ] && kill -0 "$p" 2>/dev/null && busy=$((busy + 1))
  done
  others=$((busy > 0 ? busy - 1 : 0))
  condition=loaded
  awk -v l="$load1" 'BEGIN { exit !(l < 4) }' && [ "$others" -eq 0 ] && condition=quiet
  echo "PERF-RUN build=$build commit=$(git rev-parse --short HEAD) date=$(date +%Y-%m-%dT%H:%M) condition=$condition load1=$load1 slots=$others runs=1"
  exec cargo "$@"
fi

layer=${1:?usage: run.sh <layer>}
self=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
[ "$layer" = 1 ] || { echo "run.sh: layer $layer has no rows yet" >&2; exit 2; }
root=$(git rev-parse --show-toplevel)
cd "$root"
main=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
sprint=${PERF_SPRINT:-$main/.local-state/flow-sprint}
[ -f "$sprint/slot.sh" ] && [ -f "$sprint/env.sh" ] || { echo "run.sh: no slot.sh or env.sh under $sprint" >&2; exit 2; }
build=2.4.4
[ -d crates/kernel ] && build=2.5
packages=()
for tests in crates/*/src/perf_tests.rs crates/*/src/perf_tests crates/*/*/src/perf_tests.rs crates/*/*/src/perf_tests; do
  [ -e "$tests" ] || continue
  manifest=${tests%/src/*}/Cargo.toml
  packages+=(-p "$(awk -F'"' '/^name = / { print $2; exit }' "$manifest")")
done
[ ${#packages[@]} -gt 0 ] || { echo "run.sh: no perf_tests found" >&2; exit 2; }
source "$sprint/env.sh"
SLOT_TASK=${SLOT_TASK:-perf} SLOT_LINE=${SLOT_LINE:-v250} bash "$sprint/slot.sh" cargo test --release "${packages[@]}" --lib --no-run
SLOT_TASK=${SLOT_TASK:-perf} SLOT_LINE=${SLOT_LINE:-v250} bash "$sprint/slot.sh" bash "$self" --measure "$build" "$sprint" test --release "${packages[@]}" --lib perf_tests -- --ignored --nocapture --test-threads=1
