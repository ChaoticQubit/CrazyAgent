#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
prefix=sandbox::hidden_paths::tests::
tui=tui::screens::project_tests::
red_rows=(sandbox::hidden_paths:: "${tui}a_hidden_folder_")
rows=(sandbox::hidden_paths:: project_files::
  "${tui}a_hidden_folder_"
  "${tui}settings_offer_hidden_folders"
  project_setup::engine_tests::hidden_folders_are_refused
  project_setup::engine_tests::stale_hidden_paths
  project_setup::engine_tests::hidden_paths_read_names
  project_overview:: roadmap_status:: onboarding::graph::)
want_red1=("${prefix}a_folder_inside_a_submodule_the_index_names_is_found_tracked"
  "${prefix}a_folder_inside_a_nested_repository_is_refused"
  "${prefix}a_listed_path_that_is_not_utf8_is_read_with_replacement_characters")
want_red2=("${prefix}a_listed_path_not_utf8_or_past_the_line_cap_refuses_the_check_as_unreadable")
if [ "$RUNNER_OS" = Windows ]; then
  exe=.exe
else
  exe=
  want_red1+=("${tui}a_hidden_folder_inside_a_nested_repository_is_told_why")
  want_red2+=("${tui}a_hidden_folder_left_unchecked_is_told_the_cause_and_the_way_out")
fi
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
  { [ "$rc" -eq 0 ] && [ "$lines" -eq 4 ]; } || { note "| timing $1 | **exit $rc, $lines TIMING lines, 4 wanted** |"; return 1; }
}
mode=$1
sha=${2:-}
case "$mode" in
  build)
    at "$sha"
    helpers || { note "| build at ${sha:0:9} | FAILED |"; exit 1; }
    note "| build at ${sha:0:9} | ok |"
    ;;
  red1 | red2)
    at "$sha"
    run "$mode" "${red_rows[@]}"
    if [ "$mode" = red1 ]; then want=("${want_red1[@]}"); else want=("${want_red2[@]}"); fi
    printf '%s\n' "${want[@]}" | sort > "$RUNNER_TEMP/want-$mode.failed"
    if diff -u "$RUNNER_TEMP/want-$mode.failed" "$RUNNER_TEMP/$mode.failed"; then
      note "| $mode ${sha:0:9} | $count failed, exactly the ${#want[@]} expected on $RUNNER_OS; $passed ok |"
    else
      note "| $mode ${sha:0:9} | **failed set differs from the expected ${#want[@]}** ($count failed) |"
      exit 1
    fi
    ;;
  tip)
    at "$sha"
    run tip "${rows[@]}"
    rc=$?
    failed=0
    if [ "$rc" -eq 0 ] && [ "$count" -eq 0 ] && [ "$passed" -gt 0 ]; then
      note "| tip ${sha:0:9} rows | exit 0, $passed ok |"
    else
      note "| tip ${sha:0:9} rows | **exit $rc, $count failed** |"
      failed=1
    fi
    run broad hidden
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
    helpers || { note "| build at base | FAILED |"; exit 1; }
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
