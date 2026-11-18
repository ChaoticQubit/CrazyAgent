set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
row=plugins::tests::a_powershell_server_that_timed_out_names_its_module_reading_as_the_likely_cause
files=(crates/crazyagent/src/plugins.rs crates/crazyagent/src/engine/owner/transport/plugins.rs)
failed=0
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
git checkout 6156e8a0b -- "${files[@]}" || { printf '| red files | check out | FAILED |\n' >> "$summary"; exit 1; }
if cargo test -p crazyagent --lib --locked -- --exact "$row" > "$RUNNER_TEMP/red.txt" 2>&1; then
  printf '| red row at 6156e8a0b | fails | PASSED |\n' >> "$summary"; failed=1
elif grep -q 'server_check_timeout_powershell' "$RUNNER_TEMP/red.txt"; then
  printf '| red row at 6156e8a0b | fails at its assertion | fails at its assertion |\n' >> "$summary"
else
  printf '| red row at 6156e8a0b | fails at its assertion | FAILED ELSEWHERE |\n' >> "$summary"; failed=1
fi
cat "$RUNNER_TEMP/red.txt"
git checkout HEAD -- "${files[@]}"
git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; exit 1; }
if cargo test -p crazyagent --lib --locked --no-fail-fast -- plugins::tests:: engine::owner::transport::plugins::tests:: > "$RUNNER_TEMP/green.txt" 2>&1; then
  printf '| plugins and transport plugins modules at dfaed2cf3 | pass | %s |\n' "$(grep -E '^test result' "$RUNNER_TEMP/green.txt" | tail -1)" >> "$summary"
else
  printf '| plugins and transport plugins modules at dfaed2cf3 | pass | FAILED |\n' >> "$summary"; failed=1
fi
cat "$RUNNER_TEMP/green.txt"
cargo clippy -p crazyagent --lib --tests --locked --message-format short -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
rc=$?
mine=$(grep -cE 'crates/crazyagent/src/(plugins\.rs|engine/owner/transport/plugins\.rs):' "$RUNNER_TEMP/clippy.txt")
printf '| Linux clippy --lib --tests -D warnings | exit 0 | exit %s, %s findings in the two changed files |\n' "$rc" "$mine" >> "$summary"
[ "$mine" -eq 0 ] || failed=1
cat "$RUNNER_TEMP/clippy.txt"
exit "$failed"
