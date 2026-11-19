set -uo pipefail
fix=cda30127b
red=12836a9ff
change=9d53209d7
base=dae148dee
ENDED=engine_input_reaches_an_idle_live_master_only_while_the_owner_is_not_typing
TYPING=a_failed_typing_frees_the_live_master_for_the_next_input
MODULE=team::master::tests::
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
bad=0
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
block() {
  awk -v head="---- $1 stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$2"
}
first_panic() {
  block "$1" "$2" | grep -m1 -A2 'panicked at' | tr '\n' ' ' | sed -E "s/^thread '[^']*' \([0-9]+\) //" | cut -c1-300
}
short() { git rev-parse --short=9 "$1"; }
echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-unknown}, kernel $(uname -r), $(tmux -V)"
code=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(short "$code") $(short "$code~1") $(short "$code~2") $(short "$code~3")"
if [ "$chain" != "$fix $red $change $base" ]; then
  row chain "$fix on $red on $change on $base" "NOT: $chain"
  exit 1
fi
row chain "$fix on $red on $change on $base" yes
built() {
  local at=$1 out lib
  git checkout -q -f --detach "$at" || return 1
  echo "  tree $(short HEAD), changed: $(git status --short | tr '\n' ' ')"
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; return 1; }
  out=$(cargo test -p crazyagent --lib --locked --no-run 2>&1) || { printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300; return 1; }
  lib=$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)
  [ -n "$lib" ] || { echo "  no lib test executable"; return 1; }
  cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  LIB=$(realpath "$lib")
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
  echo "  lib $LIB, helper $(ls target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock | tr '\n' ' ')"
}
judge() {
  local fn=$1 at=$2 want=$3 runs=$4 full n log got i
  full=$(keyring "$LIB" --list 2>/dev/null | grep -E "^${MODULE}${fn}: test$" | sed 's/: test$//')
  n=$(printf '%s\n' "$full" | grep -c .)
  if [ "$n" != 1 ]; then
    row "$fn at $at" "one row" "NOT: $n rows match"
    bad=1
    return
  fi
  for i in $(seq 1 "$runs"); do
    log="$RUNNER_TEMP/$at-$fn-$i.log"
    keyring "$LIB" --exact "$full" > "$log" 2>&1
    if grep -qx "test $full ... FAILED" "$log"; then
      got=fails
      echo "    $at $full: $(first_panic "$full" "$log")"
    elif grep -qx "test $full ... ok" "$log"; then
      got=passes
    else
      got="no result line"
      tail -12 "$log" | cut -c1-300
    fi
    if [ "$got" = "$want" ]; then
      row "$full at $at run $i" "$want" "$got"
    else
      row "$full at $at run $i" "$want" "NOT: $got"
      bad=1
    fi
  done
}
module() {
  local at=$1 listed log result passed failed
  listed=$(keyring "$LIB" --list 2>/dev/null | grep -cE "^${MODULE}[a-z0-9_]+: test$")
  log="$RUNNER_TEMP/$at-module.log"
  keyring "$LIB" "$MODULE" > "$log" 2>&1
  result=$(grep -m1 '^test result: ' "$log")
  passed=$(printf '%s\n' "$result" | sed -n 's/.* \([0-9][0-9]*\) passed;.*/\1/p')
  failed=$(printf '%s\n' "$result" | sed -n 's/.* \([0-9][0-9]*\) failed;.*/\1/p')
  echo "  $MODULE at $at: $listed listed, $result"
  grep -E '^test .* \.\.\. FAILED$' "$log" | while read -r line; do
    name=$(printf '%s\n' "$line" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/')
    echo "    $at $name: $(first_panic "$name" "$log")"
  done
  if [ "$listed" -gt 0 ] && [ "$passed" = "$listed" ] && [ "$failed" = 0 ]; then
    row "$MODULE at $at" "all $listed pass" "$passed passed, $failed failed"
  else
    row "$MODULE at $at" "all $listed pass" "NOT: ${passed:-none} passed, ${failed:-none} failed"
    bad=1
  fi
}
start=$(date +%s)
built "$red" || { row "build at $red" built NOT; exit 1; }
echo "  red build in $(( $(date +%s) - start )) s"
judge "$ENDED" "$red" fails 1
judge "$TYPING" "$red" passes 1
start=$(date +%s)
built "$fix" || { row "build at $fix" built NOT; exit 1; }
echo "  fix build in $(( $(date +%s) - start )) s"
judge "$ENDED" "$fix" passes 3
judge "$TYPING" "$fix" passes 3
module "$fix"
exit "$bad"
