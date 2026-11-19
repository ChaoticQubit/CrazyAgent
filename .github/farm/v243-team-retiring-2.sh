set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=2d2419e73
mine=be591c56b
row_commit=3feab9c8f
fix1=2c38d00ad
red=3b715fb5f
base=977692ab5
test_file=crates/crazyagent/src/engine/owner/transport/setup_tests.rs
NEW=engine::owner::transport::setup_tests::the_owner_server_s_start_tasks_add_nothing_to_the_schema_the_engine_start_made
NATIVE=engine::owner::transport::native_profile_tests::profile_preparation_changes_no_durable_engine_or_admission_state
MODULES="engine::owner::transport:: storage:: project_setup:: projects_home:: engine::tests::"
LANDING="index landing_agent_branches_repository index landing_change_sets_open index landing_change_sets_project table landing_agent_branches table landing_bases table landing_change_set_numbers table landing_change_sets"
WANT_RED="$LANDING table team_retiring"
WANT_FIX1="$LANDING"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git --version
echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-unknown}"
echo "base sha: $base"
git log --oneline -6
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
parents="$(git log -1 --format=%p --abbrev=9 "$code_tip")"
mine_chain="$(git log --format=%h --abbrev=9 -4 "$mine" | tr '\n' ' ')"
if [ "$(git rev-parse --short=9 "$code_tip")" != "$tip" ] || [ "$parents" != "$mine $base" ] || [ "$mine_chain" != "$mine $row_commit $fix1 $red " ]; then
  row chain "$tip = merge of $mine and $base; $mine $row_commit $fix1 $red" "NOT: $(git rev-parse --short=9 "$code_tip") parents $parents; $mine_chain"
  exit 1
fi
files() { git diff --name-only "$1" "$2" | tr '\n' ' '; }
want_files="crates/crazyagent/src/engine/owner/transport/setup_tests.rs crates/crazyagent/src/landing.rs crates/crazyagent/src/storage.rs crates/crazyagent/src/team/cleanup.rs "
if [ "$(files "$fix1" "$row_commit")" != "$test_file " ] || [ "$(files "$row_commit" "$mine")" != "crates/crazyagent/src/landing.rs crates/crazyagent/src/storage.rs " ] || [ "$(files "$base" "$tip")" != "$want_files" ]; then
  row "files per commit" "row: the test file; landing fix: landing.rs and storage.rs; $base..$tip: the 4 files" "NOT: row: $(files "$fix1" "$row_commit"); fix: $(files "$row_commit" "$mine"); merge: $(files "$base" "$tip")"
  exit 1
fi
row "files per commit" "row: the test file; landing fix: landing.rs and storage.rs; $base..$tip: the 4 files" yes
checkout() {
  case "$1" in
    RED)
      git checkout -q -f --detach "$base" || return 1
      git checkout -q "$code_tip" -- "$test_file" || return 1
      ;;
    FIX1)
      git checkout -q -f --detach "$code_tip" || return 1
      git diff "$row_commit" "$mine" | git apply -R || return 1
      ;;
    TIP) git checkout -q -f --detach "$code_tip" || return 1 ;;
  esac
}
stash() {
  local at=$1 out lib bin
  checkout "$at" || return 1
  echo "  $at tree: $(git rev-parse --short=9 HEAD), changed: $(git status --short | tr '\n' ' ')"
  out=$(cargo test -p crazyagent --lib --locked --no-run 2>&1) || { printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300; return 1; }
  lib=$(realpath "$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)")
  echo "  $at lib build ok"
  mkdir -p "$RUNNER_TEMP/t/$at"
  cp "$lib" "$RUNNER_TEMP/t/$at/lib"
  for bin in crazyagent crazyagent-landlock; do
    cargo build -p crazyagent --bin "$bin" --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; return 1; }
    cp "target/debug/$bin" "$RUNNER_TEMP/t/$at/$bin"
    echo "  $at bin $bin ok"
  done
}
for at in RED FIX1 TIP; do
  if ! stash "$at"; then
    row "lib build at $at" built "NOT: failed"
    exit 1
  fi
done
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
tree() {
  checkout "$1"
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$RUNNER_TEMP/t/$1/crazyagent"
}
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file"
}
first_panic() {
  block "$1" "$2" | grep -m1 -A2 'panicked at' | tr '\n' ' ' | cut -c1-400
}
added_set() {
  block "$NEW" "$1" | grep -oE '^    "(table|index) [a-z_]+' | sed 's/^    "//' | sort -u | tr '\n' ' ' | sed 's/ $//'
}
want_sorted() { printf '%s\n' $1 | paste -d' ' - - | sort | tr '\n' ' ' | sed 's/ $//'; }
alone() {
  local at=$1 times=$2 want=$3 i log got
  tree "$at"
  for i in $(seq 1 "$times"); do
    log="$RUNNER_TEMP/alone-$at-$i.log"
    keyring "$RUNNER_TEMP/t/$at/lib" --exact "$NEW" > "$log" 2>&1
    if grep -qx "test $NEW ... FAILED" "$log"; then
      got="fails, added: $(added_set "$log")"
      block "$NEW" "$log" | head -60 | cut -c1-300
    elif grep -qx "test $NEW ... ok" "$log"; then
      got=passes
    else
      got="no result line"
      tail -12 "$log" | cut -c1-300
    fi
    if [ "$got" = "$want" ]; then
      row "${NEW##*::} alone at $at run $i" "$want" "$got"
    else
      row "${NEW##*::} alone at $at run $i" "$want" "NOT: $got"
      failed=1
    fi
  done
}
echo "== the new row alone"
alone RED 1 "fails, added: $(want_sorted "$WANT_RED")"
alone FIX1 1 "fails, added: $(want_sorted "$WANT_FIX1")"
alone TIP 3 passes
declare -A reds
declare -A missing
declare -A reason
run_lib() {
  local at=$1 label=$2 log rc name
  shift 2
  tree "$at"
  log="$RUNNER_TEMP/$label-$at.log"
  keyring "$RUNNER_TEMP/t/$at/lib" "$@" > "$log" 2>&1
  rc=$?
  echo "  $at $label rc=$rc $(grep -m1 '^test result' "$log" | cut -c1-140)"
  for name in $(grep '^test .* FAILED$' "$log" | sed 's/^test //; s/ \.\.\. FAILED$//'); do
    reds[$label:$at:$name]=$(( ${reds[$label:$at:$name]:-0} + 1 ))
    [ -n "${reason[$at:$name]:-}" ] || reason[$at:$name]="$(first_panic "$name" "$log")"
    echo "    failed: $name"
  done
  for name in "$NATIVE" "$NEW"; do
    grep -q "^test $name \.\.\. \(ok\|FAILED\)$" "$log" || missing[$label:$at:$name]=$(( ${missing[$label:$at:$name]:-0} + 1 ))
  done
}
read -r -a filters <<< "$MODULES"
echo "== modules alternated RED and TIP, $MODULES"
for i in 1 2 3 4 5; do
  run_lib RED "modules$i" "${filters[@]}"
  run_lib TIP "modules$i" "${filters[@]}"
done
echo "== the whole lib once per tree"
run_lib RED whole
run_lib TIP whole
count() {
  local at=$1 name=$2 n=0 key
  for key in "${!reds[@]}"; do
    case "$key" in modules*:"$at:$name") n=$((n + reds[$key])) ;; esac
  done
  echo "$n"
}
echo "== every failed row, by tree, with its first panic line (runner ${ImageOS:-unknown} ${ImageVersion:-unknown}, base $base)"
for key in "${!reds[@]}"; do
  rest=${key#*:}
  echo "${rest%%:*} ${rest#*:}"
done | sort -u | while read -r at name; do
  label="modules $(count "$at" "$name") of 5"
  [ -n "${reds[whole:$at:$name]:-}" ] && label="$label, whole lib red"
  echo "  $at $name: $label | ${reason[$at:$name]}"
done
native_tip=$(count TIP "$NATIVE")
if [ "$native_tip" = 0 ] && [ -z "${missing[modules1:TIP:$NATIVE]:-}${missing[modules2:TIP:$NATIVE]:-}${missing[modules3:TIP:$NATIVE]:-}${missing[modules4:TIP:$NATIVE]:-}${missing[modules5:TIP:$NATIVE]:-}" ]; then
  row "${NATIVE##*::} in 5 modules runs at $tip" "0 of 5 failed, 5 ran" "0 of 5 failed, 5 ran"
else
  row "${NATIVE##*::} in 5 modules runs at $tip" "0 of 5 failed, 5 ran" "NOT: $native_tip failed or a run without its result line"
  failed=1
fi
new_red=$(count RED "$NEW")
new_tip=$(count TIP "$NEW")
if [ "$new_red" = 5 ] && [ "$new_tip" = 0 ]; then
  row "${NEW##*::} in 5 modules runs" "RED 5 of 5 failed, TIP 0 of 5" "RED 5, TIP 0"
else
  row "${NEW##*::} in 5 modules runs" "RED 5 of 5 failed, TIP 0 of 5" "NOT: RED $new_red, TIP $new_tip"
  failed=1
fi
only_tip=""
for key in "${!reds[@]}"; do
  rest=${key#*:}
  at=${rest%%:*}
  name=${rest#*:}
  [ "$at" = TIP ] || continue
  [ "$name" = "$NEW" ] && continue
  if [ "$(count RED "$name")" = 0 ] && [ -z "${reds[whole:RED:$name]:-}" ]; then
    only_tip="$only_tip $name"
  fi
done
only_tip=$(printf '%s\n' $only_tip | sort -u | tr '\n' ' ')
if [ -z "${only_tip// /}" ]; then
  row "rows failing at $tip that never failed at RED (modules or whole lib)" none none
else
  row "rows failing at $tip that never failed at RED (modules or whole lib)" none "NOT: $only_tip"
  failed=1
fi
whole_red=$(grep -m1 '^test result' "$RUNNER_TEMP/whole-RED.log" | cut -c1-140)
whole_tip=$(grep -m1 '^test result' "$RUNNER_TEMP/whole-TIP.log" | cut -c1-140)
row "whole lib at RED" record "$whole_red"
row "whole lib at TIP" record "$whole_tip"
git checkout -q -f "$code_tip"
exit "$failed"
