set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4a tree red mode=$MODE at $tip on $ImageOS"
base='engine::native_auth_host::'
expect() {
  case "$1" in
    *::cleanup_ends_a_held_login_child_and_its_grandchild_within_the_bound) echo 'the grandchild outlived cleanup' ;;
    *::a_reaped_child_group_reads_present_while_a_grandchild_lives_and_gone_once_ended) echo 'a living grandchild read as a gone group' ;;
    *::dropping_the_held_tree_ends_the_login_child_and_its_grandchild) echo 'GREEN' ;;
    *::a_login_child_env_is_the_boundary_env_with_its_config_home_and_the_c_locale) echo 'left: []' ;;
    *::a_login_child_runs_only_once_held_and_sees_only_its_own_env) echo 'the login child ran before its tree held it' ;;
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
pass tree child_group_windows_tests 3
cp .github/farm/login_env_standin.rs crates/crazyagent/src/engine/native_auth_host/login_env.rs
echo "login_env swapped for the dbe579784 stand-in: $(git diff --stat -- crates | tail -1)"
pass login login_windows_tests 2
echo "end in $(( $(date +%s) - started )) s, bad=$bad"
[ "$bad" -eq 0 ]
