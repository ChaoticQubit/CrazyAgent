set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
out="$RUNNER_TEMP/v244-batch-$label.txt"
: > "$out"
[ "$RUNNER_OS" = Linux ] || { echo "::error::Linux only, not $RUNNER_OS"; exit 1; }
rows=(
  engine::owner::transport::intake_parallel_tests::two_folders_move_into_the_home_together
  engine::owner::transport::intake_parallel_tests::a_stop_of_one_job_leaves_the_other_running
  engine::owner::transport::intake_parallel_tests::a_start_counts_the_claim_a_stopped_waiting_job_still_holds
  engine::owner::transport::intake_parallel_tests::a_start_past_the_waiting_list_is_busy_while_client_changes_hold_their_places
  engine::owner::transport::intake_parallel_tests::waiting_jobs_start_by_themselves_and_a_start_past_the_waiting_list_is_busy
)
terms=$(printf 'test(=%s)\n' "${rows[@]}" | paste -sd'|' - | sed 's/|/ | /g')
filter="package(=crazyagent) & kind(lib) & ($terms)"
tries=10
summary="$RUNNER_TEMP/intake-summary.md"
printf '| sha | row | failed of %s |\n| --- | --- | --- |\n' "$tries" > "$summary"
for sha in 2d0fff43a 3f09ae67f; do
  git checkout -q --detach "$sha" || { echo "::error::cannot check out $sha"; exit 1; }
  git log --oneline -1 | tee -a "$out"
  features=()
  if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
  cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-run 2>&1 | tail -3 | tee -a "$out"
  : > "$RUNNER_TEMP/fails-$sha.txt"
  for try in $(seq 1 "$tries"); do
    cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-fail-fast -E "$filter" > "$RUNNER_TEMP/try.txt" 2>&1
    tr -d '\r' < "$RUNNER_TEMP/try.txt" | tee -a "$out" | grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT) \[' | awk '{print $NF}' >> "$RUNNER_TEMP/fails-$sha.txt"
    echo "== $sha try $try done" | tee -a "$out"
  done
  for row in "${rows[@]}"; do
    n=$(grep -cxF "$row" "$RUNNER_TEMP/fails-$sha.txt")
    printf '| %s | %s | %s |\n' "$sha" "${row##*::}" "$n" >> "$summary"
    echo "== $sha ${row##*::} failed=$n/$tries" | tee -a "$out"
  done
done
cat "$summary" >> "$GITHUB_STEP_SUMMARY"
