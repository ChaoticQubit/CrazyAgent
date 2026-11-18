set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=95a3ce08b
host=rcli_host::master_process::tests::a_host_outlives_a_runtime_worker_that_handed_its_core_off
thread=a_nested_agent_outlives_the_thread_that_started_it
engine=a_kept_nested_agent_ends_with_the_engine
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
bwrap --version
git log --oneline -2
if [ "$(git rev-parse --short=9 HEAD~1)" != "$tip" ]; then
  row tip "HEAD~1 $tip" "$(git rev-parse --short=9 HEAD~1)"
  exit 1
fi
judge() {
  local label=$1 file=$2 name=$3 at
  at=$(grep -m1 -oE 'panicked at [^:]+:[0-9]+' "$file" | sed 's/panicked at //')
  echo "== $name $label"
  tail -20 "$file"
  if grep -qF -- "test $name ... ok" "$file"; then
    row "$name $label" passes passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    row "$name $label" passes "NOT: fails at ${at:-no panic line}: $(grep -m1 -A1 -F 'panicked at' "$file" | tail -1 | cut -c1-160)"
    failed=1
  else
    row "$name $label" passes "NOT: not run"
    failed=1
  fi
}
cargo test -p crazyagent --lib --test linux_sandbox --locked --no-run 2>&1 | tail -3
for run in 1 2 3 4 5; do
  started=$(date +%s)
  cargo test -p crazyagent --test linux_sandbox --locked -- "$engine" --exact > "$RUNNER_TEMP/engine-$run.txt" 2>&1
  judge "run $run of 5 ($(( $(date +%s) - started )) s)" "$RUNNER_TEMP/engine-$run.txt" "$engine"
done
if pgrep -af '604\.' > "$RUNNER_TEMP/left.txt"; then
  row "processes with the guard token after five runs" none "NOT: $(tr '\n' ' ' < "$RUNNER_TEMP/left.txt" | cut -c1-200)"
  failed=1
else
  row "processes with the guard token after five runs" none none
fi
cargo test -p crazyagent --test linux_sandbox --locked -- "$thread" --exact > "$RUNNER_TEMP/thread.txt" 2>&1
judge "at $tip" "$RUNNER_TEMP/thread.txt" "$thread"
cargo test -p crazyagent --lib --locked -- "$host" --exact > "$RUNNER_TEMP/host.txt" 2>&1
judge "at $tip" "$RUNNER_TEMP/host.txt" "$host"
exit "$failed"
