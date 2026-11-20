set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N1 rows mode=$MODE at $tip on $ImageOS $(git --version)"
prefix='engine::native_auth_host::pin_windows_tests::'
bad=0
if [ "$MODE" = green ]; then
  for target in "-p crazyagent --lib" "-p crazyagent --tests" "-p crazyagent-win-sandbox --lib" "-p crazyagent-win-sandbox --tests" "-p crazyagent-win-sandbox --features test-principals --lib"; do
    cargo clippy $target --locked -- -D warnings > "$RUNNER_TEMP/clippy.log" 2>&1
    rc=$?
    echo "clippy $target at $tip rc=$rc, $(grep -cE '^(warning|error)' "$RUNNER_TEMP/clippy.log") warning/error lines"
    [ "$rc" -eq 0 ] || { grep -E '^(warning|error)' -A6 "$RUNNER_TEMP/clippy.log" | head -60; bad=$((bad + 1)); }
  done
fi
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1
rc=$?
echo "test build rc=$rc in $(( $(date +%s) - started )) s"
if [ "$rc" -ne 0 ]; then
  grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build.log" | head -120
  echo "end in $(( $(date +%s) - started )) s, bad=build"
  exit 1
fi
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${prefix}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
rows=$(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')
echo "rows $prefix at $tip: $rows"
runs=1
[ "$MODE" = green ] && runs=3
for run in $(seq 1 "$runs"); do
  cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
  echo "run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  grep -E '^test .* \.\.\. ' "$RUNNER_TEMP/rows.log" | sed 's/^/  /'
  if [ "$MODE" = red ]; then
    awk '/^---- .* stdout ----$/{show=1} show' "$RUNNER_TEMP/rows.log" | grep -E "^---- |panicked at|left:|right:|called .Result::unwrap" | head -80
    passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/rows.log")
    echo "red check: $passed of $rows pass (want 0)"
    [ "$passed" -eq 0 ] || bad=$((bad + 1))
  else
    awk '/^---- .* stdout ----$/{show=1} show' "$RUNNER_TEMP/rows.log" | head -120
    failed=$(grep -cE '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/rows.log")
    [ "$failed" -eq 0 ] || bad=$((bad + 1))
  fi
done
if [ "$MODE" = green ]; then
  export CRAZYAGENT_FARM_PROBE_OUT="$RUNNER_TEMP/n1-timing.txt"
  cargo test -p crazyagent --lib --locked -- --exact --ignored "${prefix}the_time_one_check_takes_for_a_360_mb_harness" > "$RUNNER_TEMP/timing.log" 2>&1
  echo "timing rc=$? (test profile, sha2 and crazyagent at opt-level 3, hosted image = quiet case): $(cat "$CRAZYAGENT_FARM_PROBE_OUT" 2>&1)"
fi
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
