set -uo pipefail
mode="$1"
log="$2"
new_rows="a_deletable_file_renames_by_name_into_a_held_folder a_no_replace_rename_onto_a_taken_or_case_only_name_is_refused a_rename_through_a_handle_without_delete_access_is_refused a_folder_with_an_open_child_is_not_renamed a_rename_name_that_is_not_one_component_trips_the_assertion a_replace_over_a_held_target_is_refused_and_changes_nothing a_replace_over_a_replaceable_hold_passes_and_the_old_handle_keeps_the_old_bytes a_replace_over_a_link_replaces_the_link_and_not_its_target a_deletable_folder_renames_through_its_handle_while_its_lock_stream_is_held dir_deletable_makes_only_a_missing_folder_and_opens_only_an_existing_one read_only_replaceable_refuses_a_shared_file_and_a_link"
kept=30
added=$(echo $new_rows | wc -w | tr -d ' ')
total=$(( kept + added ))
grep -E '^test |^test result' "$log" || true
grep -E -A6 'panicked' "$log" || true
grep -qE '^test result' "$log" || { echo "::error::no test result in $log"; tail -60 "$log"; exit 1; }
if [ "$mode" = row5 ]; then
  grep -qE "^test result: FAILED\. 0 passed; 1 failed" "$log" || { echo "::error::row 5 at the first green is not 0 passed and 1 failed"; exit 1; }
  grep -qF '"a." reached the rename' "$log" || { echo "::error::row 5 did not fail on the trailing dot"; exit 1; }
  echo "N6 W1 row 5: a trailing dot reaches the first green rename, so the row is red there"
elif [ "$mode" = red ]; then
  grep -qE "^test result: FAILED\. $kept passed; $added failed" "$log" || { echo "::error::red is not $kept passed and $added failed"; exit 1; }
  for row in $new_rows; do
    grep -qE "^    $row\$" "$log" || { echo "::error::$row did not fail at red"; exit 1; }
  done
  echo "N6 W1 red: the $added new rows fail by name, the $kept kept rows pass"
else
  grep -qE "^test result: ok\. $total passed; 0 failed" "$log" || { echo "::error::tip is not $total passed"; exit 1; }
  for row in $new_rows; do
    grep -qE "^test $row \.\.\. " "$log" || { echo "::error::$row did not run"; exit 1; }
    if grep -qE "^    $row\$" "$log"; then echo "::error::$row is among the failures"; exit 1; fi
  done
  echo "N6 W1 tip: $total of $total W1 private rows pass, each new row ran and is not among the failures"
fi
