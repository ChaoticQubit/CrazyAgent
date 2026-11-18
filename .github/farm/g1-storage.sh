#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/g1-storage"
mkdir -p "$out"
red=f9c2ff41c
fixfiles=$(git diff --name-only "$red" HEAD -- crates apps)
new_rows="engine::tests::an_apply_before_the_storage_worker_receives_is_refused_unsent engine::tests::an_apply_the_busy_worker_does_not_take_in_time_is_withdrawn_and_never_lands engine::tests::an_apply_waits_for_a_storage_worker_that_starts_receiving_within_its_bound"
race_rows="engine::tests::a_job_whose_bound_runs_out_at_the_claim_is_withdrawn_and_the_worker_skips_it engine::tests::the_claim_race_at_the_bound_has_one_outcome_and_applies_at_most_once"
saturation=real_sqlite_contention_and_queue_saturation_are_bounded
suites="storage:: engine::tests:: engine::owner:: config:: tui::text::"
cpus=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
bad=0
helper() {
  if [ "${RUNNER_OS:-}" = Linux ]; then
    cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper-$1.txt" 2>&1 || { echo "::error::landlock helper build failed at $1"; tail -20 "$out/helper-$1.txt"; exit 1; }
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  fi
}
build_lib() {
  helper "$1"
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-lib-$1.txt" 2>&1
  local code=$?
  tail -2 "$out/build-lib-$1.txt"
  [ "$code" -eq 0 ] || { echo "::error::lib test build failed at $1 rc=$code"; grep -E '^(error|warning)' -A8 "$out/build-lib-$1.txt" | head -60; exit 1; }
}
build_replay() {
  cargo test -p crazyagent --test engine_replay --locked --no-run > "$out/build-replay-$1.txt" 2>&1
  local code=$?
  tail -2 "$out/build-replay-$1.txt"
  [ "$code" -eq 0 ] || { echo "::error::engine_replay build failed at $1 rc=$code"; grep -E '^error' -A8 "$out/build-replay-$1.txt" | head -40; exit 1; }
}
lib_rows() {
  local label=$1 n=$2 rows=$3 row short fails i
  for row in $rows; do
    short=${row##*::}
    fails=0
    for i in $(seq 1 "$n"); do
      if ! cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$label-$short-$i.txt" 2>&1; then
        fails=$((fails + 1))
        grep -hE 'panicked at' -A4 "$out/$label-$short-$i.txt" | head -6
      fi
      grep -qE '^test .* (ok|FAILED)$' "$out/$label-$short-$i.txt" || { echo "::error::$short did not run at $label"; bad=1; }
    done
    echo "ROWS $label $short: $fails of $n failed"
    echo "$label $short $fails $n" >> "$out/counts"
  done
}
replay_rows() {
  local label=$1 n=$2 fails=0 i
  for i in $(seq 1 "$n"); do
    if ! cargo test -p crazyagent --test engine_replay --locked -- --exact "$saturation" > "$out/$label-saturation-$i.txt" 2>&1; then
      fails=$((fails + 1))
      grep -hE 'panicked at' -A3 "$out/$label-saturation-$i.txt" | head -4
    fi
  done
  echo "ROWS $label saturation: $fails of $n failed"
  echo "$label saturation $fails $n" >> "$out/counts"
}
hogs=()
load_on() {
  local i
  for i in $(seq 1 $((cpus * 2))); do yes > /dev/null & hogs+=($!); done
  sleep 2
  echo "LOAD on: $((cpus * 2)) cpu hogs; $(uptime)"
}
load_off() { kill "${hogs[@]}" 2> /dev/null; pkill -x yes 2> /dev/null; hogs=(); echo "LOAD off: $(uptime)"; }
clean_tree() { [ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "::error::tree dirty after $1"; git status --porcelain; exit 1; }; }

echo "== RED: the fix files at $red (red rows present, fix absent)"
git checkout "$red" -- $fixfiles
build_lib red
lib_rows red 1 "$new_rows"
build_replay red
replay_rows red-quiet 10
load_on
replay_rows red-loaded 10
load_off
git checkout HEAD -- $fixfiles
clean_tree red

echo "== MUTANT: the tip with a worker that runs a job even when its caller withdrew it"
perl -0pi -e 's/if !claim\.decide\(TAKEN\) \{\s*continue;\s*\}/let _ = claim.decide(TAKEN);/' crates/crazyagent/src/storage.rs
git diff --stat | cat
[ -n "$(git status --porcelain --untracked-files=no)" ] || { echo "::error::the mutation did not apply"; exit 1; }
build_lib mutant
lib_rows mutant 1 "$race_rows"
git checkout HEAD -- crates/crazyagent/src/storage.rs
clean_tree mutant

echo "== GREEN: the tip $(git rev-parse --short=9 HEAD)"
build_lib green
lib_rows green-quiet 5 "$new_rows $race_rows"
load_on
lib_rows green-loaded 5 "$new_rows $race_rows"
load_off
build_replay green
replay_rows green-quiet 10
load_on
replay_rows green-loaded 10
load_off
cargo test -p crazyagent --test engine_replay --locked > "$out/green-replay-file.txt" 2>&1
echo "REPLAY FILE rc=$? $(grep -E '^test result' "$out/green-replay-file.txt" | tail -1)"
cargo test -p crazyagent --lib --locked -- $suites > "$out/green-suites.txt" 2>&1
echo "SUITES rc=$? $(grep -E '^test result' "$out/green-suites.txt" | tail -1)"
grep -E '^test .* FAILED$' "$out/green-suites.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/green-suite-failed.txt"
echo "SUITE FAILED $(wc -l < "$out/green-suite-failed.txt")"
cat "$out/green-suite-failed.txt"

if [ -s "$out/green-suite-failed.txt" ]; then
  echo "== ATTRIBUTE: the failed suite rows on the fix files at $red"
  git checkout "$red" -- $fixfiles
  build_lib attribute
  while read -r row; do
    if cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/attribute-${row##*::}.txt" 2>&1; then
      echo "NEW red (green before the fix): $row"
      echo "$row" >> "$out/new-reds.txt"
    else
      echo "OLD red (red before the fix too): $row"
    fi
  done < "$out/green-suite-failed.txt"
  git checkout HEAD -- $fixfiles
  clean_tree attribute
fi

cat "$out/counts"
awk '$1 ~ /^green/ && $3 > 0 {print "::error::green row failed: " $0; f = 1} END {exit f}' "$out/counts" || bad=1
awk '$1 == "red" && $2 != "an_apply_waits_for_a_storage_worker_that_starts_receiving_within_its_bound" && $3 == 0 {print "::error::red row passed before the fix: " $0; f = 1} END {exit f}' "$out/counts" || bad=1
awk '$1 == "mutant" && $3 == 0 {print "::error::race row passed against the mutant: " $0; f = 1} END {exit f}' "$out/counts" || bad=1
[ ! -s "$out/new-reds.txt" ] || { echo "::error::$(wc -l < "$out/new-reds.txt") suite rows went red with the fix"; bad=1; }
exit "$bad"
