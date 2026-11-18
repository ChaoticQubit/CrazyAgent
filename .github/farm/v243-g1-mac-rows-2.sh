#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
tip=2ad06b275
rows=(
  projects_home::move_tests::a_long_stored_project_move_reroots_rows_recorded_under_the_short_name
  projects_home::move_tests::a_project_stored_through_the_data_volume_moves_into_the_home
)
failed=0
git diff --quiet "$tip" -- crates || { echo "::error::the crates tree is not $tip"; exit 1; }
printf '| macOS row at %s | expected | result |\n| --- | --- | --- |\n' "$tip" >> "$summary"
cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/build.txt" 2>&1
rc=$?
tail -n 5 "$RUNNER_TEMP/build.txt"
if [ "$rc" -ne 0 ]; then
  grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/build.txt" | head -n 80
  printf '| lib test build | builds | FAILED, exit %s |\n' "$rc" >> "$summary"
  echo "FARM failed=1"
  exit 1
fi
for name in "${rows[@]}"; do
  cargo test -p crazyagent --locked --lib -- --exact "$name" --test-threads 1 > "$RUNNER_TEMP/row.txt" 2>&1
  rc=$?
  cat "$RUNNER_TEMP/row.txt"
  if [ "$rc" -eq 0 ] && grep -qE "^test $name \.\.\. ok$" "$RUNNER_TEMP/row.txt"; then
    printf '| %s | passes | passes |\n' "${name##*::}" >> "$summary"
  else
    printf '| %s | passes | **FAILED, exit %s** |\n' "${name##*::}" "$rc" >> "$summary"; failed=1
  fi
done
cargo test -p crazyagent --locked --lib -- projects_home::move_tests:: --test-threads 1 > "$RUNNER_TEMP/module.txt" 2>&1
rc=$?
grep -E '^test .* \.\.\. (ok|FAILED|ignored)$|^test result:' "$RUNNER_TEMP/module.txt"
grep -nE "panicked at" "$RUNNER_TEMP/module.txt" | head -n 20 || true
passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/module.txt" || true)
reds=$(grep -cE '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/module.txt" || true)
if [ "$rc" -eq 0 ] && [ "$reds" -eq 0 ] && [ "$passed" -gt 0 ]; then
  printf '| every projects_home::move_tests row | passes | %s passed |\n' "$passed" >> "$summary"
else
  printf '| every projects_home::move_tests row | passes | **%s passed, %s FAILED, exit %s** |\n' "$passed" "$reds" "$rc" >> "$summary"; failed=1
fi
echo "FARM failed=$failed"
exit "$failed"
