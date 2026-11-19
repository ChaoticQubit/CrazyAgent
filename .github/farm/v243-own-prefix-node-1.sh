set -uo pipefail
tip=d68a73b1b
base=93ca34a7f
filter=(tools:: harnesses:: setup::node::tests::)
new_rows="tools::tests::a_launch_is_refused_when_its_own_prefix_node_is_passed_over
tools::tests::the_harness_list_reads_a_passed_over_own_prefix_node_as_a_launch_does
tools::tests::only_a_passed_over_own_prefix_node_refuses_a_launch
tools::tests::a_passed_over_own_prefix_node_with_none_in_the_places_is_named_not_missing
tools::tests::a_shim_named_by_path_under_an_npm_prefix_never_asks_its_own_node
tools::tests::the_own_prefix_node_check_trips_on_a_tool_whose_file_is_relative
tools::tests::the_own_prefix_node_trips_on_a_relative_script
tools::tests::the_own_prefix_node_trips_past_the_interpreter_depth"
failed=0
git log --oneline -9
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  echo "NOT: code tip $code_tip, wanted $tip"
  exit 1
fi
leg() {
  local name=$1 rev=$2
  local out="$RUNNER_TEMP/$name-rows.txt"
  git checkout -q --detach "$rev" || { echo "NOT: checkout $rev"; exit 1; }
  echo "== $name at $(git rev-parse --short=9 HEAD)"
  cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/$name-build.txt" 2>&1
  local built=$?
  echo "$name build rc=$built"
  if [ "$built" != 0 ]; then
    grep -E '^error' -A6 "$RUNNER_TEMP/$name-build.txt" | head -40
    failed=1
    return
  fi
  cargo test -p crazyagent --lib --locked -- "${filter[@]}" > "$out" 2>&1
  echo "$name rows rc=$? $(grep -E '^test result:' "$out" | tail -1)"
  grep -E '^test .* FAILED$' "$out" | awk '{print $2}' | sort > "$RUNNER_TEMP/$name-reds.txt"
  echo "$name reds: $(tr '\n' ' ' < "$RUNNER_TEMP/$name-reds.txt")"
}
leg tip "$tip"
for row in $new_rows; do
  if grep -qE "^test $row( - should panic)? \.\.\. ok$" "$RUNNER_TEMP/tip-rows.txt"; then
    echo "NEW ok $row"
  else
    echo "NEW NOT ok $row"
    failed=1
  fi
done
leg base "$base"
comm -23 "$RUNNER_TEMP/tip-reds.txt" "$RUNNER_TEMP/base-reds.txt" > "$RUNNER_TEMP/tip-only.txt"
echo "tip-only reds: $(wc -l < "$RUNNER_TEMP/tip-only.txt" | tr -d ' ')"
while read -r name; do
  [ -n "$name" ] || continue
  echo "-- tip-only $name"
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$RUNNER_TEMP/tip-rows.txt" | head -40
  failed=1
done < "$RUNNER_TEMP/tip-only.txt"
echo "base-only reds: $(comm -13 "$RUNNER_TEMP/tip-reds.txt" "$RUNNER_TEMP/base-reds.txt" | tr '\n' ' ')"
echo "VERDICT failed=$failed"
exit "$failed"
