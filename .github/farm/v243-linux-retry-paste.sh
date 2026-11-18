set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
tui=tui::client::project_setup::tests::tui_creates_a_project_with_setup_and_reads_its_jobs_and_context
desk=desktop_creates_a_project_folder_and_reads_its_context_through_the_real_engine
row=terminal::tmux::tests::a_send_keys_run_never_cuts_an_escape_sequence_of_a_batched_input
red=d6c902dbb
head=$(git rev-parse --short=9 HEAD)
failed=0
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
mkdir -p apps/desktop/dist
cargo test -p crazyagent --lib --locked --no-fail-fast -- terminal:: "$tui" > "$RUNNER_TEMP/green-lib.txt" 2>&1
rc=$?
total=$(grep -E '^test result' "$RUNNER_TEMP/green-lib.txt" | tail -1)
for name in "$tui" "$row"; do
  if grep -qE "^test $name \.\.\. ok$" "$RUNNER_TEMP/green-lib.txt"; then
    printf '| %s at %s | passes | passes |\n' "$name" "$head" >> "$summary"
  else
    printf '| %s at %s | passes | FAILED |\n' "$name" "$head" >> "$summary"; failed=1
  fi
done
if [ "$rc" -eq 0 ]; then
  printf '| terminal:: module at %s | passes | %s |\n' "$head" "$total" >> "$summary"
else
  printf '| terminal:: module at %s | passes | FAILED, exit %s, %s |\n' "$head" "$rc" "$total" >> "$summary"; failed=1
fi
cat "$RUNNER_TEMP/green-lib.txt"
cargo test -p crazyagent-desktop --test project_setup_bridge --locked -- --exact "$desk" > "$RUNNER_TEMP/green-desk.txt" 2>&1
rc=$?
if [ "$rc" -eq 0 ] && grep -qE "^test $desk \.\.\. ok$" "$RUNNER_TEMP/green-desk.txt"; then
  printf '| %s at %s | passes | passes |\n' "$desk" "$head" >> "$summary"
else
  printf '| %s at %s | passes | FAILED, exit %s |\n' "$desk" "$head" "$rc" >> "$summary"; failed=1
fi
cat "$RUNNER_TEMP/green-desk.txt"
git diff --quiet && [ -z "$(git status --porcelain --untracked-files=all)" ] || { printf '| tree after greens | clean | DIRTY |\n' >> "$summary"; exit 1; }
git checkout -q --detach "$red" || { printf '| red checkout %s | checks out | FAILED |\n' "$red" >> "$summary"; exit 1; }
if cargo test -p crazyagent --lib --locked -- --exact "$row" > "$RUNNER_TEMP/red.txt" 2>&1; then
  printf '| %s at %s | fails | PASSED |\n' "$row" "$red" >> "$summary"; failed=1
elif grep -qE "^test $row \.\.\. FAILED$" "$RUNNER_TEMP/red.txt"; then
  printf '| %s at %s | fails at run time | fails at run time |\n' "$row" "$red" >> "$summary"
else
  printf '| %s at %s | fails at run time | FAILED ELSEWHERE |\n' "$row" "$red" >> "$summary"; failed=1
fi
cat "$RUNNER_TEMP/red.txt"
exit "$failed"
