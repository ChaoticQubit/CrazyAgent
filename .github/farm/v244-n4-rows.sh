set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4b #747 red mode=$MODE at $tip on $ImageOS"
base='engine::native_auth_host::'
expect() {
  case "$1" in
    *::a_code_written_just_before_the_custodian_exits_is_read_every_time) echo ANY ;;
    *::the_read_after_exit_takes_the_error_stream_to_its_end) echo 'the read after exit missed a late code' ;;
    *::the_read_after_exit_returns_at_its_deadline_while_a_holder_keeps_the_stream_open) echo 'the read after exit waited' ;;
  esac
}
bad=0
pass() {
  local label=$1 mod=$2 want_rows=$3
  cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build-$label.log" 2>&1 || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build-$label.log" | head -80; bad=$((bad + 1)); return; }
  cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${base}${mod}::[^ ]*(read_after_exit|just_before_the_custodian_exits)[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/$label.list"
  echo "$label rows $(wc -l < "$RUNNER_TEMP/$label.list" | tr -d ' ') (want $want_rows)"
  [ "$(wc -l < "$RUNNER_TEMP/$label.list" | tr -d ' ')" -eq "$want_rows" ] || bad=$((bad + 1))
  for run in 1 2 3; do
    cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/$label.list") > "$RUNNER_TEMP/$label.log" 2>&1
    echo "$label run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/$label.log")"
    while read -r row; do
      why=$(awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} /^failures:$/{show=0} show' "$RUNNER_TEMP/$label.log")
      echo "$why" | grep -E -A2 'panicked at' | head -4 | sed 's/^/    /'
      want=$(expect "$row")
      if [ "$want" = ANY ]; then
        if grep -qE "^test ${row} \.\.\. ok$" "$RUNNER_TEMP/$label.log"; then echo "  green at red, race not met: ${row#$base}"; else echo "  red at red: ${row#$base}"; fi
      elif [ "$want" = GREEN ]; then
        if grep -qE "^test ${row} \.\.\. ok$" "$RUNNER_TEMP/$label.log"; then echo "  green as meant: ${row#$base}"; else echo "  WRONG ${row#$base}"; bad=$((bad + 1)); fi
      elif grep -qE "^test ${row} \.\.\. FAILED$" "$RUNNER_TEMP/$label.log" && printf '%s' "$why" | grep -qF "$want"; then echo "  red as meant: ${row#$base} ($want)"; else echo "  WRONG ${row#$base}"; bad=$((bad + 1)); fi
    done < "$RUNNER_TEMP/$label.list"
  done
}
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -60; exit 1; }
echo "builds in $(( $(date +%s) - started )) s"
pass c5 custodian_windows_tests 3
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
