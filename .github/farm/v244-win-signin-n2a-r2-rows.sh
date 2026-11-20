set -uo pipefail
mode="$1"
log="$2"
both="a_held_folder_makes_opens_and_pins_its_children a_linked_swapped_open_or_hard_linked_child_is_refused a_name_that_is_not_one_plain_component_trips_the_layer shared_locks_meet_an_exclusive_one_waits_and_unlock_frees_it making_a_folder_over_an_existing_one_is_refused_and_keeps_it a_checked_name_refuses_every_unsafe_spelling_and_keeps_plain_ones making_a_file_is_create_only_and_private_and_open_write_judges_it a_listing_names_files_folders_and_links_without_following_and_is_bounded removing_an_entry_needs_its_pin_and_leaves_a_held_one errors_map_by_cause_busy_missing_and_swapped an_unjudged_call_on_a_private_tree_trips_the_layer"
windows_only="a_verbatim_root_trips_the_layer_and_the_plain_spelling_holds the_folder_above_a_held_chain_cannot_be_renamed retargeting_a_junction_above_the_root_leaves_opens_in_the_held_tree a_held_folder_turned_into_a_junction_lists_as_unsafe"
unix_only="a_fifo_at_a_file_name_is_refused_without_blocking"
if [ "$RUNNER_OS" = Windows ]; then
  rows="$both $windows_only"
  passing_at_red="a_name_that_is_not_one_plain_component_trips_the_layer shared_locks_meet_an_exclusive_one_waits_and_unlock_frees_it a_verbatim_root_trips_the_layer_and_the_plain_spelling_holds the_folder_above_a_held_chain_cannot_be_renamed retargeting_a_junction_above_the_root_leaves_opens_in_the_held_tree"
else
  rows="$both $unix_only"
  passing_at_red="a_name_that_is_not_one_plain_component_trips_the_layer shared_locks_meet_an_exclusive_one_waits_and_unlock_frees_it"
fi
count=$(echo $rows | wc -w | tr -d ' ')
passing=$(echo $passing_at_red | wc -w | tr -d ' ')
failing=$(( count - passing ))
grep -E '^test .*held::tests|^test result' "$log" || true
grep -E -A6 'panicked' "$log" || true
grep -qE '^test result' "$log" || { echo "::error::no test result in $log"; tail -60 "$log"; exit 1; }
if [ "$mode" = red ]; then
  grep -qE "^test result: FAILED\. $passing passed; $failing failed" "$log" || { echo "::error::red is not $passing passed and $failing failed"; exit 1; }
  for row in $rows; do
    if echo " $passing_at_red " | grep -q " $row "; then
      grep -qE "^    engine::bootstrap::held::tests::$row\$" "$log" && { echo "::error::$row failed at red, expected to pass"; exit 1; }
    else
      grep -qE "^    engine::bootstrap::held::tests::$row\$" "$log" || { echo "::error::$row did not fail at red"; exit 1; }
    fi
  done
  echo "N2a r2 red: $failing new or changed rows fail, the $passing unchanged ones pass"
else
  grep -qE "^test result: ok\. $count passed; 0 failed" "$log" || { echo "::error::tip is not $count passed"; exit 1; }
  for row in $rows; do
    grep -qE "held::tests::$row " "$log" || { echo "::error::$row did not run"; exit 1; }
  done
  echo "N2a r2 tip: $count of $count held rows pass, each named row ran"
fi
