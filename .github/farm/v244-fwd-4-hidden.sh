set -uo pipefail
fix=be1f74f78
red=9d4d38987
merge=ff4c3bc73
first=1e6fe0e09
second=34fc7c22a
LIB_ROWS="stale_hidden_paths_write_is_refused_so_a_stale_screen_never_unhides_a_folder hidden_paths_read_names_engine_runs_that_started_before_a_folder_was_hidden keys_typed_into_the_hidden_folders_box_never_reach_the_plugins_list quitting_with_the_hidden_box_open_warns_the_hidden_folders_are_unsaved master_sandbox_hides_a_listed_folder_from_a_real_child_on_linux"
IT_ROWS="an_installed_sandbox_with_a_hidden_folder_keeps_it_out_or_refuses_it a_confined_agent_with_a_hidden_folder_is_refused_as_confined_not_for_the_folder"
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
echo "git on PATH: $(command -v git), $(git --version)"
echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-unknown}, kernel $(uname -r)"
command -v bwrap && bwrap --version
code=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(short "$code") $(short "$code~1") $(short "$code~2") $(git log -1 --format=%p --abbrev=9 "$code~2")"
if [ "$chain" != "$fix $red $merge $first $second" ]; then
  row chain "$fix on $red on $merge = $first + $second" "NOT: $chain"
  exit 1
fi
row chain "$fix on $red on $merge = $first + $second" yes
built() {
  local at=$1 out lib it
  git checkout -q -f --detach "$at" || return 1
  echo "  tree $(short HEAD), changed: $(git status --short | tr '\n' ' ')"
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; return 1; }
  out=$(cargo test -p crazyagent --lib --locked --no-run 2>&1) || { printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300; return 1; }
  lib=$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)
  out=$(cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1) || { printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300; return 1; }
  it=$(printf '%s\n' "$out" | sed -n 's/.*Executable tests\/linux_sandbox.rs (\(.*\))/\1/p' | tail -1)
  [ -n "$lib" ] && [ -n "$it" ] || { echo "  no test executable: lib '$lib' it '$it'"; return 1; }
  cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  LIB=$(realpath "$lib")
  IT=$(realpath "$it")
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
  echo "  lib $LIB, it $IT, helper $(ls target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock | tr '\n' ' ')"
}
judge() {
  local name=$1 at=$2 bin=$3 want=$4 runs=$5 fn full n log got i
  for fn in $name; do
    full=$(keyring "$bin" --list 2>/dev/null | grep -E "(^|::)$fn: test$" | sed 's/: test$//')
    n=$(printf '%s\n' "$full" | grep -c .)
    if [ "$n" != 1 ]; then
      row "$fn at $at" "one row" "NOT: $n rows match"
      bad=1
      continue
    fi
    for i in $(seq 1 "$runs"); do
      log="$RUNNER_TEMP/$at-$fn-$i.log"
      keyring "$bin" --exact "$full" > "$log" 2>&1
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
        row "${full} at $at run $i" "$want" "$got"
      else
        row "${full} at $at run $i" "$want" "NOT: $got"
        bad=1
      fi
    done
  done
}
start=$(date +%s)
built "$red" || { row "build at $red" built NOT; exit 1; }
echo "  red build in $(( $(date +%s) - start )) s"
judge "$LIB_ROWS" "$red" "$LIB" fails 1
judge "$IT_ROWS" "$red" "$IT" fails 1
start=$(date +%s)
built "$fix" || { row "build at $fix" built NOT; exit 1; }
echo "  fix build in $(( $(date +%s) - start )) s"
judge "$LIB_ROWS" "$fix" "$LIB" passes 3
judge "$IT_ROWS" "$fix" "$IT" passes 3
exit "$bad"
