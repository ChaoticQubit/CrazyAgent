set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
files=(
  crates/crazyagent/src/coord/delivery.rs
  crates/crazyagent/src/coord/delivery/tests.rs
  crates/crazyagent/src/coord/relay.rs
  crates/crazyagent/src/coord/relay/tests.rs
  crates/crazyagent/src/coord/store.rs
  crates/crazyagent/src/team.rs
  crates/crazyagent/src/team/runtime.rs
  crates/crazyagent/src/team/runtime/tests.rs
)
rows=(
  coord::delivery::tests::waiting_message_reads_do_not_grow_with_message_history
  team::runtime::tests::the_active_projects_read_does_not_grow_with_team_history
  team::runtime::tests::ide_workspaces_are_followed_only_after_the_database_changes
  coord::relay::tests::an_unchanged_roster_is_not_written_again_and_a_tampered_one_is_restored
  team::runtime::tests::dispatch_with_nothing_queued_reads_no_per_task_settings
)
row2=team::runtime::tests::a_commit_during_the_workspace_follow_is_followed_on_the_next_tick
failed=0
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
red() {
  local sha=$1 out=$2
  shift 2
  git checkout "$sha" -- "${files[@]}" || { printf '| files at %s | check out | FAILED |\n' "$sha" >> "$summary"; exit 1; }
  cargo test -p crazyagent --lib --locked --no-fail-fast -- "$@" > "$out" 2>&1
  local errors at row
  errors=$(grep -c 'error\[E' "$out")
  for row in "$@"; do
    at=$(awk -v h="---- $row stdout ----" '$0 == h { f = 1; next } f && /^---- / { exit } f && /panicked at / { sub(/.*panicked at /, ""); sub(/:.*/, ""); print; exit }' "$out")
    if grep -qF -- "test $row ... FAILED" "$out" && [ "$errors" -eq 0 ] && [[ "$at" == */tests.rs ]]; then
      printf '| red %s at %s | fails at its assertion | fails at %s |\n' "${row##*::}" "$sha" "$at" >> "$summary"
    else
      printf '| red %s at %s | fails at its assertion | NOT: %s compile errors, panic at %s |\n' "${row##*::}" "$sha" "$errors" "${at:-none}" >> "$summary"
      failed=1
    fi
  done
  cat "$out"
  git checkout HEAD -- "${files[@]}"
  git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; exit 1; }
}
red 4aa4b6320 "$RUNNER_TEMP/red.txt" "${rows[@]}"
red c287c16bf "$RUNNER_TEMP/red2.txt" "$row2"
cargo test -p crazyagent --lib --locked --no-fail-fast -- "${rows[@]}" "$row2" > "$RUNNER_TEMP/green-rows.txt" 2>&1
for row in "${rows[@]}" "$row2"; do
  if grep -qF -- "test $row ... ok" "$RUNNER_TEMP/green-rows.txt"; then
    printf '| green %s at d86298438 | ok | ok |\n' "${row##*::}" >> "$summary"
  else
    printf '| green %s at d86298438 | ok | NOT OK |\n' "${row##*::}" >> "$summary"
    failed=1
  fi
done
cat "$RUNNER_TEMP/green-rows.txt"
if cargo test -p crazyagent --lib --locked --no-fail-fast -- coord::delivery:: coord::relay:: coord::store:: team::runtime:: ide::workspace:: > "$RUNNER_TEMP/green-modules.txt" 2>&1; then
  printf '| coord delivery, relay, store, team runtime, ide workspace modules at d86298438 | pass | %s |\n' "$(grep -E '^test result' "$RUNNER_TEMP/green-modules.txt" | tail -1)" >> "$summary"
else
  printf '| coord delivery, relay, store, team runtime, ide workspace modules at d86298438 | pass | FAILED: %s |\n' "$(grep -cE '^test .* FAILED$' "$RUNNER_TEMP/green-modules.txt") rows" >> "$summary"
  failed=1
fi
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/green-modules.txt"
cargo clippy -p crazyagent --all-targets --locked --message-format short -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
rc=$?
mine=$(grep -cE 'crates/crazyagent/src/(coord/delivery|coord/delivery/tests|coord/relay|coord/relay/tests|coord/store|team|team/runtime|team/runtime/tests)\.rs:' "$RUNNER_TEMP/clippy.txt")
printf '| macOS clippy --all-targets -D warnings | exit 0 | exit %s, %s findings in the eight changed files |\n' "$rc" "$mine" >> "$summary"
[ "$mine" -eq 0 ] || failed=1
cat "$RUNNER_TEMP/clippy.txt"
exit "$failed"
