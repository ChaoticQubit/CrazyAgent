set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4a pipes red mode=$MODE at $tip on $ImageOS"
base='engine::native_auth_host::'
expect() {
  case "$1" in
    *::a_login_child_flooding_both_outputs_meets_each_bound_without_a_blocked_read) echo 'a flood never met the bound' ;;
    *::a_login_child_flooding_output_while_a_write_is_pending_still_meets_the_bound) echo 'the output never met the bound while a write was pending' ;;
    *::a_write_to_a_login_child_that_never_reads_stays_pending_then_fails_once_ended) echo 'GREEN' ;;
  esac
}
bad=0
pass() {
  local label=$1 mod=$2 want_rows=$3
  cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build-$label.log" 2>&1 || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build-$label.log" | head -80; bad=$((bad + 1)); return; }
  cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${base}${mod}::[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/$label.list"
  echo "$label rows $(wc -l < "$RUNNER_TEMP/$label.list" | tr -d ' ') (want $want_rows)"
  [ "$(wc -l < "$RUNNER_TEMP/$label.list" | tr -d ' ')" -eq "$want_rows" ] || bad=$((bad + 1))
  for run in 1 2 3; do
    cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/$label.list") > "$RUNNER_TEMP/$label.log" 2>&1
    echo "$label run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/$label.log")"
    while read -r row; do
      why=$(awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} /^failures:$/{show=0} show' "$RUNNER_TEMP/$label.log")
      echo "$why" | grep -E -A2 'panicked at' | head -4 | sed 's/^/    /'
      want=$(expect "$row")
      if [ "$want" = GREEN ]; then
        if grep -qE "^test ${row} \.\.\. ok$" "$RUNNER_TEMP/$label.log"; then echo "  green as meant: ${row#$base}"; else echo "  WRONG ${row#$base}"; bad=$((bad + 1)); fi
      elif grep -qE "^test ${row} \.\.\. FAILED$" "$RUNNER_TEMP/$label.log" && printf '%s' "$why" | grep -qF "$want"; then echo "  red as meant: ${row#$base} ($want)"; else echo "  WRONG ${row#$base}"; bad=$((bad + 1)); fi
    done < "$RUNNER_TEMP/$label.list"
  done
}
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -60; exit 1; }
echo "builds in $(( $(date +%s) - started )) s"
pass pipes login_pipe_windows_tests 3
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
