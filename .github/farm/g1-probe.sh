#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/g1"
mkdir -p "$out"
row=real_sqlite_contention_and_queue_saturation_are_bounded
cpus=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
echo "runner $RUNNER_OS $(uname -m) cpus=$cpus sqlite3-cli=$(sqlite3 --version 2>/dev/null | cut -d' ' -f1)"
cargo test -p crazyagent --features owner-trace --locked --test engine_replay --no-run --message-format=json > "$out/build.json" 2> "$out/build.txt"
code=$?
tail -3 "$out/build.txt"
[ "$code" -eq 0 ] || { echo "::error::engine_replay build failed rc=$code"; grep -E '^error' -A8 "$out/build.txt" | head -40; exit 1; }
bin=$(node -e 'for (const line of require("fs").readFileSync(process.argv[1], "utf8").split("\n")) { if (!line) continue; const m = JSON.parse(line); if (m.reason === "compiler-artifact" && m.executable && m.target.name === "engine_replay") console.log(m.executable); }' "$out/build.json" | tail -1)
[ -x "$bin" ] || { echo "::error::no engine_replay binary"; exit 1; }
echo "binary $bin"
rows() {
  local label=$1 n=$2 fails=0 i
  for i in $(seq 1 "$n"); do
    if ! "$bin" --exact "$row" > "$out/$label-$i.txt" 2>&1; then
      fails=$((fails + 1))
      grep -hE 'panicked at|EngineError|not_settled' "$out/$label-$i.txt" | head -3
    fi
  done
  echo "ROW $label: $fails of $n failed"
}
probes() {
  local label=$1 rounds=$2 name
  for name in g1_probe_saturation_timeline g1_probe_saturation_timeline_after_a_read; do
    G1_ROUNDS=$rounds "$bin" --exact "$name" --nocapture > "$out/$label-$name.txt" 2>&1
    echo "PROBE $label $name rc=$?"
    grep -hE '^G1PROBE (sqlite|worker)' "$out/$label-$name.txt" | sort -u
    grep -hE '^G1PROBE warm' "$out/$label-$name.txt" | grep -E 'first_full=true|after=err|after=rev=Cursor\([02-9]' | head -12
    grep -hE '^G1PROBE-SUMMARY' "$out/$label-$name.txt"
    grep -hE '^G1PROBE warm' "$out/$label-$name.txt" | head -3
  done
}
parallel_rows() {
  local label=$1 lanes=$2 n=$3 lane
  for lane in $(seq 1 "$lanes"); do
    ( rows "$label-lane$lane" "$n" ) > "$out/$label-lane$lane.sum" 2>&1 &
  done
  wait
  cat "$out/$label"-lane*.sum
}
hogs=()
load_on() {
  local i
  for i in $(seq 1 $((cpus * 2))); do
    yes > /dev/null &
    hogs+=($!)
  done
  ( while :; do dd if=/dev/zero of="$RUNNER_TEMP/g1-disk" bs=1048576 count=512 2> /dev/null; sync; rm -f "$RUNNER_TEMP/g1-disk"; done ) &
  hogs+=($!)
  sleep 2
  echo "LOAD on: $((cpus * 2)) cpu hogs and a disk writer; $(uptime)"
}
load_off() {
  kill "${hogs[@]}" 2> /dev/null
  pkill -x yes 2> /dev/null
  pkill -x dd 2> /dev/null
  hogs=()
  rm -f "$RUNNER_TEMP/g1-disk"
  echo "LOAD off: $(uptime)"
}
echo "== QUIET"
rows quiet 20
probes quiet 30
parallel_rows quiet-par "$cpus" 5
echo "== LOADED"
load_on
rows loaded 20
probes loaded 30
parallel_rows loaded-par "$cpus" 5
load_off
echo "== whole engine_replay target, loaded, 5 times"
load_on
for i in 1 2 3 4 5; do
  "$bin" --skip g1_probe > "$out/target-$i.txt" 2>&1
  echo "target run $i rc=$? $(grep -E '^test result:' "$out/target-$i.txt" | tail -1)"
  grep -hE 'panicked at' "$out/target-$i.txt" | head -3
done
load_off
exit 0
