#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
red=9013e059791821f57b4c850a2c79626887810527
tip=009dbe9d44677aa601195c2ca7d2f59f1542ae24
base=e904fd105b8583354e5cbea5571c1dbbd268f35b
rows=(sandbox::hidden_paths:: project_files::)
prefix=sandbox::hidden_paths::tests::
want=("${prefix}a_list_of_folders_is_checked_with_one_git_run")
if [ "$RUNNER_OS" = Windows ]; then
  exe=.exe
else
  exe=
fi
failed=0
note() { printf '%s\n' "$1" | tee -a "$summary"; }
at() {
  git checkout -q -f --detach "$1" || { echo "checkout $1 failed"; exit 1; }
  echo "######## at $(git log --oneline -1)"
}
helpers() {
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/bins.txt"; return 1; }
  mkdir -p target/debug/deps
  cp "target/debug/crazyagent-landlock$exe" "target/debug/deps/crazyagent-landlock$exe"
  if [ "$RUNNER_OS" = Windows ]; then
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/boundary.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/boundary.txt"; return 1; }
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  fi
  cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/lib.txt" 2>&1 || { grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 80; return 1; }
}
run() {
  local label=$1
  shift
  cargo test -p crazyagent --locked --lib -- "$@" > "$RUNNER_TEMP/$label.txt" 2>&1
  local rc=$?
  grep -E '^test .* \.\.\. |^test result:' "$RUNNER_TEMP/$label.txt"
  sed -n '/^failures:$/,$p' "$RUNNER_TEMP/$label.txt" | head -n 300
  grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/$label.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort > "$RUNNER_TEMP/$label.failed"
  passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/$label.txt")
  count=$(grep -c . "$RUNNER_TEMP/$label.failed")
  return $rc
}
timing() {
  cargo test -p crazyagent --locked --lib -- --ignored --exact "${prefix}none_tracked_time_per_call" --show-output > "$RUNNER_TEMP/timing-$1.txt" 2>&1
  local rc=$?
  tail -n 30 "$RUNNER_TEMP/timing-$1.txt"
  local lines
  lines=$(grep -c '^TIMING ' "$RUNNER_TEMP/timing-$1.txt")
  while read -r line; do
    note "| timing $1 | exit $rc, $line |"
  done < <(grep '^TIMING ' "$RUNNER_TEMP/timing-$1.txt")
  { [ "$rc" -eq 0 ] && [ "$lines" -eq 2 ]; } || { note "| timing $1 | **exit $rc, $lines TIMING lines, 2 wanted** |"; failed=1; }
}
note "| ${ImageOS:-$RUNNER_OS} hidden git rows | result |"
note "| --- | --- |"
at "$red"
helpers || { note "| build at red | FAILED |"; echo "FARM failed=1"; exit 1; }
run red "${rows[@]}"
printf '%s\n' "${want[@]}" | sort > "$RUNNER_TEMP/want.failed"
if diff -u "$RUNNER_TEMP/want.failed" "$RUNNER_TEMP/red.failed"; then
  note "| red $red | $count failed, exactly the ${#want[@]} expected on $RUNNER_OS; $passed ok |"
else
  note "| red $red | **failed set differs from the expected ${#want[@]}** ($count failed) |"
  failed=1
fi
at "$tip"
helpers || { note "| build at tip | FAILED |"; echo "FARM failed=1"; exit 1; }
run tip "${rows[@]}"
rc=$?
if [ "$rc" -eq 0 ] && [ "$count" -eq 0 ] && [ "$passed" -gt 0 ]; then
  note "| tip $tip rows | exit 0, $passed ok |"
else
  note "| tip $tip rows | **exit $rc, $count failed** |"
  failed=1
fi
run broad hidden
rc=$?
note "| tip, every lib row naming hidden | exit $rc, $passed ok, $count failed |"
cp "$RUNNER_TEMP/broad.failed" "$RUNNER_TEMP/broad.list"
timing quiet
cpus=$(nproc 2>/dev/null || echo "${NUMBER_OF_PROCESSORS:-2}")
pids=()
for _ in $(seq "$cpus"); do
  ( while :; do :; done ) &
  pids+=($!)
done
sleep 5
timing "under-load-$cpus-busy-loops"
kill "${pids[@]}" 2>/dev/null
wait "${pids[@]}" 2>/dev/null
if [ -s "$RUNNER_TEMP/broad.list" ]; then
  at "$base"
  helpers || { note "| build at base | FAILED |"; echo "FARM failed=1"; exit 1; }
  while read -r name; do
    cargo test -p crazyagent --locked --lib -- --exact "$name" --test-threads 1 > "$RUNNER_TEMP/again.txt" 2>&1
    if grep -qE "^test $name \.\.\. FAILED$" "$RUNNER_TEMP/again.txt"; then
      note "| $name | red at tip and at base $base: not this branch |"
    else
      note "| $name | **red at tip only** |"
      failed=1
    fi
  done < "$RUNNER_TEMP/broad.list"
fi
echo "FARM failed=$failed"
exit "$failed"
