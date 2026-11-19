set -uo pipefail
tip=b4ede513c
base=dd4852aeb
filter=(engine::keychain:: engine::bootstrap::native_profiles::use_tests::agent_copy_tests::)
red_rows="engine::keychain::tests::a_locked_profile_keychain_opens_with_the_empty_password_without_a_prompt
engine::keychain::tests::an_earlier_engines_login_keychain_is_renamed_and_found_through_the_search_list
engine::keychain::tests::a_locked_login_keychain_an_earlier_engine_made_is_renamed_opened_and_read
engine::keychain::tests::a_profile_home_holding_both_keychain_names_is_refused_and_both_stay
engine::keychain::tests::an_agent_keychain_is_refused_in_a_home_holding_a_profile_keychain_under_either_name
engine::bootstrap::native_profiles::use_tests::agent_copy_tests::an_agent_gets_a_claude_copy_from_a_locked_profile_keychain_without_a_prompt
engine::bootstrap::native_profiles::use_tests::agent_copy_tests::a_claude_profile_keychain_an_earlier_engine_named_login_is_renamed_on_the_first_read"
tip_rows="engine::keychain::tests::unlocking_trips_on_a_relative_keychain
engine::keychain::tests::unlocking_trips_on_a_login_keychain_security_refuses_to_unlock
engine::keychain::tests::a_profile_keychain_the_empty_password_cannot_open_is_refused_and_its_settings_stay
engine::bootstrap::native_profiles::use_tests::agent_copy_tests::a_locked_profile_keychain_the_empty_password_cannot_open_gives_no_copy_and_no_prompt"

check() {
  git log --oneline -4
  local code_tip
  code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
  [ "$code_tip" = "$tip" ] || { echo "NOT: code tip $code_tip, wanted $tip"; exit 1; }
  git cat-file -e "$base^{commit}" || { echo "NOT: base $base is not in the checkout"; exit 1; }
  git merge-base --is-ancestor "$base" "$tip" || { echo "NOT: $base is not an ancestor of $tip"; exit 1; }
  echo "code tip $code_tip, base $base"
}

build() {
  local rev=$1
  git checkout -q --detach "$rev" || { echo "NOT: checkout $rev"; exit 1; }
  echo "build at $(git rev-parse --short=9 HEAD), $(date '+%H:%M:%S')"
  cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build-$rev.txt" 2>&1
  local built=$?
  echo "build rc=$built, $(date '+%H:%M:%S')"
  if [ "$built" != 0 ]; then
    grep -E '^error' -A8 "$RUNNER_TEMP/build-$rev.txt" | head -60
    exit 1
  fi
}

rows() {
  local name=$1 rev=$2
  local out="$RUNNER_TEMP/$name-rows.txt"
  [ "$(git rev-parse --short=9 HEAD)" = "$rev" ] || { echo "NOT: checkout is not $rev"; exit 1; }
  echo "SecurityAgent before $name: $(pgrep -x SecurityAgent | wc -l | tr -d ' ')"
  local since
  since=$(date '+%Y-%m-%d %H:%M:%S')
  cargo test -p crazyagent --lib --locked -- "${filter[@]}" > "$out" 2>&1
  echo "$name rows rc=$? $(grep -E '^test result:' "$out" | tail -1)"
  echo "SecurityAgent after $name: $(pgrep -x SecurityAgent | wc -l | tr -d ' ')"
  echo "SecurityAgent spawns during $name rows: $(/usr/bin/log show --start "$since" --style compact --predicate 'process == "launchd" AND eventMessage CONTAINS "Successfully spawned SecurityAgent"' 2>/dev/null | grep -vc '^Timestamp')"
  for m in "${filter[@]}"; do
    echo "rows $m $(grep -cE "^test ${m}.* \.\.\. (ok|FAILED|ignored)$" "$out")"
  done
  grep -E '^test .* FAILED$' "$out" | awk '{print $2}' | sort > "$RUNNER_TEMP/$name-reds.txt"
  echo "$name reds: $(wc -l < "$RUNNER_TEMP/$name-reds.txt" | tr -d ' ')"
  while read -r row; do
    [ -n "$row" ] || continue
    echo "-- $name red $row"
    awk -v head="---- $row stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$out" | grep -vE 'note: run with' | head -14
  done < "$RUNNER_TEMP/$name-reds.txt"
}

verdict() {
  local failed=0
  comm -23 "$RUNNER_TEMP/tip-reds.txt" "$RUNNER_TEMP/base-reds.txt" > "$RUNNER_TEMP/tip-only.txt"
  echo "reds at the tip: $(tr '\n' ' ' < "$RUNNER_TEMP/tip-reds.txt")"
  if [ -s "$RUNNER_TEMP/tip-only.txt" ]; then
    echo "NOT: tip-only reds: $(tr '\n' ' ' < "$RUNNER_TEMP/tip-only.txt")"
    failed=1
  fi
  for row in $red_rows $tip_rows; do
    if grep -qE "^test $row( - should panic)? \.\.\. ok$" "$RUNNER_TEMP/tip-rows.txt"; then
      echo "tip ok $row"
    else
      echo "NOT: tip not ok $row"
      failed=1
    fi
  done
  for row in $red_rows; do
    if grep -qxF "$row" "$RUNNER_TEMP/base-reds.txt"; then
      echo "base red $row"
    else
      echo "base not red $row"
    fi
  done
  echo "base-only reds outside the new rows: $(grep -vxF "$red_rows" "$RUNNER_TEMP/base-reds.txt" | tr '\n' ' ')"
  echo "VERDICT failed=$failed"
  exit "$failed"
}

case "${1:-}" in
  check) check ;;
  build) build "$2" ;;
  rows) rows "$2" "$3" ;;
  verdict) verdict ;;
  *) echo "NOT: unknown step ${1:-none}"; exit 1 ;;
esac
