set +e
set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N1 linux at $tip on $ImageOS"
bad=0
for target in "--lib" "--tests"; do
  cargo clippy -p crazyagent $target --locked -- -D warnings > "$RUNNER_TEMP/clippy.log" 2>&1
  rc=$?
  echo "clippy -p crazyagent $target at $tip rc=$rc, $(grep -cE '^(warning|error)' "$RUNNER_TEMP/clippy.log") warning/error lines"
  [ "$rc" -eq 0 ] || { grep -E '^(warning|error)' -A6 "$RUNNER_TEMP/clippy.log" | head -60; bad=1; }
done
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1
echo "bin build rc=$?"
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent"
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1
rc=$?
echo "test build rc=$rc in $(( $(date +%s) - started )) s"
[ "$rc" -eq 0 ] || { grep -E '^error' -A8 "$RUNNER_TEMP/build.log" | head -80; exit 1; }
prefix='engine::native_auth_host::'
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${prefix}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
echo "rows $prefix at $tip: $(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')"
cargo test -p crazyagent --lib --locked -- --exact $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
rc=$?
echo "rows rc=$rc | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/rows.log" | sed 's/^/  /'
awk '/^---- .* stdout ----$/{show=1} /^failures:$/{show=0} show' "$RUNNER_TEMP/rows.log" | head -100
[ "$rc" -eq 0 ] || bad=1
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
