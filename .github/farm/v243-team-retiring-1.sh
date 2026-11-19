set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
fix=2c38d00ad
red=3b715fb5f
base=62bf3cbe2
NEW=engine::owner::transport::setup_tests::the_owner_server_s_start_tasks_add_nothing_to_the_schema_the_engine_start_made
NATIVE=engine::owner::transport::native_profile_tests::profile_preparation_changes_no_durable_engine_or_admission_state
MODULES="engine::owner::transport:: storage:: project_setup:: projects_home:: engine::tests::"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git --version
git log --oneline -4
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --format=%h --abbrev=9 -3 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$fix $red $base " ]; then
  row chain "$fix $red $base" "NOT: $chain"
  exit 1
fi
red_files="$(git diff --name-only "$base" "$red" | tr '\n' ' ')"
fix_files="$(git diff --name-only "$red" "$fix" | tr '\n' ' ')"
want_red="crates/crazyagent/src/engine/owner/transport/setup_tests.rs "
want_fix="crates/crazyagent/src/storage.rs crates/crazyagent/src/team/cleanup.rs "
if [ "$red_files" != "$want_red" ] || [ "$fix_files" != "$want_fix" ]; then
  row "files per commit" "red: $want_red; fix: $want_fix" "NOT: red: $red_files; fix: $fix_files"
  exit 1
fi
row "files per commit" "red: the test file; fix: storage.rs and team/cleanup.rs" yes
stash() {
  local at=$1 sha=$2 out lib bin
  git checkout -q -f --detach "$sha" || return 1
  out=$(cargo test -p crazyagent --lib --locked --no-run 2>&1) || { printf '%s\n' "$out" | grep -m20 "error\[E\|^error" | cut -c1-300; return 1; }
  lib=$(realpath "$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)")
  echo "  $at lib build ok at $(git rev-parse --short=9 HEAD)"
  mkdir -p "$RUNNER_TEMP/t/$at"
  cp "$lib" "$RUNNER_TEMP/t/$at/lib"
  for bin in crazyagent crazyagent-landlock; do
    cargo build -p crazyagent --bin "$bin" --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; return 1; }
    cp "target/debug/$bin" "$RUNNER_TEMP/t/$at/$bin"
    echo "  $at bin $bin ok"
  done
}
if ! stash RED "$red"; then
  row "lib build at $red" built "NOT: failed"
  exit 1
fi
if ! stash FIX "$fix"; then
  row "lib build at $fix" built "NOT: failed"
  exit 1
fi
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
tree() {
  case "$1" in
    RED) git checkout -q -f --detach "$red" ;;
    *) git checkout -q -f --detach "$fix" ;;
  esac
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$RUNNER_TEMP/t/$1/crazyagent"
}
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
alone() {
  local at=$1 name=$2 times=$3 i log
  tree "$at"
  fails=0
  ran=0
  named=0
  for i in $(seq 1 "$times"); do
    log="$RUNNER_TEMP/alone-$at-$i.log"
    keyring "$RUNNER_TEMP/t/$at/lib" --exact "$name" > "$log" 2>&1
    if grep -qx "test $name ... FAILED" "$log"; then
      fails=$((fails + 1))
      ran=$((ran + 1))
      block "$name" "$log" | cut -c1-600
      grep -q 'team_retiring' "$log" && named=$((named + 1))
    elif grep -qx "test $name ... ok" "$log"; then
      ran=$((ran + 1))
    else
      echo "  no result line in $at run $i"
      tail -8 "$log" | cut -c1-300
    fi
  done
  echo "== ${name##*::} alone at $at: $fails of $times failed, $ran ran, $named named team_retiring"
}
declare -A reds
declare -A missing
modules() {
  local at=$1 i=$2 log rc name
  tree "$at"
  log="$RUNNER_TEMP/modules-$at-$i.log"
  keyring "$RUNNER_TEMP/t/$at/lib" "${filters[@]}" > "$log" 2>&1
  rc=$?
  echo "  $at modules $i rc=$rc $(grep -m1 '^test result' "$log" | cut -c1-140)"
  for name in $(grep '^test .* FAILED$' "$log" | sed 's/^test //; s/ \.\.\. FAILED$//'); do
    reds[$at:$name]=$(( ${reds[$at:$name]:-0} + 1 ))
    echo "    failed: $name"
  done
  for name in "$NATIVE" "$NEW"; do
    grep -q "^test $name \.\.\. \(ok\|FAILED\)$" "$log" || missing[$at:$name]=$(( ${missing[$at:$name]:-0} + 1 ))
  done
  for name in "$NATIVE" "$NEW"; do
    if grep -qx "test $name ... FAILED" "$log"; then
      block "$name" "$log" | cut -c1-600
    fi
  done
}
echo "== red leg: the new row alone at $red"
alone RED "$NEW" 1
if [ "$fails" = 1 ] && [ "$named" = 1 ]; then
  row "${NEW##*::} at $red" "fails on team_retiring" "fails on team_retiring"
else
  row "${NEW##*::} at $red" "fails on team_retiring" "NOT: $fails failed, $named named team_retiring, $ran ran"
  failed=1
fi
echo "== green leg: the new row alone at $fix"
alone FIX "$NEW" 3
if [ "$fails" = 0 ] && [ "$ran" = 3 ]; then
  row "${NEW##*::} at $fix x3" "passes 3 of 3" "passes 3 of 3"
else
  row "${NEW##*::} at $fix x3" "passes 3 of 3" "NOT: $fails failed of $ran ran"
  failed=1
fi
read -r -a filters <<< "$MODULES"
echo "== modules alternated, $MODULES"
for i in 1 2 3 4 5; do
  modules RED "$i"
  modules FIX "$i"
done
echo "== counts of 5 modules runs per tree"
for key in "${!reds[@]}"; do
  echo "  ${key%%:*} ${key#*:} ${reds[$key]}"
done | sort
for name in "$NATIVE" "$NEW"; do
  echo "  ${name##*::}: RED ${reds[RED:$name]:-0} of 5, FIX ${reds[FIX:$name]:-0} of 5, no result line RED ${missing[RED:$name]:-0} FIX ${missing[FIX:$name]:-0}"
done
if [ "${reds[FIX:$NATIVE]:-0}" = 0 ] && [ "${missing[FIX:$NATIVE]:-0}" = 0 ]; then
  row "${NATIVE##*::} in 5 modules runs at $fix" "0 of 5 failed, 5 ran" "0 of 5 failed, 5 ran"
else
  row "${NATIVE##*::} in 5 modules runs at $fix" "0 of 5 failed, 5 ran" "NOT: ${reds[FIX:$NATIVE]:-0} failed, ${missing[FIX:$NATIVE]:-0} without a result line"
  failed=1
fi
if [ "${reds[FIX:$NEW]:-0}" = 0 ] && [ "${missing[FIX:$NEW]:-0}" = 0 ] && [ "${reds[RED:$NEW]:-0}" = 5 ]; then
  row "${NEW##*::} in 5 modules runs" "RED 5 of 5 failed, FIX 0 of 5" "RED 5 of 5 failed, FIX 0 of 5"
else
  row "${NEW##*::} in 5 modules runs" "RED 5 of 5 failed, FIX 0 of 5" "NOT: RED ${reds[RED:$NEW]:-0}, FIX ${reds[FIX:$NEW]:-0}, FIX without a result line ${missing[FIX:$NEW]:-0}"
  failed=1
fi
added=""
for key in "${!reds[@]}"; do
  case "$key" in
    FIX:*)
      name=${key#FIX:}
      [ "${reds[RED:$name]:-0}" = 0 ] && added="$added $name(${reds[$key]})"
      ;;
  esac
done
if [ -z "$added" ]; then
  row "rows failing at $fix that never failed at $red" none none
else
  row "rows failing at $fix that never failed at $red" none "NOT:$added"
  failed=1
fi
git checkout -q -f "$code_tip"
exit "$failed"
