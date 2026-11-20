set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N3 r2 agent-space red mode=$MODE at $tip on $ImageOS"
pin='engine::native_auth_host::pin_windows_tests::'
row="${pin}an_npm_shim_whose_script_is_in_agent_space_is_refused_by_harness_search"
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -60; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$(cygpath -w "$GITHUB_WORKSPACE/target/debug/crazyagent.exe")"
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1 || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build.log" | head -80; exit 1; }
echo "builds in $(( $(date +%s) - started )) s"
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${pin}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/all.list"
cargo test -p crazyagent --lib --locked -- --list --ignored 2>/dev/null | grep -E "^${pin}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/ignored.list"
grep -vxF -f "$RUNNER_TEMP/ignored.list" "$RUNNER_TEMP/all.list" > "$RUNNER_TEMP/rows.list"
echo "pin rows $(wc -l < "$RUNNER_TEMP/all.list" | tr -d ' '), ignored $(wc -l < "$RUNNER_TEMP/ignored.list" | tr -d ' '), run $(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')"
bad=0
for run in 1 2 3; do
  cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
  echo "run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  failed=$(grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/rows.log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/')
  awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} /^failures:$/{show=0} show' "$RUNNER_TEMP/rows.log" | grep -E 'panicked at|left:|right:' | sed 's/^/  /'
  if [ "$failed" = "$row" ] && awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} show' "$RUNNER_TEMP/rows.log" | grep -q 'harness_path_agent_space'; then
    echo "red check run $run: only the agent-space row red, as meant"
  else
    echo "red check run $run: NOT as meant; failed rows: $failed"; bad=$((bad + 1))
  fi
done
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
