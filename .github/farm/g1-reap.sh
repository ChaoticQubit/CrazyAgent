#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/g1-reap"
mkdir -p "$out"
base=6bd700abd
files="crates/crazyagent/src/engine/master_host_fixture.rs crates/crazyagent/src/engine/codex_host/process_custody.rs crates/crazyagent/src/engine/claude_host/process.rs crates/crazyagent/src/engine/codex_host/process_tests.rs crates/crazyagent/src/engine/claude_host/tests.rs"
rows="engine::codex_host::process_tests::a_codex_host_whose_harness_it_can_never_reap_still_ends_and_releases_the_lease engine::claude_host::tests::a_claude_master_whose_harness_it_can_never_reap_still_ends_and_releases_the_lease"
cpus=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
bad=0
build() {
  if [ "${RUNNER_OS:-}" = Linux ]; then
    cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper-$1.txt" 2>&1 || { echo "::error::landlock helper build failed at $1"; tail -20 "$out/helper-$1.txt"; exit 1; }
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
    ls -l target/debug/deps/crazyagent-landlock
  fi
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-$1.txt" 2>&1
  local code=$?
  tail -2 "$out/build-$1.txt"
  [ "$code" -eq 0 ] || { echo "::error::lib test build failed at $1 rc=$code"; grep -E '^error' -A8 "$out/build-$1.txt" | head -40; exit 1; }
}
runs() {
  local label=$1 n=$2 row short fails i
  for row in $rows; do
    short=${row##*::}
    fails=0
    for i in $(seq 1 "$n"); do
      if ! cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$label-$short-$i.txt" 2>&1; then
        fails=$((fails + 1))
        grep -hE 'panicked at|reaped its harness first|lease was never released' -A1 "$out/$label-$short-$i.txt" | head -3
      fi
      grep -qE '^test .* ok$' "$out/$label-$short-$i.txt" || grep -qE 'FAILED' "$out/$label-$short-$i.txt" || { echo "::error::$short did not run at $label"; bad=1; }
      if grep -qE 'master_host_fixture.rs:2[0-9][0-9]:[0-9]+:$' "$out/$label-$short-$i.txt" && grep -qE 'Unavailable\(' "$out/$label-$short-$i.txt"; then echo "::error::$short at $label failed in fixture setup (sandbox unavailable), not the race"; bad=1; fi
    done
    echo "ROWS $label $short: $fails of $n failed"
    echo "$label $short $fails" >> "$out/counts"
  done
}
hogs=()
load_on() {
  local i
  for i in $(seq 1 $((cpus * 2))); do yes > /dev/null & hogs+=($!); done
  sleep 2
  echo "LOAD on: $((cpus * 2)) cpu hogs; $(uptime)"
}
load_off() { kill "${hogs[@]}" 2> /dev/null; pkill -x yes 2> /dev/null; hogs=(); echo "LOAD off: $(uptime)"; }
echo "== RED: the five files at $base"
git checkout "$base" -- $files
git status --short
build red
runs red-quiet 10
load_on
runs red-loaded 10
load_off
git checkout HEAD -- $files
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "::error::tree dirty after red"; git status --porcelain; exit 1; }
echo "== GREEN: the tip $(git rev-parse --short=9 HEAD)"
build green
runs green-quiet 10
load_on
runs green-loaded 10
load_off
cat "$out/counts"
green_fails=$(awk '$1 ~ /^green/ {s += $3} END {print s + 0}' "$out/counts")
red_fails=$(awk '$1 ~ /^red/ {s += $3} END {print s + 0}' "$out/counts")
echo "SUMMARY red fails $red_fails of 40, green fails $green_fails of 40"
[ "$green_fails" -eq 0 ] || { echo "::error::a green run failed"; bad=1; }
exit "$bad"
