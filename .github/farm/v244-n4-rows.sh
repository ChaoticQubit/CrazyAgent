set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4 env red mode=$MODE at $tip on $ImageOS"
mod='engine::native_auth_host::login_windows_tests::'
cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { grep -E '^error' -A8 "$RUNNER_TEMP/bin.log" | head -60; exit 1; }
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1 || { grep -E '^(error|warning)' -A8 "$RUNNER_TEMP/build.log" | head -80; exit 1; }
echo "builds in $(( $(date +%s) - started )) s"
cargo test -p crazyagent --lib --locked -- --list 2>/dev/null | grep -E "^${mod}[^ ]*: test$" | sed 's/: test$//' > "$RUNNER_TEMP/rows.list"
echo "rows $(wc -l < "$RUNNER_TEMP/rows.list" | tr -d ' ') (want 2)"
expect() {
  case "$1" in
    *a_login_child_env_is_the_boundary_env_with_its_config_home_and_the_c_locale) echo 'left: []' ;;
    *a_login_child_runs_only_once_held_and_sees_only_its_own_env) echo 'the login child ran before its tree held it' ;;
  esac
}
bad=0
for run in 1 2 3; do
  cargo test -p crazyagent --lib --locked -- --exact --test-threads=1 $(cat "$RUNNER_TEMP/rows.list") > "$RUNNER_TEMP/rows.log" 2>&1
  echo "run $run rc=$? | $(grep -m1 '^test result' "$RUNNER_TEMP/rows.log")"
  while read -r row; do
    why=$(awk -v h="---- ${row} stdout ----" '$0==h{show=1;next} /^---- .* stdout ----$/{show=0} /^failures:$/{show=0} show' "$RUNNER_TEMP/rows.log")
    echo "$why" | grep -E -A2 'panicked at' | head -4 | sed 's/^/    /'
    want=$(expect "$row")
    if grep -qE "^test ${row} \.\.\. FAILED$" "$RUNNER_TEMP/rows.log" && printf '%s' "$why" | grep -qF "$want"; then echo "  red as meant: ${row#$mod} ($want)"; else echo "  WRONG ${row#$mod}"; bad=$((bad + 1)); fi
  done < "$RUNNER_TEMP/rows.list"
done
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
