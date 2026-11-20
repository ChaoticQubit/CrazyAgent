set +e
set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "PR16 $FARM_OS at $tip (parents $(git log -1 --format=%p HEAD)) on $ImageOS"
bad=0
if [ "$FARM_OS" = windows ]; then
  cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.log" 2>&1
  rc=$?
  echo "clippy -p crazyagent --all-targets at $tip rc=$rc in $(( $(date +%s) - started )) s, $(grep -cE '^(warning|error)' "$RUNNER_TEMP/clippy.log") warning/error lines"
  [ "$rc" -eq 0 ] || { grep -E '^(warning|error)' -A8 "$RUNNER_TEMP/clippy.log" | head -150; bad=1; }
else
  prefix='terminal::tmux::tests::'
  cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1
  rc=$?
  echo "test build rc=$rc in $(( $(date +%s) - started )) s"
  if [ "$rc" -ne 0 ]; then
    grep -E '^error' -A8 "$RUNNER_TEMP/build.log" | head -100
    exit 1
  fi
  cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${prefix}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
  rows=$(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ')
  echo "rows $prefix at $tip: $rows"
  [ "$rows" -gt 0 ] || bad=1
  cargo test -p crazyagent --lib --locked -- "$prefix" > "$RUNNER_TEMP/rows.log" 2>&1
  rc=$?
  echo "rows rc=$rc | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  grep -E '^test .* \.\.\. (FAILED|ignored)' "$RUNNER_TEMP/rows.log" | sed 's/^/  /'
  awk '/^---- .* stdout ----$/{show=1} /^failures:$/{show=0} show' "$RUNNER_TEMP/rows.log" | head -150
  [ "$rc" -eq 0 ] || bad=1
fi
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
