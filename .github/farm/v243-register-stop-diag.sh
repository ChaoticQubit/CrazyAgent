set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/regstop"
mkdir -p "$out"
file=crates/crazyagent/src/team/tests.rs
row=team::tests::the_owners_register_stop_of_a_team_agent_acts_without_waiting_for_the_next_tick
rounds=10
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
patch_took() {
  [ "$(grep -c 'let took = asked.elapsed();' "$file")" = 1 ] || { note "| patch | took anchor not unique |"; return 1; }
  sed -i '' 's/let took = asked.elapsed();/let took = asked.elapsed(); eprintln!("register_stop_took_ms={}", took.as_millis());/' "$file"
}
patch_tick() {
  [ "$(grep -c 'let tick = std::time::Duration::from_secs(4);' "$file")" = 1 ] || { note "| patch | tick anchor not unique |"; return 1; }
  sed -i '' 's/let tick = std::time::Duration::from_secs(4);/let tick = std::time::Duration::from_secs(30);/' "$file"
}
measure() {
  local name="$1"
  if ! cargo test -p crazyagent --lib --locked --no-run > "$out/build-$name.txt" 2>&1; then
    note "| $name | lib test build FAILED |"
    grep -E -A12 '^error' "$out/build-$name.txt" | head -60
    return 1
  fi
  : > "$out/rows-$name.txt"
  for round in $(seq 1 "$rounds"); do
    echo "== round $round $(date -u +%H:%M:%S) load $(sysctl -n vm.loadavg)" >> "$out/rows-$name.txt"
    cargo test -p crazyagent --lib --locked -- --exact "$row" --show-output >> "$out/rows-$name.txt" 2>&1
  done
  grep -o 'register_stop_took_ms=[0-9]*' "$out/rows-$name.txt" | cut -d= -f2 | sort -n > "$out/took-$name.txt"
  local count ok failed
  count=$(wc -l < "$out/took-$name.txt" | tr -d ' ')
  ok=$(grep -c "^test $row \.\.\. ok" "$out/rows-$name.txt")
  failed=$(grep -c "^test $row \.\.\. FAILED" "$out/rows-$name.txt")
  [ "$count" -gt 0 ] || { note "| $name | no took line |"; return 1; }
  note "| $name | $count of $rounds measured, $ok ok, $failed FAILED | min $(head -1 "$out/took-$name.txt") ms, median $(sed -n "$(( (count + 1) / 2 ))p" "$out/took-$name.txt") ms, max $(tail -1 "$out/took-$name.txt") ms | all: $(paste -sd, "$out/took-$name.txt") |"
}
note "| runner | $(sysctl -n hw.ncpu) cpu, $(( $(sysctl -n hw.memsize) / 1073741824 )) GiB, $(sw_vers -productVersion), tree $(git rev-parse --short=9 HEAD) |"
note ""
note "| variant | runs | took | values |"
note "| --- | --- | --- | --- |"
patch_took || exit 1
measure tick-4s || bad=1
patch_tick || exit 1
measure tick-30s || bad=1
git checkout -q -- "$file"
for name in tick-4s tick-30s; do
  echo "== $name panics =="
  grep -E -B2 -A8 'panicked at' "$out/rows-$name.txt" | head -60
done
note "| verdict | $([ "$bad" -eq 0 ] && echo MEASURED || echo INCOMPLETE) |"
exit "$bad"
