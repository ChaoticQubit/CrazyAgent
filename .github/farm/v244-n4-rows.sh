set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4a #752 C1 green mode=$MODE at $tip on $ImageOS"
rows_re='^engine::native_auth_host::(pin|custodian|login|child_group|login_pipe)_windows_tests::[^ ]*: test$'
wide_re='^(engine::|harnesses::|tools::|paths::)[^ ]*: test$'
bad=0
for target in "-p crazyagent --lib" "-p crazyagent --tests" "-p crazyagent --bin crazyagent" "-p crazyagent-win-sandbox --lib" "-p crazyagent-win-sandbox --tests" "-p crazyagent-win-sandbox --features test-principals --lib"; do
  cargo clippy $target --locked -- -D warnings > "$RUNNER_TEMP/clippy.log" 2>&1
  rc=$?
  echo "clippy $target at $tip rc=$rc, $(grep -cE '^(warning|error)' "$RUNNER_TEMP/clippy.log") warning/error lines"
  [ "$rc" -eq 0 ] || { grep -E '^(warning|error)' -A8 "$RUNNER_TEMP/clippy.log" | head -80; bad=$((bad + 1)); }
done
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1
rc=$?
echo "bin build rc=$rc in $(( $(date +%s) - started )) s"
[ "$rc" -eq 0 ] || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -80; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$(cygpath -w "$GITHUB_WORKSPACE/target/debug/crazyagent.exe")"
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1
rc=$?
echo "test build rc=$rc in $(( $(date +%s) - started )) s"
[ "$rc" -eq 0 ] || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build.log" | head -120; exit 1; }
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null > "$RUNNER_TEMP/all.list"
cargo test -p crazyagent --lib --locked -- --list --ignored 2>/dev/null | grep -E ': test$' | sed 's/: test$//' > "$RUNNER_TEMP/ignored.list"
grep -E "$rows_re" "$RUNNER_TEMP/all.list" | sed 's/: test$//' | grep -vxF -f "$RUNNER_TEMP/ignored.list" > "$RUNNER_TEMP/rows.list"
rows=$(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')
echo "N4a Windows rows at $tip: $rows to run (pin $(grep -c '::pin_windows_tests::' "$RUNNER_TEMP/rows.list"), custodian $(grep -c '::custodian_windows_tests::' "$RUNNER_TEMP/rows.list")), ignored ones left out"
for run in 1 2 3; do
  cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
  echo "run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/rows.log" | sed 's/^/  /'
  awk '/^---- .* stdout ----$/{show=1} /^failures:$/{show=0} show' "$RUNNER_TEMP/rows.log" | grep -E "^---- |panicked at|left:|right:|called .Result::unwrap" | head -60
  passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/rows.log")
  echo "green check run $run: $passed pass (want $rows)"
  [ "$passed" -eq "$rows" ] || bad=$((bad + 1))
done
grep -E "$wide_re" "$RUNNER_TEMP/all.list" | sed 's/: test$//' | grep -vE '^engine::native_auth_host::(pin|custodian|login|child_group|login_pipe)_windows_tests::' | grep -vxF -f "$RUNNER_TEMP/ignored.list" > "$RUNNER_TEMP/wide.list"
wide=$(wc -l < "$RUNNER_TEMP/wide.list" | tr -d ' ')
echo "wide rows at $tip: $wide ($(cut -d: -f1-3 "$RUNNER_TEMP/wide.list" | sed 's/::[^:]*$//' | sort | uniq -c | sort -rn | head -12 | awk '{printf "%s %s; ", $2, $1}'))"
cargo test -p crazyagent --lib --locked -- --exact $(cat "$RUNNER_TEMP/wide.list") > "$RUNNER_TEMP/wide.log" 2>&1
echo "wide rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/wide.log")"
grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/wide.log" | sed 's/^/  /' | head -40
awk '/^---- .* stdout ----$/{show=1} /^failures:$/{show=0} show' "$RUNNER_TEMP/wide.log" | grep -E "^---- |panicked at|left:|right:|called .Result::unwrap" | head -80
wpassed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/wide.log")
echo "wide check: $wpassed pass (want $wide)"
[ "$wpassed" -eq "$wide" ] || bad=$((bad + 1))
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
