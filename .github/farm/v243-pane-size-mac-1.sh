set -uo pipefail
phase=$1
tip=d4f248374
red=622401c12
base=8953b0b7c
R3=native_term::control::tests::a_bound_panes_layout_change_gives_its_views_the_new_size
R5=native_term::screen::tests::a_clipped_view_draws_the_rows_that_end_at_the_cursor
A11Y=native_term::a11y::tests::cells_become_view_points_at_the_display_scale
R4=bridge::feed::tests::a_bound_views_session_kind_reaches_its_opener
FILES="apps/desktop/src-tauri/src/native_term/a11y.rs apps/desktop/src-tauri/src/native_term/control.rs apps/desktop/src-tauri/src/native_term/host.rs apps/desktop/src-tauri/src/native_term/screen.rs apps/desktop/src-tauri/src/native_term/table.rs apps/desktop/src/screens/team/native-panel.tsx "
T="$RUNNER_TEMP/t"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
code_tip() { cat "$RUNNER_TEMP/code_tip"; }
at_sha() {
  case "$1" in
    RED) echo "$red" ;;
    TIP) code_tip ;;
    BASE) echo "$base" ;;
  esac
}
checkout() {
  git checkout -q -f --detach "$(at_sha "$1")" || return 1
  mkdir -p apps/desktop/dist
  [ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
}
block() {
  awk -v head="---- $1 stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$2"
}
first_panic() {
  block "$1" "$2" | grep -m1 -A2 'panicked at' | tr '\n' ' ' | sed -E "s/^thread '[^']*' \([0-9]+\) //" | cut -c1-300
}
build_lib() {
  local at=$1 out rc lib start
  checkout "$at" || return 1
  start=$(date +%s)
  out=$(cargo test -p crazyagent-desktop --lib --locked --no-run 2>&1)
  rc=$?
  echo "  $at desktop lib build rc=$rc in $(( $(date +%s) - start )) s"
  if [ "$rc" -ne 0 ]; then
    printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300
    row "desktop lib build at $at" built "NOT: rc $rc"
    return 1
  fi
  lib=$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)
  [ -n "$lib" ] || { row "desktop lib build at $at" built "NOT: no lib test executable"; return 1; }
  mkdir -p "$T/$at"
  cp "$(realpath "$lib")" "$T/$at/lib"
}
named() {
  local at=$1 name=$2 want=$3 log got
  log="$RUNNER_TEMP/named-$at-${name##*::}.log"
  "$T/$at/lib" --exact "$name" > "$log" 2>&1
  if grep -qx "test $name ... FAILED" "$log"; then
    got="fails: $(first_panic "$name" "$log")"
  elif grep -qx "test $name ... ok" "$log"; then
    got=passes
  else
    got="no result line"
    tail -12 "$log" | cut -c1-300
  fi
  case "$got" in
    "$want"*) row "${name##*::} at $at" "$want" "$got"; return 0 ;;
    *) row "${name##*::} at $at" "$want" "NOT: $got"; return 1 ;;
  esac
}
failed_names() {
  grep -E '^[[:space:]]+FAIL \[' "$1" | sed -E 's/^[[:space:]]+FAIL \[[^]]*\] (\([^)]*\) )?//' | awk '{print $1, $2}' | sort -u
}
whole() {
  local at=$1 log rc start
  log="$RUNNER_TEMP/whole-$at.log"
  start=$(date +%s)
  cargo nextest run -p crazyagent-desktop --lib --profile farm --locked --no-fail-fast > "$log" 2>&1
  rc=$?
  echo "  $at whole desktop lib rc=$rc in $(( $(date +%s) - start )) s: $(grep -m1 -E '^[[:space:]]+Summary' "$log" | sed -E 's/^[[:space:]]+//' | cut -c1-200)"
  failed_names "$log" > "$RUNNER_TEMP/failed-$at.txt"
  echo "  $at failed rows: $(wc -l < "$RUNNER_TEMP/failed-$at.txt")"
  sed 's/^/    failed: /' "$RUNNER_TEMP/failed-$at.txt"
  grep -qE '^[[:space:]]+Summary' "$log" || { tail -30 "$log" | cut -c1-300; return 2; }
  return "$rc"
}
case "$phase" in
  build)
    echo "git on PATH: $(command -v git), $(git --version)"
    echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-unknown}"
    sw_vers
    code=$(git log -1 --format=%H HEAD -- . ':!.github')
    echo "$code" > "$RUNNER_TEMP/code_tip"
    parent="$(git log -1 --format=%p --abbrev=9 "$code")"
    if [ "$(git rev-parse --short=9 "$code")" != "$tip" ] || [ "$parent" != "$red" ]; then
      row chain "$tip on $red" "NOT: $(git rev-parse --short=9 "$code") on $parent"
      exit 1
    fi
    got="$(git diff --name-only "$red" "$code" | tr '\n' ' ')"
    if [ "$got" != "$FILES" ]; then
      row "files $red..$tip" "the 6 fix files" "NOT: $got"
      exit 1
    fi
    row "chain and files" "$tip on $red; the 6 fix files" yes
    build_lib RED || exit 1
    build_lib TIP || exit 1
    start=$(date +%s)
    cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; row "engine bin build at TIP" built "NOT: failed"; exit 1; }
    echo "  TIP engine bin build in $(( $(date +%s) - start )) s"
    start=$(date +%s)
    cargo test -p crazyagent-desktop --test terminal_bridge --locked --no-run > "$RUNNER_TEMP/bridge.log" 2>&1 || { grep -m20 -A6 "error\[E\|^error" "$RUNNER_TEMP/bridge.log" | cut -c1-300; row "terminal_bridge build at TIP" built "NOT: failed"; exit 1; }
    echo "  TIP terminal_bridge build in $(( $(date +%s) - start )) s"
    row "builds" "RED and TIP desktop lib tests, TIP engine bin and terminal_bridge" built
    ;;
  red)
    checkout RED
    bad=0
    named RED "$R3" fails || bad=1
    named RED "$R5" fails || bad=1
    named RED "$A11Y" fails || bad=1
    named RED "$R4" passes || bad=1
    checkout TIP
    exit "$bad"
    ;;
  tip)
    checkout TIP
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
    bad=0
    for name in "$R3" "$R5" "$A11Y" "$R4"; do
      named TIP "$name" passes || bad=1
    done
    whole TIP
    rc=$?
    if [ "$rc" = 0 ]; then
      row "whole desktop lib at $tip" "0 failed" "0 failed"
    elif [ "$rc" = 2 ]; then
      row "whole desktop lib at $tip" "a summary line" "NOT: none"
      exit 1
    else
      echo "  red rows at the tip: comparing with the base $base"
      build_lib BASE || exit 1
      whole BASE
      only_tip=$(comm -13 "$RUNNER_TEMP/failed-BASE.txt" "$RUNNER_TEMP/failed-TIP.txt" | tr '\n' ';')
      row "whole desktop lib at $tip" "0 failed, or only rows also red at $base" "$(wc -l < "$RUNNER_TEMP/failed-TIP.txt" | tr -d ' ') failed; red only at the tip: ${only_tip:-none}"
      [ -z "$only_tip" ] || bad=1
      checkout TIP
    fi
    exit "$bad"
    ;;
  bridge)
    checkout TIP
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
    log="$RUNNER_TEMP/bridge-run.log"
    start=$(date +%s)
    cargo nextest run -p crazyagent-desktop --test terminal_bridge --profile farm --locked --no-fail-fast > "$log" 2>&1
    rc=$?
    echo "  terminal_bridge rc=$rc in $(( $(date +%s) - start )) s: $(grep -m1 -E '^[[:space:]]+Summary' "$log" | sed -E 's/^[[:space:]]+//' | cut -c1-200)"
    failed_names "$log" | sed 's/^/    failed: /'
    for name in wall_windows_reach_only_the_terminal_graph_stop_and_setting_read_commands desktop_reads_the_team_graph_through_the_real_engine; do
      if grep -qE "^[[:space:]]+PASS \[.*terminal_bridge $name$" "$log"; then
        row "terminal_bridge $name at $tip" passes passes
      else
        row "terminal_bridge $name at $tip" passes "NOT: $(grep -E "terminal_bridge $name$" "$log" | head -1 | sed -E 's/^[[:space:]]+//' | cut -c1-160)"
        rc=1
      fi
    done
    [ "$rc" = 0 ] && row "terminal_bridge at $tip" "0 failed" "0 failed"
    exit "$rc"
    ;;
esac
