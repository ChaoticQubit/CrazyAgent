set -uo pipefail
phase=$1
tip=2d2419e73
mine=be591c56b
base=977692ab5
test_file=crates/crazyagent/src/engine/owner/transport/setup_tests.rs
NEW=engine::owner::transport::setup_tests::the_owner_server_s_start_tasks_add_nothing_to_the_schema_the_engine_start_made
LANDING="index landing_agent_branches_repository index landing_change_sets_open index landing_change_sets_project table landing_agent_branches table landing_bases table landing_change_set_numbers table landing_change_sets"
T="$RUNNER_TEMP/t"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
code_tip() { cat "$RUNNER_TEMP/code_tip"; }
checkout() {
  case "$1" in
    BASE)
      git checkout -q -f --detach "$base" || return 1
      git checkout -q "$(code_tip)" -- "$test_file" || return 1
      ;;
    TIP) git checkout -q -f --detach "$(code_tip)" || return 1 ;;
  esac
}
block() {
  awk -v head="---- $1 stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$2"
}
first_panic() {
  block "$1" "$2" | grep -m1 -A2 'panicked at' | tr '\n' ' ' | sed -E "s/^thread '[^']*' \([0-9]+\) //" | cut -c1-300
}
added_set() {
  block "$NEW" "$1" | grep -oE '^    "(table|index) [a-z_]+' | sed 's/^    "//' | sort -u | tr '\n' ' ' | sed 's/ $//'
}
want_sorted() { printf '%s\n' $1 | paste -d' ' - - | sort | tr '\n' ' ' | sed 's/ $//'; }
failed_names() { grep -E '^test .* \.\.\. FAILED$' "$1" | sed -E 's/^test //; s/ - should panic//; s/ \.\.\. FAILED$//' | sort -u; }
case "$phase" in
  build)
    echo "git on PATH: $(command -v git), $(git --version)"
    echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-unknown}"
    sw_vers
    code=$(git log -1 --format=%H HEAD -- . ':!.github')
    echo "$code" > "$RUNNER_TEMP/code_tip"
    parents="$(git log -1 --format=%p --abbrev=9 "$code")"
    if [ "$(git rev-parse --short=9 "$code")" != "$tip" ] || [ "$parents" != "$mine $base" ]; then
      row chain "$tip = merge of $mine and $base" "NOT: $(git rev-parse --short=9 "$code") parents $parents"
      exit 1
    fi
    want="crates/crazyagent/src/engine/owner/transport/setup_tests.rs crates/crazyagent/src/landing.rs crates/crazyagent/src/storage.rs crates/crazyagent/src/team/cleanup.rs "
    got="$(git diff --name-only "$base" "$code" | tr '\n' ' ')"
    if [ "$got" != "$want" ]; then
      row "files $base..$tip" "the 4 files" "NOT: $got"
      exit 1
    fi
    row "chain and files" "$tip merges $mine and $base; the 4 files" yes
    for at in TIP BASE; do
      checkout "$at" || exit 1
      echo "  $at tree $(git rev-parse --short=9 HEAD), changed: $(git status --short | tr '\n' ' ')"
      start=$(date +%s)
      out=$(cargo test -p crazyagent --lib --locked --no-run 2>&1)
      rc=$?
      echo "  $at lib build rc=$rc in $(( $(date +%s) - start )) s"
      if [ "$rc" -ne 0 ]; then
        printf '%s\n' "$out" | grep -m20 -A6 "error\[E\|^error" | cut -c1-300
        row "lib build at $at" built "NOT: rc $rc"
        exit 1
      fi
      lib=$(printf '%s\n' "$out" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))/\1/p' | tail -1)
      [ -n "$lib" ] || { row "lib build at $at" built "NOT: no lib test executable"; exit 1; }
      mkdir -p "$T/$at"
      cp "$(realpath "$lib")" "$T/$at/lib"
      cargo build -p crazyagent --bin crazyagent --locked > "$RUNNER_TEMP/bin.log" 2>&1 || { tail -20 "$RUNNER_TEMP/bin.log" | cut -c1-300; row "bin build at $at" built "NOT: failed"; exit 1; }
      cp target/debug/crazyagent "$T/$at/crazyagent"
      echo "  $at bins kept"
    done
    checkout TIP
    ;;
  alone)
    bad=0
    for spec in "BASE 1 fails" "TIP 3 passes"; do
      read -r at times want <<< "$spec"
      checkout "$at"
      export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$T/$at/crazyagent"
      [ "$want" = fails ] && want="fails, added: $(want_sorted "$LANDING table team_retiring")"
      for i in $(seq 1 "$times"); do
        log="$RUNNER_TEMP/alone-$at-$i.log"
        "$T/$at/lib" --exact "$NEW" > "$log" 2>&1
        if grep -qx "test $NEW ... FAILED" "$log"; then
          got="fails, added: $(added_set "$log")"
          block "$NEW" "$log" | head -40 | cut -c1-300
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
          bad=1
        fi
      done
    done
    checkout TIP
    exit "$bad"
    ;;
  whole)
    at=$2
    checkout "$at"
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$T/$at/crazyagent"
    log="$RUNNER_TEMP/whole-$at.log"
    start=$(date +%s)
    "$T/$at/lib" > "$log" 2>&1
    rc=$?
    echo "  $at whole lib rc=$rc in $(( $(date +%s) - start )) s: $(grep -m1 '^test result' "$log" | cut -c1-160)"
    failed_names "$log" > "$RUNNER_TEMP/failed-$at.txt"
    echo "  $at failed rows: $(wc -l < "$RUNNER_TEMP/failed-$at.txt")"
    while read -r name; do
      echo "    $at failed: $name | $(first_panic "$name" "$log")"
    done < "$RUNNER_TEMP/failed-$at.txt"
    checkout TIP
    grep -q '^test result' "$log" || { row "whole lib at $at" "a result line" "NOT: none, rc $rc"; exit 1; }
    ;;
  compare)
    bad=0
    for at in BASE TIP; do
      [ -f "$RUNNER_TEMP/failed-$at.txt" ] || { row "whole lib at $at" ran "NOT: no failed list"; exit 1; }
      row "whole lib at $at" record "$(grep -m1 '^test result' "$RUNNER_TEMP/whole-$at.log" | cut -c1-160)"
    done
    only_tip=$(comm -13 "$RUNNER_TEMP/failed-BASE.txt" "$RUNNER_TEMP/failed-TIP.txt" | tr '\n' ' ')
    only_base=$(comm -23 "$RUNNER_TEMP/failed-BASE.txt" "$RUNNER_TEMP/failed-TIP.txt" | tr '\n' ' ')
    echo "  red only at the base: ${only_base:-none}"
    echo "  red only at the tip: ${only_tip:-none}"
    if [ -z "${only_tip// /}" ]; then
      row "rows red at $tip that are not red at the base" none none
    else
      row "rows red at $tip that are not red at the base" none "NOT: $only_tip"
      bad=1
    fi
    if [ "$only_base" = "$NEW " ]; then
      row "rows red only at the base" "the new row" "the new row"
    else
      row "rows red only at the base" "the new row" "NOT: ${only_base:-none}"
      bad=1
    fi
    changed=0
    while read -r name; do
      a=$(first_panic "$name" "$RUNNER_TEMP/whole-BASE.log")
      b=$(first_panic "$name" "$RUNNER_TEMP/whole-TIP.log")
      if [ "$a" != "$b" ]; then
        changed=$((changed + 1))
        echo "    reason differs: $name | base: $a | tip: $b"
      fi
    done < <(comm -12 "$RUNNER_TEMP/failed-BASE.txt" "$RUNNER_TEMP/failed-TIP.txt")
    row "rows red at both with a different first panic line" 0 "$changed"
    [ "$changed" = 0 ] || bad=1
    exit "$bad"
    ;;
  clippy)
    git checkout -q -f --detach "$(code_tip)"
    start=$(date +%s)
    out=$(cargo clippy -p crazyagent --lib --tests --locked -- -D warnings 2>&1)
    rc=$?
    echo "  clippy --lib --tests rc=$rc in $(( $(date +%s) - start )) s"
    printf '%s\n' "$out" | grep -E -m30 -A8 '^(warning|error)' | cut -c1-300
    if [ "$rc" = 0 ]; then row "clippy --lib --tests -D warnings at $tip" 0 0; else row "clippy --lib --tests -D warnings at $tip" 0 "NOT: rc $rc"; exit 1; fi
    ;;
esac
