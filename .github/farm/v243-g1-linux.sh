#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
host=engine::claude_host::tests::a_reaped_claude_harness_with_a_half_written_turn_is_waited_for_without_spinning
add=an_add_plans_over_its_own_row_and_beside_an_add_elsewhere_but_not_onto_its_name
head=$(git rev-parse --short=9 HEAD)
failed=0
printf '| %s run | expected | result |\n| --- | --- | --- |\n' "$RUNNER_OS" >> "$summary"
cargo test -p crazyagent --locked --test projects_home_engine -- --exact "$add" > "$RUNNER_TEMP/add.txt" 2>&1
rc=$?
cat "$RUNNER_TEMP/add.txt"
if [ "$rc" -eq 0 ] && grep -qE "^test $add \.\.\. ok$" "$RUNNER_TEMP/add.txt"; then
  printf '| %s at %s | passes | passes |\n' "$add" "$head" >> "$summary"
else
  printf '| %s at %s | passes | FAILED, exit %s |\n' "$add" "$head" "$rc" >> "$summary"; failed=1
fi
cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1
rc=$?
tail -n 5 "$RUNNER_TEMP/bins.txt"
if [ "$rc" -ne 0 ]; then
  printf '| engine binaries at %s | build | FAILED, exit %s |\n' "$head" "$rc" >> "$summary"
  echo "FARM failed=1"
  exit 1
fi
mkdir -p target/debug/deps
cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
for run in 1 2 3 4 5; do
  cargo test -p crazyagent --locked --lib -- --exact "$host" > "$RUNNER_TEMP/host-$run.txt" 2>&1
  rc=$?
  cat "$RUNNER_TEMP/host-$run.txt"
  if [ "$rc" -eq 0 ] && grep -qE "^test $host \.\.\. ok$" "$RUNNER_TEMP/host-$run.txt"; then
    printf '| %s run %s at %s | passes | passes |\n' "$host" "$run" "$head" >> "$summary"
  else
    printf '| %s run %s at %s | passes | FAILED, exit %s |\n' "$host" "$run" "$head" "$rc" >> "$summary"; failed=1
  fi
done
echo "FARM failed=$failed"
exit "$failed"
