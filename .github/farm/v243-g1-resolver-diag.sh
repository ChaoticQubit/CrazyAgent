set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
rows=(
  tools::tests::a_group_writable_folder_is_passed_over_unless_its_group_is_admin
  tools::tests::an_npm_script_starts_with_the_node_of_its_own_prefix
  setup::pinned::tests::the_permission_fix_acts_only_on_the_target_the_installer_resolved_and_confirmed
  harnesses::interactive::tests::a_shim_harness_starts_its_real_install_or_else_itself_unless_agents_can_write_it
)
{
  printf '| fact | value |\n| --- | --- |\n'
  printf '| id | %s |\n' "$(id)"
  printf '| /tmp | %s |\n' "$(stat -c '%a %U %G' /tmp)"
  printf '| TMPDIR | %s |\n' "${TMPDIR:-unset}"
} >> "$summary"
git apply .github/farm/g1-resolver-diag.patch || { printf '| diag patch | NOT APPLIED |\n' >> "$summary"; exit 1; }
git diff --stat
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.txt" 2>&1
build=$?
printf '| lib test build | exit %s, %s compile errors |\n' "$build" "$(grep -c 'error\[E' "$RUNNER_TEMP/build.txt")" >> "$summary"
if [ "$build" -ne 0 ]; then
  grep -E -A12 '^error' "$RUNNER_TEMP/build.txt" | head -80
  exit 1
fi
cargo test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}" > "$RUNNER_TEMP/rows.txt" 2>&1
for row in "${rows[@]}"; do
  result=$(grep -F -- "test $row ... " "$RUNNER_TEMP/rows.txt" | sed 's/.* \.\.\. //' | head -1)
  printf '| %s | %s |\n' "${row##*::}" "${result:-not run}" >> "$summary"
done
grep -E '^DIAG ' "$RUNNER_TEMP/rows.txt" | while IFS= read -r line; do
  printf '| diag | %s |\n' "$(printf '%s' "$line" | cut -c1-900 | sed 's/|/\\|/g')" >> "$summary"
done
cat "$RUNNER_TEMP/rows.txt"
git checkout -- crates
git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; exit 1; }
exit 0
