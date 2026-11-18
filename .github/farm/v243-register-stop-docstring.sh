set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/regstop"
mkdir -p "$out"
cp .github/farm/v243-register-stop-nowake.diff "$out/nowake.diff"
file=crates/crazyagent/src/team/tests.rs
mutated=crates/crazyagent/src/agents.rs
row=team::tests::the_owners_register_stop_of_a_team_agent_acts_without_waiting_for_the_next_tick
tip=74521a1fb586469d108908cd6b12afbae6a292fc
bad=0
loaders=()
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
fail() {
  note "| $1 | FAILED: $2 |"
  bad=1
}
load_on() {
  local cores
  cores=$(sysctl -n hw.ncpu)
  for _ in $(seq 1 "$cores"); do
    yes > /dev/null &
    loaders+=("$!")
  done
  sleep 5
}
load_off() {
  [ "${#loaders[@]}" -eq 0 ] && return 0
  kill "${loaders[@]}" 2> /dev/null
  wait "${loaders[@]}" 2> /dev/null
  loaders=()
}
trap load_off EXIT
build() {
  local name="$1"
  if ! cargo test -p crazyagent --lib --locked --no-run > "$out/build-$name.txt" 2>&1; then
    grep -E -A12 '^error' "$out/build-$name.txt" | head -60
    fail "$name" "lib test build"
    return 1
  fi
}
rows() {
  local name="$1" rounds="$2" want="$3"
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
  if [ "$count" -eq 0 ]; then
    fail "$name" "no took line"
    return 1
  fi
  note "| $name | $count of $rounds measured, $ok ok, $failed FAILED | min $(head -1 "$out/took-$name.txt") ms, median $(sed -n "$(( (count + 1) / 2 ))p" "$out/took-$name.txt") ms, max $(tail -1 "$out/took-$name.txt") ms | $(paste -sd, "$out/took-$name.txt") |"
  [ "$count" -eq "$rounds" ] || fail "$name" "$count of $rounds runs printed took"
  if [ "$want" = ok ]; then
    [ "$ok" -eq "$rounds" ] || fail "$name" "want $rounds ok"
  else
    [ "$failed" -eq "$rounds" ] || fail "$name" "want $rounds FAILED"
    [ "$(head -1 "$out/took-$name.txt")" -ge 8000 ] || fail "$name" "a stop with no wake took under the 8 s tick"
  fi
}
note "| runner | $(sysctl -n hw.ncpu) cpu, $(( $(sysctl -n hw.memsize) / 1073741824 )) GiB, $(sw_vers -productVersion), tree $(git rev-parse --short=9 HEAD) |"
[ "$(git rev-parse HEAD^)" = "$tip" ] || { note "| ancestry | HEAD^ is not the code tip $tip |"; exit 1; }
[ "$(git diff --name-only "$tip" HEAD -- . ':!.github' | wc -l | tr -d ' ')" = 0 ] || { note "| ancestry | the farm commit changes code |"; exit 1; }
[ "$(grep -c 'let tick = std::time::Duration::from_secs(8);' "$file")" = 1 ] || { note "| tip | tick 8 s anchor not found |"; exit 1; }
[ "$(grep -c '        took < tick,' "$file")" = 1 ] || { note "| tip | took < tick anchor not found |"; exit 1; }
[ "$(grep -c 'let took = asked.elapsed();' "$file")" = 1 ] || { note "| patch | took anchor not unique |"; exit 1; }
sed -i '' 's/let took = asked.elapsed();/let took = asked.elapsed(); eprintln!("register_stop_took_ms={}", took.as_millis());/' "$file"
note ""
note "| variant | runs | took | values (ms) |"
note "| --- | --- | --- | --- |"
if build tip; then
  rows tip-quiet 10 ok
  load_on
  rows tip-loaded 10 ok
  load_off
fi
[ "$(git apply --numstat "$out/nowake.diff")" = "$(printf '0\t1\t%s' "$mutated")" ] || { note "| mutant | the diff is not one removed line in $mutated |"; exit 1; }
git apply "$out/nowake.diff"
if build nowake; then
  rows nowake-quiet 3 FAILED
  load_on
  rows nowake-loaded 5 FAILED
  load_off
fi
git apply -R "$out/nowake.diff"
git checkout -q -- "$file"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { note "| tree | not clean after the mutant |"; exit 1; }
if cargo clippy -p crazyagent --lib --tests --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
  note "| clippy --lib --tests -D warnings at the tip | clean |"
else
  grep -E -A8 '^(warning|error)' "$out/clippy.txt" | head -60
  fail clippy "--lib --tests -D warnings"
fi
for name in tip-quiet tip-loaded nowake-quiet nowake-loaded; do
  echo "== $name panics =="
  [ -f "$out/rows-$name.txt" ] && grep -E -B2 -A6 'panicked at' "$out/rows-$name.txt" | head -40
done
note "| verdict | $([ "$bad" -eq 0 ] && echo VERIFIED || echo RED) |"
exit "$bad"
