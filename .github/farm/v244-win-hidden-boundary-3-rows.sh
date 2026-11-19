#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
hp=sandbox::hidden_paths::tests::
win=sandbox::windows::tests::
tui=tui::screens::project_tests::
probe="${win}a_hidden_file_with_its_own_grant_to_every_app_container_stays_out_of_reach"
rows=(sandbox::hidden_paths:: "${tui}a_hidden_folder_"
  "${tui}settings_offer_hidden_folders"
  project_setup::engine_tests::hidden_folders_are_refused
  project_setup::engine_tests::stale_hidden_paths
  project_setup::engine_tests::hidden_paths_read_names
  project_overview:: roadmap_status:: onboarding::graph::)
want_red=("${tui}a_hidden_folder_the_windows_walk_refused_is_told_what_to_do")
compile_names=(walk held Unheld WALK_ENTRIES_MAX)
if [ "$RUNNER_OS" = Windows ]; then
  exe=.exe
  rows+=(sandbox::windows::tests::)
  want_red+=("${win}a_folder_in_a_canonical_parent_can_be_hidden"
    "${win}hidden_folders_join_the_boundary_hidden_and_an_absent_one_is_made_empty"
    "${win}a_wrapped_agent_cannot_reach_a_hidden_folder_by_any_name"
    "$probe"
    "${win}a_second_container_cannot_read_a_folder_another_run_hides")
  compile_names+=(file_links)
else
  exe=
fi
note() { printf '%s\n' "$1" | tee -a "$summary"; }
at() {
  git checkout -q -f --detach "$1" || { echo "checkout $1 failed"; exit 1; }
  echo "######## at $(git log --oneline -1)"
}
bins() {
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/bins.txt"; return 1; }
  mkdir -p target/debug/deps
  cp "target/debug/crazyagent-landlock$exe" "target/debug/deps/crazyagent-landlock$exe"
  if [ "$RUNNER_OS" = Windows ]; then
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/boundary.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/boundary.txt"; return 1; }
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  fi
}
lib() {
  cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/lib.txt" 2>&1
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
  cargo test -p crazyagent --locked --lib -- --ignored --exact "${hp}walk_time_per_call" --show-output > "$RUNNER_TEMP/timing-$1.txt" 2>&1
  local rc=$?
  tail -n 20 "$RUNNER_TEMP/timing-$1.txt"
  local lines
  lines=$(grep -c '^TIMING ' "$RUNNER_TEMP/timing-$1.txt")
  while read -r line; do
    note "| timing $1 | exit $rc, $line |"
  done < <(grep '^TIMING ' "$RUNNER_TEMP/timing-$1.txt")
  { [ "$rc" -eq 0 ] && [ "$lines" -eq 2 ]; } || { note "| timing $1 | **exit $rc, $lines TIMING lines, 2 wanted** |"; return 1; }
}
mode=$1
sha=${2:-}
case "$mode" in
  compile-red)
    at "$sha"
    bins || { note "| bins at ${sha:0:9} | FAILED |"; exit 1; }
    if lib; then
      note "| compile red ${sha:0:9} | **the lib tests built, a compile red was wanted** |"
      exit 1
    fi
    grep -E '^error' -A 4 "$RUNNER_TEMP/lib.txt" | head -n 60
    failed=0
    for name in "${compile_names[@]}"; do
      if grep -E '^error' -A 2 "$RUNNER_TEMP/lib.txt" | grep -q "\`$name\`"; then
        note "| compile red ${sha:0:9} | \`$name\` missing, as wanted |"
      else
        note "| compile red ${sha:0:9} | **no error names \`$name\`** |"
        failed=1
      fi
    done
    exit "$failed"
    ;;
  scratch-red)
    rows_sha=$2
    red_sha=$3
    fix_sha=$4
    base_sha=$5
    at "$base_sha"
    git checkout -q "$red_sha" -- crates/crazyagent/src/sandbox/windows.rs crates/crazyagent/src/tui/screens/project_tests.rs
    git diff "$fix_sha" "$rows_sha" -- crates/crazyagent/src/sandbox/windows.rs | git apply || { note "| scratch red | the later rows do not apply on the red rows |"; exit 1; }
    git diff --stat "$base_sha"
    bins && lib || { grep -E '^error' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 60; note "| scratch red at base ${base_sha:0:9} | build FAILED |"; exit 1; }
    run scratch --exact "${want_red[@]}"
    printf '%s\n' "${want_red[@]}" | sort > "$RUNNER_TEMP/want-scratch.failed"
    if diff -u "$RUNNER_TEMP/want-scratch.failed" "$RUNNER_TEMP/scratch.failed"; then
      note "| scratch red: base ${base_sha:0:9} code, rows of ${rows_sha:0:9} | $count failed, exactly the ${#want_red[@]} expected on $RUNNER_OS |"
    else
      note "| scratch red: base ${base_sha:0:9} code, rows of ${rows_sha:0:9} | **failed set differs from the expected ${#want_red[@]}** ($count failed) |"
      exit 1
    fi
    git checkout -q -f --detach "$base_sha"
    ;;
  build)
    at "$sha"
    { bins && lib; } || { grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 80; note "| build at ${sha:0:9} | FAILED |"; exit 1; }
    note "| build at ${sha:0:9} | ok |"
    ;;
  tip)
    at "$sha"
    run tip "${rows[@]}" --skip "$probe"
    rc=$?
    failed=0
    if [ "$rc" -eq 0 ] && [ "$count" -eq 0 ] && [ "$passed" -gt 0 ]; then
      note "| tip ${sha:0:9} rows | exit 0, $passed ok |"
    else
      note "| tip ${sha:0:9} rows | **exit $rc, $count failed** |"
      failed=1
    fi
    if [ "$RUNNER_OS" = Windows ]; then
      run probe --exact "$probe"
      note "| probe: a hidden file with its own grant to every app container | exit $?, $passed ok, $count failed |"
    fi
    run broad hidden --skip "$probe"
    rc=$?
    note "| tip, every lib row naming hidden | exit $rc, $passed ok, $count failed |"
    cp "$RUNNER_TEMP/broad.failed" "$RUNNER_TEMP/broad.list"
    exit "$failed"
    ;;
  timing)
    failed=0
    timing quiet || failed=1
    cpus=$(nproc 2>/dev/null || echo "${NUMBER_OF_PROCESSORS:-2}")
    pids=()
    for _ in $(seq "$cpus"); do
      ( while :; do :; done ) &
      pids+=($!)
    done
    sleep 5
    timing "under-load-$cpus-busy-loops" || failed=1
    kill "${pids[@]}" 2>/dev/null
    wait "${pids[@]}" 2>/dev/null
    exit "$failed"
    ;;
  base)
    if [ ! -s "$RUNNER_TEMP/broad.list" ]; then
      note "| base | no row naming hidden is red at the tip |"
      exit 0
    fi
    at "$sha"
    { bins && lib; } || { note "| build at base | FAILED |"; exit 1; }
    failed=0
    while read -r name; do
      cargo test -p crazyagent --locked --lib -- --exact "$name" --test-threads 1 > "$RUNNER_TEMP/again.txt" 2>&1
      if grep -qE "^test $name \.\.\. FAILED$" "$RUNNER_TEMP/again.txt"; then
        note "| $name | red at tip and at base ${sha:0:9}: not this branch |"
      else
        note "| $name | **red at tip only** |"
        failed=1
      fi
    done < "$RUNNER_TEMP/broad.list"
    exit "$failed"
    ;;
  *)
    echo "unknown mode $mode"
    exit 1
    ;;
esac
