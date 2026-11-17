set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
row=plugins::check::windows_tests::a_windows_powershell_server_answers_its_check_within_the_check_time
out="$RUNNER_TEMP/pwsh-cache-red.txt"
failed=0
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
if [ -n "${PSModuleAnalysisCachePath:-}" ]; then cache=set; else cache=unset; fi
printf '| PSModuleAnalysisCachePath in the runner env | set | %s |\n' "$cache" >> "$summary"
if ! git apply .github/farm/pwsh-cache-red.patch; then
  printf '| red patch | applies | DID NOT APPLY |\n' >> "$summary"
  exit 1
fi
if ! cargo test -p crazyagent --lib --locked --no-run; then
  printf '| red patch | compiles | DID NOT COMPILE |\n' >> "$summary"
  git apply -R .github/farm/pwsh-cache-red.patch
  exit 1
fi
if cargo test -p crazyagent --lib --locked -- --exact "$row" --nocapture --test-threads 1 > "$out" 2>&1; then
  printf '| red %s | fails at the CHECK_TIME run | PASSED |\n' "$row" >> "$summary"
  failed=1
elif grep -q 'the stand-in server answers its check given time' "$out"; then
  printf '| red %s | fails at the CHECK_TIME run | FAILED AT THE 90 S CONTROL |\n' "$row" >> "$summary"
  failed=1
elif grep -q 'answers within CHECK_TIME' "$out"; then
  printf '| red %s | fails at the CHECK_TIME run | fails at the CHECK_TIME run |\n' "$row" >> "$summary"
else
  printf '| red %s | fails at the CHECK_TIME run | FAILED ELSEWHERE |\n' "$row" >> "$summary"
  failed=1
fi
cat "$out"
git apply -R .github/farm/pwsh-cache-red.patch
git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; failed=1; }
exit "$failed"
