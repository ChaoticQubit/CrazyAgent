set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
rows=(
  harnesses::update::run::windows_tests::a_version_run_past_its_limit_ends_with_every_process_it_started
  plugins::check::windows_tests::a_silent_server_times_out_and_every_process_it_started_ends_with_it
)
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
if ! git apply .github/farm/grandchild-red.patch; then
  printf '| red patch | applies | DID NOT APPLY |\n' >> "$summary"
  exit 1
fi
if ! cargo test -p crazyagent --lib --locked --no-run; then
  printf '| red patch | compiles | DID NOT COMPILE |\n' >> "$summary"
  git apply -R .github/farm/grandchild-red.patch
  exit 1
fi
for row in "${rows[@]}"; do
  out="$RUNNER_TEMP/red-${row##*::}.txt"
  if cargo test -p crazyagent --lib --locked -- --exact "$row" --test-threads 1 > "$out" 2>&1; then
    printf '| red %s | fails at outlived | PASSED |\n' "$row" >> "$summary"
    failed=1
  elif grep -q 'outlived' "$out"; then
    printf '| red %s | fails at outlived | fails at outlived |\n' "$row" >> "$summary"
  else
    printf '| red %s | fails at outlived | FAILED ELSEWHERE |\n' "$row" >> "$summary"
    failed=1
  fi
  cat "$out"
done
git apply -R .github/farm/grandchild-red.patch
git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; failed=1; }
exit "$failed"
