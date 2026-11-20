set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N3 rows mode=$MODE at $tip on $ImageOS"
prefix='engine::native_auth_host::custodian_windows_tests::'
bad=0
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1
rc=$?
echo "bin build rc=$rc in $(( $(date +%s) - started )) s"
[ "$rc" -eq 0 ] || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -80; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$(cygpath -w "$GITHUB_WORKSPACE/target/debug/crazyagent.exe")"
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1
rc=$?
echo "test build rc=$rc in $(( $(date +%s) - started )) s"
[ "$rc" -eq 0 ] || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build.log" | head -120; exit 1; }
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${prefix}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
rows=$(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')
echo "rows $prefix at $tip: $rows"
cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
echo "run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
grep -E '^test .* \.\.\. ' "$RUNNER_TEMP/rows.log" | sed 's/^/  /'
awk '/^---- .* stdout ----$/{show=1} show' "$RUNNER_TEMP/rows.log" | grep -E "^---- |panicked at|left:|right:|called .Result::unwrap" | head -80
passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/rows.log")
if [ "$MODE" = red ]; then
  echo "red check: $passed of $rows pass (want 0)"
  [ "$passed" -eq 0 ] || bad=1
else
  failed=$(grep -cE '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/rows.log")
  [ "$failed" -eq 0 ] || bad=1
fi
export CRAZYAGENT_FARM_PROBE_OUT="$RUNNER_TEMP/n3-probe.txt"
cargo test -p crazyagent --lib --locked -- --exact --ignored "${prefix}what_a_custodian_does_with_no_system_root" > "$RUNNER_TEMP/probe.log" 2>&1
echo "probe rc=$?: $(cat "$CRAZYAGENT_FARM_PROBE_OUT" 2>&1)"
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
