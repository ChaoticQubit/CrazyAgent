#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
red=8c24044e6482635a0ead69a09514b93c5489dd32
tip=88db6a0960cd2f146654471dcbf6f56a076141a3
base=e904fd105b8583354e5cbea5571c1dbbd268f35b
rows=(sandbox::hidden_paths:: project_files::)
prefix=sandbox::hidden_paths::tests::
if [ "$RUNNER_OS" = Windows ]; then
  exe=.exe
  want=(
    "${prefix}a_folder_git_tracks_is_refused_and_an_untracked_one_is_accepted"
    "${prefix}a_folder_with_no_repository_is_not_tracked"
    "${prefix}a_tracked_folder_inside_another_is_found_in_any_case_spelling_windows_accepts"
  )
else
  exe=
  want=("${prefix}a_commondir_planted_in_the_git_folder_redirects_nothing")
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
  local line
  line=$(grep -m1 '^TIMING ' "$RUNNER_TEMP/timing-$1.txt")
  cat "$RUNNER_TEMP/timing-$1.txt" | tail -n 30
  note "| timing $1 | exit $rc, ${line:-no TIMING line} |"
  { [ "$rc" -eq 0 ] && [ -n "$line" ]; } || failed=1
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
