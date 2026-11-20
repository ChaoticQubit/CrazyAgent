set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N3 r2 rows mode=$MODE at $tip on $ImageOS"
pin='engine::native_auth_host::pin_windows_tests::'
cus='engine::native_auth_host::custodian_windows_tests::'
bad=0
for target in "-p crazyagent --lib" "-p crazyagent --tests" "-p crazyagent --bin crazyagent"; do
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
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^(${pin}|${cus})[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
echo "rows listed at $tip: $(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ') (pin $(grep -c "^$pin" "$RUNNER_TEMP/rows.list"), custodian $(grep -c "^$cus" "$RUNNER_TEMP/rows.list"))"
expect() {
  case "$1" in
    *an_npm_codex_shim_passes_harness_search_by_the_script_it_starts) echo native_auth_host_executable_unsafe ;;
    *an_npm_codex_shim_pins_its_script_and_current_keeps_the_same_host) echo native_auth_host_executable_unsafe ;;
    *a_rewritten_npm_script_is_refused_through_current) echo native_auth_host_executable_unsafe ;;
    *an_npm_shim_that_names_another_script_re_pins_it_through_current) echo native_auth_host_executable_unsafe ;;
    *one_verified_call_hashes_the_pinned_harness_once) echo 'left == right' ;;
    *the_custodian_job_holds_its_tree_from_the_start_and_ends_it_on_drop) echo native_auth_host_unavailable ;;
    *a_custodian_whose_hold_fails_is_killed) echo 'the custodian outlived its failed hold' ;;
    *) echo ok ;;
  esac
}
for run in 1 2 3; do
  cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
  echo "run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  wrong=0
  while read -r row; do
    want=$(expect "$row")
    line=$(grep -E "^test ${row} \.\.\. " "$RUNNER_TEMP/rows.log" | head -1)
    if [ "$want" = ok ]; then
      case "$line" in *' ... ok') ;; *) echo "  WRONG $row: want ok, got: $line"; wrong=$((wrong + 1)) ;; esac
    else
      why=$(awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} show' "$RUNNER_TEMP/rows.log" | grep -E 'panicked at|left:|right:|Err\(|outlived|failed' | head -4 | tr '\n' ' ')
      case "$line" in
        *' ... FAILED') case "$why" in *"$want"*) echo "  red as meant: ${row#engine::native_auth_host::} ($want)" ;; *) echo "  WRONG $row: red for another reason: $why"; wrong=$((wrong + 1)) ;; esac ;;
        *) echo "  WRONG $row: want red ($want), got: $line"; wrong=$((wrong + 1)) ;;
      esac
    fi
  done < "$RUNNER_TEMP/rows.list"
  echo "red check run $run: $wrong rows not as meant"
  [ "$wrong" -eq 0 ] || bad=$((bad + 1))
done
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
