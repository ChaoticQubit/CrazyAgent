#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
hp=sandbox::hidden_paths::tests::
win=sandbox::windows::tests::
note() { printf '%s\n' "$1" | tee -a "$summary"; }
trips=("A1|${hp}only_windows_walks_the_hidden_folders_and_only_linux_still_refuses_them|unheld.code.starts_with"
  "A2|${hp}trip_a2_more_folders_than_a_list_holds|folders.len() <= HIDDEN_PATHS_MAX"
  "A3|${hp}trip_a3_a_zero_bound|entries_max > 0"
  "A4|${hp}the_walk_counts_every_entry_of_every_folder_against_one_bound|seen <= entries_max"
  "A5|${hp}the_walk_counts_every_entry_of_every_folder_against_one_bound|stack.is_empty()")
inert=(sandbox::hidden_paths:: --skip trip_a)
if [ "$RUNNER_OS" = Windows ]; then
  trips+=("A6|${hp}the_walk_names_a_hidden_file_with_another_name_and_passes_the_rest|an open file has a name"
    "A7|${win}hidden_folders_join_the_boundary_hidden_and_an_absent_one_is_made_empty|left == right"
    "A8|${win}hidden_folders_join_the_boundary_hidden_and_an_absent_one_is_made_empty|made <= sandbox.hidden.len()")
  inert=(sandbox::hidden_paths:: "${win}hidden_folders_join_the_boundary_hidden_and_an_absent_one_is_made_empty" --skip trip_a)
fi
git checkout -q -f --detach "$1" || { echo "checkout $1 failed"; exit 1; }
echo "######## at $(git log --oneline -1)"
python3 "$RUNNER_TEMP/trips.py" || { note "| trips | the injections did not apply |"; exit 1; }
git diff --stat
cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/lib.txt" 2>&1 || { grep -E '^error' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 60; note "| trips build | FAILED |"; exit 1; }
failed=0
cargo test -p crazyagent --locked --lib -- "${inert[@]}" > "$RUNNER_TEMP/inert.txt" 2>&1
rc=$?
grep -E '^test result:' "$RUNNER_TEMP/inert.txt"
grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/inert.txt"
note "| injections inert with no TRIP variable | exit $rc, $(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/inert.txt") ok |"
[ "$rc" -eq 0 ] || failed=1
for trip in "${trips[@]}"; do
  IFS='|' read -r name row words <<< "$trip"
  env "TRIP_$name=1" cargo test -p crazyagent --locked --lib -- --exact "$row" > "$RUNNER_TEMP/trip-$name.txt" 2>&1
  rc=$?
  grep -E 'panicked at' -A 2 "$RUNNER_TEMP/trip-$name.txt" | head -n 6
  if [ "$rc" -ne 0 ] && grep -qE "^test $row \.\.\. FAILED$" "$RUNNER_TEMP/trip-$name.txt" && grep -qF "$words" "$RUNNER_TEMP/trip-$name.txt"; then
    note "| $name trips | \`$words\` fired in ${row##*::} |"
  else
    note "| $name trips | **did not fire \`$words\`** (exit $rc) |"
    failed=1
  fi
done
echo "FARM trips failed=$failed"
exit "$failed"
